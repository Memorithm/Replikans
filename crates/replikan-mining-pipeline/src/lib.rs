#![forbid(unsafe_code)]

use core::fmt;
use replikan_core::BasisPoints;
use replikan_market_feeds::{CoinbaseExchangePriceAdapter, KrakenPriceAdapter, PublicPriceAdapter};
use replikan_market_http::{FetchError, HttpTransport, MarketPriceHttpClient};
use replikan_mining_market::MiningMarketSnapshot;
use replikan_mining_market::network_consensus::{
    NetworkConsensus, NetworkConsensusError, NetworkConsensusPolicy, NetworkObservation,
    derive_network_consensus,
};
use replikan_mining_market::price_consensus::{
    PriceConsensus, PriceConsensusError, PriceConsensusPolicy, PriceObservation,
    derive_price_consensus,
};
use replikan_mining_market::snapshot_builder::{
    ElectricityObservation, MiningDeploymentProfile, SnapshotBuildError, build_consensus_snapshot,
};
use replikan_opportunities::EvidenceRef;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicPriceFeed {
    Coinbase(CoinbaseExchangePriceAdapter),
    Kraken(KrakenPriceAdapter),
}

impl PublicPriceFeed {
    #[must_use]
    pub fn provider_id(&self) -> &'static str {
        match self {
            Self::Coinbase(adapter) => adapter.provider_id(),
            Self::Kraken(adapter) => adapter.provider_id(),
        }
    }

    fn fetch<T>(
        &self,
        client: &MarketPriceHttpClient<T>,
        observed_at_unix_ms: u64,
        valid_until_unix_ms: u64,
        confidence: BasisPoints,
        evidence: Vec<EvidenceRef>,
    ) -> Result<PriceObservation, FetchError>
    where
        T: HttpTransport,
    {
        match self {
            Self::Coinbase(adapter) => client.fetch_price(
                adapter,
                observed_at_unix_ms,
                valid_until_unix_ms,
                confidence,
                evidence,
            ),
            Self::Kraken(adapter) => client.fetch_price(
                adapter,
                observed_at_unix_ms,
                valid_until_unix_ms,
                confidence,
                evidence,
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceFeedRequest {
    pub feed: PublicPriceFeed,
    pub confidence: BasisPoints,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceSourceFailure {
    pub source_id: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedMiningSnapshot {
    pub snapshot: MiningMarketSnapshot,
    pub price_consensus: PriceConsensus,
    pub network_consensus: NetworkConsensus,
    pub price_source_failures: Vec<PriceSourceFailure>,
}

#[allow(clippy::too_many_arguments)]
pub fn build_verified_mining_snapshot<T>(
    client: &MarketPriceHttpClient<T>,
    asset_symbol: &str,
    algorithm: &str,
    price_feeds: &[PriceFeedRequest],
    price_policy: PriceConsensusPolicy,
    network_observations: Vec<NetworkObservation>,
    network_policy: NetworkConsensusPolicy,
    deployment: &MiningDeploymentProfile,
    electricity: &ElectricityObservation,
    now_unix_ms: u64,
    price_ttl_ms: u64,
) -> Result<VerifiedMiningSnapshot, PipelineError>
where
    T: HttpTransport,
{
    if price_ttl_ms == 0 {
        return Err(PipelineError::ZeroPriceTtl);
    }
    let price_valid_until_unix_ms = now_unix_ms
        .checked_add(price_ttl_ms)
        .ok_or(PipelineError::TimestampOverflow)?;

    let mut observations = Vec::with_capacity(price_feeds.len());
    let mut price_source_failures = Vec::new();

    for request in price_feeds {
        match request.feed.fetch(
            client,
            now_unix_ms,
            price_valid_until_unix_ms,
            request.confidence,
            request.evidence.clone(),
        ) {
            Ok(observation) => observations.push(observation),
            Err(error) => price_source_failures.push(PriceSourceFailure {
                source_id: request.feed.provider_id().to_owned(),
                reason: error.to_string(),
            }),
        }
    }

    let price_consensus =
        derive_price_consensus(asset_symbol, observations, price_policy, now_unix_ms).map_err(
            |error| PipelineError::PriceConsensus {
                error,
                source_failures: price_source_failures.clone(),
            },
        )?;

    let network_consensus = derive_network_consensus(
        asset_symbol,
        algorithm,
        network_observations,
        network_policy,
        now_unix_ms,
    )
    .map_err(PipelineError::NetworkConsensus)?;

    let snapshot = build_consensus_snapshot(
        &price_consensus,
        &network_consensus,
        deployment,
        electricity,
    )
    .map_err(PipelineError::Snapshot)?;

    Ok(VerifiedMiningSnapshot {
        snapshot,
        price_consensus,
        network_consensus,
        price_source_failures,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PipelineError {
    ZeroPriceTtl,
    TimestampOverflow,
    PriceConsensus {
        error: PriceConsensusError,
        source_failures: Vec<PriceSourceFailure>,
    },
    NetworkConsensus(NetworkConsensusError),
    Snapshot(SnapshotBuildError),
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroPriceTtl => write!(f, "price observation TTL must be greater than zero"),
            Self::TimestampOverflow => write!(f, "price observation validity timestamp overflow"),
            Self::PriceConsensus {
                error,
                source_failures,
            } => write!(
                f,
                "price consensus failed after {} source failures: {error}",
                source_failures.len()
            ),
            Self::NetworkConsensus(error) => write!(f, "network consensus failed: {error}"),
            Self::Snapshot(error) => write!(f, "verified mining snapshot failed: {error}"),
        }
    }
}

impl std::error::Error for PipelineError {}

#[cfg(test)]
mod tests {
    use super::*;
    use replikan_core::Money;
    use replikan_market_http::{HttpResponse, TransportError};
    use replikan_opportunities::OpportunityId;

    const NOW: u64 = 1_000_000;

    struct FixtureTransport {
        coinbase_price: &'static str,
        kraken_price: &'static str,
        fail_kraken: bool,
    }

    impl HttpTransport for FixtureTransport {
        fn get(&self, endpoint: &str) -> Result<HttpResponse, TransportError> {
            if endpoint.contains("api.exchange.coinbase.com") {
                return Ok(HttpResponse {
                    status: 200,
                    body: format!(r#"{{"price":"{}"}}"#, self.coinbase_price),
                });
            }
            if endpoint.contains("api.kraken.com") {
                if self.fail_kraken {
                    return Err(TransportError::Request("kraken offline".to_owned()));
                }
                return Ok(HttpResponse {
                    status: 200,
                    body: format!(
                        r#"{{"error":[],"result":{{"PAIR":{{"c":["{}","1"]}}}}}}"#,
                        self.kraken_price
                    ),
                });
            }
            Err(TransportError::HostForbidden("unexpected".to_owned()))
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

    fn feeds() -> Vec<PriceFeedRequest> {
        let coinbase = match CoinbaseExchangePriceAdapter::new("TST-USD", "TST") {
            Ok(value) => value,
            Err(error) => unreachable!("valid Coinbase adapter: {error}"),
        };
        let kraken = match KrakenPriceAdapter::new("TSTUSD", "TST") {
            Ok(value) => value,
            Err(error) => unreachable!("valid Kraken adapter: {error}"),
        };

        vec![
            PriceFeedRequest {
                feed: PublicPriceFeed::Coinbase(coinbase),
                confidence: bps(9_000),
                evidence: vec![evidence("coinbase:fixture")],
            },
            PriceFeedRequest {
                feed: PublicPriceFeed::Kraken(kraken),
                confidence: bps(8_500),
                evidence: vec![evidence("kraken:fixture")],
            },
        ]
    }

    fn price_policy(minimum_sources: usize) -> PriceConsensusPolicy {
        PriceConsensusPolicy {
            minimum_sources,
            maximum_age_ms: 60_000,
            maximum_spread: bps(500),
        }
    }

    fn network_observation(source_id: &str, hashrate: u128, emission: u128) -> NetworkObservation {
        match NetworkObservation::new(
            source_id,
            "TST",
            "sha256",
            hashrate,
            emission,
            86_400,
            NOW - 1_000,
            NOW + 60_000,
            bps(9_000),
            vec![evidence(&format!("network:{source_id}"))],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid network observation: {error}"),
        }
    }

    fn networks() -> Vec<NetworkObservation> {
        vec![
            network_observation("node-a", 100_000, 1_000_000),
            network_observation("node-b", 101_000, 1_010_000),
        ]
    }

    fn network_policy() -> NetworkConsensusPolicy {
        NetworkConsensusPolicy {
            minimum_sources: 2,
            maximum_age_ms: 60_000,
            maximum_hashrate_spread: bps(500),
            maximum_emission_spread: bps(500),
        }
    }

    fn deployment() -> MiningDeploymentProfile {
        match MiningDeploymentProfile::new(
            id("tst:rig-a"),
            "TST",
            "sha256",
            100_000_000,
            1_000,
            1_200,
            bps(100),
            Money::from_micros(10_000),
            Money::ZERO,
            Money::ZERO,
            Money::from_micros(100_000),
            Money::ZERO,
            Money::from_micros(5_000_000),
            bps(700),
            NOW - 500,
            NOW + 60_000,
            bps(9_500),
            vec![evidence("hardware:benchmark")],
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

    fn client(fail_kraken: bool, kraken_price: &'static str) -> MarketPriceHttpClient<FixtureTransport> {
        MarketPriceHttpClient::new(FixtureTransport {
            coinbase_price: "100.000000",
            kraken_price,
            fail_kraken,
        })
    }

    fn run(
        client: &MarketPriceHttpClient<FixtureTransport>,
        minimum_price_sources: usize,
        price_ttl_ms: u64,
    ) -> Result<VerifiedMiningSnapshot, PipelineError> {
        build_verified_mining_snapshot(
            client,
            "TST",
            "sha256",
            &feeds(),
            price_policy(minimum_price_sources),
            networks(),
            network_policy(),
            &deployment(),
            &electricity(),
            NOW,
            price_ttl_ms,
        )
    }

    #[test]
    fn builds_snapshot_from_two_exchange_prices_and_network_quorum() {
        let result = run(&client(false, "101.000000"), 2, 30_000);
        let verified = match result {
            Ok(value) => value,
            Err(error) => unreachable!("valid verified pipeline: {error}"),
        };

        assert_eq!(
            verified.price_consensus.price_per_unit,
            Money::from_micros(100_000_000)
        );
        assert_eq!(verified.price_consensus.source_count, 2);
        assert_eq!(verified.network_consensus.source_count, 2);
        assert!(verified.price_source_failures.is_empty());
        assert_eq!(
            verified.snapshot.asset_price_per_unit,
            Money::from_micros(100_000_000)
        );
    }

    #[test]
    fn one_exchange_failure_is_tolerated_only_when_policy_still_has_quorum() {
        let result = run(&client(true, "101.000000"), 1, 30_000);
        let verified = match result {
            Ok(value) => value,
            Err(error) => unreachable!("single-source policy remains satisfied: {error}"),
        };

        assert_eq!(verified.price_consensus.source_count, 1);
        assert_eq!(verified.price_source_failures.len(), 1);
        assert_eq!(verified.price_source_failures[0].source_id, "kraken");
    }

    #[test]
    fn insufficient_price_quorum_fails_closed_with_source_diagnostics() {
        let result = run(&client(true, "101.000000"), 2, 30_000);

        assert!(matches!(
            result,
            Err(PipelineError::PriceConsensus {
                error: PriceConsensusError::InsufficientIndependentSources {
                    required: 2,
                    available: 1,
                },
                source_failures,
            }) if source_failures.len() == 1 && source_failures[0].source_id == "kraken"
        ));
    }

    #[test]
    fn excessive_exchange_spread_blocks_snapshot_creation() {
        let result = run(&client(false, "150.000000"), 2, 30_000);

        assert!(matches!(
            result,
            Err(PipelineError::PriceConsensus {
                error: PriceConsensusError::SpreadExceeded { .. },
                ..
            })
        ));
    }

    #[test]
    fn zero_price_ttl_is_rejected_before_network_or_economic_use() {
        assert_eq!(
            run(&client(false, "101.000000"), 2, 0),
            Err(PipelineError::ZeroPriceTtl)
        );
    }
}
