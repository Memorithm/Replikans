#![forbid(unsafe_code)]

use core::fmt;

use replikan_authorized_bitcoin_planner::{
    AuthorizedBitcoinMiningPlan, AuthorizedPlanningError, plan_authorized_bitcoin_resources,
};
use replikan_control::{
    ControlDecision, ControlError, ControlPolicy, EvaluationGate, HoldReason, decide, preflight,
};
use replikan_core::{BasisPoints, Money};
use replikan_decision_ledger::{DecisionLedger, DecisionLedgerError, DecisionObservation};
use replikan_economics::EconomicFitness;
use replikan_ledger::{EconomicLedger, LedgerError, LedgerSnapshot};
use replikan_market_http::{HttpTransport, MarketPriceHttpClient};
use replikan_mining_market::network_consensus::NetworkConsensusPolicy;
use replikan_mining_market::price_consensus::PriceConsensusPolicy;
use replikan_mining_market::snapshot_builder::ElectricityObservation;
use replikan_mining_pipeline::PriceFeedRequest;
use replikan_network_feeds::BitcoinNetworkRequest;
use replikan_opportunities::SelectionPolicy;
use replikan_resource::{AuthorizedResourceInventory, MiningDeploymentTemplate, ResourceError};
use replikan_survival::SurvivalPolicy;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapitalBaseline {
    pub opening_liquid_capital: Money,
    pub survival_reserve: Money,
    pub drawdown: BasisPoints,
}

impl CapitalBaseline {
    pub fn new(
        opening_liquid_capital: Money,
        survival_reserve: Money,
        drawdown: BasisPoints,
    ) -> Result<Self, CycleError> {
        if opening_liquid_capital.is_negative() {
            return Err(CycleError::NegativeOpeningCapital);
        }
        if survival_reserve.is_negative() {
            return Err(CycleError::NegativeSurvivalReserve);
        }
        Ok(Self {
            opening_liquid_capital,
            survival_reserve,
            drawdown,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BitcoinCyclePolicies {
    pub price: PriceConsensusPolicy,
    pub network: NetworkConsensusPolicy,
    pub selection: SelectionPolicy,
    pub survival: SurvivalPolicy,
    pub control: ControlPolicy,
    pub price_ttl_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CycleEconomicState {
    pub ledger_snapshot: LedgerSnapshot,
    pub fitness: EconomicFitness,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CycleReport {
    pub economic: CycleEconomicState,
    pub plan: Option<AuthorizedBitcoinMiningPlan>,
    pub decision: ControlDecision,
    pub decision_sequence: u64,
    pub planning_diagnostic: Option<String>,
}

pub fn derive_economic_state(
    ledger: &EconomicLedger,
    baseline: CapitalBaseline,
) -> Result<CycleEconomicState, CycleError> {
    let ledger_snapshot = ledger.snapshot().map_err(CycleError::EconomicLedger)?;
    let liquid_delta = ledger_snapshot
        .checked_liquid_delta()
        .map_err(CycleError::EconomicLedger)?;
    let liquid_capital = baseline
        .opening_liquid_capital
        .checked_add(liquid_delta)
        .ok_or(CycleError::MonetaryOverflow)?;

    Ok(CycleEconomicState {
        ledger_snapshot,
        fitness: EconomicFitness {
            realized_revenue: ledger_snapshot.realized_revenue,
            realized_costs: ledger_snapshot.costs,
            liquid_capital,
            survival_reserve: baseline.survival_reserve,
            drawdown: baseline.drawdown,
        },
    })
}

#[allow(clippy::too_many_arguments)]
pub fn run_authorized_bitcoin_cycle<P, N>(
    economic_ledger: &EconomicLedger,
    decision_ledger: &mut DecisionLedger,
    baseline: CapitalBaseline,
    inventory: &AuthorizedResourceInventory,
    templates: &[MiningDeploymentTemplate],
    price_client: &MarketPriceHttpClient<P>,
    network_transport: &N,
    price_feeds: &[PriceFeedRequest],
    network_feeds: &[BitcoinNetworkRequest],
    electricity: &ElectricityObservation,
    policies: BitcoinCyclePolicies,
    now_unix_ms: u64,
) -> Result<CycleReport, CycleError>
where
    P: HttpTransport,
    N: HttpTransport,
{
    let economic = derive_economic_state(economic_ledger, baseline)?;
    let (state, mode) = match preflight(economic.fitness, policies.survival) {
        EvaluationGate::Freeze { state } => {
            let decision = ControlDecision::Freeze { state };
            let decision_sequence = append_decision(
                decision_ledger,
                now_unix_ms,
                economic,
                policies,
                DecisionCounters::default(),
                vec!["cycle:survival-freeze".to_owned()],
                Vec::new(),
                decision.clone(),
            )?;
            return Ok(CycleReport {
                economic,
                plan: None,
                decision,
                decision_sequence,
                planning_diagnostic: None,
            });
        }
        EvaluationGate::Evaluate { state, mode } => (state, mode),
    };

    let planning = plan_authorized_bitcoin_resources(
        inventory,
        templates,
        price_client,
        network_transport,
        price_feeds,
        policies.price,
        network_feeds,
        policies.network,
        electricity,
        economic.fitness,
        policies.selection,
        now_unix_ms,
        policies.price_ttl_ms,
    );

    match planning {
        Ok(plan) => finish_planned_cycle(decision_ledger, economic, policies, now_unix_ms, plan),
        Err(AuthorizedPlanningError::NoAuthorizedDeployments { rejected }) => {
            let rejected_count = rejected.len();
            let decision = ControlDecision::Hold {
                state,
                mode,
                reason: HoldReason::NoAcceptedOpportunity,
            };
            let diagnostic = format!(
                "no authorized deployment remained after {rejected_count} materialization rejections"
            );
            let decision_sequence = append_decision(
                decision_ledger,
                now_unix_ms,
                economic,
                policies,
                DecisionCounters {
                    materialization_rejections: rejected_count,
                    ..DecisionCounters::default()
                },
                vec!["cycle:no-authorized-deployment".to_owned()],
                vec![diagnostic.clone()],
                decision.clone(),
            )?;
            Ok(CycleReport {
                economic,
                plan: None,
                decision,
                decision_sequence,
                planning_diagnostic: Some(diagnostic),
            })
        }
        Err(AuthorizedPlanningError::Planner(error)) => {
            let diagnostic = format!("verified market planning unavailable: {error}");
            let decision = ControlDecision::Hold {
                state,
                mode,
                reason: HoldReason::NoAcceptedOpportunity,
            };
            let decision_sequence = append_decision(
                decision_ledger,
                now_unix_ms,
                economic,
                policies,
                DecisionCounters::default(),
                vec!["cycle:verified-market-unavailable".to_owned()],
                vec![diagnostic.clone()],
                decision.clone(),
            )?;
            Ok(CycleReport {
                economic,
                plan: None,
                decision,
                decision_sequence,
                planning_diagnostic: Some(diagnostic),
            })
        }
        Err(AuthorizedPlanningError::Resource(error)) => Err(CycleError::Resource(error)),
    }
}

fn finish_planned_cycle(
    decision_ledger: &mut DecisionLedger,
    economic: CycleEconomicState,
    policies: BitcoinCyclePolicies,
    now_unix_ms: u64,
    plan: AuthorizedBitcoinMiningPlan,
) -> Result<CycleReport, CycleError> {
    let decision = decide(
        economic.fitness,
        policies.survival,
        &plan.bitcoin.selection,
        policies.control,
    )
    .map_err(CycleError::Control)?;

    let counters = DecisionCounters {
        materialized_deployments: plan.materialization.profiles.len(),
        materialization_rejections: plan.materialization.rejected.len(),
        accepted_opportunities: plan.bitcoin.selection.accepted.len(),
        rejected_opportunities: plan.bitcoin.selection.rejected.len(),
        price_source_count: plan.bitcoin.market.price_consensus.source_count,
        network_source_count: plan.bitcoin.market.network_consensus.source_count,
    };
    let evidence = collect_plan_evidence(&plan, &decision);
    let diagnostics = collect_plan_diagnostics(&plan);
    let decision_sequence = append_decision(
        decision_ledger,
        now_unix_ms,
        economic,
        policies,
        counters,
        evidence,
        diagnostics,
        decision.clone(),
    )?;

    Ok(CycleReport {
        economic,
        plan: Some(plan),
        decision,
        decision_sequence,
        planning_diagnostic: None,
    })
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DecisionCounters {
    materialized_deployments: usize,
    materialization_rejections: usize,
    accepted_opportunities: usize,
    rejected_opportunities: usize,
    price_source_count: usize,
    network_source_count: usize,
}

#[allow(clippy::too_many_arguments)]
fn append_decision(
    ledger: &mut DecisionLedger,
    now_unix_ms: u64,
    economic: CycleEconomicState,
    policies: BitcoinCyclePolicies,
    counters: DecisionCounters,
    evidence: Vec<String>,
    diagnostics: Vec<String>,
    decision: ControlDecision,
) -> Result<u64, CycleError> {
    let observation = DecisionObservation::new(
        now_unix_ms,
        economic.ledger_snapshot,
        economic.fitness,
        policies.selection,
        policies.survival,
        policies.control,
        counters.materialized_deployments,
        counters.materialization_rejections,
        counters.accepted_opportunities,
        counters.rejected_opportunities,
        counters.price_source_count,
        counters.network_source_count,
        evidence,
        diagnostics,
        decision,
    )
    .map_err(CycleError::DecisionLedger)?;
    ledger
        .append(observation)
        .map_err(CycleError::DecisionLedger)
}

fn collect_plan_evidence(
    plan: &AuthorizedBitcoinMiningPlan,
    decision: &ControlDecision,
) -> Vec<String> {
    let mut evidence = plan
        .bitcoin
        .market
        .price_consensus
        .evidence
        .iter()
        .chain(plan.bitcoin.market.network_consensus.evidence.iter())
        .map(|reference| reference.as_str().to_owned())
        .collect::<Vec<_>>();

    match decision {
        ControlDecision::Run { opportunity_id, .. } => {
            if let Some(candidate) = plan
                .bitcoin
                .selection
                .accepted
                .iter()
                .find(|candidate| candidate.quote.id.as_str() == opportunity_id.as_str())
            {
                evidence.extend(
                    candidate
                        .quote
                        .evidence
                        .iter()
                        .map(|reference| reference.as_str().to_owned()),
                );
            }
        }
        ControlDecision::Hold { .. } => {
            for candidate in &plan.bitcoin.selection.accepted {
                evidence.extend(
                    candidate
                        .quote
                        .evidence
                        .iter()
                        .map(|reference| reference.as_str().to_owned()),
                );
            }
        }
        ControlDecision::Freeze { .. } => {}
    }

    if evidence.is_empty() {
        evidence.push("cycle:verified-plan".to_owned());
    }
    evidence
}

fn collect_plan_diagnostics(plan: &AuthorizedBitcoinMiningPlan) -> Vec<String> {
    let mut diagnostics = Vec::new();

    for rejection in &plan.materialization.rejected {
        diagnostics.push(format!(
            "materialization:{}:{:?}",
            rejection.id.as_str(),
            rejection.reason
        ));
    }
    for failure in &plan.bitcoin.market.price_source_failures {
        diagnostics.push(format!(
            "price-source:{}:{}",
            failure.source_id, failure.reason
        ));
    }
    for failure in &plan.bitcoin.market.network_source_failures {
        diagnostics.push(format!(
            "network-source:{}:{}",
            failure.source_id, failure.reason
        ));
    }
    for failure in &plan.bitcoin.projection_failures {
        diagnostics.push(format!(
            "projection:{}:{}",
            failure.id.as_str(),
            failure.reason
        ));
    }
    diagnostics
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CycleError {
    NegativeOpeningCapital,
    NegativeSurvivalReserve,
    MonetaryOverflow,
    EconomicLedger(LedgerError),
    DecisionLedger(DecisionLedgerError),
    Control(ControlError),
    Resource(ResourceError),
}

impl fmt::Display for CycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NegativeOpeningCapital => write!(f, "opening liquid capital cannot be negative"),
            Self::NegativeSurvivalReserve => write!(f, "survival reserve cannot be negative"),
            Self::MonetaryOverflow => write!(f, "cycle monetary arithmetic overflow"),
            Self::EconomicLedger(error) => write!(f, "economic ledger failed: {error}"),
            Self::DecisionLedger(error) => write!(f, "decision ledger failed: {error}"),
            Self::Control(error) => write!(f, "survival control failed: {error}"),
            Self::Resource(error) => write!(f, "resource configuration failed: {error}"),
        }
    }
}

impl std::error::Error for CycleError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    use replikan_economics::OpportunityPolicy;
    use replikan_ledger::EntryKind;
    use replikan_market_feeds::{CoinbaseExchangePriceAdapter, KrakenPriceAdapter};
    use replikan_market_http::{HttpResponse, TransportError};
    use replikan_mining_pipeline::PublicPriceFeed;
    use replikan_network_feeds::BitcoinNetworkFeed;
    use replikan_opportunities::OpportunityId;
    use replikan_resource::{
        AuthorizationGrant, AuthorizedResource, MiningBenchmark, ResourceId, ResourceKind,
    };
    use replikan_survival::{SpendingMode, SurvivalState};

    const NOW: u64 = 1_000_000;

    struct PriceTransport {
        calls: Cell<u32>,
    }

    impl HttpTransport for PriceTransport {
        fn get(&self, endpoint: &str) -> Result<HttpResponse, TransportError> {
            self.calls.set(self.calls.get() + 1);
            if endpoint.contains("api.exchange.coinbase.com") {
                return Ok(HttpResponse {
                    status: 200,
                    body: r#"{"price":"60000.000000"}"#.to_owned(),
                });
            }
            if endpoint.contains("api.kraken.com") {
                return Ok(HttpResponse {
                    status: 200,
                    body: r#"{"error":[],"result":{"XBTUSD":{"c":["60010.000000","1"]}}}"#
                        .to_owned(),
                });
            }
            Err(TransportError::HostForbidden(
                "unexpected price fixture endpoint".to_owned(),
            ))
        }
    }

    struct NetworkTransport {
        calls: Cell<u32>,
    }

    impl HttpTransport for NetworkTransport {
        fn get(&self, endpoint: &str) -> Result<HttpResponse, TransportError> {
            self.calls.set(self.calls.get() + 1);
            let body = if endpoint.contains("mempool.space/api/v1/mining/hashrate") {
                r#"{"currentHashrate":6.5e20,"hashrates":[]}"#.to_owned()
            } else if endpoint.contains("mempool.space/api/blocks/tip/height") {
                "840000".to_owned()
            } else if endpoint.contains("api.blockchain.info/charts/hash-rate") {
                r#"{"unit":"TH/s","values":[{"x":1,"y":6.5e8}]}"#.to_owned()
            } else if endpoint.contains("blockchain.info/q/getblockcount") {
                "840000".to_owned()
            } else {
                return Err(TransportError::HostForbidden(
                    "unexpected network fixture endpoint".to_owned(),
                ));
            };
            Ok(HttpResponse { status: 200, body })
        }
    }

    fn bps(value: u32) -> BasisPoints {
        match BasisPoints::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid basis points: {error}"),
        }
    }

    fn evidence(value: &str) -> replikan_opportunities::EvidenceRef {
        match replikan_opportunities::EvidenceRef::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid evidence: {error}"),
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

    fn price_feeds() -> Vec<PriceFeedRequest> {
        let coinbase = match CoinbaseExchangePriceAdapter::new("BTC-USD", "BTC") {
            Ok(value) => value,
            Err(error) => unreachable!("valid Coinbase adapter: {error}"),
        };
        let kraken = match KrakenPriceAdapter::new("XBTUSD", "BTC") {
            Ok(value) => value,
            Err(error) => unreachable!("valid Kraken adapter: {error}"),
        };
        vec![
            PriceFeedRequest {
                feed: PublicPriceFeed::Coinbase(coinbase),
                confidence: bps(9_000),
                evidence: vec![evidence("price:coinbase")],
            },
            PriceFeedRequest {
                feed: PublicPriceFeed::Kraken(kraken),
                confidence: bps(9_000),
                evidence: vec![evidence("price:kraken")],
            },
        ]
    }

    fn network_feeds() -> Vec<BitcoinNetworkRequest> {
        vec![
            BitcoinNetworkRequest {
                feed: BitcoinNetworkFeed::MempoolSpace,
                horizon_seconds: 86_400,
                ttl_ms: 60_000,
                confidence: bps(9_000),
            },
            BitcoinNetworkRequest {
                feed: BitcoinNetworkFeed::BlockchainCom,
                horizon_seconds: 86_400,
                ttl_ms: 60_000,
                confidence: bps(8_500),
            },
        ]
    }

    fn policies() -> BitcoinCyclePolicies {
        BitcoinCyclePolicies {
            price: PriceConsensusPolicy {
                minimum_sources: 2,
                maximum_age_ms: 60_000,
                maximum_spread: bps(100),
            },
            network: NetworkConsensusPolicy {
                minimum_sources: 2,
                maximum_age_ms: 60_000,
                maximum_hashrate_spread: bps(500),
                maximum_emission_spread: bps(100),
            },
            selection: SelectionPolicy {
                economics: OpportunityPolicy {
                    max_risk: bps(3_000),
                    minimum_net_profit: Money::from_micros(1_000_000),
                    minimum_post_action_reserve: Money::from_micros(20_000_000),
                },
                minimum_confidence: bps(5_000),
                maximum_quote_age_ms: 60_000,
                minimum_evidence_count: 1,
                capital_charge: bps(100),
            },
            survival: SurvivalPolicy {
                critical_reserve: Money::from_micros(10_000_000),
                constrained_reserve: Money::from_micros(40_000_000),
                maximum_drawdown: bps(2_000),
            },
            control: match ControlPolicy::new(
                Money::ZERO,
                Money::ZERO,
                Money::from_micros(50_000_000),
                Money::from_micros(1_000_000),
            ) {
                Ok(value) => value,
                Err(error) => unreachable!("valid control policy: {error}"),
            },
            price_ttl_ms: 30_000,
        }
    }

    fn baseline(opening: i128) -> CapitalBaseline {
        match CapitalBaseline::new(
            Money::from_micros(opening),
            Money::from_micros(20_000_000),
            bps(500),
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid baseline: {error}"),
        }
    }

    fn electricity() -> ElectricityObservation {
        match ElectricityObservation::new(
            "energy-contract",
            Money::from_micros(150_000),
            NOW - 500,
            NOW + 60_000,
            bps(9_000),
            vec![evidence("energy:contract")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid electricity observation: {error}"),
        }
    }

    fn inventory(valid_until: u64) -> AuthorizedResourceInventory {
        let authorization = match AuthorizationGrant::new(
            evidence("authorization:owner"),
            NOW - 1_000,
            valid_until,
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid authorization: {error}"),
        };
        let benchmark = match MiningBenchmark::new(
            "sha256d",
            1_000_000_000_000_000_000,
            3_000,
            NOW - 500,
            NOW + 60_000,
            bps(9_500),
            vec![evidence("benchmark:asic")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid benchmark: {error}"),
        };
        match AuthorizedResourceInventory::new(vec![AuthorizedResource::local(
            resource_id("local:asic-0"),
            ResourceKind::Asic,
            authorization,
            vec![benchmark],
        )]) {
            Ok(value) => value,
            Err(error) => unreachable!("valid inventory: {error}"),
        }
    }

    fn template() -> MiningDeploymentTemplate {
        match MiningDeploymentTemplate::new(
            opportunity_id("btc:asic-0"),
            resource_id("local:asic-0"),
            "BTC",
            "sha256d",
            100_000_000,
            bps(100),
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            bps(500),
            NOW - 500,
            NOW + 60_000,
            bps(9_500),
            vec![evidence("deployment:asic-0")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid template: {error}"),
        }
    }

    #[test]
    fn capital_injection_changes_liquidity_but_not_realized_revenue() {
        let mut ledger = EconomicLedger::default();
        assert!(
            ledger
                .append(
                    EntryKind::CapitalInjection,
                    Money::from_micros(50_000_000),
                    "funding:capital",
                )
                .is_ok()
        );
        assert!(
            ledger
                .append(
                    EntryKind::EarnedRevenue,
                    Money::from_micros(10_000_000),
                    "pool:payout",
                )
                .is_ok()
        );
        assert!(
            ledger
                .append(
                    EntryKind::EnergyCost,
                    Money::from_micros(2_000_000),
                    "meter:energy",
                )
                .is_ok()
        );

        let state = match derive_economic_state(&ledger, baseline(20_000_000)) {
            Ok(value) => value,
            Err(error) => unreachable!("valid economic state: {error}"),
        };
        assert_eq!(
            state.fitness.realized_revenue,
            Money::from_micros(10_000_000)
        );
        assert_eq!(
            state.fitness.realized_costs.energy,
            Money::from_micros(2_000_000)
        );
        assert_eq!(state.fitness.liquid_capital, Money::from_micros(78_000_000));
    }

    #[test]
    fn insolvent_preflight_freezes_before_any_market_request() {
        let mut economic_ledger = EconomicLedger::default();
        assert!(
            economic_ledger
                .append(
                    EntryKind::CapitalWithdrawal,
                    Money::from_micros(1),
                    "withdrawal:test",
                )
                .is_ok()
        );
        let mut decision_ledger = DecisionLedger::default();
        let price_client = MarketPriceHttpClient::new(PriceTransport {
            calls: Cell::new(0),
        });
        let network_transport = NetworkTransport {
            calls: Cell::new(0),
        };

        let report = run_authorized_bitcoin_cycle(
            &economic_ledger,
            &mut decision_ledger,
            baseline(0),
            &AuthorizedResourceInventory::default(),
            &[],
            &price_client,
            &network_transport,
            &price_feeds(),
            &network_feeds(),
            &electricity(),
            policies(),
            NOW,
        );
        let report = match report {
            Ok(value) => value,
            Err(error) => unreachable!("freeze is a valid cycle result: {error}"),
        };

        assert!(matches!(
            report.decision,
            ControlDecision::Freeze {
                state: SurvivalState::Insolvent
            }
        ));
        assert_eq!(price_client.transport().calls.get(), 0);
        assert_eq!(network_transport.calls.get(), 0);
        assert_eq!(decision_ledger.entries().len(), 1);
    }

    #[test]
    fn expired_authorization_holds_without_market_requests_and_is_recorded() {
        let economic_ledger = EconomicLedger::default();
        let mut decision_ledger = DecisionLedger::default();
        let price_client = MarketPriceHttpClient::new(PriceTransport {
            calls: Cell::new(0),
        });
        let network_transport = NetworkTransport {
            calls: Cell::new(0),
        };

        let report = run_authorized_bitcoin_cycle(
            &economic_ledger,
            &mut decision_ledger,
            baseline(100_000_000),
            &inventory(NOW - 1),
            &[template()],
            &price_client,
            &network_transport,
            &price_feeds(),
            &network_feeds(),
            &electricity(),
            policies(),
            NOW,
        );
        let report = match report {
            Ok(value) => value,
            Err(error) => unreachable!("authorization rejection is a hold: {error}"),
        };

        assert!(matches!(
            report.decision,
            ControlDecision::Hold {
                state: SurvivalState::Healthy,
                mode: SpendingMode::Normal,
                reason: HoldReason::NoAcceptedOpportunity,
            }
        ));
        assert_eq!(price_client.transport().calls.get(), 0);
        assert_eq!(network_transport.calls.get(), 0);
        assert_eq!(
            decision_ledger.entries()[0]
                .observation
                .materialization_rejections,
            1
        );
    }

    #[test]
    fn successful_cycle_records_verified_evidence_and_selected_opportunity() {
        let economic_ledger = EconomicLedger::default();
        let mut decision_ledger = DecisionLedger::default();
        let price_client = MarketPriceHttpClient::new(PriceTransport {
            calls: Cell::new(0),
        });
        let network_transport = NetworkTransport {
            calls: Cell::new(0),
        };

        let report = run_authorized_bitcoin_cycle(
            &economic_ledger,
            &mut decision_ledger,
            baseline(100_000_000),
            &inventory(NOW + 60_000),
            &[template()],
            &price_client,
            &network_transport,
            &price_feeds(),
            &network_feeds(),
            &electricity(),
            policies(),
            NOW,
        );
        let report = match report {
            Ok(value) => value,
            Err(error) => unreachable!("valid profitable cycle: {error}"),
        };

        assert!(matches!(
            report.decision,
            ControlDecision::Run { ref opportunity_id, .. }
                if opportunity_id.as_str() == "btc:asic-0"
        ));
        assert_eq!(price_client.transport().calls.get(), 2);
        assert_eq!(network_transport.calls.get(), 4);
        let entry = &decision_ledger.entries()[0].observation;
        assert_eq!(entry.price_source_count, 2);
        assert_eq!(entry.network_source_count, 2);
        assert!(entry.evidence.iter().any(|value| value == "price:coinbase"));
        assert!(entry.evidence.iter().any(|value| value == "benchmark:asic"));
        assert!(
            entry
                .evidence
                .iter()
                .any(|value| value == "authorization:owner")
        );
    }
}
