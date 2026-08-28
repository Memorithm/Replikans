#![forbid(unsafe_code)]

use core::fmt;

use replikan_control::{ControlDecision, ControlPolicy};
use replikan_economics::EconomicFitness;
use replikan_ledger::LedgerSnapshot;
use replikan_opportunities::SelectionPolicy;
use replikan_survival::SurvivalPolicy;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisionObservation {
    pub observed_at_unix_ms: u64,
    pub ledger_snapshot: LedgerSnapshot,
    pub fitness: EconomicFitness,
    pub selection_policy: SelectionPolicy,
    pub survival_policy: SurvivalPolicy,
    pub control_policy: ControlPolicy,
    pub materialized_deployments: usize,
    pub materialization_rejections: usize,
    pub accepted_opportunities: usize,
    pub rejected_opportunities: usize,
    pub price_source_count: usize,
    pub network_source_count: usize,
    pub evidence: Vec<String>,
    pub diagnostics: Vec<String>,
    pub decision: ControlDecision,
}

impl DecisionObservation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        observed_at_unix_ms: u64,
        ledger_snapshot: LedgerSnapshot,
        fitness: EconomicFitness,
        selection_policy: SelectionPolicy,
        survival_policy: SurvivalPolicy,
        control_policy: ControlPolicy,
        materialized_deployments: usize,
        materialization_rejections: usize,
        accepted_opportunities: usize,
        rejected_opportunities: usize,
        price_source_count: usize,
        network_source_count: usize,
        mut evidence: Vec<String>,
        mut diagnostics: Vec<String>,
        decision: ControlDecision,
    ) -> Result<Self, DecisionLedgerError> {
        if evidence.is_empty() {
            return Err(DecisionLedgerError::MissingEvidence);
        }
        if evidence.iter().any(|value| value.trim().is_empty()) {
            return Err(DecisionLedgerError::BlankEvidence);
        }
        if diagnostics.iter().any(|value| value.trim().is_empty()) {
            return Err(DecisionLedgerError::BlankDiagnostic);
        }

        evidence.sort();
        evidence.dedup();
        diagnostics.sort();
        diagnostics.dedup();

        Ok(Self {
            observed_at_unix_ms,
            ledger_snapshot,
            fitness,
            selection_policy,
            survival_policy,
            control_policy,
            materialized_deployments,
            materialization_rejections,
            accepted_opportunities,
            rejected_opportunities,
            price_source_count,
            network_source_count,
            evidence,
            diagnostics,
            decision,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisionEntry {
    pub sequence: u64,
    pub observation: DecisionObservation,
}

#[derive(Clone, Debug, Default)]
pub struct DecisionLedger {
    entries: Vec<DecisionEntry>,
    next_sequence: u64,
}

impl DecisionLedger {
    #[must_use]
    pub fn entries(&self) -> &[DecisionEntry] {
        &self.entries
    }

    #[must_use]
    pub fn latest(&self) -> Option<&DecisionEntry> {
        self.entries.last()
    }

    pub fn append(&mut self, observation: DecisionObservation) -> Result<u64, DecisionLedgerError> {
        if let Some(latest) = self.latest() {
            if observation.observed_at_unix_ms < latest.observation.observed_at_unix_ms {
                return Err(DecisionLedgerError::TimestampRegression {
                    previous: latest.observation.observed_at_unix_ms,
                    observed: observation.observed_at_unix_ms,
                });
            }
        }

        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(DecisionLedgerError::SequenceOverflow)?;
        self.entries.push(DecisionEntry {
            sequence,
            observation,
        });
        Ok(sequence)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecisionLedgerError {
    MissingEvidence,
    BlankEvidence,
    BlankDiagnostic,
    SequenceOverflow,
    TimestampRegression { previous: u64, observed: u64 },
}

impl fmt::Display for DecisionLedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEvidence => write!(f, "decision observations require evidence"),
            Self::BlankEvidence => write!(f, "decision evidence references cannot be blank"),
            Self::BlankDiagnostic => write!(f, "decision diagnostics cannot be blank"),
            Self::SequenceOverflow => write!(f, "decision ledger sequence overflow"),
            Self::TimestampRegression { previous, observed } => write!(
                f,
                "decision timestamp regressed from {previous} to {observed}"
            ),
        }
    }
}

impl std::error::Error for DecisionLedgerError {}

#[cfg(test)]
mod tests {
    use super::*;
    use replikan_control::{ControlDecision, HoldReason};
    use replikan_core::{BasisPoints, Money};
    use replikan_economics::{OperatingCosts, OpportunityPolicy};
    use replikan_survival::{SpendingMode, SurvivalState};

    fn bps(value: u32) -> BasisPoints {
        match BasisPoints::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid basis points: {error}"),
        }
    }

    fn fitness() -> EconomicFitness {
        EconomicFitness {
            realized_revenue: Money::from_micros(20_000_000),
            realized_costs: OperatingCosts::default(),
            liquid_capital: Money::from_micros(80_000_000),
            survival_reserve: Money::from_micros(40_000_000),
            drawdown: bps(500),
        }
    }

    fn selection_policy() -> SelectionPolicy {
        SelectionPolicy {
            economics: OpportunityPolicy {
                max_risk: bps(2_000),
                minimum_net_profit: Money::from_micros(1_000_000),
                minimum_post_action_reserve: Money::from_micros(40_000_000),
            },
            minimum_confidence: bps(7_000),
            maximum_quote_age_ms: 60_000,
            minimum_evidence_count: 2,
            capital_charge: bps(100),
        }
    }

    fn survival_policy() -> SurvivalPolicy {
        SurvivalPolicy {
            critical_reserve: Money::from_micros(20_000_000),
            constrained_reserve: Money::from_micros(50_000_000),
            maximum_drawdown: bps(2_000),
        }
    }

    fn control_policy() -> ControlPolicy {
        match ControlPolicy::new(
            Money::ZERO,
            Money::ZERO,
            Money::from_micros(15_000_000),
            Money::from_micros(5_000_000),
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid control policy: {error}"),
        }
    }

    fn observation(at: u64) -> DecisionObservation {
        match DecisionObservation::new(
            at,
            LedgerSnapshot {
                realized_revenue: Money::from_micros(20_000_000),
                costs: OperatingCosts::default(),
                external_capital_in: Money::ZERO,
                external_capital_out: Money::ZERO,
            },
            fitness(),
            selection_policy(),
            survival_policy(),
            control_policy(),
            2,
            1,
            1,
            1,
            2,
            2,
            vec![
                "price:coinbase".to_owned(),
                "network:mempool".to_owned(),
                "price:coinbase".to_owned(),
            ],
            vec!["one resource rejected".to_owned()],
            ControlDecision::Hold {
                state: SurvivalState::Healthy,
                mode: SpendingMode::Normal,
                reason: HoldReason::NoAcceptedOpportunity,
            },
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid decision observation: {error}"),
        }
    }

    #[test]
    fn evidence_is_canonicalized_before_append() {
        let observation = observation(1_000_000);
        assert_eq!(
            observation.evidence,
            vec!["network:mempool".to_owned(), "price:coinbase".to_owned()]
        );
    }

    #[test]
    fn append_assigns_monotonic_sequences_and_preserves_policy_context() {
        let mut ledger = DecisionLedger::default();
        assert_eq!(ledger.append(observation(1_000_000)), Ok(0));
        assert_eq!(ledger.append(observation(1_000_001)), Ok(1));
        assert_eq!(ledger.entries().len(), 2);
        assert_eq!(
            ledger.entries()[0].observation.selection_policy,
            selection_policy()
        );
        assert_eq!(
            ledger.entries()[0].observation.survival_policy,
            survival_policy()
        );
    }

    #[test]
    fn timestamp_regression_is_rejected_without_mutation() {
        let mut ledger = DecisionLedger::default();
        assert_eq!(ledger.append(observation(2_000_000)), Ok(0));
        assert_eq!(
            ledger.append(observation(1_999_999)),
            Err(DecisionLedgerError::TimestampRegression {
                previous: 2_000_000,
                observed: 1_999_999,
            })
        );
        assert_eq!(ledger.entries().len(), 1);
    }

    #[test]
    fn blank_evidence_is_rejected() {
        let result = DecisionObservation::new(
            1_000_000,
            LedgerSnapshot::default(),
            fitness(),
            selection_policy(),
            survival_policy(),
            control_policy(),
            0,
            0,
            0,
            0,
            0,
            0,
            vec!["   ".to_owned()],
            Vec::new(),
            ControlDecision::Freeze {
                state: SurvivalState::Insolvent,
            },
        );
        assert_eq!(result, Err(DecisionLedgerError::BlankEvidence));
    }
}
