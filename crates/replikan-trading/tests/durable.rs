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

fn mission_config() -> Result<Config> {
    let mut c = config()?;
    c.mission = Some(replikan_trading::mission::TradingMission {
        schema_version: 1,
        mission_id: "paper-mission-test".into(),
        objective: "Synthetic acceptance only: aim for 8 QUOTE net".into(),
        instrument_id: "TEST-QUOTE".into(),
        starts_at_ms: 1000,
        expires_at_ms: 2000,
        target_net_profit: NonNegativeMoney::parse("8")?,
        max_net_realized_loss: NonNegativeMoney::parse("5")?,
        max_buy_spend: NonNegativeMoney::parse("303")?,
    });
    Ok(c)
}

fn mission_progress(runtime: &mut Runtime) -> TestResultProgress {
    runtime
        .mission_report(1001)?
        .map(|r| r.progress)
        .ok_or_else(|| "missing mission".into())
}
type TestResultProgress =
    std::result::Result<replikan_trading::mission::MissionProgress, Box<dyn std::error::Error>>;

#[test]
fn mission_net_result_replays_and_stops_new_buys_at_exact_target() -> TestResult {
    let directory = TempDir::new()?;
    let c = mission_config()?;
    let path = directory.path().join("runtime.sqlite");
    let mut runtime = Runtime::open(&path, c.clone())?;
    let mut venue = PaperVenue::open(directory.path().join("venue.sqlite"), c.clone())?;
    for (id, side, price) in [("buy", Side::Buy, "100"), ("sell", Side::Sell, "110")] {
        runtime.prepare(intent(id, side, price)?, 1000)?;
        runtime.dispatch(id, &mut venue, 1000)?;
    }
    // Independent cash identity: -100 -1 +110 -1 = 8, not gross spread 10.
    let before = mission_progress(&mut runtime)?;
    assert_eq!(before.completed_cycle_pnl, SignedAmount::parse("8")?);
    assert_eq!(before.quote_fees, SignedAmount::parse("2")?);
    assert_eq!(before.buy_spend, SignedAmount::parse("101")?);
    assert_eq!(before.completed_cycles, 1);
    assert_eq!(
        before.stopped,
        Some(replikan_trading::mission::MissionStop::TargetReached)
    );
    runtime.reconcile("sell", &mut venue, 1001)?;
    assert_eq!(mission_progress(&mut runtime)?, before);
    drop(runtime);
    let mut runtime = Runtime::open(&path, c)?;
    assert_eq!(mission_progress(&mut runtime)?, before);
    let sequence = runtime.snapshot()?.journal_sequence;
    assert!(
        runtime
            .prepare(intent("extra", Side::Buy, "100")?, 1001)
            .is_err()
    );
    assert_eq!(runtime.snapshot()?.journal_sequence, sequence);
    Ok(())
}

#[test]
fn mission_partial_sales_are_not_reported_as_closed_cycle_profit() -> TestResult {
    let directory = TempDir::new()?;
    let c = mission_config()?;
    let mut runtime = Runtime::open(directory.path().join("runtime.sqlite"), c.clone())?;
    let mut venue = PaperVenue::open(directory.path().join("venue.sqlite"), c)?;
    let mut buy = intent("buy-two", Side::Buy, "100")?;
    buy.request.quantity = Quantity::parse("2")?;
    runtime.prepare(buy, 1000)?;
    runtime.dispatch("buy-two", &mut venue, 1000)?;
    runtime.prepare(intent("sell-one", Side::Sell, "110")?, 1000)?;
    runtime.dispatch("sell-one", &mut venue, 1000)?;
    let partial = mission_progress(&mut runtime)?;
    assert_eq!(partial.base_inventory, SignedAmount::parse("1")?);
    assert_eq!(partial.quote_cash_flow, SignedAmount::parse("-92")?);
    assert_eq!(partial.completed_cycle_pnl, SignedAmount::ZERO);
    assert_eq!(partial.completed_cycles, 0);
    runtime.prepare(intent("sell-two", Side::Sell, "120")?, 1001)?;
    runtime.dispatch("sell-two", &mut venue, 1001)?;
    // -201 +109 +119 = 27, inclusive of three fills' fees.
    assert_eq!(
        mission_progress(&mut runtime)?.completed_cycle_pnl,
        SignedAmount::parse("27")?
    );
    Ok(())
}

#[test]
fn mission_rechecks_prepared_buy_after_another_order_reaches_goal() -> TestResult {
    let directory = TempDir::new()?;
    let c = mission_config()?;
    let path = directory.path().join("runtime.sqlite");
    let mut first = Runtime::open(&path, c.clone())?;
    let mut second = Runtime::open(&path, c.clone())?;
    let mut venue = PaperVenue::open(directory.path().join("venue.sqlite"), c)?;
    first.prepare(intent("later", Side::Buy, "100")?, 1000)?;
    first.prepare(intent("buy", Side::Buy, "100")?, 1000)?;
    first.dispatch("buy", &mut venue, 1000)?;
    first.prepare(intent("sell", Side::Sell, "110")?, 1000)?;
    first.dispatch("sell", &mut venue, 1000)?;
    assert!(second.dispatch("later", &mut venue, 1001).is_err());
    assert!(venue.query("later", 1001)?.is_none());
    second.abandon("later", "mission finished before dispatch", 1001)?;
    assert_eq!(
        second
            .mission_report(1001)?
            .ok_or("missing report")?
            .reserved_buy_spend,
        SignedAmount::ZERO
    );
    Ok(())
}

#[test]
fn mission_budget_includes_fees_reservations_and_is_not_refilled_by_sales() -> TestResult {
    let directory = TempDir::new()?;
    let mut c = mission_config()?;
    c.mission.as_mut().ok_or("missing policy")?.max_buy_spend = NonNegativeMoney::parse("101")?;
    let path = directory.path().join("runtime.sqlite");
    let mut first = Runtime::open(&path, c.clone())?;
    let mut second = Runtime::open(&path, c.clone())?;
    let mut venue = PaperVenue::open(directory.path().join("venue.sqlite"), c)?;
    first.prepare(intent("buy", Side::Buy, "100")?, 1000)?;
    assert!(
        second
            .prepare(intent("competing", Side::Buy, "100")?, 1000)
            .is_err()
    );
    assert_eq!(
        second
            .mission_report(1000)?
            .ok_or("missing report")?
            .remaining_buy_spend,
        SignedAmount::ZERO
    );
    // The already-reserved order is not counted twice at dispatch.
    first.dispatch("buy", &mut venue, 1000)?;
    first.prepare(intent("sell", Side::Sell, "103")?, 1000)?;
    first.dispatch("sell", &mut venue, 1000)?;
    assert_eq!(
        mission_progress(&mut first)?.completed_cycle_pnl,
        SignedAmount::parse("1")?
    );
    assert!(
        second
            .prepare(intent("recycle", Side::Buy, "100")?, 1001)
            .is_err()
    );
    Ok(())
}

#[test]
fn mission_loss_stop_and_deadline_do_not_prevent_inventory_exit() -> TestResult {
    let directory = TempDir::new()?;
    let mut c = mission_config()?;
    c.mission.as_mut().ok_or("missing policy")?.expires_at_ms = 1001;
    let mut runtime = Runtime::open(directory.path().join("runtime.sqlite"), c.clone())?;
    let mut venue = PaperVenue::open(directory.path().join("venue.sqlite"), c)?;
    assert!(
        runtime
            .prepare(intent("too-early", Side::Buy, "100")?, 999)
            .is_err()
    );
    runtime.prepare(intent("buy", Side::Buy, "100")?, 1000)?;
    runtime.dispatch("buy", &mut venue, 1000)?;
    assert!(
        runtime
            .prepare(intent("late", Side::Buy, "100")?, 1001)
            .is_err()
    );
    runtime.prepare(intent("exit", Side::Sell, "97")?, 1001)?;
    runtime.dispatch("exit", &mut venue, 1001)?;
    let report = mission_progress(&mut runtime)?;
    assert_eq!(report.completed_cycle_pnl, SignedAmount::parse("-5")?);
    assert_eq!(
        report.stopped,
        Some(replikan_trading::mission::MissionStop::RealizedLossLimit)
    );
    Ok(())
}

#[test]
fn mission_unknown_submission_blocks_new_exposure_until_reconciled() -> TestResult {
    let directory = TempDir::new()?;
    let c = mission_config()?;
    let mut runtime = Runtime::open(directory.path().join("runtime.sqlite"), c.clone())?;
    let mut venue = LoseReply(PaperVenue::open(directory.path().join("venue.sqlite"), c)?);
    runtime.prepare(intent("lost", Side::Buy, "100")?, 1000)?;
    assert!(runtime.dispatch("lost", &mut venue, 1000).is_err());
    assert!(
        runtime
            .prepare(intent("extra", Side::Buy, "100")?, 1001)
            .is_err()
    );
    assert_eq!(
        runtime
            .mission_report(1001)?
            .ok_or("missing report")?
            .new_buys_blocked_by
            .as_deref(),
        Some("reconciliation_required")
    );
    runtime.reconcile("lost", &mut venue, 1001)?;
    runtime.prepare(intent("extra", Side::Buy, "100")?, 1001)?;
    assert_eq!(mission_progress(&mut runtime)?.fills, 1);
    Ok(())
}

#[test]
fn mission_rejects_unknown_cost_basis_and_implicit_policy_migration() -> TestResult {
    let directory = TempDir::new()?;
    let mut c = mission_config()?;
    c.initial_balances
        .insert("TEST".into(), SignedAmount::parse("1")?);
    assert!(Runtime::open(directory.path().join("invalid.sqlite"), c).is_err());
    let c = mission_config()?;
    let path = directory.path().join("valid.sqlite");
    drop(Runtime::open(&path, c.clone())?);
    let mut changed = c;
    changed
        .mission
        .as_mut()
        .ok_or("missing policy")?
        .target_net_profit = NonNegativeMoney::parse("9")?;
    assert!(Runtime::open(&path, changed).is_err());
    Ok(())
}

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
        mission: None,
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
fn abandoned_expired_intent_releases_reserves_durably_without_reusing_identity() -> TestResult {
    let directory = TempDir::new()?;
    let (mut runtime, mut venue) = open(directory.path())?;
    let mut order = intent("expired", Side::Buy, "100")?;
    order.request.quantity = Quantity::parse("9")?;
    runtime.prepare(order, 1000)?;
    assert!(
        runtime
            .prepare(intent("next", Side::Buy, "100")?, 1000)
            .is_err()
    );
    assert!(runtime.abandon("expired", "", 1001).is_err());
    assert!(runtime.abandon("expired", "stale", 999).is_err());
    runtime.abandon("expired", "expired before dispatch", 2001)?;
    assert!(
        runtime
            .abandon("expired", "expired before dispatch", 2001)
            .is_err()
    );
    assert!(runtime.dispatch("expired", &mut venue, 1000).is_err());
    drop(runtime);
    let (mut runtime, _) = open(directory.path())?;
    let snapshot = runtime.snapshot()?;
    assert_eq!(snapshot.orders[0].status, ExecutionStatusV2::Rejected);
    assert!(snapshot.fills.is_empty());
    runtime.prepare(intent("next", Side::Buy, "100")?, 1000)?;
    runtime.dispatch("next", &mut venue, 1000)?;
    assert!(
        runtime
            .abandon("next", "cannot undo sent order", 1001)
            .is_err()
    );
    Ok(())
}

#[test]
fn ambiguous_submission_cannot_be_abandoned() -> TestResult {
    let directory = TempDir::new()?;
    let (mut runtime, venue) = open(directory.path())?;
    let mut venue = LoseReply(venue);
    runtime.prepare(intent("unknown", Side::Buy, "100")?, 1000)?;
    assert!(runtime.dispatch("unknown", &mut venue, 1000).is_err());
    assert!(runtime.abandon("unknown", "response lost", 1001).is_err());
    runtime.reconcile("unknown", &mut venue, 1001)?;
    assert_eq!(runtime.snapshot()?.fills.len(), 1);
    Ok(())
}

#[test]
fn reconciliation_does_not_report_success_for_unresolved_order() -> TestResult {
    let directory = TempDir::new()?;
    let (mut runtime, mut venue) = open(directory.path())?;
    runtime.prepare(intent("buy", Side::Buy, "100")?, 1000)?;
    runtime.dispatch("buy", &mut venue, 1000)?;
    runtime.record_observations(
        "buy",
        vec![Observation {
            native_sequence: None,
            received_at_ms: 1001,
            kind: ExecutionEventKindV2::SubmitRejected {
                reason: "contradictory fixture".into(),
            },
        }],
    )?;
    assert!(runtime.reconcile("buy", &mut venue, 1002).is_err());
    let snapshot = runtime.snapshot()?;
    assert_eq!(
        snapshot.orders[0].status,
        ExecutionStatusV2::ReconciliationRequired
    );
    assert_eq!(snapshot.fills.len(), 1);
    assert_eq!(snapshot.balances["QUOTE"].as_decimal_string(), "899");
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
