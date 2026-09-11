#![forbid(unsafe_code)]

use core::fmt;
use replikan_core::Money;
use replikan_economics::OperatingCosts;

const LEDGER_FORMAT: &str = "REPLIKANS_LEDGER_V1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    EarnedRevenue,
    EnergyCost,
    ComputeCost,
    NetworkFee,
    InfrastructureCost,
    DepreciationCost,
    OtherCost,
    CapitalInjection,
    CapitalWithdrawal,
}

impl EntryKind {
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::EarnedRevenue => "earned_revenue",
            Self::EnergyCost => "energy_cost",
            Self::ComputeCost => "compute_cost",
            Self::NetworkFee => "network_fee",
            Self::InfrastructureCost => "infrastructure_cost",
            Self::DepreciationCost => "depreciation_cost",
            Self::OtherCost => "other_cost",
            Self::CapitalInjection => "capital_injection",
            Self::CapitalWithdrawal => "capital_withdrawal",
        }
    }

    pub fn parse_token(token: &str) -> Result<Self, LedgerError> {
        match token {
            "earned_revenue" => Ok(Self::EarnedRevenue),
            "energy_cost" => Ok(Self::EnergyCost),
            "compute_cost" => Ok(Self::ComputeCost),
            "network_fee" => Ok(Self::NetworkFee),
            "infrastructure_cost" => Ok(Self::InfrastructureCost),
            "depreciation_cost" => Ok(Self::DepreciationCost),
            "other_cost" => Ok(Self::OtherCost),
            "capital_injection" => Ok(Self::CapitalInjection),
            "capital_withdrawal" => Ok(Self::CapitalWithdrawal),
            _ => Err(LedgerError::UnknownEntryKind),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerEntry {
    pub sequence: u64,
    pub kind: EntryKind,
    pub amount: Money,
    pub evidence: String,
}

#[derive(Clone, Debug, Default)]
pub struct EconomicLedger {
    entries: Vec<LedgerEntry>,
    next_sequence: u64,
}

impl EconomicLedger {
    #[must_use]
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn append(
        &mut self,
        kind: EntryKind,
        amount: Money,
        evidence: impl Into<String>,
    ) -> Result<u64, LedgerError> {
        if !amount.is_positive() {
            return Err(LedgerError::AmountMustBePositive);
        }

        let evidence = evidence.into();
        if evidence.trim().is_empty() {
            return Err(LedgerError::MissingEvidence);
        }
        if evidence.contains('|') || evidence.contains('\n') || evidence.contains('\r') {
            return Err(LedgerError::EvidenceNotEncodable);
        }

        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(LedgerError::SequenceOverflow)?;
        self.entries.push(LedgerEntry {
            sequence,
            kind,
            amount,
            evidence,
        });
        Ok(sequence)
    }

    pub fn snapshot(&self) -> Result<LedgerSnapshot, LedgerError> {
        let mut snapshot = LedgerSnapshot::default();
        for entry in &self.entries {
            match entry.kind {
                EntryKind::EarnedRevenue => {
                    snapshot.realized_revenue = add(snapshot.realized_revenue, entry.amount)?;
                }
                EntryKind::EnergyCost => {
                    snapshot.costs.energy = add(snapshot.costs.energy, entry.amount)?;
                }
                EntryKind::ComputeCost => {
                    snapshot.costs.compute = add(snapshot.costs.compute, entry.amount)?;
                }
                EntryKind::NetworkFee => {
                    snapshot.costs.network_fees = add(snapshot.costs.network_fees, entry.amount)?;
                }
                EntryKind::InfrastructureCost => {
                    snapshot.costs.infrastructure =
                        add(snapshot.costs.infrastructure, entry.amount)?;
                }
                EntryKind::DepreciationCost => {
                    snapshot.costs.depreciation = add(snapshot.costs.depreciation, entry.amount)?;
                }
                EntryKind::OtherCost => {
                    snapshot.costs.other = add(snapshot.costs.other, entry.amount)?;
                }
                EntryKind::CapitalInjection => {
                    snapshot.external_capital_in = add(snapshot.external_capital_in, entry.amount)?;
                }
                EntryKind::CapitalWithdrawal => {
                    snapshot.external_capital_out =
                        add(snapshot.external_capital_out, entry.amount)?;
                }
            }
        }
        Ok(snapshot)
    }

    #[must_use]
    pub fn encode(&self) -> String {
        let mut out = String::from(LEDGER_FORMAT);
        out.push('\n');
        for entry in &self.entries {
            out.push_str(&format!(
                "{}|{}|{}|{}\n",
                entry.sequence,
                entry.kind.as_token(),
                entry.amount.micros(),
                entry.evidence
            ));
        }
        out
    }

    pub fn decode(text: &str) -> Result<Self, LedgerError> {
        let mut lines = text.lines();
        match lines.next() {
            Some(LEDGER_FORMAT) => {}
            _ => return Err(LedgerError::InvalidEncoding),
        }

        let mut ledger = Self::default();
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            let mut parts = line.splitn(4, '|');
            let sequence = parts
                .next()
                .ok_or(LedgerError::InvalidEncoding)?
                .parse::<u64>()
                .map_err(|_| LedgerError::InvalidEncoding)?;
            let kind = EntryKind::parse_token(parts.next().ok_or(LedgerError::InvalidEncoding)?)?;
            let amount = parts
                .next()
                .ok_or(LedgerError::InvalidEncoding)?
                .parse::<i128>()
                .map_err(|_| LedgerError::InvalidEncoding)?;
            let evidence = parts.next().ok_or(LedgerError::InvalidEncoding)?;
            if sequence != ledger.next_sequence {
                return Err(LedgerError::SequenceMismatch);
            }
            ledger.append(kind, Money::from_micros(amount), evidence)?;
        }
        Ok(ledger)
    }
}

fn add(lhs: Money, rhs: Money) -> Result<Money, LedgerError> {
    lhs.checked_add(rhs).ok_or(LedgerError::MonetaryOverflow)
}

fn sub(lhs: Money, rhs: Money) -> Result<Money, LedgerError> {
    lhs.checked_sub(rhs).ok_or(LedgerError::MonetaryOverflow)
}

fn checked_cost_total(costs: OperatingCosts) -> Result<Money, LedgerError> {
    let mut total = Money::ZERO;
    for cost in [
        costs.energy,
        costs.compute,
        costs.network_fees,
        costs.infrastructure,
        costs.depreciation,
        costs.other,
    ] {
        total = add(total, cost)?;
    }
    Ok(total)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LedgerSnapshot {
    pub realized_revenue: Money,
    pub costs: OperatingCosts,
    pub external_capital_in: Money,
    pub external_capital_out: Money,
}

impl LedgerSnapshot {
    pub fn checked_realized_net_profit(self) -> Result<Money, LedgerError> {
        sub(self.realized_revenue, checked_cost_total(self.costs)?)
    }

    pub fn checked_net_external_capital_flow(self) -> Result<Money, LedgerError> {
        sub(self.external_capital_in, self.external_capital_out)
    }

    pub fn checked_liquid_delta(self) -> Result<Money, LedgerError> {
        add(
            self.checked_realized_net_profit()?,
            self.checked_net_external_capital_flow()?,
        )
    }

    #[must_use]
    pub fn realized_net_profit(self) -> Money {
        self.realized_revenue - self.costs.total()
    }

    #[must_use]
    pub fn net_external_capital_flow(self) -> Money {
        self.external_capital_in - self.external_capital_out
    }

    #[must_use]
    pub fn liquid_delta(self) -> Money {
        self.realized_net_profit() + self.net_external_capital_flow()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LedgerError {
    AmountMustBePositive,
    MissingEvidence,
    EvidenceNotEncodable,
    SequenceOverflow,
    SequenceMismatch,
    MonetaryOverflow,
    InvalidEncoding,
    UnknownEntryKind,
}

impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AmountMustBePositive => write!(f, "ledger amounts must be strictly positive"),
            Self::MissingEvidence => write!(f, "ledger entries require evidence"),
            Self::EvidenceNotEncodable => {
                write!(f, "ledger evidence cannot contain '|' or newlines")
            }
            Self::SequenceOverflow => write!(f, "ledger sequence overflow"),
            Self::SequenceMismatch => write!(f, "ledger sequence mismatch"),
            Self::MonetaryOverflow => write!(f, "ledger monetary overflow"),
            Self::InvalidEncoding => write!(f, "ledger encoding is invalid"),
            Self::UnknownEntryKind => write!(f, "unknown ledger entry kind"),
        }
    }
}

impl std::error::Error for LedgerError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> EconomicLedger {
        let mut ledger = EconomicLedger::default();
        assert!(ledger
            .append(
                EntryKind::EarnedRevenue,
                Money::from_micros(9_000_000),
                "rev-1",
            )
            .is_ok());
        assert!(ledger
            .append(
                EntryKind::CapitalInjection,
                Money::from_micros(3_000_000),
                "cap-1",
            )
            .is_ok());
        ledger
    }

    #[test]
    fn external_funding_never_counts_as_earned_profit() {
        let mut ledger = EconomicLedger::default();
        let result = ledger.append(
            EntryKind::CapitalInjection,
            Money::from_micros(100_000_000),
            "creator-funding-tx-test",
        );
        assert!(result.is_ok());
        let snapshot = match ledger.snapshot() {
            Ok(value) => value,
            Err(error) => unreachable!("valid ledger snapshot: {error}"),
        };
        assert_eq!(snapshot.realized_revenue, Money::ZERO);
        assert_eq!(snapshot.realized_net_profit(), Money::ZERO);
        assert_eq!(snapshot.checked_realized_net_profit(), Ok(Money::ZERO));
        assert_eq!(snapshot.liquid_delta(), Money::from_micros(100_000_000));
        assert_eq!(
            snapshot.checked_liquid_delta(),
            Ok(Money::from_micros(100_000_000))
        );
    }

    #[test]
    fn realized_profit_subtracts_operating_costs() {
        let mut ledger = EconomicLedger::default();
        assert!(ledger
            .append(
                EntryKind::EarnedRevenue,
                Money::from_micros(25_000_000),
                "pool-payout-tx-revenue",
            )
            .is_ok());
        assert!(ledger
            .append(
                EntryKind::EnergyCost,
                Money::from_micros(7_000_000),
                "meter-invoice-energy",
            )
            .is_ok());
        assert!(ledger
            .append(
                EntryKind::NetworkFee,
                Money::from_micros(500_000),
                "chain-receipt-fee",
            )
            .is_ok());
        let snapshot = match ledger.snapshot() {
            Ok(value) => value,
            Err(error) => unreachable!("valid ledger snapshot: {error}"),
        };
        assert_eq!(
            snapshot.realized_net_profit(),
            Money::from_micros(17_500_000)
        );
        assert_eq!(
            snapshot.checked_realized_net_profit(),
            Ok(Money::from_micros(17_500_000))
        );
    }

    #[test]
    fn checked_snapshot_arithmetic_rejects_overflow() {
        let snapshot = LedgerSnapshot {
            realized_revenue: Money::from_micros(i128::MAX),
            costs: OperatingCosts::default(),
            external_capital_in: Money::from_micros(1),
            external_capital_out: Money::ZERO,
        };
        assert_eq!(
            snapshot.checked_liquid_delta(),
            Err(LedgerError::MonetaryOverflow)
        );
    }

    #[test]
    fn entries_without_evidence_are_rejected() {
        let mut ledger = EconomicLedger::default();
        assert_eq!(
            ledger.append(
                EntryKind::EarnedRevenue,
                Money::from_micros(1_000_000),
                "   ",
            ),
            Err(LedgerError::MissingEvidence)
        );
        assert!(ledger.entries().is_empty());
    }

    #[test]
    fn encode_decode_preserves_entries_and_profit() {
        let ledger = seeded();
        let restored = match EconomicLedger::decode(&ledger.encode()) {
            Ok(value) => value,
            Err(error) => unreachable!("round-trip: {error}"),
        };
        assert_eq!(restored.entries(), ledger.entries());
        let original = match ledger.snapshot() {
            Ok(value) => value,
            Err(error) => unreachable!("{error}"),
        };
        let decoded = match restored.snapshot() {
            Ok(value) => value,
            Err(error) => unreachable!("{error}"),
        };
        assert_eq!(original, decoded);
        assert_eq!(decoded.realized_net_profit(), Money::from_micros(9_000_000));
        assert_eq!(decoded.external_capital_in, Money::from_micros(3_000_000));
    }

    #[test]
    fn rejects_pipe_in_evidence() {
        let mut ledger = EconomicLedger::default();
        assert_eq!(
            ledger.append(
                EntryKind::EarnedRevenue,
                Money::from_micros(1),
                "ok-evidence",
            ),
            Ok(0)
        );
        assert_eq!(
            ledger.append(EntryKind::EarnedRevenue, Money::from_micros(1), "bad|pipe"),
            Err(LedgerError::EvidenceNotEncodable)
        );
    }

    #[test]
    fn decode_rejects_unknown_header_and_sequence_gaps() {
        assert_eq!(
            EconomicLedger::decode("NOT_A_LEDGER\n").err(),
            Some(LedgerError::InvalidEncoding)
        );
        assert_eq!(
            EconomicLedger::decode("REPLIKANS_LEDGER_V1\n1|earned_revenue|1|gap\n").err(),
            Some(LedgerError::SequenceMismatch)
        );
        assert_eq!(
            EconomicLedger::decode("REPLIKANS_LEDGER_V1\n0|not_a_kind|1|x\n").err(),
            Some(LedgerError::UnknownEntryKind)
        );
    }
}
