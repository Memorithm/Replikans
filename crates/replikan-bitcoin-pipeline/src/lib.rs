#![forbid(unsafe_code)]

use core::fmt;
use replikan_market_http::{HttpTransport, MarketPriceHttpClient};
use replikan_mining_market::network_consensus::{NetworkConsensusError, NetworkConsensusPolicy};
use replikan_mining_market::price_consensus::PriceConsensusPolicy;
use replikan_mining_market::snapshot_builder::{ElectricityObservation, MiningDeploymentProfile};
use replikan_mining_pipeline::{
    PipelineError, PriceFeedRequest, VerifiedMiningSnapshot, build_verified_mining_snapshot,
};
use replikan_network_feeds::{BitcoinNetworkRequest, fetch_bitcoin_network_observation};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkSourceFailure {
    pub source_id: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedBitcoinMiningSnapshot {
    pub verified: VerifiedMiningSnapshot,
    pub network_source_failures: Vec<NetworkSourceFailure>,
}

#[allow(clippy::too_many_arguments)]
pub fn build_verified_bitcoin_mining_snapshot<P, N>(
    price_client: &MarketPriceHttpClient<P>,
    network_transport: &N,
    price_feeds: &[PriceFeedRequest],
    price_policy: PriceConsensusPolicy,
    network_feeds: &[BitcoinNetworkRequest],
    network_policy: NetworkConsensusPolicy,
    deployment: &MiningDeploymentProfile,
    electricity: &ElectricityObservation,
    now_unix_ms: u64,
    price_ttl_ms: u64,
) -> Result<VerifiedBitcoinMiningSnapshot, BitcoinPipelineError>
where
    P: HttpTransport,
    N: HttpTransport,
{
    let mut network_observations = Vec::with_capacity(network_feeds.len());
    let mut network_source_failures = Vec::new();

    for request in network_feeds {
        match fetch_bitcoin_network_observation(network_transport, request, now_unix_ms) {
            Ok(observation) => network_observations.push(observation),
            Err(error) => network_source_failures.push(NetworkSourceFailure {
                source_id: request.feed.source_id().to_owned(),
                reason: error.to_string(),
            }),
        }
    }

    let verified = match build_verified_mining_snapshot(
        price_client,
        "BTC",
        "sha256d",
        price_feeds,
        price_policy,
        network_observations,
        network_policy,
        deployment,
        electricity,
        now_unix_ms,
        price_ttl_ms,
    ) {
        Ok(verified) => verified,
        Err(PipelineError::NetworkConsensus(error)) => {
            return Err(BitcoinPipelineError::NetworkConsensus {
                error,
                source_failures: network_source_failures,
            });
        }
        Err(error) => return Err(BitcoinPipelineError::Pipeline(error)),
    };

    Ok(VerifiedBitcoinMiningSnapshot {
        verified,
        network_source_failures,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BitcoinPipelineError {
    NetworkConsensus {
        error: NetworkConsensusError,
        source_failures: Vec<NetworkSourceFailure>,
    },
    Pipeline(PipelineError),
}

impl fmt::Display for BitcoinPipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NetworkConsensus {
                error,
                source_failures,
            } => write!(
                f,
                "Bitcoin network consensus failed after {} source failures: {error}",
                source_failures.len()
            ),
            Self::Pipeline(error) => write!(f, "verified Bitcoin mining pipeline failed: {error}"),
        }
    }
}

impl std::error::Error for BitcoinPipelineError {}

#[cfg(test)]
mod tests {
    use super::*;
    use replikan_core::{BasisPoints, Money};
    use replikan_market_feeds::{CoinbaseExchangePriceAdapter, KrakenPriceAdapter};
    use replikan_market_http::{HttpResponse, TransportError};
    use replikan_mining_pipeline::{PriceFeedRequest, PublicPriceFeed};
    use replikan_network_feeds::BitcoinNetworkFeed;
    use replikan_opportunities::{EvidenceRef, OpportunityId};

    const NOW: u64 = 1_000_000;

    struct PriceTransport;

    impl HttpTransport for PriceTransport {
        fn get(&self, endpoint: &str) -> Result<HttpResponse, TransportError> {
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
        fail_blockchain: bool,
    }

    impl HttpTransport for NetworkTransport {
        fn get(&self, endpoint: &str) -> Result<HttpResponse, TransportError> {
            if endpoint.contains("blockchain.info") && self.fail_blockchain {
                return Err(TransportError::Request("blockchain.com offline".to_owned()));
            }

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

    fn opportunity_id(value: &str) -> OpportunityId {
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

    fn price_policy() -> PriceConsensusPolicy {
        PriceConsensusPolicy {
            minimum_sources: 2,
            maximum_age_ms: 60_000,
            maximum_spread: bps(100),
        }
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

    fn network_policy(minimum_sources: usize) -> NetworkConsensusPolicy {
        NetworkConsensusPolicy {
            minimum_sources,
            maximum_age_ms: 60_000,
            maximum_hashrate_spread: bps(500),
            maximum_emission_spread: bps(100),
        }
    }

    fn deployment() -> MiningDeploymentProfile {
        match MiningDeploymentProfile::new(
            opportunity_id("btc:miner-a"),
            "BTC",
            "sha256d",
            100_000_000,
            100_000_000_000_000,
            3_000,
            bps(100),
            Money::from_micros(25_000),
            Money::ZERO,
            Money::ZERO,
            Money::from_micros(500_000),
            Money::ZERO,
            Money::from_micros(10_000_000),
            bps(700),
            NOW - 500,
            NOW + 60_000,
            bps(9_500),
            vec![evidence("hardware:sha256d-benchmark")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid Bitcoin deployment: {error}"),
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

    fn run(
        network_transport: &NetworkTransport,
        minimum_network_sources: usize,
    ) -> Result<VerifiedBitcoinMiningSnapshot, BitcoinPipelineError> {
        build_verified_bitcoin_mining_snapshot(
            &MarketPriceHttpClient::new(PriceTransport),
            network_transport,
            &price_feeds(),
            price_policy(),
            &network_feeds(),
            network_policy(minimum_network_sources),
            &deployment(),
            &electricity(),
            NOW,
            30_000,
        )
    }

    #[test]
    fn independent_price_and_network_transports_build_verified_bitcoin_snapshot() {
        let result = run(
            &NetworkTransport {
                fail_blockchain: false,
            },
            2,
        );
        let verified = match result {
            Ok(value) => value,
            Err(error) => unreachable!("valid Bitcoin pipeline: {error}"),
        };

        assert_eq!(verified.verified.price_consensus.source_count, 2);
        assert_eq!(verified.verified.network_consensus.source_count, 2);
        assert!(verified.network_source_failures.is_empty());
        assert_eq!(verified.verified.snapshot.asset_symbol, "BTC");
        assert_eq!(verified.verified.snapshot.algorithm, "sha256d");
    }

    #[test]
    fn network_outage_is_tolerated_only_when_quorum_policy_remains_satisfied() {
        let result = run(
            &NetworkTransport {
                fail_blockchain: true,
            },
            1,
        );
        let verified = match result {
            Ok(value) => value,
            Err(error) => unreachable!("single-source policy remains satisfied: {error}"),
        };

        assert_eq!(verified.verified.network_consensus.source_count, 1);
        assert_eq!(verified.network_source_failures.len(), 1);
        assert_eq!(
            verified.network_source_failures[0].source_id,
            "blockchain.com"
        );
    }

    #[test]
    fn network_outage_fails_closed_with_diagnostics_when_quorum_is_required() {
        let result = run(
            &NetworkTransport {
                fail_blockchain: true,
            },
            2,
        );

        assert!(matches!(
            result,
            Err(BitcoinPipelineError::NetworkConsensus {
                error: NetworkConsensusError::InsufficientIndependentSources {
                    required: 2,
                    available: 1,
                },
                source_failures,
            }) if source_failures.len() == 1 && source_failures[0].source_id == "blockchain.com"
        ));
    }
}
