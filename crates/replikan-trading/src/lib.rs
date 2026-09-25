//! Durable, single-account spot execution built on SciRust's exact contracts.
//! No custody or live transport is enabled. SQLite is the journal authority;
//! network attempts are claimed durably before any adapter call.
#![forbid(unsafe_code)]

pub mod market;
pub mod mission;
pub mod paper;
pub mod protection;
pub use scirust_trader::{execution_v2, financial, orders};

use execution_v2::{
    ApplyOutcomeV2, ExecutionEventKindV2, ExecutionEventV2, ExecutionOrderV2, ExecutionStatusV2,
    FillRecordV2, LifecycleBookV2,
};
use financial::{
    ExactInstrumentRules, ExactOrderRequest, ExactOrderType, NonNegativeMoney, Quantity,
    ReferencePrice, SignedAmount, validate_order,
};
use mission::{MissionProgress, MissionReport, MissionStop, TradingMission};
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
    /// Optional operator mandate. Omission preserves the legacy journal hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission: Option<TradingMission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_data: Option<market::MarketDataPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protection: Option<protection::ProtectionPolicy>,
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
        if let Some(mission) = &self.mission {
            mission.validate(self)?;
        }
        if let Some(policy) = &self.market_data {
            policy.validate(self)?;
        }
        if let Some(policy) = &self.protection {
            policy.validate(self)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_snapshot_id: Option<String>,
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
    ProtectionCheck {
        now_ms: i64,
    },
    MarketSnapshot(Box<market::MarketSnapshot>),
    Intent(Box<Intent>),
    Abandon {
        client_order_id: String,
        reason: String,
        recorded_at_ms: i64,
    },
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
    latest_market: Option<market::MarketSnapshot>,
    market_claims: BTreeSet<String>,
    book: LifecycleBookV2,
    intents: BTreeMap<String, Intent>,
    dispatched: BTreeSet<String>,
    observations: BTreeSet<String>,
    balances: BTreeMap<String, SignedAmount>,
    sequence: i64,
    hash: String,
    mission_progress: MissionProgress,
    protection_progress: protection::ProtectionProgress,
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
            latest_market: None,
            market_claims: BTreeSet::new(),
            book: domain(LifecycleBookV2::new(&config.venue, &config.account_id))?,
            intents: BTreeMap::new(),
            dispatched: BTreeSet::new(),
            observations: BTreeSet::new(),
            balances: config.initial_balances.clone(),
            sequence: 0,
            hash: digest("", 0, &serde_json::to_string(config)?),
            mission_progress: MissionProgress::default(),
            protection_progress: protection::ProtectionProgress::default(),
        })
    }

    fn apply(&mut self, record: &Record, config: &Config) -> Result<()> {
        match record {
            Record::ProtectionCheck { now_ms } => {
                if *now_ms < 0 || config.protection.is_none() {
                    return Err(Error("invalid protection check".into()));
                }
                self.evaluate_protection(config, *now_ms)?;
            }
            Record::MarketSnapshot(quote) => {
                let policy = config
                    .market_data
                    .as_ref()
                    .ok_or_else(|| Error("market collection is not configured".into()))?;
                quote.validate(policy)?;
                if self.latest_market.as_ref().is_some_and(|previous| {
                    previous
                        .received_at_ms
                        .checked_add(policy.min_refresh_interval_ms)
                        .is_none_or(|earliest| quote.request_started_at_ms < earliest)
                }) {
                    return Err(Error(
                        "market observations overlap, regress or exceed refresh cadence".into(),
                    ));
                }
                self.latest_market = Some(*quote.clone());
                self.evaluate_protection(config, quote.received_at_ms)?;
            }
            Record::Abandon {
                client_order_id,
                reason,
                recorded_at_ms,
            } => {
                let intent = self
                    .intents
                    .get(client_order_id)
                    .ok_or_else(|| Error("unknown intent".into()))?;
                if self.dispatched.contains(client_order_id)
                    || reason.trim().is_empty()
                    || *recorded_at_ms < intent.created_at_ms
                    || self
                        .book
                        .orders
                        .get(client_order_id)
                        .is_none_or(|order| order.status != ExecutionStatusV2::PendingSubmit)
                {
                    return Err(Error("only an undispatched intent can be abandoned".into()));
                }
                self.apply(
                    &Record::Observation {
                        client_order_id: client_order_id.clone(),
                        value: Box::new(Observation {
                            native_sequence: None,
                            received_at_ms: *recorded_at_ms,
                            kind: ExecutionEventKindV2::SubmitRejected {
                                reason: format!("local abandonment: {reason}"),
                            },
                        }),
                    },
                    config,
                )?;
            }
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
                if let Some(quote_id) = &self.intents[client_order_id].market_snapshot_id {
                    let side = self.intents[client_order_id].request.side;
                    if !self.market_claims.insert(format!("{quote_id}:{side:?}")) {
                        return Err(Error("snapshot side already consumed by a dispatch".into()));
                    }
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
                    if let Some(mission) = &config.mission {
                        let quote_fee = if fee_asset == &rules.quote_asset {
                            *fee_amount
                        } else {
                            SignedAmount::ZERO
                        };
                        let buy_spend = if intent.request.side == Side::Buy {
                            // Rebates never replenish the gross purchase budget.
                            notional.checked_add(quote_fee.max(SignedAmount::ZERO))?
                        } else {
                            SignedAmount::ZERO
                        };
                        let stop = if fee_asset != &rules.quote_asset {
                            Some(MissionStop::UnsupportedFeeAsset)
                        } else if self.balances.values().any(|v| v.is_negative()) {
                            Some(MissionStop::BalanceDeficit)
                        } else {
                            None
                        };
                        self.mission_progress.observe(
                            mission,
                            quote.checked_sub(quote_fee)?,
                            buy_spend,
                            quote_fee,
                            self.balances
                                .get(&rules.base_asset)
                                .copied()
                                .unwrap_or(SignedAmount::ZERO),
                            stop,
                        )?;
                        self.evaluate_protection(config, value.received_at_ms)?;
                    }
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

    fn mission_reservations(
        &self,
        config: &Config,
        excluding: Option<&str>,
    ) -> Result<SignedAmount> {
        let mut reserved = SignedAmount::ZERO;
        for (id, order) in &self.book.orders {
            if order.status.is_terminal() || excluding == Some(id.as_str()) {
                continue;
            }
            let intent = self
                .intents
                .get(id)
                .ok_or_else(|| Error("missing intent".into()))?;
            if intent.request.side == Side::Buy {
                for amount in requirements(intent, config)?.values() {
                    reserved = reserved.checked_add(*amount)?;
                }
            }
        }
        Ok(reserved)
    }

    fn authorize_market(&self, intent: &Intent, config: &Config, now_ms: i64) -> Result<()> {
        let Some(policy) = &config.market_data else {
            if intent.market_snapshot_id.is_some() {
                return Err(Error(
                    "market snapshot supplied without configured collector".into(),
                ));
            }
            return Ok(());
        };
        let quote = self
            .latest_market
            .as_ref()
            .ok_or_else(|| Error("no collected market snapshot".into()))?;
        quote.validate_at(policy, now_ms)?;
        if intent.market_snapshot_id.as_deref() != Some(quote.id())
            || intent.reference != quote.reference(policy, intent.request.side)?
            || intent.request.order_type != ExactOrderType::Market
            || intent.request.quantity > quote.capacity(intent.request.side)
            || self
                .market_claims
                .contains(&format!("{}:{:?}", quote.id(), intent.request.side))
        {
            return Err(Error("intent must use the current unconsumed quote side, exact reference and available top quantity; market orders only".into()));
        }
        Ok(())
    }

    fn mission_block(&self, mission: &TradingMission, now_ms: i64) -> Option<String> {
        if let Some(reason) = &self.protection_progress.stopped {
            return Some(reason.clone());
        }
        if let Some(reason) = self.mission_progress.stopped {
            return Some(reason.code().into());
        }
        if now_ms < mission.starts_at_ms {
            return Some("mission_not_started".into());
        }
        if now_ms >= mission.expires_at_ms {
            return Some("mission_expired".into());
        }
        if self.book.orders.iter().any(|(id, order)| {
            self.dispatched.contains(id)
                && (order.status == ExecutionStatusV2::PendingSubmit
                    || order.status.is_ambiguous()
                    || matches!(
                        order.status,
                        ExecutionStatusV2::PendingCancel | ExecutionStatusV2::PendingAmend
                    ))
        }) {
            return Some("reconciliation_required".into());
        }
        None
    }

    fn authorize_mission_buy(
        &self,
        intent: &Intent,
        config: &Config,
        now_ms: i64,
        excluding: Option<&str>,
    ) -> Result<()> {
        let Some(mission) = &config.mission else {
            return Ok(());
        };
        if intent.request.side != Side::Buy {
            // Inventory-reducing sales retain the ordinary inventory/expiry/rule
            // checks, and remain possible after the mission stops new exposure.
            return Ok(());
        }
        if let Some(reason) = self.mission_block(mission, now_ms) {
            return Err(Error(format!("mission blocks new buys: {reason}")));
        }
        self.authorize_protection_buy(config, intent, now_ms, excluding)?;
        let mut total = self
            .mission_progress
            .buy_spend
            .checked_add(self.mission_reservations(config, excluding)?)?;
        for amount in requirements(intent, config)?.values() {
            total = total.checked_add(*amount)?;
        }
        if total > SignedAmount::from(mission.max_buy_spend) {
            return Err(Error("mission cumulative buy budget exceeded".into()));
        }
        Ok(())
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
    pub fn market_enabled(&self) -> bool {
        self.config.market_data.is_some()
    }

    /// Called by trusted collectors; no raw-response import is exposed to MCP.
    pub fn record_market_snapshot(&mut self, quote: market::MarketSnapshot) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut state = replay(&transaction, &self.config)?;
        append(
            &transaction,
            &mut state,
            &self.config,
            Record::MarketSnapshot(Box::new(quote)),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn refresh_market(&mut self) -> Result<serde_json::Value> {
        let policy = self
            .config
            .market_data
            .clone()
            .ok_or_else(|| Error("public feed is not configured".into()))?;
        let now = market::now_ms()?;
        {
            let transaction = self.connection.transaction()?;
            let state = replay(&transaction, &self.config)?;
            if state.latest_market.as_ref().is_some_and(|q| {
                q.received_at_ms
                    .checked_add(policy.min_refresh_interval_ms)
                    .is_none_or(|earliest| now < earliest)
            }) {
                return Err(Error("refresh cadence not elapsed".into()));
            }
        }
        let quote = market::collect(&policy, &policy.transport()?, market::now_ms)?;
        self.record_market_snapshot(quote)?;
        self.market_snapshot(market::now_ms()?)
    }

    pub fn market_snapshot(&mut self, now_ms: i64) -> Result<serde_json::Value> {
        let policy = self
            .config
            .market_data
            .as_ref()
            .ok_or_else(|| Error("public feed is not configured".into()))?;
        let transaction = self.connection.transaction()?;
        let state = replay(&transaction, &self.config)?;
        let quote = state
            .latest_market
            .as_ref()
            .ok_or_else(|| Error("no collected quote".into()))?;
        quote.validate_at(policy, now_ms)?;
        Ok(serde_json::json!({"mode":"paper", "snapshot":quote,
            "buy_reference":quote.reference(policy, Side::Buy)?,
            "sell_reference":quote.reference(policy, Side::Sell)?,
            "buy_side_consumed":state.market_claims.contains(&format!("{}:Buy",quote.id())),
            "sell_side_consumed":state.market_claims.contains(&format!("{}:Sell",quote.id())),
            "runtime_journal_hash":state.hash,
            "exchange_event_timestamp_available":false}))
    }
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

    /// Read authoritative mission progress. This cannot change operator policy.
    pub fn mission_report(&mut self, now_ms: i64) -> Result<Option<MissionReport>> {
        let Some(mission) = &self.config.mission else {
            return Ok(None);
        };
        let transaction = self.connection.transaction()?;
        let state = replay(&transaction, &self.config)?;
        let reserved = state.mission_reservations(&self.config, None)?;
        let remaining = SignedAmount::from(mission.max_buy_spend)
            .checked_sub(state.mission_progress.buy_spend)?
            .checked_sub(reserved)?;
        let blocked = state
            .mission_block(mission, now_ms)
            .or_else(|| (remaining <= SignedAmount::ZERO).then(|| "buy_budget_exhausted".into()));
        let rules = self
            .config
            .instruments
            .get(&mission.instrument_id)
            .ok_or_else(|| Error("missing mission instrument".into()))?;
        Ok(Some(MissionReport {
            runtime_journal_hash: state.hash,
            runtime_journal_sequence: state.sequence,
            policy: mission.clone(),
            quote_asset: rules.quote_asset.clone(),
            progress: state.mission_progress,
            reserved_buy_spend: reserved,
            remaining_buy_spend: remaining,
            new_buys_blocked_by: blocked,
            valuation_basis: "quote cash flows at fully closed spot cycles; no unrealized valuation or operating costs",
            profitability_guaranteed: false,
        }))
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
        state.authorize_market(&intent, &self.config, now_ms)?;
        state.authorize_mission_buy(&intent, &self.config, now_ms, None)?;
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

    /// Release reservations only when the journal proves no dispatch was claimed.
    /// This is a local terminal decision, never a claim of external cancellation.
    pub fn abandon(&mut self, id: &str, reason: &str, now_ms: i64) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut state = replay(&transaction, &self.config)?;
        append(
            &transaction,
            &mut state,
            &self.config,
            Record::Abandon {
                client_order_id: id.to_owned(),
                reason: reason.to_owned(),
                recorded_at_ms: now_ms,
            },
        )?;
        transaction.commit()?;
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
        state.authorize_market(&intent, &self.config, now_ms)?;
        state.authorize_mission_buy(&intent, &self.config, now_ms, Some(id))?;
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
        self.record_observations(id, observations)?;
        let snapshot = self.snapshot()?;
        if snapshot
            .recovery_required
            .iter()
            .any(|pending| pending == id)
        {
            return Err(Error(
                "receipts recorded, but reconciliation remains unresolved".into(),
            ));
        }
        Ok(())
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
