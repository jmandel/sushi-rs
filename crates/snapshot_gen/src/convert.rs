//! Pipeline STAGE 2: pure R4 StructureDefinition JSON -> R5-internal-model JSON.
//!
//! This is a port of the conversion the IG Publisher performs *before* snapshot
//! generation: `VersionConvertorFactory_40_50.convertResource` for a
//! `StructureDefinition` (default `BaseAdvisor_40_50`), then serialize with the
//! R5 `JsonParser`. It is **context-free**: no package context, no base SD, no
//! `generateSnapshot`. R5 inputs pass through unchanged (stage-2 no-op).
//!
//! Ground-truth spec: `snapshot/specs/r4-to-r5-conversion.md`. Every rule traces
//! there (which in turn cites fhir-core `file:line` at commit `5c4d5a0ff`). The
//! output is gated (order-sensitive) against the 39
//! `snapshot/converted-goldens/**/*.converted.json` oracle goldens.
//!
//! Self-contained: depends only on `serde_json` + `anyhow`, never on `legacy.rs`.

use anyhow::{bail, Result};
use serde_json::{Map, Value};

// --- Extension URL constants (spec §7) ---------------------------------------

/// `constraint.xpath` -> extension (field->ext, appended LAST). The `/4.0/` path.
/// Cite: ElementDefinition40_50:551-553; ExtensionDefinitions.java:158.
const EXT_XPATH_CONSTRAINT: &str =
    "http://hl7.org/fhir/4.0/StructureDefinition/extension-ElementDefinition.constraint.xpath";
/// tooling additional-binding (ext->field binding.additional[]).
/// Cite: VersionConvertorConstants EXT_BINDING_ADDITIONAL; ExtensionDefinitions.java:130.
const EXT_BINDING_ADDITIONAL_TOOLS: &str =
    "http://hl7.org/fhir/tools/StructureDefinition/additional-binding";
/// 5.0 backport additional-binding (ext->field binding.additional[]).
/// Cite: VersionConvertorConstants EXT_ADDITIONAL_BINDING.
const EXT_ADDITIONAL_BINDING_50: &str =
    "http://hl7.org/fhir/5.0/StructureDefinition/extension-ElementDefinition.binding.additional";
/// ED extension[mustHaveValue] -> ElementDefinition.mustHaveValue (boolean).
/// Cite: VersionConvertorConstants EXT_MUST_VALUE; ElementDefinition40_50:36-38,87-89.
const EXT_MUST_VALUE: &str =
    "http://hl7.org/fhir/5.0/StructureDefinition/extension-ElementDefinition.mustHaveValue";
/// ED extension[valueAlternatives][] -> ElementDefinition.valueAlternatives[] (canonical).
/// Cite: VersionConvertorConstants EXT_VALUE_ALT; ElementDefinition40_50:36-38,90-92.
const EXT_VALUE_ALT: &str =
    "http://hl7.org/fhir/5.0/StructureDefinition/extension-ElementDefinition.valueAlternatives";

// -----------------------------------------------------------------------------
// Entry point
// -----------------------------------------------------------------------------

/// Convert an R4 `StructureDefinition` JSON to the R5 internal model JSON, exactly
/// as `VersionConvertor_40_50` + the R5 `JsonParser` would emit it (key order and
/// field/extension transforms included). R5 inputs pass through unchanged.
pub(crate) fn r4_sd_to_r5(sd: &Value) -> Result<Value> {
    let obj = sd
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("StructureDefinition must be a JSON object"))?;
    if obj.get("resourceType").and_then(Value::as_str) != Some("StructureDefinition") {
        bail!(
            "r4_sd_to_r5 expects a StructureDefinition, got resourceType={:?}",
            obj.get("resourceType")
        );
    }
    // R5 inputs pass through unchanged (stage-2 no-op). fhirVersion 4.x is
    // converted; anything else (5.x, absent) is already R5-internal form.
    let fhir_version = obj.get("fhirVersion").and_then(Value::as_str);
    if !matches!(fhir_version, Some(v) if v.starts_with('4')) {
        return Ok(sd.clone());
    }
    convert_structure_definition(obj)
}

// -----------------------------------------------------------------------------
// Low-level helpers (self-contained; no legacy.rs dependency)
// -----------------------------------------------------------------------------

/// `StringUtils.isBlank` semantics: empty, or whitespace-only.
fn is_blank(s: &str) -> bool {
    s.chars().all(char::is_whitespace)
}

/// True if a JSON value is a string primitive whose value is blank (the
/// `PrimitiveType.hasValue()` == false case -> drop the field). Spec §3.1.
fn is_blank_primitive(v: &Value) -> bool {
    matches!(v, Value::String(s) if is_blank(s))
}

/// Copy a plain (structurally-verbatim) field if present.
fn copy(src: &Map<String, Value>, dst: &mut Map<String, Value>, key: &str) {
    if let Some(v) = src.get(key) {
        dst.insert(key.to_string(), v.clone());
    }
}

/// Copy a primitive field, applying the blank-string drop (`hasValue()` guard)
/// and carrying its `_field` sidecar (id/extension) if present. Value emitted
/// first, then the sidecar (spec §3.3 order).
fn prim_field(src: &Map<String, Value>, dst: &mut Map<String, Value>, key: &str) {
    if let Some(v) = src.get(key) {
        if !is_blank_primitive(v) {
            dst.insert(key.to_string(), v.clone());
        }
    }
    let under = format!("_{key}");
    if let Some(v) = src.get(&under) {
        dst.insert(under, v.clone());
    }
}

/// Copy a primitive *array* field with its parallel `_field` sidecar array.
/// (The array's null placeholders are serializer cosmetics preserved verbatim;
/// spec §3.3.)
fn prim_array_field(src: &Map<String, Value>, dst: &mut Map<String, Value>, key: &str) {
    copy(src, dst, key);
    let under = format!("_{key}");
    if let Some(v) = src.get(&under) {
        dst.insert(under, v.clone());
    }
}

fn ext_url_in(ext: &Value, urls: &[&str]) -> bool {
    ext.get("url")
        .and_then(Value::as_str)
        .is_some_and(|u| urls.contains(&u))
}

/// Copy the `id` + converted `extension[]` envelope (Element40_50.copyElement),
/// skipping any URL in `ignore`. `id` first, then `extension[]` in source order.
fn base_copy(src: &Map<String, Value>, ignore: &[&str]) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    copy(src, &mut out, "id");
    if let Some(exts) = src.get("extension").and_then(Value::as_array) {
        let mut converted = Vec::new();
        for e in exts {
            if !ext_url_in(e, ignore) {
                converted.push(convert_extension(e)?);
            }
        }
        if !converted.is_empty() {
            out.insert("extension".to_string(), Value::Array(converted));
        }
    }
    Ok(out)
}

fn as_obj(v: &Value) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    v.as_object().unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}

/// Map an object's array field with `f`, inserting the result under `key`.
fn map_array(
    src: &Map<String, Value>,
    dst: &mut Map<String, Value>,
    key: &str,
    f: impl Fn(&Value) -> Result<Value>,
) -> Result<()> {
    if let Some(arr) = src.get(key).and_then(Value::as_array) {
        let mut mapped = Vec::with_capacity(arr.len());
        for item in arr {
            mapped.push(f(item)?);
        }
        dst.insert(key.to_string(), Value::Array(mapped));
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// Extension conversion (Extension40_50: [id, extension] -> url -> value[x])
// -----------------------------------------------------------------------------

fn convert_extension(e: &Value) -> Result<Value> {
    let src = as_obj(e);
    let mut out = base_copy(src, &[])?;
    copy(src, &mut out, "url");
    convert_value_choice(src, &mut out, "value")?;
    Ok(Value::Object(out))
}

// -----------------------------------------------------------------------------
// value[x] choice + datatype dispatch
// -----------------------------------------------------------------------------

/// Primitive type suffixes (choice `value<Suffix>`): a blank string value is
/// dropped (spec §3.1). Their JSON value is copied verbatim.
fn is_primitive_type(suffix: &str) -> bool {
    matches!(
        suffix,
        "Boolean"
            | "Integer"
            | "Integer64"
            | "Decimal"
            | "String"
            | "Uri"
            | "Url"
            | "Canonical"
            | "Code"
            | "Id"
            | "Oid"
            | "Uuid"
            | "Markdown"
            | "Base64Binary"
            | "Instant"
            | "Date"
            | "DateTime"
            | "Time"
            | "PositiveInt"
            | "UnsignedInt"
    )
}

/// Convert a `<base><Type>` choice family (e.g. fixed/pattern/defaultValue/value/
/// minValue/maxValue). Applies the primitive blank-drop and carries `_<base><Type>`
/// sidecars. Returns Err naming an unimplemented datatype (fail-loud, no silent
/// passthrough).
fn convert_value_choice(
    src: &Map<String, Value>,
    dst: &mut Map<String, Value>,
    base: &str,
) -> Result<()> {
    for (k, v) in src.iter() {
        if let Some(suffix) = choice_suffix(k, base) {
            let converted = convert_datatype(suffix, v)?;
            // Primitive blank -> drop the value key (but keep sidecar below).
            let drop = is_primitive_type(suffix) && is_blank_primitive(&converted);
            if !drop {
                dst.insert(k.clone(), converted);
            }
            let under = format!("_{k}");
            if let Some(sc) = src.get(&under) {
                dst.insert(under, sc.clone());
            }
        }
    }
    Ok(())
}

/// If `key` is `<base><Suffix>` with an uppercase-led suffix, return the suffix.
fn choice_suffix<'a>(key: &'a str, base: &str) -> Option<&'a str> {
    let rest = key.strip_prefix(base)?;
    let first = rest.chars().next()?;
    if first.is_ascii_uppercase() {
        Some(rest)
    } else {
        None
    }
}

/// Dispatch a datatype value by its choice suffix. Structurally same-name copies
/// per `CONV/conv40_50/datatypes40_50/*`, reconstructing canonical R5 key order.
/// Unimplemented datatype -> Err(name) (fail-loud).
fn convert_datatype(suffix: &str, v: &Value) -> Result<Value> {
    if is_primitive_type(suffix) {
        // Primitives copy their JSON value verbatim (decimals/integers preserved).
        return Ok(v.clone());
    }
    match suffix {
        "Coding" => conv_coding(v),
        "CodeableConcept" => conv_codeable_concept(v),
        "Identifier" => conv_identifier(v),
        "Reference" => conv_reference(v),
        "Period" => conv_period(v),
        // Duration/Age/Count/Distance/MoneyQuantity/SimpleQuantity share the
        // Quantity copy (Duration40_50 delegates to Quantity40_50.copyQuantity).
        "Quantity" | "Duration" | "Age" | "Count" | "Distance" | "MoneyQuantity"
        | "SimpleQuantity" => conv_quantity(v),
        other => bail!("unimplemented value[x] datatype converter: {other}"),
    }
}

// -----------------------------------------------------------------------------
// Datatype converters (field order from CONV/conv40_50/datatypes40_50/*)
// -----------------------------------------------------------------------------

fn conv_coding(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    for k in ["system", "version", "code", "display", "userSelected"] {
        prim_field(src, &mut o, k);
    }
    Ok(Value::Object(o))
}

fn conv_codeable_concept(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    map_array(src, &mut o, "coding", conv_coding)?;
    prim_field(src, &mut o, "text");
    Ok(Value::Object(o))
}

fn conv_period(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    prim_field(src, &mut o, "start");
    prim_field(src, &mut o, "end");
    Ok(Value::Object(o))
}

fn conv_quantity(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    for k in ["value", "comparator", "unit", "system", "code"] {
        prim_field(src, &mut o, k);
    }
    Ok(Value::Object(o))
}

fn conv_identifier(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    prim_field(src, &mut o, "use");
    if let Some(t) = src.get("type") {
        o.insert("type".to_string(), conv_codeable_concept(t)?);
    }
    prim_field(src, &mut o, "system");
    prim_field(src, &mut o, "value");
    if let Some(p) = src.get("period") {
        o.insert("period".to_string(), conv_period(p)?);
    }
    if let Some(a) = src.get("assigner") {
        o.insert("assigner".to_string(), conv_reference(a)?);
    }
    Ok(Value::Object(o))
}

fn conv_reference(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    prim_field(src, &mut o, "reference");
    prim_field(src, &mut o, "type");
    if let Some(i) = src.get("identifier") {
        o.insert("identifier".to_string(), conv_identifier(i)?);
    }
    prim_field(src, &mut o, "display");
    Ok(Value::Object(o))
}

fn conv_contact_point(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    for k in ["system", "value", "use", "rank"] {
        prim_field(src, &mut o, k);
    }
    if let Some(p) = src.get("period") {
        o.insert("period".to_string(), conv_period(p)?);
    }
    Ok(Value::Object(o))
}

fn conv_contact_detail(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    prim_field(src, &mut o, "name");
    map_array(src, &mut o, "telecom", conv_contact_point)?;
    Ok(Value::Object(o))
}

fn conv_usage_context(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    if let Some(c) = src.get("code") {
        o.insert("code".to_string(), conv_coding(c)?);
    }
    convert_value_choice(src, &mut o, "value")?;
    Ok(Value::Object(o))
}

// -----------------------------------------------------------------------------
// StructureDefinition top-level (spec §4, §10.3)
// -----------------------------------------------------------------------------

fn convert_structure_definition(src: &Map<String, Value>) -> Result<Value> {
    let mut o = Map::new();
    o.insert(
        "resourceType".to_string(),
        Value::String("StructureDefinition".to_string()),
    );
    // copyDomainResource: id, meta, implicitRules, language, text, contained[],
    // extension[], modifierExtension[] (spec §2). meta/text are copied verbatim
    // here (SD fixtures rarely carry them; contained[] would need its own
    // resource converter, out of scope — fail-loud if a fixture ever hits it).
    copy(src, &mut o, "id");
    copy(src, &mut o, "meta");
    copy(src, &mut o, "implicitRules");
    copy(src, &mut o, "language");
    copy(src, &mut o, "text");
    if src.contains_key("contained") {
        bail!("contained[] resource conversion is out of scope for stage 2");
    }
    if let Some(exts) = src.get("extension").and_then(Value::as_array) {
        let mut converted = Vec::with_capacity(exts.len());
        for e in exts {
            converted.push(convert_extension(e)?);
        }
        o.insert("extension".to_string(), Value::Array(converted));
    }
    if let Some(exts) = src.get("modifierExtension").and_then(Value::as_array) {
        let mut converted = Vec::with_capacity(exts.len());
        for e in exts {
            converted.push(convert_extension(e)?);
        }
        o.insert("modifierExtension".to_string(), Value::Array(converted));
    }
    // Typed SD fields, source order (spec §4 table). Metadata datatypes go
    // through their converters (reordering keys) — NOT verbatim copies.
    prim_field(src, &mut o, "url");
    map_array(src, &mut o, "identifier", conv_identifier)?;
    prim_field(src, &mut o, "version");
    prim_field(src, &mut o, "name");
    prim_field(src, &mut o, "title");
    prim_field(src, &mut o, "status");
    copy(src, &mut o, "experimental");
    prim_field(src, &mut o, "date");
    prim_field(src, &mut o, "publisher");
    map_array(src, &mut o, "contact", conv_contact_detail)?;
    prim_field(src, &mut o, "description");
    map_array(src, &mut o, "useContext", conv_usage_context)?;
    map_array(src, &mut o, "jurisdiction", conv_codeable_concept)?;
    prim_field(src, &mut o, "purpose");
    prim_field(src, &mut o, "copyright");
    map_array(src, &mut o, "keyword", conv_coding)?;
    prim_field(src, &mut o, "fhirVersion");
    map_array(src, &mut o, "mapping", conv_sd_mapping)?;
    prim_field(src, &mut o, "kind");
    copy(src, &mut o, "abstract");
    map_array(src, &mut o, "context", conv_sd_context)?;
    prim_array_field(src, &mut o, "contextInvariant");
    prim_field(src, &mut o, "type");
    prim_field(src, &mut o, "baseDefinition");
    prim_field(src, &mut o, "derivation");
    if let Some(snap) = src.get("snapshot") {
        o.insert("snapshot".to_string(), convert_element_list(snap)?);
    }
    if let Some(diff) = src.get("differential") {
        o.insert("differential".to_string(), convert_element_list(diff)?);
    }
    Ok(Value::Object(o))
}

fn conv_sd_mapping(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    prim_field(src, &mut o, "identity");
    prim_field(src, &mut o, "uri");
    prim_field(src, &mut o, "name");
    prim_field(src, &mut o, "comment");
    Ok(Value::Object(o))
}

fn conv_sd_context(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    prim_field(src, &mut o, "type");
    prim_field(src, &mut o, "expression");
    Ok(Value::Object(o))
}

/// snapshot / differential backbone: [id, extension] then element[] (each an ED).
fn convert_element_list(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    let mut elems = Vec::new();
    if let Some(arr) = src.get("element").and_then(Value::as_array) {
        for e in arr {
            elems.push(convert_element_definition(e)?);
        }
    }
    o.insert("element".to_string(), Value::Array(elems));
    Ok(Value::Object(o))
}

// -----------------------------------------------------------------------------
// ElementDefinition (spec §5, §10.4)
// -----------------------------------------------------------------------------

/// Convert an extension list, skipping ignored URLs. Used for ED extension +
/// modifierExtension (copyBackboneElement with the same ignore list).
fn convert_ext_list(exts: &[Value], ignore: &[&str]) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for e in exts {
        if !ext_url_in(e, ignore) {
            out.push(convert_extension(e)?);
        }
    }
    Ok(out)
}

fn convert_element_definition(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = Map::new();
    copy(src, &mut o, "id");
    // copyBackboneElement stripping EXT_MUST_VALUE + EXT_VALUE_ALT from BOTH
    // extension and modifierExtension (spec §5 line 36-38, open question 6).
    let ignore = [EXT_MUST_VALUE, EXT_VALUE_ALT];
    if let Some(exts) = src.get("extension").and_then(Value::as_array) {
        let kept = convert_ext_list(exts, &ignore)?;
        if !kept.is_empty() {
            o.insert("extension".to_string(), Value::Array(kept));
        }
    }
    if let Some(exts) = src.get("modifierExtension").and_then(Value::as_array) {
        let kept = convert_ext_list(exts, &ignore)?;
        if !kept.is_empty() {
            o.insert("modifierExtension".to_string(), Value::Array(kept));
        }
    }
    prim_field(src, &mut o, "path");
    prim_array_field(src, &mut o, "representation");
    prim_field(src, &mut o, "sliceName");
    copy(src, &mut o, "sliceIsConstraining");
    prim_field(src, &mut o, "label");
    map_array(src, &mut o, "code", conv_coding)?;
    if let Some(s) = src.get("slicing") {
        o.insert("slicing".to_string(), conv_slicing(s)?);
    }
    prim_field(src, &mut o, "short");
    prim_field(src, &mut o, "definition");
    prim_field(src, &mut o, "comment");
    prim_field(src, &mut o, "requirements");
    prim_array_field(src, &mut o, "alias");
    copy(src, &mut o, "min");
    prim_field(src, &mut o, "max");
    if let Some(b) = src.get("base") {
        o.insert("base".to_string(), conv_ed_base(b)?);
    }
    prim_field(src, &mut o, "contentReference");
    map_array(src, &mut o, "type", conv_type_ref)?;
    convert_value_choice(src, &mut o, "defaultValue")?;
    prim_field(src, &mut o, "meaningWhenMissing");
    prim_field(src, &mut o, "orderMeaning");
    convert_value_choice(src, &mut o, "fixed")?;
    convert_value_choice(src, &mut o, "pattern")?;
    map_array(src, &mut o, "example", conv_example)?;
    convert_value_choice(src, &mut o, "minValue")?;
    convert_value_choice(src, &mut o, "maxValue")?;
    copy(src, &mut o, "maxLength");
    prim_array_field(src, &mut o, "condition");
    map_array(src, &mut o, "constraint", conv_constraint)?;
    // mustHaveValue / valueAlternatives promoted from the ignored extensions.
    if let Some(exts) = src.get("extension").and_then(Value::as_array) {
        if let Some(mhv) = exts
            .iter()
            .find(|e| e.get("url").and_then(Value::as_str) == Some(EXT_MUST_VALUE))
            .and_then(|e| e.get("valueBoolean"))
        {
            o.insert("mustHaveValue".to_string(), mhv.clone());
        }
        let alts: Vec<Value> = exts
            .iter()
            .filter(|e| e.get("url").and_then(Value::as_str) == Some(EXT_VALUE_ALT))
            .filter_map(|e| e.get("valueCanonical").cloned())
            .collect();
        if !alts.is_empty() {
            o.insert("valueAlternatives".to_string(), Value::Array(alts));
        }
    }
    copy(src, &mut o, "mustSupport");
    copy(src, &mut o, "isModifier");
    prim_field(src, &mut o, "isModifierReason");
    copy(src, &mut o, "isSummary");
    if let Some(b) = src.get("binding") {
        o.insert("binding".to_string(), conv_binding(b)?);
    }
    map_array(src, &mut o, "mapping", conv_ed_mapping)?;
    Ok(Value::Object(o))
}

fn conv_slicing(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    map_array(src, &mut o, "discriminator", conv_discriminator)?;
    prim_field(src, &mut o, "description");
    copy(src, &mut o, "ordered");
    prim_field(src, &mut o, "rules");
    Ok(Value::Object(o))
}

fn conv_discriminator(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    prim_field(src, &mut o, "type");
    prim_field(src, &mut o, "path");
    Ok(Value::Object(o))
}

fn conv_ed_base(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    prim_field(src, &mut o, "path");
    copy(src, &mut o, "min");
    prim_field(src, &mut o, "max");
    Ok(Value::Object(o))
}

fn conv_type_ref(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    prim_field(src, &mut o, "code");
    prim_array_field(src, &mut o, "profile");
    prim_array_field(src, &mut o, "targetProfile");
    prim_array_field(src, &mut o, "aggregation");
    prim_field(src, &mut o, "versioning");
    Ok(Value::Object(o))
}

fn conv_example(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    prim_field(src, &mut o, "label");
    convert_value_choice(src, &mut o, "value")?;
    Ok(Value::Object(o))
}

/// constraint: [id, extension(+xpath appended LAST)] -> key -> requirements ->
/// severity -> human -> expression -> source (spec §6, §7.1).
fn conv_constraint(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    if let Some(xpath) = src.get("xpath").and_then(Value::as_str) {
        let ext = o
            .entry("extension".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(arr) = ext.as_array_mut() {
            let mut xext = Map::new();
            xext.insert(
                "url".to_string(),
                Value::String(EXT_XPATH_CONSTRAINT.to_string()),
            );
            xext.insert("valueString".to_string(), Value::String(xpath.to_string()));
            arr.push(Value::Object(xext));
        }
    }
    prim_field(src, &mut o, "key");
    prim_field(src, &mut o, "requirements"); // string->markdown (§8): value identical
    prim_field(src, &mut o, "severity");
    prim_field(src, &mut o, "human");
    prim_field(src, &mut o, "expression");
    prim_field(src, &mut o, "source");
    Ok(Value::Object(o))
}

/// binding: [id, extension(minus additional-binding families)] -> strength ->
/// description -> valueSet -> additional[] (built from the ignored ext families).
fn conv_binding(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let ignore = [EXT_BINDING_ADDITIONAL_TOOLS, EXT_ADDITIONAL_BINDING_50];
    let mut o = base_copy(src, &ignore)?;
    prim_field(src, &mut o, "strength");
    prim_field(src, &mut o, "description"); // string->markdown (§8)
    prim_field(src, &mut o, "valueSet");
    if let Some(exts) = src.get("extension").and_then(Value::as_array) {
        let mut additional = Vec::new();
        for e in exts {
            if ext_url_in(e, &ignore) {
                additional.push(conv_additional(e)?);
            }
        }
        if !additional.is_empty() {
            o.insert("additional".to_string(), Value::Array(additional));
        }
    }
    Ok(Value::Object(o))
}

/// binding.additional: [id, extension(minus consumed children)] -> purpose ->
/// valueSet -> documentation -> shortDoco -> usage[] -> any (spec §6, §7.2).
fn conv_additional(ext: &Value) -> Result<Value> {
    let src = as_obj(ext);
    const CHILD_IGNORE: [&str; 6] = [
        "valueSet",
        "purpose",
        "documentation",
        "shortDoco",
        "usage",
        "any",
    ];
    let mut o = Map::new();
    copy(src, &mut o, "id");
    let empty: Vec<Value> = Vec::new();
    let kids: &[Value] = src
        .get("extension")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let extra = convert_ext_list(kids, &CHILD_IGNORE)?;
    if !extra.is_empty() {
        o.insert("extension".to_string(), Value::Array(extra));
    }
    let child = |url: &str| {
        kids.iter()
            .find(|k| k.get("url").and_then(Value::as_str) == Some(url))
    };
    if let Some(p) = child("purpose").and_then(|k| k.get("valueCode")) {
        o.insert("purpose".to_string(), p.clone());
    }
    if let Some(vs) = child("valueSet").and_then(|k| k.get("valueCanonical")) {
        o.insert("valueSet".to_string(), vs.clone());
    }
    if let Some(d) = child("documentation").and_then(|k| k.get("valueMarkdown")) {
        o.insert("documentation".to_string(), d.clone());
    }
    if let Some(sd) = child("shortDoco").and_then(|k| k.get("valueString")) {
        o.insert("shortDoco".to_string(), sd.clone());
    }
    let usage: Vec<Value> = kids
        .iter()
        .filter(|k| k.get("url").and_then(Value::as_str) == Some("usage"))
        .filter_map(|k| k.get("valueUsageContext").cloned())
        .collect();
    if !usage.is_empty() {
        o.insert("usage".to_string(), Value::Array(usage));
    }
    if let Some(a) = child("any").and_then(|k| k.get("valueBoolean")) {
        o.insert("any".to_string(), a.clone());
    }
    Ok(Value::Object(o))
}

fn conv_ed_mapping(v: &Value) -> Result<Value> {
    let src = as_obj(v);
    let mut o = base_copy(src, &[])?;
    prim_field(src, &mut o, "identity");
    prim_field(src, &mut o, "language");
    prim_field(src, &mut o, "map");
    prim_field(src, &mut o, "comment"); // string->markdown (§8)
    Ok(Value::Object(o))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r5_input_passes_through() {
        let sd = json!({
            "resourceType": "StructureDefinition",
            "fhirVersion": "5.0.0",
            "url": "http://example.org/x",
            "differential": {"element": [{"path": "Patient"}]}
        });
        assert_eq!(r4_sd_to_r5(&sd).unwrap(), sd);
    }

    #[test]
    fn blank_string_primitive_dropped() {
        let sd = json!({
            "resourceType": "StructureDefinition",
            "fhirVersion": "4.0.1",
            "url": "http://x",
            "differential": {"element": [{"path": "Observation.value[x]", "comment": " "}]}
        });
        let out = r4_sd_to_r5(&sd).unwrap();
        let elem = &out["differential"]["element"][0];
        assert!(elem.get("comment").is_none(), "blank comment must drop");
    }

    #[test]
    fn constraint_xpath_appended_last() {
        let sd = json!({
            "resourceType": "StructureDefinition",
            "fhirVersion": "4.0.1",
            "url": "http://x",
            "differential": {"element": [{
                "path": "Observation",
                "constraint": [{
                    "extension": [{"url": "http://pre", "valueBoolean": true}],
                    "key": "k1", "severity": "error", "human": "h",
                    "xpath": "f:x"
                }]
            }]}
        });
        let out = r4_sd_to_r5(&sd).unwrap();
        let c = &out["differential"]["element"][0]["constraint"][0];
        let exts = c["extension"].as_array().unwrap();
        assert_eq!(exts.len(), 2);
        assert_eq!(exts[0]["url"], json!("http://pre"));
        assert_eq!(exts[1]["url"], json!(EXT_XPATH_CONSTRAINT));
        assert_eq!(exts[1]["valueString"], json!("f:x"));
        assert!(c.get("xpath").is_none());
    }

    #[test]
    fn unimplemented_datatype_is_error() {
        let sd = json!({
            "resourceType": "StructureDefinition",
            "fhirVersion": "4.0.1",
            "url": "http://x",
            "differential": {"element": [{
                "path": "X.y",
                "fixedTiming": {"event": ["2020"]}
            }]}
        });
        let err = r4_sd_to_r5(&sd).unwrap_err();
        assert!(err.to_string().contains("Timing"), "{err}");
    }
}
