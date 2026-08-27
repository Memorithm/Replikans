use core::fmt;
use replikan_core::{BasisPoints, Money};
use replikan_opportunities::EvidenceRef;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceObservation {
    pub source_id: String,
    pub asset_symbol: String,
    pub price_per_unit: Money,
    pub observed_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub confidence: BasisPoints,
    pub evidence: Vec<EvidenceRef>,
}

impl PriceObservation {
    pub fn new(
        source_id: impl Into<String>,
        asset_symbol: impl Into<String>,
        price_per_unit: Money,
        observed_at_unix_ms: u64,
        valid_until_unix_ms: u64,
        confidence: BasisPoints,
        evidence: Vec<EvidenceRef>,
    ) -> Result<Self, PriceConsensusError> {
        let source_id = source_id.into();
        let asset_symbol = asset_symbol.into();

        if source_id.trim().is_empty() {
            return Err(PriceConsensusError::EmptySourceId);
        }
        if asset_symbol.trim().is_empty() {
            return Err(PriceConsensusError::EmptyAssetSymbol);
        }
        if price_per_unit.is_negative() {
            return Err(PriceConsensusError::NegativePrice);
        }
        if valid_until_unix_ms <= observed_at_unix_ms {
            return Err(PriceConsensusError::InvalidValidityWindow);
        }
        if evidence.is_empty() {
            return Err(PriceConsensusError::MissingEvidence);
        }

        Ok(Self {
            source_id,
            asset_symbol,
            price_per_unit,
            observed_at_unix_ms,
            valid_until_unix_ms,
            confidence,
            evidence,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PriceConsensusPolicy {
    pub minimum_sources: usize,
    pub maximum_age_ms: u64,
    pub maximum_spread: BasisPoints,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceConsensus {
    pub asset_symbol: String,
    pub price_per_unit: Money,
    pub confidence: BasisPoints,
    pub oldest_observed_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub source_count: usize,
    pub evidence: Vec<EvidenceRef>,
}

pub fn derive_price_consensus(
    asset_symbol: &str,
    observations: impl IntoIterator<Item = PriceObservation>,
    policy: PriceConsensusPolicy,
    now_unix_ms: u64,
) -> Result<PriceConsensus, PriceConsensusError> {
    if asset_symbol.trim().is_empty() {
        return Err(PriceConsensusError::EmptyAssetSymbol);
    }
    if policy.minimum_sources == 0 {
        return Err(PriceConsensusError::ZeroMinimumSources);
    }

    let mut seen_sources = BTreeSet::new();
    let mut usable = Vec::new();

    for observation in observations {
        if observation.asset_symbol != asset_symbol {
            return Err(PriceConsensusError::AssetMismatch {
                expected: asset_symbol.to_owned(),
                observed: observation.asset_symbol,
            });
        }
        if !seen_sources.insert(observation.source_id.clone()) {
            return Err(PriceConsensusError::DuplicateSource(observation.source_id));
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
        return Err(PriceConsensusError::InsufficientIndependentSources {
            required: policy.minimum_sources,
            available: usable.len(),
        });
    }

    usable.sort_by_key(|observation| observation.price_per_unit);
    let median_index = (usable.len() - 1) / 2;
    let median = usable[median_index].price_per_unit;
    let minimum = usable[0].price_per_unit;
    let maximum = usable[usable.len() - 1].price_per_unit;
    enforce_spread(minimum, median, maximum, policy.maximum_spread)?;

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

    Ok(PriceConsensus {
        asset_symbol: asset_symbol.to_owned(),
        price_per_unit: median,
        confidence,
        oldest_observed_at_unix_ms,
        valid_until_unix_ms,
        source_count: usable.len(),
        evidence,
    })
}

fn enforce_spread(
    minimum: Money,
    median: Money,
    maximum: Money,
    allowed: BasisPoints,
) -> Result<(), PriceConsensusError> {
    let median_micros = median.micros();
    let spread = maximum
        .micros()
        .checked_sub(minimum.micros())
        .ok_or(PriceConsensusError::ArithmeticOverflow)?;

    if median_micros == 0 {
        if spread == 0 {
            return Ok(());
        }
        return Err(PriceConsensusError::ZeroMedianWithNonZeroSpread);
    }

    let spread_bps = spread
        .checked_mul(i128::from(BasisPoints::FULL_SCALE))
        .ok_or(PriceConsensusError::ArithmeticOverflow)?
        / median_micros;

    if spread_bps > i128::from(allowed.value()) {
        let spread_bps =
            u128::try_from(spread_bps).map_err(|_| PriceConsensusError::ArithmeticOverflow)?;
        return Err(PriceConsensusError::SpreadExceeded {
            spread_bps,
            maximum_bps: allowed.value(),
        });
    }

    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PriceConsensusError {
    EmptySourceId,
    EmptyAssetSymbol,
    NegativePrice,
    InvalidValidityWindow,
    MissingEvidence,
    ZeroMinimumSources,
    DuplicateSource(String),
    AssetMismatch { expected: String, observed: String },
    InsufficientIndependentSources { required: usize, available: usize },
    ZeroMedianWithNonZeroSpread,
    SpreadExceeded { spread_bps: u128, maximum_bps: u32 },
    ArithmeticOverflow,
}

impl fmt::Display for PriceConsensusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySourceId => write!(f, "price source id cannot be empty"),
            Self::EmptyAssetSymbol => write!(f, "price asset symbol cannot be empty"),
            Self::NegativePrice => write!(f, "price cannot be negative"),
            Self::InvalidValidityWindow => write!(f, "price validity window is invalid"),
            Self::MissingEvidence => write!(f, "price observation requires evidence"),
            Self::ZeroMinimumSources => write!(f, "price consensus requires at least one source"),
            Self::DuplicateSource(source) => write!(f, "duplicate price source: {source}"),
            Self::AssetMismatch { expected, observed } => {
                write!(
                    f,
                    "price asset mismatch: expected {expected}, observed {observed}"
                )
            }
            Self::InsufficientIndependentSources {
                required,
                available,
            } => write!(
                f,
                "insufficient independent price sources: required {required}, available {available}"
            ),
            Self::ZeroMedianWithNonZeroSpread => {
                write!(f, "zero median price with non-zero source spread")
            }
            Self::SpreadExceeded {
                spread_bps,
                maximum_bps,
            } => write!(
                f,
                "price source spread {spread_bps} bps exceeds maximum {maximum_bps} bps"
            ),
            Self::ArithmeticOverflow => write!(f, "price consensus arithmetic overflow"),
        }
    }
}

impl std::error::Error for PriceConsensusError {}

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
        price_micros: i128,
        observed_at_unix_ms: u64,
        confidence: u32,
    ) -> PriceObservation {
        match PriceObservation::new(
            source_id,
            "TST",
            Money::from_micros(price_micros),
            observed_at_unix_ms,
            observed_at_unix_ms + 120_000,
            bps(confidence),
            vec![evidence(&format!("price:{source_id}"))],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid observation: {error}"),
        }
    }

    fn policy(minimum_sources: usize, maximum_spread_bps: u32) -> PriceConsensusPolicy {
        PriceConsensusPolicy {
            minimum_sources,
            maximum_age_ms: 60_000,
            maximum_spread: bps(maximum_spread_bps),
        }
    }

    #[test]
    fn uses_median_price_and_conservative_confidence() {
        let now = 1_000_000;
        let result = derive_price_consensus(
            "TST",
            vec![
                observation("a", 100_000_000, now - 1_000, 9_000),
                observation("b", 101_000_000, now - 2_000, 8_000),
                observation("c", 99_000_000, now - 3_000, 9_500),
            ],
            policy(3, 300),
            now,
        );
        let consensus = match result {
            Ok(value) => value,
            Err(error) => unreachable!("valid consensus: {error}"),
        };

        assert_eq!(consensus.price_per_unit, Money::from_micros(100_000_000));
        assert_eq!(consensus.confidence, bps(8_000));
        assert_eq!(consensus.source_count, 3);
        assert_eq!(consensus.evidence.len(), 3);
    }

    #[test]
    fn even_source_count_uses_lower_median() {
        let now = 1_000_000;
        let consensus = match derive_price_consensus(
            "TST",
            vec![
                observation("a", 100_000_000, now - 1_000, 9_000),
                observation("b", 102_000_000, now - 1_000, 9_000),
            ],
            policy(2, 300),
            now,
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid consensus: {error}"),
        };

        assert_eq!(consensus.price_per_unit, Money::from_micros(100_000_000));
    }

    #[test]
    fn rejects_duplicate_source_identity() {
        let now = 1_000_000;
        let result = derive_price_consensus(
            "TST",
            vec![
                observation("same", 100_000_000, now - 1_000, 9_000),
                observation("same", 100_500_000, now - 1_000, 9_000),
            ],
            policy(2, 300),
            now,
        );

        assert_eq!(
            result,
            Err(PriceConsensusError::DuplicateSource("same".to_owned()))
        );
    }

    #[test]
    fn stale_sources_do_not_satisfy_quorum() {
        let now = 1_000_000;
        let result = derive_price_consensus(
            "TST",
            vec![
                observation("fresh", 100_000_000, now - 1_000, 9_000),
                observation("stale", 100_000_000, now - 61_000, 9_000),
            ],
            policy(2, 300),
            now,
        );

        assert_eq!(
            result,
            Err(PriceConsensusError::InsufficientIndependentSources {
                required: 2,
                available: 1,
            })
        );
    }

    #[test]
    fn rejects_excessive_cross_source_spread() {
        let now = 1_000_000;
        let result = derive_price_consensus(
            "TST",
            vec![
                observation("a", 100_000_000, now - 1_000, 9_000),
                observation("b", 101_000_000, now - 1_000, 9_000),
                observation("attacker", 150_000_000, now - 1_000, 9_000),
            ],
            policy(3, 500),
            now,
        );

        assert!(matches!(
            result,
            Err(PriceConsensusError::SpreadExceeded { .. })
        ));
    }
}
