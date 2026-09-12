#![forbid(unsafe_code)]

use core::fmt;
use std::fs;
use std::path::Path;

use replikan_core::{BasisPoints, Money};
use replikan_economics::{EconomicFitness, OperatingCosts};
use replikan_survival::SurvivalState;

use crate::ledger::FitnessPoint;

const ARCHIVE_FORMAT: &str = "REPLIKANS_FITNESS_V1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveError {
    InvalidEncoding,
    SequenceMismatch,
    TimestampRegression,
    UnknownSurvivalState,
    InvalidDrawdown,
    Io,
    ExistingArchiveConflict,
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEncoding => write!(f, "fitness archive encoding is invalid"),
            Self::SequenceMismatch => write!(f, "fitness archive sequence mismatch"),
            Self::TimestampRegression => write!(f, "fitness archive timestamp regression"),
            Self::UnknownSurvivalState => write!(f, "unknown survival state token"),
            Self::InvalidDrawdown => write!(f, "fitness archive drawdown is out of range"),
            Self::Io => write!(f, "fitness archive I/O failed"),
            Self::ExistingArchiveConflict => {
                write!(f, "existing fitness archive conflicts with new points")
            }
        }
    }
}

impl std::error::Error for ArchiveError {}

fn state_token(state: SurvivalState) -> &'static str {
    match state {
        SurvivalState::Healthy => "healthy",
        SurvivalState::Constrained => "constrained",
        SurvivalState::Critical => "critical",
        SurvivalState::Insolvent => "insolvent",
    }
}

fn parse_state(token: &str) -> Result<SurvivalState, ArchiveError> {
    match token {
        "healthy" => Ok(SurvivalState::Healthy),
        "constrained" => Ok(SurvivalState::Constrained),
        "critical" => Ok(SurvivalState::Critical),
        "insolvent" => Ok(SurvivalState::Insolvent),
        _ => Err(ArchiveError::UnknownSurvivalState),
    }
}

fn parse_money(token: &str) -> Result<Money, ArchiveError> {
    token
        .parse::<i128>()
        .map(Money::from_micros)
        .map_err(|_| ArchiveError::InvalidEncoding)
}

fn parse_u64(token: &str) -> Result<u64, ArchiveError> {
    token
        .parse::<u64>()
        .map_err(|_| ArchiveError::InvalidEncoding)
}

/// Deterministic text snapshot of realized fitness points (roadmap REP1 / REP5).
#[must_use]
pub fn encode_fitness_archive(points: &[FitnessPoint]) -> String {
    let mut out = String::from(ARCHIVE_FORMAT);
    out.push('\n');
    for point in points {
        out.push_str(&format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}\n",
            point.sequence,
            point.observed_at_unix_ms,
            point.fitness.realized_revenue.micros(),
            point.fitness.realized_costs.energy.micros(),
            point.fitness.realized_costs.compute.micros(),
            point.fitness.realized_costs.network_fees.micros(),
            point.fitness.realized_costs.infrastructure.micros(),
            point.fitness.realized_costs.depreciation.micros(),
            point.fitness.realized_costs.other.micros(),
            point.fitness.liquid_capital.micros(),
            point.fitness.survival_reserve.micros(),
            point.fitness.drawdown.value(),
            state_token(point.state),
        ));
    }
    out
}

pub fn decode_fitness_archive(text: &str) -> Result<Vec<FitnessPoint>, ArchiveError> {
    let mut lines = text.lines();
    match lines.next() {
        Some(ARCHIVE_FORMAT) => {}
        _ => return Err(ArchiveError::InvalidEncoding),
    }

    let mut points = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split('|');
        let sequence = parse_u64(parts.next().ok_or(ArchiveError::InvalidEncoding)?)?;
        let observed_at_unix_ms = parse_u64(parts.next().ok_or(ArchiveError::InvalidEncoding)?)?;
        let realized_revenue = parse_money(parts.next().ok_or(ArchiveError::InvalidEncoding)?)?;
        let energy = parse_money(parts.next().ok_or(ArchiveError::InvalidEncoding)?)?;
        let compute = parse_money(parts.next().ok_or(ArchiveError::InvalidEncoding)?)?;
        let network_fees = parse_money(parts.next().ok_or(ArchiveError::InvalidEncoding)?)?;
        let infrastructure = parse_money(parts.next().ok_or(ArchiveError::InvalidEncoding)?)?;
        let depreciation = parse_money(parts.next().ok_or(ArchiveError::InvalidEncoding)?)?;
        let other = parse_money(parts.next().ok_or(ArchiveError::InvalidEncoding)?)?;
        let liquid_capital = parse_money(parts.next().ok_or(ArchiveError::InvalidEncoding)?)?;
        let survival_reserve = parse_money(parts.next().ok_or(ArchiveError::InvalidEncoding)?)?;
        let drawdown_raw = parts
            .next()
            .ok_or(ArchiveError::InvalidEncoding)?
            .parse::<u32>()
            .map_err(|_| ArchiveError::InvalidEncoding)?;
        let drawdown = BasisPoints::new(drawdown_raw).map_err(|_| ArchiveError::InvalidDrawdown)?;
        let state = parse_state(parts.next().ok_or(ArchiveError::InvalidEncoding)?)?;
        if parts.next().is_some() {
            return Err(ArchiveError::InvalidEncoding);
        }
        if sequence != points.len() as u64 {
            return Err(ArchiveError::SequenceMismatch);
        }
        if let Some(previous) = points.last() {
            if observed_at_unix_ms < previous.observed_at_unix_ms {
                return Err(ArchiveError::TimestampRegression);
            }
        }
        points.push(FitnessPoint {
            sequence,
            observed_at_unix_ms,
            fitness: EconomicFitness {
                realized_revenue,
                realized_costs: OperatingCosts {
                    energy,
                    compute,
                    network_fees,
                    infrastructure,
                    depreciation,
                    other,
                },
                liquid_capital,
                survival_reserve,
                drawdown,
            },
            state,
        });
    }
    Ok(points)
}

/// Create or extend an archive. Existing bytes must be a prefix of the new series.
pub fn persist_fitness_archive(path: &Path, points: &[FitnessPoint]) -> Result<(), ArchiveError> {
    if path.exists() {
        let existing = read_fitness_archive(path)?;
        if existing.len() > points.len() {
            return Err(ArchiveError::ExistingArchiveConflict);
        }
        if existing != points[..existing.len()] {
            return Err(ArchiveError::ExistingArchiveConflict);
        }
        if existing.len() == points.len() {
            return Ok(());
        }
    }
    fs::write(path, encode_fitness_archive(points)).map_err(|_| ArchiveError::Io)
}

pub fn read_fitness_archive(path: &Path) -> Result<Vec<FitnessPoint>, ArchiveError> {
    let text = fs::read_to_string(path).map_err(|_| ArchiveError::Io)?;
    decode_fitness_archive(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn bps(value: u32) -> BasisPoints {
        match BasisPoints::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid basis points: {error}"),
        }
    }

    fn point(sequence: u64, at: u64, profitable: bool, state: SurvivalState) -> FitnessPoint {
        let revenue = if profitable {
            Money::from_micros(20_000_000)
        } else {
            Money::from_micros(1)
        };
        FitnessPoint {
            sequence,
            observed_at_unix_ms: at,
            fitness: EconomicFitness {
                realized_revenue: revenue,
                realized_costs: OperatingCosts {
                    energy: Money::from_micros(4_000_000),
                    compute: Money::from_micros(1_000_000),
                    network_fees: Money::from_micros(500_000),
                    infrastructure: Money::from_micros(2_000_000),
                    depreciation: Money::from_micros(1_000_000),
                    other: Money::ZERO,
                },
                liquid_capital: Money::from_micros(80_000_000),
                survival_reserve: Money::from_micros(40_000_000),
                drawdown: bps(500),
            },
            state,
        }
    }

    #[test]
    fn encode_decode_preserves_points_and_profitability() {
        let original = vec![
            point(0, 1_000, true, SurvivalState::Healthy),
            point(1, 2_000, true, SurvivalState::Healthy),
        ];
        let restored = match decode_fitness_archive(&encode_fitness_archive(&original)) {
            Ok(value) => value,
            Err(error) => unreachable!("round-trip: {error}"),
        };
        assert_eq!(restored, original);
        assert!(restored[0].fitness.is_profitable());
    }

    #[test]
    fn decode_rejects_gaps_unknown_state_and_bad_header() {
        assert_eq!(
            decode_fitness_archive("NOT_A_ARCHIVE\n").err(),
            Some(ArchiveError::InvalidEncoding)
        );
        assert_eq!(
            decode_fitness_archive("REPLIKANS_FITNESS_V1\n1|1000|1|0|0|0|0|0|0|1|1|0|healthy\n")
                .err(),
            Some(ArchiveError::SequenceMismatch)
        );
        assert_eq!(
            decode_fitness_archive("REPLIKANS_FITNESS_V1\n0|1000|1|0|0|0|0|0|0|1|1|0|unknown\n")
                .err(),
            Some(ArchiveError::UnknownSurvivalState)
        );
        assert_eq!(
            decode_fitness_archive(
                "REPLIKANS_FITNESS_V1\n0|2000|1|0|0|0|0|0|0|1|1|0|healthy\n1|1000|1|0|0|0|0|0|0|1|1|0|healthy\n",
            )
            .err(),
            Some(ArchiveError::TimestampRegression)
        );
    }

    #[test]
    fn persist_is_append_only_and_rejects_rewrites() {
        let stamp = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(value) => value.as_nanos(),
            Err(_) => 1,
        };
        let path = std::env::temp_dir().join(format!("replikans-fitness-{stamp}.txt"));
        let first = vec![point(0, 1_000, true, SurvivalState::Healthy)];
        let second = vec![
            point(0, 1_000, true, SurvivalState::Healthy),
            point(1, 2_000, false, SurvivalState::Constrained),
        ];
        assert!(persist_fitness_archive(&path, &first).is_ok());
        assert!(persist_fitness_archive(&path, &second).is_ok());
        let loaded = match read_fitness_archive(&path) {
            Ok(value) => value,
            Err(error) => unreachable!("read: {error}"),
        };
        assert_eq!(loaded, second);
        let conflicting = vec![point(0, 9_000, false, SurvivalState::Insolvent)];
        assert_eq!(
            persist_fitness_archive(&path, &conflicting),
            Err(ArchiveError::ExistingArchiveConflict)
        );
        let _ = fs::remove_file(&path);
    }
}
