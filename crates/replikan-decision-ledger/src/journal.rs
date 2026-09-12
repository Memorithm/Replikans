#![forbid(unsafe_code)]

use core::fmt;
use std::fs;
use std::path::Path;

use replikan_control::{ControlDecision, ControlError, ControlPolicy, HoldReason};
use replikan_core::{BasisPoints, Money};
use replikan_economics::{EconomicFitness, OperatingCosts, OpportunityPolicy};
use replikan_ledger::LedgerSnapshot;
use replikan_opportunities::{OpportunityId, SelectionPolicy};
use replikan_survival::{SpendingMode, SurvivalPolicy, SurvivalPolicyError, SurvivalState};

use crate::ledger::{DecisionLedger, DecisionLedgerError, DecisionObservation};

const JOURNAL_FORMAT: &str = "REPLIKANS_DECISION_V1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalError {
    InvalidEncoding,
    SequenceMismatch,
    TimestampRegression,
    UnknownSurvivalState,
    UnknownSpendingMode,
    UnknownHoldReason,
    UnknownDecisionKind,
    InvalidDrawdown,
    InvalidPolicy,
    FieldNotEncodable,
    Io,
    ExistingJournalConflict,
    Observation(DecisionLedgerError),
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEncoding => write!(f, "decision journal encoding is invalid"),
            Self::SequenceMismatch => write!(f, "decision journal sequence mismatch"),
            Self::TimestampRegression => write!(f, "decision journal timestamp regression"),
            Self::UnknownSurvivalState => write!(f, "unknown survival state token"),
            Self::UnknownSpendingMode => write!(f, "unknown spending mode token"),
            Self::UnknownHoldReason => write!(f, "unknown hold reason token"),
            Self::UnknownDecisionKind => write!(f, "unknown control decision token"),
            Self::InvalidDrawdown => write!(f, "decision journal drawdown is out of range"),
            Self::InvalidPolicy => write!(f, "decision journal policy is invalid"),
            Self::FieldNotEncodable => {
                write!(f, "decision journal field cannot contain '|' or newlines")
            }
            Self::Io => write!(f, "decision journal I/O failed"),
            Self::ExistingJournalConflict => {
                write!(f, "existing decision journal conflicts with new entries")
            }
            Self::Observation(error) => write!(f, "decision observation failed: {error}"),
        }
    }
}

impl std::error::Error for JournalError {}

fn reject_token(value: &str) -> Result<(), JournalError> {
    if value.is_empty() || value.contains('|') || value.contains('\n') || value.contains('\r') {
        return Err(JournalError::FieldNotEncodable);
    }
    Ok(())
}

fn state_token(state: SurvivalState) -> &'static str {
    match state {
        SurvivalState::Healthy => "healthy",
        SurvivalState::Constrained => "constrained",
        SurvivalState::Critical => "critical",
        SurvivalState::Insolvent => "insolvent",
    }
}

fn parse_state(token: &str) -> Result<SurvivalState, JournalError> {
    match token {
        "healthy" => Ok(SurvivalState::Healthy),
        "constrained" => Ok(SurvivalState::Constrained),
        "critical" => Ok(SurvivalState::Critical),
        "insolvent" => Ok(SurvivalState::Insolvent),
        _ => Err(JournalError::UnknownSurvivalState),
    }
}

fn mode_token(mode: SpendingMode) -> &'static str {
    match mode {
        SpendingMode::Normal => "normal",
        SpendingMode::PreserveCapital => "preserve_capital",
        SpendingMode::EssentialOnly => "essential_only",
        SpendingMode::Frozen => "frozen",
    }
}

fn parse_mode(token: &str) -> Result<SpendingMode, JournalError> {
    match token {
        "normal" => Ok(SpendingMode::Normal),
        "preserve_capital" => Ok(SpendingMode::PreserveCapital),
        "essential_only" => Ok(SpendingMode::EssentialOnly),
        "frozen" => Ok(SpendingMode::Frozen),
        _ => Err(JournalError::UnknownSpendingMode),
    }
}

fn hold_token(reason: HoldReason) -> &'static str {
    match reason {
        HoldReason::NoAcceptedOpportunity => "no_accepted_opportunity",
        HoldReason::SurvivalModeRestriction => "survival_mode_restriction",
    }
}

fn parse_hold(token: &str) -> Result<HoldReason, JournalError> {
    match token {
        "no_accepted_opportunity" => Ok(HoldReason::NoAcceptedOpportunity),
        "survival_mode_restriction" => Ok(HoldReason::SurvivalModeRestriction),
        _ => Err(JournalError::UnknownHoldReason),
    }
}

fn parse_money(token: &str) -> Result<Money, JournalError> {
    token
        .parse::<i128>()
        .map(Money::from_micros)
        .map_err(|_| JournalError::InvalidEncoding)
}

fn parse_u64(token: &str) -> Result<u64, JournalError> {
    token.parse::<u64>().map_err(|_| JournalError::InvalidEncoding)
}

fn parse_usize(token: &str) -> Result<usize, JournalError> {
    token.parse::<usize>().map_err(|_| JournalError::InvalidEncoding)
}

fn parse_bps(token: &str) -> Result<BasisPoints, JournalError> {
    let raw = token.parse::<u32>().map_err(|_| JournalError::InvalidEncoding)?;
    BasisPoints::new(raw).map_err(|_| JournalError::InvalidDrawdown)
}

fn next<'a>(parts: &mut std::str::Split<'a, char>) -> Result<&'a str, JournalError> {
    parts.next().ok_or(JournalError::InvalidEncoding)
}

fn money(parts: &mut std::str::Split<'_, char>) -> Result<Money, JournalError> {
    parse_money(next(parts)?)
}

fn u64v(parts: &mut std::str::Split<'_, char>) -> Result<u64, JournalError> {
    parse_u64(next(parts)?)
}

fn usizev(parts: &mut std::str::Split<'_, char>) -> Result<usize, JournalError> {
    parse_usize(next(parts)?)
}

fn bps(parts: &mut std::str::Split<'_, char>) -> Result<BasisPoints, JournalError> {
    parse_bps(next(parts)?)
}

fn costs(parts: &mut std::str::Split<'_, char>) -> Result<OperatingCosts, JournalError> {
    Ok(OperatingCosts {
        energy: money(parts)?,
        compute: money(parts)?,
        network_fees: money(parts)?,
        infrastructure: money(parts)?,
        depreciation: money(parts)?,
        other: money(parts)?,
    })
}

fn encode_decision(decision: &ControlDecision) -> Result<String, JournalError> {
    match decision {
        ControlDecision::Freeze { state } => Ok(format!("freeze|{}", state_token(*state))),
        ControlDecision::Hold {
            state,
            mode,
            reason,
        } => Ok(format!(
            "hold|{}|{}|{}",
            state_token(*state),
            mode_token(*mode),
            hold_token(*reason)
        )),
        ControlDecision::Run {
            opportunity_id,
            state,
            mode,
            expected_net_profit,
            capital_required,
        } => {
            reject_token(opportunity_id.as_str())?;
            Ok(format!(
                "run|{}|{}|{}|{}|{}",
                state_token(*state),
                mode_token(*mode),
                opportunity_id.as_str(),
                expected_net_profit.micros(),
                capital_required.micros()
            ))
        }
    }
}

fn parse_decision(parts: &mut std::str::Split<'_, char>) -> Result<ControlDecision, JournalError> {
    match next(parts)? {
        "freeze" => {
            let state = parse_state(next(parts)?)?;
            if parts.next().is_some() {
                return Err(JournalError::InvalidEncoding);
            }
            Ok(ControlDecision::Freeze { state })
        }
        "hold" => {
            let state = parse_state(next(parts)?)?;
            let mode = parse_mode(next(parts)?)?;
            let reason = parse_hold(next(parts)?)?;
            if parts.next().is_some() {
                return Err(JournalError::InvalidEncoding);
            }
            Ok(ControlDecision::Hold {
                state,
                mode,
                reason,
            })
        }
        "run" => {
            let state = parse_state(next(parts)?)?;
            let mode = parse_mode(next(parts)?)?;
            let opportunity_id =
                OpportunityId::new(next(parts)?.to_owned()).map_err(|_| JournalError::InvalidEncoding)?;
            let expected_net_profit = parse_money(next(parts)?)?;
            let capital_required = parse_money(next(parts)?)?;
            if parts.next().is_some() {
                return Err(JournalError::InvalidEncoding);
            }
            Ok(ControlDecision::Run {
                opportunity_id,
                state,
                mode,
                expected_net_profit,
                capital_required,
            })
        }
        _ => Err(JournalError::UnknownDecisionKind),
    }
}

fn push_money(fields: &mut Vec<String>, value: Money) {
    fields.push(value.micros().to_string());
}

fn push_costs(fields: &mut Vec<String>, costs: OperatingCosts) {
    push_money(fields, costs.energy);
    push_money(fields, costs.compute);
    push_money(fields, costs.network_fees);
    push_money(fields, costs.infrastructure);
    push_money(fields, costs.depreciation);
    push_money(fields, costs.other);
}

/// Deterministic text snapshot of recorded control decisions (roadmap REP1 / REP5).
pub fn encode_decision_journal(ledger: &DecisionLedger) -> Result<String, JournalError> {
    let mut out = String::from(JOURNAL_FORMAT);
    out.push('\n');
    for entry in ledger.entries() {
        let observation = &entry.observation;
        for value in observation
            .evidence
            .iter()
            .chain(observation.diagnostics.iter())
        {
            reject_token(value)?;
        }
        let mut fields = vec![
            entry.sequence.to_string(),
            observation.observed_at_unix_ms.to_string(),
        ];
        push_money(&mut fields, observation.ledger_snapshot.realized_revenue);
        push_costs(&mut fields, observation.ledger_snapshot.costs);
        push_money(&mut fields, observation.ledger_snapshot.external_capital_in);
        push_money(&mut fields, observation.ledger_snapshot.external_capital_out);
        push_money(&mut fields, observation.fitness.realized_revenue);
        push_costs(&mut fields, observation.fitness.realized_costs);
        push_money(&mut fields, observation.fitness.liquid_capital);
        push_money(&mut fields, observation.fitness.survival_reserve);
        fields.push(observation.fitness.drawdown.value().to_string());
        fields.push(
            observation
                .selection_policy
                .economics
                .max_risk
                .value()
                .to_string(),
        );
        push_money(
            &mut fields,
            observation.selection_policy.economics.minimum_net_profit,
        );
        push_money(
            &mut fields,
            observation
                .selection_policy
                .economics
                .minimum_post_action_reserve,
        );
        fields.push(
            observation
                .selection_policy
                .minimum_confidence
                .value()
                .to_string(),
        );
        fields.push(observation.selection_policy.maximum_quote_age_ms.to_string());
        fields.push(observation.selection_policy.minimum_evidence_count.to_string());
        fields.push(observation.selection_policy.capital_charge.value().to_string());
        push_money(&mut fields, observation.survival_policy.critical_reserve);
        push_money(&mut fields, observation.survival_policy.constrained_reserve);
        fields.push(observation.survival_policy.maximum_drawdown.value().to_string());
        push_money(
            &mut fields,
            observation.control_policy.preserve_capital_max_new_capital,
        );
        push_money(
            &mut fields,
            observation.control_policy.essential_only_max_new_capital,
        );
        push_money(
            &mut fields,
            observation.control_policy.essential_only_max_expected_cost,
        );
        push_money(
            &mut fields,
            observation.control_policy.essential_only_minimum_net_profit,
        );
        fields.push(observation.materialized_deployments.to_string());
        fields.push(observation.materialization_rejections.to_string());
        fields.push(observation.accepted_opportunities.to_string());
        fields.push(observation.rejected_opportunities.to_string());
        fields.push(observation.price_source_count.to_string());
        fields.push(observation.network_source_count.to_string());
        fields.push(encode_decision(&observation.decision)?);
        out.push_str("OBS|");
        out.push_str(&fields.join("|"));
        out.push('\n');
        for evidence in &observation.evidence {
            out.push_str(&format!("EVD|{}|{evidence}\n", entry.sequence));
        }
        for diagnostic in &observation.diagnostics {
            out.push_str(&format!("DIA|{}|{diagnostic}\n", entry.sequence));
        }
    }
    Ok(out)
}

pub fn decode_decision_journal(text: &str) -> Result<DecisionLedger, JournalError> {
    let mut lines = text.lines();
    match lines.next() {
        Some(JOURNAL_FORMAT) => {}
        _ => return Err(JournalError::InvalidEncoding),
    }
    let mut ledger = DecisionLedger::default();
    let mut pending: Option<(u64, DecisionObservation)> = None;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split('|');
        match next(&mut parts)? {
            "OBS" => {
                if let Some((_, observation)) = pending.take() {
                    commit_pending(&mut ledger, observation)?;
                }
                let sequence = u64v(&mut parts)?;
                if sequence != ledger.entries().len() as u64 {
                    return Err(JournalError::SequenceMismatch);
                }
                let observed_at_unix_ms = u64v(&mut parts)?;
                let snapshot = LedgerSnapshot {
                    realized_revenue: money(&mut parts)?,
                    costs: costs(&mut parts)?,
                    external_capital_in: money(&mut parts)?,
                    external_capital_out: money(&mut parts)?,
                };
                let fitness = EconomicFitness {
                    realized_revenue: money(&mut parts)?,
                    realized_costs: costs(&mut parts)?,
                    liquid_capital: money(&mut parts)?,
                    survival_reserve: money(&mut parts)?,
                    drawdown: bps(&mut parts)?,
                };
                let selection_policy = SelectionPolicy {
                    economics: OpportunityPolicy {
                        max_risk: bps(&mut parts)?,
                        minimum_net_profit: money(&mut parts)?,
                        minimum_post_action_reserve: money(&mut parts)?,
                    },
                    minimum_confidence: bps(&mut parts)?,
                    maximum_quote_age_ms: u64v(&mut parts)?,
                    minimum_evidence_count: usizev(&mut parts)?,
                    capital_charge: bps(&mut parts)?,
                };
                let survival_policy = SurvivalPolicy::new(
                    money(&mut parts)?,
                    money(&mut parts)?,
                    bps(&mut parts)?,
                )
                .map_err(|_: SurvivalPolicyError| JournalError::InvalidPolicy)?;
                let control_policy = ControlPolicy::new(
                    money(&mut parts)?,
                    money(&mut parts)?,
                    money(&mut parts)?,
                    money(&mut parts)?,
                )
                .map_err(|_: ControlError| JournalError::InvalidPolicy)?;
                let materialized_deployments = usizev(&mut parts)?;
                let materialization_rejections = usizev(&mut parts)?;
                let accepted_opportunities = usizev(&mut parts)?;
                let rejected_opportunities = usizev(&mut parts)?;
                let price_source_count = usizev(&mut parts)?;
                let network_source_count = usizev(&mut parts)?;
                let decision = parse_decision(&mut parts)?;
                pending = Some((
                    sequence,
                    DecisionObservation {
                        observed_at_unix_ms,
                        ledger_snapshot: snapshot,
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
                        evidence: Vec::new(),
                        diagnostics: Vec::new(),
                        decision,
                    },
                ));
            }
            "EVD" => {
                let sequence = u64v(&mut parts)?;
                let value = next(&mut parts)?;
                if parts.next().is_some() {
                    return Err(JournalError::InvalidEncoding);
                }
                reject_token(value)?;
                match pending.as_mut() {
                    Some((pending_sequence, observation)) if *pending_sequence == sequence => {
                        observation.evidence.push(value.to_owned());
                    }
                    _ => return Err(JournalError::SequenceMismatch),
                }
            }
            "DIA" => {
                let sequence = u64v(&mut parts)?;
                let value = next(&mut parts)?;
                if parts.next().is_some() {
                    return Err(JournalError::InvalidEncoding);
                }
                reject_token(value)?;
                match pending.as_mut() {
                    Some((pending_sequence, observation)) if *pending_sequence == sequence => {
                        observation.diagnostics.push(value.to_owned());
                    }
                    _ => return Err(JournalError::SequenceMismatch),
                }
            }
            _ => return Err(JournalError::InvalidEncoding),
        }
    }
    if let Some((_, observation)) = pending.take() {
        commit_pending(&mut ledger, observation)?;
    }
    Ok(ledger)
}

fn commit_pending(
    ledger: &mut DecisionLedger,
    observation: DecisionObservation,
) -> Result<(), JournalError> {
    let observation = DecisionObservation::new(
        observation.observed_at_unix_ms,
        observation.ledger_snapshot,
        observation.fitness,
        observation.selection_policy,
        observation.survival_policy,
        observation.control_policy,
        observation.materialized_deployments,
        observation.materialization_rejections,
        observation.accepted_opportunities,
        observation.rejected_opportunities,
        observation.price_source_count,
        observation.network_source_count,
        observation.evidence,
        observation.diagnostics,
        observation.decision,
    )
    .map_err(JournalError::Observation)?;
    ledger
        .append(observation)
        .map_err(JournalError::Observation)?;
    Ok(())
}

pub fn persist_decision_journal(path: &Path, ledger: &DecisionLedger) -> Result<(), JournalError> {
    let encoded = encode_decision_journal(ledger)?;
    if path.exists() {
        let existing = fs::read_to_string(path).map_err(|_| JournalError::Io)?;
        if !encoded.starts_with(&existing) {
            return Err(JournalError::ExistingJournalConflict);
        }
        if existing == encoded {
            return Ok(());
        }
    }
    fs::write(path, encoded).map_err(|_| JournalError::Io)
}

pub fn read_decision_journal(path: &Path) -> Result<DecisionLedger, JournalError> {
    let text = fs::read_to_string(path).map_err(|_| JournalError::Io)?;
    decode_decision_journal(&text)
}
