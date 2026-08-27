#![forbid(unsafe_code)]

use core::cmp::Ordering;
use core::fmt;
use replikan_core::{BasisPoints, Money};
use replikan_economics::{
    EconomicFitness, OperatingCosts, OpportunityDecision as EconomicDecision, OpportunityEstimate,
    OpportunityPolicy, OpportunityRejection as EconomicRejection, evaluate_opportunity,
};

const SECONDS_PER_DAY: i128 = 86_400;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OpportunityId(String);

impl OpportunityId {
    pub fn new(value: impl Into<String>) -> Result<Self, QuoteError> {
        let value = value.into();
        if value.trim().is_empty() {
            Err(QuoteError::EmptyOpportunityId)
        } else {
            Ok(Self(value))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpportunityKind {
    Mining,
    ComputeMarketplace,
    Validator,
    ProtocolReward,
    Bounty,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceRef(String);

impl EvidenceRef {
    pub fn new(value: impl Into<String>) -> Result<Self, QuoteError> {
        let value = value.into();
        if value.trim().is_empty() {
            Err(QuoteError::EmptyEvidence)
        } else {
            Ok(Self(value))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpportunityQuote {
    pub id: OpportunityId,
    pub kind: OpportunityKind,
    pub source: String,
    pub observed_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub horizon_seconds: u64,
    pub expected_revenue: Money,
    pub expected_costs: OperatingCosts,
    pub capital_required: Money,
    pub risk: BasisPoints,
    pub confidence: BasisPoints,
    pub evidence: Vec<EvidenceRef>,
}

impl OpportunityQuote {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: OpportunityId,
        kind: OpportunityKind,
        source: impl Into<String>,
        observed_at_unix_ms: u64,
        valid_until_unix_ms: u64,
        horizon_seconds: u64,
        expected_revenue: Money,
        expected_costs: OperatingCosts,
        capital_required: Money,
        risk: BasisPoints,
        confidence: BasisPoints,
        evidence: Vec<EvidenceRef>,
    ) -> Result<Self, QuoteError> {
        let source = source.into();
        if source.trim().is_empty() {
            return Err(QuoteError::EmptySource);
        }
        if valid_until_unix_ms <= observed_at_unix_ms {
            return Err(QuoteError::InvalidValidityWindow);
        }
        if horizon_seconds == 0 {
            return Err(QuoteError::ZeroHorizon);
        }
        if expected_revenue.is_negative() {
            return Err(QuoteError::NegativeExpectedRevenue);
        }
        if capital_required.is_negative() {
            return Err(QuoteError::NegativeCapitalRequired);
        }
        if has_negative_cost(expected_costs) {
            return Err(QuoteError::NegativeExpectedCost);
        }

        Ok(Self {
            id,
            kind,
            source,
            observed_at_unix_ms,
            valid_until_unix_ms,
            horizon_seconds,
            expected_revenue,
            expected_costs,
            capital_required,
            risk,
            confidence,
            evidence,
        })
    }

    pub fn checked_expected_cost(&self) -> Result<Money, EngineError> {
        checked_cost_total(self.expected_costs)
    }

    pub fn checked_expected_net_profit(&self) -> Result<Money, EngineError> {
        self.expected_revenue
            .checked_sub(self.checked_expected_cost()?)
            .ok_or(EngineError::MonetaryOverflow)
    }
}

fn has_negative_cost(costs: OperatingCosts) -> bool {
    costs.energy.is_negative()
        || costs.compute.is_negative()
        || costs.network_fees.is_negative()
        || costs.infrastructure.is_negative()
        || costs.depreciation.is_negative()
        || costs.other.is_negative()
}

fn checked_cost_total(costs: OperatingCosts) -> Result<Money, EngineError> {
    let mut total = Money::ZERO;
    for cost in [
        costs.energy,
        costs.compute,
        costs.network_fees,
        costs.infrastructure,
        costs.depreciation,
        costs.other,
    ] {
        total = total
            .checked_add(cost)
            .ok_or(EngineError::MonetaryOverflow)?;
    }
    Ok(total)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectionPolicy {
    pub economics: OpportunityPolicy,
    pub minimum_confidence: BasisPoints,
    pub maximum_quote_age_ms: u64,
    pub minimum_evidence_count: usize,
    /// Opportunity-cost charge applied to capital committed for the quote horizon.
    pub capital_charge: BasisPoints,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RejectionReason {
    ObservationFromFuture,
    Expired,
    Stale,
    InsufficientEvidence,
    ConfidenceTooLow,
    Economic(EconomicRejection),
    NonPositiveAfterCapitalCharge,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RejectedOpportunity {
    pub id: OpportunityId,
    pub reason: RejectionReason,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RankedOpportunity {
    pub quote: OpportunityQuote,
    /// Risk-, confidence-, capital- and horizon-adjusted deterministic score.
    /// The unit is quote-currency micros per normalized day.
    pub score_micros_per_day: i128,
}

impl RankedOpportunity {
    #[must_use]
    pub const fn score_micros_per_day(&self) -> i128 {
        self.score_micros_per_day
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SelectionReport {
    pub accepted: Vec<RankedOpportunity>,
    pub rejected: Vec<RejectedOpportunity>,
}

impl SelectionReport {
    #[must_use]
    pub fn best(&self) -> Option<&RankedOpportunity> {
        self.accepted.first()
    }
}

pub fn evaluate_and_rank(
    fitness: EconomicFitness,
    quotes: impl IntoIterator<Item = OpportunityQuote>,
    policy: SelectionPolicy,
    now_unix_ms: u64,
) -> Result<SelectionReport, EngineError> {
    let mut report = SelectionReport::default();

    for quote in quotes {
        match evaluate_quote(fitness, &quote, policy, now_unix_ms)? {
            QuoteDecision::Accept(score_micros_per_day) => {
                report.accepted.push(RankedOpportunity {
                    quote,
                    score_micros_per_day,
                });
            }
            QuoteDecision::Reject(reason) => {
                report.rejected.push(RejectedOpportunity {
                    id: quote.id,
                    reason,
                });
            }
        }
    }

    report.accepted.sort_by(compare_ranked);
    Ok(report)
}

fn compare_ranked(left: &RankedOpportunity, right: &RankedOpportunity) -> Ordering {
    right
        .score_micros_per_day
        .cmp(&left.score_micros_per_day)
        .then_with(|| {
            left.quote
                .capital_required
                .cmp(&right.quote.capital_required)
        })
        .then_with(|| left.quote.id.cmp(&right.quote.id))
}

enum QuoteDecision {
    Accept(i128),
    Reject(RejectionReason),
}

fn evaluate_quote(
    fitness: EconomicFitness,
    quote: &OpportunityQuote,
    policy: SelectionPolicy,
    now_unix_ms: u64,
) -> Result<QuoteDecision, EngineError> {
    if quote.observed_at_unix_ms > now_unix_ms {
        return Ok(QuoteDecision::Reject(
            RejectionReason::ObservationFromFuture,
        ));
    }
    if now_unix_ms > quote.valid_until_unix_ms {
        return Ok(QuoteDecision::Reject(RejectionReason::Expired));
    }
    if now_unix_ms.saturating_sub(quote.observed_at_unix_ms) > policy.maximum_quote_age_ms {
        return Ok(QuoteDecision::Reject(RejectionReason::Stale));
    }
    if quote.evidence.len() < policy.minimum_evidence_count {
        return Ok(QuoteDecision::Reject(RejectionReason::InsufficientEvidence));
    }
    if quote.confidence < policy.minimum_confidence {
        return Ok(QuoteDecision::Reject(RejectionReason::ConfidenceTooLow));
    }

    let expected_cost = quote.checked_expected_cost()?;
    let estimate = OpportunityEstimate {
        expected_revenue: quote.expected_revenue,
        expected_cost,
        capital_required: quote.capital_required,
        risk: quote.risk,
    };

    if let EconomicDecision::Reject(reason) =
        evaluate_opportunity(fitness, estimate, policy.economics)
    {
        return Ok(QuoteDecision::Reject(RejectionReason::Economic(reason)));
    }

    let net = quote
        .expected_revenue
        .checked_sub(expected_cost)
        .ok_or(EngineError::MonetaryOverflow)?;
    let capital_charge = scale_by_bps(quote.capital_required, policy.capital_charge)?;
    let after_capital_charge = net
        .checked_sub(capital_charge)
        .ok_or(EngineError::MonetaryOverflow)?;
    if !after_capital_charge.is_positive() {
        return Ok(QuoteDecision::Reject(
            RejectionReason::NonPositiveAfterCapitalCharge,
        ));
    }

    let confidence_adjusted = scale_by_bps(after_capital_charge, quote.confidence)?;
    let survival_probability = BasisPoints::FULL_SCALE
        .checked_sub(quote.risk.value())
        .ok_or(EngineError::ArithmeticOverflow)?;
    let conservative = scale_by_raw_bps(confidence_adjusted, survival_probability)?;
    let normalized = conservative
        .micros()
        .checked_mul(SECONDS_PER_DAY)
        .ok_or(EngineError::ArithmeticOverflow)?
        .checked_div(i128::from(quote.horizon_seconds))
        .ok_or(EngineError::ArithmeticOverflow)?;

    Ok(QuoteDecision::Accept(normalized))
}

fn scale_by_bps(value: Money, ratio: BasisPoints) -> Result<Money, EngineError> {
    scale_by_raw_bps(value, ratio.value())
}

fn scale_by_raw_bps(value: Money, basis_points: u32) -> Result<Money, EngineError> {
    let scaled = value
        .micros()
        .checked_mul(i128::from(basis_points))
        .ok_or(EngineError::ArithmeticOverflow)?
        .checked_div(i128::from(BasisPoints::FULL_SCALE))
        .ok_or(EngineError::ArithmeticOverflow)?;
    Ok(Money::from_micros(scaled))
}

/// Runtime-agnostic discovery boundary. Networked adapters can implement this trait
/// without giving the economic core custody or transaction-signing capabilities.
pub trait OpportunitySource {
    type Error;

    fn source_id(&self) -> &str;
    fn discover(&self, observed_at_unix_ms: u64) -> Result<Vec<OpportunityQuote>, Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuoteError {
    EmptyOpportunityId,
    EmptySource,
    EmptyEvidence,
    InvalidValidityWindow,
    ZeroHorizon,
    NegativeExpectedRevenue,
    NegativeExpectedCost,
    NegativeCapitalRequired,
}

impl fmt::Display for QuoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::EmptyOpportunityId => "opportunity id cannot be empty",
            Self::EmptySource => "opportunity source cannot be empty",
            Self::EmptyEvidence => "evidence reference cannot be empty",
            Self::InvalidValidityWindow => "valid-until must be later than observation time",
            Self::ZeroHorizon => "opportunity horizon must be greater than zero",
            Self::NegativeExpectedRevenue => "expected revenue cannot be negative",
            Self::NegativeExpectedCost => "expected costs cannot be negative",
            Self::NegativeCapitalRequired => "required capital cannot be negative",
        };
        write!(f, "{message}")
    }
}

impl std::error::Error for QuoteError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineError {
    MonetaryOverflow,
    ArithmeticOverflow,
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MonetaryOverflow => write!(f, "monetary arithmetic overflow"),
            Self::ArithmeticOverflow => write!(f, "opportunity score arithmetic overflow"),
        }
    }
}

impl std::error::Error for EngineError {}

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
            Err(error) => unreachable!("valid test opportunity id: {error}"),
        }
    }

    fn evidence(value: &str) -> EvidenceRef {
        match EvidenceRef::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid test evidence: {error}"),
        }
    }

    fn costs(total: i128) -> OperatingCosts {
        OperatingCosts {
            energy: Money::from_micros(total),
            compute: Money::ZERO,
            network_fees: Money::ZERO,
            infrastructure: Money::ZERO,
            depreciation: Money::ZERO,
            other: Money::ZERO,
        }
    }

    fn fitness() -> EconomicFitness {
        EconomicFitness {
            realized_revenue: Money::from_micros(50_000_000),
            realized_costs: costs(10_000_000),
            liquid_capital: Money::from_micros(200_000_000),
            survival_reserve: Money::from_micros(50_000_000),
            drawdown: bps(500),
        }
    }

    fn policy() -> SelectionPolicy {
        SelectionPolicy {
            economics: OpportunityPolicy {
                max_risk: bps(3_000),
                minimum_net_profit: Money::from_micros(1_000_000),
                minimum_post_action_reserve: Money::from_micros(50_000_000),
            },
            minimum_confidence: bps(6_000),
            maximum_quote_age_ms: 60_000,
            minimum_evidence_count: 1,
            capital_charge: bps(100),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn quote(
        name: &str,
        observed: u64,
        valid_until: u64,
        horizon_seconds: u64,
        revenue: i128,
        cost: i128,
        capital: i128,
        risk: u32,
        confidence: u32,
    ) -> OpportunityQuote {
        match OpportunityQuote::new(
            id(name),
            OpportunityKind::Mining,
            "test-source",
            observed,
            valid_until,
            horizon_seconds,
            Money::from_micros(revenue),
            costs(cost),
            Money::from_micros(capital),
            bps(risk),
            bps(confidence),
            vec![evidence("test:observation")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid test quote: {error}"),
        }
    }

    #[test]
    fn ranks_net_value_not_gross_revenue() {
        let now = 1_000_000;
        let high_gross_low_margin = quote(
            "high-gross",
            now,
            now + 60_000,
            86_400,
            100_000_000,
            90_000_000,
            10_000_000,
            500,
            9_000,
        );
        let lower_gross_high_margin = quote(
            "high-margin",
            now,
            now + 60_000,
            86_400,
            60_000_000,
            20_000_000,
            10_000_000,
            500,
            9_000,
        );

        let report = match evaluate_and_rank(
            fitness(),
            [high_gross_low_margin, lower_gross_high_margin],
            policy(),
            now,
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid selection report: {error}"),
        };

        assert_eq!(
            report.best().map(|item| item.quote.id.as_str()),
            Some("high-margin")
        );
    }

    #[test]
    fn rejects_expired_and_low_confidence_quotes() {
        let now = 1_000_000;
        let expired = quote(
            "expired",
            now - 120_000,
            now - 1,
            86_400,
            50_000_000,
            10_000_000,
            10_000_000,
            500,
            9_000,
        );
        let low_confidence = quote(
            "low-confidence",
            now,
            now + 60_000,
            86_400,
            50_000_000,
            10_000_000,
            10_000_000,
            500,
            5_000,
        );

        let report = match evaluate_and_rank(fitness(), [expired, low_confidence], policy(), now) {
            Ok(value) => value,
            Err(error) => unreachable!("valid selection report: {error}"),
        };

        assert!(report.accepted.is_empty());
        assert_eq!(report.rejected.len(), 2);
        assert_eq!(report.rejected[0].reason, RejectionReason::Expired);
        assert_eq!(report.rejected[1].reason, RejectionReason::ConfidenceTooLow);
    }

    #[test]
    fn capital_charge_penalizes_capital_heavy_opportunities() {
        let now = 1_000_000;
        let light = quote(
            "capital-light",
            now,
            now + 60_000,
            86_400,
            50_000_000,
            20_000_000,
            10_000_000,
            500,
            9_000,
        );
        let heavy = quote(
            "capital-heavy",
            now,
            now + 60_000,
            86_400,
            50_000_000,
            20_000_000,
            100_000_000,
            500,
            9_000,
        );

        let report = match evaluate_and_rank(fitness(), [heavy, light], policy(), now) {
            Ok(value) => value,
            Err(error) => unreachable!("valid selection report: {error}"),
        };

        assert_eq!(
            report.best().map(|item| item.quote.id.as_str()),
            Some("capital-light")
        );
    }

    #[test]
    fn shorter_horizon_wins_when_conservative_profit_is_equal() {
        let now = 1_000_000;
        let daily = quote(
            "daily",
            now,
            now + 60_000,
            86_400,
            40_000_000,
            10_000_000,
            10_000_000,
            500,
            9_000,
        );
        let two_day = quote(
            "two-day",
            now,
            now + 60_000,
            172_800,
            40_000_000,
            10_000_000,
            10_000_000,
            500,
            9_000,
        );

        let report = match evaluate_and_rank(fitness(), [two_day, daily], policy(), now) {
            Ok(value) => value,
            Err(error) => unreachable!("valid selection report: {error}"),
        };

        assert_eq!(
            report.best().map(|item| item.quote.id.as_str()),
            Some("daily")
        );
    }

    #[test]
    fn quote_rejects_negative_costs_and_invalid_windows() {
        let invalid_cost = OpportunityQuote::new(
            id("negative-cost"),
            OpportunityKind::Other,
            "test",
            1,
            2,
            60,
            Money::from_micros(10),
            costs(-1),
            Money::ZERO,
            bps(0),
            bps(10_000),
            vec![evidence("test")],
        );
        assert_eq!(invalid_cost, Err(QuoteError::NegativeExpectedCost));

        let invalid_window = OpportunityQuote::new(
            id("invalid-window"),
            OpportunityKind::Other,
            "test",
            2,
            2,
            60,
            Money::from_micros(10),
            costs(0),
            Money::ZERO,
            bps(0),
            bps(10_000),
            vec![evidence("test")],
        );
        assert_eq!(invalid_window, Err(QuoteError::InvalidValidityWindow));
    }
}
