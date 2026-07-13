//! Typed `cycle-site/v2` projection over [`crate::PreparedGuide`].
//!
//! This is the renderer-facing SiteBuild projection. It does not expose
//! relational row spelling: numeric surrogate keys,
//! PascalCase column names, JSON strings, and base64 asset bodies do not cross
//! the boundary.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    ArtifactCatalog, ArtifactKey, ArtifactProvenance, ArtifactRecord, ArtifactState,
    AssetNamespace, AuthoredFileRole, BuildDiagnostic, ClosedSiteBuild, ContentRef, ContractError,
    PackageLock, PreparedGuide, ProducerRef, ProjectIdentity, ReadDependency, RenderMode,
    RenderPlan, RenderTarget, SealError, Sha256Digest, SiteBuild, SiteBuildError, SourceKind,
    SourcePath,
};

// Preserve the original public type paths while placing their definitions at
// the renderer-neutral PreparedGuide seam.
pub use crate::{
    ExpansionCode, GeneratedIdentity, GuideIdentity, MenuNode, PageNode, PublisherCompatibility,
    ResourcePublication, SemanticResource, SemanticResourceKey, SourceControlIdentity,
    ValueSetExpansion,
};

pub const TARGET: &str = "cycle-site/v2";
pub const NAMESPACE: &str = "cycle.semantic/v1";
pub const RESOURCES_SCHEMA: &str = "cycle.semantic.resources/v1";
pub const TERMINOLOGY_SCHEMA: &str = "cycle.semantic.terminology/v1";
pub const NAVIGATION_SCHEMA: &str = "cycle.semantic.navigation/v2";
pub const CONFIG_SCHEMA: &str = "cycle.semantic.config/v1";

pub const RESOURCES_NAME: &str = "resources.json";
pub const TERMINOLOGY_NAME: &str = "terminology.json";
pub const NAVIGATION_NAME: &str = "navigation.json";
pub const CONFIG_NAME: &str = "config.json";
pub const AUTHORED_INCLUDE_NAMESPACE: &str = "cycle.authored.include/v1";

pub fn data_key(name: &str) -> ArtifactKey {
    ArtifactKey::Data {
        namespace: NAMESPACE.into(),
        name: name.into(),
    }
}
pub fn resources_key() -> ArtifactKey {
    data_key(RESOURCES_NAME)
}

pub fn terminology_key() -> ArtifactKey {
    data_key(TERMINOLOGY_NAME)
}

pub fn navigation_key() -> ArtifactKey {
    data_key(NAVIGATION_NAME)
}

pub fn config_key() -> ArtifactKey {
    data_key(CONFIG_NAME)
}

pub fn asset_key(path: SourcePath) -> ArtifactKey {
    ArtifactKey::Asset {
        namespace: AssetNamespace::Authored,
        path,
    }
}

pub fn include_key(path: SourcePath) -> ArtifactKey {
    ArtifactKey::Asset {
        namespace: AssetNamespace::Other {
            name: AUTHORED_INCLUDE_NAMESPACE.into(),
        },
        path,
    }
}

#[derive(Clone, Debug)]
pub struct CycleProjectionInput {
    pub project: ProjectIdentity,
    pub package_lock: PackageLock,
    pub render_target: RenderTarget,
    pub diagnostics: BTreeSet<BuildDiagnostic>,
}

/// A closed manifest and every artifact body introduced by the v2 projection.
/// Source and package objects remain the host's responsibility because their
/// content references were supplied in [`CycleProjectionInput`].
#[derive(Clone, Debug)]
pub struct ClosedCycleProjection {
    pub site_build: ClosedSiteBuild,
    pub objects: BTreeMap<Sha256Digest, Vec<u8>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourcesDocument {
    pub schema: String,
    pub guide: GuideIdentity,
    pub resources: Vec<SemanticResource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher_compatibility: Option<PublisherCompatibility>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminologyDocument {
    pub schema: String,
    pub expansions: Vec<ValueSetExpansion>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NavigationDocument {
    pub schema: String,
    pub pages: Vec<PageNode>,
    pub menu: Vec<MenuNode>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigDocument {
    pub schema: String,
    pub sushi_config: Value,
}

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Debug, Error)]
pub enum CycleProjectionError {
    #[error("cycle semantic projection requires target cycle-site/v2 with renderer cycle-site@2")]
    WrongTarget,
    #[error("invalid prepared Cycle site: {0}")]
    Invalid(String),
    #[error("conflicting bytes for CAS digest {0}")]
    ConflictingObject(Sha256Digest),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Catalog(#[from] ContractError),
    #[error(transparent)]
    Build(#[from] SiteBuildError),
    #[error(transparent)]
    Seal(#[from] SealError),
}

/// Produce the complete callback-free `cycle-site/v2` handoff directly from
/// renderer-neutral prepared guide semantics.
pub fn close_prepared(
    prepared: &PreparedGuide,
    input: CycleProjectionInput,
) -> Result<ClosedCycleProjection, CycleProjectionError> {
    validate_target(&input.render_target)?;
    validate_prepared(prepared, &input)?;

    let resources = ResourcesDocument {
        schema: RESOURCES_SCHEMA.into(),
        guide: prepared.guide.clone(),
        resources: prepared.resources.clone(),
        publisher_compatibility: prepared.publisher_compatibility.clone(),
    };
    let terminology = TerminologyDocument {
        schema: TERMINOLOGY_SCHEMA.into(),
        expansions: prepared.expansions.clone(),
    };
    let navigation = NavigationDocument {
        schema: NAVIGATION_SCHEMA.into(),
        pages: prepared.pages.clone(),
        menu: prepared.menu.clone(),
    };
    let config = ConfigDocument {
        schema: CONFIG_SCHEMA.into(),
        sushi_config: prepared.sushi_config.clone(),
    };

    // These are deterministic typed envelopes, but embedded FHIR/config JSON
    // deliberately retains insertion order. The exact bytes are committed by
    // ContentRef; recursively sorting them would alter machine JSON output.
    let resources_bytes = serde_json::to_vec(&resources)?;
    let terminology_bytes = serde_json::to_vec(&terminology)?;
    let navigation_bytes = serde_json::to_vec(&navigation)?;
    let config_bytes = serde_json::to_vec(&config)?;

    let all_inputs = all_input_reads(&input);
    let resources_key = resources_key();
    let terminology_key = terminology_key();
    let navigation_key = navigation_key();
    let config_key = config_key();
    let mut records = Vec::new();
    let mut objects = BTreeMap::new();
    let mut required = BTreeSet::new();

    push_json_artifact(
        &mut records,
        &mut objects,
        resources_key.clone(),
        resources_bytes,
        "site_semantics.resources",
        RESOURCES_SCHEMA,
        all_inputs.clone(),
    )?;
    required.insert(resources_key.clone());

    let terminology_reads = input
        .package_lock
        .iter()
        .map(|(coordinate, _)| ReadDependency::Package {
            coordinate: coordinate.clone(),
        })
        .chain([ReadDependency::Artifact {
            key: resources_key.clone(),
        }])
        .collect();
    push_json_artifact(
        &mut records,
        &mut objects,
        terminology_key.clone(),
        terminology_bytes,
        "site_semantics.terminology",
        TERMINOLOGY_SCHEMA,
        terminology_reads,
    )?;
    required.insert(terminology_key);

    let navigation_reads = input
        .project
        .sources
        .iter()
        .filter(|(_, entry)| matches!(entry.kind, SourceKind::Config | SourceKind::Page))
        .map(|(path, _)| ReadDependency::Source { path: path.clone() })
        .chain([ReadDependency::Artifact { key: resources_key }])
        .collect();
    push_json_artifact(
        &mut records,
        &mut objects,
        navigation_key.clone(),
        navigation_bytes,
        "site_semantics.navigation",
        NAVIGATION_SCHEMA,
        navigation_reads,
    )?;
    required.insert(navigation_key);

    let config_reads = input
        .project
        .sources
        .iter()
        .filter(|(_, entry)| matches!(entry.kind, SourceKind::Config))
        .map(|(path, _)| ReadDependency::Source { path: path.clone() })
        .collect();
    push_json_artifact(
        &mut records,
        &mut objects,
        config_key.clone(),
        config_bytes,
        "site_semantics.config",
        CONFIG_SCHEMA,
        config_reads,
    )?;
    required.insert(config_key);

    let mut seen_assets = BTreeSet::new();
    for asset in &prepared.authored_files {
        let path = SourcePath::parse(asset.path.as_str().to_owned()).map_err(|_| {
            CycleProjectionError::Invalid(format!("unsafe prepared asset path {}", asset.path))
        })?;
        let key = match asset.role {
            AuthoredFileRole::Image => asset_key(path),
            AuthoredFileRole::Include => include_key(path),
            AuthoredFileRole::PageContent
            | AuthoredFileRole::ResourceContent
            | AuthoredFileRole::Data
            | AuthoredFileRole::ImageSource => continue,
        };
        if !seen_assets.insert(key.clone()) {
            return Err(CycleProjectionError::Invalid(format!(
                "duplicate asset {:?}",
                asset.path.as_str()
            )));
        }
        let content = ContentRef::of_bytes(&asset.content, Some(asset.mime.clone()));
        insert_object(&mut objects, &content, asset.content.clone())?;
        records.push(ArtifactRecord {
            key: key.clone(),
            state: ArtifactState::Ready { content },
            provenance: provenance(
                "site_semantics.asset",
                match asset.role {
                    AuthoredFileRole::Image => "authored-image/v1",
                    AuthoredFileRole::Include => "authored-include/v1",
                    _ => unreachable!("non-Cycle inputs were filtered above"),
                },
            ),
            reads: asset
                .source_reads
                .iter()
                .map(|path| {
                    SourcePath::parse(path.as_str().to_owned())
                        .map(|path| ReadDependency::Source { path })
                        .expect("PreparedPath is normalized")
                })
                .collect(),
        });
        required.insert(key);
    }

    let artifacts = ArtifactCatalog::from_records(records)?;
    let site_build = SiteBuild::new(
        input.project,
        input.package_lock,
        input.render_target,
        RenderPlan::new(required),
        artifacts,
        input.diagnostics,
    )?
    .close()?;

    Ok(ClosedCycleProjection {
        site_build,
        objects,
    })
}

fn validate_target(target: &RenderTarget) -> Result<(), CycleProjectionError> {
    if target.mode != RenderMode::ExternalBuilder
        || target.renderer.id != "cycle-site"
        || target.renderer.version != "2"
        || target.parameters.get("contract").map(String::as_str) != Some(TARGET)
    {
        return Err(CycleProjectionError::WrongTarget);
    }
    Ok(())
}

fn validate_prepared(
    prepared: &PreparedGuide,
    input: &CycleProjectionInput,
) -> Result<(), CycleProjectionError> {
    if prepared.guide.fhir_version != input.render_target.fhir_version {
        return Err(CycleProjectionError::Invalid(format!(
            "prepared FHIR version {} disagrees with render target {}",
            prepared.guide.fhir_version, input.render_target.fhir_version
        )));
    }
    if prepared.guide.generated.epoch_seconds
        != input
            .render_target
            .parameters
            .get("buildEpochSecs")
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or_else(|| {
                CycleProjectionError::Invalid(
                    "missing or invalid buildEpochSecs target parameter".into(),
                )
            })?
    {
        return Err(CycleProjectionError::Invalid(
            "prepared generation epoch disagrees with render target".into(),
        ));
    }
    if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&prepared.guide.generated.epoch_seconds) {
        return Err(CycleProjectionError::Invalid(
            "buildEpochSecs is outside the JavaScript safe-integer range".into(),
        ));
    }
    if !prepared.sushi_config.is_object() {
        return Err(CycleProjectionError::Invalid(
            "sushi-config semantic value must be an object".into(),
        ));
    }

    let mut resources = BTreeMap::new();
    for resource in &prepared.resources {
        let resource_type = resource
            .resource
            .get("resourceType")
            .and_then(Value::as_str);
        let id = resource.resource.get("id").and_then(Value::as_str);
        if resource_type != Some(resource.key.resource_type.as_str())
            || id != Some(resource.key.id.as_str())
        {
            return Err(CycleProjectionError::Invalid(format!(
                "prepared resource {}/{} disagrees with its JSON identity",
                resource.key.resource_type, resource.key.id
            )));
        }
        if resources
            .insert(resource.key.clone(), &resource.resource)
            .is_some()
        {
            return Err(CycleProjectionError::Invalid(format!(
                "duplicate resource {}/{}",
                resource.key.resource_type, resource.key.id
            )));
        }
    }
    let primary = &prepared.guide.implementation_guide;
    if primary.resource_type != "ImplementationGuide" || !resources.contains_key(primary) {
        return Err(CycleProjectionError::Invalid(format!(
            "prepared primary guide {}/{} is absent from resources",
            primary.resource_type, primary.id
        )));
    }
    for expansion in &prepared.expansions {
        if expansion.value_set.resource_type != "ValueSet"
            || !resources.contains_key(&expansion.value_set)
        {
            return Err(CycleProjectionError::Invalid(format!(
                "expansion references absent ValueSet {}/{}",
                expansion.value_set.resource_type, expansion.value_set.id
            )));
        }
    }
    validate_page_sources(&prepared.pages, &input.project)?;
    for asset in &prepared.authored_files {
        for source in &asset.source_reads {
            let source_path =
                SourcePath::parse(source.as_str().to_owned()).expect("PreparedPath is normalized");
            if input.project.sources.get(&source_path).is_none() {
                return Err(CycleProjectionError::Invalid(format!(
                    "asset {} reads absent project source {}",
                    asset.path, source
                )));
            }
        }
    }
    Ok(())
}

fn validate_page_sources(
    pages: &[PageNode],
    project: &ProjectIdentity,
) -> Result<(), CycleProjectionError> {
    for page in pages {
        match (&page.body, &page.source) {
            (Some(_), Some(source)) => {
                let source = SourcePath::parse(source.as_str().to_owned())
                    .expect("PreparedPath is normalized");
                if project.sources.get(&source).is_none() {
                    return Err(CycleProjectionError::Invalid(format!(
                        "page {} reads absent project source {}",
                        page.name_url, source
                    )));
                }
            }
            (None, None) => {}
            _ => {
                return Err(CycleProjectionError::Invalid(format!(
                    "page {} must have both an authored body and source, or neither",
                    page.name_url
                )));
            }
        }
        validate_page_sources(&page.children, project)?;
    }
    Ok(())
}

fn all_input_reads(input: &CycleProjectionInput) -> BTreeSet<ReadDependency> {
    input
        .project
        .sources
        .iter()
        .map(|(path, _)| ReadDependency::Source { path: path.clone() })
        .chain(
            input
                .package_lock
                .iter()
                .map(|(coordinate, _)| ReadDependency::Package {
                    coordinate: coordinate.clone(),
                }),
        )
        .collect()
}

fn provenance(producer: &str, recipe: &str) -> ArtifactProvenance {
    ArtifactProvenance {
        producer: ProducerRef::new(producer, env!("CARGO_PKG_VERSION")),
        recipe: recipe.into(),
        attributes: BTreeMap::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn push_json_artifact(
    records: &mut Vec<ArtifactRecord>,
    objects: &mut BTreeMap<Sha256Digest, Vec<u8>>,
    key: ArtifactKey,
    bytes: Vec<u8>,
    producer: &str,
    recipe: &str,
    reads: BTreeSet<ReadDependency>,
) -> Result<(), CycleProjectionError> {
    let content = ContentRef::of_bytes(&bytes, Some("application/json"));
    insert_object(objects, &content, bytes)?;
    records.push(ArtifactRecord {
        key,
        state: ArtifactState::Ready { content },
        provenance: provenance(producer, recipe),
        reads,
    });
    Ok(())
}

fn insert_object(
    objects: &mut BTreeMap<Sha256Digest, Vec<u8>>,
    content: &ContentRef,
    bytes: Vec<u8>,
) -> Result<(), CycleProjectionError> {
    if let Some(existing) = objects.get(&content.sha256) {
        if existing != &bytes {
            return Err(CycleProjectionError::ConflictingObject(
                content.sha256.clone(),
            ));
        }
    } else {
        objects.insert(content.sha256.clone(), bytes);
    }
    Ok(())
}
