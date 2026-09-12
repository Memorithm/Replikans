#![forbid(unsafe_code)]

use core::fmt;

use replikan_decision_ledger::{
    decode_fitness_archive, ArchiveError, DecisionLedger,
};
use replikan_economics::EconomicFitness;
use replikan_replication::{
    FitnessSample, ReplicationCandidate, ReplicationDecision, ReplicationPolicy,
    ReplicationRejection, SustainedFitnessError, SustainedFitnessPolicy,
    evaluate_replication_with_history,
};
use replikan_survival::SurvivalState;

/// Project recorded control observations into the sustained-fitness sample type.
#[must_use]
pub fn timeline_samples(decision_ledger: &DecisionLedger) -> Vec<FitnessSample> {
    decision_ledger
        .fitness_timeline()
        .into_iter()
        .map(|point| FitnessSample {
            observed_at_unix_ms: point.observed_at_unix_ms,
            fitness: point.fitness,
            state: point.state,
        })
        .collect()
}

/// Replication is refused unless the decision ledger already proves sustained
/// realized fitness. An empty history fails closed (roadmap REP5).
pub fn consider_replication(
    decision_ledger: &DecisionLedger,
    parent: EconomicFitness,
    parent_state: SurvivalState,
    child: ReplicationCandidate,
    policy: ReplicationPolicy,
    now_unix_ms: u64,
    sustained: SustainedFitnessPolicy,
) -> Result<ReplicationDecision, SustainedFitnessError> {
    let samples = timeline_samples(decision_ledger);
    if samples.is_empty() {
        return Ok(ReplicationDecision::Rejected(
            ReplicationRejection::SustainedFitnessUnproven,
        ));
    }
    evaluate_replication_with_history(
        parent,
        parent_state,
        child,
        policy,
        &samples,
        now_unix_ms,
        sustained,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveReplicationError {
    Archive(ArchiveError),
    Sustained(SustainedFitnessError),
}

impl fmt::Display for ArchiveReplicationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Archive(error) => write!(f, "fitness archive failed: {error}"),
            Self::Sustained(error) => write!(f, "sustained fitness failed: {error}"),
        }
    }
}

impl std::error::Error for ArchiveReplicationError {}

/// Same gate as [`consider_replication`], sourced from a persisted archive.
/// Corrupt encoding fails closed as an error; an empty valid archive is unproven.
pub fn consider_replication_from_archive(
    archive_text: &str,
    parent: EconomicFitness,
    parent_state: SurvivalState,
    child: ReplicationCandidate,
    policy: ReplicationPolicy,
    now_unix_ms: u64,
    sustained: SustainedFitnessPolicy,
) -> Result<ReplicationDecision, ArchiveReplicationError> {
    let points =
        decode_fitness_archive(archive_text).map_err(ArchiveReplicationError::Archive)?;
    let samples: Vec<FitnessSample> = points
        .into_iter()
        .map(|point| FitnessSample {
            observed_at_unix_ms: point.observed_at_unix_ms,
            fitness: point.fitness,
            state: point.state,
        })
        .collect();
    if samples.is_empty() {
        return Ok(ReplicationDecision::Rejected(
            ReplicationRejection::SustainedFitnessUnproven,
        ));
    }
    evaluate_replication_with_history(
        parent,
        parent_state,
        child,
        policy,
        &samples,
        now_unix_ms,
        sustained,
    )
    .map_err(ArchiveReplicationError::Sustained)
}

#[cfg(test)]
mod tests {
    use super::*;
    use replikan_control::{ControlDecision, ControlPolicy, HoldReason};
    use replikan_core::{BasisPoints, Money};
    use replikan_decision_ledger::{encode_fitness_archive, DecisionObservation, FitnessPoint};
    use replikan_economics::{OperatingCosts, OpportunityPolicy};
    use replikan_ledger::LedgerSnapshot;
    use replikan_opportunities::SelectionPolicy;
    use replikan_survival::{SpendingMode, SurvivalPolicy};

    fn bps(value: u32) -> BasisPoints {
        match BasisPoints::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid basis points: {error}"),
        }
    }

    fn fitness() -> EconomicFitness {
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

    fn child() -> ReplicationCandidate {
        ReplicationCandidate {
            expected_lifetime_revenue: Money::from_micros(80_000_000),
            expected_lifetime_operating_cost: Money::from_micros(20_000_000),
            replication_cost: Money::from_micros(5_000_000),
            risk_premium: Money::from_micros(5_000_000),
            upfront_capital_required: Money::from_micros(30_000_000),
        }
    }

    fn replication_policy() -> ReplicationPolicy {
        ReplicationPolicy {
            minimum_parent_realized_profit: Money::from_micros(10_000_000),
            minimum_child_expected_net_value: Money::from_micros(5_000_000),
            minimum_post_replication_reserve: Money::from_micros(50_000_000),
        }
    }

    fn short_window() -> SustainedFitnessPolicy {
        SustainedFitnessPolicy {
            window_ms: 10_000,
            minimum_samples: 3,
            minimum_profitable_samples: 3,
            require_healthy_throughout: true,
        }
    }

    fn observation(at: u64) -> DecisionObservation {
        let selection = SelectionPolicy {
            economics: OpportunityPolicy {
                max_risk: bps(2_000),
                minimum_net_profit: Money::from_micros(1_000_000),
                minimum_post_action_reserve: Money::from_micros(40_000_000),
            },
            minimum_confidence: bps(7_000),
            maximum_quote_age_ms: 60_000,
            minimum_evidence_count: 2,
            capital_charge: bps(100),
        };
        let survival = SurvivalPolicy {
            critical_reserve: Money::from_micros(20_000_000),
            constrained_reserve: Money::from_micros(50_000_000),
            maximum_drawdown: bps(2_000),
        };
        let control = match ControlPolicy::new(
            Money::ZERO,
            Money::ZERO,
            Money::from_micros(15_000_000),
            Money::from_micros(5_000_000),
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid control policy: {error}"),
        };
        match DecisionObservation::new(
            at,
            LedgerSnapshot {
                realized_revenue: Money::from_micros(50_000_000),
                costs: OperatingCosts::default(),
                external_capital_in: Money::ZERO,
                external_capital_out: Money::ZERO,
            },
            fitness(),
            selection,
            survival,
            control,
            1,
            0,
            1,
            0,
            2,
            2,
            vec!["cycle-verified-plan".to_owned()],
            Vec::new(),
            ControlDecision::Hold {
                state: SurvivalState::Healthy,
                mode: SpendingMode::Normal,
                reason: HoldReason::NoAcceptedOpportunity,
            },
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid observation: {error}"),
        }
    }

    fn archive_point(sequence: u64, at: u64) -> FitnessPoint {
        FitnessPoint {
            sequence,
            observed_at_unix_ms: at,
            fitness: fitness(),
            state: SurvivalState::Healthy,
        }
    }

    #[test]
    fn empty_decision_history_fails_closed() {
        let ledger = DecisionLedger::default();
        let decision = match consider_replication(
            &ledger,
            fitness(),
            SurvivalState::Healthy,
            child(),
            replication_policy(),
            3_000,
            short_window(),
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("empty history is a rejection, not an error: {error}"),
        };
        assert_eq!(
            decision,
            ReplicationDecision::Rejected(ReplicationRejection::SustainedFitnessUnproven)
        );
        assert!(timeline_samples(&ledger).is_empty());
    }

    #[test]
    fn one_observation_is_not_sustained() {
        let mut ledger = DecisionLedger::default();
        assert_eq!(ledger.append(observation(1_000)), Ok(0));
        let decision = match consider_replication(
            &ledger,
            fitness(),
            SurvivalState::Healthy,
            child(),
            replication_policy(),
            1_000,
            short_window(),
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("{error}"),
        };
        assert_eq!(
            decision,
            ReplicationDecision::Rejected(ReplicationRejection::SustainedFitnessUnproven)
        );
    }

    #[test]
    fn three_healthy_profitable_observations_may_replicate() {
        let mut ledger = DecisionLedger::default();
        assert_eq!(ledger.append(observation(1_000)), Ok(0));
        assert_eq!(ledger.append(observation(2_000)), Ok(1));
        assert_eq!(ledger.append(observation(3_000)), Ok(2));
        assert_eq!(timeline_samples(&ledger).len(), 3);
        let decision = match consider_replication(
            &ledger,
            fitness(),
            SurvivalState::Healthy,
            child(),
            replication_policy(),
            3_000,
            short_window(),
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("{error}"),
        };
        assert_eq!(decision, ReplicationDecision::Allowed);
    }

    #[test]
    fn empty_archive_is_unproven_and_corrupt_archive_fails_closed() {
        let empty = match consider_replication_from_archive(
            "REPLIKANS_FITNESS_V1\n",
            fitness(),
            SurvivalState::Healthy,
            child(),
            replication_policy(),
            3_000,
            short_window(),
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("empty valid archive is a rejection: {error}"),
        };
        assert_eq!(
            empty,
            ReplicationDecision::Rejected(ReplicationRejection::SustainedFitnessUnproven)
        );
        assert_eq!(
            consider_replication_from_archive(
                "NOT_AN_ARCHIVE\n",
                fitness(),
                SurvivalState::Healthy,
                child(),
                replication_policy(),
                3_000,
                short_window(),
            ),
            Err(ArchiveReplicationError::Archive(ArchiveError::InvalidEncoding))
        );
    }

    #[test]
    fn persisted_archive_can_prove_sustained_fitness() {
        let text = encode_fitness_archive(&[
            archive_point(0, 1_000),
            archive_point(1, 2_000),
            archive_point(2, 3_000),
        ]);
        let decision = match consider_replication_from_archive(
            &text,
            fitness(),
            SurvivalState::Healthy,
            child(),
            replication_policy(),
            3_000,
            short_window(),
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("{error}"),
        };
        assert_eq!(decision, ReplicationDecision::Allowed);
    }
}
