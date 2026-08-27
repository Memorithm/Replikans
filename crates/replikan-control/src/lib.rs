#![forbid(unsafe_code)]

use core::fmt;

use replikan_core::Money;
use replikan_economics::EconomicFitness;
use replikan_opportunities::{EngineError, OpportunityId, RankedOpportunity, SelectionReport};
use replikan_survival::{
    SpendingMode, SurvivalPolicy, SurvivalState, classify, spending_mode,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlPolicy {
    pub preserve_capital_max_new_capital: Money,
    pub essential_only_max_new_capital: Money,
    pub essential_only_max_expected_cost: Money,
    pub essential_only_minimum_net_profit: Money,
}

impl ControlPolicy {
    pub fn new(
        preserve_capital_max_new_capital: Money,
        essential_only_max_new_capital: Money,
        essential_only_max_expected_cost: Money,
        essential_only_minimum_net_profit: Money,
    ) -> Result<Self, ControlError> {
        if preserve_capital_max_new_capital.is_negative()
            || essential_only_max_new_capital.is_negative()
            || essential_only_max_expected_cost.is_negative()
            || essential_only_minimum_net_profit.is_negative()
        {
            return Err(ControlError::NegativePolicyLimit);
        }
        Ok(Self {
            preserve_capital_max_new_capital,
            essential_only_max_new_capital,
            essential_only_max_expected_cost,
            essential_only_minimum_net_profit,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvaluationGate {
    Evaluate {
        state: SurvivalState,
        mode: SpendingMode,
    },
    Freeze {
        state: SurvivalState,
    },
}

#[must_use]
pub fn preflight(fitness: EconomicFitness, survival_policy: SurvivalPolicy) -> EvaluationGate {
    let state = classify(fitness, survival_policy);
    let mode = spending_mode(state);
    match mode {
        SpendingMode::Frozen => EvaluationGate::Freeze { state },
        SpendingMode::Normal | SpendingMode::PreserveCapital | SpendingMode::EssentialOnly => {
            EvaluationGate::Evaluate { state, mode }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlDecision {
    Run {
        opportunity_id: OpportunityId,
        state: SurvivalState,
        mode: SpendingMode,
        expected_net_profit: Money,
        capital_required: Money,
    },
    Hold {
        state: SurvivalState,
        mode: SpendingMode,
        reason: HoldReason,
    },
    Freeze {
        state: SurvivalState,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HoldReason {
    NoAcceptedOpportunity,
    SurvivalModeRestriction,
}

pub fn decide(
    fitness: EconomicFitness,
    survival_policy: SurvivalPolicy,
    selection: &SelectionReport,
    control_policy: ControlPolicy,
) -> Result<ControlDecision, ControlError> {
    let EvaluationGate::Evaluate { state, mode } = preflight(fitness, survival_policy) else {
        return Ok(ControlDecision::Freeze {
            state: SurvivalState::Insolvent,
        });
    };

    if selection.accepted.is_empty() {
        return Ok(ControlDecision::Hold {
            state,
            mode,
            reason: HoldReason::NoAcceptedOpportunity,
        });
    }

    let candidate = match mode {
        SpendingMode::Normal => selection.accepted.first(),
        SpendingMode::PreserveCapital => selection
            .accepted
            .iter()
            .find(|candidate| {
                candidate.quote.capital_required <= control_policy.preserve_capital_max_new_capital
            }),
        SpendingMode::EssentialOnly => {
            find_essential_candidate(selection, control_policy)?
        }
        SpendingMode::Frozen => None,
    };

    let Some(candidate) = candidate else {
        return Ok(ControlDecision::Hold {
            state,
            mode,
            reason: HoldReason::SurvivalModeRestriction,
        });
    };

    run_decision(candidate, state, mode)
}

fn find_essential_candidate(
    selection: &SelectionReport,
    policy: ControlPolicy,
) -> Result<Option<&RankedOpportunity>, ControlError> {
    for candidate in &selection.accepted {
        if candidate.quote.capital_required > policy.essential_only_max_new_capital {
            continue;
        }
        let expected_cost = candidate
            .quote
            .checked_expected_cost()
            .map_err(ControlError::Engine)?;
        if expected_cost > policy.essential_only_max_expected_cost {
            continue;
        }
        let net = candidate
            .quote
            .checked_expected_net_profit()
            .map_err(ControlError::Engine)?;
        if net < policy.essential_only_minimum_net_profit {
            continue;
        }
        return Ok(Some(candidate));
    }
    Ok(None)
}

fn run_decision(
    candidate: &RankedOpportunity,
    state: SurvivalState,
    mode: SpendingMode,
) -> Result<ControlDecision, ControlError> {
    let expected_net_profit = candidate
        .quote
        .checked_expected_net_profit()
        .map_err(ControlError::Engine)?;
    Ok(ControlDecision::Run {
        opportunity_id: candidate.quote.id.clone(),
        state,
        mode,
        expected_net_profit,
        capital_required: candidate.quote.capital_required,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlError {
    NegativePolicyLimit,
    Engine(EngineError),
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NegativePolicyLimit => write!(f, "control policy limits cannot be negative"),
            Self::Engine(error) => write!(f, "control evaluation failed: {error}"),
        }
    }
}

impl std::error::Error for ControlError {}

#[cfg(test)]
mod tests {
    use super::*;
    use replikan_core::BasisPoints;
    use replikan_economics::OperatingCosts;
    use replikan_opportunities::{
        EvidenceRef, OpportunityKind, OpportunityQuote, RankedOpportunity,
    };

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

    fn evidence() -> EvidenceRef {
        match EvidenceRef::new("control:test") {
            Ok(value) => value,
            Err(error) => unreachable!("valid evidence: {error}"),
        }
    }

    fn fitness(capital: i128, reserve: i128, drawdown: u32) -> EconomicFitness {
        EconomicFitness {
            realized_revenue: Money::from_micros(20_000_000),
            realized_costs: OperatingCosts::default(),
            liquid_capital: Money::from_micros(capital),
            survival_reserve: Money::from_micros(reserve),
            drawdown: bps(drawdown),
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

    fn ranked(
        name: &str,
        score: i128,
        revenue: i128,
        cost: i128,
        capital: i128,
    ) -> RankedOpportunity {
        let quote = match OpportunityQuote::new(
            id(name),
            OpportunityKind::Mining,
            "control-test",
            1_000_000,
            1_060_000,
            86_400,
            Money::from_micros(revenue),
            OperatingCosts {
                energy: Money::from_micros(cost),
                compute: Money::ZERO,
                network_fees: Money::ZERO,
                infrastructure: Money::ZERO,
                depreciation: Money::ZERO,
                other: Money::ZERO,
            },
            Money::from_micros(capital),
            bps(500),
            bps(9_000),
            vec![evidence()],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid quote: {error}"),
        };
        RankedOpportunity {
            quote,
            score_micros_per_day: score,
        }
    }

    fn report(accepted: Vec<RankedOpportunity>) -> SelectionReport {
        SelectionReport {
            accepted,
            rejected: Vec::new(),
        }
    }

    #[test]
    fn insolvent_state_freezes_before_external_evaluation() {
        assert_eq!(
            preflight(fitness(-1, 20_000_000, 100), survival_policy()),
            EvaluationGate::Freeze {
                state: SurvivalState::Insolvent
            }
        );
    }

    #[test]
    fn healthy_mode_runs_highest_ranked_candidate() {
        let selection = report(vec![
            ranked("best", 100, 30_000_000, 10_000_000, 10_000_000),
            ranked("second", 90, 25_000_000, 10_000_000, 0),
        ]);
        let decision = match decide(
            fitness(80_000_000, 40_000_000, 100),
            survival_policy(),
            &selection,
            control_policy(),
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid decision: {error}"),
        };
        assert!(matches!(
            decision,
            ControlDecision::Run { opportunity_id, mode: SpendingMode::Normal, .. }
                if opportunity_id.as_str() == "best"
        ));
    }

    #[test]
    fn constrained_mode_prefers_zero_new_capital_over_higher_score() {
        let selection = report(vec![
            ranked("capital-heavy", 100, 30_000_000, 10_000_000, 5_000_000),
            ranked("capital-free", 90, 25_000_000, 10_000_000, 0),
        ]);
        let decision = match decide(
            fitness(45_000_000, 40_000_000, 100),
            survival_policy(),
            &selection,
            control_policy(),
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid decision: {error}"),
        };
        assert!(matches!(
            decision,
            ControlDecision::Run {
                opportunity_id,
                mode: SpendingMode::PreserveCapital,
                ..
            } if opportunity_id.as_str() == "capital-free"
        ));
    }

    #[test]
    fn critical_mode_requires_low_cost_strong_margin_and_no_new_capital() {
        let selection = report(vec![
            ranked("too-costly", 100, 40_000_000, 20_000_000, 0),
            ranked("survival-fit", 90, 20_000_000, 10_000_000, 0),
        ]);
        let decision = match decide(
            fitness(15_000_000, 10_000_000, 100),
            survival_policy(),
            &selection,
            control_policy(),
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid decision: {error}"),
        };
        assert!(matches!(
            decision,
            ControlDecision::Run {
                opportunity_id,
                mode: SpendingMode::EssentialOnly,
                ..
            } if opportunity_id.as_str() == "survival-fit"
        ));
    }

    #[test]
    fn constrained_mode_holds_when_every_candidate_requires_new_capital() {
        let selection = report(vec![ranked(
            "capital-heavy",
            100,
            30_000_000,
            10_000_000,
            5_000_000,
        )]);
        let decision = match decide(
            fitness(45_000_000, 40_000_000, 100),
            survival_policy(),
            &selection,
            control_policy(),
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid decision: {error}"),
        };
        assert_eq!(
            decision,
            ControlDecision::Hold {
                state: SurvivalState::Constrained,
                mode: SpendingMode::PreserveCapital,
                reason: HoldReason::SurvivalModeRestriction,
            }
        );
    }
}
