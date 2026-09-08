//! Durable, single-account spot execution built on SciRust's exact contracts.
//! No custody or live transport is enabled. SQLite is the journal authority;
//! network attempts are claimed durably before any adapter call.
#![forbid(unsafe_code)]

pub mod paper;
pub use scirust_trader::{execution_v2, financial, orders};

use execution_v2::{
    ApplyOutcomeV2, ExecutionEventKindV2, ExecutionEventV2, ExecutionOrderV2, ExecutionStatusV2,
    FillRecordV2, LifecycleBookV2,
};
use financial::{
    ExactInstrumentRules, ExactOrderRequest, ExactOrderType, NonNegativeMoney, Quantity,
    ReferencePrice, SignedAmount, validate_order,
};
use orders::Side;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error(pub String);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self(error.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self(error.to_string())
    }
}
impl From<financial::FinancialError> for Error {
    fn from(error: financial::FinancialError) -> Self {
        Self(error.to_string())
    }
}

fn domain<T>(value: std::result::Result<T, execution_v2::LifecycleV2Error>) -> Result<T> {
    value.map_err(|error| Error(format!("lifecycle: {error:?}")))
}

/// Trusted operator configuration; it is never read from an agent tool argument.
/// These are paper qualification limits, not the existing mining policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub venue: String,
    pub account_id: String,
    pub instruments: BTreeMap<String, ExactInstrumentRules>,
    pub initial_balances: BTreeMap<String, SignedAmount>,
    pub max_quantity: Quantity,
    pub max_order_notional: NonNegativeMoney,
    pub max_open_orders: usize,
    pub max_intent_age_ms: i64,
    /// Flat quote-asset fee used by the deterministic paper adapter and reserved
    /// before send. It is not represented as an exchange fee schedule.
    pub paper_quote_fee: NonNegativeMoney,
}

impl Config {
    fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.venue != "paper"
            || self.account_id.trim().is_empty()
            || self.instruments.is_empty()
            || self.max_open_orders == 0
            || self.max_intent_age_ms <= 0
        {
            return Err(Error(
                "invalid configuration; only paper mode is enabled".into(),
            ));
        }
        for (id, rules) in &self.instruments {
            rules.validate()?;
            if id != &rules.instrument_id || rules.venue != self.venue {
                return Err(Error("instrument configuration identity mismatch".into()));
            }
        }
        if self
            .initial_balances
            .iter()
            .any(|(asset, value)| asset.trim().is_empty() || value.is_negative())
        {
            return Err(Error("invalid opening balance".into()));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub intent_id: String,
    pub idempotency_key: String,
    pub agent_id: String,
    pub client_order_id: String,
    pub decision_id: String,
    pub strategy_version: String,
    pub evidence_refs: Vec<String>,
    pub rationale: String,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub request: ExactOrderRequest,
    pub reference: ReferencePrice,
}

/// Transport returns observations, never an arbitrary replacement local state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub native_sequence: Option<u64>,
    pub received_at_ms: i64,
    pub kind: ExecutionEventKindV2,
}

/// Implementations are trusted runtime code. Agent-facing commands cannot
/// supply arbitrary receipts. A query returning None never authorizes resubmit.
pub trait VenueAdapter {
    fn identity(&self) -> (&str, &str);
    fn submit(&mut self, intent: &Intent, config: &Config, now_ms: i64)
    -> Result<Vec<Observation>>;
    fn query(&mut self, client_order_id: &str, now_ms: i64) -> Result<Option<Vec<Observation>>>;
    fn cancel(&mut self, client_order_id: &str, now_ms: i64) -> Result<Vec<Observation>>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
enum Record {
    Intent(Box<Intent>),
    Dispatch {
        client_order_id: String,
    },
    Observation {
        client_order_id: String,
        value: Box<Observation>,
    },
}

#[derive(Clone)]
struct State {
    book: LifecycleBookV2,
    intents: BTreeMap<String, Intent>,
    dispatched: BTreeSet<String>,
    observations: BTreeSet<String>,
    balances: BTreeMap<String, SignedAmount>,
    sequence: i64,
    hash: String,
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub mode: &'static str,
    pub venue: String,
    pub account_id: String,
    pub journal_sequence: i64,
    pub journal_hash: String,
    pub balances: BTreeMap<String, SignedAmount>,
    pub orders: Vec<ExecutionOrderV2>,
    /// Vector deliberately avoids JSON object keys made from composite FillKey.
    pub fills: Vec<FillRecordV2>,
    pub recovery_required: Vec<String>,
}

fn digest(previous: &str, sequence: i64, payload: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"replikans.trading.journal.v1\0");
    hash.update(previous.as_bytes());
    hash.update(sequence.to_le_bytes());
    hash.update(payload.as_bytes());
    format!("{:x}", hash.finalize())
}

fn balance_change(
    balances: &mut BTreeMap<String, SignedAmount>,
    asset: &str,
    delta: SignedAmount,
) -> Result<()> {
    let value = balances
        .get(asset)
        .copied()
        .unwrap_or(SignedAmount::ZERO)
        .checked_add(delta)?;
    balances.insert(asset.to_owned(), value);
    Ok(())
}

impl State {
    fn empty(config: &Config) -> Result<Self> {
        Ok(Self {
            book: domain(LifecycleBookV2::new(&config.venue, &config.account_id))?,
            intents: BTreeMap::new(),
            dispatched: BTreeSet::new(),
            observations: BTreeSet::new(),
            balances: config.initial_balances.clone(),
            sequence: 0,
            hash: digest("", 0, &serde_json::to_string(config)?),
        })
    }

    fn apply(&mut self, record: &Record, config: &Config) -> Result<()> {
        match record {
            Record::Intent(intent) => {
                domain(self.book.register_intent(
                    intent.intent_id.clone(),
                    intent.idempotency_key.clone(),
                    intent.agent_id.clone(),
                    intent.client_order_id.clone(),
                    intent.request.clone(),
                    intent.created_at_ms,
                ))?;
                self.intents
                    .insert(intent.client_order_id.clone(), *intent.clone());
            }
            Record::Dispatch { client_order_id } => {
                let order = self
                    .book
                    .orders
                    .get(client_order_id)
                    .ok_or_else(|| Error("unknown order".into()))?;
                if order.status != ExecutionStatusV2::PendingSubmit
                    || !self.dispatched.insert(client_order_id.clone())
                {
                    return Err(Error(
                        "submission already claimed or no longer pending".into(),
                    ));
                }
            }
            Record::Observation {
                client_order_id,
                value,
            } => {
                let receipt_identity = serde_json::to_string(&(client_order_id, value))?;
                if self.observations.contains(&receipt_identity) {
                    return Ok(());
                }
                let event = ExecutionEventV2 {
                    local_sequence: self
                        .book
                        .last_local_sequence
                        .checked_add(1)
                        .ok_or_else(|| Error("sequence overflow".into()))?,
                    native_sequence: value.native_sequence,
                    received_at_ms: value.received_at_ms,
                    client_order_id: client_order_id.clone(),
                    kind: value.kind.clone(),
                };
                let outcome = domain(self.book.apply_event(event))?;
                self.observations.insert(receipt_identity);
                if outcome != ApplyOutcomeV2::DuplicateFill
                    && let ExecutionEventKindV2::Fill {
                        price,
                        quantity,
                        fee_asset,
                        fee_amount,
                        ..
                    } = &value.kind
                {
                    let intent = self
                        .intents
                        .get(client_order_id)
                        .ok_or_else(|| Error("missing intent".into()))?;
                    let rules = config
                        .instruments
                        .get(&intent.request.instrument_id)
                        .ok_or_else(|| Error("missing instrument".into()))?;
                    let notional = SignedAmount::from(price.checked_notional(*quantity)?);
                    let quantity = SignedAmount::from(*quantity);
                    let (base, quote) = match intent.request.side {
                        Side::Buy => (quantity, SignedAmount::ZERO.checked_sub(notional)?),
                        Side::Sell => (SignedAmount::ZERO.checked_sub(quantity)?, notional),
                    };
                    balance_change(&mut self.balances, &rules.base_asset, base)?;
                    balance_change(&mut self.balances, &rules.quote_asset, quote)?;
                    balance_change(
                        &mut self.balances,
                        fee_asset,
                        SignedAmount::ZERO.checked_sub(*fee_amount)?,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn available(&self, config: &Config) -> Result<BTreeMap<String, SignedAmount>> {
        let mut available = self.balances.clone();
        for (id, order) in &self.book.orders {
            if order.status.is_terminal() {
                continue;
            }
            let intent = self
                .intents
                .get(id)
                .ok_or_else(|| Error("missing intent".into()))?;
            // Reserve the original full amount until terminal confirmation.
            // This is deliberately conservative for partial fills.
            for (asset, amount) in requirements(intent, config)? {
                balance_change(
                    &mut available,
                    &asset,
                    SignedAmount::ZERO.checked_sub(amount)?,
                )?;
            }
        }
        Ok(available)
    }
}

fn requirements(intent: &Intent, config: &Config) -> Result<BTreeMap<String, SignedAmount>> {
    let rules = config
        .instruments
        .get(&intent.request.instrument_id)
        .ok_or_else(|| Error("unsupported instrument".into()))?;
    let price = match intent.request.order_type {
        ExactOrderType::Market => intent.reference.price,
        ExactOrderType::Limit { price } => price,
        _ => return Err(Error("paper runtime supports market and limit only".into())),
    };
    let notional = price.checked_notional(intent.request.quantity)?;
    if notional > config.max_order_notional {
        return Err(Error("order notional exceeds configured policy".into()));
    }
    let mut amounts = BTreeMap::new();
    match intent.request.side {
        Side::Buy => {
            amounts.insert(rules.quote_asset.clone(), SignedAmount::from(notional));
        }
        Side::Sell => {
            amounts.insert(
                rules.base_asset.clone(),
                SignedAmount::from(intent.request.quantity),
            );
        }
    }
    balance_change(
        &mut amounts,
        &rules.quote_asset,
        SignedAmount::from(config.paper_quote_fee),
    )?;
    Ok(amounts)
}

fn authorize(intent: &Intent, config: &Config, now_ms: i64) -> Result<()> {
    if [
        &intent.intent_id,
        &intent.idempotency_key,
        &intent.agent_id,
        &intent.client_order_id,
        &intent.decision_id,
        &intent.strategy_version,
        &intent.rationale,
    ]
    .iter()
    .any(|v| v.trim().is_empty())
        || intent.evidence_refs.is_empty()
        || intent.evidence_refs.iter().any(|v| v.trim().is_empty())
        || intent.created_at_ms > now_ms
        || intent.expires_at_ms < now_ms
        || intent.expires_at_ms < intent.created_at_ms
        || now_ms
            .checked_sub(intent.created_at_ms)
            .is_none_or(|age| age > config.max_intent_age_ms)
        || intent.request.quantity > config.max_quantity
        || intent.request.reduce_only
        || intent.request.post_only
        || intent.request.tif != orders::TimeInForce::Gtc
    {
        return Err(Error(
            "intent rejected by paper policy, validity or unsupported order flags".into(),
        ));
    }
    let rules = config
        .instruments
        .get(&intent.request.instrument_id)
        .ok_or_else(|| Error("unsupported instrument".into()))?;
    intent.reference.validate_at(now_ms)?;
    if intent.reference.venue != config.venue
        || intent.reference.instrument_id != rules.instrument_id
    {
        return Err(Error("reference identity mismatch".into()));
    }
    validate_order(
        &intent.request,
        rules,
        Some(intent.reference.clone()),
        now_ms,
    )?;
    requirements(intent, config)?;
    Ok(())
}

/// Runtime storage. Transactions reload the journal to avoid stale projections
/// when multiple processes share the same database. No database lock is held
/// while calling an adapter. A failed append never updates a cached projection.
pub struct Runtime {
    connection: Connection,
    config: Config,
}

pub(crate) fn connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
    )?;
    Ok(connection)
}

impl Runtime {
    pub fn open(path: impl AsRef<Path>, config: Config) -> Result<Self> {
        config.validate()?;
        let mut connection = connection(path.as_ref())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch("CREATE TABLE IF NOT EXISTS trading_config (id INTEGER PRIMARY KEY CHECK(id=1), json TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS trading_journal (sequence INTEGER PRIMARY KEY, payload TEXT NOT NULL, hash TEXT NOT NULL);")?;
        let json = serde_json::to_string(&config)?;
        transaction.execute(
            "INSERT OR IGNORE INTO trading_config (id,json) VALUES (1,?1)",
            [&json],
        )?;
        let stored: String =
            transaction.query_row("SELECT json FROM trading_config WHERE id=1", [], |row| {
                row.get(0)
            })?;
        if stored != json {
            return Err(Error(
                "configuration mismatch; implicit state migration refused".into(),
            ));
        }
        replay(&transaction, &config)?;
        transaction.commit()?;
        Ok(Self { connection, config })
    }

    pub fn snapshot(&mut self) -> Result<Snapshot> {
        let transaction = self.connection.transaction()?;
        let state = replay(&transaction, &self.config)?;
        let recovery_required = state
            .book
            .orders
            .iter()
            .filter(|(id, order)| {
                state.dispatched.contains(*id)
                    && (order.status == ExecutionStatusV2::PendingSubmit
                        || order.status.is_ambiguous()
                        || matches!(
                            order.status,
                            ExecutionStatusV2::PendingCancel | ExecutionStatusV2::PendingAmend
                        ))
            })
            .map(|(id, _)| id.clone())
            .collect();
        Ok(Snapshot {
            schema_version: 1,
            mode: "paper",
            venue: self.config.venue.clone(),
            account_id: self.config.account_id.clone(),
            journal_sequence: state.sequence,
            journal_hash: state.hash,
            balances: state.balances,
            orders: state.book.orders.into_values().collect(),
            fills: state.book.fills.into_values().collect(),
            recovery_required,
        })
    }

    /// Prepare without sending. A replay of the identical intent is idempotent;
    /// reusing any identity with changed content is an error.
    pub fn prepare(&mut self, intent: Intent, now_ms: i64) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut state = replay(&transaction, &self.config)?;
        if let Some(existing) = state.intents.get(&intent.client_order_id) {
            if existing == &intent {
                return Ok(());
            }
            return Err(Error(
                "client order identity reused with changed content".into(),
            ));
        }
        authorize(&intent, &self.config, now_ms)?;
        if state.balances.values().any(|balance| balance.is_negative()) {
            return Err(Error("unresolved account deficit".into()));
        }
        if state
            .book
            .orders
            .values()
            .filter(|order| !order.status.is_terminal())
            .count()
            >= self.config.max_open_orders
        {
            return Err(Error("maximum open orders reached".into()));
        }
        let available = state.available(&self.config)?;
        for (asset, amount) in requirements(&intent, &self.config)? {
            if available.get(&asset).copied().unwrap_or(SignedAmount::ZERO) < amount {
                return Err(Error(format!("insufficient unreserved balance: {asset}")));
            }
        }
        append(
            &transaction,
            &mut state,
            &self.config,
            Record::Intent(Box::new(intent)),
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn check_adapter(&self, adapter: &dyn VenueAdapter) -> Result<()> {
        if adapter.identity() != (self.config.venue.as_str(), self.config.account_id.as_str()) {
            return Err(Error("adapter account or venue mismatch".into()));
        }
        Ok(())
    }

    /// The durable claim is committed BEFORE invoking submit. Recovery never
    /// repeats this call, including a crash between claim and actual network send.
    pub fn dispatch(
        &mut self,
        id: &str,
        adapter: &mut dyn VenueAdapter,
        now_ms: i64,
    ) -> Result<()> {
        self.check_adapter(adapter)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut state = replay(&transaction, &self.config)?;
        let intent = state
            .intents
            .get(id)
            .cloned()
            .ok_or_else(|| Error("unknown intent".into()))?;
        authorize(&intent, &self.config, now_ms)?;
        if state
            .available(&self.config)?
            .values()
            .any(|balance| balance.is_negative())
        {
            return Err(Error("unresolved balance or reservation deficit".into()));
        }
        append(
            &transaction,
            &mut state,
            &self.config,
            Record::Dispatch {
                client_order_id: id.to_owned(),
            },
        )?;
        transaction.commit()?;
        match adapter.submit(&intent, &self.config, now_ms) {
            Ok(observations) => self.record_observations(id, observations),
            Err(error) => {
                self.record_observations(
                    id,
                    vec![Observation {
                        native_sequence: None,
                        received_at_ms: now_ms,
                        kind: ExecutionEventKindV2::SubmitUnknown {
                            reason: "adapter did not return a definitive outcome".into(),
                        },
                    }],
                )?;
                Err(error)
            }
        }
    }

    pub fn reconcile(
        &mut self,
        id: &str,
        adapter: &mut dyn VenueAdapter,
        now_ms: i64,
    ) -> Result<()> {
        self.check_adapter(adapter)?;
        let observations = adapter.query(id, now_ms)?.ok_or_else(|| {
            Error("order not found; absence is not permission to resubmit".into())
        })?;
        self.record_observations(id, observations)
    }

    pub fn cancel(&mut self, id: &str, adapter: &mut dyn VenueAdapter, now_ms: i64) -> Result<()> {
        self.check_adapter(adapter)?;
        self.record_observations(
            id,
            vec![Observation {
                native_sequence: None,
                received_at_ms: now_ms,
                kind: ExecutionEventKindV2::CancelRequested,
            }],
        )?;
        match adapter.cancel(id, now_ms) {
            Ok(observations) => self.record_observations(id, observations),
            Err(error) => {
                self.record_observations(
                    id,
                    vec![Observation {
                        native_sequence: None,
                        received_at_ms: now_ms,
                        kind: ExecutionEventKindV2::CancelUnknown {
                            reason: "adapter did not confirm cancellation".into(),
                        },
                    }],
                )?;
                Err(error)
            }
        }
    }

    /// Trusted adapter ingestion, not exposed as an agent JSON command.
    pub fn record_observations(&mut self, id: &str, observations: Vec<Observation>) -> Result<()> {
        if observations.is_empty() {
            return Err(Error("empty adapter receipt".into()));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut state = replay(&transaction, &self.config)?;
        if !state.dispatched.contains(id) {
            return Err(Error("receipt for an undispatched intent".into()));
        }
        for value in observations {
            append(
                &transaction,
                &mut state,
                &self.config,
                Record::Observation {
                    client_order_id: id.to_owned(),
                    value: Box::new(value),
                },
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Contains decisions and receipts, no signing material. Hash chain detects
    /// internal alteration, not a privileged rewrite of the entire database.
    pub fn export(&mut self) -> Result<serde_json::Value> {
        let transaction = self.connection.transaction()?;
        let state = replay(&transaction, &self.config)?;
        let mut query = transaction
            .prepare("SELECT sequence,payload,hash FROM trading_journal ORDER BY sequence")?;
        let rows = query.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (sequence, payload, hash) = row?;
            entries.push(serde_json::json!({"sequence":sequence,"record":serde_json::from_str::<serde_json::Value>(&payload)?,"hash":hash}));
        }
        Ok(
            serde_json::json!({"schema_version":1,"config":self.config,"journal_hash":state.hash,"entries":entries}),
        )
    }
}

fn replay(connection: &Connection, config: &Config) -> Result<State> {
    let mut state = State::empty(config)?;
    let mut query = connection
        .prepare("SELECT sequence,payload,hash FROM trading_journal ORDER BY sequence")?;
    let rows = query.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (sequence, payload, hash) = row?;
        if sequence
            != state
                .sequence
                .checked_add(1)
                .ok_or_else(|| Error("sequence overflow".into()))?
            || sequence > 1_000_000
            || payload.len() > 1_048_576
            || hash != digest(&state.hash, sequence, &payload)
        {
            return Err(Error("journal integrity or replay budget violation".into()));
        }
        state.apply(&serde_json::from_str(&payload)?, config)?;
        state.sequence = sequence;
        state.hash = hash;
    }
    Ok(state)
}

fn append(
    connection: &Connection,
    state: &mut State,
    config: &Config,
    record: Record,
) -> Result<()> {
    let payload = serde_json::to_string(&record)?;
    let sequence = state
        .sequence
        .checked_add(1)
        .ok_or_else(|| Error("sequence overflow".into()))?;
    if payload.len() > 1_048_576 || sequence > 1_000_000 {
        return Err(Error("journal budget exceeded".into()));
    }
    let mut candidate = state.clone();
    candidate.apply(&record, config)?;
    let hash = digest(&state.hash, sequence, &payload);
    connection.execute(
        "INSERT INTO trading_journal(sequence,payload,hash) VALUES (?1,?2,?3)",
        params![sequence, payload, hash],
    )?;
    candidate.sequence = sequence;
    candidate.hash = hash;
    *state = candidate;
    Ok(())
}
