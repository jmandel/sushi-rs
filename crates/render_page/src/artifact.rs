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
use site_build::{ArtifactKey, FragmentKind, FragmentScope, ResourceKey};

/// Parameter containing the exact HL7 Publisher fragment kind understood by
/// `FragmentEngine` (`snapshot`, `dict-ms`, `dependency-table`, ...).
pub const PUBLISHER_KIND_PARAMETER: &str = "publisher_kind";

/// Parameter retaining the Publisher resource reference spelling
/// (`StructureDefinition-us-core-patient`). It is required for resource-scoped
/// fragments and for whole-IG kinds such as `uses` that still have a subject.
pub const PUBLISHER_REFERENCE_PARAMETER: &str = "publisher_reference";

const PUBLISHER_FRAGMENT_FAMILY: &str = "hl7.fhir.publisher";

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
pub struct ArtifactResolveError {
    message: String,
}

impl ArtifactResolveError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ArtifactResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
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
            .map_err(|error| ArtifactResolveError::new(error.to_string()))
    }
}

/// A build-generation-scoped cache. The key is semantic identity, not the
/// legacy filename. `None` records a failed/deferred attempt so repeated Liquid
/// lookups in the same generation preserve the old first-miss behavior.
pub type SharedArtifactCache = Rc<RefCell<BTreeMap<ArtifactKey, Option<String>>>>;

/// Typed requests attempted and artifacts successfully read while rendering
/// one page. Cached successes are reads too; failed requests appear only in
/// `requested` so invalidation can retry them when their inputs change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageArtifactReadSet {
    requested: BTreeSet<ArtifactKey>,
    read: BTreeSet<ArtifactKey>,
}

impl PageArtifactReadSet {
    pub fn requested(&self) -> &BTreeSet<ArtifactKey> {
        &self.requested
    }

    pub fn read(&self) -> &BTreeSet<ArtifactKey> {
        &self.read
    }

    pub(crate) fn request(&mut self, key: ArtifactKey) {
        self.requested.insert(key);
    }

    pub(crate) fn record_read(&mut self, key: ArtifactKey) {
        self.read.insert(key);
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
