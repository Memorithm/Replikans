use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use replikan_opportunities::OpportunityId;
use replikan_resource::ResourceId;

use crate::{ActivationId, ActivationJournal, ActivationState, JournalFailure};

const JOURNAL_HEADER: &str = "REPLIKANS_ACTIVATION_JOURNAL_V1";

/// Durable, append-only activation journal with a single-writer lock.
///
/// Every state transition is appended and `sync_all` is completed before the
/// in-memory state is changed. A malformed or truncated journal is rejected on
/// open instead of being partially recovered. The adjacent lock directory is
/// intentionally left behind by an unclean process crash, forcing explicit
/// reconciliation before another executor instance can open the journal.
pub struct FileActivationJournal {
    path: PathBuf,
    file: File,
    states: BTreeMap<ActivationId, ActivationState>,
    _lock: ExclusivePathLock,
}

impl FileActivationJournal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalFailure> {
        let path = path.as_ref().to_path_buf();
        if path.as_os_str().is_empty() {
            return Err(failure("activation journal path cannot be empty"));
        }
        reject_symlink(&path)?;

        let lock = ExclusivePathLock::acquire(&path)?;
        let existing = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(io_failure("read activation journal", &error)),
        };

        let states = match existing.as_deref() {
            Some(bytes) => parse_journal(bytes)?,
            None => BTreeMap::new(),
        };

        let mut options = OpenOptions::new();
        options.create(true).append(true).read(true);
        set_owner_only_create_mode(&mut options);
        let mut file = options
            .open(&path)
            .map_err(|error| io_failure("open activation journal", &error))?;
        reject_unsafe_permissions(&file)?;

        if existing.is_none() {
            file.write_all(JOURNAL_HEADER.as_bytes())
                .and_then(|()| file.write_all(b"\n"))
                .and_then(|()| file.sync_all())
                .map_err(|error| io_failure("initialize activation journal", &error))?;
            sync_parent_directory(&path)?;
        }

        Ok(Self {
            path,
            file,
            states,
            _lock: lock,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.states.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    fn append_record(&mut self, record: &str) -> Result<(), JournalFailure> {
        if record.contains('\n') || record.contains('\r') {
            return Err(failure("activation journal record contains a newline"));
        }
        self.file
            .write_all(record.as_bytes())
            .and_then(|()| self.file.write_all(b"\n"))
            .and_then(|()| self.file.sync_all())
            .map_err(|error| io_failure("append activation journal record", &error))
    }
}

impl ActivationJournal for FileActivationJournal {
    fn state(&self, id: &ActivationId) -> Result<Option<ActivationState>, JournalFailure> {
        Ok(self.states.get(id).cloned())
    }

    fn begin(&mut self, id: ActivationId, began_at_unix_ms: u64) -> Result<(), JournalFailure> {
        if self.states.contains_key(&id) {
            return Err(failure("activation already has durable journal state"));
        }
        let record = encode_pending(&id, began_at_unix_ms);
        self.append_record(&record)?;
        self.states
            .insert(id, ActivationState::Pending { began_at_unix_ms });
        Ok(())
    }

    fn commit(
        &mut self,
        id: &ActivationId,
        committed_at_unix_ms: u64,
        evidence: &str,
    ) -> Result<(), JournalFailure> {
        if evidence.trim().is_empty() {
            return Err(failure("activation commit evidence is blank"));
        }
        let began_at_unix_ms = match self.states.get(id) {
            Some(ActivationState::Pending { began_at_unix_ms }) => *began_at_unix_ms,
            Some(ActivationState::Committed { .. }) | None => {
                return Err(failure("activation is not pending"));
            }
        };
        if committed_at_unix_ms < began_at_unix_ms {
            return Err(failure("activation commit timestamp regressed"));
        }

        let record = encode_committed(id, began_at_unix_ms, committed_at_unix_ms, evidence);
        self.append_record(&record)?;
        self.states.insert(
            id.clone(),
            ActivationState::Committed {
                began_at_unix_ms,
                committed_at_unix_ms,
                evidence: evidence.to_owned(),
            },
        );
        Ok(())
    }
}

fn encode_pending(id: &ActivationId, began_at_unix_ms: u64) -> String {
    format!(
        "P|{}|{}|{}|{}",
        id.decision_sequence,
        hex_encode(id.opportunity_id.as_str().as_bytes()),
        hex_encode(id.resource_id.as_str().as_bytes()),
        began_at_unix_ms
    )
}

fn encode_committed(
    id: &ActivationId,
    began_at_unix_ms: u64,
    committed_at_unix_ms: u64,
    evidence: &str,
) -> String {
    format!(
        "C|{}|{}|{}|{}|{}|{}",
        id.decision_sequence,
        hex_encode(id.opportunity_id.as_str().as_bytes()),
        hex_encode(id.resource_id.as_str().as_bytes()),
        began_at_unix_ms,
        committed_at_unix_ms,
        hex_encode(evidence.as_bytes())
    )
}

fn parse_journal(bytes: &[u8]) -> Result<BTreeMap<ActivationId, ActivationState>, JournalFailure> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| failure("activation journal is not valid UTF-8"))?;
    if !text.ends_with('\n') {
        return Err(failure("activation journal is truncated"));
    }

    let mut lines = text.lines();
    if lines.next() != Some(JOURNAL_HEADER) {
        return Err(failure("activation journal header/version is invalid"));
    }

    let mut states = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            return Err(failure("activation journal contains an empty record"));
        }
        parse_record(line, &mut states)?;
    }
    Ok(states)
}

fn parse_record(
    line: &str,
    states: &mut BTreeMap<ActivationId, ActivationState>,
) -> Result<(), JournalFailure> {
    let fields = line.split('|').collect::<Vec<_>>();
    match fields.as_slice() {
        ["P", sequence, opportunity, resource, began] => {
            let id = decode_activation_id(sequence, opportunity, resource)?;
            let began_at_unix_ms = parse_u64(began, "pending timestamp")?;
            if states.contains_key(&id) {
                return Err(failure("duplicate pending activation record"));
            }
            states.insert(id, ActivationState::Pending { began_at_unix_ms });
            Ok(())
        }
        ["C", sequence, opportunity, resource, began, committed, evidence] => {
            let id = decode_activation_id(sequence, opportunity, resource)?;
            let began_at_unix_ms = parse_u64(began, "begin timestamp")?;
            let committed_at_unix_ms = parse_u64(committed, "commit timestamp")?;
            if committed_at_unix_ms < began_at_unix_ms {
                return Err(failure("committed activation timestamp regressed"));
            }
            let evidence = decode_text(evidence, "activation evidence")?;
            if evidence.trim().is_empty() {
                return Err(failure("committed activation evidence is blank"));
            }
            match states.get(&id) {
                Some(ActivationState::Pending {
                    began_at_unix_ms: pending_began,
                }) if *pending_began == began_at_unix_ms => {}
                Some(ActivationState::Pending { .. }) => {
                    return Err(failure("activation begin timestamp changed before commit"));
                }
                Some(ActivationState::Committed { .. }) | None => {
                    return Err(failure("commit record has no matching pending activation"));
                }
            }
            states.insert(
                id,
                ActivationState::Committed {
                    began_at_unix_ms,
                    committed_at_unix_ms,
                    evidence,
                },
            );
            Ok(())
        }
        _ => Err(failure("activation journal record shape is invalid")),
    }
}

fn decode_activation_id(
    sequence: &str,
    opportunity: &str,
    resource: &str,
) -> Result<ActivationId, JournalFailure> {
    let decision_sequence = parse_u64(sequence, "decision sequence")?;
    let opportunity_text = decode_text(opportunity, "opportunity id")?;
    let resource_text = decode_text(resource, "resource id")?;
    let opportunity_id = OpportunityId::new(opportunity_text)
        .map_err(|error| failure(&format!("invalid opportunity id in journal: {error}")))?;
    let resource_id = ResourceId::new(resource_text)
        .map_err(|error| failure(&format!("invalid resource id in journal: {error}")))?;
    Ok(ActivationId {
        decision_sequence,
        opportunity_id,
        resource_id,
    })
}

fn parse_u64(value: &str, field: &str) -> Result<u64, JournalFailure> {
    value
        .parse::<u64>()
        .map_err(|_| failure(&format!("activation journal {field} is invalid")))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    result
}

fn decode_text(encoded: &str, field: &str) -> Result<String, JournalFailure> {
    let bytes = hex_decode(encoded, field)?;
    String::from_utf8(bytes)
        .map_err(|_| failure(&format!("activation journal {field} is not UTF-8")))
}

fn hex_decode(encoded: &str, field: &str) -> Result<Vec<u8>, JournalFailure> {
    if !encoded.len().is_multiple_of(2) {
        return Err(failure(&format!(
            "activation journal {field} has odd-length hex"
        )));
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    let raw = encoded.as_bytes();
    for chunk in raw.chunks_exact(2) {
        let high = hex_nibble(chunk[0])
            .ok_or_else(|| failure(&format!("activation journal {field} contains non-hex data")))?;
        let low = hex_nibble(chunk[1])
            .ok_or_else(|| failure(&format!("activation journal {field} contains non-hex data")))?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

struct ExclusivePathLock {
    path: PathBuf,
}

impl ExclusivePathLock {
    fn acquire(journal_path: &Path) -> Result<Self, JournalFailure> {
        let path = lock_path(journal_path);
        fs::create_dir(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                failure("activation journal is already locked or requires crash reconciliation")
            } else {
                io_failure("acquire activation journal lock", &error)
            }
        })?;
        sync_parent_directory(&path)?;
        Ok(Self { path })
    }
}

impl Drop for ExclusivePathLock {
    fn drop(&mut self) {
        let _ignored = fs::remove_dir(&self.path);
        let _ignored = sync_parent_directory(&self.path);
    }
}

fn lock_path(journal_path: &Path) -> PathBuf {
    let mut name = OsString::from(journal_path.as_os_str());
    name.push(".lock");
    PathBuf::from(name)
}

fn reject_symlink(path: &Path) -> Result<(), JournalFailure> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(failure("activation journal path cannot be a symlink"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_failure("inspect activation journal path", &error)),
    }
}

#[cfg(unix)]
fn set_owner_only_create_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_owner_only_create_mode(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn reject_unsafe_permissions(file: &File) -> Result<(), JournalFailure> {
    use std::os::unix::fs::PermissionsExt;
    let mode = file
        .metadata()
        .map_err(|error| io_failure("inspect activation journal permissions", &error))?
        .permissions()
        .mode();
    if mode & 0o022 != 0 {
        return Err(failure(
            "activation journal must not be writable by group or other users",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn reject_unsafe_permissions(_file: &File) -> Result<(), JournalFailure> {
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<(), JournalFailure> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let directory = File::open(parent)
        .map_err(|error| io_failure("open activation journal parent directory", &error))?;
    directory
        .sync_all()
        .map_err(|error| io_failure("sync activation journal parent directory", &error))
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> Result<(), JournalFailure> {
    Ok(())
}

fn io_failure(context: &str, error: &std::io::Error) -> JournalFailure {
    failure(&format!("{context}: {error}"))
}

fn failure(reason: &str) -> JournalFailure {
    match JournalFailure::new(reason) {
        Ok(value) => value,
        Err(error) => unreachable!("constructed journal failure is valid: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

    fn activation_id() -> ActivationId {
        let opportunity_id = match OpportunityId::new("mine:btc:asic-0") {
            Ok(value) => value,
            Err(error) => unreachable!("valid opportunity id: {error}"),
        };
        let resource_id = match ResourceId::new("asic-0") {
            Ok(value) => value,
            Err(error) => unreachable!("valid resource id: {error}"),
        };
        ActivationId {
            decision_sequence: 7,
            opportunity_id,
            resource_id,
        }
    }

    fn test_path(label: &str) -> PathBuf {
        let nonce = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "replikan-activation-journal-{}-{label}-{nonce}.log",
            std::process::id()
        ))
    }

    fn cleanup(path: &Path) {
        let _ignored = fs::remove_file(path);
        let _ignored = fs::remove_dir(lock_path(path));
    }

    #[test]
    fn committed_state_survives_reopen() {
        let path = test_path("committed");
        cleanup(&path);
        let id = activation_id();
        {
            let mut journal = match FileActivationJournal::open(&path) {
                Ok(value) => value,
                Err(error) => unreachable!("open journal: {error}"),
            };
            assert!(journal.begin(id.clone(), 1_000).is_ok());
            assert!(journal.commit(&id, 1_100, "adapter:receipt:1").is_ok());
        }

        let journal = match FileActivationJournal::open(&path) {
            Ok(value) => value,
            Err(error) => unreachable!("reopen journal: {error}"),
        };
        assert!(matches!(
            journal.state(&id),
            Ok(Some(ActivationState::Committed {
                committed_at_unix_ms: 1_100,
                ..
            }))
        ));
        drop(journal);
        cleanup(&path);
    }

    #[test]
    fn pending_state_survives_reopen_for_fail_closed_reconciliation() {
        let path = test_path("pending");
        cleanup(&path);
        let id = activation_id();
        {
            let mut journal = match FileActivationJournal::open(&path) {
                Ok(value) => value,
                Err(error) => unreachable!("open journal: {error}"),
            };
            assert!(journal.begin(id.clone(), 1_000).is_ok());
        }

        let journal = match FileActivationJournal::open(&path) {
            Ok(value) => value,
            Err(error) => unreachable!("reopen journal: {error}"),
        };
        assert_eq!(
            journal.state(&id),
            Ok(Some(ActivationState::Pending {
                began_at_unix_ms: 1_000
            }))
        );
        drop(journal);
        cleanup(&path);
    }

    #[test]
    fn concurrent_open_is_rejected() {
        let path = test_path("lock");
        cleanup(&path);
        let first = match FileActivationJournal::open(&path) {
            Ok(value) => value,
            Err(error) => unreachable!("open journal: {error}"),
        };
        assert!(FileActivationJournal::open(&path).is_err());
        drop(first);
        assert!(FileActivationJournal::open(&path).is_ok());
        cleanup(&path);
    }

    #[test]
    fn truncated_journal_is_rejected() {
        let path = test_path("truncated");
        cleanup(&path);
        assert!(fs::write(&path, format!("{JOURNAL_HEADER}\nP|7|00")).is_ok());
        assert!(FileActivationJournal::open(&path).is_err());
        cleanup(&path);
    }

    #[test]
    fn invalid_state_transition_is_rejected_on_replay() {
        let path = test_path("transition");
        cleanup(&path);
        let id = activation_id();
        let commit_without_pending = format!(
            "{JOURNAL_HEADER}\n{}\n",
            encode_committed(&id, 1_000, 1_100, "adapter:receipt:1")
        );
        assert!(fs::write(&path, commit_without_pending).is_ok());
        assert!(FileActivationJournal::open(&path).is_err());
        cleanup(&path);
    }

    #[test]
    fn arbitrary_identifier_delimiters_round_trip_through_hex_encoding() {
        let opportunity_id = match OpportunityId::new("mine|btc\nunit") {
            Ok(value) => value,
            Err(error) => unreachable!("valid opportunity id: {error}"),
        };
        let resource_id = match ResourceId::new("asic|0") {
            Ok(value) => value,
            Err(error) => unreachable!("valid resource id: {error}"),
        };
        let id = ActivationId {
            decision_sequence: 9,
            opportunity_id,
            resource_id,
        };
        let mut states = BTreeMap::new();
        let record = encode_pending(&id, 1_000);
        assert!(parse_record(&record, &mut states).is_ok());
        assert_eq!(
            states.get(&id),
            Some(&ActivationState::Pending {
                began_at_unix_ms: 1_000
            })
        );
    }
}
