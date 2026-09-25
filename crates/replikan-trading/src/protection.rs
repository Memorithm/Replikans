//! Replayable bid-liquidation risk and model-independent paper exits.
use crate::financial::{NonNegativeMoney, Quantity, SignedAmount};
use crate::{Config, Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectionPolicy {
    pub max_net_loss: NonNegativeMoney,
    pub max_drawdown: NonNegativeMoney,
}

impl ProtectionPolicy {
    pub(crate) fn validate(&self, config: &Config) -> Result<()> {
        if config.mission.is_none()
            || config.market_data.is_none()
            || SignedAmount::from(self.max_net_loss).is_zero()
            || SignedAmount::from(self.max_drawdown).is_zero()
        {
            return Err(Error(
                "protection requires a mission, collected quotes and positive loss/drawdown limits"
                    .into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProtectionProgress {
    /// Cumulative liquidation PnL, not a per-lot realized attribution.
    pub liquidation_pnl: Option<SignedAmount>,
    /// Starts at zero, so loss of opening capital also counts as drawdown.
    pub high_water_pnl: SignedAmount,
    pub valued_at_ms: Option<i64>,
    /// Sticky once an observed threshold, mission stop or deadline is reached.
    pub stopped: Option<String>,
}

impl Default for ProtectionProgress {
    fn default() -> Self {
        Self {
            liquidation_pnl: None,
            high_water_pnl: SignedAmount::ZERO,
            valued_at_ms: None,
            stopped: None,
        }
    }
}

impl crate::State {
    pub(crate) fn liquidation_pnl(&self, config: &Config, now: i64) -> Result<SignedAmount> {
        let progress = &self.mission_progress;
        if !progress.quote_accounting_complete || progress.base_inventory.is_negative() {
            return Err(Error(
                "incomplete quote accounting or negative inventory".into(),
            ));
        }
        if progress.base_inventory.is_zero() {
            return Ok(progress.quote_cash_flow);
        }
        let policy = config
            .market_data
            .as_ref()
            .ok_or_else(|| Error("missing market policy".into()))?;
        let quote = self
            .latest_market
            .as_ref()
            .ok_or_else(|| Error("missing market observation".into()))?;
        quote.validate_at(policy, now)?;
        let inventory = Quantity::parse(&progress.base_inventory.as_decimal_string())?;
        progress
            .quote_cash_flow
            .checked_add(SignedAmount::from(quote.bid.checked_notional(inventory)?))?
            .checked_sub(SignedAmount::from(config.paper_quote_fee))
            .map_err(Error::from)
    }

    pub(crate) fn authorize_protection_buy(
        &self,
        config: &Config,
        intent: &crate::Intent,
        now: i64,
        excluding: Option<&str>,
    ) -> Result<()> {
        let Some(policy) = &config.protection else {
            return Ok(());
        };
        let mut projected = self.clone();
        let cost = SignedAmount::from(
            intent
                .reference
                .price
                .checked_notional(intent.request.quantity)?,
        )
        .checked_add(SignedAmount::from(config.paper_quote_fee))?;
        projected.mission_progress.quote_cash_flow = projected
            .mission_progress
            .quote_cash_flow
            .checked_sub(cost)?;
        projected.mission_progress.base_inventory = projected
            .mission_progress
            .base_inventory
            .checked_add(SignedAmount::from(intent.request.quantity))?;
        let mut pnl = projected.liquidation_pnl(config, now)?;
        let quote = self
            .latest_market
            .as_ref()
            .ok_or_else(|| Error("missing collected quote".into()))?;
        // Pending buys can still fill; do not spend the same risk budget twice.
        // Full original reservations are deliberately conservative after partial fills.
        for (id, order) in &self.book.orders {
            if order.status.is_terminal() || excluding == Some(id.as_str()) {
                continue;
            }
            let pending = &self.intents[id];
            if pending.request.side != crate::orders::Side::Buy {
                continue;
            }
            let cost = SignedAmount::from(
                pending
                    .reference
                    .price
                    .checked_notional(pending.request.quantity)?,
            )
            .checked_add(SignedAmount::from(config.paper_quote_fee))?;
            // Never credit hypothetical gains from a pending (possibly stale)
            // reference to finance the risk of a new order.
            let contribution =
                SignedAmount::from(quote.bid.checked_notional(pending.request.quantity)?)
                    .checked_sub(cost)?
                    .min(SignedAmount::ZERO);
            pnl = pnl.checked_add(contribution)?;
        }
        if pnl <= SignedAmount::ZERO.checked_sub(SignedAmount::from(policy.max_net_loss))?
            || self
                .protection_progress
                .high_water_pnl
                .max(pnl)
                .checked_sub(pnl)?
                >= SignedAmount::from(policy.max_drawdown)
        {
            return Err(Error(
                "purchase would breach liquidation loss/drawdown protection after spread and fees"
                    .into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn evaluate_protection(&mut self, config: &Config, now: i64) -> Result<()> {
        let Some(policy) = &config.protection else {
            return Ok(());
        };
        let mission = config
            .mission
            .as_ref()
            .ok_or_else(|| Error("missing mission".into()))?;
        if now >= mission.expires_at_ms {
            self.protection_progress
                .stopped
                .get_or_insert_with(|| "mission_expired".into());
        }
        if let Some(reason) = self.mission_progress.stopped {
            self.protection_progress
                .stopped
                .get_or_insert_with(|| reason.code().into());
        }
        // An absent/stale quote cannot establish an unrealized threshold. The
        // report retains no current valuation; an already-latched stop survives.
        let Ok(pnl) = self.liquidation_pnl(config, now) else {
            self.protection_progress.liquidation_pnl = None;
            self.protection_progress.valued_at_ms = None;
            return Ok(());
        };
        let risk = &mut self.protection_progress;
        risk.liquidation_pnl = Some(pnl);
        risk.valued_at_ms = Some(now);
        risk.high_water_pnl = risk.high_water_pnl.max(pnl);
        let drawdown = risk.high_water_pnl.checked_sub(pnl)?;
        let reason =
            if pnl <= SignedAmount::ZERO.checked_sub(SignedAmount::from(policy.max_net_loss))? {
                Some("net_liquidation_loss_limit")
            } else if drawdown >= SignedAmount::from(policy.max_drawdown) {
                Some("liquidation_drawdown_limit")
            } else if pnl >= SignedAmount::from(mission.target_net_profit) {
                Some("liquidation_target_reached")
            } else {
                None
            };
        if let Some(reason) = reason {
            risk.stopped.get_or_insert_with(|| reason.into());
        }
        Ok(())
    }
}

impl crate::Runtime {
    pub fn protection_enabled(&self) -> bool {
        self.config.protection.is_some()
    }

    pub fn protection_status(&mut self, now: i64) -> Result<serde_json::Value> {
        let transaction = self.connection.transaction()?;
        let state = crate::replay(&transaction, &self.config)?;
        Ok(serde_json::json!({
            "mode":"paper", "policy":self.config.protection,
            "progress":state.protection_progress,
            "current_liquidation_pnl": if self.config.protection.is_some() {state.liquidation_pnl(&self.config, now).ok()} else {None},
            "runtime_journal_hash":state.hash,
            "flat":state.mission_progress.base_inventory.is_zero(),
            "open_orders":state.book.orders.values().filter(|o| !o.status.is_terminal()).count(),
            "valuation_basis":"net quote cash flow + inventory at collected bid - one configured exit fee; top-of-book mark, not guaranteed executable value",
            "operating_costs_included":false
        }))
    }

    /// Host operation: latch risk, settle pending orders, then try one full exit.
    /// Uses normal durable authorization/claim/receipt paths, never an LLM.
    pub fn protect(
        &mut self,
        adapter: &mut dyn crate::VenueAdapter,
        now: i64,
    ) -> Result<serde_json::Value> {
        self.check_adapter(adapter)?;
        if !self.protection_enabled() {
            return Err(Error("protection is not configured".into()));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut state = crate::replay(&transaction, &self.config)?;
        crate::append(
            &transaction,
            &mut state,
            &self.config,
            crate::Record::ProtectionCheck { now_ms: now },
        )?;
        transaction.commit()?;
        if state.protection_progress.stopped.is_none() {
            return self.protection_status(now);
        }
        for (id, order) in &state.book.orders {
            if order.status.is_terminal() {
                continue;
            }
            if !state.dispatched.contains(id) {
                self.abandon(id, "runtime protection stopped exposure", now)?;
            } else {
                // Query first even for apparent resting orders; an ambiguous
                // outcome is never treated as absence or permission to resubmit.
                self.reconcile(id, adapter, now)?;
                let snapshot = self.snapshot()?;
                if snapshot
                    .orders
                    .iter()
                    .any(|o| o.client_order_id == *id && !o.status.is_terminal())
                {
                    self.cancel(id, adapter, now)?;
                }
            }
        }
        let state = {
            let tx = self.connection.transaction()?;
            crate::replay(&tx, &self.config)?
        };
        if state.book.orders.values().any(|o| !o.status.is_terminal()) {
            return Err(Error(
                "protection awaits terminal confirmation of existing orders".into(),
            ));
        }
        let inventory = state.mission_progress.base_inventory;
        if inventory.is_zero() {
            return self.protection_status(now);
        }
        let policy = self
            .config
            .market_data
            .as_ref()
            .ok_or_else(|| Error("missing market policy".into()))?;
        let quote = state
            .latest_market
            .as_ref()
            .ok_or_else(|| Error("protection exit has no quote".into()))?;
        quote.validate_at(policy, now)?;
        let quantity = Quantity::parse(&inventory.as_decimal_string())?;
        let rules = &self.config.instruments[&policy.instrument_id];
        let reference = quote.reference(policy, crate::orders::Side::Sell)?;
        let id = format!("protection-{}", state.hash);
        let intent = crate::Intent {
            market_snapshot_id: Some(quote.id().into()),
            intent_id: id.clone(),
            idempotency_key: id.clone(),
            agent_id: "runtime-protection".into(),
            client_order_id: id.clone(),
            decision_id: id.clone(),
            strategy_version: "runtime-protection-v1".into(),
            evidence_refs: vec![state.hash.clone(), quote.id().into()],
            rationale: state
                .protection_progress
                .stopped
                .clone()
                .unwrap_or_default(),
            created_at_ms: now,
            expires_at_ms: reference.valid_until_ms.min(
                now.checked_add(self.config.max_intent_age_ms)
                    .ok_or_else(|| Error("time overflow".into()))?,
            ),
            request: crate::financial::ExactOrderRequest {
                instrument_id: policy.instrument_id.clone(),
                side: crate::orders::Side::Sell,
                order_type: crate::financial::ExactOrderType::Market,
                quantity,
                tif: crate::orders::TimeInForce::Gtc,
                reduce_only: false,
                post_only: false,
                rules_version: rules.rules_version.clone(),
            },
            reference,
        };
        self.prepare(intent, now)?;
        self.dispatch(&id, adapter, now)?;
        self.protection_status(now)
    }
}
