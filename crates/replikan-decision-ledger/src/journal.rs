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
