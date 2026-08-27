#![forbid(unsafe_code)]

use core::fmt;

use replikan_bitcoin_planner::{BitcoinMiningPlan, PlannerError, plan_bitcoin_deployments};
use replikan_economics::EconomicFitness;
use replikan_market_http::{HttpTransport, MarketPriceHttpClient};
use replikan_mining_market::network_consensus::NetworkConsensusPolicy;
use replikan_mining_market::price_consensus::PriceConsensusPolicy;
use replikan_mining_market::snapshot_builder::ElectricityObservation;
use replikan_mining_pipeline::PriceFeedRequest;
use replikan_network_feeds::BitcoinNetworkRequest;
use replikan_opportunities::SelectionPolicy;
use replikan_resource::{
    AuthorizedResourceInventory, MaterializationFailure, MaterializationReport,
    MiningDeploymentTemplate, ResourceError, materialize_authorized_deployments,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedBitcoinMiningPlan {
    pub materialization: MaterializationReport,
    pub bitcoin: BitcoinMiningPlan,
}

#[allow(clippy::too_many_arguments)]
pub fn plan_authorized_bitcoin_resources<P, N>(
    inventory: &AuthorizedResourceInventory,
    templates: &[MiningDeploymentTemplate],
    price_client: &MarketPriceHttpClient<P>,
    network_transport: &N,
    price_feeds: &[PriceFeedRequest],
    price_policy: PriceConsensusPolicy,
    network_feeds: &[BitcoinNetworkRequest],
    network_policy: NetworkConsensusPolicy,
    electricity: &ElectricityObservation,
    fitness: EconomicFitness,
    selection_policy: SelectionPolicy,
    now_unix_ms: u64,
    price_ttl_ms: u64,
) -> Result<AuthorizedBitcoinMiningPlan, AuthorizedPlanningError>
where
    P: HttpTransport,
    N: HttpTransport,
{
    let materialization = materialize_authorized_deployments(inventory, templates, now_unix_ms)
        .map_err(AuthorizedPlanningError::Resource)?;

    if materialization.profiles.is_empty() {
        return Err(AuthorizedPlanningError::NoAuthorizedDeployments {
            rejected: materialization.rejected,
        });
    }

    let bitcoin = plan_bitcoin_deployments(
        price_client,
        network_transport,
        price_feeds,
        price_policy,
        network_feeds,
        network_policy,
        &materialization.profiles,
        electricity,
        fitness,
        selection_policy,
        now_unix_ms,
        price_ttl_ms,
    )
    .map_err(AuthorizedPlanningError::Planner)?;

    Ok(AuthorizedBitcoinMiningPlan {
        materialization,
        bitcoin,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorizedPlanningError {
    Resource(ResourceError),
    NoAuthorizedDeployments {
        rejected: Vec<MaterializationFailure>,
    },
    Planner(PlannerError),
}

impl fmt::Display for AuthorizedPlanningError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resource(error) => write!(f, "resource materialization failed: {error}"),
            Self::NoAuthorizedDeployments { rejected } => write!(
                f,
                "no authorized Bitcoin deployment remains after {} rejections",
                rejected.len()
            ),
            Self::Planner(error) => write!(f, "authorized Bitcoin planner failed: {error}"),
        }
    }
}

impl std::error::Error for AuthorizedPlanningError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    use replikan_core::{BasisPoints, Money};
    use replikan_economics::{OperatingCosts, OpportunityPolicy};
    use replikan_market_feeds::{CoinbaseExchangePriceAdapter, KrakenPriceAdapter};
    use replikan_market_http::{HttpResponse, TransportError};
    use replikan_mining_pipeline::{PriceFeedRequest, PublicPriceFeed};
    use replikan_network_feeds::BitcoinNetworkFeed;
    use replikan_opportunities::{EvidenceRef, OpportunityId};
    use replikan_resource::{
        AuthorizationGrant, AuthorizedResource, MiningBenchmark, ResourceId, ResourceKind,
    };

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

    fn resource_id(value: &str) -> ResourceId {
        match ResourceId::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid resource id: {error}"),
        }
    }

    fn opportunity_id(value: &str) -> OpportunityId {
        match OpportunityId::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid opportunity id: {error}"),
        }
    }

    fn authorization(reference: &str, valid_until: u64) -> AuthorizationGrant {
        match AuthorizationGrant::new(evidence(reference), NOW - 1_000, valid_until) {
            Ok(value) => value,
            Err(error) => unreachable!("valid authorization: {error}"),
        }
    }

    fn benchmark(hashrate: u128, power_watts: u64) -> MiningBenchmark {
        match MiningBenchmark::new(
            "sha256d",
            hashrate,
            power_watts,
            NOW - 500,
            NOW + 60_000,
            bps(9_500),
            vec![evidence("benchmark:measured")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid benchmark: {error}"),
        }
    }

    fn resource(
        id: &str,
        valid_until: u64,
        hashrate: u128,
        power_watts: u64,
    ) -> AuthorizedResource {
        AuthorizedResource::local(
            resource_id(id),
            ResourceKind::Asic,
            authorization(&format!("authorization:{id}"), valid_until),
            vec![benchmark(hashrate, power_watts)],
        )
    }

    fn inventory(resources: Vec<AuthorizedResource>) -> AuthorizedResourceInventory {
        match AuthorizedResourceInventory::new(resources) {
            Ok(value) => value,
            Err(error) => unreachable!("valid inventory: {error}"),
        }
    }

    fn template(name: &str, resource: &str) -> MiningDeploymentTemplate {
        match MiningDeploymentTemplate::new(
            opportunity_id(name),
            resource_id(resource),
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
            vec![evidence("deployment:owner-config")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid template: {error}"),
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
                confidence: bps(9_000),
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
            Err(error) => unreachable!("valid electricity: {error}"),
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

    fn run(
        inventory: &AuthorizedResourceInventory,
        templates: &[MiningDeploymentTemplate],
        price_client: &MarketPriceHttpClient<PriceTransport>,
        network_transport: &NetworkTransport,
    ) -> Result<AuthorizedBitcoinMiningPlan, AuthorizedPlanningError> {
        plan_authorized_bitcoin_resources(
            inventory,
            templates,
            price_client,
            network_transport,
            &price_feeds(),
            price_policy(),
            &network_feeds(),
            network_policy(),
            &electricity(),
            fitness(),
            selection_policy(),
            NOW,
            30_000,
        )
    }

    #[test]
    fn authorized_benchmark_flows_into_verified_economic_plan() {
        let inventory = inventory(vec![resource(
            "local:asic-0",
            NOW + 120_000,
            1_000_000_000_000_000,
            3_000,
        )]);
        let templates = vec![template("btc:asic-0", "local:asic-0")];
        let price_client = MarketPriceHttpClient::new(PriceTransport {
            calls: Cell::new(0),
        });
        let network_transport = NetworkTransport {
            calls: Cell::new(0),
        };

        let plan = match run(&inventory, &templates, &price_client, &network_transport) {
            Ok(value) => value,
            Err(error) => unreachable!("valid authorized plan: {error}"),
        };

        assert_eq!(plan.materialization.profiles.len(), 1);
        assert_eq!(plan.bitcoin.snapshots.len(), 1);
        assert_eq!(
            plan.bitcoin.snapshots[0].miner_hashrate_units,
            1_000_000_000_000_000
        );
        assert_eq!(plan.bitcoin.snapshots[0].power_watts, 3_000);
        assert_eq!(price_client.transport().calls.get(), 2);
        assert_eq!(network_transport.calls.get(), 4);
        assert!(plan.bitcoin.best().is_some());
    }

    #[test]
    fn no_market_request_occurs_when_authorization_is_expired() {
        let inventory = inventory(vec![resource(
            "local:asic-expired",
            NOW - 1,
            1_000_000_000_000_000,
            3_000,
        )]);
        let templates = vec![template("btc:expired", "local:asic-expired")];
        let price_client = MarketPriceHttpClient::new(PriceTransport {
            calls: Cell::new(0),
        });
        let network_transport = NetworkTransport {
            calls: Cell::new(0),
        };

        let result = run(&inventory, &templates, &price_client, &network_transport);
        assert!(matches!(
            result,
            Err(AuthorizedPlanningError::NoAuthorizedDeployments { rejected })
                if rejected.len() == 1
        ));
        assert_eq!(price_client.transport().calls.get(), 0);
        assert_eq!(network_transport.calls.get(), 0);
    }

    #[test]
    fn invalid_resource_is_isolated_while_authorized_candidate_is_planned() {
        let inventory = inventory(vec![
            resource(
                "local:asic-good",
                NOW + 120_000,
                1_000_000_000_000_000,
                3_000,
            ),
            resource("local:asic-expired", NOW - 1, 1_000_000_000_000_000, 3_000),
        ]);
        let templates = vec![
            template("btc:good", "local:asic-good"),
            template("btc:expired", "local:asic-expired"),
        ];
        let price_client = MarketPriceHttpClient::new(PriceTransport {
            calls: Cell::new(0),
        });
        let network_transport = NetworkTransport {
            calls: Cell::new(0),
        };

        let plan = match run(&inventory, &templates, &price_client, &network_transport) {
            Ok(value) => value,
            Err(error) => unreachable!("valid mixed authorized plan: {error}"),
        };

        assert_eq!(plan.materialization.profiles.len(), 1);
        assert_eq!(plan.materialization.rejected.len(), 1);
        assert_eq!(plan.bitcoin.snapshots.len(), 1);
        assert_eq!(price_client.transport().calls.get(), 2);
        assert_eq!(network_transport.calls.get(), 4);
    }
}
