#![forbid(unsafe_code)]

use replikan_core::{BasisPoints, Money};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
        self.energy
            + self.compute
            + self.network_fees
            + self.infrastructure
            + self.depreciation
            + self.other
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
        self.realized_revenue - self.realized_costs.total()
    }

    #[must_use]
    pub fn reserve_surplus(self) -> Money {
        self.liquid_capital - self.survival_reserve
    }

    #[must_use]
    pub fn is_profitable(self) -> bool {
        self.realized_net_profit().is_positive()
    }

    #[must_use]
    pub fn is_reserve_funded(self) -> bool {
        self.reserve_surplus() >= Money::ZERO
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
}

#[must_use]
pub fn evaluate_opportunity(
    fitness: EconomicFitness,
    opportunity: OpportunityEstimate,
    policy: OpportunityPolicy,
) -> OpportunityDecision {
    let expected_net = opportunity.expected_net_profit();

    if !expected_net.is_positive() {
        return OpportunityDecision::Reject(OpportunityRejection::NonPositiveExpectedProfit);
    }
    if expected_net < policy.minimum_net_profit {
        return OpportunityDecision::Reject(OpportunityRejection::BelowMinimumProfit);
    }
    if opportunity.risk > policy.max_risk {
        return OpportunityDecision::Reject(OpportunityRejection::RiskBudgetExceeded);
    }
    if opportunity.capital_required > fitness.liquid_capital {
        return OpportunityDecision::Reject(OpportunityRejection::InsufficientCapital);
    }

    let post_action_capital = fitness.liquid_capital - opportunity.capital_required;
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
        assert_eq!(fitness().realized_net_profit(), Money::from_micros(11_500_000));
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
}
