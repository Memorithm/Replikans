#![forbid(unsafe_code)]

use core::fmt;
use std::collections::BTreeSet;

use replikan_economics::EconomicFitness;
use replikan_market_http::{FetchError, HttpTransport, MarketPriceHttpClient};
use replikan_mining_market::MiningMarketSnapshot;
use replikan_mining_market::network_consensus::{
    NetworkConsensus, NetworkConsensusError, NetworkConsensusPolicy, derive_network_consensus,
};
use replikan_mining_market::price_consensus::{
    PriceConsensus, PriceConsensusError, PriceConsensusPolicy, PriceObservation,
    derive_price_consensus,
};
use replikan_mining_market::snapshot_builder::{
    ElectricityObservation, MiningDeploymentProfile, build_consensus_snapshot,
};
use replikan_mining_pipeline::{PriceFeedRequest, PublicPriceFeed};
use replikan_network_feeds::{BitcoinNetworkRequest, fetch_bitcoin_network_observation};
use replikan_opportunities::{
    EngineError, OpportunityId, OpportunityQuote, SelectionPolicy, SelectionReport,
    evaluate_and_rank,
};

const VERIFIED_BITCOIN_SOURCE: &str = "bitcoin:verified-market";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFailure {
    pub source_id: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedBitcoinMarketContext {
    pub price_consensus: PriceConsensus,
    pub network_consensus: NetworkConsensus,
    pub price_source_failures: Vec<SourceFailure>,
    pub network_source_failures: Vec<SourceFailure>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeploymentProjectionFailure {
    pub id: OpportunityId,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BitcoinMiningPlan {
    pub market: VerifiedBitcoinMarketContext,
    pub snapshots: Vec<MiningMarketSnapshot>,
    pub projection_failures: Vec<DeploymentProjectionFailure>,
    pub selection: SelectionReport,
}

impl BitcoinMiningPlan {
    #[must_use]
    pub fn best(&self) -> Option<&replikan_opportunities::RankedOpportunity> {
        self.selection.best()
    }
}

#[allow(clippy::too_many_arguments)]
pub fn collect_verified_bitcoin_market_context<P, N>(
    price_client: &MarketPriceHttpClient<P>,
    network_transport: &N,
    price_feeds: &[PriceFeedRequest],
    price_policy: PriceConsensusPolicy,
    network_feeds: &[BitcoinNetworkRequest],
    network_policy: NetworkConsensusPolicy,
    now_unix_ms: u64,
    price_ttl_ms: u64,
) -> Result<VerifiedBitcoinMarketContext, PlannerError>
where
    P: HttpTransport,
    N: HttpTransport,
{
    if price_ttl_ms == 0 {
        return Err(PlannerError::ZeroPriceTtl);
    }
    let price_valid_until_unix_ms = now_unix_ms
        .checked_add(price_ttl_ms)
        .ok_or(PlannerError::TimestampOverflow)?;

    let mut price_observations = Vec::with_capacity(price_feeds.len());
    let mut price_source_failures = Vec::new();

    for request in price_feeds {
        match fetch_price(
            price_client,
            request,
            now_unix_ms,
            price_valid_until_unix_ms,
        ) {
            Ok(observation) => price_observations.push(observation),
            Err(error) => price_source_failures.push(SourceFailure {
                source_id: request.feed.provider_id().to_owned(),
                reason: error.to_string(),
            }),
        }
    }

    let price_consensus =
        derive_price_consensus("BTC", price_observations, price_policy, now_unix_ms).map_err(
            |error| PlannerError::PriceConsensus {
                error,
                source_failures: price_source_failures.clone(),
            },
        )?;

    let mut network_observations = Vec::with_capacity(network_feeds.len());
    let mut network_source_failures = Vec::new();

    for request in network_feeds {
        match fetch_bitcoin_network_observation(network_transport, request, now_unix_ms) {
            Ok(observation) => network_observations.push(observation),
            Err(error) => network_source_failures.push(SourceFailure {
                source_id: request.feed.source_id().to_owned(),
                reason: error.to_string(),
            }),
        }
    }

    let network_consensus = derive_network_consensus(
        "BTC",
        "sha256d",
        network_observations,
        network_policy,
        now_unix_ms,
    )
    .map_err(|error| PlannerError::NetworkConsensus {
        error,
        source_failures: network_source_failures.clone(),
    })?;

    Ok(VerifiedBitcoinMarketContext {
        price_consensus,
        network_consensus,
        price_source_failures,
        network_source_failures,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn plan_bitcoin_deployments<P, N>(
    price_client: &MarketPriceHttpClient<P>,
    network_transport: &N,
    price_feeds: &[PriceFeedRequest],
    price_policy: PriceConsensusPolicy,
    network_feeds: &[BitcoinNetworkRequest],
    network_policy: NetworkConsensusPolicy,
    deployments: &[MiningDeploymentProfile],
    electricity: &ElectricityObservation,
    fitness: EconomicFitness,
    selection_policy: SelectionPolicy,
    now_unix_ms: u64,
    price_ttl_ms: u64,
) -> Result<BitcoinMiningPlan, PlannerError>
where
    P: HttpTransport,
    N: HttpTransport,
{
    validate_deployment_ids(deployments)?;

    let market = collect_verified_bitcoin_market_context(
        price_client,
        network_transport,
        price_feeds,
        price_policy,
        network_feeds,
        network_policy,
        now_unix_ms,
        price_ttl_ms,
    )?;

    let mut snapshots = Vec::with_capacity(deployments.len());
    let mut quotes = Vec::with_capacity(deployments.len());
    let mut projection_failures = Vec::new();

    for deployment in deployments {
        match project_deployment(&market, deployment, electricity) {
            Ok((snapshot, quote)) => {
                snapshots.push(snapshot);
                quotes.push(quote);
            }
            Err(reason) => projection_failures.push(DeploymentProjectionFailure {
                id: deployment.id.clone(),
                reason,
            }),
        }
    }

    let selection = evaluate_and_rank(fitness, quotes, selection_policy, now_unix_ms)
        .map_err(PlannerError::Engine)?;

    Ok(BitcoinMiningPlan {
        market,
        snapshots,
        projection_failures,
        selection,
    })
}

fn fetch_price<T>(
    client: &MarketPriceHttpClient<T>,
    request: &PriceFeedRequest,
    observed_at_unix_ms: u64,
    valid_until_unix_ms: u64,
) -> Result<PriceObservation, FetchError>
where
    T: HttpTransport,
{
    match &request.feed {
        PublicPriceFeed::Coinbase(adapter) => client.fetch_price(
            adapter,
            observed_at_unix_ms,
            valid_until_unix_ms,
            request.confidence,
            request.evidence.clone(),
        ),
        PublicPriceFeed::Kraken(adapter) => client.fetch_price(
            adapter,
            observed_at_unix_ms,
            valid_until_unix_ms,
            request.confidence,
            request.evidence.clone(),
        ),
    }
}

fn validate_deployment_ids(deployments: &[MiningDeploymentProfile]) -> Result<(), PlannerError> {
    if deployments.is_empty() {
        return Err(PlannerError::NoDeployments);
    }

    let mut ids = BTreeSet::new();
    for deployment in deployments {
        if !ids.insert(deployment.id.as_str().to_owned()) {
            return Err(PlannerError::DuplicateDeploymentId(deployment.id.clone()));
        }
    }
    Ok(())
}

fn project_deployment(
    market: &VerifiedBitcoinMarketContext,
    deployment: &MiningDeploymentProfile,
    electricity: &ElectricityObservation,
) -> Result<(MiningMarketSnapshot, OpportunityQuote), String> {
    let snapshot = build_consensus_snapshot(
        &market.price_consensus,
        &market.network_consensus,
        deployment,
        electricity,
    )
    .map_err(|error| error.to_string())?;

    let observation = snapshot
        .to_observation()
        .map_err(|error| error.to_string())?;
    let quote = observation
        .to_quote(VERIFIED_BITCOIN_SOURCE)
        .map_err(|error| error.to_string())?;

    Ok((snapshot, quote))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlannerError {
    NoDeployments,
    DuplicateDeploymentId(OpportunityId),
    ZeroPriceTtl,
    TimestampOverflow,
    PriceConsensus {
        error: PriceConsensusError,
        source_failures: Vec<SourceFailure>,
    },
    NetworkConsensus {
        error: NetworkConsensusError,
        source_failures: Vec<SourceFailure>,
    },
    Engine(EngineError),
}

impl fmt::Display for PlannerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDeployments => write!(f, "Bitcoin planner requires at least one deployment"),
            Self::DuplicateDeploymentId(id) => {
                write!(f, "duplicate Bitcoin deployment id: {}", id.as_str())
            }
            Self::ZeroPriceTtl => write!(f, "Bitcoin price TTL must be greater than zero"),
            Self::TimestampOverflow => write!(f, "Bitcoin price validity timestamp overflow"),
            Self::PriceConsensus {
                error,
                source_failures,
            } => write!(
                f,
                "Bitcoin price consensus failed after {} source failures: {error}",
                source_failures.len()
            ),
            Self::NetworkConsensus {
                error,
                source_failures,
            } => write!(
                f,
                "Bitcoin network consensus failed after {} source failures: {error}",
                source_failures.len()
            ),
            Self::Engine(error) => write!(f, "Bitcoin opportunity evaluation failed: {error}"),
        }
    }
}

impl std::error::Error for PlannerError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    use replikan_core::{BasisPoints, Money};
    use replikan_economics::{OperatingCosts, OpportunityPolicy};
    use replikan_market_feeds::{CoinbaseExchangePriceAdapter, KrakenPriceAdapter};
    use replikan_market_http::{HttpResponse, TransportError};
    use replikan_network_feeds::BitcoinNetworkFeed;
    use replikan_opportunities::{EvidenceRef, RejectionReason};

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

    fn evidence(value: &str) -> EvidenceRef {
        match EvidenceRef::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid evidence: {error}"),
        }
    }

    fn id(value: &str) -> OpportunityId {
        match OpportunityId::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid opportunity id: {error}"),
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

    fn price_policy() -> PriceConsensusPolicy {
        PriceConsensusPolicy {
            minimum_sources: 2,
            maximum_age_ms: 60_000,
            maximum_spread: bps(100),
        }
    }

    fn network_policy() -> NetworkConsensusPolicy {
        NetworkConsensusPolicy {
            minimum_sources: 2,
            maximum_age_ms: 60_000,
            maximum_hashrate_spread: bps(500),
            maximum_emission_spread: bps(100),
        }
    }

    fn deployment(name: &str, hashrate: u128, power_watts: u64) -> MiningDeploymentProfile {
        match MiningDeploymentProfile::new(
            id(name),
            "BTC",
            "sha256d",
            100_000_000,
            hashrate,
            power_watts,
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
            vec![evidence(&format!("hardware:{name}"))],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid deployment: {error}"),
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

    fn fitness() -> EconomicFitness {
        EconomicFitness {
            realized_revenue: Money::from_micros(100_000_000),
            realized_costs: OperatingCosts::default(),
            liquid_capital: Money::from_micros(1_000_000_000),
            survival_reserve: Money::from_micros(100_000_000),
            drawdown: bps(100),
        }
    }

    fn selection_policy() -> SelectionPolicy {
        SelectionPolicy {
            economics: OpportunityPolicy {
                max_risk: bps(2_000),
                minimum_net_profit: Money::from_micros(1),
                minimum_post_action_reserve: Money::from_micros(100_000_000),
            },
            minimum_confidence: bps(8_000),
            maximum_quote_age_ms: 60_000,
            minimum_evidence_count: 4,
            capital_charge: bps(0),
        }
    }

    fn planner(
        price_client: &MarketPriceHttpClient<PriceTransport>,
        network_transport: &NetworkTransport,
        deployments: &[MiningDeploymentProfile],
    ) -> Result<BitcoinMiningPlan, PlannerError> {
        plan_bitcoin_deployments(
            price_client,
            network_transport,
            &price_feeds(),
            price_policy(),
            &network_feeds(),
            network_policy(),
            deployments,
            &electricity(),
            fitness(),
            selection_policy(),
            NOW,
            30_000,
        )
    }

    #[test]
    fn multiple_deployments_share_one_market_collection_cycle() {
        let price_client = MarketPriceHttpClient::new(PriceTransport {
            calls: Cell::new(0),
        });
        let network_transport = NetworkTransport {
            calls: Cell::new(0),
        };
        let deployments = vec![
            deployment("btc:efficient", 1_000_000_000_000_000, 3_000),
            deployment("btc:inefficient", 1_000_000_000_000_000, 10_000),
        ];

        let plan = match planner(&price_client, &network_transport, &deployments) {
            Ok(value) => value,
            Err(error) => unreachable!("valid planner: {error}"),
        };

        assert_eq!(price_client.transport().calls.get(), 2);
        assert_eq!(network_transport.calls.get(), 4);
        assert_eq!(plan.snapshots.len(), 2);
        assert!(plan.projection_failures.is_empty());
    }

    #[test]
    fn ranks_more_energy_efficient_deployment_first() {
        let price_client = MarketPriceHttpClient::new(PriceTransport {
            calls: Cell::new(0),
        });
        let network_transport = NetworkTransport {
            calls: Cell::new(0),
        };
        let deployments = vec![
            deployment("btc:inefficient", 1_000_000_000_000_000, 10_000),
            deployment("btc:efficient", 1_000_000_000_000_000, 3_000),
        ];

        let plan = match planner(&price_client, &network_transport, &deployments) {
            Ok(value) => value,
            Err(error) => unreachable!("valid planner: {error}"),
        };
        let best = match plan.best() {
            Some(value) => value,
            None => unreachable!("at least one deployment should be profitable"),
        };

        assert_eq!(best.quote.id.as_str(), "btc:efficient");
    }

    #[test]
    fn invalid_projection_does_not_hide_valid_candidate() {
        let price_client = MarketPriceHttpClient::new(PriceTransport {
            calls: Cell::new(0),
        });
        let network_transport = NetworkTransport {
            calls: Cell::new(0),
        };
        let deployments = vec![
            deployment("btc:valid", 1_000_000_000_000_000, 3_000),
            deployment("btc:impossible", u128::MAX, 3_000),
        ];

        let plan = match planner(&price_client, &network_transport, &deployments) {
            Ok(value) => value,
            Err(error) => unreachable!("planner should isolate invalid projection: {error}"),
        };

        assert_eq!(plan.snapshots.len(), 1);
        assert_eq!(plan.projection_failures.len(), 1);
        assert_eq!(plan.projection_failures[0].id.as_str(), "btc:impossible");
        assert_eq!(plan.selection.accepted.len(), 1);
    }

    #[test]
    fn duplicate_ids_fail_before_any_external_collection() {
        let price_client = MarketPriceHttpClient::new(PriceTransport {
            calls: Cell::new(0),
        });
        let network_transport = NetworkTransport {
            calls: Cell::new(0),
        };
        let deployments = vec![
            deployment("btc:duplicate", 1_000_000_000_000_000, 3_000),
            deployment("btc:duplicate", 900_000_000_000_000, 2_800),
        ];

        let result = planner(&price_client, &network_transport, &deployments);
        assert!(matches!(
            result,
            Err(PlannerError::DuplicateDeploymentId(_))
        ));
        assert_eq!(price_client.transport().calls.get(), 0);
        assert_eq!(network_transport.calls.get(), 0);
    }

    #[test]
    fn policy_rejections_remain_visible_in_selection_report() {
        let price_client = MarketPriceHttpClient::new(PriceTransport {
            calls: Cell::new(0),
        });
        let network_transport = NetworkTransport {
            calls: Cell::new(0),
        };
        let mut risky = deployment("btc:risky", 1_000_000_000_000_000, 3_000);
        risky.risk = bps(9_000);

        let plan = match planner(&price_client, &network_transport, &[risky]) {
            Ok(value) => value,
            Err(error) => unreachable!("planner should return policy rejection: {error}"),
        };

        assert!(plan.selection.accepted.is_empty());
        assert!(matches!(
            plan.selection.rejected.as_slice(),
            [rejected] if matches!(rejected.reason, RejectionReason::Economic(_))
        ));
    }
}
