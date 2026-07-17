use std::collections::BTreeMap;

use serde_json::Value;
use thiserror::Error;

const MAX_SNAPSHOT_RESOURCES: usize = 20_000;
const MAX_SNAPSHOT_ROWS: usize = 250_000;
const MAX_SNAPSHOT_LOGICAL_BYTES: usize = 64 * 1024 * 1024;
const ROW_LOGICAL_OVERHEAD: usize = 64;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum QuerySnapshotError {
    #[error("Publisher SQL snapshot contains more than {MAX_SNAPSHOT_RESOURCES} resources")]
    ResourceLimit,
    #[error("Publisher SQL snapshot contains more than {MAX_SNAPSHOT_ROWS} relational rows")]
    RowLimit,
    #[error("Publisher SQL snapshot exceeds {MAX_SNAPSHOT_LOGICAL_BYTES} logical bytes")]
    ByteLimit,
    #[error("Publisher SQL snapshot could not serialize a FHIR resource: {0}")]
    Serialization(String),
}

#[derive(Clone, Debug)]
pub(crate) struct ResourceRow {
    pub key: i64,
    pub resource_type: String,
    pub custom: i64,
    pub id: String,
    pub web: String,
    pub url: Option<String>,
    pub version: Option<String>,
    pub status: Option<String>,
    pub date: Option<String>,
    pub name: Option<String>,
    pub title: Option<String>,
    pub experimental: Option<String>,
    pub realm: Option<String>,
    pub description: Option<String>,
    pub purpose: Option<String>,
    pub copyright: Option<String>,
    pub copyright_label: Option<String>,
    pub derivation: Option<String>,
    pub standard_status: Option<String>,
    pub kind: Option<String>,
    pub sd_type: Option<String>,
    pub base: Option<String>,
    pub content: Option<String>,
    pub supplements: Option<String>,
    pub json: Vec<u8>,
}

#[derive(Clone, Debug)]
pub(crate) struct PropertyRow {
    pub key: i64,
    pub resource_key: i64,
    pub code: String,
    pub uri: Option<String>,
    pub description: Option<String>,
    pub property_type: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ConceptRow {
    pub key: i64,
    pub resource_key: i64,
    pub parent_key: Option<i64>,
    pub code: Option<String>,
    pub display: Option<String>,
    pub definition: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ConceptPropertyRow {
    pub key: i64,
    pub resource_key: i64,
    pub concept_key: i64,
    pub property_key: Option<i64>,
    pub code: Option<String>,
    pub value: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct DesignationRow {
    pub key: i64,
    pub resource_key: i64,
    pub concept_key: i64,
    pub use_system: Option<String>,
    pub use_code: Option<String>,
    pub language: Option<String>,
    pub value: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ConceptMappingRow {
    pub key: i64,
    pub resource_key: i64,
    pub source_system: Option<String>,
    pub source_version: Option<String>,
    pub source_code: Option<String>,
    pub relationship: Option<String>,
    pub target_system: Option<String>,
    pub target_version: Option<String>,
    pub target_code: Option<String>,
}

/// Canonical, target-neutral rows for the currently supported own-resource
/// subset. Native and WASM materialize the same rows; database-file bytes are
/// intentionally not the identity.
#[derive(Clone, Debug)]
pub(crate) struct QuerySnapshot {
    pub(crate) resources: Vec<ResourceRow>,
    pub(crate) properties: Vec<PropertyRow>,
    pub(crate) concepts: Vec<ConceptRow>,
    pub(crate) concept_properties: Vec<ConceptPropertyRow>,
    pub(crate) designations: Vec<DesignationRow>,
    pub(crate) concept_mappings: Vec<ConceptMappingRow>,
}

impl QuerySnapshot {
    /// Project the exact ordered compiled-resource set. Callers provide the
    /// same values used to construct Publisher render semantics; no ambient
    /// package or filesystem lookup is permitted here.
    pub(crate) fn from_resources<'a>(
        resources: impl IntoIterator<Item = &'a Value>,
    ) -> Result<Self, QuerySnapshotError> {
        let resources = resources.into_iter().collect::<Vec<_>>();
        let resources = preflight(&resources)?;
        let mut snapshot = Self {
            resources: Vec::new(),
            properties: Vec::new(),
            concepts: Vec::new(),
            concept_properties: Vec::new(),
            designations: Vec::new(),
            concept_mappings: Vec::new(),
        };

        for (resource, json) in resources {
            snapshot.push_resource(resource, json);
        }
        Ok(snapshot)
    }

    fn push_resource(&mut self, resource: &Value, json: Vec<u8>) {
        let Some(resource_type) = text(resource, "resourceType") else {
            return;
        };
        let Some(id) = text(resource, "id") else {
            return;
        };
        let resource_key = self.resources.len() as i64 + 1;
        let web = format!("{resource_type}-{id}.html");
        self.resources.push(ResourceRow {
            key: resource_key,
            resource_type: resource_type.clone(),
            custom: 0,
            id,
            web,
            url: text(resource, "url"),
            version: text(resource, "version"),
            status: text(resource, "status"),
            date: scalar_text(resource.get("date")),
            name: text(resource, "name"),
            title: text(resource, "title"),
            experimental: scalar_text(resource.get("experimental")),
            realm: None,
            description: text(resource, "description"),
            purpose: text(resource, "purpose"),
            copyright: text(resource, "copyright"),
            copyright_label: text(resource, "copyrightLabel"),
            derivation: (resource_type == "StructureDefinition")
                .then(|| text(resource, "derivation"))
                .flatten(),
            standard_status: extension_code(
                resource,
                "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status",
            ),
            kind: (resource_type == "StructureDefinition")
                .then(|| text(resource, "kind"))
                .flatten(),
            sd_type: (resource_type == "StructureDefinition")
                .then(|| text(resource, "type"))
                .flatten(),
            base: (resource_type == "StructureDefinition")
                .then(|| text(resource, "baseDefinition"))
                .flatten(),
            content: (resource_type == "CodeSystem")
                .then(|| text(resource, "content"))
                .flatten(),
            supplements: (resource_type == "CodeSystem")
                .then(|| text(resource, "supplements"))
                .flatten(),
            json,
        });

        if resource_type == "CodeSystem" {
            self.push_code_system(resource_key, resource);
        } else if resource_type == "ConceptMap" {
            self.push_concept_map(resource_key, resource);
        }
    }

    fn push_code_system(&mut self, resource_key: i64, resource: &Value) {
        let mut property_keys = BTreeMap::new();
        for property in array(resource, "property") {
            let Some(code) = text(property, "code") else {
                continue;
            };
            let key = self.properties.len() as i64 + 1;
            property_keys.insert(code.clone(), key);
            self.properties.push(PropertyRow {
                key,
                resource_key,
                code,
                uri: text(property, "uri"),
                description: text(property, "description"),
                property_type: text(property, "type"),
            });
        }
        self.push_concepts(
            resource_key,
            None,
            array(resource, "concept"),
            &property_keys,
        );
    }

    fn push_concepts(
        &mut self,
        resource_key: i64,
        parent_key: Option<i64>,
        concepts: &[Value],
        property_keys: &BTreeMap<String, i64>,
    ) {
        for concept in concepts {
            let concept_key = self.concepts.len() as i64 + 1;
            self.concepts.push(ConceptRow {
                key: concept_key,
                resource_key,
                parent_key,
                code: text(concept, "code"),
                display: text(concept, "display"),
                definition: text(concept, "definition"),
            });

            for property in array(concept, "property") {
                let code = text(property, "code");
                let value = property
                    .as_object()
                    .and_then(|object| {
                        object
                            .iter()
                            .find(|(name, _)| name.starts_with("value"))
                            .map(|(_, value)| value)
                    })
                    .and_then(|value| scalar_text(Some(value)));
                let property_key = code
                    .as_ref()
                    .and_then(|code| property_keys.get(code).copied());
                self.concept_properties.push(ConceptPropertyRow {
                    key: self.concept_properties.len() as i64 + 1,
                    resource_key,
                    concept_key,
                    property_key,
                    code,
                    value,
                });
            }

            for designation in array(concept, "designation") {
                let use_coding = designation.get("use");
                self.designations.push(DesignationRow {
                    key: self.designations.len() as i64 + 1,
                    resource_key,
                    concept_key,
                    use_system: use_coding.and_then(|value| text(value, "system")),
                    use_code: use_coding.and_then(|value| text(value, "code")),
                    language: text(designation, "language"),
                    value: text(designation, "value"),
                });
            }

            self.push_concepts(
                resource_key,
                Some(concept_key),
                array(concept, "concept"),
                property_keys,
            );
        }
    }

    fn push_concept_map(&mut self, resource_key: i64, resource: &Value) {
        for group in array(resource, "group") {
            for source in array(group, "element") {
                for target in array(source, "target") {
                    self.concept_mappings.push(ConceptMappingRow {
                        key: self.concept_mappings.len() as i64 + 1,
                        resource_key,
                        source_system: text(group, "source"),
                        source_version: text(group, "sourceVersion"),
                        source_code: text(source, "code"),
                        relationship: text(target, "relationship")
                            .or_else(|| text(target, "equivalence")),
                        target_system: text(group, "target"),
                        target_version: text(group, "targetVersion"),
                        target_code: text(target, "code"),
                    });
                }
            }
        }
    }
}

fn preflight<'a>(resources: &[&'a Value]) -> Result<Vec<(&'a Value, Vec<u8>)>, QuerySnapshotError> {
    let mut resource_count = 0usize;
    let mut row_count = 0usize;
    let mut logical_bytes = 0usize;
    let mut admitted = Vec::new();
    for resource in resources {
        if text(resource, "resourceType").is_none() || text(resource, "id").is_none() {
            continue;
        }
        resource_count = resource_count.saturating_add(1);
        if resource_count > MAX_SNAPSHOT_RESOURCES {
            return Err(QuerySnapshotError::ResourceLimit);
        }
        row_count = row_count.saturating_add(1);
        let json = serde_json::to_vec(resource)
            .map_err(|error| QuerySnapshotError::Serialization(error.to_string()))?;
        logical_bytes = logical_bytes.saturating_add(json.len());
        match text(resource, "resourceType").as_deref() {
            Some("CodeSystem") => {
                row_count = row_count.saturating_add(array(resource, "property").len());
                let mut pending = array(resource, "concept").iter().collect::<Vec<_>>();
                while let Some(concept) = pending.pop() {
                    row_count = row_count
                        .saturating_add(1)
                        .saturating_add(array(concept, "property").len())
                        .saturating_add(array(concept, "designation").len());
                    pending.extend(array(concept, "concept"));
                }
            }
            Some("ConceptMap") => {
                for group in array(resource, "group") {
                    for source in array(group, "element") {
                        row_count = row_count.saturating_add(array(source, "target").len());
                    }
                }
            }
            _ => {}
        }
        if row_count > MAX_SNAPSHOT_ROWS {
            return Err(QuerySnapshotError::RowLimit);
        }
        if logical_bytes.saturating_add(row_count.saturating_mul(ROW_LOGICAL_OVERHEAD))
            > MAX_SNAPSHOT_LOGICAL_BYTES
        {
            return Err(QuerySnapshotError::ByteLimit);
        }
        admitted.push((*resource, json));
    }
    Ok(admitted)
}

fn array<'a>(value: &'a Value, name: &str) -> &'a [Value] {
    value
        .get(name)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn text(value: &Value, name: &str) -> Option<String> {
    value.get(name).and_then(Value::as_str).map(str::to_string)
}

fn scalar_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn extension_code(resource: &Value, url: &str) -> Option<String> {
    array(resource, "extension").iter().find_map(|extension| {
        (extension.get("url").and_then(Value::as_str) == Some(url))
            .then(|| {
                extension
                    .as_object()?
                    .iter()
                    .find(|(name, _)| name.starts_with("value"))
                    .and_then(|(_, value)| scalar_text(Some(value)))
            })
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn code_system_rows_are_preorder_and_repeatable() {
        let resource = json!({
            "resourceType": "CodeSystem",
            "id": "stages",
            "url": "https://example.org/CodeSystem/stages",
            "status": "active",
            "content": "complete",
            "concept": [
                {"code": "author", "display": "Author", "concept": [
                    {"code": "edit", "display": "Edit"}
                ]},
                {"code": "preview", "display": "Preview"}
            ]
        });
        let first = QuerySnapshot::from_resources([&resource]).unwrap();
        let second = QuerySnapshot::from_resources([&resource]).unwrap();
        assert_eq!(first.resources.len(), second.resources.len());
        assert_eq!(first.concepts.len(), second.concepts.len());
        assert_eq!(first.resources.len(), 1);
        assert_eq!(first.concepts.len(), 3);
        assert_eq!(first.concepts[0].code.as_deref(), Some("author"));
        assert_eq!(first.concepts[1].parent_key, Some(1));
        assert_eq!(first.concepts[2].code.as_deref(), Some("preview"));
    }
}
