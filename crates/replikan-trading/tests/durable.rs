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

fn market_config() -> Result<Config> {
    let mut c = config()?;
    c.market_data = Some(replikan_trading::market::MarketDataPolicy {
        instrument_id: "TEST-QUOTE".into(),
        symbol: "TESTQUOTE".into(),
        max_age_ms: 5000,
        min_refresh_interval_ms: 1000,
    });
    Ok(c)
}

struct QuoteTransport {
    body: String,
    status: u16,
}
impl replikan_market_http::HttpTransport for QuoteTransport {
    fn get(
        &self,
        endpoint: &str,
    ) -> std::result::Result<replikan_market_http::HttpResponse, replikan_market_http::TransportError>
    {
        assert_eq!(
            endpoint,
            "https://data-api.binance.vision/api/v3/ticker/bookTicker?symbol=TESTQUOTE&symbolStatus=TRADING"
        );
        Ok(replikan_market_http::HttpResponse {
            status: self.status,
            body: self.body.clone(),
        })
    }
}

fn collected_quote(
    c: &Config,
    start: i64,
    bid: &str,
    ask: &str,
) -> Result<replikan_trading::market::MarketSnapshot> {
    let policy = c
        .market_data
        .as_ref()
        .ok_or_else(|| Error("missing test policy".into()))?;
    let body = serde_json::json!({"symbol":"TESTQUOTE", "bidPrice":bid, "askPrice":ask, "bidQty":"2", "askQty":"2"}).to_string();
    let mut time = start;
    replikan_trading::market::collect(policy, &QuoteTransport { body, status: 200 }, || {
        let now = time;
        time += 1;
        Ok(now)
    })
}

fn quoted_intent(
    c: &Config,
    quote: &replikan_trading::market::MarketSnapshot,
    id: &str,
    side: Side,
) -> Result<Intent> {
    let mut i = intent(id, side, "100")?;
    i.market_snapshot_id = Some(quote.id().into());
    i.reference = quote.reference(
        c.market_data
            .as_ref()
            .ok_or_else(|| Error("missing policy".into()))?,
        side,
    )?;
    i.created_at_ms = quote.received_at_ms;
    i.expires_at_ms = quote.received_at_ms + 1000;
    Ok(i)
}

#[test]
fn collected_bid_ask_bind_orders_and_survive_replay_without_model_prices() -> TestResult {
    let directory = TempDir::new()?;
    let c = market_config()?;
    let quote = collected_quote(&c, 1000, "99", "101")?;
    let path = directory.path().join("runtime.sqlite");
    let mut runtime = Runtime::open(&path, c.clone())?;
    let mut venue = PaperVenue::open(directory.path().join("venue.sqlite"), c.clone())?;
    assert!(
        runtime
            .prepare(intent("invented", Side::Buy, "1")?, 1001)
            .is_err()
    );
    runtime.record_market_snapshot(quote.clone())?;
    let buy = quoted_intent(&c, &quote, "buy", Side::Buy)?;
    let mut forged = buy.clone();
    forged.reference.price = Price::parse("1")?;
    assert!(runtime.prepare(forged, 1001).is_err());
    runtime.prepare(buy, 1001)?;
    runtime.dispatch("buy", &mut venue, 1001)?;
    runtime.prepare(quoted_intent(&c, &quote, "sell", Side::Sell)?, 1001)?;
    runtime.dispatch("sell", &mut venue, 1001)?;
    // Buying at ask and selling at bid incurs the spread and two quote fees.
    assert_eq!(
        runtime.snapshot()?.balances["QUOTE"],
        SignedAmount::parse("996")?
    );
    let market = runtime.market_snapshot(1001)?;
    assert_eq!(market["buy_side_consumed"], true);
    assert_eq!(market["sell_side_consumed"], true);
    drop(runtime);
    let mut runtime = Runtime::open(path, c.clone())?;
    assert_eq!(runtime.market_snapshot(1001)?, market);
    assert!(
        runtime
            .prepare(quoted_intent(&c, &quote, "reuse", Side::Buy)?, 1001)
            .is_err()
    );
    Ok(())
}

#[test]
fn quote_refresh_invalidates_old_prepared_intent_and_depth_is_bounded() -> TestResult {
    let directory = TempDir::new()?;
    let c = market_config()?;
    let mut runtime = Runtime::open(directory.path().join("runtime.sqlite"), c.clone())?;
    let mut venue = PaperVenue::open(directory.path().join("venue.sqlite"), c.clone())?;
    let quote = collected_quote(&c, 1000, "99", "101")?;
    runtime.record_market_snapshot(quote.clone())?;
    let mut too_large = quoted_intent(&c, &quote, "large", Side::Buy)?;
    too_large.request.quantity = Quantity::parse("3")?;
    assert!(runtime.prepare(too_large, 1001).is_err());
    runtime.prepare(quoted_intent(&c, &quote, "old", Side::Buy)?, 1001)?;
    assert!(
        runtime
            .record_market_snapshot(collected_quote(&c, 1000, "99", "101")?)
            .is_err()
    );
    runtime.record_market_snapshot(collected_quote(&c, 2001, "100", "102")?)?;
    assert!(runtime.dispatch("old", &mut venue, 2001).is_err());
    assert!(venue.query("old", 2001)?.is_none());
    assert!(runtime.market_snapshot(2001).is_err()); // receipt is 2002
    assert!(runtime.market_snapshot(7002).is_err()); // age starts before HTTP
    runtime.abandon("old", "market reference superseded", 2002)?;
    Ok(())
}

#[test]
fn quote_parser_rejects_wrong_identity_crossed_empty_nondecimal_and_http_failures() -> TestResult {
    let c = market_config()?;
    let policy = c.market_data.as_ref().ok_or("missing policy")?;
    let valid = serde_json::json!({"symbol":"TESTQUOTE","bidPrice":"99","bidQty":"2","askPrice":"101","askQty":"2"});
    for (field, bad) in [
        ("symbol", serde_json::json!("OTHER")),
        ("bidPrice", serde_json::json!("102")),
        ("askQty", serde_json::json!("0")),
        ("bidPrice", serde_json::json!(99.0)),
    ] {
        let mut body = valid.clone();
        body[field] = bad;
        assert!(
            replikan_trading::market::collect(
                policy,
                &QuoteTransport {
                    body: body.to_string(),
                    status: 200
                },
                || Ok(1000)
            )
            .is_err()
        );
    }
    for status in [301, 403, 429, 500] {
        assert!(
            replikan_trading::market::collect(
                policy,
                &QuoteTransport {
                    body: valid.to_string(),
                    status
                },
                || Ok(1000)
            )
            .is_err()
        );
    }
    let mut clock = 0;
    assert!(
        replikan_trading::market::collect(
            policy,
            &QuoteTransport {
                body: valid.to_string(),
                status: 200
            },
            || {
                clock += 5000;
                Ok(clock)
            }
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn quote_content_tamper_and_symbol_policy_injection_are_rejected() -> TestResult {
    let directory = TempDir::new()?;
    let c = market_config()?;
    let quote = collected_quote(&c, 1000, "99", "101")?;
    let mut runtime = Runtime::open(directory.path().join("runtime.sqlite"), c.clone())?;
    let mut changed = serde_json::to_value(&quote)?;
    changed["ask"] = serde_json::json!("1");
    assert!(
        runtime
            .record_market_snapshot(serde_json::from_value(changed)?)
            .is_err()
    );
    assert_eq!(runtime.snapshot()?.journal_sequence, 0);
    let mut bad = c;
    bad.market_data.as_mut().ok_or("missing policy")?.symbol = "BTCUSDT&other=1".into();
    assert!(Runtime::open(directory.path().join("bad.sqlite"), bad).is_err());
    Ok(())
}

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
        market_data: None,
        protection: None,
    })
}

fn intent(id: &str, side: Side, price: &str) -> Result<Intent> {
    Ok(Intent {
        market_snapshot_id: None,
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

fn protected_config() -> Result<Config> {
    let mut c = market_config()?;
    c.mission = mission_config()?.mission;
    if let Some(mission) = c.mission.as_mut() {
        mission.expires_at_ms = 100_000;
    }
    c.protection = Some(replikan_trading::protection::ProtectionPolicy {
        max_net_loss: NonNegativeMoney::parse("10")?,
        max_drawdown: NonNegativeMoney::parse("6")?,
    });
    Ok(c)
}

#[test]
fn protection_latches_loss_abandons_pending_entry_and_exits_without_model() -> TestResult {
    let d = TempDir::new()?;
    let c = protected_config()?;
    let path = d.path().join("r");
    let mut r = Runtime::open(&path, c.clone())?;
    let mut v = PaperVenue::open(d.path().join("v"), c.clone())?;
    let q = collected_quote(&c, 1000, "99", "101")?;
    r.record_market_snapshot(q.clone())?;
    r.prepare(quoted_intent(&c, &q, "buy", Side::Buy)?, 1001)?;
    r.dispatch("buy", &mut v, 1001)?;
    assert_eq!(r.protection_status(1001)?["current_liquidation_pnl"], "-4");
    let q = collected_quote(&c, 2100, "101", "102")?;
    r.record_market_snapshot(q.clone())?;
    r.prepare(quoted_intent(&c, &q, "pending", Side::Buy)?, 2101)?;
    let q = collected_quote(&c, 3200, "90", "91")?;
    r.record_market_snapshot(q.clone())?;
    assert_eq!(
        r.protection_status(3201)?["progress"]["stopped"],
        "net_liquidation_loss_limit"
    );
    assert!(
        r.prepare(quoted_intent(&c, &q, "forbidden", Side::Buy)?, 3201)
            .is_err()
    );
    drop(r);
    let mut r = Runtime::open(&path, c)?;
    r.protect(&mut v, 3201)?;
    let snap = r.snapshot()?;
    assert_eq!(snap.balances["TEST"], SignedAmount::ZERO);
    assert_eq!(snap.balances["QUOTE"].as_decimal_string(), "987");
    assert_eq!(snap.fills.len(), 2);
    assert!(snap.orders.iter().all(|o| o.status.is_terminal()));
    r.protect(&mut v, 3202)?;
    assert_eq!(r.snapshot()?.fills.len(), 2);
    assert_eq!(
        r.protection_status(3202)?["progress"]["stopped"],
        "net_liquidation_loss_limit"
    );
    Ok(())
}

#[test]
fn protection_drawdown_uses_observed_high_water_and_target_captures_net_fees() -> TestResult {
    for (high, low, reason, final_cash) in [
        ("108", "102", "liquidation_drawdown_limit", "999"),
        ("112", "112", "liquidation_target_reached", "1009"),
    ] {
        let d = TempDir::new()?;
        let c = protected_config()?;
        let mut r = Runtime::open(d.path().join("r"), c.clone())?;
        let mut v = PaperVenue::open(d.path().join("v"), c.clone())?;
        let q = collected_quote(&c, 1000, "99", "101")?;
        r.record_market_snapshot(q.clone())?;
        r.prepare(quoted_intent(&c, &q, "buy", Side::Buy)?, 1001)?;
        r.dispatch("buy", &mut v, 1001)?;
        r.record_market_snapshot(collected_quote(&c, 2100, high, high)?)?;
        r.record_market_snapshot(collected_quote(&c, 3200, low, low)?)?;
        assert_eq!(r.protection_status(3201)?["progress"]["stopped"], reason);
        r.protect(&mut v, 3201)?;
        assert_eq!(
            r.snapshot()?.balances["QUOTE"].as_decimal_string(),
            final_cash
        );
    }
    Ok(())
}

#[test]
fn protection_stale_data_never_prices_an_exit_and_deadline_survives_failure() -> TestResult {
    let d = TempDir::new()?;
    let mut c = protected_config()?;
    if let Some(m) = c.mission.as_mut() {
        m.expires_at_ms = 8000;
    }
    let mut r = Runtime::open(d.path().join("r"), c.clone())?;
    let mut v = PaperVenue::open(d.path().join("v"), c.clone())?;
    let q = collected_quote(&c, 1000, "99", "101")?;
    r.record_market_snapshot(q.clone())?;
    r.prepare(quoted_intent(&c, &q, "buy", Side::Buy)?, 1001)?;
    r.dispatch("buy", &mut v, 1001)?;
    assert!(r.protection_status(8000)?["current_liquidation_pnl"].is_null());
    assert!(r.protect(&mut v, 8000).is_err());
    assert_eq!(r.snapshot()?.fills.len(), 1);
    assert_eq!(
        r.protection_status(8000)?["progress"]["stopped"],
        "mission_expired"
    );
    r.record_market_snapshot(collected_quote(&c, 8100, "100", "101")?)?;
    r.protect(&mut v, 8101)?;
    assert_eq!(r.snapshot()?.balances["TEST"], SignedAmount::ZERO);
    Ok(())
}

#[test]
fn protection_lost_exit_reply_is_reconciled_without_duplicate_sale() -> TestResult {
    let d = TempDir::new()?;
    let c = protected_config()?;
    let path = d.path().join("r");
    let mut r = Runtime::open(&path, c.clone())?;
    let mut v = PaperVenue::open(d.path().join("v"), c.clone())?;
    let q = collected_quote(&c, 1000, "99", "101")?;
    r.record_market_snapshot(q.clone())?;
    r.prepare(quoted_intent(&c, &q, "buy", Side::Buy)?, 1001)?;
    r.dispatch("buy", &mut v, 1001)?;
    r.record_market_snapshot(collected_quote(&c, 2100, "90", "91")?)?;
    let mut lost = LoseReply(v);
    assert!(r.protect(&mut lost, 2101).is_err());
    assert_eq!(r.snapshot()?.recovery_required.len(), 1);
    drop(r);
    let mut r = Runtime::open(&path, c)?;
    r.protect(&mut lost.0, 2102)?;
    assert_eq!(r.snapshot()?.fills.len(), 2);
    assert!(r.snapshot()?.recovery_required.is_empty());
    assert_eq!(r.snapshot()?.balances["TEST"], SignedAmount::ZERO);
    Ok(())
}

#[test]
fn protection_rejects_immediate_spread_loss_and_does_not_bypass_exit_depth() -> TestResult {
    let d = TempDir::new()?;
    let c = protected_config()?;
    let mut r = Runtime::open(d.path().join("r"), c.clone())?;
    let mut v = PaperVenue::open(d.path().join("v"), c.clone())?;
    let q = collected_quote(&c, 1000, "90", "101")?;
    r.record_market_snapshot(q.clone())?;
    assert!(
        r.prepare(quoted_intent(&c, &q, "bad", Side::Buy)?, 1001)
            .is_err()
    );
    let q = collected_quote(&c, 2100, "100", "101")?;
    r.record_market_snapshot(q.clone())?;
    r.prepare(quoted_intent(&c, &q, "buy", Side::Buy)?, 2101)?;
    r.dispatch("buy", &mut v, 2101)?;
    let policy = c.market_data.as_ref().ok_or("policy")?;
    let body =
        r#"{"symbol":"TESTQUOTE","bidPrice":"90","askPrice":"91","bidQty":"0.1","askQty":"2"}"#;
    let q = replikan_trading::market::collect(
        policy,
        &QuoteTransport {
            body: body.into(),
            status: 200,
        },
        || Ok(3200),
    )?;
    r.record_market_snapshot(q)?;
    assert!(r.protect(&mut v, 3200).is_err());
    assert_eq!(r.snapshot()?.fills.len(), 1);
    assert_eq!(r.snapshot()?.balances["TEST"].as_decimal_string(), "1");
    Ok(())
}

#[test]
fn protection_reserves_pending_buy_risk_and_excludes_own_dispatch_reservation() -> TestResult {
    let d = TempDir::new()?;
    let c = protected_config()?;
    let mut r = Runtime::open(d.path().join("r"), c.clone())?;
    let mut v = PaperVenue::open(d.path().join("v"), c.clone())?;
    let q = collected_quote(&c, 1000, "99", "101")?;
    r.record_market_snapshot(q.clone())?;
    r.prepare(quoted_intent(&c, &q, "one", Side::Buy)?, 1001)?;
    assert!(
        r.prepare(quoted_intent(&c, &q, "two", Side::Buy)?, 1001)
            .is_err()
    );
    r.dispatch("one", &mut v, 1001)?;
    assert_eq!(r.snapshot()?.fills.len(), 1);
    Ok(())
}

#[test]
fn protection_never_credits_hypothetical_pending_gains_to_finance_new_risk() -> TestResult {
    let d = TempDir::new()?;
    let c = protected_config()?;
    let mut r = Runtime::open(d.path().join("r"), c.clone())?;
    let old = collected_quote(&c, 1000, "99", "101")?;
    r.record_market_snapshot(old.clone())?;
    r.prepare(quoted_intent(&c, &old, "pending", Side::Buy)?, 1001)?;
    let current = collected_quote(&c, 2100, "110", "120")?;
    r.record_market_snapshot(current.clone())?;
    // Pending buy at 101 would appear profitable at 110, but is not a fill and
    // cannot subsidize this purchase's known 12-unit liquidation loss.
    assert!(
        r.prepare(quoted_intent(&c, &current, "bad", Side::Buy)?, 2101)
            .is_err()
    );
    assert!(r.snapshot()?.fills.is_empty());
    Ok(())
}
