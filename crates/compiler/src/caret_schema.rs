//! Embedded FHIR element-type schema for caret-value application on
//! StructureDefinition bodies and ElementDefinition elements. Covers the
//! metadata + ElementDefinition datatypes that real caret rules touch. This
//! avoids fishing the SD-of-SD; it mirrors the relevant element types only.

use crate::export::{split_caret_path, Seg};

fn is_primitive_type(ty: &str) -> bool {
    matches!(
        ty,
        "code" | "string" | "uri" | "url" | "canonical" | "markdown" | "boolean" | "integer"
            | "unsignedInt" | "positiveInt" | "decimal" | "dateTime" | "date" | "instant" | "id"
            | "base64Binary" | "time" | "oid" | "uuid"
    )
}

/// `(element_type, is_array)` for `type_name.base`, or None.
fn field_def(type_name: &str, base: &str) -> Option<(&'static str, bool)> {
    let shared = |base: &str| -> Option<(&'static str, bool)> {
        Some(match base {
            "id" => ("id", false),
            "meta" => ("Meta", false),
            "implicitRules" => ("uri", false),
            "language" => ("code", false),
            "extension" => ("Extension", true),
            "modifierExtension" => ("Extension", true),
            "url" => ("uri", false),
            "identifier" => ("Identifier", true),
            "version" => ("string", false),
            "name" => ("string", false),
            "title" => ("string", false),
            "status" => ("code", false),
            "experimental" => ("boolean", false),
            "date" => ("dateTime", false),
            "publisher" => ("string", false),
            "contact" => ("ContactDetail", true),
            "description" => ("markdown", false),
            "useContext" => ("UsageContext", true),
            "jurisdiction" => ("CodeableConcept", true),
            "purpose" => ("markdown", false),
            "copyright" => ("markdown", false),
            _ => return None,
        })
    };
    match type_name {
        "StructureDefinition" => shared(base).or(match base {
            "keyword" => Some(("Coding", true)),
            "fhirVersion" => Some(("code", false)),
            "mapping" => Some(("StructureDefinitionMapping", true)),
            "kind" => Some(("code", false)),
            "abstract" => Some(("boolean", false)),
            "context" => Some(("StructureDefinitionContext", true)),
            "contextInvariant" => Some(("string", true)),
            "type" => Some(("uri", false)),
            "baseDefinition" => Some(("canonical", false)),
            "derivation" => Some(("code", false)),
            "snapshot" => Some(("Ignore", false)),
            "differential" => Some(("Ignore", false)),
            _ => None,
        }),
        "StructureDefinitionMapping" => Some(match base {
            "identity" => ("id", false),
            "uri" => ("uri", false),
            "name" => ("string", false),
            "comment" => ("string", false),
            _ => return None,
        }),
        "StructureDefinitionContext" => Some(match base {
            "type" => ("code", false),
            "expression" => ("string", false),
            _ => return None,
        }),
        "ElementDefinition" => Some(match base {
            "id" => ("string", false),
            "extension" => ("Extension", true),
            "modifierExtension" => ("Extension", true),
            "path" => ("string", false),
            "representation" => ("code", true),
            "sliceName" => ("string", false),
            "sliceIsConstraining" => ("boolean", false),
            "label" => ("string", false),
            "code" => ("Coding", true),
            "slicing" => ("ElementDefinitionSlicing", false),
            "short" => ("string", false),
            "definition" => ("markdown", false),
            "comment" => ("markdown", false),
            "requirements" => ("markdown", false),
            "alias" => ("string", true),
            "min" => ("unsignedInt", false),
            "max" => ("string", false),
            "base" => ("ElementDefinitionBase", false),
            "contentReference" => ("uri", false),
            "type" => ("ElementDefinitionType", true),
            "meaningWhenMissing" => ("markdown", false),
            "orderMeaning" => ("string", false),
            "example" => ("ElementDefinitionExample", true),
            "maxLength" => ("integer", false),
            "condition" => ("id", true),
            "constraint" => ("ElementDefinitionConstraint", true),
            "mustSupport" => ("boolean", false),
            "isModifier" => ("boolean", false),
            "isModifierReason" => ("string", false),
            "isSummary" => ("boolean", false),
            "binding" => ("ElementDefinitionBinding", false),
            "mapping" => ("ElementDefinitionElementMapping", true),
            _ => return None,
        }),
        "ElementDefinitionSlicing" => Some(match base {
            "discriminator" => ("ElementDefinitionDiscriminator", true),
            "description" => ("string", false),
            "ordered" => ("boolean", false),
            "rules" => ("code", false),
            _ => return None,
        }),
        "ElementDefinitionDiscriminator" => Some(match base {
            "type" => ("code", false),
            "path" => ("string", false),
            _ => return None,
        }),
        "ElementDefinitionBase" => Some(match base {
            "path" => ("string", false),
            "min" => ("unsignedInt", false),
            "max" => ("string", false),
            _ => return None,
        }),
        "ElementDefinitionType" => Some(match base {
            "code" => ("uri", false),
            "profile" => ("canonical", true),
            "targetProfile" => ("canonical", true),
            "aggregation" => ("code", true),
            "versioning" => ("code", false),
            "extension" => ("Extension", true),
            _ => return None,
        }),
        "ElementDefinitionConstraint" => Some(match base {
            "key" => ("id", false),
            "requirements" => ("markdown", false),
            "severity" => ("code", false),
            "suppress" => ("boolean", false),
            "human" => ("string", false),
            "expression" => ("string", false),
            "xpath" => ("string", false),
            "source" => ("canonical", false),
            _ => return None,
        }),
        "ElementDefinitionBinding" => Some(match base {
            "strength" => ("code", false),
            "description" => ("markdown", false),
            "valueSet" => ("canonical", false),
            "additional" => ("ElementDefinitionAdditionalBinding", true),
            _ => return None,
        }),
        "ElementDefinitionExample" => Some(match base {
            "label" => ("string", false),
            _ => return None,
        }),
        "ElementDefinitionElementMapping" => Some(match base {
            "identity" => ("id", false),
            "language" => ("code", false),
            "map" => ("string", false),
            "comment" => ("string", false),
            _ => return None,
        }),
        "Meta" => Some(match base {
            "versionId" => ("id", false),
            "lastUpdated" => ("instant", false),
            "source" => ("uri", false),
            "profile" => ("canonical", true),
            "security" => ("Coding", true),
            "tag" => ("Coding", true),
            _ => return None,
        }),
        "Identifier" => Some(match base {
            "use" => ("code", false),
            "type" => ("CodeableConcept", false),
            "system" => ("uri", false),
            "value" => ("string", false),
            "period" => ("Period", false),
            "assigner" => ("Reference", false),
            _ => return None,
        }),
        "ContactDetail" => Some(match base {
            "name" => ("string", false),
            "telecom" => ("ContactPoint", true),
            _ => return None,
        }),
        "ContactPoint" => Some(match base {
            "system" => ("code", false),
            "value" => ("string", false),
            "use" => ("code", false),
            "rank" => ("positiveInt", false),
            "period" => ("Period", false),
            _ => return None,
        }),
        "CodeableConcept" => Some(match base {
            "coding" => ("Coding", true),
            "text" => ("string", false),
            _ => return None,
        }),
        "Coding" => Some(match base {
            "system" => ("uri", false),
            "version" => ("string", false),
            "code" => ("code", false),
            "display" => ("string", false),
            "userSelected" => ("boolean", false),
            _ => return None,
        }),
        "Extension" => Some(match base {
            "url" => ("uri", false),
            "extension" => ("Extension", true),
            _ => return None,
        }),
        "UsageContext" => Some(match base {
            "code" => ("Coding", false),
            _ => return None,
        }),
        "Reference" => Some(match base {
            "reference" => ("string", false),
            "type" => ("uri", false),
            "identifier" => ("Identifier", false),
            "display" => ("string", false),
            _ => return None,
        }),
        "Quantity" => Some(match base {
            "value" => ("decimal", false),
            "comparator" => ("code", false),
            "unit" => ("string", false),
            "system" => ("uri", false),
            "code" => ("code", false),
            _ => return None,
        }),
        "Period" => Some(match base {
            "start" => ("dateTime", false),
            "end" => ("dateTime", false),
            _ => return None,
        }),
        _ => None,
    }
}

/// Resolve a `value[x]`/`fixed[x]`/`pattern[x]`/`minValue[x]`/`maxValue[x]`/
/// `defaultValue[x]` concrete key to its FHIR type.
fn choice_type(type_name: &str, base: &str) -> Option<&'static str> {
    let prefixes: &[&str] = match type_name {
        "ElementDefinition" => &["fixed", "pattern", "minValue", "maxValue", "defaultValue"],
        "ElementDefinitionExample" => &["value"],
        "Extension" | "UsageContext" => &["value"],
        _ => &[],
    };
    let suffix = prefixes.iter().find_map(|p| base.strip_prefix(p))?;
    if suffix.is_empty() || !suffix.chars().next().unwrap().is_ascii_uppercase() {
        return None;
    }
    Some(match suffix {
        "Coding" => "Coding",
        "CodeableConcept" => "CodeableConcept",
        "Quantity" => "Quantity",
        "Reference" => "Reference",
        "Period" => "Period",
        "Identifier" => "Identifier",
        "Code" => "code",
        "String" => "string",
        "Uri" => "uri",
        "Url" => "url",
        "Canonical" => "canonical",
        "Markdown" => "markdown",
        "Boolean" => "boolean",
        "Integer" => "integer",
        "UnsignedInt" => "unsignedInt",
        "PositiveInt" => "positiveInt",
        "Decimal" => "decimal",
        "DateTime" => "dateTime",
        "Date" => "date",
        "Instant" => "instant",
        "Id" => "id",
        "Time" => "time",
        "Oid" => "oid",
        "Uuid" => "uuid",
        "Base64Binary" => "base64Binary",
        _ => return None,
    })
}

/// Resolve a caret path on a resource/datatype into segments + the leaf type.
pub(crate) fn resolve_path(resource_type: &str, caret_path: &str) -> Option<(Vec<Seg>, String)> {
    let parts = split_caret_path(caret_path);
    if parts.is_empty() {
        return None;
    }
    let mut cur = resource_type.to_string();
    let mut segs = Vec::with_capacity(parts.len());
    let mut leaf_ty = String::new();
    let n = parts.len();
    for (i, part) in parts.iter().enumerate() {
        // Proper multi-bracket parse: base + list of brackets.
        let pp = fhir_model::parse_fsh_path(part);
        let (base, brackets): (String, Vec<String>) = match pp.into_iter().next() {
            Some(p) => (p.base, p.brackets),
            None => (part.clone(), vec![]),
        };
        let (ty, array) = match field_def(&cur, &base) {
            Some(v) => v,
            None => match choice_type(&cur, &base) {
                Some(c) => (c, false),
                // Every BackboneElement/datatype can carry extension/modifierExtension.
                None if base == "extension" || base == "modifierExtension" => ("Extension", true),
                None if base == "id" => ("string", false),
                None => return None,
            },
        };
        // Determine slice_url (non-numeric bracket on extension) and numeric index.
        let mut index = None;
        let mut slice_url = None;
        for b in &brackets {
            if b.chars().all(|c| c.is_ascii_digit()) && !b.is_empty() {
                index = b.parse::<usize>().ok();
            } else if base == "extension" || base == "modifierExtension" {
                slice_url = Some(b.clone());
            }
        }
        // Primitive-sibling redirect: navigating deeper than a primitive (e.g.
        // `targetProfile[0].extension`) targets the `_`-sibling array.
        let key = if i < n - 1 && is_primitive_type(ty) {
            format!("_{base}")
        } else {
            base
        };
        segs.push(Seg {
            key,
            array,
            slice_url,
            index,
        });
        if i == n - 1 {
            leaf_ty = ty.to_string();
        } else {
            cur = ty.to_string();
        }
    }
    Some((segs, leaf_ty))
}
