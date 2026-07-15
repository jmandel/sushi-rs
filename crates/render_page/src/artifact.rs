//! Typed boundary between legacy Publisher include names and semantic artifacts.
//!
//! Liquid still names generated fragments using Publisher-era filenames such as
//! `StructureDefinition-patient-snapshot.xhtml`. That spelling is confined to
//! [`legacy_include_to_artifact_key`]. The page provider and its callers deal in
//! [`ArtifactKey`] values after that compatibility edge.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use render_sd::engine::{FragmentEngine, PER_RESOURCE_KINDS, SINGLETON_KINDS};
use site_build::{
    ArtifactKey, ArtifactState, BuildDiagnostic, DiagnosticSeverity, FragmentKind, FragmentScope,
    ResourceKey, SourcePath,
};

/// Parameter containing the exact HL7 Publisher fragment kind understood by
/// `FragmentEngine` (`snapshot`, `dict-ms`, `dependency-table`, ...).
pub const PUBLISHER_KIND_PARAMETER: &str = "publisher_kind";

/// Parameter retaining the Publisher resource reference spelling
/// (`StructureDefinition-us-core-patient`). It is required for resource-scoped
/// fragments and for whole-IG kinds such as `uses` that still have a subject.
pub const PUBLISHER_REFERENCE_PARAMETER: &str = "publisher_reference";

const PUBLISHER_FRAGMENT_FAMILY: &str = "hl7.fhir.publisher";

pub const STOCK_PAGE_SOURCE_NAMESPACE: &str = "stock.page_source";
pub const STOCK_SITE_DATA_NAMESPACE: &str = "stock.site_data";
pub const STOCK_SITE_DATA_LOOKUP_NAMESPACE: &str = "stock.site_data.lookup";
pub const STOCK_SITE_NAMESPACE: &str = "stock.site";
pub const STOCK_STAGED_INCLUDE_NAMESPACE: &str = "stock.include.staged";
pub const STOCK_TEMPLATE_INCLUDE_NAMESPACE: &str = "stock.include.template";
pub const STOCK_RUNTIME_INPUT_NAMESPACE: &str = "stock.publisher_runtime";

pub fn stock_input_artifact(namespace: &str, name: impl Into<String>) -> ArtifactKey {
    ArtifactKey::Data {
        namespace: namespace.to_string(),
        name: name.into(),
    }
}

pub fn is_safe_stock_relative_path(name: &str) -> bool {
    SourcePath::parse(name.to_string()).is_ok()
}

/// Parse the Publisher filename prefix (`Type-id`) into the renderer-neutral
/// resource identity used by `SiteBuild`.
pub fn publisher_reference_to_resource_key(reference: &str) -> Option<ResourceKey> {
    let (resource_type, id) = reference.split_once('-')?;
    if resource_type.is_empty() || id.is_empty() {
        return None;
    }
    Some(ResourceKey {
        resource_type: resource_type.to_string(),
        id: id.to_string(),
    })
}

fn fragment_family(kind: &str) -> FragmentKind {
    if kind == "grid"
        || kind.starts_with("span")
        || kind.starts_with("snapshot")
        || kind.starts_with("diff")
    {
        FragmentKind::Table
    } else if kind.starts_with("dict") {
        FragmentKind::Dictionary
    } else if kind.starts_with("tx")
        || matches!(kind, "cld" | "expansion" | "content")
        || kind.starts_with("valueset-")
        || kind.starts_with("codesystem-")
    {
        FragmentKind::Terminology
    } else if kind.starts_with("summary") {
        FragmentKind::Summary
    } else {
        FragmentKind::Other {
            name: PUBLISHER_FRAGMENT_FAMILY.to_string(),
        }
    }
}

/// The one compatibility translation from a legacy include filename to a
/// typed semantic artifact request.
///
/// Unknown/authored/template include names return `None`; they remain ordinary
/// files. Registered singleton and whole-IG-per-resource kinds receive
/// [`FragmentScope::WholeIg`]. Every request carries the exact Publisher kind,
/// and every non-singleton request carries its original resource reference.
pub fn legacy_include_to_artifact_key(name: &str) -> Option<ArtifactKey> {
    let (reference, publisher_kind) = FragmentEngine::split_include(name)?;
    let whole_ig = FragmentEngine::is_whole_ig_kind(&publisher_kind);
    let scope = if whole_ig {
        FragmentScope::WholeIg
    } else {
        FragmentScope::Resource {
            resource: publisher_reference_to_resource_key(&reference)?,
        }
    };
    let mut parameters =
        BTreeMap::from([(PUBLISHER_KIND_PARAMETER.to_string(), publisher_kind.clone())]);
    if !reference.is_empty() {
        parameters.insert(PUBLISHER_REFERENCE_PARAMETER.to_string(), reference);
    }
    Some(ArtifactKey::Fragment {
        scope,
        fragment: fragment_family(&publisher_kind),
        parameters,
    })
}

/// A typed semantic artifact resolver. File lookup and Liquid know nothing
/// about how a fragment is produced; the resolver owns that decision.
pub trait ArtifactResolver {
    fn resolve(&self, key: &ArtifactKey) -> Result<String, ArtifactResolveError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactResolveFailure {
    Deferred { reason: String },
    Unsupported { capability: String, reason: String },
    Failed { code: String, message: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactResolveError {
    failure: ArtifactResolveFailure,
}

impl ArtifactResolveError {
    /// A concrete resolution failure. Existing resolvers use this constructor;
    /// demand-driven collectors preserve it as a typed failed artifact.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            failure: ArtifactResolveFailure::Failed {
                code: "artifact.resolve".into(),
                message: message.into(),
            },
        }
    }

    pub fn deferred(reason: impl Into<String>) -> Self {
        Self {
            failure: ArtifactResolveFailure::Deferred {
                reason: reason.into(),
            },
        }
    }

    pub fn unsupported(capability: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            failure: ArtifactResolveFailure::Unsupported {
                capability: capability.into(),
                reason: reason.into(),
            },
        }
    }

    pub fn failed(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            failure: ArtifactResolveFailure::Failed {
                code: code.into(),
                message: message.into(),
            },
        }
    }

    pub fn failure(&self) -> &ArtifactResolveFailure {
        &self.failure
    }

    pub fn artifact_state(&self) -> ArtifactState {
        match &self.failure {
            ArtifactResolveFailure::Deferred { reason } => ArtifactState::Deferred {
                reason: reason.clone(),
            },
            ArtifactResolveFailure::Unsupported { capability, reason } => {
                ArtifactState::Unsupported {
                    capability: capability.clone(),
                    reason: reason.clone(),
                }
            }
            ArtifactResolveFailure::Failed { code, message } => ArtifactState::Failed {
                diagnostics: BTreeSet::from([BuildDiagnostic::new(
                    DiagnosticSeverity::Error,
                    code.clone(),
                    message.clone(),
                )]),
            },
        }
    }
}

impl std::fmt::Display for ArtifactResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.failure {
            ArtifactResolveFailure::Deferred { reason }
            | ArtifactResolveFailure::Unsupported { reason, .. } => f.write_str(reason),
            ArtifactResolveFailure::Failed { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for ArtifactResolveError {}

/// Adapter from the existing native fragment engine to the typed resolver
/// boundary. It validates that key scope and exact Publisher parameters agree
/// before invoking the renderer.
pub struct FragmentEngineArtifactResolver<'a> {
    engine: &'a FragmentEngine,
}

impl<'a> FragmentEngineArtifactResolver<'a> {
    pub fn new(engine: &'a FragmentEngine) -> Self {
        Self { engine }
    }

    fn request_parts(key: &ArtifactKey) -> Result<(String, String), ArtifactResolveError> {
        let ArtifactKey::Fragment {
            scope, parameters, ..
        } = key
        else {
            return Err(ArtifactResolveError::new(format!(
                "fragment resolver cannot resolve non-fragment artifact {key:?}"
            )));
        };
        let kind = parameters
            .get(PUBLISHER_KIND_PARAMETER)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ArtifactResolveError::new(format!(
                    "fragment artifact is missing {PUBLISHER_KIND_PARAMETER}"
                ))
            })?
            .clone();
        let parameter_reference = parameters
            .get(PUBLISHER_REFERENCE_PARAMETER)
            .filter(|value| !value.is_empty())
            .cloned();
        let whole_ig_kind = FragmentEngine::is_whole_ig_kind(&kind);

        let reference = match scope {
            FragmentScope::Resource { resource } => {
                if whole_ig_kind {
                    return Err(ArtifactResolveError::new(format!(
                        "Publisher kind {kind} requires whole-IG scope"
                    )));
                }
                let expected = format!("{}-{}", resource.resource_type, resource.id);
                if parameter_reference.as_deref() != Some(expected.as_str()) {
                    return Err(ArtifactResolveError::new(format!(
                        "fragment resource scope {expected} disagrees with {PUBLISHER_REFERENCE_PARAMETER}"
                    )));
                }
                expected
            }
            FragmentScope::WholeIg => {
                if !whole_ig_kind {
                    return Err(ArtifactResolveError::new(format!(
                        "Publisher kind {kind} requires resource scope"
                    )));
                }
                parameter_reference.unwrap_or_default()
            }
        };

        let singleton = SINGLETON_KINDS.contains(&kind.as_str());
        let per_resource = PER_RESOURCE_KINDS.contains(&kind.as_str());
        if singleton && !reference.is_empty() {
            return Err(ArtifactResolveError::new(format!(
                "singleton Publisher kind {kind} must not carry a resource reference"
            )));
        }
        if per_resource && reference.is_empty() {
            return Err(ArtifactResolveError::new(format!(
                "per-resource Publisher kind {kind} requires {PUBLISHER_REFERENCE_PARAMETER}"
            )));
        }
        Ok((reference, kind))
    }
}

impl ArtifactResolver for FragmentEngineArtifactResolver<'_> {
    fn resolve(&self, key: &ArtifactKey) -> Result<String, ArtifactResolveError> {
        let (reference, kind) = Self::request_parts(key)?;
        self.engine
            .render_fragment(&reference, &kind)
            .map_err(|error| match error {
                render_sd::engine::FragError::UnknownKind(kind) => {
                    ArtifactResolveError::unsupported(
                        format!("publisher.fragment.{kind}"),
                        format!("unknown fragment kind: {kind}"),
                    )
                }
                render_sd::engine::FragError::Gap { kind, refname, msg } => {
                    ArtifactResolveError::unsupported(
                        format!("publisher.fragment.{kind}"),
                        format!("fragment gap [{kind} / {refname}]: {msg}"),
                    )
                }
                render_sd::engine::FragError::NoSuchResource(resource) => {
                    ArtifactResolveError::failed(
                        "publisher.fragment.missing_resource",
                        format!("no such resource: {resource}"),
                    )
                }
            })
    }
}

/// The cached outcome of one typed resolution attempt. Failures remain typed;
/// they are not collapsed to `None`, so a SiteBuild revision can preserve why a
/// requested artifact did not become a successful page read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactCacheEntry {
    Ready(String),
    NotReady(ArtifactResolveError),
}

/// A build-generation-scoped cache. The key is semantic identity, not the
/// legacy filename. A typed non-ready entry records a failed/deferred attempt
/// so repeated Liquid lookups in the same generation preserve the old
/// first-miss behavior without discarding closure diagnostics.
pub type SharedArtifactCache = Rc<RefCell<BTreeMap<ArtifactKey, ArtifactCacheEntry>>>;

/// Resolution outcomes observed while rendering a page. Ready bytes are kept
/// so the revision collector can publish them to the CAS exactly once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactObservation {
    Ready { bytes: Vec<u8> },
    NotReady { error: ArtifactResolveError },
}

/// Typed requests attempted and artifacts successfully read while rendering
/// one page. Cached successes are reads too; failed requests appear only in
/// `requested` so invalidation can retry them when their inputs change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageArtifactReadSet {
    requested: BTreeSet<ArtifactKey>,
    read: BTreeSet<ArtifactKey>,
    input_reads: BTreeSet<ArtifactKey>,
    input_objects: BTreeMap<ArtifactKey, BTreeSet<Vec<u8>>>,
    observations: BTreeMap<ArtifactKey, ArtifactObservation>,
}

impl PageArtifactReadSet {
    pub fn requested(&self) -> &BTreeSet<ArtifactKey> {
        &self.requested
    }

    pub fn read(&self) -> &BTreeSet<ArtifactKey> {
        &self.read
    }

    /// Non-generated page inputs actually read: the page source, `_data`
    /// entries, staged/template includes, and include-relative sources.
    pub fn input_reads(&self) -> &BTreeSet<ArtifactKey> {
        &self.input_reads
    }

    /// Exact bytes observed for every non-generated page input. A set is used
    /// so a changing backing tree cannot collapse A→B reads into one identity;
    /// revision collection rejects keys with more than one observed value.
    pub fn input_objects(&self) -> &BTreeMap<ArtifactKey, BTreeSet<Vec<u8>>> {
        &self.input_objects
    }

    /// Every successful typed dependency of the rendered page. Failed requests
    /// are deliberately absent; their typed outcomes remain in
    /// [`observations`](Self::observations).
    pub fn dependencies(&self) -> BTreeSet<ArtifactKey> {
        self.read.union(&self.input_reads).cloned().collect()
    }

    pub fn observations(&self) -> &BTreeMap<ArtifactKey, ArtifactObservation> {
        &self.observations
    }

    pub(crate) fn request(&mut self, key: ArtifactKey) {
        self.requested.insert(key);
    }

    /// Record a non-generated namespace candidate even when it misses. This is
    /// compiled only into explicit dependency-observation builds at call sites.
    #[cfg(feature = "dependency-observation")]
    pub(crate) fn request_input(&mut self, key: ArtifactKey) {
        self.request(key);
    }

    #[cfg(feature = "dependency-observation")]
    pub(crate) fn observe_input_lookup(&mut self, key: ArtifactKey, found: bool) {
        self.request(key.clone());
        if found {
            self.record_read(key);
        }
    }

    pub(crate) fn record_read(&mut self, key: ArtifactKey) {
        self.read.insert(key);
    }

    pub(crate) fn record_input(&mut self, key: ArtifactKey, bytes: impl Into<Vec<u8>>) {
        self.input_reads.insert(key.clone());
        self.input_objects
            .entry(key)
            .or_default()
            .insert(bytes.into());
    }

    /// Add a non-generated input discovered by a renderer post-pass (for
    /// example the Publisher release-header substitution). Ordinary Liquid
    /// reads are recorded automatically by [`crate::PageProvider`].
    pub fn add_input_object(&mut self, key: ArtifactKey, bytes: impl Into<Vec<u8>>) {
        self.record_input(key, bytes);
    }

    pub(crate) fn observe(&mut self, key: ArtifactKey, outcome: ArtifactObservation) {
        self.observations.insert(key, outcome);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use render_sd::context::IgContext;
    use render_sd::engine::IgFacts;
    use render_sd::tree::MemTree;
    use serde_json::json;

    use super::*;

    fn publisher_kind(key: &ArtifactKey) -> &str {
        let ArtifactKey::Fragment { parameters, .. } = key else {
            panic!("not a fragment")
        };
        parameters[PUBLISHER_KIND_PARAMETER].as_str()
    }

    #[test]
    fn translates_resource_singleton_and_whole_ig_subject_names() {
        let resource = legacy_include_to_artifact_key(
            "StructureDefinition-us-core-patient-snapshot-by-mustsupport-all.xhtml",
        )
        .expect("registered resource fragment");
        assert_eq!(publisher_kind(&resource), "snapshot-by-mustsupport-all");
        let ArtifactKey::Fragment {
            scope: FragmentScope::Resource { resource: subject },
            fragment: FragmentKind::Table,
            parameters,
        } = resource
        else {
            panic!("resource table key")
        };
        assert_eq!(subject.resource_type, "StructureDefinition");
        assert_eq!(subject.id, "us-core-patient");
        assert_eq!(
            parameters[PUBLISHER_REFERENCE_PARAMETER],
            "StructureDefinition-us-core-patient"
        );

        let singleton =
            legacy_include_to_artifact_key("dependency-table.xhtml").expect("registered singleton");
        assert!(matches!(
            singleton,
            ArtifactKey::Fragment {
                scope: FragmentScope::WholeIg,
                ..
            }
        ));
        assert_eq!(publisher_kind(&singleton), "dependency-table");

        let aggregate =
            legacy_include_to_artifact_key("StructureDefinition-us-core-patient-uses.xhtml")
                .expect("whole-IG kind with a subject");
        assert!(matches!(
            aggregate,
            ArtifactKey::Fragment {
                scope: FragmentScope::WholeIg,
                ..
            }
        ));
        assert_eq!(publisher_kind(&aggregate), "uses");
        assert!(legacy_include_to_artifact_key("authored-note.md").is_none());
    }

    #[test]
    fn fragment_engine_adapter_resolves_a_typed_key() {
        let mut tree = MemTree::new();
        let sd = json!({
            "resourceType": "StructureDefinition",
            "id": "test",
            "url": "http://example.org/StructureDefinition/test",
            "name": "Test",
            "status": "draft",
            "fhirVersion": "4.0.1",
            "kind": "resource",
            "abstract": false,
            "type": "Patient",
            "baseDefinition": "http://hl7.org/fhir/StructureDefinition/Patient",
            "derivation": "constraint",
            "snapshot": { "element": [{ "id": "Patient", "path": "Patient" }] }
        });
        tree.insert_text(
            Path::new("/own/StructureDefinition-test.json"),
            &serde_json::to_string(&sd).unwrap(),
        );
        let tree: Rc<dyn render_sd::tree::TreeSource> = Rc::new(tree);
        let ctx = IgContext::load_with_tree(tree, Path::new("/own"), Path::new("/packages"), None);
        let engine = FragmentEngine::new(ctx, "test-run".into(), false, IgFacts::default());
        let resolver = FragmentEngineArtifactResolver::new(&engine);
        let key = legacy_include_to_artifact_key("StructureDefinition-test-history.xhtml")
            .expect("history key");
        let body = resolver.resolve(&key).expect("adapter resolution");
        assert_eq!(body, "{% raw %}{% endraw %}");

        let mut invalid = key;
        let ArtifactKey::Fragment { scope, .. } = &mut invalid else {
            unreachable!()
        };
        *scope = FragmentScope::WholeIg;
        assert!(resolver.resolve(&invalid).is_err());
    }
}
