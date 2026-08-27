#![forbid(unsafe_code)]

use core::fmt;
use replikan_economics::EconomicFitness;
use replikan_opportunities::{
    EngineError, OpportunityId, OpportunityQuote, OpportunitySource, SelectionPolicy,
    SelectionReport, evaluate_and_rank,
};
use std::collections::BTreeMap;

trait DynOpportunitySource: Send + Sync {
    fn source_id(&self) -> &str;
    fn discover(&self, now_unix_ms: u64) -> Result<Vec<OpportunityQuote>, String>;
}

struct SourceAdapter<T> {
    inner: T,
}

impl<T> DynOpportunitySource for SourceAdapter<T>
where
    T: OpportunitySource + Send + Sync,
    T::Error: fmt::Display,
{
    fn source_id(&self) -> &str {
        self.inner.source_id()
    }

    fn discover(&self, now_unix_ms: u64) -> Result<Vec<OpportunityQuote>, String> {
        self.inner
            .discover(now_unix_ms)
            .map_err(|error| error.to_string())
    }
}

#[derive(Default)]
pub struct OpportunityOrchestrator {
    sources: Vec<Box<dyn DynOpportunitySource>>,
}

impl OpportunityOrchestrator {
    #[must_use]
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    pub fn register<T>(&mut self, source: T) -> Result<(), OrchestratorError>
    where
        T: OpportunitySource + Send + Sync + 'static,
        T::Error: fmt::Display,
    {
        let source_id = source.source_id().trim().to_owned();
        if source_id.is_empty() {
            return Err(OrchestratorError::EmptySourceId);
        }
        if self
            .sources
            .iter()
            .any(|registered| registered.source_id() == source_id)
        {
            return Err(OrchestratorError::DuplicateSourceId(source_id));
        }

        self.sources.push(Box::new(SourceAdapter { inner: source }));
        Ok(())
    }

    pub fn run(
        &self,
        fitness: EconomicFitness,
        policy: SelectionPolicy,
        now_unix_ms: u64,
    ) -> Result<OrchestrationReport, EngineError> {
        let mut grouped: BTreeMap<OpportunityId, Vec<OpportunityQuote>> = BTreeMap::new();
        let mut source_failures = Vec::new();
        let mut protocol_violations = Vec::new();

        for source in &self.sources {
            let source_id = source.source_id();
            match source.discover(now_unix_ms) {
                Ok(quotes) => {
                    for quote in quotes {
                        if quote.source != source_id {
                            protocol_violations.push(SourceProtocolViolation {
                                source_id: source_id.to_owned(),
                                opportunity_id: quote.id,
                                declared_source: quote.source,
                            });
                            continue;
                        }
                        grouped.entry(quote.id.clone()).or_default().push(quote);
                    }
                }
                Err(error) => source_failures.push(SourceFailure {
                    source_id: source_id.to_owned(),
                    error,
                }),
            }
        }

        let mut unique_quotes = Vec::new();
        let mut duplicate_opportunities = Vec::new();

        for (id, mut quotes) in grouped {
            if quotes.len() == 1 {
                if let Some(quote) = quotes.pop() {
                    unique_quotes.push(quote);
                }
                continue;
            }

            let mut sources = quotes
                .iter()
                .map(|quote| quote.source.clone())
                .collect::<Vec<_>>();
            sources.sort();
            sources.dedup();
            duplicate_opportunities.push(DuplicateOpportunity { id, sources });
        }

        let selection = evaluate_and_rank(fitness, unique_quotes, policy, now_unix_ms)?;
        Ok(OrchestrationReport {
            selection,
            source_failures,
            protocol_violations,
            duplicate_opportunities,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFailure {
    pub source_id: String,
    pub error: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceProtocolViolation {
    pub source_id: String,
    pub opportunity_id: OpportunityId,
    pub declared_source: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DuplicateOpportunity {
    pub id: OpportunityId,
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrchestrationReport {
    pub selection: SelectionReport,
    pub source_failures: Vec<SourceFailure>,
    pub protocol_violations: Vec<SourceProtocolViolation>,
    pub duplicate_opportunities: Vec<DuplicateOpportunity>,
}

impl OrchestrationReport {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.source_failures.is_empty()
            && self.protocol_violations.is_empty()
            && self.duplicate_opportunities.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrchestratorError {
    EmptySourceId,
    DuplicateSourceId(String),
}

impl fmt::Display for OrchestratorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySourceId => write!(f, "opportunity source id cannot be empty"),
            Self::DuplicateSourceId(source_id) => {
                write!(f, "duplicate opportunity source id: {source_id}")
            }
        }
    }
}

impl std::error::Error for OrchestratorError {}

#[cfg(test)]
mod tests {
    use super::*;
    use replikan_core::{BasisPoints, Money};
    use replikan_economics::{OperatingCosts, OpportunityPolicy};
    use replikan_opportunities::{EvidenceRef, OpportunityKind, QuoteError};

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct FakeError(&'static str);

    impl fmt::Display for FakeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    #[derive(Clone)]
    struct FakeSource {
        id: String,
        result: Result<Vec<OpportunityQuote>, FakeError>,
    }

    impl OpportunitySource for FakeSource {
        type Error = FakeError;

        fn source_id(&self) -> &str {
            &self.id
        }

        fn discover(
            &self,
            _observed_at_unix_ms: u64,
        ) -> Result<Vec<OpportunityQuote>, Self::Error> {
            self.result.clone()
        }
    }

    fn bps(value: u32) -> BasisPoints {
        match BasisPoints::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid test basis points: {error}"),
        }
    }

    fn id(value: &str) -> OpportunityId {
        match OpportunityId::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid test id: {error}"),
        }
    }

    fn evidence() -> EvidenceRef {
        match EvidenceRef::new("test:evidence") {
            Ok(value) => value,
            Err(error) => unreachable!("valid test evidence: {error}"),
        }
    }

    fn quote(name: &str, source: &str, revenue: i128, cost: i128) -> OpportunityQuote {
        let result = OpportunityQuote::new(
            id(name),
            OpportunityKind::Other,
            source,
            1_000_000,
            1_060_000,
            86_400,
            Money::from_micros(revenue),
            OperatingCosts {
                energy: Money::from_micros(cost),
                compute: Money::ZERO,
                network_fees: Money::ZERO,
                infrastructure: Money::ZERO,
                depreciation: Money::ZERO,
                other: Money::ZERO,
            },
            Money::from_micros(10_000_000),
            bps(500),
            bps(9_000),
            vec![evidence()],
        );
        match result {
            Ok(value) => value,
            Err(error) => match error {
                QuoteError::EmptyOpportunityId
                | QuoteError::EmptySource
                | QuoteError::EmptyEvidence
                | QuoteError::InvalidValidityWindow
                | QuoteError::ZeroHorizon
                | QuoteError::NegativeExpectedRevenue
                | QuoteError::NegativeExpectedCost
                | QuoteError::NegativeCapitalRequired => {
                    unreachable!("valid test quote: {error}")
                }
            },
        }
    }

    fn fitness() -> EconomicFitness {
        EconomicFitness {
            realized_revenue: Money::from_micros(50_000_000),
            realized_costs: OperatingCosts::default(),
            liquid_capital: Money::from_micros(100_000_000),
            survival_reserve: Money::from_micros(20_000_000),
            drawdown: bps(500),
        }
    }

    fn policy() -> SelectionPolicy {
        SelectionPolicy {
            economics: OpportunityPolicy {
                max_risk: bps(2_000),
                minimum_net_profit: Money::from_micros(1_000_000),
                minimum_post_action_reserve: Money::from_micros(20_000_000),
            },
            minimum_confidence: bps(5_000),
            maximum_quote_age_ms: 60_000,
            minimum_evidence_count: 1,
            capital_charge: bps(100),
        }
    }

    #[test]
    fn source_failure_does_not_block_healthy_sources() {
        let mut orchestrator = OpportunityOrchestrator::default();
        assert!(
            orchestrator
                .register(FakeSource {
                    id: "healthy".to_owned(),
                    result: Ok(vec![quote("good", "healthy", 50_000_000, 10_000_000)]),
                })
                .is_ok()
        );
        assert!(
            orchestrator
                .register(FakeSource {
                    id: "broken".to_owned(),
                    result: Err(FakeError("offline")),
                })
                .is_ok()
        );

        let report = match orchestrator.run(fitness(), policy(), 1_000_000) {
            Ok(value) => value,
            Err(error) => unreachable!("valid orchestration: {error}"),
        };
        assert_eq!(report.selection.accepted.len(), 1);
        assert_eq!(report.source_failures.len(), 1);
        assert_eq!(report.source_failures[0].source_id, "broken");
    }

    #[test]
    fn source_identity_mismatch_is_excluded() {
        let mut orchestrator = OpportunityOrchestrator::default();
        assert!(
            orchestrator
                .register(FakeSource {
                    id: "actual".to_owned(),
                    result: Ok(vec![quote(
                        "spoofed",
                        "declared-other",
                        50_000_000,
                        10_000_000
                    )]),
                })
                .is_ok()
        );

        let report = match orchestrator.run(fitness(), policy(), 1_000_000) {
            Ok(value) => value,
            Err(error) => unreachable!("valid orchestration: {error}"),
        };
        assert!(report.selection.accepted.is_empty());
        assert_eq!(report.protocol_violations.len(), 1);
        assert_eq!(report.protocol_violations[0].source_id, "actual");
        assert_eq!(
            report.protocol_violations[0].declared_source,
            "declared-other"
        );
    }

    #[test]
    fn duplicate_opportunity_ids_are_excluded_fail_closed() {
        let mut orchestrator = OpportunityOrchestrator::default();
        for source_id in ["source-a", "source-b"] {
            assert!(
                orchestrator
                    .register(FakeSource {
                        id: source_id.to_owned(),
                        result: Ok(vec![quote(
                            "same-id",
                            source_id,
                            50_000_000,
                            10_000_000
                        )]),
                    })
                    .is_ok()
            );
        }

        let report = match orchestrator.run(fitness(), policy(), 1_000_000) {
            Ok(value) => value,
            Err(error) => unreachable!("valid orchestration: {error}"),
        };
        assert!(report.selection.accepted.is_empty());
        assert_eq!(report.duplicate_opportunities.len(), 1);
        assert_eq!(report.duplicate_opportunities[0].id.as_str(), "same-id");
    }

    #[test]
    fn duplicate_source_registration_is_rejected() {
        let mut orchestrator = OpportunityOrchestrator::default();
        let first = FakeSource {
            id: "source".to_owned(),
            result: Ok(vec![]),
        };
        let second = first.clone();
        assert!(orchestrator.register(first).is_ok());
        assert_eq!(
            orchestrator.register(second),
            Err(OrchestratorError::DuplicateSourceId("source".to_owned()))
        );
    }

    #[test]
    fn selects_best_opportunity_globally_across_sources() {
        let mut orchestrator = OpportunityOrchestrator::default();
        assert!(
            orchestrator
                .register(FakeSource {
                    id: "source-a".to_owned(),
                    result: Ok(vec![quote("lower", "source-a", 30_000_000, 10_000_000)]),
                })
                .is_ok()
        );
        assert!(
            orchestrator
                .register(FakeSource {
                    id: "source-b".to_owned(),
                    result: Ok(vec![quote("higher", "source-b", 60_000_000, 10_000_000)]),
                })
                .is_ok()
        );

        let report = match orchestrator.run(fitness(), policy(), 1_000_000) {
            Ok(value) => value,
            Err(error) => unreachable!("valid orchestration: {error}"),
        };
        assert_eq!(
            report
                .selection
                .best()
                .map(|candidate| candidate.quote.id.as_str()),
            Some("higher")
        );
        assert!(report.is_clean());
    }
}
