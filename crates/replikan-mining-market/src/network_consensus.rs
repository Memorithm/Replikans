use core::fmt;
use replikan_core::BasisPoints;
use replikan_opportunities::EvidenceRef;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkObservation {
    pub source_id: String,
    pub asset_symbol: String,
    pub algorithm: String,
    pub network_hashrate_units: u128,
    pub network_emission_atoms: u128,
    pub horizon_seconds: u64,
    pub observed_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub confidence: BasisPoints,
    pub evidence: Vec<EvidenceRef>,
}

impl NetworkObservation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_id: impl Into<String>,
        asset_symbol: impl Into<String>,
        algorithm: impl Into<String>,
        network_hashrate_units: u128,
        network_emission_atoms: u128,
        horizon_seconds: u64,
        observed_at_unix_ms: u64,
        valid_until_unix_ms: u64,
        confidence: BasisPoints,
        evidence: Vec<EvidenceRef>,
    ) -> Result<Self, NetworkConsensusError> {
        let source_id = source_id.into();
        let asset_symbol = asset_symbol.into();
        let algorithm = algorithm.into();

        if source_id.trim().is_empty() {
            return Err(NetworkConsensusError::EmptySourceId);
        }
        if asset_symbol.trim().is_empty() {
            return Err(NetworkConsensusError::EmptyAssetSymbol);
        }
        if algorithm.trim().is_empty() {
            return Err(NetworkConsensusError::EmptyAlgorithm);
        }
        if network_hashrate_units == 0 {
            return Err(NetworkConsensusError::ZeroNetworkHashrate);
        }
        if horizon_seconds == 0 {
            return Err(NetworkConsensusError::ZeroHorizon);
        }
        if valid_until_unix_ms <= observed_at_unix_ms {
            return Err(NetworkConsensusError::InvalidValidityWindow);
        }
        if evidence.is_empty() {
            return Err(NetworkConsensusError::MissingEvidence);
        }

        Ok(Self {
            source_id,
            asset_symbol,
            algorithm,
            network_hashrate_units,
            network_emission_atoms,
            horizon_seconds,
            observed_at_unix_ms,
            valid_until_unix_ms,
            confidence,
            evidence,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkConsensusPolicy {
    pub minimum_sources: usize,
    pub maximum_age_ms: u64,
    pub maximum_hashrate_spread: BasisPoints,
    pub maximum_emission_spread: BasisPoints,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkConsensus {
    pub asset_symbol: String,
    pub algorithm: String,
    pub network_hashrate_units: u128,
    pub network_emission_atoms: u128,
    pub horizon_seconds: u64,
    pub confidence: BasisPoints,
    pub oldest_observed_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub source_count: usize,
    pub evidence: Vec<EvidenceRef>,
}

pub fn derive_network_consensus(
    asset_symbol: &str,
    algorithm: &str,
    observations: impl IntoIterator<Item = NetworkObservation>,
    policy: NetworkConsensusPolicy,
    now_unix_ms: u64,
) -> Result<NetworkConsensus, NetworkConsensusError> {
    if asset_symbol.trim().is_empty() {
        return Err(NetworkConsensusError::EmptyAssetSymbol);
    }
    if algorithm.trim().is_empty() {
        return Err(NetworkConsensusError::EmptyAlgorithm);
    }
    if policy.minimum_sources == 0 {
        return Err(NetworkConsensusError::ZeroMinimumSources);
    }

    let mut seen_sources = BTreeSet::new();
    let mut usable = Vec::new();
    let mut expected_horizon = None;

    for observation in observations {
        if observation.asset_symbol != asset_symbol {
            return Err(NetworkConsensusError::AssetMismatch {
                expected: asset_symbol.to_owned(),
                observed: observation.asset_symbol,
            });
        }
        if observation.algorithm != algorithm {
            return Err(NetworkConsensusError::AlgorithmMismatch {
                expected: algorithm.to_owned(),
                observed: observation.algorithm,
            });
        }
        if !seen_sources.insert(observation.source_id.clone()) {
            return Err(NetworkConsensusError::DuplicateSource(observation.source_id));
        }

        if let Some(horizon) = expected_horizon {
            if observation.horizon_seconds != horizon {
                return Err(NetworkConsensusError::HorizonMismatch {
                    expected: horizon,
                    observed: observation.horizon_seconds,
                });
            }
        } else {
            expected_horizon = Some(observation.horizon_seconds);
        }

        if observation.observed_at_unix_ms > now_unix_ms
            || now_unix_ms > observation.valid_until_unix_ms
            || now_unix_ms.saturating_sub(observation.observed_at_unix_ms) > policy.maximum_age_ms
        {
            continue;
        }
        usable.push(observation);
    }

    if usable.len() < policy.minimum_sources {
        return Err(NetworkConsensusError::InsufficientIndependentSources {
            required: policy.minimum_sources,
            available: usable.len(),
        });
    }

    let horizon_seconds = usable[0].horizon_seconds;
    let mut hashrates = usable
        .iter()
        .map(|observation| observation.network_hashrate_units)
        .collect::<Vec<_>>();
    let mut emissions = usable
        .iter()
        .map(|observation| observation.network_emission_atoms)
        .collect::<Vec<_>>();
    hashrates.sort_unstable();
    emissions.sort_unstable();

    let median_index = (usable.len() - 1) / 2;
    let network_hashrate_units = hashrates[median_index];
    let network_emission_atoms = emissions[median_index];

    enforce_spread(
        hashrates[0],
        network_hashrate_units,
        hashrates[hashrates.len() - 1],
        policy.maximum_hashrate_spread,
        Metric::Hashrate,
    )?;
    enforce_spread(
        emissions[0],
        network_emission_atoms,
        emissions[emissions.len() - 1],
        policy.maximum_emission_spread,
        Metric::Emission,
    )?;

    let mut confidence = usable[0].confidence;
    let mut oldest_observed_at_unix_ms = usable[0].observed_at_unix_ms;
    let mut valid_until_unix_ms = usable[0].valid_until_unix_ms;
    let mut evidence = Vec::new();

    for observation in &usable {
        if observation.confidence < confidence {
            confidence = observation.confidence;
        }
        oldest_observed_at_unix_ms =
            oldest_observed_at_unix_ms.min(observation.observed_at_unix_ms);
        valid_until_unix_ms = valid_until_unix_ms.min(observation.valid_until_unix_ms);
        evidence.extend(observation.evidence.iter().cloned());
    }

    Ok(NetworkConsensus {
        asset_symbol: asset_symbol.to_owned(),
        algorithm: algorithm.to_owned(),
        network_hashrate_units,
        network_emission_atoms,
        horizon_seconds,
        confidence,
        oldest_observed_at_unix_ms,
        valid_until_unix_ms,
        source_count: usable.len(),
        evidence,
    })
}

#[derive(Clone, Copy)]
enum Metric {
    Hashrate,
    Emission,
}

fn enforce_spread(
    minimum: u128,
    median: u128,
    maximum: u128,
    allowed: BasisPoints,
    metric: Metric,
) -> Result<(), NetworkConsensusError> {
    let spread = maximum
        .checked_sub(minimum)
        .ok_or(NetworkConsensusError::ArithmeticOverflow)?;

    if median == 0 {
        if spread == 0 {
            return Ok(());
        }
        return Err(match metric {
            Metric::Hashrate => NetworkConsensusError::ZeroMedianHashrateWithNonZeroSpread,
            Metric::Emission => NetworkConsensusError::ZeroMedianEmissionWithNonZeroSpread,
        });
    }

    let spread_bps = spread
        .checked_mul(u128::from(BasisPoints::FULL_SCALE))
        .ok_or(NetworkConsensusError::ArithmeticOverflow)?
        / median;

    if spread_bps > u128::from(allowed.value()) {
        return Err(match metric {
            Metric::Hashrate => NetworkConsensusError::HashrateSpreadExceeded {
                spread_bps,
                maximum_bps: allowed.value(),
            },
            Metric::Emission => NetworkConsensusError::EmissionSpreadExceeded {
                spread_bps,
                maximum_bps: allowed.value(),
            },
        });
    }

    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkConsensusError {
    EmptySourceId,
    EmptyAssetSymbol,
    EmptyAlgorithm,
    ZeroNetworkHashrate,
    ZeroHorizon,
    InvalidValidityWindow,
    MissingEvidence,
    ZeroMinimumSources,
    DuplicateSource(String),
    AssetMismatch { expected: String, observed: String },
    AlgorithmMismatch { expected: String, observed: String },
    HorizonMismatch { expected: u64, observed: u64 },
    InsufficientIndependentSources { required: usize, available: usize },
    ZeroMedianHashrateWithNonZeroSpread,
    ZeroMedianEmissionWithNonZeroSpread,
    HashrateSpreadExceeded { spread_bps: u128, maximum_bps: u32 },
    EmissionSpreadExceeded { spread_bps: u128, maximum_bps: u32 },
    ArithmeticOverflow,
}

impl fmt::Display for NetworkConsensusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySourceId => write!(f, "network source id cannot be empty"),
            Self::EmptyAssetSymbol => write!(f, "network asset symbol cannot be empty"),
            Self::EmptyAlgorithm => write!(f, "network algorithm cannot be empty"),
            Self::ZeroNetworkHashrate => write!(f, "network hashrate must be greater than zero"),
            Self::ZeroHorizon => write!(f, "network horizon must be greater than zero"),
            Self::InvalidValidityWindow => write!(f, "network validity window is invalid"),
            Self::MissingEvidence => write!(f, "network observation requires evidence"),
            Self::ZeroMinimumSources => write!(f, "network consensus requires at least one source"),
            Self::DuplicateSource(source) => write!(f, "duplicate network source: {source}"),
            Self::AssetMismatch { expected, observed } => write!(
                f,
                "network asset mismatch: expected {expected}, observed {observed}"
            ),
            Self::AlgorithmMismatch { expected, observed } => write!(
                f,
                "network algorithm mismatch: expected {expected}, observed {observed}"
            ),
            Self::HorizonMismatch { expected, observed } => write!(
                f,
                "network horizon mismatch: expected {expected}, observed {observed}"
            ),
            Self::InsufficientIndependentSources {
                required,
                available,
            } => write!(
                f,
                "insufficient independent network sources: required {required}, available {available}"
            ),
            Self::ZeroMedianHashrateWithNonZeroSpread => {
                write!(f, "zero median network hashrate with non-zero spread")
            }
            Self::ZeroMedianEmissionWithNonZeroSpread => {
                write!(f, "zero median network emission with non-zero spread")
            }
            Self::HashrateSpreadExceeded {
                spread_bps,
                maximum_bps,
            } => write!(
                f,
                "network hashrate spread {spread_bps} bps exceeds maximum {maximum_bps} bps"
            ),
            Self::EmissionSpreadExceeded {
                spread_bps,
                maximum_bps,
            } => write!(
                f,
                "network emission spread {spread_bps} bps exceeds maximum {maximum_bps} bps"
            ),
            Self::ArithmeticOverflow => write!(f, "network consensus arithmetic overflow"),
        }
    }
}

impl std::error::Error for NetworkConsensusError {}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn observation(
        source_id: &str,
        hashrate: u128,
        emission: u128,
        observed_at_unix_ms: u64,
        confidence: u32,
    ) -> NetworkObservation {
        match NetworkObservation::new(
            source_id,
            "TST",
            "sha256",
            hashrate,
            emission,
            86_400,
            observed_at_unix_ms,
            observed_at_unix_ms + 120_000,
            bps(confidence),
            vec![evidence(&format!("network:{source_id}"))],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid network observation: {error}"),
        }
    }

    fn policy(minimum_sources: usize) -> NetworkConsensusPolicy {
        NetworkConsensusPolicy {
            minimum_sources,
            maximum_age_ms: 60_000,
            maximum_hashrate_spread: bps(500),
            maximum_emission_spread: bps(500),
        }
    }

    #[test]
    fn derives_lower_median_network_values_and_minimum_confidence() {
        let now = 1_000_000;
        let consensus = match derive_network_consensus(
            "TST",
            "sha256",
            vec![
                observation("a", 100_000, 1_000_000, now - 1_000, 9_000),
                observation("b", 102_000, 1_020_000, now - 2_000, 8_000),
                observation("c", 98_000, 990_000, now - 3_000, 9_500),
            ],
            policy(3),
            now,
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid network consensus: {error}"),
        };

        assert_eq!(consensus.network_hashrate_units, 100_000);
        assert_eq!(consensus.network_emission_atoms, 1_000_000);
        assert_eq!(consensus.confidence, bps(8_000));
        assert_eq!(consensus.source_count, 3);
    }

    #[test]
    fn stale_observations_do_not_satisfy_quorum() {
        let now = 1_000_000;
        let result = derive_network_consensus(
            "TST",
            "sha256",
            vec![
                observation("fresh", 100_000, 1_000_000, now - 1_000, 9_000),
                observation("stale", 100_000, 1_000_000, now - 61_000, 9_000),
            ],
            policy(2),
            now,
        );

        assert_eq!(
            result,
            Err(NetworkConsensusError::InsufficientIndependentSources {
                required: 2,
                available: 1,
            })
        );
    }

    #[test]
    fn rejects_duplicate_source_identity() {
        let now = 1_000_000;
        let result = derive_network_consensus(
            "TST",
            "sha256",
            vec![
                observation("same", 100_000, 1_000_000, now - 1_000, 9_000),
                observation("same", 101_000, 1_000_000, now - 1_000, 9_000),
            ],
            policy(2),
            now,
        );

        assert_eq!(
            result,
            Err(NetworkConsensusError::DuplicateSource("same".to_owned()))
        );
    }

    #[test]
    fn rejects_inconsistent_reward_horizon() {
        let now = 1_000_000;
        let mut second = observation("b", 100_000, 1_000_000, now - 1_000, 9_000);
        second.horizon_seconds = 3_600;
        let result = derive_network_consensus(
            "TST",
            "sha256",
            vec![
                observation("a", 100_000, 1_000_000, now - 1_000, 9_000),
                second,
            ],
            policy(2),
            now,
        );

        assert_eq!(
            result,
            Err(NetworkConsensusError::HorizonMismatch {
                expected: 86_400,
                observed: 3_600,
            })
        );
    }

    #[test]
    fn rejects_manipulated_hashrate_outlier() {
        let now = 1_000_000;
        let result = derive_network_consensus(
            "TST",
            "sha256",
            vec![
                observation("a", 100_000, 1_000_000, now - 1_000, 9_000),
                observation("b", 101_000, 1_000_000, now - 1_000, 9_000),
                observation("attacker", 160_000, 1_000_000, now - 1_000, 9_000),
            ],
            policy(3),
            now,
        );

        assert!(matches!(
            result,
            Err(NetworkConsensusError::HashrateSpreadExceeded { .. })
        ));
    }
}
