//! Operator-bound spot mission and exact closed-cycle accounting.
//!
//! This is not mark-to-market or FIFO accounting. A cycle's result becomes
//! realized only when the entire configured base inventory returns to zero.
//! All amounts are in the one configured instrument's quote asset.

use crate::financial::{NonNegativeMoney, SignedAmount};
use crate::{Config, Error, Result};
use serde::{Deserialize, Serialize};

/// Immutable operator policy, bound to the journal's configuration hash.
/// The model can read this mandate, but cannot approve or revise it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TradingMission {
    pub schema_version: u32,
    pub mission_id: String,
    pub objective: String,
    pub instrument_id: String,
    pub starts_at_ms: i64,
    pub expires_at_ms: i64,
    pub target_net_profit: NonNegativeMoney,
    /// Stop new buys after cumulative fully closed cycles lose this amount.
    /// This does not bound open-position losses or provide a stop-loss order.
    pub max_net_realized_loss: NonNegativeMoney,
    /// Lifetime sum of buy notionals and buy fees, including reservations.
    /// This budget is never replenished by sales or by restarting the process.
    pub max_buy_spend: NonNegativeMoney,
}

impl TradingMission {
    pub(crate) fn validate(&self, config: &Config) -> Result<()> {
        let rules = config.instruments.get(&self.instrument_id).ok_or_else(|| {
            Error("mission instrument is absent from operator configuration".into())
        })?;
        if self.schema_version != 1
            || self.mission_id.trim().is_empty()
            || self.mission_id.len() > 256
            || self.objective.trim().is_empty()
            || self.objective.len() > 16_384
            || self.starts_at_ms < 0
            || self.expires_at_ms <= self.starts_at_ms
            || SignedAmount::from(self.target_net_profit).is_zero()
            || SignedAmount::from(self.max_net_realized_loss).is_zero()
            || SignedAmount::from(self.max_buy_spend).is_zero()
            || config.instruments.len() != 1
            || rules.base_asset == rules.quote_asset
            || config
                .initial_balances
                .iter()
                .any(|(asset, balance)| asset != &rules.quote_asset && !balance.is_zero())
        {
            return Err(Error("mission requires a bounded single-instrument spot mandate and zero opening base inventory".into()));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionStop {
    TargetReached,
    RealizedLossLimit,
    UnsupportedFeeAsset,
    BalanceDeficit,
}

impl MissionStop {
    pub const fn code(self) -> &'static str {
        match self {
            Self::TargetReached => "target_reached",
            Self::RealizedLossLimit => "realized_loss_limit",
            Self::UnsupportedFeeAsset => "unsupported_fee_asset",
            Self::BalanceDeficit => "balance_deficit",
        }
    }
}

/// Derived solely from deduplicated, journaled fills, in local receipt order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MissionProgress {
    /// False after a non-quote fee: quote cash flow then omits that asset's cost.
    pub quote_accounting_complete: bool,
    pub buy_spend: SignedAmount,
    pub quote_cash_flow: SignedAmount,
    pub quote_fees: SignedAmount,
    pub base_inventory: SignedAmount,
    /// Net quote cash flow at the latest return to zero base inventory.
    /// Partial sales are deliberately not labeled realized profit here.
    pub completed_cycle_pnl: SignedAmount,
    pub completed_cycles: u64,
    pub fills: u64,
    /// Sticky across subsequent receipts and restarts.
    pub stopped: Option<MissionStop>,
}

impl Default for MissionProgress {
    fn default() -> Self {
        Self {
            quote_accounting_complete: true,
            buy_spend: SignedAmount::ZERO,
            quote_cash_flow: SignedAmount::ZERO,
            quote_fees: SignedAmount::ZERO,
            base_inventory: SignedAmount::ZERO,
            completed_cycle_pnl: SignedAmount::ZERO,
            completed_cycles: 0,
            fills: 0,
            stopped: None,
        }
    }
}

impl MissionProgress {
    pub(crate) fn observe(
        &mut self,
        mission: &TradingMission,
        quote_delta: SignedAmount,
        buy_spend: SignedAmount,
        quote_fee: SignedAmount,
        inventory: SignedAmount,
        stop: Option<MissionStop>,
    ) -> Result<()> {
        let was_open = !self.base_inventory.is_zero();
        self.quote_cash_flow = self.quote_cash_flow.checked_add(quote_delta)?;
        self.buy_spend = self.buy_spend.checked_add(buy_spend)?;
        self.quote_fees = self.quote_fees.checked_add(quote_fee)?;
        self.base_inventory = inventory;
        self.fills = self
            .fills
            .checked_add(1)
            .ok_or_else(|| Error("fill count overflow".into()))?;
        self.stopped = self.stopped.or(stop);
        if stop == Some(MissionStop::UnsupportedFeeAsset) {
            self.quote_accounting_complete = false;
        }
        if inventory.is_zero() && was_open {
            self.completed_cycles = self
                .completed_cycles
                .checked_add(1)
                .ok_or_else(|| Error("cycle count overflow".into()))?;
            self.completed_cycle_pnl = self.quote_cash_flow;
            if self.completed_cycle_pnl >= SignedAmount::from(mission.target_net_profit) {
                self.stopped = self.stopped.or(Some(MissionStop::TargetReached));
            } else if self.completed_cycle_pnl
                <= SignedAmount::ZERO
                    .checked_sub(SignedAmount::from(mission.max_net_realized_loss))?
            {
                self.stopped = self.stopped.or(Some(MissionStop::RealizedLossLimit));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct MissionReport {
    pub runtime_journal_hash: String,
    pub runtime_journal_sequence: i64,
    pub policy: TradingMission,
    pub quote_asset: String,
    pub progress: MissionProgress,
    pub reserved_buy_spend: SignedAmount,
    pub remaining_buy_spend: SignedAmount,
    /// Contextual gate, not approval of any specific order.
    pub new_buys_blocked_by: Option<String>,
    pub valuation_basis: &'static str,
    pub profitability_guaranteed: bool,
}
