//! Complete renderer-neutral guide preparation over snapshot-complete resources
//! and captured authored inputs. Relational rows are downstream projections.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::{
    augment, timefmt, AugmentInputs, GeneratedIdentity, GuideIdentity, PreparedGuide,
    PublisherCompatibility, ResourcePublication, SemanticResource, SemanticResourceKey,
    SourceControlIdentity,
};

/// Exact semantic inputs after compilation and snapshot completion. This is an
/// operation input, not another persisted domain value.
pub struct PrepareInputs<'a> {
    pub generated: &'a [Value],
    pub primary_implementation_guide: &'a Value,
    pub examples: &'a [Value],
    pub sushi_config_yaml: &'a str,
    pub build_epoch_secs: i64,
    pub branch: Option<String>,
    pub revision: Option<String>,
    pub augmentation: AugmentInputs<'a>,
}

fn scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn field(value: &Value, name: &str) -> Option<String> {
    value.get(name).and_then(scalar_string)
}

fn resource_type(resource: &Value) -> &str {
    resource
        .get("resourceType")
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn resource_ref(resource: &Value) -> String {
    format!(
        "{}/{}",
        resource_type(resource),
        resource.get("id").and_then(Value::as_str).unwrap_or("")
    )
}

fn type_rank(resource_type: &str) -> u32 {
    match resource_type {
        "ImplementationGuide" => 0,
        "CodeSystem" => 1,
        "StructureDefinition" => 2,
        "ValueSet" => 3,
        "Bundle" => 4,
        "Observation" => 5,
        _ => 100,
    }
}

const CANONICAL_RESOURCE_TYPES: &[&str] = &[
    "ActivityDefinition",
    "CapabilityStatement",
    "ChargeItemDefinition",
    "CodeSystem",
    "CompartmentDefinition",
    "ConceptMap",
    "EffectEvidenceSynthesis",
    "EventDefinition",
    "Evidence",
    "EvidenceVariable",
    "ExampleScenario",
    "GraphDefinition",
    "ImplementationGuide",
    "Library",
    "Measure",
    "MessageDefinition",
    "NamingSystem",
    "OperationDefinition",
    "PlanDefinition",
    "Questionnaire",
    "ResearchElementDefinition",
    "RiskEvidenceSynthesis",
    "SearchParameter",
    "StructureDefinition",
    "StructureMap",
    "TerminologyCapabilities",
    "TestScript",
    "ValueSet",
];

fn canonical_resource(resource: &Value) -> bool {
    CANONICAL_RESOURCE_TYPES.contains(&resource_type(resource))
}

fn config_flag(config: &Value, name: &str) -> bool {
    match config.pointer(&format!("/parameters/{name}")) {
        Some(Value::Bool(true)) => true,
        Some(Value::String(value)) => value == "true",
        _ => false,
    }
}

fn configured_contact(config: &Value) -> Option<Value> {
    if let Some(Value::Array(entries)) = config.get("contact") {
        if !entries.is_empty() {
            return Some(Value::Array(entries.clone()));
        }
    }
    let name = config.pointer("/publisher/name");
    let url = config.pointer("/publisher/url");
    if name.is_none() && url.is_none() {
        return None;
    }
    let mut entry = Map::new();
    if let Some(name) = name {
        entry.insert("name".into(), name.clone());
    }
    if let Some(url) = url {
        entry.insert("telecom".into(), json!([{ "system": "url", "value": url }]));
    }
    Some(Value::Array(vec![Value::Object(entry)]))
}

fn apply_global_metadata(mut resource: Value, config: &Value, generated_at: &str) -> Value {
    let has_url = matches!(resource.get("url"), Some(Value::String(value)) if !value.is_empty());
    if !has_url && !canonical_resource(&resource) {
        return resource;
    }
    let resource_type = resource_type(&resource).to_string();
    let resource_id = resource
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let canonical = config
        .get("canonical")
        .and_then(Value::as_str)
        .map(str::to_string);
    let object = resource
        .as_object_mut()
        .expect("FHIR resource is an object");
    if !has_url {
        if let (Some(canonical), Some(id)) = (canonical, resource_id) {
            object.insert(
                "url".into(),
                Value::String(format!(
                    "{}/{resource_type}/{id}",
                    canonical.trim_end_matches('/')
                )),
            );
        }
    }
    if let Some(version) = config.get("version") {
        object.insert("version".into(), version.clone());
    }
    if config_flag(config, "apply-publisher") {
        if let Some(publisher) = config.pointer("/publisher/name") {
            object.insert("publisher".into(), publisher.clone());
        }
    }
    if config_flag(config, "apply-contact") {
        if let Some(contact) = configured_contact(config) {
            object.insert("contact".into(), contact);
        }
    }
    object
        .entry("date")
        .or_insert_with(|| Value::String(generated_at.into()));
    object
        .entry("status")
        .or_insert_with(|| Value::String("draft".into()));
    resource
}

fn strip_ig_suffix(url: &str) -> String {
    if let Some(position) = url.rfind("/ImplementationGuide/") {
        let tail = &url[position + "/ImplementationGuide/".len()..];
        if !tail.is_empty() && !tail.contains('/') {
            return url[..position].into();
        }
    }
    url.into()
}

fn strip_ig_full_suffix(url: &str) -> String {
    url.find("/ImplementationGuide/")
        .map(|position| url[..position].to_string())
        .unwrap_or_else(|| url.to_string())
}

fn configured_package_id(config: &Value, guide: &Value) -> String {
    field(config, "packageId")
        .or_else(|| field(guide, "packageId"))
        .or_else(|| field(config, "id"))
        .or_else(|| field(guide, "id"))
        .unwrap_or_default()
}

fn fhir_publication_base(version: &str) -> String {
    if version.is_empty() {
        String::new()
    } else if version.starts_with("3.0.") {
        "http://hl7.org/fhir/STU3/".into()
    } else if version.starts_with("4.0.") {
        "http://hl7.org/fhir/R4/".into()
    } else if version.starts_with("4.3.") {
        "http://hl7.org/fhir/R4B/".into()
    } else if version.starts_with("5.") {
        "http://hl7.org/fhir/R5/".into()
    } else {
        format!("http://hl7.org/fhir/{version}/")
    }
}

fn guide_identity(
    config: &Value,
    guide: &Value,
    primary: SemanticResourceKey,
    epoch: i64,
    branch: Option<String>,
    revision: Option<String>,
) -> (GuideIdentity, PublisherCompatibility) {
    let fhir_version = match config.get("fhirVersion") {
        Some(Value::Array(values)) => values
            .first()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        value => value.and_then(scalar_string).unwrap_or_default(),
    };
    let package_id = configured_package_id(config, guide);
    let canonical = config
        .get("canonical")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            guide
                .get("url")
                .and_then(Value::as_str)
                .map(strip_ig_full_suffix)
        })
        .filter(|value| !value.is_empty());
    let name = field(config, "name")
        .or_else(|| field(guide, "name"))
        .filter(|value| !value.is_empty());
    let version = field(config, "version")
        .or_else(|| field(guide, "version"))
        .filter(|value| !value.is_empty());
    let release_label = field(config, "releaseLabel").or_else(|| Some("ci-build".into()));
    let branch = branch.filter(|value| !value.trim().is_empty() && value != "unknown");
    let revision = revision.filter(|value| !value.trim().is_empty() && value != "unknown");
    let source_control = (branch.is_some() || revision.is_some())
        .then_some(SourceControlIdentity { branch, revision });
    (
        GuideIdentity {
            implementation_guide: primary,
            package_id,
            canonical,
            name,
            version,
            fhir_version: fhir_version.clone(),
            release_label,
            fhir_publication_base: fhir_publication_base(&fhir_version),
            generated: GeneratedIdentity {
                epoch_seconds: epoch,
                date: timefmt::gen_date(epoch),
                day: timefmt::gen_day(epoch),
            },
            source_control,
        },
        PublisherCompatibility {
            error_count: "0".into(),
            tooling_version: "site-gen.publisher".into(),
            tooling_revision: "0".into(),
            tooling_version_full: "site-gen.publisher experiment".into(),
        },
    )
}

const STANDARDS_STATUS_EXT: &str =
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status";
const NON_IMPLEMENTABLE_STATUS_TYPES: &[&str] = &[
    "ChargeItemDefinition",
    "Citation",
    "ConditionDefinition",
    "EvidenceReport",
    "EvidenceVariable",
    "ExampleScenario",
    "ObservationDefinition",
];

fn standard_status(resource: &Value) -> Option<String> {
    resource
        .get("extension")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|extension| {
            extension.get("url").and_then(Value::as_str) == Some(STANDARDS_STATUS_EXT)
        })
        .and_then(|extension| extension.get("valueCode"))
        .and_then(scalar_string)
}

fn example_resource(metadata: Option<&Value>) -> bool {
    metadata.is_some_and(|metadata| {
        metadata.get("exampleBoolean") == Some(&Value::Bool(true))
            || matches!(metadata.get("exampleCanonical"), Some(Value::String(_)))
            || matches!(metadata.get("profile"), Some(Value::String(_)))
    })
}

fn current_guide_profile(resource: &Value, canonical: Option<&str>) -> bool {
    canonical.is_some_and(|canonical| {
        resource
            .pointer("/meta/profile")
            .and_then(Value::as_array)
            .is_some_and(|profiles| {
                profiles.iter().any(|profile| {
                    profile.as_str().is_some_and(|profile| {
                        profile.starts_with(&format!("{canonical}/StructureDefinition/"))
                    })
                })
            })
    })
}

fn propagated_status(
    resource: &Value,
    metadata: Option<&Value>,
    guide_status: Option<&str>,
    guide_canonical: Option<&str>,
) -> Option<String> {
    let explicit = standard_status(resource);
    if explicit.is_some() || guide_status.is_none() || example_resource(metadata) {
        return explicit;
    }
    let kind = resource_type(resource);
    let experimental = resource.get("experimental") == Some(&Value::Bool(true));
    if experimental
        && matches!(kind, "CodeSystem" | "Questionnaire" | "ValueSet")
        && current_guide_profile(resource, guide_canonical)
    {
        return None;
    }
    if experimental || NON_IMPLEMENTABLE_STATUS_TYPES.contains(&kind) {
        Some("informative".into())
    } else {
        guide_status.map(str::to_string)
    }
}

struct DependencyCanonicalVersion {
    canonical: String,
    version: String,
    pin_when_multiple: bool,
}

struct CandidateCanonical {
    canonical: String,
    version: String,
    candidate: bool,
}

fn multiple_choice_canonical(canonical: &str, entries: &[CandidateCanonical]) -> bool {
    let exact_versions = entries
        .iter()
        .filter(|entry| entry.canonical == canonical)
        .map(|entry| entry.version.as_str())
        .collect::<HashSet<_>>();
    exact_versions.len() > 1
        || entries.iter().any(|entry| {
            entry.canonical != canonical && entry.canonical.starts_with(&format!("{canonical}/v"))
        })
}

fn dependency_canonical_versions(config: &Value) -> Vec<DependencyCanonicalVersion> {
    let dependencies = match config.get("dependencies") {
        Some(Value::Array(values)) => values.iter().collect::<Vec<_>>(),
        Some(Value::Object(values)) => values.values().collect(),
        _ => Vec::new(),
    };
    let mut entries = dependencies
        .into_iter()
        .filter_map(|dependency| {
            let uri = dependency.get("uri").and_then(scalar_string)?;
            let version = dependency.get("version").and_then(scalar_string)?;
            let canonical = strip_ig_suffix(&uri);
            (canonical != uri).then_some(CandidateCanonical {
                canonical,
                version,
                candidate: true,
            })
        })
        .collect::<Vec<_>>();
    if let Some(resolved) = config
        .get("__publisherPackageCanonicalVersions")
        .and_then(Value::as_array)
    {
        entries.extend(resolved.iter().filter_map(|dependency| {
            Some(CandidateCanonical {
                canonical: dependency.get("canonical").and_then(scalar_string)?,
                version: dependency.get("version").and_then(scalar_string)?,
                candidate: dependency.get("candidate") == Some(&Value::Bool(true)),
            })
        }));
    }
    let mut unique = Vec::new();
    for entry in entries {
        if !unique.iter().any(|existing: &CandidateCanonical| {
            existing.canonical == entry.canonical
                && existing.version == entry.version
                && existing.candidate == entry.candidate
        }) {
            unique.push(entry);
        }
    }
    unique.sort_by(|left, right| {
        right
            .canonical
            .len()
            .cmp(&left.canonical.len())
            .then_with(|| left.canonical.cmp(&right.canonical))
    });
    unique
        .iter()
        .filter(|entry| entry.candidate)
        .map(|entry| DependencyCanonicalVersion {
            canonical: entry.canonical.clone(),
            version: entry.version.clone(),
            pin_when_multiple: multiple_choice_canonical(&entry.canonical, &unique),
        })
        .collect()
}

fn base_definition(resource: &Value, config: &Value) -> Option<String> {
    let base = field(resource, "baseDefinition")?;
    let mode = config
        .pointer("/parameters/pin-canonicals")
        .and_then(Value::as_str);
    if base.contains('|') || !matches!(mode, Some("pin-all" | "pin-multiples")) {
        return Some(base);
    }
    if mode == Some("pin-all") {
        if let (Some(canonical), Some(version)) = (
            config.get("canonical").and_then(Value::as_str),
            field(config, "version"),
        ) {
            if base.starts_with(&format!("{canonical}/")) {
                return Some(format!("{base}|{version}"));
            }
        }
    }
    for dependency in dependency_canonical_versions(config) {
        if base.starts_with(&format!("{}/", dependency.canonical))
            && (mode == Some("pin-all") || dependency.pin_when_multiple)
        {
            return Some(format!("{base}|{}", dependency.version));
        }
    }
    if mode == Some("pin-all") && base.starts_with("http://hl7.org/fhir/StructureDefinition/") {
        let fhir_version = match config.get("fhirVersion") {
            Some(Value::Array(values)) => {
                values.first().and_then(Value::as_str).map(str::to_string)
            }
            value => value.and_then(scalar_string),
        };
        if let Some(fhir_version) = fhir_version {
            return Some(format!("{base}|{fhir_version}"));
        }
    }
    Some(base)
}

fn prepared_resources(
    resources: &[Value],
    metadata: &HashMap<String, Value>,
    config: &Value,
    primary: &SemanticResourceKey,
) -> Vec<SemanticResource> {
    let guide = resources.iter().find(|resource| {
        resource_type(resource) == primary.resource_type
            && resource.get("id").and_then(Value::as_str) == Some(primary.id.as_str())
    });
    let guide_status = guide.and_then(standard_status);
    let guide_canonical = guide
        .and_then(|guide| guide.get("url"))
        .and_then(Value::as_str)
        .map(strip_ig_suffix);
    resources
        .iter()
        .map(|resource| {
            let key = SemanticResourceKey {
                resource_type: resource_type(resource).into(),
                id: resource
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
            };
            let publication_metadata = metadata.get(&resource_ref(resource));
            let canonical =
                matches!(resource.get("url"), Some(Value::String(value)) if !value.is_empty());
            let publication = ResourcePublication {
                display_name: if canonical {
                    field(resource, "name")
                } else {
                    field(resource, "name")
                        .or_else(|| publication_metadata.and_then(|value| field(value, "name")))
                        .or_else(|| field(resource, "title"))
                        .or_else(|| Some(key.id.clone()))
                },
                description: if canonical {
                    field(resource, "description")
                } else {
                    publication_metadata
                        .and_then(|value| field(value, "description"))
                        .or_else(|| field(resource, "description"))
                },
                standard_status: canonical
                    .then(|| {
                        propagated_status(
                            resource,
                            publication_metadata,
                            guide_status.as_deref(),
                            guide_canonical.as_deref(),
                        )
                    })
                    .flatten(),
                base_definition: (key.resource_type == "StructureDefinition")
                    .then(|| base_definition(resource, config))
                    .flatten(),
            };
            SemanticResource {
                key,
                resource: resource.clone(),
                publication: (!publication.is_empty()).then_some(publication),
            }
        })
        .collect()
}

/// Produce complete renderer-neutral guide semantics.
pub fn prepare(input: &PrepareInputs<'_>) -> Result<PreparedGuide> {
    let guide = input.primary_implementation_guide.clone();
    if resource_type(&guide) != "ImplementationGuide" {
        bail!("primary guide input is not an ImplementationGuide resource");
    }
    if input.augmentation.ig != input.primary_implementation_guide {
        bail!("augmentation guide differs from the explicit primary guide");
    }
    let primary = SemanticResourceKey {
        resource_type: "ImplementationGuide".into(),
        id: guide
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("generated ImplementationGuide has no id"))?
            .into(),
    };
    let metadata = guide
        .pointer("/definition/resource")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            entry
                .pointer("/reference/reference")
                .and_then(Value::as_str)
                .map(|reference| (reference.to_string(), entry.clone()))
        })
        .collect::<HashMap<_, _>>();
    let config_yaml: serde_yaml::Value = serde_yaml::from_str(input.sushi_config_yaml)?;
    let config: Value = serde_yaml::from_value(config_yaml)?;

    let mut by_reference = BTreeMap::new();
    for resource in input.generated.iter().chain(input.examples) {
        if !resource_type(resource).is_empty()
            && resource.get("id").and_then(Value::as_str).is_some()
        {
            by_reference.insert(resource_ref(resource), resource.clone());
        }
    }
    let mut ordered = Vec::new();
    let mut seen = HashSet::new();
    let push = |ordered: &mut Vec<Value>, seen: &mut HashSet<String>, resource: Value| {
        if seen.insert(resource_ref(&resource)) {
            ordered.push(resource);
        }
    };
    push(&mut ordered, &mut seen, guide.clone());
    if let Some(entries) = guide
        .pointer("/definition/resource")
        .and_then(Value::as_array)
    {
        for entry in entries {
            if let Some(resource) = entry
                .pointer("/reference/reference")
                .and_then(Value::as_str)
                .and_then(|reference| by_reference.get(reference))
            {
                push(&mut ordered, &mut seen, resource.clone());
            }
        }
    }
    for resource in by_reference.into_values() {
        push(&mut ordered, &mut seen, resource);
    }
    ordered.sort_by(|left, right| {
        type_rank(resource_type(left))
            .cmp(&type_rank(resource_type(right)))
            .then_with(|| {
                left.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .cmp(right.get("id").and_then(Value::as_str).unwrap_or_default())
            })
    });
    let generated_at = timefmt::fhir_datetime(input.build_epoch_secs);
    let resources = ordered
        .into_iter()
        .map(|resource| apply_global_metadata(resource, &config, &generated_at))
        .collect::<Vec<_>>();
    let (identity, compatibility) = guide_identity(
        &config,
        &guide,
        primary.clone(),
        input.build_epoch_secs,
        input.branch.clone(),
        input.revision.clone(),
    );
    let resources = prepared_resources(&resources, &metadata, &config, &primary);
    let augmentation =
        augment::prepare(&input.augmentation).context("authored guide preparation")?;
    Ok(PreparedGuide {
        guide: identity,
        resources,
        publisher_compatibility: Some(compatibility),
        expansions: Vec::new(),
        pages: augmentation.pages,
        menu: augmentation.menu,
        sushi_config: augmentation.sushi_config,
        assets: augmentation.assets,
    })
}
