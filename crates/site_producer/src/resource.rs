//! The lightweight resource model + enumeration. We read the fields the
//! publisher needs to (a) place a resource in the config table and (b) fill
//! `{{[...]}}` layout placeholders — no full FHIR typing required.

use std::path::PathBuf;

use serde_json::Value;

/// One artifact that gets a page. Fields mirror what
/// `IGKnowledgeProvider.doReplacements` / `findConfiguration` read.
#[derive(Debug, Clone)]
pub struct Resource {
    pub rt: String,
    pub id: String,
    /// The resource's `name` child value — the publisher's `FetchedResource`
    /// title (PublisherIGLoader.java:3028 sets `title = name`). Only meaningful
    /// (a string) for canonical resources; complex-typed `name` (e.g.
    /// `Patient.name`) yields `None` and the title falls back to `type/id`
    /// (FetchedResource.getTitle, FetchedResource.java:137).
    pub name: Option<String>,
    pub url: Option<String>,
    pub kind: Option<String>,
    pub derivation: Option<String>,
    pub type_: Option<String>,
    pub abstract_: bool,
    pub is_example: bool,
    /// The full parsed JSON (for `_data` derivation: status/description/etc.).
    pub json: Value,
    /// Logical source path used by the Publisher data model.
    pub file: PathBuf,
}

impl Resource {
    /// `r.getTitle()` — the `{{[title]}}` value (FetchedResource.java:137).
    pub fn title(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("{}/{}", self.rt, self.id))
    }

    /// The base output page filename, `<type>-<id>.html`.
    pub fn base_path(&self) -> String {
        format!("{}-{}.html", self.rt, self.id)
    }

    /// Build a `Resource` from an already-parsed resource `Value` (the wasm /
    /// `Session` path, where the render set lives in memory). `file_name` is the
    /// publisher-style origin filename (`{Type}-{id}.json`) used for the
    /// `resources.json` `source`/`sourceTail` fields. Returns `None` for a Value
    /// without `resourceType` + `id`.
    pub fn from_value(v: Value, file_name: &str, is_example: bool) -> Option<Resource> {
        let rt = v.get("resourceType").and_then(Value::as_str)?.to_string();
        let id = v.get("id").and_then(Value::as_str)?.to_string();
        let name = v.get("name").and_then(Value::as_str).map(str::to_string);
        Some(Resource {
            rt,
            id,
            name,
            url: v.get("url").and_then(Value::as_str).map(str::to_string),
            kind: v.get("kind").and_then(Value::as_str).map(str::to_string),
            derivation: v
                .get("derivation")
                .and_then(Value::as_str)
                .map(str::to_string),
            type_: v.get("type").and_then(Value::as_str).map(str::to_string),
            abstract_: v.get("abstract").and_then(Value::as_bool).unwrap_or(false),
            is_example,
            json: v,
            file: PathBuf::from(file_name),
        })
    }
}
