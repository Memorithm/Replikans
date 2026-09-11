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
    SustainedFitnessUnproven,
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

/// One realized observation used to prove fitness over time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FitnessSample {
    pub observed_at_unix_ms: u64,
    pub fitness: EconomicFitness,
    pub state: SurvivalState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SustainedFitnessPolicy {
    pub window_ms: u64,
    pub minimum_samples: usize,
    pub minimum_profitable_samples: usize,
    pub require_healthy_throughout: bool,
}

impl SustainedFitnessPolicy {
    pub fn conservative_default() -> Self {
        Self {
            window_ms: 7 * 24 * 60 * 60 * 1_000,
            minimum_samples: 3,
            minimum_profitable_samples: 3,
            require_healthy_throughout: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SustainedFitnessError {
    EmptyWindow,
    MissingSamples,
    TimestampRegression,
}

impl core::fmt::Display for SustainedFitnessError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EmptyWindow => write!(f, "sustained fitness window must be positive"),
            Self::MissingSamples => write!(f, "sustained fitness requires at least one sample"),
            Self::TimestampRegression => write!(f, "fitness samples are not time-ordered"),
        }
    }
}

impl std::error::Error for SustainedFitnessError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SustainedFitnessReport {
    pub sample_count: usize,
    pub profitable_samples: usize,
    pub healthy_samples: usize,
    pub proven: bool,
}

/// Require repeated realized profitability inside a declared window (roadmap REP5).
pub fn evaluate_sustained_fitness(
    samples: &[FitnessSample],
    now_unix_ms: u64,
    policy: SustainedFitnessPolicy,
) -> Result<SustainedFitnessReport, SustainedFitnessError> {
    if policy.window_ms == 0 || policy.minimum_samples == 0 {
        return Err(SustainedFitnessError::EmptyWindow);
    }
    if samples.is_empty() {
        return Err(SustainedFitnessError::MissingSamples);
    }

    let mut previous = 0_u64;
    for (index, sample) in samples.iter().enumerate() {
        if index > 0 && sample.observed_at_unix_ms < previous {
            return Err(SustainedFitnessError::TimestampRegression);
        }
        previous = sample.observed_at_unix_ms;
    }

    let window_start = now_unix_ms.saturating_sub(policy.window_ms);
    let in_window: Vec<&FitnessSample> = samples
        .iter()
        .filter(|sample| {
            sample.observed_at_unix_ms >= window_start && sample.observed_at_unix_ms <= now_unix_ms
        })
        .collect();

    let profitable_samples = in_window
        .iter()
        .filter(|sample| sample.fitness.is_profitable())
        .count();
    let healthy_samples = in_window
        .iter()
        .filter(|sample| sample.state == SurvivalState::Healthy)
        .count();

    let proven = in_window.len() >= policy.minimum_samples
        && profitable_samples >= policy.minimum_profitable_samples
        && (!policy.require_healthy_throughout || healthy_samples == in_window.len());

    Ok(SustainedFitnessReport {
        sample_count: in_window.len(),
        profitable_samples,
        healthy_samples,
        proven,
    })
}

pub fn evaluate_replication_with_history(
    parent: EconomicFitness,
    parent_state: SurvivalState,
    child: ReplicationCandidate,
    policy: ReplicationPolicy,
    samples: &[FitnessSample],
    now_unix_ms: u64,
    sustained: SustainedFitnessPolicy,
) -> Result<ReplicationDecision, SustainedFitnessError> {
    let report = evaluate_sustained_fitness(samples, now_unix_ms, sustained)?;
    if !report.proven {
        return Ok(ReplicationDecision::Rejected(
            ReplicationRejection::SustainedFitnessUnproven,
        ));
    }
    Ok(evaluate_replication(parent, parent_state, child, policy))
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
            evaluate_replication(
                profitable_parent(),
                SurvivalState::Critical,
                child,
                policy()
            ),
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

    fn sample(at: u64, profitable: bool, healthy: bool) -> FitnessSample {
        let mut parent = profitable_parent();
        if !profitable {
            parent.realized_revenue = Money::from_micros(1);
        }
        FitnessSample {
            observed_at_unix_ms: at,
            fitness: parent,
            state: if healthy {
                SurvivalState::Healthy
            } else {
                SurvivalState::Constrained
            },
        }
    }

    fn fit_child() -> ReplicationCandidate {
        ReplicationCandidate {
            expected_lifetime_revenue: Money::from_micros(80_000_000),
            expected_lifetime_operating_cost: Money::from_micros(20_000_000),
            replication_cost: Money::from_micros(5_000_000),
            risk_premium: Money::from_micros(5_000_000),
            upfront_capital_required: Money::from_micros(30_000_000),
        }
    }

    #[test]
    fn sustained_gate_rejects_a_single_lucky_period() {
        let samples = [sample(1_000, true, true)];
        let decision = match evaluate_replication_with_history(
            profitable_parent(),
            SurvivalState::Healthy,
            fit_child(),
            policy(),
            &samples,
            1_000,
            SustainedFitnessPolicy::conservative_default(),
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid sustained evaluation: {error}"),
        };
        assert_eq!(
            decision,
            ReplicationDecision::Rejected(ReplicationRejection::SustainedFitnessUnproven)
        );
    }

    #[test]
    fn sustained_gate_allows_repeated_healthy_profit() {
        let samples = [
            sample(1_000, true, true),
            sample(2_000, true, true),
            sample(3_000, true, true),
        ];
        let policy_window = SustainedFitnessPolicy {
            window_ms: 10_000,
            minimum_samples: 3,
            minimum_profitable_samples: 3,
            require_healthy_throughout: true,
        };
        let decision = match evaluate_replication_with_history(
            profitable_parent(),
            SurvivalState::Healthy,
            fit_child(),
            policy(),
            &samples,
            3_000,
            policy_window,
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid sustained evaluation: {error}"),
        };
        assert_eq!(decision, ReplicationDecision::Allowed);
    }
}
