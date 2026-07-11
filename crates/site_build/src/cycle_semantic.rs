//! Typed `cycle-site/v2` projection over [`crate::PreparedGuide`].
//!
//! This is the renderer-facing SiteBuild projection. Unlike
//! [`crate::site_db_compat`],
//! it does not expose relational row spelling: numeric surrogate keys,
//! PascalCase column names, JSON strings, and base64 asset bodies do not cross
//! the boundary. The core projector has no `site_db` dependency. During the
//! migration, [`prepare_from_site_db`] is the sole compatibility adapter.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    ArtifactCatalog, ArtifactKey, ArtifactProvenance, ArtifactRecord, ArtifactState,
    AssetNamespace, BuildDiagnostic, ClosedSiteBuild, ContentRef, ContractError, PackageLock,
    PreparedGuide, ProducerRef, ProjectRevision, ReadDependency, RenderMode, RenderPlan,
    RenderTarget, SealError, Sha256Digest, SiteBuild, SiteBuildError, SourceKind, SourcePath,
};

#[cfg(feature = "site-db-projections")]
use crate::{PreparedAsset, PreparedPath};

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
pub const NAVIGATION_SCHEMA: &str = "cycle.semantic.navigation/v1";
pub const CONFIG_SCHEMA: &str = "cycle.semantic.config/v1";

pub const RESOURCES_NAME: &str = "resources.json";
pub const TERMINOLOGY_NAME: &str = "terminology.json";
pub const NAVIGATION_NAME: &str = "navigation.json";
pub const CONFIG_NAME: &str = "config.json";

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

#[derive(Clone, Debug)]
pub struct CycleProjectionInput {
    pub project: ProjectRevision,
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

#[derive(Clone, Debug)]
#[cfg(feature = "site-db-projections")]
struct ProjectedResourceIdentity {
    key: SemanticResourceKey,
    url: Option<String>,
}

#[cfg(feature = "site-db-projections")]
const MAX_NAVIGATION_DEPTH: i64 = 256;
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

/// Transitional adapter from the legacy relational row model.
///
/// New producers should construct [`PreparedGuide`] directly. Keeping this
/// conversion separate makes `site.db` an optional projection concern rather
/// than the renderer handoff.
#[cfg(feature = "site-db-projections")]
pub fn prepare_from_site_db(
    db: &site_db::SiteDb,
    input: &CycleProjectionInput,
) -> Result<PreparedGuide, CycleProjectionError> {
    validate_target(&input.render_target)?;
    let (resources, resource_keys) = resources_document(db, input)?;
    let terminology = terminology_document(db, &resource_keys)?;
    let navigation = navigation_document(db)?;
    let config = config_document(db)?;
    let source_reads: BTreeSet<PreparedPath> = input
        .project
        .sources
        .iter()
        .map(|(path, _)| {
            PreparedPath::parse(path.as_str().to_owned()).expect("SourcePath is normalized")
        })
        .collect();
    let assets = db
        .assets
        .iter()
        .map(|asset| {
            Ok(PreparedAsset {
                path: PreparedPath::parse(asset.name.clone()).map_err(|_| {
                    CycleProjectionError::Invalid(format!("unsafe asset path {:?}", asset.name))
                })?,
                mime: asset.mime.clone(),
                content: asset.content.clone(),
                // SiteDb loses exact asset origin after last-writer de-dup. The
                // adapter therefore records the honest conservative read set.
                source_reads: source_reads.clone(),
            })
        })
        .collect::<Result<Vec<_>, CycleProjectionError>>()?;
    Ok(PreparedGuide {
        guide: resources.guide,
        resources: resources.resources,
        publisher_compatibility: resources.publisher_compatibility,
        expansions: terminology.expansions,
        pages: navigation.pages,
        menu: navigation.menu,
        sushi_config: config.sushi_config,
        assets,
    })
}

/// Compatibility convenience: adapt SiteDb, then use the direct projector.
#[cfg(feature = "site-db-projections")]
pub fn close_projection(
    db: &site_db::SiteDb,
    input: CycleProjectionInput,
) -> Result<ClosedCycleProjection, CycleProjectionError> {
    let prepared = prepare_from_site_db(db, &input)?;
    close_prepared(&prepared, input)
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
    for asset in &prepared.assets {
        let key = asset_key(
            SourcePath::parse(asset.path.as_str().to_owned()).map_err(|_| {
                CycleProjectionError::Invalid(format!("unsafe prepared asset path {}", asset.path))
            })?,
        );
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
            provenance: provenance("site_semantics.asset", "authored-asset/v1"),
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
    for asset in &prepared.assets {
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

#[cfg(feature = "site-db-projections")]
fn metadata(db: &site_db::SiteDb) -> Result<BTreeMap<&str, &str>, CycleProjectionError> {
    let mut values = BTreeMap::new();
    for row in &db.metadata {
        if values
            .insert(row.name.as_str(), row.value.as_str())
            .is_some()
        {
            return Err(CycleProjectionError::Invalid(format!(
                "duplicate metadata name {}",
                row.name
            )));
        }
    }
    Ok(values)
}

#[cfg(feature = "site-db-projections")]
fn optional_metadata(values: &BTreeMap<&str, &str>, key: &str) -> Option<String> {
    values
        .get(key)
        .copied()
        .filter(|value| !value.trim().is_empty() && *value != "unknown")
        .map(str::to_string)
}

#[cfg(feature = "site-db-projections")]
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|item| !item.trim().is_empty())
}

#[cfg(feature = "site-db-projections")]
fn required_metadata(
    values: &BTreeMap<&str, &str>,
    key: &str,
) -> Result<String, CycleProjectionError> {
    values
        .get(key)
        .copied()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| CycleProjectionError::Invalid(format!("missing metadata {key}")))
}

#[cfg(feature = "site-db-projections")]
fn resources_document(
    db: &site_db::SiteDb,
    input: &CycleProjectionInput,
) -> Result<(ResourcesDocument, BTreeMap<i64, ProjectedResourceIdentity>), CycleProjectionError> {
    let mut resources = Vec::with_capacity(db.resources.len());
    let mut by_numeric_key = BTreeMap::new();
    let mut semantic_keys = BTreeSet::new();
    for row in &db.resources {
        let resource: Value = serde_json::from_str(&row.json)?;
        let resource_type = resource
            .get("resourceType")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CycleProjectionError::Invalid(format!(
                    "resource row {} has no resourceType",
                    row.key
                ))
            })?;
        let id = resource.get("id").and_then(Value::as_str).ok_or_else(|| {
            CycleProjectionError::Invalid(format!("resource row {} has no id", row.key))
        })?;
        if resource_type.trim().is_empty() || id.trim().is_empty() {
            return Err(CycleProjectionError::Invalid(format!(
                "resource row {} has an empty resourceType/id",
                row.key
            )));
        }
        if resource_type != row.type_ {
            return Err(CycleProjectionError::Invalid(format!(
                "resource row {} type {} disagrees with JSON {}",
                row.key, row.type_, resource_type
            )));
        }
        let key = SemanticResourceKey {
            resource_type: resource_type.into(),
            id: id.into(),
        };
        if !semantic_keys.insert(key.clone()) {
            return Err(CycleProjectionError::Invalid(format!(
                "duplicate resource {}/{}",
                key.resource_type, key.id
            )));
        }
        let projected = ProjectedResourceIdentity {
            key: key.clone(),
            url: resource
                .get("url")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        };
        if by_numeric_key.insert(row.key, projected).is_some() {
            return Err(CycleProjectionError::Invalid(format!(
                "duplicate legacy resource key {}",
                row.key
            )));
        }
        let publication = ResourcePublication {
            display_name: non_empty(row.name.clone()),
            description: non_empty(row.description.clone()),
            standard_status: non_empty(row.standard_status.clone()),
            base_definition: non_empty(row.base.clone()),
        };
        resources.push(SemanticResource {
            key,
            resource,
            publication: (!publication.is_empty()).then_some(publication),
        });
    }

    let primary_guide = db.primary_implementation_guide.as_ref().ok_or_else(|| {
        CycleProjectionError::Invalid(
            "prepared site does not identify its primary ImplementationGuide".into(),
        )
    })?;
    let implementation_guide = SemanticResourceKey {
        resource_type: primary_guide.resource_type.clone(),
        id: primary_guide.id.clone(),
    };
    if implementation_guide.resource_type != "ImplementationGuide"
        || !semantic_keys.contains(&implementation_guide)
    {
        return Err(CycleProjectionError::Invalid(format!(
            "prepared primary guide {}/{} is absent from resources",
            implementation_guide.resource_type, implementation_guide.id
        )));
    }
    let values = metadata(db)?;
    let epoch_seconds = input
        .render_target
        .parameters
        .get("buildEpochSecs")
        .ok_or_else(|| {
            CycleProjectionError::Invalid("missing buildEpochSecs target parameter".into())
        })?
        .parse::<i64>()
        .map_err(|_| {
            CycleProjectionError::Invalid("invalid buildEpochSecs target parameter".into())
        })?;
    if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&epoch_seconds) {
        return Err(CycleProjectionError::Invalid(
            "buildEpochSecs is outside the JavaScript safe-integer range".into(),
        ));
    }
    let package_id =
        optional_metadata(&values, "packageId").unwrap_or_else(|| input.project.project_id.clone());
    let branch = optional_metadata(&values, "gitstatus");
    let revision = optional_metadata(&values, "revision");
    let source_control = (branch.is_some() || revision.is_some())
        .then_some(SourceControlIdentity { branch, revision });
    let guide = GuideIdentity {
        implementation_guide,
        package_id,
        canonical: optional_metadata(&values, "canonical"),
        name: optional_metadata(&values, "igName"),
        version: optional_metadata(&values, "igVer"),
        fhir_version: input.render_target.fhir_version.clone(),
        release_label: optional_metadata(&values, "releaseLabel"),
        fhir_publication_base: required_metadata(&values, "path")?,
        generated: GeneratedIdentity {
            epoch_seconds,
            date: required_metadata(&values, "genDate")?,
            day: required_metadata(&values, "genDay")?,
        },
        source_control,
    };
    let publisher_compatibility = Some(PublisherCompatibility {
        error_count: values
            .get("errorCount")
            .copied()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("0")
            .into(),
        tooling_version: values
            .get("toolingVersion")
            .copied()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("site-gen.publisher")
            .into(),
        tooling_revision: values
            .get("toolingRevision")
            .copied()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("0")
            .into(),
        tooling_version_full: values
            .get("toolingVersionFull")
            .copied()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("site-gen.publisher experiment")
            .into(),
    });

    Ok((
        ResourcesDocument {
            schema: RESOURCES_SCHEMA.into(),
            guide,
            resources,
            publisher_compatibility,
        },
        by_numeric_key,
    ))
}

#[cfg(feature = "site-db-projections")]
fn terminology_document(
    db: &site_db::SiteDb,
    resources: &BTreeMap<i64, ProjectedResourceIdentity>,
) -> Result<TerminologyDocument, CycleProjectionError> {
    let mut expansions: Vec<ValueSetExpansion> = Vec::new();
    let mut index = BTreeMap::<(i64, String, String), usize>::new();
    for row in &db.value_set_codes {
        let resource = resources.get(&row.resource_key).ok_or_else(|| {
            CycleProjectionError::Invalid(format!(
                "ValueSet expansion references missing resource key {}",
                row.resource_key
            ))
        })?;
        let key = &resource.key;
        if key.resource_type != "ValueSet" {
            return Err(CycleProjectionError::Invalid(format!(
                "expansion resource {}/{} is not a ValueSet",
                key.resource_type, key.id
            )));
        }
        if row.value_set_uri.trim().is_empty()
            || row.system.trim().is_empty()
            || row.code.trim().is_empty()
        {
            return Err(CycleProjectionError::Invalid(format!(
                "ValueSet expansion row {} has an empty URL/system/code",
                row.key
            )));
        }
        if resource
            .url
            .as_deref()
            .is_some_and(|url| url != row.value_set_uri)
        {
            return Err(CycleProjectionError::Invalid(format!(
                "ValueSet expansion URL {} disagrees with resource {}/{}",
                row.value_set_uri, key.resource_type, key.id
            )));
        }
        let group = (
            row.resource_key,
            row.value_set_uri.clone(),
            row.value_set_version.clone(),
        );
        let position = if let Some(position) = index.get(&group) {
            *position
        } else {
            let position = expansions.len();
            index.insert(group, position);
            expansions.push(ValueSetExpansion {
                value_set: key.clone(),
                url: row.value_set_uri.clone(),
                version: (!row.value_set_version.is_empty()).then(|| row.value_set_version.clone()),
                codes: Vec::new(),
            });
            position
        };
        expansions[position].codes.push(ExpansionCode {
            system: row.system.clone(),
            version: row.version.clone().filter(|value| !value.is_empty()),
            code: row.code.clone(),
            display: non_empty(row.display.clone()),
        });
    }
    for expansion in &mut expansions {
        expansion.codes.sort_by(|left, right| {
            left.system
                .cmp(&right.system)
                .then(left.code.cmp(&right.code))
                .then(left.version.cmp(&right.version))
                .then(left.display.cmp(&right.display))
        });
    }
    expansions.sort_by(|left, right| {
        left.url
            .cmp(&right.url)
            .then(left.version.cmp(&right.version))
            .then(left.value_set.cmp(&right.value_set))
    });
    Ok(TerminologyDocument {
        schema: TERMINOLOGY_SCHEMA.into(),
        expansions,
    })
}

#[cfg(feature = "site-db-projections")]
fn navigation_document(db: &site_db::SiteDb) -> Result<NavigationDocument, CycleProjectionError> {
    let mut page_names = BTreeSet::new();
    for (index, page) in db.pages.iter().enumerate() {
        if page.ord != index as i64 {
            return Err(CycleProjectionError::Invalid(format!(
                "page {} has non-canonical ordinal {}",
                page.name_url, page.ord
            )));
        }
        if !page_names.insert(page.name_url.as_str()) {
            return Err(CycleProjectionError::Invalid(format!(
                "duplicate page nameUrl {}",
                page.name_url
            )));
        }
    }
    let page_base_depth = db.pages.first().map_or(0, |page| page.depth);
    if page_base_depth < 0 {
        return Err(CycleProjectionError::Invalid(format!(
            "page tree starts at negative depth {page_base_depth}"
        )));
    }
    let mut page_index = 0usize;
    let pages = page_level(&db.pages, &mut page_index, page_base_depth, 0)?;
    if page_index != db.pages.len() {
        return Err(CycleProjectionError::Invalid(
            "page tree has an invalid depth transition".into(),
        ));
    }
    let menu = menu_tree(&db.menu)?;
    Ok(NavigationDocument {
        schema: NAVIGATION_SCHEMA.into(),
        pages,
        menu,
    })
}

#[cfg(feature = "site-db-projections")]
fn page_level(
    rows: &[site_db::model::PageRow],
    index: &mut usize,
    source_depth: i64,
    semantic_depth: i64,
) -> Result<Vec<PageNode>, CycleProjectionError> {
    let mut result = Vec::new();
    while let Some(row) = rows.get(*index) {
        if row.depth < source_depth {
            break;
        }
        if row.depth > source_depth {
            return Err(CycleProjectionError::Invalid(format!(
                "page {} jumps from depth {} to {}",
                row.name_url,
                source_depth.saturating_sub(1),
                row.depth
            )));
        }
        if semantic_depth > MAX_NAVIGATION_DEPTH {
            return Err(CycleProjectionError::Invalid(format!(
                "page tree exceeds maximum depth {MAX_NAVIGATION_DEPTH}"
            )));
        }
        if row.name_url.trim().is_empty()
            || row.title.trim().is_empty()
            || row.generation.trim().is_empty()
        {
            return Err(CycleProjectionError::Invalid(format!(
                "page ordinal {} has an empty name/title/generation",
                row.ord
            )));
        }
        *index += 1;
        let child_source_depth = source_depth.checked_add(1).ok_or_else(|| {
            CycleProjectionError::Invalid("page tree depth overflows its source range".into())
        })?;
        let children = page_level(rows, index, child_source_depth, semantic_depth + 1)?;
        result.push(PageNode {
            name_url: row.name_url.clone(),
            title: row.title.clone(),
            generation: row.generation.clone(),
            body: row.body.clone(),
            children,
        });
    }
    Ok(result)
}

#[cfg(feature = "site-db-projections")]
fn menu_tree(rows: &[site_db::model::MenuRow]) -> Result<Vec<MenuNode>, CycleProjectionError> {
    let mut by_id = BTreeMap::new();
    let mut ordinals = BTreeSet::new();
    for row in rows {
        if by_id.insert(row.id, row).is_some() {
            return Err(CycleProjectionError::Invalid(format!(
                "duplicate menu id {}",
                row.id
            )));
        }
        if !ordinals.insert(row.ord) {
            return Err(CycleProjectionError::Invalid(format!(
                "duplicate menu ordinal {}",
                row.ord
            )));
        }
    }
    for row in rows {
        if let Some(parent) = row.parent_id {
            if !by_id.contains_key(&parent) {
                return Err(CycleProjectionError::Invalid(format!(
                    "menu {} references missing parent {}",
                    row.id, parent
                )));
            }
        }
    }
    let mut children: BTreeMap<Option<i64>, Vec<&site_db::model::MenuRow>> = BTreeMap::new();
    for row in rows {
        children.entry(row.parent_id).or_default().push(row);
    }
    for values in children.values_mut() {
        values.sort_by_key(|row| row.ord);
    }
    let mut visited = BTreeSet::new();
    fn build(
        parent: Option<i64>,
        depth: i64,
        children: &BTreeMap<Option<i64>, Vec<&site_db::model::MenuRow>>,
        visited: &mut BTreeSet<i64>,
    ) -> Result<Vec<MenuNode>, CycleProjectionError> {
        let level = children.get(&parent);
        if depth > MAX_NAVIGATION_DEPTH && level.is_some_and(|rows| !rows.is_empty()) {
            return Err(CycleProjectionError::Invalid(format!(
                "menu tree exceeds maximum depth {MAX_NAVIGATION_DEPTH}"
            )));
        }
        let mut result = Vec::new();
        for row in level.into_iter().flatten() {
            if !visited.insert(row.id) {
                return Err(CycleProjectionError::Invalid(format!(
                    "menu cycle at id {}",
                    row.id
                )));
            }
            if row.depth != depth {
                return Err(CycleProjectionError::Invalid(format!(
                    "menu {} has depth {}, expected {}",
                    row.id, row.depth, depth
                )));
            }
            let items = build(Some(row.id), depth + 1, children, visited)?;
            if (row.kind == "link") != row.href.is_some()
                || (row.kind == "group") != row.href.is_none()
            {
                return Err(CycleProjectionError::Invalid(format!(
                    "menu {} has inconsistent kind/href",
                    row.id
                )));
            }
            if row.label.trim().is_empty()
                || row
                    .href
                    .as_deref()
                    .is_some_and(|href| href.trim().is_empty())
            {
                return Err(CycleProjectionError::Invalid(format!(
                    "menu {} has an empty label/href",
                    row.id
                )));
            }
            if row.href.is_some() && !items.is_empty() {
                return Err(CycleProjectionError::Invalid(format!(
                    "menu link {} also has child items",
                    row.id
                )));
            }
            result.push(MenuNode {
                label: row.label.clone(),
                href: row.href.clone(),
                items,
            });
        }
        Ok(result)
    }
    let result = build(None, 0, &children, &mut visited)?;
    if visited.len() != rows.len() {
        return Err(CycleProjectionError::Invalid(
            "menu contains an unreachable cycle".into(),
        ));
    }
    Ok(result)
}

#[cfg(feature = "site-db-projections")]
fn config_document(db: &site_db::SiteDb) -> Result<ConfigDocument, CycleProjectionError> {
    let mut matches = db
        .site_config
        .iter()
        .filter(|row| row.name == "sushi-config");
    let row = matches.next().ok_or_else(|| {
        CycleProjectionError::Invalid("missing sushi-config semantic value".into())
    })?;
    if matches.next().is_some() {
        return Err(CycleProjectionError::Invalid(
            "duplicate sushi-config semantic value".into(),
        ));
    }
    let sushi_config: Value = serde_json::from_str(&row.json)?;
    if !sushi_config.is_object() {
        return Err(CycleProjectionError::Invalid(
            "sushi-config semantic value must be an object".into(),
        ));
    }
    Ok(ConfigDocument {
        schema: CONFIG_SCHEMA.into(),
        sushi_config,
    })
}
