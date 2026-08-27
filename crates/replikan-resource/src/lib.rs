#![forbid(unsafe_code)]

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use replikan_core::{BasisPoints, Money};
use replikan_mining_market::snapshot_builder::MiningDeploymentProfile;
use replikan_opportunities::{EvidenceRef, OpportunityId};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ResourceId(String);

impl ResourceId {
    pub fn new(value: impl Into<String>) -> Result<Self, ResourceError> {
        let value = value.into();
        if value.trim().is_empty() {
            Err(ResourceError::EmptyResourceId)
        } else {
            Ok(Self(value))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceKind {
    Cpu,
    Gpu,
    Asic,
    Fpga,
}

/// The initial resource boundary is deliberately local-only. Remote resources
/// require a future explicit authorization adapter; this crate never discovers
/// hosts or expands an authorization scope by itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceScope {
    LocalMachine,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationGrant {
    pub evidence: EvidenceRef,
    pub valid_from_unix_ms: u64,
    pub valid_until_unix_ms: u64,
}

impl AuthorizationGrant {
    pub fn new(
        evidence: EvidenceRef,
        valid_from_unix_ms: u64,
        valid_until_unix_ms: u64,
    ) -> Result<Self, ResourceError> {
        if valid_until_unix_ms <= valid_from_unix_ms {
            return Err(ResourceError::InvalidAuthorizationWindow);
        }
        Ok(Self {
            evidence,
            valid_from_unix_ms,
            valid_until_unix_ms,
        })
    }

    #[must_use]
    pub const fn is_active_at(&self, now_unix_ms: u64) -> bool {
        self.valid_from_unix_ms <= now_unix_ms && now_unix_ms <= self.valid_until_unix_ms
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MiningBenchmark {
    pub algorithm: String,
    pub hashrate_units: u128,
    pub power_watts: u64,
    pub observed_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub confidence: BasisPoints,
    pub evidence: Vec<EvidenceRef>,
}

impl MiningBenchmark {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        algorithm: impl Into<String>,
        hashrate_units: u128,
        power_watts: u64,
        observed_at_unix_ms: u64,
        valid_until_unix_ms: u64,
        confidence: BasisPoints,
        evidence: Vec<EvidenceRef>,
    ) -> Result<Self, ResourceError> {
        let algorithm = algorithm.into();
        if algorithm.trim().is_empty() {
            return Err(ResourceError::EmptyAlgorithm);
        }
        if hashrate_units == 0 {
            return Err(ResourceError::ZeroHashrate);
        }
        if power_watts == 0 {
            return Err(ResourceError::ZeroPower);
        }
        if valid_until_unix_ms <= observed_at_unix_ms {
            return Err(ResourceError::InvalidBenchmarkWindow);
        }
        if evidence.is_empty() {
            return Err(ResourceError::MissingBenchmarkEvidence);
        }

        Ok(Self {
            algorithm,
            hashrate_units,
            power_watts,
            observed_at_unix_ms,
            valid_until_unix_ms,
            confidence,
            evidence,
        })
    }

    #[must_use]
    pub const fn is_active_at(&self, now_unix_ms: u64) -> bool {
        self.observed_at_unix_ms <= now_unix_ms && now_unix_ms <= self.valid_until_unix_ms
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedResource {
    pub id: ResourceId,
    pub kind: ResourceKind,
    pub scope: ResourceScope,
    pub authorization: AuthorizationGrant,
    pub mining_benchmarks: Vec<MiningBenchmark>,
}

impl AuthorizedResource {
    #[must_use]
    pub fn local(
        id: ResourceId,
        kind: ResourceKind,
        authorization: AuthorizationGrant,
        mining_benchmarks: Vec<MiningBenchmark>,
    ) -> Self {
        Self {
            id,
            kind,
            scope: ResourceScope::LocalMachine,
            authorization,
            mining_benchmarks,
        }
    }

    #[must_use]
    pub fn active_benchmark(&self, algorithm: &str, now_unix_ms: u64) -> Option<&MiningBenchmark> {
        self.mining_benchmarks
            .iter()
            .filter(|benchmark| {
                benchmark.algorithm == algorithm && benchmark.is_active_at(now_unix_ms)
            })
            .max_by_key(|benchmark| benchmark.observed_at_unix_ms)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AuthorizedResourceInventory {
    resources: BTreeMap<ResourceId, AuthorizedResource>,
}

impl AuthorizedResourceInventory {
    pub fn new(resources: Vec<AuthorizedResource>) -> Result<Self, ResourceError> {
        let mut inventory = Self::default();
        for resource in resources {
            let id = resource.id.clone();
            if inventory.resources.insert(id.clone(), resource).is_some() {
                return Err(ResourceError::DuplicateResourceId(id));
            }
        }
        Ok(inventory)
    }

    #[must_use]
    pub fn get(&self, id: &ResourceId) -> Option<&AuthorizedResource> {
        self.resources.get(id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.resources.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MiningDeploymentTemplate {
    pub id: OpportunityId,
    pub resource_id: ResourceId,
    pub asset_symbol: String,
    pub algorithm: String,
    pub asset_atoms_per_unit: u128,
    pub pool_fee: BasisPoints,
    pub onchain_fee: Money,
    pub compute_cost: Money,
    pub infrastructure_cost: Money,
    pub depreciation_cost: Money,
    pub other_cost: Money,
    pub capital_required: Money,
    pub risk: BasisPoints,
    pub observed_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub confidence: BasisPoints,
    pub evidence: Vec<EvidenceRef>,
}

impl MiningDeploymentTemplate {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: OpportunityId,
        resource_id: ResourceId,
        asset_symbol: impl Into<String>,
        algorithm: impl Into<String>,
        asset_atoms_per_unit: u128,
        pool_fee: BasisPoints,
        onchain_fee: Money,
        compute_cost: Money,
        infrastructure_cost: Money,
        depreciation_cost: Money,
        other_cost: Money,
        capital_required: Money,
        risk: BasisPoints,
        observed_at_unix_ms: u64,
        valid_until_unix_ms: u64,
        confidence: BasisPoints,
        evidence: Vec<EvidenceRef>,
    ) -> Result<Self, ResourceError> {
        let asset_symbol = asset_symbol.into();
        let algorithm = algorithm.into();
        if asset_symbol.trim().is_empty() {
            return Err(ResourceError::EmptyAssetSymbol);
        }
        if algorithm.trim().is_empty() {
            return Err(ResourceError::EmptyAlgorithm);
        }
        if asset_atoms_per_unit == 0 {
            return Err(ResourceError::ZeroAssetScale);
        }
        if valid_until_unix_ms <= observed_at_unix_ms {
            return Err(ResourceError::InvalidTemplateWindow);
        }
        if [
            onchain_fee,
            compute_cost,
            infrastructure_cost,
            depreciation_cost,
            other_cost,
            capital_required,
        ]
        .into_iter()
        .any(Money::is_negative)
        {
            return Err(ResourceError::NegativeCostOrCapital);
        }
        if evidence.is_empty() {
            return Err(ResourceError::MissingTemplateEvidence);
        }

        Ok(Self {
            id,
            resource_id,
            asset_symbol,
            algorithm,
            asset_atoms_per_unit,
            pool_fee,
            onchain_fee,
            compute_cost,
            infrastructure_cost,
            depreciation_cost,
            other_cost,
            capital_required,
            risk,
            observed_at_unix_ms,
            valid_until_unix_ms,
            confidence,
            evidence,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializationFailure {
    pub id: OpportunityId,
    pub reason: MaterializationRejection,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MaterializationRejection {
    UnknownResource(ResourceId),
    AuthorizationInactive,
    TemplateFromFuture,
    TemplateExpired,
    NoActiveBenchmark,
    NoCommonValidityWindow,
    InvalidProfile(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MaterializationReport {
    pub profiles: Vec<MiningDeploymentProfile>,
    pub rejected: Vec<MaterializationFailure>,
}

pub fn materialize_authorized_deployments(
    inventory: &AuthorizedResourceInventory,
    templates: &[MiningDeploymentTemplate],
    now_unix_ms: u64,
) -> Result<MaterializationReport, ResourceError> {
    if templates.is_empty() {
        return Err(ResourceError::NoDeploymentTemplates);
    }

    let mut ids = BTreeSet::new();
    for template in templates {
        if !ids.insert(template.id.as_str().to_owned()) {
            return Err(ResourceError::DuplicateOpportunityId(template.id.clone()));
        }
    }

    let mut report = MaterializationReport::default();
    for template in templates {
        match materialize_one(inventory, template, now_unix_ms) {
            Ok(profile) => report.profiles.push(profile),
            Err(reason) => report.rejected.push(MaterializationFailure {
                id: template.id.clone(),
                reason,
            }),
        }
    }
    Ok(report)
}

fn materialize_one(
    inventory: &AuthorizedResourceInventory,
    template: &MiningDeploymentTemplate,
    now_unix_ms: u64,
) -> Result<MiningDeploymentProfile, MaterializationRejection> {
    let resource = inventory
        .get(&template.resource_id)
        .ok_or_else(|| MaterializationRejection::UnknownResource(template.resource_id.clone()))?;

    if !resource.authorization.is_active_at(now_unix_ms) {
        return Err(MaterializationRejection::AuthorizationInactive);
    }
    if template.observed_at_unix_ms > now_unix_ms {
        return Err(MaterializationRejection::TemplateFromFuture);
    }
    if now_unix_ms > template.valid_until_unix_ms {
        return Err(MaterializationRejection::TemplateExpired);
    }

    let benchmark = resource
        .active_benchmark(&template.algorithm, now_unix_ms)
        .ok_or(MaterializationRejection::NoActiveBenchmark)?;

    let observed_at_unix_ms = template
        .observed_at_unix_ms
        .min(benchmark.observed_at_unix_ms);
    let valid_until_unix_ms = template
        .valid_until_unix_ms
        .min(benchmark.valid_until_unix_ms)
        .min(resource.authorization.valid_until_unix_ms);
    if valid_until_unix_ms <= observed_at_unix_ms {
        return Err(MaterializationRejection::NoCommonValidityWindow);
    }

    let confidence = if template.confidence < benchmark.confidence {
        template.confidence
    } else {
        benchmark.confidence
    };

    let mut evidence = template.evidence.clone();
    evidence.extend(benchmark.evidence.iter().cloned());
    evidence.push(resource.authorization.evidence.clone());

    MiningDeploymentProfile::new(
        template.id.clone(),
        template.asset_symbol.clone(),
        template.algorithm.clone(),
        template.asset_atoms_per_unit,
        benchmark.hashrate_units,
        benchmark.power_watts,
        template.pool_fee,
        template.onchain_fee,
        template.compute_cost,
        template.infrastructure_cost,
        template.depreciation_cost,
        template.other_cost,
        template.capital_required,
        template.risk,
        observed_at_unix_ms,
        valid_until_unix_ms,
        confidence,
        evidence,
    )
    .map_err(|error| MaterializationRejection::InvalidProfile(error.to_string()))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceError {
    EmptyResourceId,
    DuplicateResourceId(ResourceId),
    InvalidAuthorizationWindow,
    EmptyAlgorithm,
    ZeroHashrate,
    ZeroPower,
    InvalidBenchmarkWindow,
    MissingBenchmarkEvidence,
    EmptyAssetSymbol,
    ZeroAssetScale,
    InvalidTemplateWindow,
    NegativeCostOrCapital,
    MissingTemplateEvidence,
    NoDeploymentTemplates,
    DuplicateOpportunityId(OpportunityId),
}

impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyResourceId => write!(f, "resource id cannot be empty"),
            Self::DuplicateResourceId(id) => write!(f, "duplicate resource id: {}", id.as_str()),
            Self::InvalidAuthorizationWindow => {
                write!(f, "resource authorization window is invalid")
            }
            Self::EmptyAlgorithm => write!(f, "mining algorithm cannot be empty"),
            Self::ZeroHashrate => write!(f, "measured hashrate must be greater than zero"),
            Self::ZeroPower => write!(f, "measured power must be greater than zero"),
            Self::InvalidBenchmarkWindow => write!(f, "benchmark validity window is invalid"),
            Self::MissingBenchmarkEvidence => write!(f, "benchmark requires evidence"),
            Self::EmptyAssetSymbol => write!(f, "deployment asset symbol cannot be empty"),
            Self::ZeroAssetScale => write!(f, "asset atoms-per-unit must be greater than zero"),
            Self::InvalidTemplateWindow => {
                write!(f, "deployment template validity window is invalid")
            }
            Self::NegativeCostOrCapital => {
                write!(f, "deployment costs and capital cannot be negative")
            }
            Self::MissingTemplateEvidence => write!(f, "deployment template requires evidence"),
            Self::NoDeploymentTemplates => {
                write!(f, "at least one deployment template is required")
            }
            Self::DuplicateOpportunityId(id) => {
                write!(f, "duplicate deployment opportunity id: {}", id.as_str())
            }
        }
    }
}

impl std::error::Error for ResourceError {}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn resource_id(value: &str) -> ResourceId {
        match ResourceId::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid resource id: {error}"),
        }
    }

    fn opportunity_id(value: &str) -> OpportunityId {
        match OpportunityId::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid opportunity id: {error}"),
        }
    }

    fn grant(valid_until: u64) -> AuthorizationGrant {
        match AuthorizationGrant::new(
            evidence("authorization:local-owner"),
            NOW - 100_000,
            valid_until,
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid authorization: {error}"),
        }
    }

    fn benchmark(hashrate: u128, power_watts: u64, observed_at: u64) -> MiningBenchmark {
        match MiningBenchmark::new(
            "sha256d",
            hashrate,
            power_watts,
            observed_at,
            NOW + 60_000,
            bps(9_000),
            vec![evidence(&format!("benchmark:{hashrate}:{power_watts}"))],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid benchmark: {error}"),
        }
    }

    fn resource(
        authorization: AuthorizationGrant,
        benchmarks: Vec<MiningBenchmark>,
    ) -> AuthorizedResource {
        AuthorizedResource::local(
            resource_id("local:asic-0"),
            ResourceKind::Asic,
            authorization,
            benchmarks,
        )
    }

    fn inventory(
        authorization: AuthorizationGrant,
        benchmarks: Vec<MiningBenchmark>,
    ) -> AuthorizedResourceInventory {
        match AuthorizedResourceInventory::new(vec![resource(authorization, benchmarks)]) {
            Ok(value) => value,
            Err(error) => unreachable!("valid inventory: {error}"),
        }
    }

    fn template(name: &str, algorithm: &str) -> MiningDeploymentTemplate {
        match MiningDeploymentTemplate::new(
            opportunity_id(name),
            resource_id("local:asic-0"),
            "BTC",
            algorithm,
            100_000_000,
            bps(100),
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            bps(500),
            NOW - 5_000,
            NOW + 60_000,
            bps(9_500),
            vec![evidence("deployment:configured-by-owner")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid deployment template: {error}"),
        }
    }

    #[test]
    fn materializes_hashrate_and_power_from_authorized_benchmark() {
        let inventory = inventory(
            grant(NOW + 120_000),
            vec![benchmark(100_000_000_000_000, 3_000, NOW - 10_000)],
        );
        let report = match materialize_authorized_deployments(
            &inventory,
            &[template("btc:asic-0", "sha256d")],
            NOW,
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid materialization: {error}"),
        };

        assert_eq!(report.profiles.len(), 1);
        assert!(report.rejected.is_empty());
        assert_eq!(report.profiles[0].miner_hashrate_units, 100_000_000_000_000);
        assert_eq!(report.profiles[0].power_watts, 3_000);
        assert_eq!(report.profiles[0].evidence.len(), 3);
    }

    #[test]
    fn expired_authorization_cannot_materialize_a_profile() {
        let inventory = inventory(
            grant(NOW - 1),
            vec![benchmark(100_000_000_000_000, 3_000, NOW - 10_000)],
        );
        let report = match materialize_authorized_deployments(
            &inventory,
            &[template("btc:expired", "sha256d")],
            NOW,
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("materialization report should be produced: {error}"),
        };

        assert!(report.profiles.is_empty());
        assert!(matches!(
            report.rejected.as_slice(),
            [failure] if failure.reason == MaterializationRejection::AuthorizationInactive
        ));
    }

    #[test]
    fn latest_active_benchmark_is_selected_deterministically() {
        let inventory = inventory(
            grant(NOW + 120_000),
            vec![
                benchmark(80_000_000_000_000, 3_200, NOW - 20_000),
                benchmark(100_000_000_000_000, 3_000, NOW - 1_000),
            ],
        );
        let report = match materialize_authorized_deployments(
            &inventory,
            &[template("btc:latest", "sha256d")],
            NOW,
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid materialization: {error}"),
        };

        assert_eq!(report.profiles[0].miner_hashrate_units, 100_000_000_000_000);
        assert_eq!(report.profiles[0].power_watts, 3_000);
    }

    #[test]
    fn missing_algorithm_measurement_is_isolated_as_rejection() {
        let inventory = inventory(
            grant(NOW + 120_000),
            vec![benchmark(100_000_000_000_000, 3_000, NOW - 1_000)],
        );
        let report = match materialize_authorized_deployments(
            &inventory,
            &[template("btc:no-benchmark", "other-hash")],
            NOW,
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("materialization report should be produced: {error}"),
        };

        assert!(report.profiles.is_empty());
        assert!(matches!(
            report.rejected.as_slice(),
            [failure] if failure.reason == MaterializationRejection::NoActiveBenchmark
        ));
    }

    #[test]
    fn duplicate_opportunity_ids_fail_before_materialization() {
        let inventory = inventory(
            grant(NOW + 120_000),
            vec![benchmark(100_000_000_000_000, 3_000, NOW - 1_000)],
        );
        let templates = vec![
            template("btc:duplicate", "sha256d"),
            template("btc:duplicate", "sha256d"),
        ];

        assert!(matches!(
            materialize_authorized_deployments(&inventory, &templates, NOW),
            Err(ResourceError::DuplicateOpportunityId(_))
        ));
    }

    #[test]
    fn resource_scope_is_local_only() {
        let resource = resource(
            grant(NOW + 120_000),
            vec![benchmark(100_000_000_000_000, 3_000, NOW - 1_000)],
        );
        assert_eq!(resource.scope, ResourceScope::LocalMachine);
    }
}
