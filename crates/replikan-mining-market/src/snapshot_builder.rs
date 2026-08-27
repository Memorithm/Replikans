use core::fmt;
use replikan_core::{BasisPoints, Money};
use replikan_opportunities::{EvidenceRef, OpportunityId};

use crate::network_consensus::NetworkConsensus;
use crate::price_consensus::PriceConsensus;
use crate::{MarketError, MiningMarketSnapshot};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MiningDeploymentProfile {
    pub id: OpportunityId,
    pub asset_symbol: String,
    pub algorithm: String,
    pub asset_atoms_per_unit: u128,
    pub miner_hashrate_units: u128,
    pub power_watts: u64,
    pub pool_fee: BasisPoints,
    pub onchain_fee: Money,
    pub compute_cost: Money,
    pub infrastructure_cost: Money,
    pub depreciation_cost: Money,
    pub other_cost: Money,
    pub capital_required: Money,
    pub risk: BasisPoints,
    pub observed_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub confidence: BasisPoints,
    pub evidence: Vec<EvidenceRef>,
}

impl MiningDeploymentProfile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: OpportunityId,
        asset_symbol: impl Into<String>,
        algorithm: impl Into<String>,
        asset_atoms_per_unit: u128,
        miner_hashrate_units: u128,
        power_watts: u64,
        pool_fee: BasisPoints,
        onchain_fee: Money,
        compute_cost: Money,
        infrastructure_cost: Money,
        depreciation_cost: Money,
        other_cost: Money,
        capital_required: Money,
        risk: BasisPoints,
        observed_at_unix_ms: u64,
        valid_until_unix_ms: u64,
        confidence: BasisPoints,
        evidence: Vec<EvidenceRef>,
    ) -> Result<Self, SnapshotBuildError> {
        let asset_symbol = asset_symbol.into();
        let algorithm = algorithm.into();

        if asset_symbol.trim().is_empty() {
            return Err(SnapshotBuildError::EmptyAssetSymbol);
        }
        if algorithm.trim().is_empty() {
            return Err(SnapshotBuildError::EmptyAlgorithm);
        }
        if asset_atoms_per_unit == 0 {
            return Err(SnapshotBuildError::ZeroAssetScale);
        }
        if miner_hashrate_units == 0 {
            return Err(SnapshotBuildError::ZeroMinerHashrate);
        }
        if power_watts == 0 {
            return Err(SnapshotBuildError::ZeroPower);
        }
        if valid_until_unix_ms <= observed_at_unix_ms {
            return Err(SnapshotBuildError::InvalidValidityWindow);
        }
        if [
            onchain_fee,
            compute_cost,
            infrastructure_cost,
            depreciation_cost,
            other_cost,
            capital_required,
        ]
        .into_iter()
        .any(Money::is_negative)
        {
            return Err(SnapshotBuildError::NegativeCostOrCapital);
        }
        if evidence.is_empty() {
            return Err(SnapshotBuildError::MissingEvidence);
        }

        Ok(Self {
            id,
            asset_symbol,
            algorithm,
            asset_atoms_per_unit,
            miner_hashrate_units,
            power_watts,
            pool_fee,
            onchain_fee,
            compute_cost,
            infrastructure_cost,
            depreciation_cost,
            other_cost,
            capital_required,
            risk,
            observed_at_unix_ms,
            valid_until_unix_ms,
            confidence,
            evidence,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ElectricityObservation {
    pub source_id: String,
    pub price_per_kwh: Money,
    pub observed_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub confidence: BasisPoints,
    pub evidence: Vec<EvidenceRef>,
}

impl ElectricityObservation {
    pub fn new(
        source_id: impl Into<String>,
        price_per_kwh: Money,
        observed_at_unix_ms: u64,
        valid_until_unix_ms: u64,
        confidence: BasisPoints,
        evidence: Vec<EvidenceRef>,
    ) -> Result<Self, SnapshotBuildError> {
        let source_id = source_id.into();
        if source_id.trim().is_empty() {
            return Err(SnapshotBuildError::EmptyElectricitySource);
        }
        if price_per_kwh.is_negative() {
            return Err(SnapshotBuildError::NegativeElectricityPrice);
        }
        if valid_until_unix_ms <= observed_at_unix_ms {
            return Err(SnapshotBuildError::InvalidValidityWindow);
        }
        if evidence.is_empty() {
            return Err(SnapshotBuildError::MissingEvidence);
        }

        Ok(Self {
            source_id,
            price_per_kwh,
            observed_at_unix_ms,
            valid_until_unix_ms,
            confidence,
            evidence,
        })
    }
}

pub fn build_consensus_snapshot(
    price: &PriceConsensus,
    network: &NetworkConsensus,
    deployment: &MiningDeploymentProfile,
    electricity: &ElectricityObservation,
) -> Result<MiningMarketSnapshot, SnapshotBuildError> {
    if price.asset_symbol != network.asset_symbol || price.asset_symbol != deployment.asset_symbol {
        return Err(SnapshotBuildError::AssetMismatch);
    }
    if network.algorithm != deployment.algorithm {
        return Err(SnapshotBuildError::AlgorithmMismatch);
    }

    let observed_at_unix_ms = price
        .oldest_observed_at_unix_ms
        .min(network.oldest_observed_at_unix_ms)
        .min(deployment.observed_at_unix_ms)
        .min(electricity.observed_at_unix_ms);
    let valid_until_unix_ms = price
        .valid_until_unix_ms
        .min(network.valid_until_unix_ms)
        .min(deployment.valid_until_unix_ms)
        .min(electricity.valid_until_unix_ms);

    if valid_until_unix_ms <= observed_at_unix_ms {
        return Err(SnapshotBuildError::NoCommonValidityWindow);
    }

    let confidence = minimum_confidence([
        price.confidence,
        network.confidence,
        deployment.confidence,
        electricity.confidence,
    ]);

    let mut evidence = Vec::new();
    evidence.extend(price.evidence.iter().cloned());
    evidence.extend(network.evidence.iter().cloned());
    evidence.extend(deployment.evidence.iter().cloned());
    evidence.extend(electricity.evidence.iter().cloned());

    MiningMarketSnapshot::new(
        deployment.id.clone(),
        deployment.asset_symbol.clone(),
        deployment.algorithm.clone(),
        observed_at_unix_ms,
        valid_until_unix_ms,
        network.horizon_seconds,
        price.price_per_unit,
        deployment.asset_atoms_per_unit,
        network.network_emission_atoms,
        deployment.miner_hashrate_units,
        network.network_hashrate_units,
        deployment.power_watts,
        electricity.price_per_kwh,
        deployment.pool_fee,
        deployment.onchain_fee,
        deployment.compute_cost,
        deployment.infrastructure_cost,
        deployment.depreciation_cost,
        deployment.other_cost,
        deployment.capital_required,
        deployment.risk,
        confidence,
        evidence,
    )
    .map_err(SnapshotBuildError::Market)
}

fn minimum_confidence(values: [BasisPoints; 4]) -> BasisPoints {
    let mut minimum = values[0];
    for value in values.into_iter().skip(1) {
        if value < minimum {
            minimum = value;
        }
    }
    minimum
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnapshotBuildError {
    EmptyAssetSymbol,
    EmptyAlgorithm,
    EmptyElectricitySource,
    ZeroAssetScale,
    ZeroMinerHashrate,
    ZeroPower,
    NegativeCostOrCapital,
    NegativeElectricityPrice,
    InvalidValidityWindow,
    MissingEvidence,
    AssetMismatch,
    AlgorithmMismatch,
    NoCommonValidityWindow,
    Market(MarketError),
}

impl fmt::Display for SnapshotBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyAssetSymbol => write!(f, "deployment asset symbol cannot be empty"),
            Self::EmptyAlgorithm => write!(f, "deployment algorithm cannot be empty"),
            Self::EmptyElectricitySource => write!(f, "electricity source cannot be empty"),
            Self::ZeroAssetScale => {
                write!(f, "asset atoms-per-unit scale must be greater than zero")
            }
            Self::ZeroMinerHashrate => write!(f, "miner hashrate must be greater than zero"),
            Self::ZeroPower => write!(f, "mining power draw must be greater than zero"),
            Self::NegativeCostOrCapital => {
                write!(f, "deployment costs and capital cannot be negative")
            }
            Self::NegativeElectricityPrice => write!(f, "electricity price cannot be negative"),
            Self::InvalidValidityWindow => write!(f, "observation validity window is invalid"),
            Self::MissingEvidence => write!(f, "snapshot inputs require evidence"),
            Self::AssetMismatch => write!(f, "price, network and deployment assets do not match"),
            Self::AlgorithmMismatch => write!(f, "network and deployment algorithms do not match"),
            Self::NoCommonValidityWindow => {
                write!(f, "snapshot inputs have no common validity window")
            }
            Self::Market(error) => write!(f, "market snapshot construction failed: {error}"),
        }
    }
}

impl std::error::Error for SnapshotBuildError {}

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

    fn id(value: &str) -> OpportunityId {
        match OpportunityId::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid opportunity id: {error}"),
        }
    }

    fn price() -> PriceConsensus {
        PriceConsensus {
            asset_symbol: "TST".to_owned(),
            price_per_unit: Money::from_micros(20_000_000),
            confidence: bps(9_000),
            oldest_observed_at_unix_ms: 980_000,
            valid_until_unix_ms: 1_060_000,
            source_count: 3,
            evidence: vec![evidence("price:quorum")],
        }
    }

    fn network() -> NetworkConsensus {
        NetworkConsensus {
            asset_symbol: "TST".to_owned(),
            algorithm: "sha256".to_owned(),
            network_hashrate_units: 1_000_000,
            network_emission_atoms: 5_000_000_000,
            horizon_seconds: 86_400,
            confidence: bps(8_500),
            oldest_observed_at_unix_ms: 990_000,
            valid_until_unix_ms: 1_050_000,
            source_count: 3,
            evidence: vec![evidence("network:quorum")],
        }
    }

    fn deployment() -> MiningDeploymentProfile {
        match MiningDeploymentProfile::new(
            id("tst:miner-a"),
            "TST",
            "sha256",
            100_000_000,
            10_000,
            1_200,
            bps(100),
            Money::from_micros(10_000),
            Money::ZERO,
            Money::ZERO,
            Money::from_micros(100_000),
            Money::ZERO,
            Money::from_micros(5_000_000),
            bps(700),
            995_000,
            1_070_000,
            bps(9_500),
            vec![evidence("hardware:benchmark")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid deployment: {error}"),
        }
    }

    fn electricity() -> ElectricityObservation {
        match ElectricityObservation::new(
            "meter-contract",
            Money::from_micros(150_000),
            985_000,
            1_040_000,
            bps(8_000),
            vec![evidence("energy:meter")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid electricity observation: {error}"),
        }
    }

    #[test]
    fn builds_snapshot_from_consensus_and_local_measurements() {
        let snapshot =
            match build_consensus_snapshot(&price(), &network(), &deployment(), &electricity()) {
                Ok(value) => value,
                Err(error) => unreachable!("valid snapshot: {error}"),
            };

        assert_eq!(
            snapshot.asset_price_per_unit,
            Money::from_micros(20_000_000)
        );
        assert_eq!(snapshot.network_hashrate_units, 1_000_000);
        assert_eq!(snapshot.network_emission_atoms, 5_000_000_000);
        assert_eq!(
            snapshot.electricity_price_per_kwh,
            Money::from_micros(150_000)
        );
        assert_eq!(snapshot.confidence, bps(8_000));
        assert_eq!(snapshot.observed_at_unix_ms, 980_000);
        assert_eq!(snapshot.valid_until_unix_ms, 1_040_000);
        assert_eq!(snapshot.evidence.len(), 4);
    }

    #[test]
    fn rejects_asset_mismatch_before_economic_use() {
        let mut network = network();
        network.asset_symbol = "OTHER".to_owned();
        let result = build_consensus_snapshot(&price(), &network, &deployment(), &electricity());
        assert_eq!(result, Err(SnapshotBuildError::AssetMismatch));
    }

    #[test]
    fn rejects_algorithm_mismatch_before_economic_use() {
        let mut network = network();
        network.algorithm = "other".to_owned();
        let result = build_consensus_snapshot(&price(), &network, &deployment(), &electricity());
        assert_eq!(result, Err(SnapshotBuildError::AlgorithmMismatch));
    }

    #[test]
    fn rejects_inputs_without_common_validity_window() {
        let mut electricity = electricity();
        electricity.valid_until_unix_ms = 970_000;
        let result = build_consensus_snapshot(&price(), &network(), &deployment(), &electricity);
        assert_eq!(result, Err(SnapshotBuildError::NoCommonValidityWindow));
    }

    #[test]
    fn impossible_local_hashrate_is_rejected_by_market_model() {
        let mut deployment = deployment();
        deployment.miner_hashrate_units = 2_000_000;
        let result = build_consensus_snapshot(&price(), &network(), &deployment, &electricity());
        assert!(matches!(
            result,
            Err(SnapshotBuildError::Market(
                MarketError::MinerHashrateExceedsNetwork
            ))
        ));
    }
}
