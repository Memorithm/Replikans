#![forbid(unsafe_code)]

use replikan_core::Money;
use replikan_economics::EconomicFitness;
use replikan_survival::SurvivalState;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplicationCandidate {
    pub expected_lifetime_revenue: Money,
    pub expected_lifetime_operating_cost: Money,
    pub replication_cost: Money,
    pub risk_premium: Money,
    pub upfront_capital_required: Money,
}

impl ReplicationCandidate {
    #[must_use]
    pub fn expected_net_value(self) -> Money {
        self.expected_lifetime_revenue
            - self.expected_lifetime_operating_cost
            - self.replication_cost
            - self.risk_premium
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplicationPolicy {
    pub minimum_parent_realized_profit: Money,
    pub minimum_child_expected_net_value: Money,
    pub minimum_post_replication_reserve: Money,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplicationDecision {
    Allowed,
    Rejected(ReplicationRejection),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplicationRejection {
    ParentNotHealthy,
    ParentNotProfitableEnough,
    ChildNonPositiveExpectedValue,
    ChildBelowMinimumExpectedValue,
    InsufficientLiquidCapital,
    ParentReserveWouldBeViolated,
}

/// Enforce the foundational Replikans rule: no profitable survival, no replication.
#[must_use]
pub fn evaluate_replication(
    parent: EconomicFitness,
    parent_state: SurvivalState,
    child: ReplicationCandidate,
    policy: ReplicationPolicy,
) -> ReplicationDecision {
    if parent_state != SurvivalState::Healthy {
        return ReplicationDecision::Rejected(ReplicationRejection::ParentNotHealthy);
    }

    if parent.realized_net_profit() < policy.minimum_parent_realized_profit {
        return ReplicationDecision::Rejected(ReplicationRejection::ParentNotProfitableEnough);
    }

    let child_value = child.expected_net_value();
    if !child_value.is_positive() {
        return ReplicationDecision::Rejected(ReplicationRejection::ChildNonPositiveExpectedValue);
    }
    if child_value < policy.minimum_child_expected_net_value {
        return ReplicationDecision::Rejected(ReplicationRejection::ChildBelowMinimumExpectedValue);
    }

    if child.upfront_capital_required > parent.liquid_capital {
        return ReplicationDecision::Rejected(ReplicationRejection::InsufficientLiquidCapital);
    }

    let post_replication_capital = parent.liquid_capital - child.upfront_capital_required;
    let required_reserve = if policy.minimum_post_replication_reserve > parent.survival_reserve {
        policy.minimum_post_replication_reserve
    } else {
        parent.survival_reserve
    };
    if post_replication_capital < required_reserve {
        return ReplicationDecision::Rejected(ReplicationRejection::ParentReserveWouldBeViolated);
    }

    ReplicationDecision::Allowed
}

#[cfg(test)]
mod tests {
    use super::*;
    use replikan_core::BasisPoints;
    use replikan_economics::OperatingCosts;

    fn bps(value: u32) -> BasisPoints {
        match BasisPoints::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid test basis points: {error}"),
        }
    }

    fn profitable_parent() -> EconomicFitness {
        EconomicFitness {
            realized_revenue: Money::from_micros(50_000_000),
            realized_costs: OperatingCosts {
                energy: Money::from_micros(10_000_000),
                compute: Money::from_micros(5_000_000),
                network_fees: Money::from_micros(1_000_000),
                infrastructure: Money::from_micros(2_000_000),
                depreciation: Money::from_micros(2_000_000),
                other: Money::ZERO,
            },
            liquid_capital: Money::from_micros(100_000_000),
            survival_reserve: Money::from_micros(50_000_000),
            drawdown: bps(500),
        }
    }

    fn policy() -> ReplicationPolicy {
        ReplicationPolicy {
            minimum_parent_realized_profit: Money::from_micros(10_000_000),
            minimum_child_expected_net_value: Money::from_micros(5_000_000),
            minimum_post_replication_reserve: Money::from_micros(50_000_000),
        }
    }

    #[test]
    fn rejects_replication_for_non_healthy_parent() {
        let child = ReplicationCandidate {
            expected_lifetime_revenue: Money::from_micros(50_000_000),
            expected_lifetime_operating_cost: Money::from_micros(20_000_000),
            replication_cost: Money::from_micros(5_000_000),
            risk_premium: Money::from_micros(5_000_000),
            upfront_capital_required: Money::from_micros(20_000_000),
        };
        assert_eq!(
            evaluate_replication(profitable_parent(), SurvivalState::Critical, child, policy()),
            ReplicationDecision::Rejected(ReplicationRejection::ParentNotHealthy)
        );
    }

    #[test]
    fn rejects_child_with_negative_value_after_risk_and_costs() {
        let child = ReplicationCandidate {
            expected_lifetime_revenue: Money::from_micros(20_000_000),
            expected_lifetime_operating_cost: Money::from_micros(12_000_000),
            replication_cost: Money::from_micros(5_000_000),
            risk_premium: Money::from_micros(4_000_000),
            upfront_capital_required: Money::from_micros(10_000_000),
        };
        assert_eq!(
            evaluate_replication(profitable_parent(), SurvivalState::Healthy, child, policy()),
            ReplicationDecision::Rejected(ReplicationRejection::ChildNonPositiveExpectedValue)
        );
    }

    #[test]
    fn rejects_profitable_child_if_parent_reserve_would_be_breached() {
        let child = ReplicationCandidate {
            expected_lifetime_revenue: Money::from_micros(80_000_000),
            expected_lifetime_operating_cost: Money::from_micros(20_000_000),
            replication_cost: Money::from_micros(5_000_000),
            risk_premium: Money::from_micros(5_000_000),
            upfront_capital_required: Money::from_micros(60_000_000),
        };
        assert_eq!(
            evaluate_replication(profitable_parent(), SurvivalState::Healthy, child, policy()),
            ReplicationDecision::Rejected(ReplicationRejection::ParentReserveWouldBeViolated)
        );
    }

    #[test]
    fn allows_economically_fit_replication() {
        let child = ReplicationCandidate {
            expected_lifetime_revenue: Money::from_micros(80_000_000),
            expected_lifetime_operating_cost: Money::from_micros(20_000_000),
            replication_cost: Money::from_micros(5_000_000),
            risk_premium: Money::from_micros(5_000_000),
            upfront_capital_required: Money::from_micros(30_000_000),
        };
        assert_eq!(
            evaluate_replication(profitable_parent(), SurvivalState::Healthy, child, policy()),
            ReplicationDecision::Allowed
        );
    }
}
