#![forbid(unsafe_code)]

use replikan_core::{BasisPoints, Money};
use replikan_economics::EconomicFitness;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurvivalPolicy {
    pub critical_reserve: Money,
    pub constrained_reserve: Money,
    pub maximum_drawdown: BasisPoints,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurvivalState {
    Healthy,
    Constrained,
    Critical,
    Insolvent,
}

#[must_use]
pub fn classify(fitness: EconomicFitness, policy: SurvivalPolicy) -> SurvivalState {
    if fitness.liquid_capital < Money::ZERO {
        return SurvivalState::Insolvent;
    }
    if fitness.liquid_capital < policy.critical_reserve
        || fitness.drawdown > policy.maximum_drawdown
    {
        return SurvivalState::Critical;
    }
    if fitness.liquid_capital < policy.constrained_reserve || !fitness.is_reserve_funded() {
        return SurvivalState::Constrained;
    }
    SurvivalState::Healthy
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpendingMode {
    Normal,
    PreserveCapital,
    EssentialOnly,
    Frozen,
}

#[must_use]
pub const fn spending_mode(state: SurvivalState) -> SpendingMode {
    match state {
        SurvivalState::Healthy => SpendingMode::Normal,
        SurvivalState::Constrained => SpendingMode::PreserveCapital,
        SurvivalState::Critical => SpendingMode::EssentialOnly,
        SurvivalState::Insolvent => SpendingMode::Frozen,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use replikan_economics::OperatingCosts;

    fn bps(value: u32) -> BasisPoints {
        match BasisPoints::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid test basis points: {error}"),
        }
    }

    fn fitness(capital: i128, reserve: i128, drawdown: u32) -> EconomicFitness {
        EconomicFitness {
            realized_revenue: Money::from_micros(10_000_000),
            realized_costs: OperatingCosts {
                energy: Money::from_micros(1_000_000),
                compute: Money::from_micros(1_000_000),
                network_fees: Money::ZERO,
                infrastructure: Money::ZERO,
                depreciation: Money::ZERO,
                other: Money::ZERO,
            },
            liquid_capital: Money::from_micros(capital),
            survival_reserve: Money::from_micros(reserve),
            drawdown: bps(drawdown),
        }
    }

    #[test]
    fn healthy_requires_reserve_and_bounded_drawdown() {
        let policy = SurvivalPolicy {
            critical_reserve: Money::from_micros(20_000_000),
            constrained_reserve: Money::from_micros(50_000_000),
            maximum_drawdown: bps(2_000),
        };
        assert_eq!(
            classify(fitness(80_000_000, 50_000_000, 500), policy),
            SurvivalState::Healthy
        );
    }

    #[test]
    fn drawdown_can_force_critical_state_even_with_cash() {
        let policy = SurvivalPolicy {
            critical_reserve: Money::from_micros(20_000_000),
            constrained_reserve: Money::from_micros(50_000_000),
            maximum_drawdown: bps(2_000),
        };
        assert_eq!(
            classify(fitness(80_000_000, 50_000_000, 2_500), policy),
            SurvivalState::Critical
        );
        assert_eq!(
            spending_mode(SurvivalState::Critical),
            SpendingMode::EssentialOnly
        );
    }

    #[test]
    fn negative_capital_freezes_spending() {
        let policy = SurvivalPolicy {
            critical_reserve: Money::from_micros(20_000_000),
            constrained_reserve: Money::from_micros(50_000_000),
            maximum_drawdown: bps(2_000),
        };
        let state = classify(fitness(-1, 50_000_000, 500), policy);
        assert_eq!(state, SurvivalState::Insolvent);
        assert_eq!(spending_mode(state), SpendingMode::Frozen);
    }
}
