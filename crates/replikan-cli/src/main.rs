#![forbid(unsafe_code)]

use replikan_core::{BasisPoints, Money};
use replikan_economics::{
    EconomicFitness, OperatingCosts, OpportunityEstimate, OpportunityPolicy, evaluate_opportunity,
};
use replikan_ledger::{EconomicLedger, EntryKind};
use replikan_replication::{
    FitnessSample, ReplicationCandidate, ReplicationPolicy, SustainedFitnessPolicy,
    evaluate_replication_with_history,
};
use replikan_survival::{SurvivalPolicy, SurvivalState, classify, spending_mode};

fn main() {
    let code = match run(std::env::args().skip(1).collect()) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    };
    std::process::exit(code);
}

fn run(args: Vec<String>) -> Result<(), String> {
    match args.first().map(String::as_str) {
        None | Some("help") | Some("--help") | Some("-h") => {
            print_help();
            Ok(())
        }
        Some("version") | Some("--version") | Some("-V") => {
            println!("replikans {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("demo") => run_demo(),
        Some("money") => match args.get(1).map(String::as_str) {
            Some(raw) => {
                let value = parse_money(raw)?;
                println!("{} micros={}", value, value.micros());
                Ok(())
            }
            None => Err("usage: replikan money <units.micros>".to_owned()),
        },
        Some(other) => Err(format!("unknown command: {other}")),
    }
}

fn print_help() {
    println!(
        "Replikans — deterministic economic policy CLI\n\n\
         Commands:\n\
           help                 Show this message\n\
           version              Print crate version\n\
           money <amount>       Parse a decimal amount into micros\n\
           demo                 Run built-in fitness, survival and replication scenarios\n\n\
         This binary never loads private keys, seed phrases, or payout destinations.",
    );
}

fn parse_money(raw: &str) -> Result<Money, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty monetary amount".to_owned());
    }
    let negative = trimmed.starts_with('-');
    let unsigned = trimmed
        .strip_prefix('+')
        .or_else(|| trimmed.strip_prefix('-'))
        .unwrap_or(trimmed);
    let mut parts = unsigned.split('.');
    let whole = parts.next().unwrap_or("0");
    let frac = parts.next().unwrap_or("");
    if parts.next().is_some() {
        return Err("invalid monetary amount".to_owned());
    }
    if whole.is_empty() || !whole.chars().all(|ch| ch.is_ascii_digit()) {
        return Err("invalid monetary amount".to_owned());
    }
    if frac.len() > 6 || !frac.chars().all(|ch| ch.is_ascii_digit()) {
        return Err("fraction must be at most 6 digits".to_owned());
    }
    let whole_units: i128 = whole
        .parse()
        .map_err(|_| "integer part overflows i128".to_owned())?;
    let mut frac_buf = frac.to_owned();
    while frac_buf.len() < 6 {
        frac_buf.push('0');
    }
    let frac_units: i128 = if frac_buf.is_empty() {
        0
    } else {
        frac_buf
            .parse()
            .map_err(|_| "fractional part overflows".to_owned())?
    };
    let abs = whole_units
        .checked_mul(1_000_000)
        .and_then(|units| units.checked_add(frac_units))
        .ok_or_else(|| "monetary overflow".to_owned())?;
    let micros = if negative {
        abs.checked_neg()
            .ok_or_else(|| "monetary overflow".to_owned())?
    } else {
        abs
    };
    Ok(Money::from_micros(micros))
}

fn bps(value: u32) -> Result<BasisPoints, String> {
    BasisPoints::new(value).map_err(|error| error.to_string())
}

fn run_demo() -> Result<(), String> {
    let costs = OperatingCosts {
        energy: Money::from_micros(4_000_000),
        compute: Money::from_micros(1_000_000),
        network_fees: Money::from_micros(500_000),
        infrastructure: Money::from_micros(2_000_000),
        depreciation: Money::from_micros(1_000_000),
        other: Money::ZERO,
    };
    let fitness = EconomicFitness {
        realized_revenue: Money::from_micros(20_000_000),
        realized_costs: costs,
        liquid_capital: Money::from_micros(100_000_000),
        survival_reserve: Money::from_micros(40_000_000),
        drawdown: bps(500)?,
    };
    let survival_policy = SurvivalPolicy::new(
        Money::from_micros(20_000_000),
        Money::from_micros(50_000_000),
        bps(2_000)?,
    )
    .map_err(|error| error.to_string())?;
    let state = classify(fitness, survival_policy);

    println!("== economic fitness ==");
    println!("realized_net_profit={}", fitness.realized_net_profit());
    println!("reserve_surplus={}", fitness.reserve_surplus());
    println!(
        "diagnostic_score={}",
        fitness
            .diagnostic_score()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "overflow".to_owned())
    );
    println!("survival_state={state:?}");
    println!("spending_mode={:?}", spending_mode(state));

    let opportunity = OpportunityEstimate {
        expected_revenue: Money::from_micros(15_000_000),
        expected_cost: Money::from_micros(10_000_000),
        capital_required: Money::from_micros(20_000_000),
        risk: bps(500)?,
    };
    let policy = OpportunityPolicy {
        max_risk: bps(1_000)?,
        minimum_net_profit: Money::from_micros(1_000_000),
        minimum_post_action_reserve: Money::from_micros(40_000_000),
    };
    println!(
        "opportunity_decision={:?}",
        evaluate_opportunity(fitness, opportunity, policy)
    );

    let mut ledger = EconomicLedger::default();
    ledger
        .append(
            EntryKind::EarnedRevenue,
            Money::from_micros(20_000_000),
            "demo:pool-payout",
        )
        .map_err(|error| error.to_string())?;
    ledger
        .append(
            EntryKind::EnergyCost,
            Money::from_micros(4_000_000),
            "demo:energy-invoice",
        )
        .map_err(|error| error.to_string())?;
    ledger
        .append(
            EntryKind::CapitalInjection,
            Money::from_micros(10_000_000),
            "demo:external-funding",
        )
        .map_err(|error| error.to_string())?;
    let snapshot = ledger.snapshot().map_err(|error| error.to_string())?;
    println!("== ledger snapshot ==");
    println!("realized_net_profit={}", snapshot.realized_net_profit());
    println!(
        "external_capital_does_not_count_as_profit={}",
        snapshot.realized_net_profit() != snapshot.liquid_delta()
    );

    let child = ReplicationCandidate {
        expected_lifetime_revenue: Money::from_micros(80_000_000),
        expected_lifetime_operating_cost: Money::from_micros(20_000_000),
        replication_cost: Money::from_micros(5_000_000),
        risk_premium: Money::from_micros(5_000_000),
        upfront_capital_required: Money::from_micros(30_000_000),
    };
    let replication_policy = ReplicationPolicy {
        minimum_parent_realized_profit: Money::from_micros(10_000_000),
        minimum_child_expected_net_value: Money::from_micros(5_000_000),
        minimum_post_replication_reserve: Money::from_micros(40_000_000),
    };
    let samples = [
        FitnessSample {
            observed_at_unix_ms: 1_000,
            fitness,
            state: SurvivalState::Healthy,
        },
        FitnessSample {
            observed_at_unix_ms: 2_000,
            fitness,
            state: SurvivalState::Healthy,
        },
        FitnessSample {
            observed_at_unix_ms: 3_000,
            fitness,
            state: SurvivalState::Healthy,
        },
    ];
    let decision = evaluate_replication_with_history(
        fitness,
        state,
        child,
        replication_policy,
        &samples,
        3_000,
        SustainedFitnessPolicy {
            window_ms: 10_000,
            minimum_samples: 3,
            minimum_profitable_samples: 3,
            require_healthy_throughout: true,
        },
    )
    .map_err(|error| error.to_string())?;
    println!("== replication ==");
    println!("decision={decision:?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_money;
    use replikan_core::Money;

    #[test]
    fn parses_signed_fixed_point_amounts() {
        assert_eq!(parse_money("12.5"), Ok(Money::from_micros(12_500_000)));
        assert_eq!(parse_money("-0.000001"), Ok(Money::from_micros(-1)));
        assert!(parse_money("1.1234567").is_err());
        assert!(parse_money("").is_err());
    }
}
