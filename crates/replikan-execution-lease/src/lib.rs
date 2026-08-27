#![forbid(unsafe_code)]

use core::fmt;
use std::collections::BTreeSet;

use replikan_control::ControlDecision;
use replikan_decision_ledger::DecisionEntry;
use replikan_opportunities::OpportunityId;
use replikan_resource::{
    AuthorizedResourceInventory, MiningBenchmark, MiningDeploymentTemplate, ResourceId,
    ResourceScope,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LeasePolicy {
    pub maximum_decision_age_ms: u64,
    pub maximum_lease_duration_ms: u64,
}

impl LeasePolicy {
    pub fn new(
        maximum_decision_age_ms: u64,
        maximum_lease_duration_ms: u64,
    ) -> Result<Self, LeaseError> {
        if maximum_decision_age_ms == 0 {
            return Err(LeaseError::ZeroMaximumDecisionAge);
        }
        if maximum_lease_duration_ms == 0 {
            return Err(LeaseError::ZeroMaximumLeaseDuration);
        }
        Ok(Self {
            maximum_decision_age_ms,
            maximum_lease_duration_ms,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionAction {
    ActivateMining,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MiningExecutionLease {
    pub decision_sequence: u64,
    pub opportunity_id: OpportunityId,
    pub resource_id: ResourceId,
    pub action: ExecutionAction,
    pub asset_symbol: String,
    pub algorithm: String,
    pub issued_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub evidence: Vec<String>,
}

impl MiningExecutionLease {
    #[must_use]
    pub const fn is_active_at(&self, now_unix_ms: u64) -> bool {
        self.issued_at_unix_ms <= now_unix_ms && now_unix_ms <= self.valid_until_unix_ms
    }
}

pub fn issue_mining_execution_lease(
    decision: &DecisionEntry,
    inventory: &AuthorizedResourceInventory,
    templates: &[MiningDeploymentTemplate],
    policy: LeasePolicy,
    now_unix_ms: u64,
) -> Result<MiningExecutionLease, LeaseError> {
    let decision_observed_at = decision.observation.observed_at_unix_ms;
    if decision_observed_at > now_unix_ms {
        return Err(LeaseError::DecisionFromFuture);
    }
    let decision_valid_until = decision_observed_at
        .checked_add(policy.maximum_decision_age_ms)
        .ok_or(LeaseError::TimestampOverflow)?;
    if now_unix_ms > decision_valid_until {
        return Err(LeaseError::DecisionStale);
    }

    let opportunity_id = match &decision.observation.decision {
        ControlDecision::Run { opportunity_id, .. } => opportunity_id,
        ControlDecision::Hold { .. } | ControlDecision::Freeze { .. } => {
            return Err(LeaseError::DecisionNotRunnable);
        }
    };

    validate_unique_template_ids(templates)?;
    let template = templates
        .iter()
        .find(|template| template.id.as_str() == opportunity_id.as_str())
        .ok_or_else(|| LeaseError::OpportunityTemplateMissing(opportunity_id.clone()))?;

    if template.observed_at_unix_ms > now_unix_ms {
        return Err(LeaseError::TemplateFromFuture);
    }
    if now_unix_ms > template.valid_until_unix_ms {
        return Err(LeaseError::TemplateExpired);
    }

    let resource = inventory
        .get(&template.resource_id)
        .ok_or_else(|| LeaseError::ResourceMissing(template.resource_id.clone()))?;
    if resource.scope != ResourceScope::LocalMachine {
        return Err(LeaseError::UnsupportedResourceScope);
    }
    if !resource.authorization.is_active_at(now_unix_ms) {
        return Err(LeaseError::AuthorizationInactive);
    }

    let decision_evidence = decision
        .observation
        .evidence
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if !decision_evidence.contains(resource.authorization.evidence.as_str()) {
        return Err(LeaseError::AuthorizationEvidenceMissing);
    }
    if !all_evidence_present(&template.evidence, &decision_evidence) {
        return Err(LeaseError::TemplateEvidenceMissing);
    }

    let active_benchmarks = resource
        .mining_benchmarks
        .iter()
        .filter(|benchmark| {
            benchmark.algorithm == template.algorithm && benchmark.is_active_at(now_unix_ms)
        })
        .collect::<Vec<_>>();
    if active_benchmarks.is_empty() {
        return Err(LeaseError::NoActiveBenchmark);
    }
    let benchmark = active_benchmarks
        .into_iter()
        .filter(|benchmark| all_evidence_present(&benchmark.evidence, &decision_evidence))
        .max_by_key(|benchmark| benchmark.observed_at_unix_ms)
        .ok_or(LeaseError::BenchmarkEvidenceMissing)?;

    issue_lease(
        decision,
        template,
        benchmark,
        resource.authorization.valid_until_unix_ms,
        decision_valid_until,
        policy,
        now_unix_ms,
    )
}

fn validate_unique_template_ids(templates: &[MiningDeploymentTemplate]) -> Result<(), LeaseError> {
    let mut ids = BTreeSet::new();
    for template in templates {
        if !ids.insert(template.id.as_str()) {
            return Err(LeaseError::DuplicateOpportunityTemplate(
                template.id.clone(),
            ));
        }
    }
    Ok(())
}

fn all_evidence_present(
    evidence: &[replikan_opportunities::EvidenceRef],
    available: &BTreeSet<&str>,
) -> bool {
    evidence
        .iter()
        .all(|reference| available.contains(reference.as_str()))
}

#[allow(clippy::too_many_arguments)]
fn issue_lease(
    decision: &DecisionEntry,
    template: &MiningDeploymentTemplate,
    benchmark: &MiningBenchmark,
    authorization_valid_until_unix_ms: u64,
    decision_valid_until_unix_ms: u64,
    policy: LeasePolicy,
    now_unix_ms: u64,
) -> Result<MiningExecutionLease, LeaseError> {
    let policy_valid_until = now_unix_ms
        .checked_add(policy.maximum_lease_duration_ms)
        .ok_or(LeaseError::TimestampOverflow)?;
    let valid_until_unix_ms = policy_valid_until
        .min(decision_valid_until_unix_ms)
        .min(authorization_valid_until_unix_ms)
        .min(template.valid_until_unix_ms)
        .min(benchmark.valid_until_unix_ms);
    if valid_until_unix_ms <= now_unix_ms {
        return Err(LeaseError::NoCommonValidityWindow);
    }

    let mut evidence = decision.observation.evidence.clone();
    evidence.sort();
    evidence.dedup();

    Ok(MiningExecutionLease {
        decision_sequence: decision.sequence,
        opportunity_id: template.id.clone(),
        resource_id: template.resource_id.clone(),
        action: ExecutionAction::ActivateMining,
        asset_symbol: template.asset_symbol.clone(),
        algorithm: template.algorithm.clone(),
        issued_at_unix_ms: now_unix_ms,
        valid_until_unix_ms,
        evidence,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeaseError {
    ZeroMaximumDecisionAge,
    ZeroMaximumLeaseDuration,
    DecisionFromFuture,
    DecisionStale,
    DecisionNotRunnable,
    DuplicateOpportunityTemplate(OpportunityId),
    OpportunityTemplateMissing(OpportunityId),
    TemplateFromFuture,
    TemplateExpired,
    ResourceMissing(ResourceId),
    UnsupportedResourceScope,
    AuthorizationInactive,
    AuthorizationEvidenceMissing,
    TemplateEvidenceMissing,
    NoActiveBenchmark,
    BenchmarkEvidenceMissing,
    TimestampOverflow,
    NoCommonValidityWindow,
}

impl fmt::Display for LeaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroMaximumDecisionAge => {
                write!(f, "maximum decision age must be greater than zero")
            }
            Self::ZeroMaximumLeaseDuration => {
                write!(f, "maximum lease duration must be greater than zero")
            }
            Self::DecisionFromFuture => write!(f, "execution decision is from the future"),
            Self::DecisionStale => write!(f, "execution decision is stale"),
            Self::DecisionNotRunnable => write!(f, "only Run decisions can issue execution leases"),
            Self::DuplicateOpportunityTemplate(id) => {
                write!(f, "duplicate opportunity template: {}", id.as_str())
            }
            Self::OpportunityTemplateMissing(id) => {
                write!(f, "missing template for opportunity: {}", id.as_str())
            }
            Self::TemplateFromFuture => write!(f, "deployment template is from the future"),
            Self::TemplateExpired => write!(f, "deployment template is expired"),
            Self::ResourceMissing(id) => {
                write!(f, "authorized resource is missing: {}", id.as_str())
            }
            Self::UnsupportedResourceScope => write!(f, "resource scope is not executable locally"),
            Self::AuthorizationInactive => write!(f, "resource authorization is not active"),
            Self::AuthorizationEvidenceMissing => {
                write!(
                    f,
                    "decision does not contain resource authorization evidence"
                )
            }
            Self::TemplateEvidenceMissing => {
                write!(f, "decision does not contain deployment template evidence")
            }
            Self::NoActiveBenchmark => write!(f, "no active benchmark matches the algorithm"),
            Self::BenchmarkEvidenceMissing => {
                write!(
                    f,
                    "decision does not contain evidence for an active benchmark"
                )
            }
            Self::TimestampOverflow => write!(f, "execution lease timestamp overflow"),
            Self::NoCommonValidityWindow => {
                write!(f, "execution lease has no common validity window")
            }
        }
    }
}

impl std::error::Error for LeaseError {}

#[cfg(test)]
mod tests {
    use super::*;
    use replikan_control::{ControlPolicy, HoldReason};
    use replikan_core::{BasisPoints, Money};
    use replikan_decision_ledger::DecisionObservation;
    use replikan_economics::{EconomicFitness, OperatingCosts, OpportunityPolicy};
    use replikan_ledger::LedgerSnapshot;
    use replikan_opportunities::{EvidenceRef, SelectionPolicy};
    use replikan_resource::{AuthorizationGrant, AuthorizedResource, ResourceKind};
    use replikan_survival::{SpendingMode, SurvivalPolicy, SurvivalState};

    const NOW: u64 = 1_000_000;

    fn bps(value: u32) -> BasisPoints {
        match BasisPoints::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid basis points: {error}"),
        }
    }

    fn evidence(value: &str) -> EvidenceRef {
        match EvidenceRef::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid evidence: {error}"),
        }
    }

    fn opportunity_id(value: &str) -> OpportunityId {
        match OpportunityId::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid opportunity id: {error}"),
        }
    }

    fn resource_id(value: &str) -> ResourceId {
        match ResourceId::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid resource id: {error}"),
        }
    }

    fn selection_policy() -> SelectionPolicy {
        SelectionPolicy {
            economics: OpportunityPolicy {
                max_risk: bps(2_000),
                minimum_net_profit: Money::from_micros(1_000_000),
                minimum_post_action_reserve: Money::from_micros(20_000_000),
            },
            minimum_confidence: bps(7_000),
            maximum_quote_age_ms: 60_000,
            minimum_evidence_count: 1,
            capital_charge: bps(100),
        }
    }

    fn survival_policy() -> SurvivalPolicy {
        SurvivalPolicy {
            critical_reserve: Money::from_micros(10_000_000),
            constrained_reserve: Money::from_micros(40_000_000),
            maximum_drawdown: bps(2_000),
        }
    }

    fn control_policy() -> ControlPolicy {
        match ControlPolicy::new(
            Money::ZERO,
            Money::ZERO,
            Money::from_micros(50_000_000),
            Money::from_micros(1_000_000),
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid control policy: {error}"),
        }
    }

    fn fitness() -> EconomicFitness {
        EconomicFitness {
            realized_revenue: Money::from_micros(20_000_000),
            realized_costs: OperatingCosts::default(),
            liquid_capital: Money::from_micros(100_000_000),
            survival_reserve: Money::from_micros(20_000_000),
            drawdown: bps(500),
        }
    }

    fn run_entry(observed_at_unix_ms: u64, evidence_values: Vec<&str>) -> DecisionEntry {
        let observation = match DecisionObservation::new(
            observed_at_unix_ms,
            LedgerSnapshot::default(),
            fitness(),
            selection_policy(),
            survival_policy(),
            control_policy(),
            1,
            0,
            1,
            0,
            2,
            2,
            evidence_values.into_iter().map(str::to_owned).collect(),
            Vec::new(),
            ControlDecision::Run {
                opportunity_id: opportunity_id("btc:asic-0"),
                state: SurvivalState::Healthy,
                mode: SpendingMode::Normal,
                expected_net_profit: Money::from_micros(10_000_000),
                capital_required: Money::ZERO,
            },
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid decision observation: {error}"),
        };
        DecisionEntry {
            sequence: 7,
            observation,
        }
    }

    fn hold_entry() -> DecisionEntry {
        let mut entry = run_entry(
            NOW - 100,
            vec!["authorization:owner", "benchmark:asic", "deployment:asic"],
        );
        entry.observation.decision = ControlDecision::Hold {
            state: SurvivalState::Healthy,
            mode: SpendingMode::Normal,
            reason: HoldReason::NoAcceptedOpportunity,
        };
        entry
    }

    fn benchmark(valid_until: u64) -> MiningBenchmark {
        match MiningBenchmark::new(
            "sha256d",
            1_000_000_000_000_000,
            3_000,
            NOW - 500,
            valid_until,
            bps(9_500),
            vec![evidence("benchmark:asic")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid benchmark: {error}"),
        }
    }

    fn inventory(
        authorization_valid_until: u64,
        benchmark_valid_until: u64,
    ) -> AuthorizedResourceInventory {
        let authorization = match AuthorizationGrant::new(
            evidence("authorization:owner"),
            NOW - 1_000,
            authorization_valid_until,
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid authorization: {error}"),
        };
        match AuthorizedResourceInventory::new(vec![AuthorizedResource::local(
            resource_id("local:asic-0"),
            ResourceKind::Asic,
            authorization,
            vec![benchmark(benchmark_valid_until)],
        )]) {
            Ok(value) => value,
            Err(error) => unreachable!("valid inventory: {error}"),
        }
    }

    fn template(valid_until: u64) -> MiningDeploymentTemplate {
        match MiningDeploymentTemplate::new(
            opportunity_id("btc:asic-0"),
            resource_id("local:asic-0"),
            "BTC",
            "sha256d",
            100_000_000,
            bps(100),
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            bps(500),
            NOW - 500,
            valid_until,
            bps(9_500),
            vec![evidence("deployment:asic")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid deployment template: {error}"),
        }
    }

    fn policy() -> LeasePolicy {
        match LeasePolicy::new(1_000, 500) {
            Ok(value) => value,
            Err(error) => unreachable!("valid lease policy: {error}"),
        }
    }

    #[test]
    fn run_decision_issues_bounded_local_mining_lease() {
        let entry = run_entry(
            NOW - 100,
            vec![
                "price:coinbase",
                "authorization:owner",
                "benchmark:asic",
                "deployment:asic",
            ],
        );
        let lease = issue_mining_execution_lease(
            &entry,
            &inventory(NOW + 400, NOW + 300),
            &[template(NOW + 600)],
            policy(),
            NOW,
        );
        let lease = match lease {
            Ok(value) => value,
            Err(error) => unreachable!("valid execution lease: {error}"),
        };

        assert_eq!(lease.decision_sequence, 7);
        assert_eq!(lease.opportunity_id.as_str(), "btc:asic-0");
        assert_eq!(lease.resource_id.as_str(), "local:asic-0");
        assert_eq!(lease.action, ExecutionAction::ActivateMining);
        assert_eq!(lease.valid_until_unix_ms, NOW + 300);
        assert!(lease.is_active_at(NOW + 200));
    }

    #[test]
    fn hold_decision_cannot_issue_execution_lease() {
        assert_eq!(
            issue_mining_execution_lease(
                &hold_entry(),
                &inventory(NOW + 1_000, NOW + 1_000),
                &[template(NOW + 1_000)],
                policy(),
                NOW,
            ),
            Err(LeaseError::DecisionNotRunnable)
        );
    }

    #[test]
    fn stale_decision_is_rejected_before_resource_use() {
        let entry = run_entry(
            NOW - 2_000,
            vec!["authorization:owner", "benchmark:asic", "deployment:asic"],
        );
        assert_eq!(
            issue_mining_execution_lease(
                &entry,
                &AuthorizedResourceInventory::default(),
                &[],
                policy(),
                NOW,
            ),
            Err(LeaseError::DecisionStale)
        );
    }

    #[test]
    fn active_resource_without_decision_benchmark_evidence_is_rejected() {
        let entry = run_entry(NOW - 100, vec!["authorization:owner", "deployment:asic"]);
        assert_eq!(
            issue_mining_execution_lease(
                &entry,
                &inventory(NOW + 1_000, NOW + 1_000),
                &[template(NOW + 1_000)],
                policy(),
                NOW,
            ),
            Err(LeaseError::BenchmarkEvidenceMissing)
        );
    }

    #[test]
    fn expired_authorization_cannot_be_revived_by_old_run_decision() {
        let entry = run_entry(
            NOW - 100,
            vec!["authorization:owner", "benchmark:asic", "deployment:asic"],
        );
        assert_eq!(
            issue_mining_execution_lease(
                &entry,
                &inventory(NOW - 1, NOW + 1_000),
                &[template(NOW + 1_000)],
                policy(),
                NOW,
            ),
            Err(LeaseError::AuthorizationInactive)
        );
    }

    #[test]
    fn duplicate_template_ids_fail_closed() {
        let entry = run_entry(
            NOW - 100,
            vec!["authorization:owner", "benchmark:asic", "deployment:asic"],
        );
        let first = template(NOW + 1_000);
        let second = first.clone();
        assert_eq!(
            issue_mining_execution_lease(
                &entry,
                &inventory(NOW + 1_000, NOW + 1_000),
                &[first, second],
                policy(),
                NOW,
            ),
            Err(LeaseError::DuplicateOpportunityTemplate(opportunity_id(
                "btc:asic-0"
            )))
        );
    }
}
