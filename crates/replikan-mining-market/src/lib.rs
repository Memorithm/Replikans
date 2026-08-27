#![forbid(unsafe_code)]

pub mod price_consensus;

use core::fmt;
use replikan_core::{BasisPoints, Money};
use replikan_mining::{MiningError, MiningObservation};
use replikan_opportunities::{EvidenceRef, OpportunityId, OpportunityQuote, OpportunitySource};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MiningMarketSnapshot {
    pub id: OpportunityId,
    pub asset_symbol: String,
    pub algorithm: String,
    pub observed_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub horizon_seconds: u64,
    pub asset_price_per_unit: Money,
    pub asset_atoms_per_unit: u128,
    pub network_emission_atoms: u128,
    pub miner_hashrate_units: u128,
    pub network_hashrate_units: u128,
    pub power_watts: u64,
    pub electricity_price_per_kwh: Money,
    pub pool_fee: BasisPoints,
    pub onchain_fee: Money,
    pub compute_cost: Money,
    pub infrastructure_cost: Money,
    pub depreciation_cost: Money,
    pub other_cost: Money,
    pub capital_required: Money,
    pub risk: BasisPoints,
    pub confidence: BasisPoints,
    pub evidence: Vec<EvidenceRef>,
}

impl MiningMarketSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: OpportunityId,
        asset_symbol: impl Into<String>,
        algorithm: impl Into<String>,
        observed_at_unix_ms: u64,
        valid_until_unix_ms: u64,
        horizon_seconds: u64,
        asset_price_per_unit: Money,
        asset_atoms_per_unit: u128,
        network_emission_atoms: u128,
        miner_hashrate_units: u128,
        network_hashrate_units: u128,
        power_watts: u64,
        electricity_price_per_kwh: Money,
        pool_fee: BasisPoints,
        onchain_fee: Money,
        compute_cost: Money,
        infrastructure_cost: Money,
        depreciation_cost: Money,
        other_cost: Money,
        capital_required: Money,
        risk: BasisPoints,
        confidence: BasisPoints,
        evidence: Vec<EvidenceRef>,
    ) -> Result<Self, MarketError> {
        let asset_symbol = asset_symbol.into();
        let algorithm = algorithm.into();

        if asset_symbol.trim().is_empty() {
            return Err(MarketError::EmptyAssetSymbol);
        }
        if algorithm.trim().is_empty() {
            return Err(MarketError::EmptyAlgorithm);
        }
        if valid_until_unix_ms <= observed_at_unix_ms {
            return Err(MarketError::InvalidValidityWindow);
        }
        if horizon_seconds == 0 {
            return Err(MarketError::ZeroHorizon);
        }
        if asset_price_per_unit.is_negative() {
            return Err(MarketError::NegativeAssetPrice);
        }
        if asset_atoms_per_unit == 0 {
            return Err(MarketError::ZeroAssetScale);
        }
        if miner_hashrate_units == 0 {
            return Err(MarketError::ZeroMinerHashrate);
        }
        if network_hashrate_units == 0 {
            return Err(MarketError::ZeroNetworkHashrate);
        }
        if miner_hashrate_units > network_hashrate_units {
            return Err(MarketError::MinerHashrateExceedsNetwork);
        }
        if evidence.is_empty() {
            return Err(MarketError::MissingEvidence);
        }

        Ok(Self {
            id,
            asset_symbol,
            algorithm,
            observed_at_unix_ms,
            valid_until_unix_ms,
            horizon_seconds,
            asset_price_per_unit,
            asset_atoms_per_unit,
            network_emission_atoms,
            miner_hashrate_units,
            network_hashrate_units,
            power_watts,
            electricity_price_per_kwh,
            pool_fee,
            onchain_fee,
            compute_cost,
            infrastructure_cost,
            depreciation_cost,
            other_cost,
            capital_required,
            risk,
            confidence,
            evidence,
        })
    }

    pub fn expected_miner_reward_atoms(&self) -> Result<u128, MarketError> {
        self.network_emission_atoms
            .checked_mul(self.miner_hashrate_units)
            .ok_or(MarketError::ArithmeticOverflow)
            .map(|numerator| numerator / self.network_hashrate_units)
    }

    pub fn expected_gross_reward_quote(&self) -> Result<Money, MarketError> {
        let reward_atoms = self.expected_miner_reward_atoms()?;
        let price_micros = u128::try_from(self.asset_price_per_unit.micros())
            .map_err(|_| MarketError::ArithmeticDomain)?;
        let gross_micros = reward_atoms
            .checked_mul(price_micros)
            .ok_or(MarketError::ArithmeticOverflow)?
            / self.asset_atoms_per_unit;
        let gross_micros =
            i128::try_from(gross_micros).map_err(|_| MarketError::ArithmeticOverflow)?;
        Ok(Money::from_micros(gross_micros))
    }

    pub fn to_observation(&self) -> Result<MiningObservation, MarketError> {
        MiningObservation::new(
            self.id.clone(),
            self.asset_symbol.clone(),
            self.algorithm.clone(),
            self.observed_at_unix_ms,
            self.valid_until_unix_ms,
            self.horizon_seconds,
            self.expected_gross_reward_quote()?,
            self.power_watts,
            self.electricity_price_per_kwh,
            self.pool_fee,
            self.onchain_fee,
            self.compute_cost,
            self.infrastructure_cost,
            self.depreciation_cost,
            self.other_cost,
            self.capital_required,
            self.risk,
            self.confidence,
            self.evidence.clone(),
        )
        .map_err(MarketError::Mining)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketMiningOpportunitySource {
    source_id: String,
    snapshots: Vec<MiningMarketSnapshot>,
}

impl MarketMiningOpportunitySource {
    pub fn new(
        source_id: impl Into<String>,
        snapshots: Vec<MiningMarketSnapshot>,
    ) -> Result<Self, MarketError> {
        let source_id = source_id.into();
        if source_id.trim().is_empty() {
            return Err(MarketError::EmptySourceId);
        }
        Ok(Self {
            source_id,
            snapshots,
        })
    }

    #[must_use]
    pub fn snapshots(&self) -> &[MiningMarketSnapshot] {
        &self.snapshots
    }
}

impl OpportunitySource for MarketMiningOpportunitySource {
    type Error = MarketError;

    fn source_id(&self) -> &str {
        &self.source_id
    }

    fn discover(&self, _observed_at_unix_ms: u64) -> Result<Vec<OpportunityQuote>, Self::Error> {
        self.snapshots
            .iter()
            .map(|snapshot| {
                snapshot
                    .to_observation()?
                    .to_quote(&self.source_id)
                    .map_err(MarketError::Mining)
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketError {
    EmptyAssetSymbol,
    EmptyAlgorithm,
    EmptySourceId,
    InvalidValidityWindow,
    ZeroHorizon,
    NegativeAssetPrice,
    ZeroAssetScale,
    ZeroMinerHashrate,
    ZeroNetworkHashrate,
    MinerHashrateExceedsNetwork,
    MissingEvidence,
    ArithmeticDomain,
    ArithmeticOverflow,
    Mining(MiningError),
}

impl fmt::Display for MarketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyAssetSymbol => write!(f, "market asset symbol cannot be empty"),
            Self::EmptyAlgorithm => write!(f, "market mining algorithm cannot be empty"),
            Self::EmptySourceId => write!(f, "market mining source id cannot be empty"),
            Self::InvalidValidityWindow => {
                write!(f, "market snapshot validity window is invalid")
            }
            Self::ZeroHorizon => {
                write!(f, "market snapshot horizon must be greater than zero")
            }
            Self::NegativeAssetPrice => write!(f, "asset price cannot be negative"),
            Self::ZeroAssetScale => {
                write!(f, "asset atoms-per-unit scale must be greater than zero")
            }
            Self::ZeroMinerHashrate => write!(f, "miner hashrate must be greater than zero"),
            Self::ZeroNetworkHashrate => write!(f, "network hashrate must be greater than zero"),
            Self::MinerHashrateExceedsNetwork => {
                write!(f, "miner hashrate cannot exceed network hashrate")
            }
            Self::MissingEvidence => write!(f, "market snapshot requires evidence"),
            Self::ArithmeticDomain => write!(f, "invalid arithmetic domain in market model"),
            Self::ArithmeticOverflow => write!(f, "market model arithmetic overflow"),
            Self::Mining(error) => write!(f, "mining observation conversion failed: {error}"),
        }
    }
}

impl std::error::Error for MarketError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn bps(value: u32) -> BasisPoints {
        match BasisPoints::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid basis points: {error}"),
        }
    }

    fn id(value: &str) -> OpportunityId {
        match OpportunityId::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid opportunity id: {error}"),
        }
    }

    fn evidence(value: &str) -> EvidenceRef {
        match EvidenceRef::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid evidence: {error}"),
        }
    }

    fn snapshot(miner_hashrate: u128, network_hashrate: u128) -> MiningMarketSnapshot {
        match MiningMarketSnapshot::new(
            id("sha256:test"),
            "TST",
            "sha256",
            1_000_000,
            1_060_000,
            86_400,
            Money::from_micros(2_000_000_000),
            100_000_000,
            5_000_000_000,
            miner_hashrate,
            network_hashrate,
            1_000,
            Money::from_micros(100_000),
            bps(100),
            Money::from_micros(50_000),
            Money::ZERO,
            Money::ZERO,
            Money::from_micros(100_000),
            Money::ZERO,
            Money::from_micros(5_000_000),
            bps(500),
            bps(9_000),
            vec![
                evidence("market:price:1"),
                evidence("network:hashrate:1"),
                evidence("network:emission:1"),
            ],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid market snapshot: {error}"),
        }
    }

    #[test]
    fn derives_gross_reward_from_network_share_and_asset_price() {
        let snapshot = snapshot(1, 1_000);
        assert_eq!(snapshot.expected_miner_reward_atoms(), Ok(5_000_000));
        assert_eq!(
            snapshot.expected_gross_reward_quote(),
            Ok(Money::from_micros(100_000_000))
        );
    }

    #[test]
    fn revenue_rounds_down_conservatively() {
        let mut snapshot = snapshot(1, 3);
        snapshot.network_emission_atoms = 1;
        assert_eq!(snapshot.expected_miner_reward_atoms(), Ok(0));
        assert_eq!(snapshot.expected_gross_reward_quote(), Ok(Money::ZERO));
    }

    #[test]
    fn rejects_impossible_miner_share() {
        let result = MiningMarketSnapshot::new(
            id("invalid-share"),
            "TST",
            "sha256",
            1_000_000,
            1_060_000,
            86_400,
            Money::from_micros(1_000_000),
            100_000_000,
            100_000_000,
            2,
            1,
            1_000,
            Money::from_micros(100_000),
            bps(100),
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            bps(500),
            bps(9_000),
            vec![evidence("test:evidence")],
        );
        assert_eq!(result, Err(MarketError::MinerHashrateExceedsNetwork));
    }

    #[test]
    fn converts_market_snapshot_into_full_cost_mining_quote() {
        let snapshot = snapshot(1, 1_000);
        let observation = match snapshot.to_observation() {
            Ok(value) => value,
            Err(error) => unreachable!("valid mining observation: {error}"),
        };
        let quote = match observation.to_quote("market-model") {
            Ok(value) => value,
            Err(error) => unreachable!("valid mining quote: {error}"),
        };

        assert_eq!(quote.expected_revenue, Money::from_micros(100_000_000));
        assert!(quote.expected_costs.energy.is_positive());
        assert!(quote.expected_costs.network_fees.is_positive());
        assert_eq!(quote.source, "market-model");
    }

    #[test]
    fn source_exposes_quotes_without_any_custody_capability() {
        let source =
            match MarketMiningOpportunitySource::new("market-model", vec![snapshot(1, 1_000)]) {
                Ok(value) => value,
                Err(error) => unreachable!("valid source: {error}"),
            };
        let quotes = match source.discover(1_000_000) {
            Ok(value) => value,
            Err(error) => unreachable!("valid source discovery: {error}"),
        };

        assert_eq!(quotes.len(), 1);
        assert_eq!(quotes[0].source, "market-model");
    }
}
