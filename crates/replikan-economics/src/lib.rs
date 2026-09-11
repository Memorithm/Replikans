#![forbid(unsafe_code)]

use replikan_core::{BasisPoints, Money};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OperatingCosts {
    pub energy: Money,
    pub compute: Money,
    pub network_fees: Money,
    pub infrastructure: Money,
    pub depreciation: Money,
    pub other: Money,
}

impl OperatingCosts {
    #[must_use]
    pub fn total(self) -> Money {
        match self.checked_total() {
            Some(value) => value,
            None => Money::from_micros(i128::MAX),
        }
    }

    #[must_use]
    pub fn checked_total(self) -> Option<Money> {
        self.energy
            .checked_add(self.compute)?
            .checked_add(self.network_fees)?
            .checked_add(self.infrastructure)?
            .checked_add(self.depreciation)?
            .checked_add(self.other)
    }

    #[must_use]
    pub fn has_negative_component(self) -> bool {
        self.energy.is_negative()
            || self.compute.is_negative()
            || self.network_fees.is_negative()
            || self.infrastructure.is_negative()
            || self.depreciation.is_negative()
            || self.other.is_negative()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EconomicFitness {
    pub realized_revenue: Money,
    pub realized_costs: OperatingCosts,
    pub liquid_capital: Money,
    pub survival_reserve: Money,
    pub drawdown: BasisPoints,
}

impl EconomicFitness {
    #[must_use]
    pub fn realized_net_profit(self) -> Money {
        match self.checked_realized_net_profit() {
            Some(value) => value,
            None => Money::from_micros(i128::MIN),
        }
    }

    #[must_use]
    pub fn checked_realized_net_profit(self) -> Option<Money> {
        self.realized_revenue
            .checked_sub(self.realized_costs.checked_total()?)
    }

    #[must_use]
    pub fn reserve_surplus(self) -> Money {
        match self.checked_reserve_surplus() {
            Some(value) => value,
            None => Money::from_micros(i128::MIN),
        }
    }

    #[must_use]
    pub fn checked_reserve_surplus(self) -> Option<Money> {
        self.liquid_capital.checked_sub(self.survival_reserve)
    }

    #[must_use]
    pub fn is_profitable(self) -> bool {
        self.realized_net_profit().is_positive()
    }

    #[must_use]
    pub fn is_reserve_funded(self) -> bool {
        match self.checked_reserve_surplus() {
            Some(surplus) => surplus >= Money::ZERO,
            None => false,
        }
    }

    /// Compact integer score used for reporting, never for authorization.
    /// Higher is healthier. Overflow or missing data yields `None`.
    #[must_use]
    pub fn diagnostic_score(self) -> Option<i128> {
        let profit = self.checked_realized_net_profit()?.micros();
        let surplus = self.checked_reserve_surplus()?.micros();
        let drawdown_penalty = i128::from(self.drawdown.value()) * 1_000;
        profit
            .checked_add(surplus)?
            .checked_sub(drawdown_penalty)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpportunityEstimate {
    pub expected_revenue: Money,
    pub expected_cost: Money,
    pub capital_required: Money,
    pub risk: BasisPoints,
}

impl OpportunityEstimate {
    #[must_use]
    pub fn expected_net_profit(self) -> Money {
        self.expected_revenue - self.expected_cost
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpportunityPolicy {
    pub max_risk: BasisPoints,
    pub minimum_net_profit: Money,
    pub minimum_post_action_reserve: Money,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpportunityDecision {
    Accept,
    Reject(OpportunityRejection),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpportunityRejection {
    NonPositiveExpectedProfit,
    BelowMinimumProfit,
    RiskBudgetExceeded,
    InsufficientCapital,
    SurvivalReserveViolation,
    ArithmeticOverflow,
}

#[must_use]
pub fn evaluate_opportunity(
    fitness: EconomicFitness,
    opportunity: OpportunityEstimate,
    policy: OpportunityPolicy,
) -> OpportunityDecision {
    let expected_net = match opportunity
        .expected_revenue
        .checked_sub(opportunity.expected_cost)
    {
        Some(value) => value,
        None => return OpportunityDecision::Reject(OpportunityRejection::ArithmeticOverflow),
    };

    if !expected_net.is_positive() {
        return OpportunityDecision::Reject(OpportunityRejection::NonPositiveExpectedProfit);
    }
    if expected_net < policy.minimum_net_profit {
        return OpportunityDecision::Reject(OpportunityRejection::BelowMinimumProfit);
    }
    if opportunity.risk > policy.max_risk {
        return OpportunityDecision::Reject(OpportunityRejection::RiskBudgetExceeded);
    }
    if opportunity.capital_required.is_negative() || fitness.liquid_capital.is_negative() {
        return OpportunityDecision::Reject(OpportunityRejection::InsufficientCapital);
    }
    if opportunity.capital_required > fitness.liquid_capital {
        return OpportunityDecision::Reject(OpportunityRejection::InsufficientCapital);
    }

    let post_action_capital = match fitness
        .liquid_capital
        .checked_sub(opportunity.capital_required)
    {
        Some(value) => value,
        None => return OpportunityDecision::Reject(OpportunityRejection::ArithmeticOverflow),
    };
    let required_reserve = if policy.minimum_post_action_reserve > fitness.survival_reserve {
        policy.minimum_post_action_reserve
    } else {
        fitness.survival_reserve
    };
    if post_action_capital < required_reserve {
        return OpportunityDecision::Reject(OpportunityRejection::SurvivalReserveViolation);
    }

    OpportunityDecision::Accept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bps(value: u32) -> BasisPoints {
        match BasisPoints::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid test basis points: {error}"),
        }
    }

    fn fitness() -> EconomicFitness {
        EconomicFitness {
            realized_revenue: Money::from_micros(20_000_000),
            realized_costs: OperatingCosts {
                energy: Money::from_micros(4_000_000),
                compute: Money::from_micros(1_000_000),
                network_fees: Money::from_micros(500_000),
                infrastructure: Money::from_micros(2_000_000),
                depreciation: Money::from_micros(1_000_000),
                other: Money::ZERO,
            },
            liquid_capital: Money::from_micros(100_000_000),
            survival_reserve: Money::from_micros(40_000_000),
            drawdown: bps(500),
        }
    }

    #[test]
    fn computes_realized_profit_after_all_costs() {
        assert_eq!(
            fitness().realized_net_profit(),
            Money::from_micros(11_500_000)
        );
    }

    #[test]
    fn rejects_nominally_profitable_action_that_breaks_survival_reserve() {
        let decision = evaluate_opportunity(
            fitness(),
            OpportunityEstimate {
                expected_revenue: Money::from_micros(15_000_000),
                expected_cost: Money::from_micros(10_000_000),
                capital_required: Money::from_micros(70_000_000),
                risk: bps(500),
            },
            OpportunityPolicy {
                max_risk: bps(1_000),
                minimum_net_profit: Money::from_micros(1_000_000),
                minimum_post_action_reserve: Money::from_micros(40_000_000),
            },
        );

        assert_eq!(
            decision,
            OpportunityDecision::Reject(OpportunityRejection::SurvivalReserveViolation)
        );
    }

    #[test]
    fn accepts_profitable_bounded_risk_action_with_reserve_intact() {
        let decision = evaluate_opportunity(
            fitness(),
            OpportunityEstimate {
                expected_revenue: Money::from_micros(15_000_000),
                expected_cost: Money::from_micros(10_000_000),
                capital_required: Money::from_micros(20_000_000),
                risk: bps(500),
            },
            OpportunityPolicy {
                max_risk: bps(1_000),
                minimum_net_profit: Money::from_micros(1_000_000),
                minimum_post_action_reserve: Money::from_micros(40_000_000),
            },
        );

        assert_eq!(decision, OpportunityDecision::Accept);
    }

    #[test]
    fn overflow_fails_closed_instead_of_wrapping() {
        let decision = evaluate_opportunity(
            EconomicFitness {
                realized_revenue: Money::ZERO,
                realized_costs: OperatingCosts::default(),
                liquid_capital: Money::from_micros(i128::MIN),
                survival_reserve: Money::ZERO,
                drawdown: bps(0),
            },
            OpportunityEstimate {
                expected_revenue: Money::from_micros(i128::MAX),
                expected_cost: Money::from_micros(-1),
                capital_required: Money::ZERO,
                risk: bps(0),
            },
            OpportunityPolicy {
                max_risk: bps(1),
                minimum_net_profit: Money::ZERO,
                minimum_post_action_reserve: Money::ZERO,
            },
        );
        assert_eq!(
            decision,
            OpportunityDecision::Reject(OpportunityRejection::ArithmeticOverflow)
        );
    }
}
