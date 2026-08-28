#![forbid(unsafe_code)]

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use replikan_execution_lease::{ExecutionAction, MiningExecutionLease};
use replikan_opportunities::OpportunityId;
use replikan_resource::ResourceId;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AdapterId(String);

impl AdapterId {
    pub fn new(value: impl Into<String>) -> Result<Self, ExecutorError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ExecutorError::EmptyAdapterId);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterDescriptor {
    pub id: AdapterId,
    pub resource_id: ResourceId,
    pub asset_symbol: String,
    pub algorithm: String,
}

impl AdapterDescriptor {
    pub fn new(
        id: AdapterId,
        resource_id: ResourceId,
        asset_symbol: impl Into<String>,
        algorithm: impl Into<String>,
    ) -> Result<Self, ExecutorError> {
        let asset_symbol = asset_symbol.into();
        let algorithm = algorithm.into();
        validate_binding_text(&asset_symbol, &algorithm)?;
        Ok(Self {
            id,
            resource_id,
            asset_symbol,
            algorithm,
        })
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ActivationId {
    pub decision_sequence: u64,
    pub opportunity_id: OpportunityId,
    pub resource_id: ResourceId,
}

impl ActivationId {
    #[must_use]
    pub fn from_lease(lease: &MiningExecutionLease) -> Self {
        Self {
            decision_sequence: lease.decision_sequence,
            opportunity_id: lease.opportunity_id.clone(),
            resource_id: lease.resource_id.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MiningActivationRequest {
    pub activation_id: ActivationId,
    pub asset_symbol: String,
    pub algorithm: String,
    pub lease_issued_at_unix_ms: u64,
    pub lease_valid_until_unix_ms: u64,
    pub requested_at_unix_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterActivation {
    evidence: String,
}

impl AdapterActivation {
    pub fn new(evidence: impl Into<String>) -> Result<Self, ExecutorError> {
        let evidence = evidence.into();
        if evidence.trim().is_empty() {
            return Err(ExecutorError::BlankActivationEvidence);
        }
        Ok(Self { evidence })
    }

    fn into_evidence(self) -> String {
        self.evidence
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterFailure(String);

impl AdapterFailure {
    pub fn new(reason: impl Into<String>) -> Result<Self, ExecutorError> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(ExecutorError::BlankAdapterFailure);
        }
        Ok(Self(reason))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Capability adapter for one explicitly registered local mining activation.
///
/// Implementations must treat `request.activation_id` as their idempotency key.
/// The interface intentionally contains no executable path, command string,
/// argv, environment map, shell, remote host, wallet, or payout parameter.
pub trait MiningActivationAdapter {
    fn descriptor(&self) -> &AdapterDescriptor;

    fn activate(
        &self,
        request: &MiningActivationRequest,
    ) -> Result<AdapterActivation, AdapterFailure>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivationState {
    Pending {
        began_at_unix_ms: u64,
    },
    Committed {
        began_at_unix_ms: u64,
        committed_at_unix_ms: u64,
        evidence: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalFailure(String);

impl JournalFailure {
    pub fn new(reason: impl Into<String>) -> Result<Self, ExecutorError> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(ExecutorError::BlankJournalFailure);
        }
        Ok(Self(reason))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Persistence boundary for activation idempotency.
///
/// Production implementations should persist this state durably. A `Pending`
/// entry is deliberately fail-closed: after a crash, Replikans must reconcile
/// the real device state before any retry for the same activation id.
pub trait ActivationJournal {
    fn state(&self, id: &ActivationId) -> Result<Option<ActivationState>, JournalFailure>;

    fn begin(&mut self, id: ActivationId, began_at_unix_ms: u64) -> Result<(), JournalFailure>;

    fn commit(
        &mut self,
        id: &ActivationId,
        committed_at_unix_ms: u64,
        evidence: &str,
    ) -> Result<(), JournalFailure>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InMemoryActivationJournal {
    states: BTreeMap<ActivationId, ActivationState>,
}

impl InMemoryActivationJournal {
    #[must_use]
    pub fn len(&self) -> usize {
        self.states.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
}

impl ActivationJournal for InMemoryActivationJournal {
    fn state(&self, id: &ActivationId) -> Result<Option<ActivationState>, JournalFailure> {
        Ok(self.states.get(id).cloned())
    }

    fn begin(&mut self, id: ActivationId, began_at_unix_ms: u64) -> Result<(), JournalFailure> {
        if self.states.contains_key(&id) {
            return Err(journal_failure("activation already has journal state"));
        }
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
            return Err(journal_failure("activation commit evidence is blank"));
        }
        let began_at_unix_ms = match self.states.get(id).cloned() {
            Some(ActivationState::Pending { began_at_unix_ms }) => began_at_unix_ms,
            Some(ActivationState::Committed { .. }) | None => {
                return Err(journal_failure("activation is not pending"));
            }
        };
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

fn journal_failure(reason: &str) -> JournalFailure {
    match JournalFailure::new(reason) {
        Ok(value) => value,
        Err(error) => unreachable!("static journal failure reason is valid: {error}"),
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct BindingKey {
    resource_id: ResourceId,
    asset_symbol: String,
    algorithm: String,
}

impl BindingKey {
    fn from_descriptor(descriptor: &AdapterDescriptor) -> Self {
        Self {
            resource_id: descriptor.resource_id.clone(),
            asset_symbol: descriptor.asset_symbol.clone(),
            algorithm: descriptor.algorithm.clone(),
        }
    }

    fn from_lease(lease: &MiningExecutionLease) -> Self {
        Self {
            resource_id: lease.resource_id.clone(),
            asset_symbol: lease.asset_symbol.clone(),
            algorithm: lease.algorithm.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionReceipt {
    pub activation_id: ActivationId,
    pub adapter_id: AdapterId,
    pub activated_at_unix_ms: u64,
    pub lease_valid_until_unix_ms: u64,
    pub evidence: String,
}

pub struct LocalExecutionRegistry<'a, J> {
    adapters: Vec<&'a dyn MiningActivationAdapter>,
    journal: J,
}

impl<'a, J> LocalExecutionRegistry<'a, J>
where
    J: ActivationJournal,
{
    pub fn new(
        adapters: Vec<&'a dyn MiningActivationAdapter>,
        journal: J,
    ) -> Result<Self, ExecutorError> {
        let mut adapter_ids = BTreeSet::new();
        let mut bindings = BTreeSet::new();

        for adapter in &adapters {
            let descriptor = adapter.descriptor();
            validate_binding_text(&descriptor.asset_symbol, &descriptor.algorithm)?;
            if !adapter_ids.insert(descriptor.id.clone()) {
                return Err(ExecutorError::DuplicateAdapterId(descriptor.id.clone()));
            }
            let binding = BindingKey::from_descriptor(descriptor);
            if !bindings.insert(binding.clone()) {
                return Err(ExecutorError::DuplicateBinding {
                    resource_id: binding.resource_id,
                    asset_symbol: binding.asset_symbol,
                    algorithm: binding.algorithm,
                });
            }
        }

        Ok(Self { adapters, journal })
    }

    #[must_use]
    pub fn adapter_count(&self) -> usize {
        self.adapters.len()
    }

    #[must_use]
    pub fn journal(&self) -> &J {
        &self.journal
    }

    pub fn dispatch(
        &mut self,
        lease: &MiningExecutionLease,
        now_unix_ms: u64,
    ) -> Result<ExecutionReceipt, ExecutorError> {
        match lease.action {
            ExecutionAction::ActivateMining => {}
        }
        if !lease.is_active_at(now_unix_ms) {
            return Err(ExecutorError::LeaseInactive);
        }

        let binding = BindingKey::from_lease(lease);
        let adapter = self
            .adapters
            .iter()
            .find(|adapter| BindingKey::from_descriptor(adapter.descriptor()) == binding)
            .ok_or_else(|| ExecutorError::NoMatchingAdapter {
                resource_id: lease.resource_id.clone(),
                asset_symbol: lease.asset_symbol.clone(),
                algorithm: lease.algorithm.clone(),
            })?;

        let activation_id = ActivationId::from_lease(lease);
        let activation_state = match self.journal.state(&activation_id) {
            Ok(value) => value,
            Err(failure) => {
                return Err(ExecutorError::JournalBeforeActivation {
                    reason: failure.as_str().to_owned(),
                });
            }
        };
        match activation_state {
            Some(ActivationState::Pending { .. }) => {
                return Err(ExecutorError::ActivationUncertain(activation_id));
            }
            Some(ActivationState::Committed { .. }) => {
                return Err(ExecutorError::LeaseAlreadyConsumed);
            }
            None => {}
        }

        if let Err(failure) = self.journal.begin(activation_id.clone(), now_unix_ms) {
            return Err(ExecutorError::JournalBeforeActivation {
                reason: failure.as_str().to_owned(),
            });
        }

        let request = MiningActivationRequest {
            activation_id: activation_id.clone(),
            asset_symbol: lease.asset_symbol.clone(),
            algorithm: lease.algorithm.clone(),
            lease_issued_at_unix_ms: lease.issued_at_unix_ms,
            lease_valid_until_unix_ms: lease.valid_until_unix_ms,
            requested_at_unix_ms: now_unix_ms,
        };
        let activation = match adapter.activate(&request) {
            Ok(value) => value,
            Err(failure) => {
                return Err(ExecutorError::AdapterFailedActivationUncertain {
                    activation_id,
                    adapter_id: adapter.descriptor().id.clone(),
                    reason: failure.as_str().to_owned(),
                });
            }
        };
        let evidence = activation.into_evidence();

        if let Err(failure) = self.journal.commit(&activation_id, now_unix_ms, &evidence) {
            return Err(ExecutorError::JournalCommitFailedAfterActivation {
                activation_id,
                evidence,
                reason: failure.as_str().to_owned(),
            });
        }

        Ok(ExecutionReceipt {
            activation_id,
            adapter_id: adapter.descriptor().id.clone(),
            activated_at_unix_ms: now_unix_ms,
            lease_valid_until_unix_ms: lease.valid_until_unix_ms,
            evidence,
        })
    }
}

fn validate_binding_text(asset_symbol: &str, algorithm: &str) -> Result<(), ExecutorError> {
    if asset_symbol.trim().is_empty() {
        return Err(ExecutorError::EmptyAssetSymbol);
    }
    if algorithm.trim().is_empty() {
        return Err(ExecutorError::EmptyAlgorithm);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutorError {
    EmptyAdapterId,
    EmptyAssetSymbol,
    EmptyAlgorithm,
    BlankActivationEvidence,
    BlankAdapterFailure,
    BlankJournalFailure,
    DuplicateAdapterId(AdapterId),
    DuplicateBinding {
        resource_id: ResourceId,
        asset_symbol: String,
        algorithm: String,
    },
    LeaseInactive,
    LeaseAlreadyConsumed,
    ActivationUncertain(ActivationId),
    NoMatchingAdapter {
        resource_id: ResourceId,
        asset_symbol: String,
        algorithm: String,
    },
    JournalBeforeActivation {
        reason: String,
    },
    AdapterFailedActivationUncertain {
        activation_id: ActivationId,
        adapter_id: AdapterId,
        reason: String,
    },
    JournalCommitFailedAfterActivation {
        activation_id: ActivationId,
        evidence: String,
        reason: String,
    },
}

impl fmt::Display for ExecutorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyAdapterId => write!(f, "adapter id cannot be empty"),
            Self::EmptyAssetSymbol => write!(f, "adapter asset symbol cannot be empty"),
            Self::EmptyAlgorithm => write!(f, "adapter algorithm cannot be empty"),
            Self::BlankActivationEvidence => write!(f, "activation evidence cannot be blank"),
            Self::BlankAdapterFailure => write!(f, "adapter failure reason cannot be blank"),
            Self::BlankJournalFailure => write!(f, "journal failure reason cannot be blank"),
            Self::DuplicateAdapterId(id) => {
                write!(f, "duplicate execution adapter id: {}", id.as_str())
            }
            Self::DuplicateBinding {
                resource_id,
                asset_symbol,
                algorithm,
            } => write!(
                f,
                "duplicate execution binding for {} {asset_symbol} {algorithm}",
                resource_id.as_str()
            ),
            Self::LeaseInactive => write!(f, "execution lease is not active"),
            Self::LeaseAlreadyConsumed => write!(f, "execution lease was already committed"),
            Self::ActivationUncertain(id) => write!(
                f,
                "activation {}:{} is pending and requires reconciliation",
                id.decision_sequence,
                id.opportunity_id.as_str()
            ),
            Self::NoMatchingAdapter {
                resource_id,
                asset_symbol,
                algorithm,
            } => write!(
                f,
                "no allowlisted adapter matches {} {asset_symbol} {algorithm}",
                resource_id.as_str()
            ),
            Self::JournalBeforeActivation { reason } => {
                write!(f, "activation journal failed before execution: {reason}")
            }
            Self::AdapterFailedActivationUncertain {
                activation_id,
                adapter_id,
                reason,
            } => write!(
                f,
                "adapter {} failed for activation {}:{}; state is uncertain: {reason}",
                adapter_id.as_str(),
                activation_id.decision_sequence,
                activation_id.opportunity_id.as_str()
            ),
            Self::JournalCommitFailedAfterActivation {
                activation_id,
                evidence,
                reason,
            } => write!(
                f,
                "activation {}:{} succeeded with evidence {evidence}, but journal commit failed: {reason}",
                activation_id.decision_sequence,
                activation_id.opportunity_id.as_str()
            ),
        }
    }
}

impl std::error::Error for ExecutorError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn adapter_id(value: &str) -> AdapterId {
        match AdapterId::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid adapter id: {error}"),
        }
    }

    fn opportunity_id(value: &str) -> OpportunityId {
        match OpportunityId::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid opportunity id: {error}"),
        }
    }

    fn resource_id(value: &str) -> ResourceId {
        match ResourceId::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid resource id: {error}"),
        }
    }

    fn descriptor(id: &str, resource: &str, asset: &str, algorithm: &str) -> AdapterDescriptor {
        match AdapterDescriptor::new(adapter_id(id), resource_id(resource), asset, algorithm) {
            Ok(value) => value,
            Err(error) => unreachable!("valid descriptor: {error}"),
        }
    }

    fn lease(resource: &str, asset: &str, algorithm: &str) -> MiningExecutionLease {
        MiningExecutionLease {
            decision_sequence: 7,
            opportunity_id: opportunity_id("mine:btc:asic-0"),
            resource_id: resource_id(resource),
            action: ExecutionAction::ActivateMining,
            asset_symbol: asset.to_owned(),
            algorithm: algorithm.to_owned(),
            issued_at_unix_ms: 1_000,
            valid_until_unix_ms: 2_000,
            evidence: vec!["decision:7".to_owned()],
        }
    }

    struct FakeAdapter {
        descriptor: AdapterDescriptor,
        calls: Cell<u32>,
        fail: bool,
    }

    impl FakeAdapter {
        fn successful(descriptor: AdapterDescriptor) -> Self {
            Self {
                descriptor,
                calls: Cell::new(0),
                fail: false,
            }
        }

        fn failing(descriptor: AdapterDescriptor) -> Self {
            Self {
                descriptor,
                calls: Cell::new(0),
                fail: true,
            }
        }
    }

    impl MiningActivationAdapter for FakeAdapter {
        fn descriptor(&self) -> &AdapterDescriptor {
            &self.descriptor
        }

        fn activate(
            &self,
            _request: &MiningActivationRequest,
        ) -> Result<AdapterActivation, AdapterFailure> {
            self.calls.set(self.calls.get() + 1);
            if self.fail {
                let failure = match AdapterFailure::new("device activation outcome unknown") {
                    Ok(value) => value,
                    Err(error) => unreachable!("valid failure: {error}"),
                };
                return Err(failure);
            }
            match AdapterActivation::new("adapter:activation:receipt-1") {
                Ok(value) => Ok(value),
                Err(error) => unreachable!("valid activation: {error}"),
            }
        }
    }

    fn registry<'a>(
        adapter: &'a FakeAdapter,
    ) -> LocalExecutionRegistry<'a, InMemoryActivationJournal> {
        match LocalExecutionRegistry::new(vec![adapter], InMemoryActivationJournal::default()) {
            Ok(value) => value,
            Err(error) => unreachable!("valid registry: {error}"),
        }
    }

    #[test]
    fn expired_lease_is_rejected_without_calling_adapter() {
        let adapter =
            FakeAdapter::successful(descriptor("asic-adapter", "asic-0", "BTC", "sha256d"));
        let mut registry = registry(&adapter);

        assert_eq!(
            registry.dispatch(&lease("asic-0", "BTC", "sha256d"), 2_001),
            Err(ExecutorError::LeaseInactive)
        );
        assert_eq!(adapter.calls.get(), 0);
        assert!(registry.journal().is_empty());
    }

    #[test]
    fn exact_binding_is_required_before_journal_or_adapter_call() {
        let adapter =
            FakeAdapter::successful(descriptor("asic-adapter", "asic-0", "BTC", "sha256d"));
        let mut registry = registry(&adapter);

        assert!(matches!(
            registry.dispatch(&lease("asic-0", "BTC", "scrypt"), 1_500),
            Err(ExecutorError::NoMatchingAdapter { .. })
        ));
        assert_eq!(adapter.calls.get(), 0);
        assert!(registry.journal().is_empty());
    }

    #[test]
    fn duplicate_bindings_fail_closed() {
        let first = FakeAdapter::successful(descriptor("first", "asic-0", "BTC", "sha256d"));
        let second = FakeAdapter::successful(descriptor("second", "asic-0", "BTC", "sha256d"));

        assert!(matches!(
            LocalExecutionRegistry::new(
                vec![&first, &second],
                InMemoryActivationJournal::default()
            ),
            Err(ExecutorError::DuplicateBinding { .. })
        ));
    }

    #[test]
    fn successful_dispatch_commits_and_cannot_replay() {
        let adapter =
            FakeAdapter::successful(descriptor("asic-adapter", "asic-0", "BTC", "sha256d"));
        let mut registry = registry(&adapter);
        let lease = lease("asic-0", "BTC", "sha256d");
        let activation_id = ActivationId::from_lease(&lease);

        let receipt = match registry.dispatch(&lease, 1_500) {
            Ok(value) => value,
            Err(error) => unreachable!("valid dispatch: {error}"),
        };
        assert_eq!(receipt.adapter_id.as_str(), "asic-adapter");
        assert_eq!(receipt.activation_id, activation_id);
        assert_eq!(receipt.evidence, "adapter:activation:receipt-1");
        assert_eq!(adapter.calls.get(), 1);
        assert!(matches!(
            registry.journal().state(&activation_id),
            Ok(Some(ActivationState::Committed { .. }))
        ));

        assert_eq!(
            registry.dispatch(&lease, 1_501),
            Err(ExecutorError::LeaseAlreadyConsumed)
        );
        assert_eq!(adapter.calls.get(), 1);
    }

    #[test]
    fn adapter_failure_leaves_pending_state_and_blocks_automatic_retry() {
        let adapter = FakeAdapter::failing(descriptor("asic-adapter", "asic-0", "BTC", "sha256d"));
        let mut registry = registry(&adapter);
        let lease = lease("asic-0", "BTC", "sha256d");
        let activation_id = ActivationId::from_lease(&lease);

        assert!(matches!(
            registry.dispatch(&lease, 1_500),
            Err(ExecutorError::AdapterFailedActivationUncertain { .. })
        ));
        assert_eq!(adapter.calls.get(), 1);
        assert!(matches!(
            registry.journal().state(&activation_id),
            Ok(Some(ActivationState::Pending { .. }))
        ));

        assert_eq!(
            registry.dispatch(&lease, 1_501),
            Err(ExecutorError::ActivationUncertain(activation_id))
        );
        assert_eq!(adapter.calls.get(), 1);
    }

    #[test]
    fn preexisting_pending_state_blocks_adapter_call() {
        let adapter =
            FakeAdapter::successful(descriptor("asic-adapter", "asic-0", "BTC", "sha256d"));
        let lease = lease("asic-0", "BTC", "sha256d");
        let activation_id = ActivationId::from_lease(&lease);
        let mut journal = InMemoryActivationJournal::default();
        assert!(journal.begin(activation_id.clone(), 1_400).is_ok());
        let mut registry = match LocalExecutionRegistry::new(vec![&adapter], journal) {
            Ok(value) => value,
            Err(error) => unreachable!("valid registry: {error}"),
        };

        assert_eq!(
            registry.dispatch(&lease, 1_500),
            Err(ExecutorError::ActivationUncertain(activation_id))
        );
        assert_eq!(adapter.calls.get(), 0);
    }

    struct CommitFailJournal {
        inner: InMemoryActivationJournal,
    }

    impl ActivationJournal for CommitFailJournal {
        fn state(&self, id: &ActivationId) -> Result<Option<ActivationState>, JournalFailure> {
            self.inner.state(id)
        }

        fn begin(&mut self, id: ActivationId, began_at_unix_ms: u64) -> Result<(), JournalFailure> {
            self.inner.begin(id, began_at_unix_ms)
        }

        fn commit(
            &mut self,
            _id: &ActivationId,
            _committed_at_unix_ms: u64,
            _evidence: &str,
        ) -> Result<(), JournalFailure> {
            Err(journal_failure("simulated durable commit failure"))
        }
    }

    #[test]
    fn commit_failure_after_activation_preserves_pending_reconciliation_state() {
        let adapter =
            FakeAdapter::successful(descriptor("asic-adapter", "asic-0", "BTC", "sha256d"));
        let journal = CommitFailJournal {
            inner: InMemoryActivationJournal::default(),
        };
        let mut registry = match LocalExecutionRegistry::new(vec![&adapter], journal) {
            Ok(value) => value,
            Err(error) => unreachable!("valid registry: {error}"),
        };
        let lease = lease("asic-0", "BTC", "sha256d");
        let activation_id = ActivationId::from_lease(&lease);

        assert!(matches!(
            registry.dispatch(&lease, 1_500),
            Err(ExecutorError::JournalCommitFailedAfterActivation { .. })
        ));
        assert_eq!(adapter.calls.get(), 1);
        assert!(matches!(
            registry.journal().state(&activation_id),
            Ok(Some(ActivationState::Pending { .. }))
        ));
    }

    #[test]
    fn duplicate_adapter_ids_fail_closed_even_for_different_bindings() {
        let first = FakeAdapter::successful(descriptor("same", "asic-0", "BTC", "sha256d"));
        let second = FakeAdapter::successful(descriptor("same", "asic-1", "BTC", "sha256d"));

        assert_eq!(
            LocalExecutionRegistry::new(
                vec![&first, &second],
                InMemoryActivationJournal::default()
            )
            .err(),
            Some(ExecutorError::DuplicateAdapterId(adapter_id("same")))
        );
    }
}
