#![forbid(unsafe_code)]

use core::fmt;
use std::collections::BTreeSet;

use replikan_execution_lease::{ExecutionAction, MiningExecutionLease};
use replikan_opportunities::OpportunityId;
use replikan_resource::ResourceId;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AdapterId(String);

impl AdapterId {
    pub fn new(value: impl Into<String>) -> Result<Self, ExecutorError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ExecutorError::EmptyAdapterId);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterDescriptor {
    pub id: AdapterId,
    pub resource_id: ResourceId,
    pub asset_symbol: String,
    pub algorithm: String,
}

impl AdapterDescriptor {
    pub fn new(
        id: AdapterId,
        resource_id: ResourceId,
        asset_symbol: impl Into<String>,
        algorithm: impl Into<String>,
    ) -> Result<Self, ExecutorError> {
        let asset_symbol = asset_symbol.into();
        let algorithm = algorithm.into();
        if asset_symbol.trim().is_empty() {
            return Err(ExecutorError::EmptyAssetSymbol);
        }
        if algorithm.trim().is_empty() {
            return Err(ExecutorError::EmptyAlgorithm);
        }
        Ok(Self {
            id,
            resource_id,
            asset_symbol,
            algorithm,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MiningActivationRequest {
    pub decision_sequence: u64,
    pub opportunity_id: OpportunityId,
    pub resource_id: ResourceId,
    pub asset_symbol: String,
    pub algorithm: String,
    pub lease_issued_at_unix_ms: u64,
    pub lease_valid_until_unix_ms: u64,
    pub requested_at_unix_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterActivation {
    pub evidence: String,
}

impl AdapterActivation {
    pub fn new(evidence: impl Into<String>) -> Result<Self, ExecutorError> {
        let evidence = evidence.into();
        if evidence.trim().is_empty() {
            return Err(ExecutorError::BlankActivationEvidence);
        }
        Ok(Self { evidence })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterFailure(String);

impl AdapterFailure {
    pub fn new(reason: impl Into<String>) -> Result<Self, ExecutorError> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(ExecutorError::BlankAdapterFailure);
        }
        Ok(Self(reason))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Capability adapter for one explicitly registered local mining activation.
///
/// The interface intentionally contains no executable path, command string,
/// argv, environment map, shell, remote host, wallet, or payout parameter.
pub trait MiningActivationAdapter {
    fn descriptor(&self) -> &AdapterDescriptor;

    fn activate(
        &self,
        request: &MiningActivationRequest,
    ) -> Result<AdapterActivation, AdapterFailure>;
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct BindingKey {
    resource_id: ResourceId,
    asset_symbol: String,
    algorithm: String,
}

impl BindingKey {
    fn from_descriptor(descriptor: &AdapterDescriptor) -> Self {
        Self {
            resource_id: descriptor.resource_id.clone(),
            asset_symbol: descriptor.asset_symbol.clone(),
            algorithm: descriptor.algorithm.clone(),
        }
    }

    fn from_lease(lease: &MiningExecutionLease) -> Self {
        Self {
            resource_id: lease.resource_id.clone(),
            asset_symbol: lease.asset_symbol.clone(),
            algorithm: lease.algorithm.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct LeaseKey {
    decision_sequence: u64,
    opportunity_id: OpportunityId,
    resource_id: ResourceId,
}

impl LeaseKey {
    fn from_lease(lease: &MiningExecutionLease) -> Self {
        Self {
            decision_sequence: lease.decision_sequence,
            opportunity_id: lease.opportunity_id.clone(),
            resource_id: lease.resource_id.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionReceipt {
    pub adapter_id: AdapterId,
    pub decision_sequence: u64,
    pub opportunity_id: OpportunityId,
    pub resource_id: ResourceId,
    pub activated_at_unix_ms: u64,
    pub lease_valid_until_unix_ms: u64,
    pub evidence: String,
}

pub struct LocalExecutionRegistry<'a> {
    adapters: Vec<&'a dyn MiningActivationAdapter>,
    consumed_leases: BTreeSet<LeaseKey>,
}

impl<'a> LocalExecutionRegistry<'a> {
    pub fn new(adapters: Vec<&'a dyn MiningActivationAdapter>) -> Result<Self, ExecutorError> {
        let mut adapter_ids = BTreeSet::new();
        let mut bindings = BTreeSet::new();

        for adapter in &adapters {
            let descriptor = adapter.descriptor();
            if !adapter_ids.insert(descriptor.id.clone()) {
                return Err(ExecutorError::DuplicateAdapterId(descriptor.id.clone()));
            }
            let binding = BindingKey::from_descriptor(descriptor);
            if !bindings.insert(binding.clone()) {
                return Err(ExecutorError::DuplicateBinding {
                    resource_id: binding.resource_id,
                    asset_symbol: binding.asset_symbol,
                    algorithm: binding.algorithm,
                });
            }
        }

        Ok(Self {
            adapters,
            consumed_leases: BTreeSet::new(),
        })
    }

    #[must_use]
    pub fn adapter_count(&self) -> usize {
        self.adapters.len()
    }

    #[must_use]
    pub fn consumed_lease_count(&self) -> usize {
        self.consumed_leases.len()
    }

    pub fn dispatch(
        &mut self,
        lease: &MiningExecutionLease,
        now_unix_ms: u64,
    ) -> Result<ExecutionReceipt, ExecutorError> {
        if lease.action != ExecutionAction::ActivateMining {
            return Err(ExecutorError::UnsupportedAction);
        }
        if !lease.is_active_at(now_unix_ms) {
            return Err(ExecutorError::LeaseInactive);
        }

        let lease_key = LeaseKey::from_lease(lease);
        if self.consumed_leases.contains(&lease_key) {
            return Err(ExecutorError::LeaseAlreadyConsumed);
        }

        let binding = BindingKey::from_lease(lease);
        let adapter = self
            .adapters
            .iter()
            .find(|adapter| BindingKey::from_descriptor(adapter.descriptor()) == binding)
            .ok_or_else(|| ExecutorError::NoMatchingAdapter {
                resource_id: lease.resource_id.clone(),
                asset_symbol: lease.asset_symbol.clone(),
                algorithm: lease.algorithm.clone(),
            })?;

        let request = MiningActivationRequest {
            decision_sequence: lease.decision_sequence,
            opportunity_id: lease.opportunity_id.clone(),
            resource_id: lease.resource_id.clone(),
            asset_symbol: lease.asset_symbol.clone(),
            algorithm: lease.algorithm.clone(),
            lease_issued_at_unix_ms: lease.issued_at_unix_ms,
            lease_valid_until_unix_ms: lease.valid_until_unix_ms,
            requested_at_unix_ms: now_unix_ms,
        };

        let activation = adapter
            .activate(&request)
            .map_err(|failure| ExecutorError::AdapterFailed {
                adapter_id: adapter.descriptor().id.clone(),
                reason: failure.as_str().to_owned(),
            })?;
        if activation.evidence.trim().is_empty() {
            return Err(ExecutorError::BlankActivationEvidence);
        }

        self.consumed_leases.insert(lease_key);
        Ok(ExecutionReceipt {
            adapter_id: adapter.descriptor().id.clone(),
            decision_sequence: lease.decision_sequence,
            opportunity_id: lease.opportunity_id.clone(),
            resource_id: lease.resource_id.clone(),
            activated_at_unix_ms: now_unix_ms,
            lease_valid_until_unix_ms: lease.valid_until_unix_ms,
            evidence: activation.evidence,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutorError {
    EmptyAdapterId,
    EmptyAssetSymbol,
    EmptyAlgorithm,
    BlankActivationEvidence,
    BlankAdapterFailure,
    DuplicateAdapterId(AdapterId),
    DuplicateBinding {
        resource_id: ResourceId,
        asset_symbol: String,
        algorithm: String,
    },
    UnsupportedAction,
    LeaseInactive,
    LeaseAlreadyConsumed,
    NoMatchingAdapter {
        resource_id: ResourceId,
        asset_symbol: String,
        algorithm: String,
    },
    AdapterFailed {
        adapter_id: AdapterId,
        reason: String,
    },
}

impl fmt::Display for ExecutorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyAdapterId => write!(f, "adapter id cannot be empty"),
            Self::EmptyAssetSymbol => write!(f, "adapter asset symbol cannot be empty"),
            Self::EmptyAlgorithm => write!(f, "adapter algorithm cannot be empty"),
            Self::BlankActivationEvidence => write!(f, "activation evidence cannot be blank"),
            Self::BlankAdapterFailure => write!(f, "adapter failure reason cannot be blank"),
            Self::DuplicateAdapterId(id) => {
                write!(f, "duplicate execution adapter id: {}", id.as_str())
            }
            Self::DuplicateBinding {
                resource_id,
                asset_symbol,
                algorithm,
            } => write!(
                f,
                "duplicate execution binding for {} {asset_symbol} {algorithm}",
                resource_id.as_str()
            ),
            Self::UnsupportedAction => write!(f, "execution action is not supported"),
            Self::LeaseInactive => write!(f, "execution lease is not active"),
            Self::LeaseAlreadyConsumed => write!(f, "execution lease was already consumed"),
            Self::NoMatchingAdapter {
                resource_id,
                asset_symbol,
                algorithm,
            } => write!(
                f,
                "no allowlisted adapter matches {} {asset_symbol} {algorithm}",
                resource_id.as_str()
            ),
            Self::AdapterFailed { adapter_id, reason } => {
                write!(f, "adapter {} failed: {reason}", adapter_id.as_str())
            }
        }
    }
}

impl std::error::Error for ExecutorError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn adapter_id(value: &str) -> AdapterId {
        match AdapterId::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid adapter id: {error}"),
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

    fn descriptor(id: &str, resource: &str, asset: &str, algorithm: &str) -> AdapterDescriptor {
        match AdapterDescriptor::new(
            adapter_id(id),
            resource_id(resource),
            asset,
            algorithm,
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid descriptor: {error}"),
        }
    }

    fn lease(resource: &str, asset: &str, algorithm: &str) -> MiningExecutionLease {
        MiningExecutionLease {
            decision_sequence: 7,
            opportunity_id: opportunity_id("mine:btc:asic-0"),
            resource_id: resource_id(resource),
            action: ExecutionAction::ActivateMining,
            asset_symbol: asset.to_owned(),
            algorithm: algorithm.to_owned(),
            issued_at_unix_ms: 1_000,
            valid_until_unix_ms: 2_000,
            evidence: vec!["decision:7".to_owned()],
        }
    }

    struct FakeAdapter {
        descriptor: AdapterDescriptor,
        calls: Cell<u32>,
        fail: bool,
    }

    impl FakeAdapter {
        fn successful(descriptor: AdapterDescriptor) -> Self {
            Self {
                descriptor,
                calls: Cell::new(0),
                fail: false,
            }
        }

        fn failing(descriptor: AdapterDescriptor) -> Self {
            Self {
                descriptor,
                calls: Cell::new(0),
                fail: true,
            }
        }
    }

    impl MiningActivationAdapter for FakeAdapter {
        fn descriptor(&self) -> &AdapterDescriptor {
            &self.descriptor
        }

        fn activate(
            &self,
            _request: &MiningActivationRequest,
        ) -> Result<AdapterActivation, AdapterFailure> {
            self.calls.set(self.calls.get() + 1);
            if self.fail {
                let failure = match AdapterFailure::new("device refused activation") {
                    Ok(value) => value,
                    Err(error) => unreachable!("valid failure: {error}"),
                };
                return Err(failure);
            }
            match AdapterActivation::new("adapter:activation:receipt-1") {
                Ok(value) => Ok(value),
                Err(error) => unreachable!("valid activation: {error}"),
            }
        }
    }

    #[test]
    fn expired_lease_is_rejected_without_calling_adapter() {
        let adapter = FakeAdapter::successful(descriptor("asic-adapter", "asic-0", "BTC", "sha256d"));
        let mut registry = match LocalExecutionRegistry::new(vec![&adapter]) {
            Ok(value) => value,
            Err(error) => unreachable!("valid registry: {error}"),
        };

        assert_eq!(
            registry.dispatch(&lease("asic-0", "BTC", "sha256d"), 2_001),
            Err(ExecutorError::LeaseInactive)
        );
        assert_eq!(adapter.calls.get(), 0);
    }

    #[test]
    fn exact_binding_is_required_before_adapter_call() {
        let adapter = FakeAdapter::successful(descriptor("asic-adapter", "asic-0", "BTC", "sha256d"));
        let mut registry = match LocalExecutionRegistry::new(vec![&adapter]) {
            Ok(value) => value,
            Err(error) => unreachable!("valid registry: {error}"),
        };

        assert!(matches!(
            registry.dispatch(&lease("asic-0", "BTC", "scrypt"), 1_500),
            Err(ExecutorError::NoMatchingAdapter { .. })
        ));
        assert_eq!(adapter.calls.get(), 0);
    }

    #[test]
    fn duplicate_bindings_fail_closed() {
        let first = FakeAdapter::successful(descriptor("first", "asic-0", "BTC", "sha256d"));
        let second = FakeAdapter::successful(descriptor("second", "asic-0", "BTC", "sha256d"));

        assert!(matches!(
            LocalExecutionRegistry::new(vec![&first, &second]),
            Err(ExecutorError::DuplicateBinding { .. })
        ));
    }

    #[test]
    fn successful_dispatch_returns_receipt_and_consumes_lease_once() {
        let adapter = FakeAdapter::successful(descriptor("asic-adapter", "asic-0", "BTC", "sha256d"));
        let mut registry = match LocalExecutionRegistry::new(vec![&adapter]) {
            Ok(value) => value,
            Err(error) => unreachable!("valid registry: {error}"),
        };
        let lease = lease("asic-0", "BTC", "sha256d");

        let receipt = match registry.dispatch(&lease, 1_500) {
            Ok(value) => value,
            Err(error) => unreachable!("valid dispatch: {error}"),
        };
        assert_eq!(receipt.adapter_id.as_str(), "asic-adapter");
        assert_eq!(receipt.decision_sequence, 7);
        assert_eq!(receipt.evidence, "adapter:activation:receipt-1");
        assert_eq!(registry.consumed_lease_count(), 1);
        assert_eq!(adapter.calls.get(), 1);

        assert_eq!(
            registry.dispatch(&lease, 1_501),
            Err(ExecutorError::LeaseAlreadyConsumed)
        );
        assert_eq!(adapter.calls.get(), 1);
    }

    #[test]
    fn adapter_failure_does_not_consume_lease() {
        let adapter = FakeAdapter::failing(descriptor("asic-adapter", "asic-0", "BTC", "sha256d"));
        let mut registry = match LocalExecutionRegistry::new(vec![&adapter]) {
            Ok(value) => value,
            Err(error) => unreachable!("valid registry: {error}"),
        };
        let lease = lease("asic-0", "BTC", "sha256d");

        assert!(matches!(
            registry.dispatch(&lease, 1_500),
            Err(ExecutorError::AdapterFailed { .. })
        ));
        assert_eq!(registry.consumed_lease_count(), 0);
        assert_eq!(adapter.calls.get(), 1);
    }

    #[test]
    fn duplicate_adapter_ids_fail_closed_even_for_different_bindings() {
        let first = FakeAdapter::successful(descriptor("same", "asic-0", "BTC", "sha256d"));
        let second = FakeAdapter::successful(descriptor("same", "asic-1", "BTC", "sha256d"));

        assert_eq!(
            LocalExecutionRegistry::new(vec![&first, &second]).err(),
            Some(ExecutorError::DuplicateAdapterId(adapter_id("same")))
        );
    }
}
