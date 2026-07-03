//! Thin typed views over a StructureDefinition's JSON (serde_json::Value). We do
//! NOT depend on fhir_model (standalone crate policy); the renderer only needs a
//! handful of fields off each ElementDefinition, read directly from JSON with
//! the FHIR json field names.
//!
//! Input is a snapshot-complete SD exactly as the publisher held it (the F0
//! build's `output/StructureDefinition-*.json`), so `snapshot.element[]` carries
//! base cardinalities, types, constraints, bindings, definitions, etc.

use serde_json::Value;

/// A borrowed ElementDefinition view.
#[derive(Clone, Copy)]
pub struct Ed<'a> {
    pub v: &'a Value,
}

impl<'a> Ed<'a> {
    pub fn new(v: &'a Value) -> Ed<'a> {
        Ed { v }
    }
    pub fn str(&self, key: &str) -> Option<&'a str> {
        self.v.get(key).and_then(|x| x.as_str())
    }
    pub fn path(&self) -> &'a str {
        self.str("path").unwrap_or("")
    }
    pub fn id(&self) -> &'a str {
        self.str("id").unwrap_or("")
    }
    pub fn slice_name(&self) -> Option<&'a str> {
        self.str("sliceName")
    }
    pub fn has_slice_name(&self) -> bool {
        self.slice_name().is_some()
    }
    pub fn slicing(&self) -> Option<&'a Value> {
        self.v.get("slicing")
    }
    pub fn has_slicing(&self) -> bool {
        self.slicing().is_some()
    }
    pub fn min(&self) -> Option<i64> {
        self.v.get("min").and_then(|x| x.as_i64())
    }
    pub fn max(&self) -> Option<&'a str> {
        self.str("max")
    }
    pub fn must_support(&self) -> bool {
        self.v.get("mustSupport").and_then(|x| x.as_bool()).unwrap_or(false)
    }
    pub fn has_must_support(&self) -> bool {
        self.v.get("mustSupport").is_some()
    }
    pub fn is_modifier(&self) -> bool {
        self.v.get("isModifier").and_then(|x| x.as_bool()).unwrap_or(false)
    }
    pub fn definition(&self) -> Option<&'a str> {
        self.str("definition")
    }
    pub fn has_definition(&self) -> bool {
        self.definition().map(|s| !s.is_empty()).unwrap_or(false)
    }
    pub fn comment(&self) -> Option<&'a str> {
        self.str("comment")
    }
    pub fn max_length(&self) -> Option<i64> {
        self.v.get("maxLength").and_then(|x| x.as_i64())
    }
    pub fn content_reference(&self) -> Option<&'a str> {
        self.str("contentReference")
    }
    pub fn types(&self) -> Vec<TypeRef<'a>> {
        self.v
            .get("type")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().map(|t| TypeRef { v: t }).collect())
            .unwrap_or_default()
    }
    pub fn constraints(&self) -> Vec<Constraint<'a>> {
        self.v
            .get("constraint")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().map(|c| Constraint { v: c }).collect())
            .unwrap_or_default()
    }
    pub fn binding(&self) -> Option<&'a Value> {
        self.v.get("binding")
    }
    pub fn base(&self) -> Option<&'a Value> {
        self.v.get("base")
    }
    pub fn base_max(&self) -> Option<&'a str> {
        self.base().and_then(|b| b.get("max")).and_then(|x| x.as_str())
    }
    pub fn base_min(&self) -> Option<i64> {
        self.base().and_then(|b| b.get("min")).and_then(|x| x.as_i64())
    }
    pub fn short(&self) -> Option<&'a str> {
        self.str("short")
    }
    pub fn is_summary(&self) -> bool {
        self.v.get("isSummary").and_then(|x| x.as_bool()).unwrap_or(false)
    }
    pub fn must_have_value(&self) -> bool {
        self.v.get("mustHaveValue").and_then(|x| x.as_bool()).unwrap_or(false)
    }
    pub fn order_meaning(&self) -> Option<&'a str> {
        self.str("orderMeaning")
    }
    pub fn conditions(&self) -> Vec<&'a str> {
        self.v
            .get("condition")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|c| c.as_str()).collect())
            .unwrap_or_default()
    }
    pub fn extensions(&self) -> Vec<&'a Value> {
        self.v
            .get("extension")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().collect())
            .unwrap_or_default()
    }
    pub fn has_extension(&self, url: &str) -> bool {
        self.extensions()
            .iter()
            .any(|e| e.get("url").and_then(|x| x.as_str()) == Some(url))
    }
    pub fn base_path(&self) -> Option<&'a str> {
        self.base().and_then(|b| b.get("path")).and_then(|x| x.as_str())
    }
    /// A fixed[x] value: returns (json-type-suffix, value) if any key starts with
    /// "fixed".
    pub fn fixed(&self) -> Option<(&'a str, &'a Value)> {
        self.find_prefixed("fixed")
    }
    pub fn pattern(&self) -> Option<(&'a str, &'a Value)> {
        self.find_prefixed("pattern")
    }
    pub fn example(&self) -> Vec<&'a Value> {
        self.v
            .get("example")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().collect())
            .unwrap_or_default()
    }
    fn find_prefixed(&self, prefix: &str) -> Option<(&'a str, &'a Value)> {
        let obj = self.v.as_object()?;
        for (k, val) in obj {
            if let Some(rest) = k.strip_prefix(prefix) {
                // must be fixed<Type> with a capitalized type suffix
                if rest.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
                    return Some((rest, val));
                }
            }
        }
        None
    }
}

/// An ElementDefinition.type entry.
#[derive(Clone, Copy)]
pub struct TypeRef<'a> {
    pub v: &'a Value,
}
impl<'a> TypeRef<'a> {
    pub fn code(&self) -> &'a str {
        self.v.get("code").and_then(|x| x.as_str()).unwrap_or("")
    }
    /// Java `TypeRefComponent.getWorkingCode()`: the `structuredefinition-
    /// fhir-type` extension on `code` wins over the raw code (this is how
    /// `http://hl7.org/fhirpath/System.String` renders as `string`).
    pub fn working_code(&self) -> &'a str {
        if let Some(exts) = self
            .v
            .get("extension")
            .or_else(|| self.v.get("_code").and_then(|c| c.get("extension")))
            .and_then(|x| x.as_array())
        {
            for e in exts {
                if e.get("url").and_then(|x| x.as_str())
                    == Some("http://hl7.org/fhir/StructureDefinition/structuredefinition-fhir-type")
                {
                    if let Some(v) = e
                        .get("valueUrl")
                        .or_else(|| e.get("valueUri"))
                        .and_then(|x| x.as_str())
                    {
                        return v;
                    }
                }
            }
        }
        self.code()
    }
    /// Java `TypeRefComponent.hasTarget()` (ElementDefinition.java:2628):
    /// raw code in {Reference, canonical, CodeableReference}.
    pub fn has_target(&self) -> bool {
        matches!(self.code(), "Reference" | "canonical" | "CodeableReference")
    }
    pub fn profiles(&self) -> Vec<&'a str> {
        self.v
            .get("profile")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|p| p.as_str()).collect())
            .unwrap_or_default()
    }
    pub fn target_profiles(&self) -> Vec<&'a str> {
        self.v
            .get("targetProfile")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|p| p.as_str()).collect())
            .unwrap_or_default()
    }
    /// FHIRPath System primitives (e.g. http://hl7.org/fhirpath/System.String)
    /// are "primitive" in the isPrimitive sense used by genGridElement (a single
    /// primitive type => bold name).
    pub fn is_primitive_code(&self) -> bool {
        // A datatype whose code is a lowercase-initial FHIR primitive. The grid
        // check is `element.getType().get(0).isPrimitive()` — TypeRefComponent
        // .isPrimitive() checks the code is a primitive type. Approximate with
        // the FHIR primitive set + System.* codes.
        let c = self.code();
        c.starts_with("http://hl7.org/fhirpath/System.")
            || is_fhir_primitive(c)
    }
}

/// An ElementDefinition.constraint entry.
#[derive(Clone, Copy)]
pub struct Constraint<'a> {
    pub v: &'a Value,
}
impl<'a> Constraint<'a> {
    pub fn key(&self) -> &'a str {
        self.v.get("key").and_then(|x| x.as_str()).unwrap_or("")
    }
    pub fn human(&self) -> &'a str {
        self.v.get("human").and_then(|x| x.as_str()).unwrap_or("")
    }
}

/// The FHIR R4/R5 primitive datatype set (lowercase-initial types).
pub fn is_fhir_primitive(code: &str) -> bool {
    matches!(
        code,
        "boolean"
            | "integer"
            | "integer64"
            | "string"
            | "decimal"
            | "uri"
            | "url"
            | "canonical"
            | "base64Binary"
            | "instant"
            | "date"
            | "dateTime"
            | "time"
            | "code"
            | "oid"
            | "id"
            | "markdown"
            | "unsignedInt"
            | "positiveInt"
            | "uuid"
    )
}

/// A whole StructureDefinition view.
pub struct Sd {
    pub root: Value,
}

impl Sd {
    pub fn from_json(s: &str) -> serde_json::Result<Sd> {
        Ok(Sd {
            root: serde_json::from_str(s)?,
        })
    }
    /// Wrap an owned SD `Value` (e.g. a clone of a `load_resource` Rc).
    pub fn from_value(root: Value) -> Sd {
        Sd { root }
    }
    pub fn id(&self) -> &str {
        self.root.get("id").and_then(|x| x.as_str()).unwrap_or("")
    }
    pub fn kind(&self) -> &str {
        self.root.get("kind").and_then(|x| x.as_str()).unwrap_or("")
    }
    pub fn is_logical(&self) -> bool {
        self.kind() == "logical"
    }
    pub fn derivation(&self) -> &str {
        self.root.get("derivation").and_then(|x| x.as_str()).unwrap_or("")
    }
    pub fn fhir_version(&self) -> &str {
        self.root.get("fhirVersion").and_then(|x| x.as_str()).unwrap_or("")
    }
    /// `profile.getTypeName()` — the SD's `type` (the base resource/type name).
    pub fn type_name(&self) -> &str {
        self.root.get("type").and_then(|x| x.as_str()).unwrap_or("")
    }
    /// The raw differential element array (empty if absent).
    pub fn differential_elements(&self) -> Vec<Ed<'_>> {
        self.root
            .get("differential")
            .and_then(|s| s.get("element"))
            .and_then(|e| e.as_array())
            .map(|a| a.iter().map(Ed::new).collect())
            .unwrap_or_default()
    }
    pub fn snapshot_elements(&self) -> Vec<Ed<'_>> {
        self.root
            .get("snapshot")
            .and_then(|s| s.get("element"))
            .and_then(|e| e.as_array())
            .map(|a| a.iter().map(Ed::new).collect())
            .unwrap_or_default()
    }
    pub fn has_snapshot(&self) -> bool {
        self.root
            .get("snapshot")
            .and_then(|s| s.get("element"))
            .and_then(|e| e.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    }
    pub fn mappings(&self) -> Vec<&Value> {
        self.root
            .get("mapping")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().collect())
            .unwrap_or_default()
    }
}
