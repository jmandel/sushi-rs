//! Serialize the `fsh_model` import AST to `serde_json::Value` in the exact shape
//! the `parse-oracle.cjs` emits: class instances tagged `__type`, JS `Map` as
//! `{"__map": {..}}`, `bigint` as `{"__bigint": ".."}`, the id getter dumped as
//! `_id` (the stored field). Object key order is irrelevant (semantic equality).

use fsh_model::*;
use serde_json::{json, Map, Value as J};

pub fn dump_docs(docs: &[FshDocument]) -> J {
    J::Array(docs.iter().map(dump_doc).collect())
}

fn obj() -> Map<String, J> {
    Map::new()
}

fn map_of<T>(items: &[(String, T)], f: impl Fn(&T) -> J) -> J {
    let mut m = obj();
    for (k, v) in items {
        m.insert(k.clone(), f(v));
    }
    json!({ "__map": J::Object(m) })
}

fn str_map(items: &[(String, String)]) -> J {
    let mut m = obj();
    for (k, v) in items {
        m.insert(k.clone(), J::String(v.clone()));
    }
    json!({ "__map": J::Object(m) })
}

fn dump_doc(d: &FshDocument) -> J {
    let mut o = obj();
    o.insert("__type".into(), "FSHDocument".into());
    o.insert("file".into(), J::String(d.file.clone()));
    o.insert("aliases".into(), str_map(&d.aliases));
    o.insert("profiles".into(), map_of(&d.profiles, dump_structure));
    o.insert("extensions".into(), map_of(&d.extensions, dump_structure));
    o.insert("resources".into(), map_of(&d.resources, dump_structure));
    o.insert("logicals".into(), map_of(&d.logicals, dump_structure));
    o.insert("instances".into(), map_of(&d.instances, dump_instance));
    o.insert("valueSets".into(), map_of(&d.value_sets, dump_valueset));
    o.insert(
        "codeSystems".into(),
        map_of(&d.code_systems, dump_codesystem),
    );
    o.insert("invariants".into(), map_of(&d.invariants, dump_invariant));
    o.insert("ruleSets".into(), map_of(&d.rule_sets, dump_ruleset));
    o.insert(
        "appliedRuleSets".into(),
        map_of(&d.applied_rule_sets, dump_ruleset),
    );
    o.insert("mappings".into(), map_of(&d.mappings, dump_mapping));
    J::Object(o)
}

fn dump_loc(l: &Location) -> J {
    json!({
        "startLine": l.start_line,
        "startColumn": l.start_column,
        "endLine": l.end_line,
        "endColumn": l.end_column,
    })
}

fn dump_source_info(si: &SourceInfo) -> J {
    let mut o = obj();
    if let Some(l) = &si.location {
        o.insert("location".into(), dump_loc(l));
    }
    if let Some(f) = &si.file {
        o.insert("file".into(), J::String(f.clone()));
    }
    if let Some(l) = &si.applied_location {
        o.insert("appliedLocation".into(), dump_loc(l));
    }
    if let Some(f) = &si.applied_file {
        o.insert("appliedFile".into(), J::String(f.clone()));
    }
    J::Object(o)
}

fn put_opt_str(o: &mut Map<String, J>, key: &str, v: &Option<String>) {
    if let Some(s) = v {
        o.insert(key.into(), J::String(s.clone()));
    }
}

fn dump_structure(s: &StructureDef) -> J {
    let mut o = obj();
    let type_name = match s.kind {
        StructureKind::Profile => "Profile",
        StructureKind::Extension => "Extension",
        StructureKind::Logical => "Logical",
        StructureKind::Resource => "Resource",
    };
    o.insert("__type".into(), type_name.into());
    o.insert("sourceInfo".into(), dump_source_info(&s.source_info));
    o.insert("name".into(), J::String(s.name.clone()));
    o.insert("_id".into(), J::String(s.id.clone()));
    o.insert(
        "rules".into(),
        J::Array(s.rules.iter().map(dump_rule).collect()),
    );
    put_opt_str(&mut o, "parent", &s.parent);
    put_opt_str(&mut o, "title", &s.title);
    put_opt_str(&mut o, "description", &s.description);
    if s.kind == StructureKind::Extension {
        o.insert(
            "contexts".into(),
            J::Array(s.contexts.iter().map(dump_ext_context).collect()),
        );
    }
    if s.kind == StructureKind::Logical {
        o.insert(
            "characteristics".into(),
            J::Array(
                s.characteristics
                    .iter()
                    .map(|c| J::String(c.clone()))
                    .collect(),
            ),
        );
    }
    J::Object(o)
}

fn dump_ext_context(c: &ExtensionContext) -> J {
    json!({
        "value": c.value,
        "isQuoted": c.is_quoted,
        "sourceInfo": dump_source_info(&c.source_info),
    })
}

fn dump_instance(i: &Instance) -> J {
    let mut o = obj();
    o.insert("__type".into(), "Instance".into());
    o.insert("sourceInfo".into(), dump_source_info(&i.source_info));
    o.insert("name".into(), J::String(i.name.clone()));
    o.insert("_id".into(), J::String(i.id.clone()));
    o.insert(
        "rules".into(),
        J::Array(i.rules.iter().map(dump_rule).collect()),
    );
    o.insert("usage".into(), J::String(i.usage.clone()));
    if let Some(io) = &i.instance_of {
        o.insert("instanceOf".into(), J::String(io.clone()));
    }
    put_opt_str(&mut o, "title", &i.title);
    put_opt_str(&mut o, "description", &i.description);
    J::Object(o)
}

fn dump_valueset(v: &FshValueSet) -> J {
    let mut o = obj();
    o.insert("__type".into(), "FshValueSet".into());
    o.insert("sourceInfo".into(), dump_source_info(&v.source_info));
    o.insert("name".into(), J::String(v.name.clone()));
    o.insert("_id".into(), J::String(v.id.clone()));
    o.insert(
        "rules".into(),
        J::Array(v.rules.iter().map(dump_rule).collect()),
    );
    put_opt_str(&mut o, "title", &v.title);
    put_opt_str(&mut o, "description", &v.description);
    J::Object(o)
}

fn dump_codesystem(v: &FshCodeSystem) -> J {
    let mut o = obj();
    o.insert("__type".into(), "FshCodeSystem".into());
    o.insert("sourceInfo".into(), dump_source_info(&v.source_info));
    o.insert("name".into(), J::String(v.name.clone()));
    o.insert("_id".into(), J::String(v.id.clone()));
    o.insert(
        "rules".into(),
        J::Array(v.rules.iter().map(dump_rule).collect()),
    );
    put_opt_str(&mut o, "title", &v.title);
    put_opt_str(&mut o, "description", &v.description);
    J::Object(o)
}

fn dump_invariant(v: &Invariant) -> J {
    let mut o = obj();
    o.insert("__type".into(), "Invariant".into());
    o.insert("sourceInfo".into(), dump_source_info(&v.source_info));
    o.insert("name".into(), J::String(v.name.clone()));
    o.insert(
        "rules".into(),
        J::Array(v.rules.iter().map(dump_rule).collect()),
    );
    put_opt_str(&mut o, "description", &v.description);
    put_opt_str(&mut o, "expression", &v.expression);
    put_opt_str(&mut o, "xpath", &v.xpath);
    if let Some(sev) = &v.severity {
        o.insert("severity".into(), dump_code(sev));
    }
    J::Object(o)
}

fn dump_ruleset(v: &RuleSet) -> J {
    let mut o = obj();
    o.insert("__type".into(), "RuleSet".into());
    o.insert("sourceInfo".into(), dump_source_info(&v.source_info));
    o.insert("name".into(), J::String(v.name.clone()));
    o.insert(
        "rules".into(),
        J::Array(v.rules.iter().map(dump_rule).collect()),
    );
    J::Object(o)
}

fn dump_mapping(v: &Mapping) -> J {
    let mut o = obj();
    o.insert("__type".into(), "Mapping".into());
    o.insert("sourceInfo".into(), dump_source_info(&v.source_info));
    o.insert("name".into(), J::String(v.name.clone()));
    o.insert("id".into(), J::String(v.id.clone()));
    o.insert(
        "rules".into(),
        J::Array(v.rules.iter().map(dump_rule).collect()),
    );
    put_opt_str(&mut o, "source", &v.source);
    put_opt_str(&mut o, "target", &v.target);
    put_opt_str(&mut o, "description", &v.description);
    put_opt_str(&mut o, "title", &v.title);
    J::Object(o)
}

// ---------- values ----------

fn dump_code(c: &FshCode) -> J {
    let mut o = obj();
    o.insert("__type".into(), "FshCode".into());
    o.insert("sourceInfo".into(), dump_source_info(&c.source_info));
    o.insert("code".into(), J::String(c.code.clone()));
    put_opt_str(&mut o, "system", &c.system);
    put_opt_str(&mut o, "display", &c.display);
    J::Object(o)
}

fn dump_quantity(q: &FshQuantity) -> J {
    let mut o = obj();
    o.insert("__type".into(), "FshQuantity".into());
    o.insert("sourceInfo".into(), dump_source_info(&q.source_info));
    if let Some(v) = q.value {
        o.insert("value".into(), num_json(v));
    }
    if let Some(u) = &q.unit {
        o.insert("unit".into(), dump_code(u));
    }
    J::Object(o)
}

fn dump_ratio(r: &FshRatio) -> J {
    json!({
        "__type": "FshRatio",
        "sourceInfo": dump_source_info(&r.source_info),
        "numerator": dump_quantity(&r.numerator),
        "denominator": dump_quantity(&r.denominator),
    })
}

fn dump_reference(r: &FshReference) -> J {
    let mut o = obj();
    o.insert("__type".into(), "FshReference".into());
    o.insert("sourceInfo".into(), dump_source_info(&r.source_info));
    o.insert("reference".into(), J::String(r.reference.clone()));
    put_opt_str(&mut o, "display", &r.display);
    J::Object(o)
}

fn dump_canonical(c: &FshCanonical) -> J {
    let mut o = obj();
    o.insert("__type".into(), "FshCanonical".into());
    o.insert("sourceInfo".into(), dump_source_info(&c.source_info));
    o.insert("entityName".into(), J::String(c.entity_name.clone()));
    put_opt_str(&mut o, "version", &c.version);
    J::Object(o)
}

fn num_json(f: f64) -> J {
    // Mirror JS JSON.stringify: an integral number serializes without a decimal
    // point (e.g. 140.0 -> 140), so serde_json must use an integer Number to
    // compare equal to the oracle's output.
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e18 {
        return J::Number((f as i64).into());
    }
    serde_json::Number::from_f64(f)
        .map(J::Number)
        .unwrap_or(J::Null)
}

fn dump_value(v: &Value) -> J {
    match v {
        Value::Bool(b) => J::Bool(*b),
        Value::BigInt(s) => json!({ "__bigint": s }),
        Value::Float(f) => num_json(*f),
        Value::Str(s) => J::String(s.clone()),
        Value::Code(c) => dump_code(c),
        Value::Quantity(q) => dump_quantity(q),
        Value::Ratio(r) => dump_ratio(r),
        Value::Reference(r) => dump_reference(r),
        Value::Canonical(c) => dump_canonical(c),
    }
}

fn dump_flags(o: &mut Map<String, J>, f: &Flags) {
    if f.must_support {
        o.insert("mustSupport".into(), J::Bool(true));
    }
    if f.summary {
        o.insert("summary".into(), J::Bool(true));
    }
    if f.modifier {
        o.insert("modifier".into(), J::Bool(true));
    }
    if f.trial_use {
        o.insert("trialUse".into(), J::Bool(true));
    }
    if f.normative {
        o.insert("normative".into(), J::Bool(true));
    }
    if f.draft {
        o.insert("draft".into(), J::Bool(true));
    }
}

fn dump_only_type(t: &OnlyRuleType) -> J {
    let mut o = obj();
    o.insert("type".into(), J::String(t.type_.clone()));
    if t.is_reference {
        o.insert("isReference".into(), J::Bool(true));
    }
    if t.is_canonical {
        o.insert("isCanonical".into(), J::Bool(true));
    }
    if t.is_codeable_reference {
        o.insert("isCodeableReference".into(), J::Bool(true));
    }
    J::Object(o)
}

fn dump_from(f: &ValueSetComponentFrom) -> J {
    let mut o = obj();
    if let Some(s) = &f.system {
        o.insert("system".into(), J::String(s.clone()));
    }
    if let Some(vs) = &f.value_sets {
        o.insert(
            "valueSets".into(),
            J::Array(vs.iter().map(|s| J::String(s.clone())).collect()),
        );
    }
    J::Object(o)
}

fn dump_filter_value(v: &FilterValue) -> J {
    match v {
        FilterValue::Code(c) => dump_code(c),
        FilterValue::Str(s) => J::String(s.clone()),
        FilterValue::Bool(b) => J::Bool(*b),
        FilterValue::Regex(_) => json!({ "__type": "RegExp" }),
    }
}

fn dump_filter(f: &ValueSetFilter) -> J {
    json!({
        "property": f.property,
        "operator": f.operator,
        "value": dump_filter_value(&f.value),
    })
}

fn dump_rule(r: &Rule) -> J {
    match r {
        Rule::Card {
            source_info,
            path,
            min,
            max,
        } => {
            let mut o = obj();
            o.insert("__type".into(), "CardRule".into());
            o.insert("sourceInfo".into(), dump_source_info(source_info));
            o.insert("path".into(), J::String(path.clone()));
            o.insert(
                "min".into(),
                min.map(|m| J::Number(m.into())).unwrap_or(J::Null),
            );
            o.insert("max".into(), J::String(max.clone()));
            J::Object(o)
        }
        Rule::Flag {
            source_info,
            path,
            flags,
        } => {
            let mut o = obj();
            o.insert("__type".into(), "FlagRule".into());
            o.insert("sourceInfo".into(), dump_source_info(source_info));
            o.insert("path".into(), J::String(path.clone()));
            dump_flags(&mut o, flags);
            J::Object(o)
        }
        Rule::Binding {
            source_info,
            path,
            value_set,
            strength,
        } => {
            json!({
                "__type": "BindingRule",
                "sourceInfo": dump_source_info(source_info),
                "path": path,
                "valueSet": value_set,
                "strength": strength,
            })
        }
        Rule::Assignment {
            source_info,
            path,
            value,
            raw_value,
            exactly,
            is_instance,
        } => {
            let mut o = obj();
            o.insert("__type".into(), "AssignmentRule".into());
            o.insert("sourceInfo".into(), dump_source_info(source_info));
            o.insert("path".into(), J::String(path.clone()));
            if let Some(v) = value {
                o.insert("value".into(), dump_value(v));
            }
            put_opt_str(&mut o, "rawValue", raw_value);
            o.insert("exactly".into(), J::Bool(*exactly));
            o.insert("isInstance".into(), J::Bool(*is_instance));
            J::Object(o)
        }
        Rule::Only {
            source_info,
            path,
            types,
        } => {
            json!({
                "__type": "OnlyRule",
                "sourceInfo": dump_source_info(source_info),
                "path": path,
                "types": J::Array(types.iter().map(dump_only_type).collect()),
            })
        }
        Rule::Contains {
            source_info,
            path,
            items,
        } => {
            let items_j: Vec<J> = items
                .iter()
                .map(|i| {
                    let mut o = obj();
                    o.insert("name".into(), J::String(i.name.clone()));
                    if let Some(t) = &i.type_ {
                        o.insert("type".into(), J::String(t.clone()));
                    }
                    J::Object(o)
                })
                .collect();
            json!({
                "__type": "ContainsRule",
                "sourceInfo": dump_source_info(source_info),
                "path": path,
                "items": J::Array(items_j),
            })
        }
        Rule::CaretValue {
            source_info,
            path,
            caret_path,
            value,
            raw_value,
            is_instance,
            is_code_caret_rule,
            path_array,
        } => {
            let mut o = obj();
            o.insert("__type".into(), "CaretValueRule".into());
            o.insert("sourceInfo".into(), dump_source_info(source_info));
            o.insert("path".into(), J::String(path.clone()));
            o.insert("isCodeCaretRule".into(), J::Bool(*is_code_caret_rule));
            o.insert(
                "pathArray".into(),
                J::Array(path_array.iter().map(|s| J::String(s.clone())).collect()),
            );
            if let Some(cp) = caret_path {
                o.insert("caretPath".into(), J::String(cp.clone()));
            }
            if let Some(v) = value {
                o.insert("value".into(), dump_value(v));
            }
            put_opt_str(&mut o, "rawValue", raw_value);
            o.insert("isInstance".into(), J::Bool(*is_instance));
            J::Object(o)
        }
        Rule::Obeys {
            source_info,
            path,
            invariant,
        } => {
            json!({
                "__type": "ObeysRule",
                "sourceInfo": dump_source_info(source_info),
                "path": path,
                "invariant": invariant,
            })
        }
        Rule::Insert {
            source_info,
            path,
            path_array,
            params,
            rule_set,
        } => {
            json!({
                "__type": "InsertRule",
                "sourceInfo": dump_source_info(source_info),
                "path": path,
                "pathArray": J::Array(path_array.iter().map(|s| J::String(s.clone())).collect()),
                "params": J::Array(params.iter().map(|s| J::String(s.clone())).collect()),
                "ruleSet": rule_set,
            })
        }
        Rule::Path { source_info, path } => {
            json!({
                "__type": "PathRule",
                "sourceInfo": dump_source_info(source_info),
                "path": path,
            })
        }
        Rule::Concept {
            source_info,
            path,
            code,
            display,
            definition,
            system,
            hierarchy,
        } => {
            let mut o = obj();
            o.insert("__type".into(), "ConceptRule".into());
            o.insert("sourceInfo".into(), dump_source_info(source_info));
            o.insert("path".into(), J::String(path.clone()));
            if let Some(s) = system {
                o.insert("system".into(), J::String(s.clone()));
            }
            o.insert(
                "hierarchy".into(),
                J::Array(hierarchy.iter().map(|s| J::String(s.clone())).collect()),
            );
            o.insert("code".into(), J::String(code.clone()));
            put_opt_str(&mut o, "display", display);
            put_opt_str(&mut o, "definition", definition);
            J::Object(o)
        }
        Rule::Mapping {
            source_info,
            path,
            map,
            comment,
            language,
        } => {
            let mut o = obj();
            o.insert("__type".into(), "MappingRule".into());
            o.insert("sourceInfo".into(), dump_source_info(source_info));
            o.insert("path".into(), J::String(path.clone()));
            o.insert("map".into(), J::String(map.clone()));
            put_opt_str(&mut o, "comment", comment);
            if let Some(l) = language {
                o.insert("language".into(), dump_code(l));
            }
            J::Object(o)
        }
        Rule::AddElement {
            source_info,
            path,
            min,
            max,
            flags,
            types,
            content_reference,
            short,
            definition,
        } => {
            let mut o = obj();
            o.insert("__type".into(), "AddElementRule".into());
            o.insert("sourceInfo".into(), dump_source_info(source_info));
            o.insert("path".into(), J::String(path.clone()));
            dump_flags(&mut o, flags);
            o.insert(
                "min".into(),
                min.map(|m| J::Number(m.into())).unwrap_or(J::Null),
            );
            o.insert("max".into(), J::String(max.clone()));
            o.insert(
                "types".into(),
                J::Array(types.iter().map(dump_only_type).collect()),
            );
            put_opt_str(&mut o, "contentReference", content_reference);
            put_opt_str(&mut o, "short", short);
            put_opt_str(&mut o, "definition", definition);
            J::Object(o)
        }
        Rule::VsConcept {
            source_info,
            path,
            inclusion,
            from,
            concepts,
        } => {
            json!({
                "__type": "ValueSetConceptComponentRule",
                "sourceInfo": dump_source_info(source_info),
                "path": path,
                "inclusion": inclusion,
                "from": dump_from(from),
                "concepts": J::Array(concepts.iter().map(dump_code).collect()),
            })
        }
        Rule::VsFilter {
            source_info,
            path,
            inclusion,
            from,
            filters,
        } => {
            json!({
                "__type": "ValueSetFilterComponentRule",
                "sourceInfo": dump_source_info(source_info),
                "path": path,
                "inclusion": inclusion,
                "from": dump_from(from),
                "filters": J::Array(filters.iter().map(dump_filter).collect()),
            })
        }
    }
}
