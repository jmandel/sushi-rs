//! Observation-only causal evidence for incremental-build investigation.
//!
//! This crate is deliberately absent from every default feature set. It does
//! not schedule work, carry build inputs, or alter `prepare -> Build ->
//! outputs/render/finalize`. Producers may report what the canonical full build
//! actually read; consumers must fail closed whenever that evidence is not
//! complete.

use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "dependency-observation/v1";

/// Incremental execution is intentionally disabled while the observation
/// corpus still contains conservative or unknown evidence.
pub const INCREMENTAL_EXECUTION_ENABLED: bool = false;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(String);

impl NodeId {
    pub fn new(value: impl Into<String>) -> Result<Self, ObservationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ObservationError::EmptyNodeId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NodeKind {
    Source,
    Declaration,
    Rule,
    Alias,
    RuleSet,
    Lookup,
    LookupNamespace,
    PackageResource,
    CompiledResource,
    SnapshotResource,
    PreparedGuide,
    PreparedResource,
    Artifact,
    Fragment,
    Page,
    RuntimePass,
    FinalOutput,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceLocation {
    pub path: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub label: String,
    pub content_digest: Option<String>,
    pub location: Option<SourceLocation>,
    pub attributes: BTreeMap<String, String>,
}

impl Node {
    pub fn new(id: NodeId, kind: NodeKind, label: impl Into<String>) -> Self {
        Self {
            id,
            kind,
            label: label.into(),
            content_digest: None,
            location: None,
            attributes: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeKind {
    Contains,
    Reads,
    Attempts,
    Candidate,
    Eligible,
    Winner,
    Miss,
    Derives,
    Expands,
    Enumerates,
    PostProcesses,
    Publishes,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EvidenceScope {
    Node(NodeId),
    Namespace(String),
    RenderSemantics(String),
    Build,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Evidence {
    Complete,
    Conservative {
        scope: EvidenceScope,
        reason: String,
    },
    Unknown {
        phase: String,
        reason: String,
        location: Option<SourceLocation>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GlobalChange {
    SourceAdded(String),
    SourceDeleted(String),
    SourceRenamed { from: String, to: String },
    SourceOrder,
    Config,
    PackageClosure,
    Template,
    NegativeNamespace(String),
    UnattributedDiagnostic,
    UnresolvedSnapshot,
    DynamicEnumeration(String),
    OutputCatalog,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RebuildDecision {
    /// Evidence is complete for the named nodes. Execution is nevertheless
    /// disabled by [`INCREMENTAL_EXECUTION_ENABLED`] until differential proof
    /// establishes the complete end-to-end model.
    Exact {
        affected: BTreeSet<NodeId>,
    },
    Conservative {
        scopes: BTreeSet<EvidenceScope>,
    },
    FullBuild {
        reasons: BTreeSet<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservationError {
    EmptyNodeId,
    ConflictingNode(NodeId),
    MissingEdgeEndpoint(NodeId),
}

impl std::fmt::Display for ObservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyNodeId => formatter.write_str("dependency node id is empty"),
            Self::ConflictingNode(id) => {
                write!(
                    formatter,
                    "dependency node {} has conflicting definitions",
                    id.as_str()
                )
            }
            Self::MissingEdgeEndpoint(id) => {
                write!(
                    formatter,
                    "dependency edge endpoint {} is absent",
                    id.as_str()
                )
            }
        }
    }
}

impl std::error::Error for ObservationError {}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObservationGraph {
    nodes: BTreeMap<NodeId, Node>,
    edges: BTreeSet<Edge>,
    evidence: BTreeMap<NodeId, BTreeSet<Evidence>>,
    global_unknowns: BTreeSet<String>,
}

impl ObservationGraph {
    pub fn insert_node(&mut self, node: Node) -> Result<(), ObservationError> {
        if let Some(existing) = self.nodes.get(&node.id) {
            if existing != &node {
                return Err(ObservationError::ConflictingNode(node.id));
            }
            return Ok(());
        }
        self.nodes.insert(node.id.clone(), node);
        Ok(())
    }

    pub fn insert_edge(&mut self, edge: Edge) -> Result<(), ObservationError> {
        for endpoint in [&edge.from, &edge.to] {
            if !self.nodes.contains_key(endpoint) {
                return Err(ObservationError::MissingEdgeEndpoint(endpoint.clone()));
            }
        }
        self.edges.insert(edge);
        Ok(())
    }

    /// Accumulate evidence monotonically. A node may have several independent
    /// complete, conservative, or unknown facts; recording one never erases or
    /// relabels another.
    pub fn record_evidence(
        &mut self,
        subject: NodeId,
        evidence: Evidence,
    ) -> Result<(), ObservationError> {
        if !self.nodes.contains_key(&subject) {
            return Err(ObservationError::MissingEdgeEndpoint(subject));
        }
        self.evidence.entry(subject).or_default().insert(evidence);
        Ok(())
    }

    pub fn record_global_unknown(&mut self, reason: impl Into<String>) {
        self.global_unknowns.insert(reason.into());
    }

    pub fn nodes(&self) -> &BTreeMap<NodeId, Node> {
        &self.nodes
    }

    pub fn edges(&self) -> &BTreeSet<Edge> {
        &self.edges
    }

    pub fn evidence(&self) -> &BTreeMap<NodeId, BTreeSet<Evidence>> {
        &self.evidence
    }

    pub fn assess(
        &self,
        affected: impl IntoIterator<Item = NodeId>,
        global_changes: impl IntoIterator<Item = GlobalChange>,
    ) -> RebuildDecision {
        let affected = affected.into_iter().collect::<BTreeSet<_>>();
        let mut closure = affected.clone();
        let mut frontier = affected.iter().cloned().collect::<Vec<_>>();
        while let Some(consumer) = frontier.pop() {
            for edge in self.edges.iter().filter(|edge| edge.from == consumer) {
                if closure.insert(edge.to.clone()) {
                    frontier.push(edge.to.clone());
                }
            }
        }
        let mut reasons = self.global_unknowns.clone();
        if !INCREMENTAL_EXECUTION_ENABLED {
            reasons.insert("incremental execution is disabled pending differential proof".into());
        }
        for change in global_changes {
            reasons.insert(format!("global invalidation: {change:?}"));
        }
        let mut scopes = BTreeSet::new();
        for subject in &closure {
            match self.evidence.get(subject) {
                Some(evidence) => {
                    for fact in evidence {
                        match fact {
                            Evidence::Complete => {}
                            Evidence::Conservative { scope, .. } => {
                                scopes.insert(scope.clone());
                            }
                            Evidence::Unknown { phase, reason, .. } => {
                                reasons.insert(format!("{phase}: {reason}"));
                            }
                        }
                    }
                }
                None => {
                    reasons.insert(format!("no dependency evidence for {}", subject.as_str()));
                }
            }
        }
        if !reasons.is_empty() {
            RebuildDecision::FullBuild { reasons }
        } else if !scopes.is_empty() {
            RebuildDecision::Conservative { scopes }
        } else {
            RebuildDecision::Exact { affected }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LookupOperation {
    Fhir,
    Metadata,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LookupCandidate {
    /// Exact resolver coordinate owning the candidate, when the package-store
    /// path can be attributed to its cache root.
    pub package: Option<String>,
    /// Full package-relative member path, not merely its basename.
    pub member: Option<String>,
    /// Exact member digest when the candidate is an engine-embedded virtual
    /// definition rather than a member of an authenticated package carrier.
    pub member_digest: Option<String>,
    pub resource_type: String,
    pub id: String,
    pub name: Option<String>,
    pub url: Option<String>,
    pub version: Option<String>,
    pub fish_type: String,
    pub load_sequence: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BodyReadOutcome {
    NotAttempted,
    Ready,
    MissingOrInvalid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageLookupObservation {
    pub sequence: u64,
    pub operation: LookupOperation,
    pub query: String,
    pub requested_types: Vec<String>,
    /// Union of id/name/url matches before version/type filtering.
    pub candidates: Vec<LookupCandidate>,
    /// Candidates remaining after exact version and fish-type filtering.
    pub eligible: Vec<LookupCandidate>,
    pub winner: Option<LookupCandidate>,
    pub body_read: BodyReadOutcome,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageLookupTrace {
    pub observations: Vec<PackageLookupObservation>,
    /// Exact sum of the retained `Vec` and `String` capacities owned by
    /// `observations`, including the outer observation vector but excluding
    /// allocator bookkeeping and this trace's inline fields. Producers enforce
    /// their hard bound against this value after every admitted observation.
    pub retained_bytes: usize,
    /// The observer deliberately stopped retaining individual lookups at its
    /// memory bound. Consumers must treat the missing suffix as Unknown.
    pub overflowed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> NodeId {
        NodeId::new(value).unwrap()
    }

    #[test]
    fn evidence_only_moves_toward_more_conservative() {
        let mut graph = ObservationGraph::default();
        graph
            .insert_node(Node::new(id("page:index"), NodeKind::Page, "index.html"))
            .unwrap();
        graph
            .record_evidence(
                id("page:index"),
                Evidence::Unknown {
                    phase: "post-pass".into(),
                    reason: "runtime reads are not observed".into(),
                    location: None,
                },
            )
            .unwrap();
        graph
            .record_evidence(id("page:index"), Evidence::Complete)
            .unwrap();
        assert!(matches!(
            graph.assess([id("page:index")], []),
            RebuildDecision::FullBuild { .. }
        ));
    }

    #[test]
    fn independent_unknown_facts_accumulate_without_becoming_conflicts() {
        let mut graph = ObservationGraph::default();
        graph
            .insert_node(Node::new(id("page:index"), NodeKind::Page, "index.html"))
            .unwrap();
        for (phase, reason) in [
            ("catalog", "namespace enumeration is incomplete"),
            ("render", "fragment internals are incomplete"),
        ] {
            graph
                .record_evidence(
                    id("page:index"),
                    Evidence::Unknown {
                        phase: phase.into(),
                        reason: reason.into(),
                        location: None,
                    },
                )
                .unwrap();
        }
        let facts = &graph.evidence()[&id("page:index")];
        assert_eq!(facts.len(), 2);
        let RebuildDecision::FullBuild { reasons } = graph.assess([id("page:index")], []) else {
            panic!("unknown evidence must force a full build");
        };
        assert!(reasons.iter().any(|reason| reason.starts_with("catalog:")));
        assert!(reasons.iter().any(|reason| reason.starts_with("render:")));
        assert!(!reasons
            .iter()
            .any(|reason| reason.contains("conflicting evidence")));
    }

    #[test]
    fn every_global_invalidation_forces_full_build() {
        let changes = [
            GlobalChange::SourceAdded("a.fsh".into()),
            GlobalChange::SourceDeleted("a.fsh".into()),
            GlobalChange::SourceRenamed {
                from: "a".into(),
                to: "b".into(),
            },
            GlobalChange::SourceOrder,
            GlobalChange::Config,
            GlobalChange::PackageClosure,
            GlobalChange::Template,
            GlobalChange::NegativeNamespace("aliases".into()),
            GlobalChange::UnattributedDiagnostic,
            GlobalChange::UnresolvedSnapshot,
            GlobalChange::DynamicEnumeration("input/pagecontent".into()),
            GlobalChange::OutputCatalog,
        ];
        for change in changes {
            assert!(matches!(
                ObservationGraph::default().assess([], [change]),
                RebuildDecision::FullBuild { .. }
            ));
        }
        assert!(!INCREMENTAL_EXECUTION_ENABLED);
    }

    #[test]
    fn graph_rejects_dangling_edges() {
        let mut graph = ObservationGraph::default();
        graph
            .insert_node(Node::new(id("source:a"), NodeKind::Source, "a.fsh"))
            .unwrap();
        assert_eq!(
            graph.insert_edge(Edge {
                from: id("source:a"),
                to: id("compiled:x"),
                kind: EdgeKind::Derives,
            }),
            Err(ObservationError::MissingEdgeEndpoint(id("compiled:x")))
        );
    }

    #[test]
    fn disabled_execution_and_reachable_unknowns_both_fail_closed() {
        let mut graph = ObservationGraph::default();
        graph
            .insert_node(Node::new(id("page:index"), NodeKind::Page, "index.html"))
            .unwrap();
        graph
            .insert_node(Node::new(id("fragment:x"), NodeKind::Fragment, "x"))
            .unwrap();
        graph
            .insert_edge(Edge {
                from: id("page:index"),
                to: id("fragment:x"),
                kind: EdgeKind::Reads,
            })
            .unwrap();
        graph
            .record_evidence(id("page:index"), Evidence::Complete)
            .unwrap();
        graph
            .record_evidence(
                id("fragment:x"),
                Evidence::Unknown {
                    phase: "fragment".into(),
                    reason: "internal reads are incomplete".into(),
                    location: None,
                },
            )
            .unwrap();
        let RebuildDecision::FullBuild { reasons } = graph.assess([id("page:index")], []) else {
            panic!("disabled execution and a reachable unknown must fail closed");
        };
        assert!(reasons.iter().any(|reason| reason.contains("disabled")));
        assert!(reasons.iter().any(|reason| reason.starts_with("fragment:")));
    }
}
