#![forbid(unsafe_code)]

use core::fmt;
use replikan_core::Money;
use replikan_economics::OperatingCosts;

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
    SequenceOverflow,
    MonetaryOverflow,
}

impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AmountMustBePositive => write!(f, "ledger amounts must be strictly positive"),
            Self::MissingEvidence => write!(f, "ledger entries require evidence"),
            Self::SequenceOverflow => write!(f, "ledger sequence overflow"),
            Self::MonetaryOverflow => write!(f, "ledger monetary overflow"),
        }
    }
}

impl std::error::Error for LedgerError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_funding_never_counts_as_earned_profit() {
        let mut ledger = EconomicLedger::default();
        let result = ledger.append(
            EntryKind::CapitalInjection,
            Money::from_micros(100_000_000),
            "creator funding tx:test",
        );
        assert!(result.is_ok());

        let snapshot = match ledger.snapshot() {
            Ok(value) => value,
            Err(error) => unreachable!("valid ledger snapshot: {error}"),
        };
        assert_eq!(snapshot.realized_revenue, Money::ZERO);
        assert_eq!(snapshot.realized_net_profit(), Money::ZERO);
        assert_eq!(
            snapshot.checked_realized_net_profit(),
            Ok(Money::ZERO)
        );
        assert_eq!(snapshot.liquid_delta(), Money::from_micros(100_000_000));
        assert_eq!(
            snapshot.checked_liquid_delta(),
            Ok(Money::from_micros(100_000_000))
        );
    }

    #[test]
    fn realized_profit_subtracts_operating_costs() {
        let mut ledger = EconomicLedger::default();
        assert!(
            ledger
                .append(
                    EntryKind::EarnedRevenue,
                    Money::from_micros(25_000_000),
                    "pool payout tx:revenue",
                )
                .is_ok()
        );
        assert!(
            ledger
                .append(
                    EntryKind::EnergyCost,
                    Money::from_micros(7_000_000),
                    "meter invoice:energy",
                )
                .is_ok()
        );
        assert!(
            ledger
                .append(
                    EntryKind::NetworkFee,
                    Money::from_micros(500_000),
                    "chain receipt:fee",
                )
                .is_ok()
        );

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
}
