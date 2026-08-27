#![forbid(unsafe_code)]

use core::fmt;
use replikan_core::{BasisPoints, Money};
use replikan_economics::OperatingCosts;
use replikan_opportunities::{
    EvidenceRef, OpportunityId, OpportunityKind, OpportunityQuote, OpportunitySource, QuoteError,
};

const WATT_SECONDS_PER_KWH: i128 = 3_600_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MiningObservation {
    pub id: OpportunityId,
    pub asset_symbol: String,
    pub algorithm: String,
    pub observed_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub horizon_seconds: u64,
    pub gross_reward_quote: Money,
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

impl MiningObservation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: OpportunityId,
        asset_symbol: impl Into<String>,
        algorithm: impl Into<String>,
        observed_at_unix_ms: u64,
        valid_until_unix_ms: u64,
        horizon_seconds: u64,
        gross_reward_quote: Money,
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
    ) -> Result<Self, MiningError> {
        let asset_symbol = asset_symbol.into();
        let algorithm = algorithm.into();

        if asset_symbol.trim().is_empty() {
            return Err(MiningError::EmptyAssetSymbol);
        }
        if algorithm.trim().is_empty() {
            return Err(MiningError::EmptyAlgorithm);
        }
        if valid_until_unix_ms <= observed_at_unix_ms {
            return Err(MiningError::InvalidValidityWindow);
        }
        if horizon_seconds == 0 {
            return Err(MiningError::ZeroHorizon);
        }
        if power_watts == 0 {
            return Err(MiningError::ZeroPower);
        }
        if evidence.is_empty() {
            return Err(MiningError::MissingEvidence);
        }
        if gross_reward_quote.is_negative() {
            return Err(MiningError::NegativeGrossReward);
        }
        if electricity_price_per_kwh.is_negative() {
            return Err(MiningError::NegativeElectricityPrice);
        }
        if onchain_fee.is_negative()
            || compute_cost.is_negative()
            || infrastructure_cost.is_negative()
            || depreciation_cost.is_negative()
            || other_cost.is_negative()
        {
            return Err(MiningError::NegativeCost);
        }
        if capital_required.is_negative() {
            return Err(MiningError::NegativeCapitalRequired);
        }

        Ok(Self {
            id,
            asset_symbol,
            algorithm,
            observed_at_unix_ms,
            valid_until_unix_ms,
            horizon_seconds,
            gross_reward_quote,
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

    pub fn cost_breakdown(&self) -> Result<MiningCostBreakdown, MiningError> {
        let energy = energy_cost(
            self.power_watts,
            self.horizon_seconds,
            self.electricity_price_per_kwh,
        )?;
        let pool_fee = scale_money_ceil(self.gross_reward_quote, self.pool_fee)?;
        let network_fees = pool_fee
            .checked_add(self.onchain_fee)
            .ok_or(MiningError::ArithmeticOverflow)?;

        Ok(MiningCostBreakdown {
            energy,
            pool_fee,
            onchain_fee: self.onchain_fee,
            network_fees,
            compute: self.compute_cost,
            infrastructure: self.infrastructure_cost,
            depreciation: self.depreciation_cost,
            other: self.other_cost,
        })
    }

    pub fn to_quote(&self, source_id: &str) -> Result<OpportunityQuote, MiningError> {
        if source_id.trim().is_empty() {
            return Err(MiningError::EmptySourceId);
        }

        let breakdown = self.cost_breakdown()?;
        OpportunityQuote::new(
            self.id.clone(),
            OpportunityKind::Mining,
            source_id,
            self.observed_at_unix_ms,
            self.valid_until_unix_ms,
            self.horizon_seconds,
            self.gross_reward_quote,
            breakdown.operating_costs(),
            self.capital_required,
            self.risk,
            self.confidence,
            self.evidence.clone(),
        )
        .map_err(MiningError::Quote)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MiningCostBreakdown {
    pub energy: Money,
    pub pool_fee: Money,
    pub onchain_fee: Money,
    pub network_fees: Money,
    pub compute: Money,
    pub infrastructure: Money,
    pub depreciation: Money,
    pub other: Money,
}

impl MiningCostBreakdown {
    #[must_use]
    pub const fn operating_costs(self) -> OperatingCosts {
        OperatingCosts {
            energy: self.energy,
            compute: self.compute,
            network_fees: self.network_fees,
            infrastructure: self.infrastructure,
            depreciation: self.depreciation,
            other: self.other,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MiningOpportunitySource {
    source_id: String,
    observations: Vec<MiningObservation>,
}

impl MiningOpportunitySource {
    pub fn new(
        source_id: impl Into<String>,
        observations: Vec<MiningObservation>,
    ) -> Result<Self, MiningError> {
        let source_id = source_id.into();
        if source_id.trim().is_empty() {
            return Err(MiningError::EmptySourceId);
        }
        Ok(Self {
            source_id,
            observations,
        })
    }

    #[must_use]
    pub fn observations(&self) -> &[MiningObservation] {
        &self.observations
    }
}

impl OpportunitySource for MiningOpportunitySource {
    type Error = MiningError;

    fn source_id(&self) -> &str {
        &self.source_id
    }

    fn discover(&self, _observed_at_unix_ms: u64) -> Result<Vec<OpportunityQuote>, Self::Error> {
        self.observations
            .iter()
            .map(|observation| observation.to_quote(&self.source_id))
            .collect()
    }
}

fn energy_cost(
    power_watts: u64,
    horizon_seconds: u64,
    electricity_price_per_kwh: Money,
) -> Result<Money, MiningError> {
    let watt_seconds = i128::from(power_watts)
        .checked_mul(i128::from(horizon_seconds))
        .ok_or(MiningError::ArithmeticOverflow)?;
    let numerator = watt_seconds
        .checked_mul(electricity_price_per_kwh.micros())
        .ok_or(MiningError::ArithmeticOverflow)?;
    let micros = ceil_div_nonnegative(numerator, WATT_SECONDS_PER_KWH)?;
    Ok(Money::from_micros(micros))
}

fn scale_money_ceil(value: Money, ratio: BasisPoints) -> Result<Money, MiningError> {
    let numerator = value
        .micros()
        .checked_mul(i128::from(ratio.value()))
        .ok_or(MiningError::ArithmeticOverflow)?;
    let micros = ceil_div_nonnegative(numerator, i128::from(BasisPoints::FULL_SCALE))?;
    Ok(Money::from_micros(micros))
}

fn ceil_div_nonnegative(numerator: i128, denominator: i128) -> Result<i128, MiningError> {
    if numerator < 0 || denominator <= 0 {
        return Err(MiningError::ArithmeticDomain);
    }
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    if remainder == 0 {
        Ok(quotient)
    } else {
        quotient
            .checked_add(1)
            .ok_or(MiningError::ArithmeticOverflow)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MiningError {
    EmptyAssetSymbol,
    EmptyAlgorithm,
    EmptySourceId,
    InvalidValidityWindow,
    ZeroHorizon,
    ZeroPower,
    MissingEvidence,
    NegativeGrossReward,
    NegativeElectricityPrice,
    NegativeCost,
    NegativeCapitalRequired,
    ArithmeticDomain,
    ArithmeticOverflow,
    Quote(QuoteError),
}

impl fmt::Display for MiningError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyAssetSymbol => write!(f, "mining asset symbol cannot be empty"),
            Self::EmptyAlgorithm => write!(f, "mining algorithm cannot be empty"),
            Self::EmptySourceId => write!(f, "mining source id cannot be empty"),
            Self::InvalidValidityWindow => write!(f, "mining observation validity window is invalid"),
            Self::ZeroHorizon => write!(f, "mining observation horizon must be greater than zero"),
            Self::ZeroPower => write!(f, "mining power draw must be greater than zero"),
            Self::MissingEvidence => write!(f, "mining observations require evidence"),
            Self::NegativeGrossReward => write!(f, "gross mining reward cannot be negative"),
            Self::NegativeElectricityPrice => write!(f, "electricity price cannot be negative"),
            Self::NegativeCost => write!(f, "mining costs cannot be negative"),
            Self::NegativeCapitalRequired => write!(f, "required mining capital cannot be negative"),
            Self::ArithmeticDomain => write!(f, "invalid arithmetic domain for mining cost calculation"),
            Self::ArithmeticOverflow => write!(f, "mining cost arithmetic overflow"),
            Self::Quote(error) => write!(f, "invalid mining opportunity quote: {error}"),
        }
    }
}

impl std::error::Error for MiningError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn bps(value: u32) -> BasisPoints {
        match BasisPoints::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid test basis points: {error}"),
        }
    }

    fn id(value: &str) -> OpportunityId {
        match OpportunityId::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid test id: {error}"),
        }
    }

    fn evidence(value: &str) -> EvidenceRef {
        match EvidenceRef::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid test evidence: {error}"),
        }
    }

    fn observation() -> MiningObservation {
        match MiningObservation::new(
            id("miner-a"),
            "TEST",
            "test-hash",
            1_000_000,
            1_060_000,
            3_600,
            Money::from_micros(10_000_000),
            1_000,
            Money::from_micros(200_000),
            bps(200),
            Money::from_micros(50_000),
            Money::from_micros(25_000),
            Money::from_micros(30_000),
            Money::from_micros(100_000),
            Money::from_micros(10_000),
            Money::from_micros(20_000_000),
            bps(1_000),
            bps(9_000),
            vec![evidence("pool-api:sample"), evidence("meter:sample")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid mining observation: {error}"),
        }
    }

    #[test]
    fn one_kwh_energy_cost_is_exact() {
        let cost = match energy_cost(1_000, 3_600, Money::from_micros(200_000)) {
            Ok(value) => value,
            Err(error) => unreachable!("valid energy calculation: {error}"),
        };
        assert_eq!(cost, Money::from_micros(200_000));
    }

    #[test]
    fn fractional_energy_cost_rounds_up_conservatively() {
        let cost = match energy_cost(1, 1, Money::from_micros(1)) {
            Ok(value) => value,
            Err(error) => unreachable!("valid energy calculation: {error}"),
        };
        assert_eq!(cost, Money::from_micros(1));
    }

    #[test]
    fn quote_accounts_for_energy_pool_fee_and_other_costs() {
        let observation = observation();
        let breakdown = match observation.cost_breakdown() {
            Ok(value) => value,
            Err(error) => unreachable!("valid cost breakdown: {error}"),
        };

        assert_eq!(breakdown.energy, Money::from_micros(200_000));
        assert_eq!(breakdown.pool_fee, Money::from_micros(200_000));
        assert_eq!(breakdown.network_fees, Money::from_micros(250_000));

        let quote = match observation.to_quote("mining:test") {
            Ok(value) => value,
            Err(error) => unreachable!("valid mining quote: {error}"),
        };
        assert_eq!(quote.expected_revenue, Money::from_micros(10_000_000));
        assert_eq!(quote.expected_costs.energy, Money::from_micros(200_000));
        assert_eq!(quote.expected_costs.network_fees, Money::from_micros(250_000));
        assert_eq!(quote.expected_costs.depreciation, Money::from_micros(100_000));
    }

    #[test]
    fn source_exposes_quotes_without_custody_capability() {
        let source = match MiningOpportunitySource::new("mining:test", vec![observation()]) {
            Ok(value) => value,
            Err(error) => unreachable!("valid mining source: {error}"),
        };
        let quotes = match source.discover(1_000_000) {
            Ok(value) => value,
            Err(error) => unreachable!("valid mining discovery: {error}"),
        };

        assert_eq!(source.source_id(), "mining:test");
        assert_eq!(quotes.len(), 1);
        assert_eq!(quotes[0].kind, OpportunityKind::Mining);
    }

    #[test]
    fn observation_requires_evidence() {
        let result = MiningObservation::new(
            id("no-evidence"),
            "TEST",
            "test-hash",
            1,
            2,
            60,
            Money::from_micros(1_000_000),
            100,
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
            vec![],
        );
        assert_eq!(result, Err(MiningError::MissingEvidence));
    }
}
