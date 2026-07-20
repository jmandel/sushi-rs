//! The template `config.json` `defaults` type→layout+filename table, and the
//! `extraTemplates` list. Mirrors `org.hl7.fhir.igtools.templates.Template`
//! (`getConfig`, Template.java:539) and `IGKnowledgeProvider.getProperty`
//! (IGKnowledgeProvider.java:255) precedence.

use anyhow::{Context, Result};
use serde_json::Value;

/// The parsed resource projection fields from the template config.
#[derive(Debug, Clone)]
pub struct Defaults {
    /// The `defaults` object: key (e.g. `StructureDefinition`,
    /// `StructureDefinition:extension`, `Any`, `example`) → its config object.
    pub table: Value,
    /// `extraTemplates` names in declaration order (e.g. mappings, testing,
    /// examples, format, profile-history, change-history). Note: the publisher
    /// (`PublisherIGLoader.java:1266`) injects `defns` + `format` if absent; the
    /// stock config already declares `format`, and `defns` is handled explicitly
    /// by `makeTemplates`, so this list is the declared set.
    pub extra_templates: Vec<String>,
    /// Resource serialization formats requested by the template's root
    /// `formats` array, in declaration order. These drive the Publisher's
    /// separate `template-format` pass; they are not ordinary extra templates.
    pub formats: Vec<String>,
}

impl Defaults {
    pub fn from_value(cfg: &Value) -> Result<Defaults> {
        let table = cfg
            .get("defaults")
            .cloned()
            .context("template config.json has no `defaults`")?;
        let mut extra_templates = Vec::new();
        if let Some(arr) = cfg.get("extraTemplates").and_then(Value::as_array) {
            for t in arr {
                let name = match t {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(_) => t.get("name").and_then(Value::as_str).map(str::to_string),
                    _ => None,
                };
                if let Some(n) = name {
                    extra_templates.push(n);
                }
            }
        }
        let formats = cfg
            .get("formats")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        Ok(Defaults {
            table,
            extra_templates,
            formats,
        })
    }

    fn obj(&self, key: &str) -> Option<&Value> {
        self.table.get(key).filter(|v| v.is_object())
    }

    fn obj_str<'a>(o: Option<&'a Value>, prop: &str) -> Option<&'a str> {
        o.and_then(|c| c.get(prop)).and_then(Value::as_str)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn root_formats_are_distinct_from_extra_templates_and_keep_order() {
        let defaults = Defaults::from_value(&json!({
            "defaults": {},
            "formats": ["xml", "json", 3, "ttl"],
            "extraTemplates": ["testing", {"name": "format"}]
        }))
        .unwrap();

        assert_eq!(defaults.formats, ["xml", "json", "ttl"]);
        assert_eq!(defaults.extra_templates, ["testing", "format"]);
    }
}

/// The StructureDefinition flavor used to index `StructureDefinition:<flavor>`.
/// Port of `IGKnowledgeProvider.getSDType(FetchedResource)`
/// (IGKnowledgeProvider.java:293).
pub fn sd_type(
    type_: Option<&str>,
    kind: Option<&str>,
    derivation: Option<&str>,
    abstract_: bool,
) -> String {
    if type_ == Some("Extension") {
        return "extension".to_string();
    }
    if kind == Some("resource") && derivation == Some("specialization") {
        return "resourcedefn".to_string();
    }
    let k = kind.unwrap_or("");
    if abstract_ {
        format!("{k}:abstract")
    } else {
        k.to_string()
    }
}

impl Defaults {
    /// `findConfiguration` (IGKnowledgeProvider.java:417): pick the config object
    /// for a resource. Returns the config-object key that applies (so callers can
    /// re-run property precedence), plus the resolved object.
    pub fn find_config<'a>(&'a self, r: &crate::Resource) -> Option<&'a Value> {
        // StructureDefinition:<flavor>
        if r.rt == "StructureDefinition" {
            let flavor = sd_type(
                r.type_.as_deref(),
                r.kind.as_deref(),
                r.derivation.as_deref(),
                r.abstract_,
            );
            if let Some(c) = self.obj(&format!("StructureDefinition:{flavor}")) {
                return Some(c);
            }
        }
        // template.getConfig(type, id, false) => defaults[type]
        if let Some(c) = self.obj(&r.rt) {
            return Some(c);
        }
        // example fallback
        if r.is_example {
            if let Some(c) = self.obj("example") {
                return Some(c);
            }
        }
        // template.getConfig(type, id, true) => defaults[type] else Any
        self.obj(&r.rt).or_else(|| self.obj("Any"))
    }

    /// `getProperty` precedence (IGKnowledgeProvider.java:255): resource's own
    /// config (== `find_config`), then `StructureDefinition:<flavor>`, then the
    /// type default, then `Any`.
    pub fn get_property(&self, r: &crate::Resource, prop: &str) -> Option<String> {
        // 1. resource's own config
        if let Some(v) = Self::obj_str(self.find_config(r), prop) {
            return Some(v.to_string());
        }
        // 2. StructureDefinition:<flavor>
        if r.rt == "StructureDefinition" {
            let flavor = sd_type(
                r.type_.as_deref(),
                r.kind.as_deref(),
                r.derivation.as_deref(),
                r.abstract_,
            );
            if let Some(v) = Self::obj_str(self.obj(&format!("StructureDefinition:{flavor}")), prop)
            {
                return Some(v.to_string());
            }
        }
        // 3. the type default
        if let Some(v) = Self::obj_str(self.obj(&r.rt), prop) {
            return Some(v.to_string());
        }
        // 4. Any
        if let Some(v) = Self::obj_str(self.obj("Any"), prop) {
            return Some(v.to_string());
        }
        None
    }
}
