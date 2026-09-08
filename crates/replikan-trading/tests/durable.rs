use replikan_trading::{
    Config, Error, Intent, Observation, Result, Runtime, VenueAdapter,
    execution_v2::{ExecutionEventKindV2, ExecutionStatusV2, FillKey},
    financial::{
        ExactInstrumentRules, ExactOrderRequest, ExactOrderType, NonNegativeMoney, Price, Quantity,
        ReferencePrice, SignedAmount,
    },
    orders::{Side, TimeInForce},
    paper::PaperVenue,
};
use std::collections::BTreeMap;
use std::path::Path;
use tempfile::TempDir;

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

fn config() -> Result<Config> {
    let rules = ExactInstrumentRules {
        venue: "paper".into(),
        instrument_id: "TEST-QUOTE".into(),
        base_asset: "TEST".into(),
        quote_asset: "QUOTE".into(),
        rules_version: "test-v1".into(),
        price_tick: Price::parse("0.01")?,
        quantity_step: Quantity::parse("0.01")?,
        min_quantity: Quantity::parse("0.01")?,
        max_quantity: None,
        min_notional: NonNegativeMoney::parse("1")?,
        max_notional: None,
    };
    Ok(Config {
        schema_version: 1,
        venue: "paper".into(),
        account_id: "test-account".into(),
        instruments: BTreeMap::from([("TEST-QUOTE".into(), rules)]),
        initial_balances: BTreeMap::from([("QUOTE".into(), SignedAmount::parse("1000")?)]),
        max_quantity: Quantity::parse("10")?,
        max_order_notional: NonNegativeMoney::parse("1000")?,
        max_open_orders: 5,
        max_intent_age_ms: 1000,
        paper_quote_fee: NonNegativeMoney::parse("1")?,
    })
}

fn intent(id: &str, side: Side, price: &str) -> Result<Intent> {
    Ok(Intent {
        intent_id: format!("intent-{id}"),
        idempotency_key: format!("key-{id}"),
        agent_id: "test-agent".into(),
        client_order_id: id.into(),
        decision_id: format!("decision-{id}"),
        strategy_version: "fixture-v1".into(),
        evidence_refs: vec!["fixture:reference".into()],
        rationale: "deterministic test decision".into(),
        created_at_ms: 1000,
        expires_at_ms: 2000,
        request: ExactOrderRequest {
            instrument_id: "TEST-QUOTE".into(),
            side,
            order_type: ExactOrderType::Market,
            quantity: Quantity::parse("1")?,
            tif: TimeInForce::Gtc,
            reduce_only: false,
            post_only: false,
            rules_version: "test-v1".into(),
        },
        reference: ReferencePrice {
            venue: "paper".into(),
            instrument_id: "TEST-QUOTE".into(),
            price: Price::parse(price)?,
            observed_at_ms: 1000,
            valid_until_ms: 2000,
        },
    })
}

fn open(directory: &Path) -> Result<(Runtime, PaperVenue)> {
    Ok((
        Runtime::open(directory.join("runtime.sqlite"), config()?)?,
        PaperVenue::open(directory.join("venue.sqlite"), config()?)?,
    ))
}

#[test]
fn buy_sell_roundtrip_reopens_with_exact_balances_and_export() -> TestResult {
    let directory = TempDir::new()?;
    let (mut runtime, mut venue) = open(directory.path())?;
    runtime.prepare(intent("buy", Side::Buy, "100")?, 1000)?;
    runtime.dispatch("buy", &mut venue, 1000)?;
    runtime.prepare(intent("sell", Side::Sell, "110")?, 1001)?;
    runtime.dispatch("sell", &mut venue, 1001)?;
    let before = runtime.snapshot()?;
    assert_eq!(before.balances["QUOTE"].as_decimal_string(), "1008");
    assert_eq!(before.balances["TEST"], SignedAmount::ZERO);
    assert_eq!(before.fills.len(), 2);
    let exported = runtime.export()?;
    drop(runtime);
    let (mut reopened, _) = open(directory.path())?;
    assert_eq!(reopened.snapshot()?.journal_hash, before.journal_hash);
    assert_eq!(reopened.export()?, exported);
    Ok(())
}

#[test]
fn duplicate_intent_is_idempotent_but_duplicate_dispatch_cannot_send() -> TestResult {
    let directory = TempDir::new()?;
    let (mut runtime, mut venue) = open(directory.path())?;
    let order = intent("buy", Side::Buy, "100")?;
    runtime.prepare(order.clone(), 1000)?;
    runtime.prepare(order.clone(), 1000)?;
    assert_eq!(runtime.snapshot()?.journal_sequence, 1);
    runtime.dispatch("buy", &mut venue, 1000)?;
    assert!(runtime.dispatch("buy", &mut venue, 1000).is_err());
    runtime.reconcile("buy", &mut venue, 1001)?;
    assert_eq!(runtime.snapshot()?.fills.len(), 1);
    assert_eq!(
        runtime.snapshot()?.balances["QUOTE"].as_decimal_string(),
        "899"
    );
    let mut changed = order;
    changed.rationale = "changed payload".into();
    assert!(runtime.prepare(changed, 1000).is_err());
    Ok(())
}

struct LoseReply(PaperVenue);
impl VenueAdapter for LoseReply {
    fn identity(&self) -> (&str, &str) {
        self.0.identity()
    }
    fn submit(&mut self, intent: &Intent, config: &Config, now: i64) -> Result<Vec<Observation>> {
        self.0.submit(intent, config, now)?;
        Err(Error("injected lost response".into()))
    }
    fn query(&mut self, id: &str, now: i64) -> Result<Option<Vec<Observation>>> {
        self.0.query(id, now)
    }
    fn cancel(&mut self, id: &str, now: i64) -> Result<Vec<Observation>> {
        self.0.cancel(id, now)
    }
}

#[test]
fn lost_reply_is_unknown_then_reconciled_without_resubmit() -> TestResult {
    let directory = TempDir::new()?;
    let (mut runtime, venue) = open(directory.path())?;
    let mut venue = LoseReply(venue);
    runtime.prepare(intent("buy", Side::Buy, "100")?, 1000)?;
    assert!(runtime.dispatch("buy", &mut venue, 1000).is_err());
    assert_eq!(
        runtime.snapshot()?.orders[0].status,
        ExecutionStatusV2::SubmitUnknown
    );
    drop(runtime);
    let (mut reopened, mut venue) = open(directory.path())?;
    assert!(reopened.dispatch("buy", &mut venue, 1001).is_err());
    reopened.reconcile("buy", &mut venue, 1001)?;
    assert_eq!(
        reopened.snapshot()?.orders[0].status,
        ExecutionStatusV2::Filled
    );
    assert_eq!(
        reopened.snapshot()?.balances["QUOTE"].as_decimal_string(),
        "899"
    );
    Ok(())
}

struct ExitAdapter {
    venue: PaperVenue,
    before_send: bool,
}
impl VenueAdapter for ExitAdapter {
    fn identity(&self) -> (&str, &str) {
        self.venue.identity()
    }
    fn submit(&mut self, intent: &Intent, config: &Config, now: i64) -> Result<Vec<Observation>> {
        if !self.before_send {
            self.venue.submit(intent, config, now)?;
        }
        std::process::exit(86)
    }
    fn query(&mut self, id: &str, now: i64) -> Result<Option<Vec<Observation>>> {
        self.venue.query(id, now)
    }
    fn cancel(&mut self, id: &str, now: i64) -> Result<Vec<Observation>> {
        self.venue.cancel(id, now)
    }
}

#[test]
fn crash_child() -> TestResult {
    let Ok(path) = std::env::var("REPLIKAN_TEST_CRASH_DIRECTORY") else {
        return Ok(());
    };
    let (mut runtime, venue) = open(Path::new(&path))?;
    runtime.prepare(intent("crash", Side::Buy, "100")?, 1000)?;
    runtime.dispatch(
        "crash",
        &mut ExitAdapter {
            venue,
            before_send: std::env::var_os("REPLIKAN_TEST_BEFORE_SEND").is_some(),
        },
        1000,
    )?;
    Err("child did not exit at injected crash".into())
}

#[test]
fn real_process_death_after_claim_and_after_external_commit_is_recoverable() -> TestResult {
    for before_send in [false, true] {
        let directory = TempDir::new()?;
        let mut child = std::process::Command::new(std::env::current_exe()?);
        child
            .args(["--exact", "crash_child", "--nocapture"])
            .env("REPLIKAN_TEST_CRASH_DIRECTORY", directory.path());
        if before_send {
            child.env("REPLIKAN_TEST_BEFORE_SEND", "1");
        }
        assert_eq!(child.status()?.code(), Some(86));
        let (mut runtime, mut venue) = open(directory.path())?;
        assert_eq!(runtime.snapshot()?.recovery_required, vec!["crash"]);
        assert!(runtime.dispatch("crash", &mut venue, 1001).is_err());
        let result = runtime.reconcile("crash", &mut venue, 1001);
        if before_send {
            assert!(result.is_err());
            assert_eq!(runtime.snapshot()?.fills.len(), 0);
        } else {
            result?;
            assert_eq!(runtime.snapshot()?.fills.len(), 1);
            assert_eq!(
                runtime.snapshot()?.balances["QUOTE"].as_decimal_string(),
                "899"
            );
        }
    }
    Ok(())
}

#[test]
fn database_failure_rolls_back_receipt_batch_then_query_recovers() -> TestResult {
    let directory = TempDir::new()?;
    let (mut runtime, mut venue) = open(directory.path())?;
    runtime.prepare(intent("buy", Side::Buy, "100")?, 1000)?;
    let connection = rusqlite::Connection::open(directory.path().join("runtime.sqlite"))?;
    connection.execute_batch("CREATE TRIGGER reject_fill BEFORE INSERT ON trading_journal
        WHEN NEW.payload LIKE '%\"Fill\"%' BEGIN SELECT RAISE(ABORT, 'injected storage failure'); END;")?;
    assert!(runtime.dispatch("buy", &mut venue, 1000).is_err());
    assert_eq!(
        runtime.snapshot()?.orders[0].status,
        ExecutionStatusV2::PendingSubmit
    );
    assert_eq!(
        runtime.snapshot()?.balances["QUOTE"].as_decimal_string(),
        "1000"
    );
    connection.execute_batch("DROP TRIGGER reject_fill;")?;
    runtime.reconcile("buy", &mut venue, 1001)?;
    assert_eq!(
        runtime.snapshot()?.balances["QUOTE"].as_decimal_string(),
        "899"
    );
    Ok(())
}

#[test]
fn two_process_connections_cannot_claim_the_same_order_twice() -> TestResult {
    let directory = TempDir::new()?;
    let (mut first, mut venue) = open(directory.path())?;
    let (mut second, mut second_venue) = open(directory.path())?;
    first.prepare(intent("buy", Side::Buy, "100")?, 1000)?;
    first.dispatch("buy", &mut venue, 1000)?;
    assert!(second.dispatch("buy", &mut second_venue, 1000).is_err());
    assert_eq!(second.snapshot()?.fills.len(), 1);
    Ok(())
}

#[test]
fn reservations_expiry_and_sell_inventory_are_enforced() -> TestResult {
    let directory = TempDir::new()?;
    let (mut runtime, mut venue) = open(directory.path())?;
    assert!(
        runtime
            .prepare(intent("sell", Side::Sell, "100")?, 1000)
            .is_err()
    );
    let mut first = intent("first", Side::Buy, "100")?;
    first.request.quantity = Quantity::parse("9")?;
    runtime.prepare(first, 1000)?;
    assert!(
        runtime
            .prepare(intent("second", Side::Buy, "100")?, 1000)
            .is_err()
    );
    assert!(runtime.dispatch("first", &mut venue, 2001).is_err());
    assert!(
        runtime
            .prepare(intent("stale", Side::Buy, "100")?, 999)
            .is_err()
    );
    Ok(())
}

#[test]
fn cancel_open_limit_is_durable_and_query_is_idempotent() -> TestResult {
    let directory = TempDir::new()?;
    let (mut runtime, mut venue) = open(directory.path())?;
    let mut order = intent("limit", Side::Buy, "100")?;
    order.request.order_type = ExactOrderType::Limit {
        price: Price::parse("90")?,
    };
    runtime.prepare(order, 1000)?;
    runtime.dispatch("limit", &mut venue, 1000)?;
    assert_eq!(
        runtime.snapshot()?.orders[0].status,
        ExecutionStatusV2::Open
    );
    runtime.cancel("limit", &mut venue, 1001)?;
    runtime.reconcile("limit", &mut venue, 1002)?;
    assert_eq!(
        runtime.snapshot()?.orders[0].status,
        ExecutionStatusV2::Canceled
    );
    assert_eq!(
        runtime.snapshot()?.balances["QUOTE"].as_decimal_string(),
        "1000"
    );
    Ok(())
}

#[test]
fn partial_fills_deduplicate_across_transport_sequences_and_charge_third_asset_fee() -> TestResult {
    let directory = TempDir::new()?;
    let (mut runtime, mut venue) = open(directory.path())?;
    let mut order = intent("limit", Side::Buy, "100")?;
    order.request.order_type = ExactOrderType::Limit {
        price: Price::parse("90")?,
    };
    runtime.prepare(order, 1000)?;
    runtime.dispatch("limit", &mut venue, 1000)?;
    let mut receipt = Observation {
        native_sequence: Some(1),
        received_at_ms: 1001,
        kind: ExecutionEventKindV2::Fill {
            key: FillKey {
                venue: "paper".into(),
                account_id: "test-account".into(),
                instrument_id: "TEST-QUOTE".into(),
                trade_id: "fixture-fill".into(),
            },
            occurred_at_ms: 1001,
            price: Price::parse("90")?,
            quantity: Quantity::parse("0.2")?,
            fee_asset: "FEE".into(),
            fee_amount: SignedAmount::parse("0.01")?,
        },
    };
    runtime.record_observations("limit", vec![receipt.clone()])?;
    receipt.native_sequence = Some(900);
    runtime.record_observations("limit", vec![receipt])?;
    let snapshot = runtime.snapshot()?;
    assert_eq!(snapshot.fills.len(), 1);
    assert_eq!(snapshot.balances["QUOTE"].as_decimal_string(), "982");
    assert_eq!(snapshot.balances["TEST"].as_decimal_string(), "0.2");
    // Real receipts remain authoritative even when they expose a balance deficit.
    assert_eq!(snapshot.balances["FEE"].as_decimal_string(), "-0.01");
    Ok(())
}

#[test]
fn journal_tampering_and_configuration_drift_fail_closed() -> TestResult {
    let directory = TempDir::new()?;
    let (mut runtime, _) = open(directory.path())?;
    runtime.prepare(intent("buy", Side::Buy, "100")?, 1000)?;
    drop(runtime);
    let mut changed = config()?;
    changed.account_id = "different-account".into();
    assert!(Runtime::open(directory.path().join("runtime.sqlite"), changed).is_err());
    let connection = rusqlite::Connection::open(directory.path().join("runtime.sqlite"))?;
    connection.execute(
        "UPDATE trading_journal SET hash='altered' WHERE sequence=1",
        [],
    )?;
    assert!(open(directory.path()).is_err());
    Ok(())
}
