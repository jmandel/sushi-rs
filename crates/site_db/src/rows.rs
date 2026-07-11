//! S5 — row derivation. A faithful Rust port of the cycle TS producer's
//! `site-gen/publisher/{rows,resource-metadata}.ts`, so that a site.db built by
//! this crate is row-identical to one the TS producer builds over the same
//! inputs (the row-parity oracle, gate ii). Every non-trivial rule cites the TS
//! source line it mirrors.
//!
//! Serialization discipline: the `Json` column must byte-match the TS producer's
//! `JSON.stringify(resource)`. The TS object is the parsed resource with
//! `applyGlobalResourceMetadata` mutations appended (spread preserves original
//! key order; new keys append in assignment order). serde_json's `preserve_order`
//! (IndexMap) replicates this exactly when we apply the same assignment order.

use prepared_guide::{
    GeneratedIdentity, GuideIdentity, PublisherCompatibility, ResourcePublication,
    SemanticResource, SemanticResourceKey, SourceControlIdentity,
};
use serde_json::{json, Map, Value};

use crate::model::{ConceptRow, MetadataRow, ResourceIdentity, ResourceRow, SiteDb};

fn scalar_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn get_scalar_string(obj: &Value, key: &str) -> Option<String> {
    obj.get(key).and_then(scalar_string)
}

fn bool_string(v: &Value, key: &str) -> Option<String> {
    match v.get(key) {
        Some(Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    }
}

fn resource_type(r: &Value) -> &str {
    r.get("resourceType").and_then(Value::as_str).unwrap_or("")
}

/// `${resourceType}/${id}` — rows.ts:137.
pub fn resource_ref(r: &Value) -> String {
    format!(
        "{}/{}",
        resource_type(r),
        r.get("id").and_then(Value::as_str).unwrap_or("")
    )
}

fn has_canonical_url(r: &Value) -> bool {
    matches!(r.get("url"), Some(Value::String(s)) if !s.is_empty())
}

/// rows.ts:133 — pageFor.
fn page_for(type_: &str, id: &str, primary_guide: bool) -> String {
    if primary_guide {
        "index.html".to_string()
    } else {
        format!("{type_}-{id}.html")
    }
}

/// resource-metadata.ts:34 — cfg.parameters?.[name] === true || === 'true'.
fn config_flag(cfg: &Value, name: &str) -> bool {
    match cfg.pointer(&format!("/parameters/{name}")) {
        Some(Value::Bool(true)) => true,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

// ---- resource-metadata.ts: applyGlobalResourceMetadata ----

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

fn is_canonical_resource(r: &Value) -> bool {
    CANONICAL_RESOURCE_TYPES.contains(&resource_type(r))
}

/// resource-metadata.ts:59 — canonicalUrlForResource.
fn canonical_url_for_resource(r: &Value, cfg: &Value) -> Option<String> {
    if !is_canonical_resource(r) {
        return None;
    }
    let id = r.get("id").and_then(Value::as_str)?;
    let canonical = cfg.get("canonical").and_then(Value::as_str)?;
    Some(format!(
        "{}/{}/{}",
        canonical.trim_end_matches('/'),
        resource_type(r),
        id
    ))
}

/// resource-metadata.ts:64 — configuredContact.
fn configured_contact(cfg: &Value) -> Option<Value> {
    if let Some(Value::Array(a)) = cfg.get("contact") {
        if !a.is_empty() {
            return Some(Value::Array(a.clone()));
        }
    }
    let name = cfg.pointer("/publisher/name");
    let url = cfg.pointer("/publisher/url");
    if name.is_none() && url.is_none() {
        return None;
    }
    let mut entry = Map::new();
    if let Some(name) = name {
        entry.insert("name".to_string(), name.clone());
    }
    if let Some(url) = url {
        entry.insert(
            "telecom".to_string(),
            json!([{ "system": "url", "value": url }]),
        );
    }
    Some(Value::Array(vec![Value::Object(entry)]))
}

/// resource-metadata.ts:73 — applyGlobalResourceMetadata. Mutates canonical /
/// url-bearing resources in place (version/contact/date/status/publisher) before
/// rows and the Json blob are derived. `now_fhir` is the injected build
/// timestamp already formatted per `formatFhirDateTime` (never wall clock).
pub fn apply_global_resource_metadata(mut r: Value, cfg: &Value, now_fhir: &str) -> Value {
    if !has_canonical_url(&r) && !is_canonical_resource(&r) {
        return r;
    }
    let obj = r.as_object_mut().expect("resource is an object");
    // if (!hasCanonicalUrl(out)) out.url = canonicalUrlForResource(...)
    let has_url = matches!(obj.get("url"), Some(Value::String(s)) if !s.is_empty());
    if !has_url {
        if let Some(url) = canonical_url_for_resource(&Value::Object(obj.clone()), cfg) {
            obj.insert("url".to_string(), Value::String(url));
        }
    }
    if let Some(version) = cfg.get("version") {
        obj.insert("version".to_string(), version.clone());
    }
    if config_flag(cfg, "apply-publisher") {
        if let Some(name) = cfg.pointer("/publisher/name") {
            obj.insert("publisher".to_string(), name.clone());
        }
    }
    if config_flag(cfg, "apply-contact") {
        if let Some(contact) = configured_contact(cfg) {
            if contact.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                obj.insert("contact".to_string(), contact);
            }
        }
    }
    if obj.get("date").is_none() {
        obj.insert("date".to_string(), Value::String(now_fhir.to_string()));
    }
    if obj.get("status").is_none() {
        obj.insert("status".to_string(), Value::String("draft".to_string()));
    }
    r
}

// ---- rows.ts: standardStatus derivation ----

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

fn extension_value_code(r: &Value, url: &str) -> Option<String> {
    r.get("extension")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|e| e.get("url").and_then(Value::as_str) == Some(url))
        .and_then(|e| e.get("valueCode"))
        .and_then(scalar_string)
}

/// rows.ts:167 — explicit standards-status extension value.
fn standard_status(r: &Value) -> Option<String> {
    extension_value_code(r, STANDARDS_STATUS_EXT)
}

/// rows.ts:171 — isExampleResource (meta from IG definition.resource).
fn is_example_resource(meta: Option<&Value>) -> bool {
    match meta {
        None => false,
        Some(m) => {
            m.get("exampleBoolean") == Some(&Value::Bool(true))
                || matches!(m.get("exampleCanonical"), Some(Value::String(_)))
                || matches!(m.get("profile"), Some(Value::String(_)))
        }
    }
}

fn ig_canonical_base(ig: Option<&Value>) -> Option<String> {
    let url = ig?.get("url").and_then(Value::as_str)?;
    // url.replace(/\/ImplementationGuide\/[^/]+$/, '')
    Some(strip_ig_suffix(url))
}

fn strip_ig_suffix(url: &str) -> String {
    if let Some(pos) = url.rfind("/ImplementationGuide/") {
        let tail = &url[pos + "/ImplementationGuide/".len()..];
        if !tail.is_empty() && !tail.contains('/') {
            return url[..pos].to_string();
        }
    }
    url.to_string()
}

fn has_current_ig_profile(r: &Value, ig_canonical: Option<&str>) -> bool {
    let Some(ig_canonical) = ig_canonical else {
        return false;
    };
    r.pointer("/meta/profile")
        .and_then(Value::as_array)
        .map(|profiles| {
            profiles.iter().any(|p| {
                p.as_str()
                    .map(|s| s.starts_with(&format!("{ig_canonical}/StructureDefinition/")))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// rows.ts:185 — propagatedStandardStatus.
fn propagated_standard_status(
    r: &Value,
    meta: Option<&Value>,
    ig_standard_status: Option<&str>,
    ig_canonical: Option<&str>,
) -> Option<String> {
    let explicit = standard_status(r);
    if explicit.is_some() || ig_standard_status.is_none() || is_example_resource(meta) {
        return explicit;
    }
    let rt = resource_type(r);
    let experimental = r.get("experimental") == Some(&Value::Bool(true));
    if experimental
        && (rt == "CodeSystem" || rt == "Questionnaire" || rt == "ValueSet")
        && has_current_ig_profile(r, ig_canonical)
    {
        return None;
    }
    if experimental || NON_IMPLEMENTABLE_STATUS_TYPES.contains(&rt) {
        return Some("informative".to_string());
    }
    ig_standard_status.map(str::to_string)
}

// ---- rows.ts: base version pinning ----

struct DepCanonicalVersion {
    canonical: String,
    version: String,
    pin_when_multiple: bool,
}

struct CandidateCanonical {
    canonical: String,
    version: String,
    candidate: bool,
}

/// rows.ts:212 — canonicalPinningMode.
fn canonical_pinning_mode(cfg: &Value) -> Option<&'static str> {
    match cfg
        .pointer("/parameters/pin-canonicals")
        .and_then(Value::as_str)
    {
        Some("pin-all") => Some("pin-all"),
        Some("pin-multiples") => Some("pin-multiples"),
        _ => None,
    }
}

/// rows.ts:225 — isMultipleChoiceCanonical.
fn is_multiple_choice_canonical(canonical: &str, entries: &[CandidateCanonical]) -> bool {
    let exact_versions: std::collections::HashSet<&str> = entries
        .iter()
        .filter(|e| e.canonical == canonical)
        .map(|e| e.version.as_str())
        .collect();
    if exact_versions.len() > 1 {
        return true;
    }
    entries
        .iter()
        .any(|e| e.canonical != canonical && e.canonical.starts_with(&format!("{canonical}/v")))
}

/// rows.ts:231 — dependencyCanonicalVersions.
fn dependency_canonical_versions(cfg: &Value) -> Vec<DepCanonicalVersion> {
    let mut from_config: Vec<CandidateCanonical> = Vec::new();
    let deps = cfg.get("dependencies");
    let dep_values: Vec<&Value> = match deps {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(Value::Object(o)) => o.values().collect(),
        _ => Vec::new(),
    };
    for dep in dep_values {
        let uri = dep.get("uri").and_then(scalar_string);
        let version = dep.get("version").and_then(scalar_string);
        if let (Some(uri), Some(version)) = (uri, version) {
            let canonical = strip_ig_suffix(&uri);
            if canonical != uri {
                from_config.push(CandidateCanonical {
                    canonical,
                    version,
                    candidate: true,
                });
            }
        }
    }
    let mut from_resolved: Vec<CandidateCanonical> = Vec::new();
    if let Some(Value::Array(a)) = cfg.get("__publisherPackageCanonicalVersions") {
        for dep in a {
            let canonical = dep.get("canonical").and_then(scalar_string);
            let version = dep.get("version").and_then(scalar_string);
            if let (Some(canonical), Some(version)) = (canonical, version) {
                from_resolved.push(CandidateCanonical {
                    canonical,
                    version,
                    candidate: dep.get("candidate") == Some(&Value::Bool(true)),
                });
            }
        }
    }
    let mut entries: Vec<CandidateCanonical> = Vec::new();
    for e in from_config.into_iter().chain(from_resolved) {
        if !entries.iter().any(|o| {
            o.canonical == e.canonical && o.version == e.version && o.candidate == e.candidate
        }) {
            entries.push(e);
        }
    }
    // sort: b.canonical.length - a.canonical.length || a.canonical.localeCompare(b.canonical)
    entries.sort_by(|a, b| {
        b.canonical
            .len()
            .cmp(&a.canonical.len())
            .then_with(|| a.canonical.cmp(&b.canonical))
    });
    entries
        .iter()
        .filter(|e| e.candidate)
        .map(|e| DepCanonicalVersion {
            canonical: e.canonical.clone(),
            version: e.version.clone(),
            pin_when_multiple: is_multiple_choice_canonical(&e.canonical, &entries),
        })
        .collect()
}

/// rows.ts:199 — baseDefinitionForDb.
fn base_definition_for_db(r: &Value, cfg: &Value) -> Option<String> {
    let base = get_scalar_string(r, "baseDefinition")?;
    let pin_mode = canonical_pinning_mode(cfg);
    if base.contains('|') || pin_mode.is_none() {
        return Some(base);
    }
    let pin_mode = pin_mode.unwrap();
    let fhir_version = match cfg.get("fhirVersion") {
        Some(Value::Array(a)) => a.first().and_then(Value::as_str).map(str::to_string),
        v => v.and_then(scalar_string),
    };
    let canonical = cfg.get("canonical").and_then(Value::as_str);
    if pin_mode == "pin-all" {
        if let (Some(canonical), Some(version)) = (canonical, cfg.get("version")) {
            if base.starts_with(&format!("{canonical}/")) {
                if let Some(v) = scalar_string(version) {
                    return Some(format!("{base}|{v}"));
                }
            }
        }
    }
    for dep in dependency_canonical_versions(cfg) {
        if base.starts_with(&format!("{}/", dep.canonical))
            && (pin_mode == "pin-all" || dep.pin_when_multiple)
        {
            return Some(format!("{base}|{}", dep.version));
        }
    }
    if pin_mode == "pin-all" && base.starts_with("http://hl7.org/fhir/StructureDefinition/") {
        if let Some(fv) = fhir_version {
            return Some(format!("{base}|{fv}"));
        }
    }
    Some(base)
}

// ---- rows.ts: id/url/name/description helpers ----

fn configured_package_id(cfg: &Value, ig: Option<&Value>) -> String {
    get_scalar_string(cfg, "packageId")
        .or_else(|| ig.and_then(|i| get_scalar_string(i, "packageId")))
        .or_else(|| get_scalar_string(cfg, "id"))
        .or_else(|| ig.and_then(|i| get_scalar_string(i, "id")))
        .unwrap_or_default()
}

fn resource_row_id(r: &Value, cfg: &Value, primary_guide: bool) -> String {
    if primary_guide {
        let pkg = configured_package_id(cfg, Some(r));
        if !pkg.is_empty() {
            return pkg;
        }
        return r
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
    }
    r.get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn resource_row_url(r: &Value, cfg: &Value, id: &str, primary_guide: bool) -> Option<String> {
    if primary_guide {
        if let Some(canonical) = cfg.get("canonical").and_then(Value::as_str) {
            if !id.is_empty() {
                return Some(format!(
                    "{}/ImplementationGuide/{}",
                    canonical.trim_end_matches('/'),
                    id
                ));
            }
        }
    }
    if has_canonical_url(r) {
        r.get("url").and_then(Value::as_str).map(str::to_string)
    } else {
        None
    }
}

fn display_name(r: &Value, meta: Option<&Value>) -> Option<String> {
    get_scalar_string(r, "name")
        .or_else(|| meta.and_then(|m| get_scalar_string(m, "name")))
        .or_else(|| get_scalar_string(r, "title"))
        .or_else(|| r.get("id").and_then(Value::as_str).map(str::to_string))
        .or_else(|| Some(resource_type(r).to_string()))
}

// ---- Metadata rows ----

pub struct MetadataInputs<'a> {
    pub cfg: &'a Value,
    pub ig: &'a Value,
    pub gen_date: String,
    pub gen_day: String,
    pub build_epoch_secs: i64,
    pub branch: Option<String>,
    pub revision: Option<String>,
}

fn metadata_pairs(input: &MetadataInputs) -> Vec<(&'static str, String)> {
    let cfg = input.cfg;
    let ig = input.ig;
    let fhir_version = match cfg.get("fhirVersion") {
        Some(Value::Array(a)) => a.first().and_then(Value::as_str).unwrap_or("").to_string(),
        v => v.and_then(scalar_string).unwrap_or_default(),
    };
    let package_id = configured_package_id(cfg, Some(ig));
    let canonical = cfg
        .get("canonical")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            ig.get("url")
                .and_then(Value::as_str)
                .map(|u| strip_ig_dotfull_suffix(u))
        })
        .unwrap_or_default();
    let ig_name = get_scalar_string(cfg, "name")
        .or_else(|| get_scalar_string(ig, "name"))
        .unwrap_or_default();
    let ig_ver = get_scalar_string(cfg, "version")
        .or_else(|| get_scalar_string(ig, "version"))
        .unwrap_or_default();
    let release_label = get_scalar_string(cfg, "releaseLabel").unwrap_or_else(|| "ci-build".into());
    let revision = input.revision.clone().unwrap_or_else(|| "unknown".into());
    let path = if fhir_version.is_empty() {
        String::new()
    } else {
        fhir_publication_base_for_version(&fhir_version)
    };
    let version_full = if fhir_version.is_empty() {
        revision.clone()
    } else {
        format!("{fhir_version}-{revision}")
    };
    vec![
        ("path", path),
        ("canonical", canonical),
        ("igId", package_id.clone()),
        ("igName", ig_name),
        ("packageId", package_id),
        ("igVer", ig_ver),
        ("errorCount", "0".into()),
        ("version", fhir_version.clone()),
        ("releaseLabel", release_label),
        ("revision", revision.clone()),
        ("versionFull", version_full),
        ("toolingVersion", "site-gen.publisher".into()),
        ("toolingRevision", "0".into()),
        ("toolingVersionFull", "site-gen.publisher experiment".into()),
        ("genDate", input.gen_date.clone()),
        ("genDay", input.gen_day.clone()),
        (
            "gitstatus",
            input.branch.clone().unwrap_or_else(|| "unknown".into()),
        ),
    ]
}

/// rows.ts:269 — deriveMetadataRows. Values are order-preserving.
pub fn derive_metadata_rows(input: &MetadataInputs) -> Vec<MetadataRow> {
    metadata_pairs(input)
        .into_iter()
        .enumerate()
        .map(|(i, (name, value))| MetadataRow {
            key: (i + 1) as i64,
            name: name.to_string(),
            value,
        })
        .collect()
}

/// Renderer-neutral identity derived from the same semantic values as the
/// compatibility Metadata rows, before those rows enter a SiteDb.
pub fn derive_prepared_identity(
    input: &MetadataInputs,
    primary_implementation_guide: SemanticResourceKey,
) -> (GuideIdentity, PublisherCompatibility) {
    let values: std::collections::HashMap<_, _> = metadata_pairs(input).into_iter().collect();
    let optional = |name: &str| {
        values
            .get(name)
            .filter(|value| !value.trim().is_empty() && value.as_str() != "unknown")
            .cloned()
    };
    let required = |name: &str| values.get(name).cloned().unwrap_or_default();
    let branch = optional("gitstatus");
    let revision = optional("revision");
    let source_control = (branch.is_some() || revision.is_some())
        .then_some(SourceControlIdentity { branch, revision });
    (
        GuideIdentity {
            implementation_guide: primary_implementation_guide,
            package_id: required("packageId"),
            canonical: optional("canonical"),
            name: optional("igName"),
            version: optional("igVer"),
            fhir_version: required("version"),
            release_label: optional("releaseLabel"),
            fhir_publication_base: required("path"),
            generated: GeneratedIdentity {
                epoch_seconds: input.build_epoch_secs,
                date: required("genDate"),
                day: required("genDay"),
            },
            source_control,
        },
        PublisherCompatibility {
            error_count: required("errorCount"),
            tooling_version: required("toolingVersion"),
            tooling_revision: required("toolingRevision"),
            tooling_version_full: required("toolingVersionFull"),
        },
    )
}

fn strip_ig_dotfull_suffix(url: &str) -> String {
    // ig.url.replace(/\/ImplementationGuide\/.+$/, '')  (greedy: strips at first
    // occurrence of the segment)
    if let Some(pos) = url.find("/ImplementationGuide/") {
        return url[..pos].to_string();
    }
    url.to_string()
}

/// fhir-versions.ts:7 — fhirPublicationBaseForVersion (trailing slash included).
fn fhir_publication_base_for_version(v: &str) -> String {
    if v.starts_with("3.0.") {
        "http://hl7.org/fhir/STU3/".to_string()
    } else if v.starts_with("4.0.") {
        "http://hl7.org/fhir/R4/".to_string()
    } else if v.starts_with("4.3.") {
        "http://hl7.org/fhir/R4B/".to_string()
    } else if v.starts_with("5.") {
        "http://hl7.org/fhir/R5/".to_string()
    } else if v.starts_with("6.") {
        format!("http://hl7.org/fhir/{v}/")
    } else {
        format!("http://hl7.org/fhir/{v}/")
    }
}

// ---- Resource rows ----

/// Renderer-neutral resource semantics derived directly from prepared resource
/// values. Compatibility row ids and JSON-string columns never enter this path.
pub fn derive_prepared_resources(
    resources: &[Value],
    resource_meta: &std::collections::HashMap<String, Value>,
    cfg: &Value,
    primary_implementation_guide: &ResourceIdentity,
) -> Vec<SemanticResource> {
    let ig = resources.iter().find(|resource| {
        resource_type(resource) == primary_implementation_guide.resource_type
            && resource.get("id").and_then(Value::as_str)
                == Some(primary_implementation_guide.id.as_str())
    });
    let ig_standard_status = ig.and_then(standard_status);
    let ig_canonical = ig_canonical_base(ig);
    resources
        .iter()
        .map(|resource| {
            let reference = resource_ref(resource);
            let meta = resource_meta.get(&reference);
            let canonical = has_canonical_url(resource);
            let resource_type = resource_type(resource).to_string();
            let publication = ResourcePublication {
                display_name: if canonical {
                    get_scalar_string(resource, "name")
                } else {
                    display_name(resource, meta)
                },
                description: if canonical {
                    get_scalar_string(resource, "description")
                } else {
                    meta.and_then(|value| get_scalar_string(value, "description"))
                        .or_else(|| get_scalar_string(resource, "description"))
                },
                standard_status: canonical
                    .then(|| {
                        propagated_standard_status(
                            resource,
                            meta,
                            ig_standard_status.as_deref(),
                            ig_canonical.as_deref(),
                        )
                    })
                    .flatten(),
                base_definition: (resource_type == "StructureDefinition")
                    .then(|| base_definition_for_db(resource, cfg))
                    .flatten(),
            };
            SemanticResource {
                key: SemanticResourceKey {
                    resource_type,
                    id: resource
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                },
                resource: resource.clone(),
                publication: (!publication.is_empty()).then_some(publication),
            }
        })
        .collect()
}

/// rows.ts:301 — deriveResourceRows. Returns rows + keyByRef map for concept
/// linkage. `resources` is the ordered, metadata-applied resource list; `json`
/// is the byte-stable serialization of each resource (index-aligned).
pub fn derive_resource_rows(
    resources: &[Value],
    resource_meta: &std::collections::HashMap<String, Value>,
    cfg: &Value,
    json_by_index: &[String],
    primary_implementation_guide: &ResourceIdentity,
) -> (Vec<ResourceRow>, std::collections::HashMap<String, i64>) {
    let ig = resources.iter().find(|r| {
        resource_type(r) == primary_implementation_guide.resource_type
            && r.get("id").and_then(Value::as_str) == Some(primary_implementation_guide.id.as_str())
    });
    let ig_standard_status = ig.and_then(|ig| standard_status(ig));
    let ig_canonical = ig_canonical_base(ig);
    let mut rows = Vec::with_capacity(resources.len());
    let mut key_by_ref = std::collections::HashMap::new();
    for (i, r) in resources.iter().enumerate() {
        let key = (i + 1) as i64;
        key_by_ref.insert(resource_ref(r), key);
        let meta = resource_meta.get(&resource_ref(r));
        let canonical_resource = has_canonical_url(r);
        let rt = resource_type(r).to_string();
        let primary_guide = rt == primary_implementation_guide.resource_type
            && r.get("id").and_then(Value::as_str)
                == Some(primary_implementation_guide.id.as_str());
        let row_id = resource_row_id(r, cfg, primary_guide);
        rows.push(ResourceRow {
            key,
            web: page_for(&rt, &row_id, primary_guide),
            url: resource_row_url(r, cfg, &row_id, primary_guide),
            version: if canonical_resource {
                get_scalar_string(r, "version")
            } else {
                None
            },
            status: get_scalar_string(r, "status"),
            date: if canonical_resource {
                get_scalar_string(r, "date")
            } else {
                None
            },
            name: if canonical_resource {
                get_scalar_string(r, "name")
            } else {
                display_name(r, meta)
            },
            title: get_scalar_string(r, "title"),
            experimental: bool_string(r, "experimental"),
            realm: None,
            description: if canonical_resource {
                get_scalar_string(r, "description")
            } else {
                meta.and_then(|m| get_scalar_string(m, "description"))
                    .or_else(|| get_scalar_string(r, "description"))
            },
            purpose: get_scalar_string(r, "purpose"),
            copyright: get_scalar_string(r, "copyright"),
            copyright_label: get_scalar_string(r, "copyrightLabel"),
            derivation: get_scalar_string(r, "derivation"),
            standard_status: if canonical_resource {
                propagated_standard_status(
                    r,
                    meta,
                    ig_standard_status.as_deref(),
                    ig_canonical.as_deref(),
                )
            } else {
                None
            },
            kind: if rt == "StructureDefinition" {
                get_scalar_string(r, "kind")
            } else {
                None
            },
            sd_type: if rt == "StructureDefinition" {
                get_scalar_string(r, "type")
            } else {
                None
            },
            base: if rt == "StructureDefinition" {
                base_definition_for_db(r, cfg)
            } else {
                None
            },
            content: get_scalar_string(r, "content"),
            supplements: get_scalar_string(r, "supplements"),
            custom: 0,
            id: row_id,
            type_: rt,
            json: json_by_index[i].clone(),
        });
    }
    (rows, key_by_ref)
}

// ---- Concept rows ----

/// rows.ts:344 — deriveConceptRows (flatten CodeSystem concept[] w/ ParentKey).
pub fn derive_concept_rows(
    resources: &[Value],
    key_by_ref: &std::collections::HashMap<String, i64>,
) -> Vec<ConceptRow> {
    let mut rows: Vec<ConceptRow> = Vec::new();
    fn walk(
        rows: &mut Vec<ConceptRow>,
        resource_key: i64,
        concepts: &Value,
        parent_key: Option<i64>,
    ) {
        let Some(arr) = concepts.as_array() else {
            return;
        };
        for c in arr {
            let key = (rows.len() + 1) as i64;
            rows.push(ConceptRow {
                key,
                resource_key,
                parent_key,
                code: get_scalar_string(c, "code"),
                display: get_scalar_string(c, "display"),
                definition: get_scalar_string(c, "definition"),
            });
            if let Some(children) = c.get("concept") {
                walk(rows, resource_key, children, Some(key));
            }
        }
    }
    for r in resources
        .iter()
        .filter(|r| resource_type(r) == "CodeSystem")
    {
        if let Some(&resource_key) = key_by_ref.get(&resource_ref(r)) {
            if let Some(concepts) = r.get("concept") {
                walk(&mut rows, resource_key, concepts, None);
            }
        }
    }
    rows
}

/// Populate metadata/resources/concepts on a SiteDb. ValueSet_Codes is left
/// empty (S4 deferred, per §4b — cycle needs zero expansions).
pub fn populate_core_rows(
    db: &mut SiteDb,
    metadata: Vec<MetadataRow>,
    resource_rows: Vec<ResourceRow>,
    concept_rows: Vec<ConceptRow>,
) {
    db.metadata = metadata;
    db.resources = resource_rows;
    db.concepts = concept_rows;
}
