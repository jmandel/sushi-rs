//! Markdown / relative-link rewriting and Publisher native-text quirks. Shared
//! by the native-R5 projection pass and by `normalize_inherited_element`.

#![allow(unused_imports)]
use anyhow::{bail, Context};
use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::*;

pub(crate) fn rewrite_markdown_links(element: &mut Value, spec_url: &str, keep_known_relative: bool) {
    for key in [
        "definition",
        "comment",
        "requirements",
        "meaningWhenMissing",
    ] {
        rewrite_string_field(element, key, spec_url, keep_known_relative);
    }
    if let Some(binding) = element.get_mut("binding") {
        rewrite_string_field(binding, "description", spec_url, keep_known_relative);
    }
    if let Some(Value::Array(exts)) = element.get_mut("extension") {
        for ext in exts {
            rewrite_string_field(ext, "valueMarkdown", spec_url, keep_known_relative);
        }
    }
}

pub(crate) fn rewrite_string_field(value: &mut Value, key: &str, spec_url: &str, keep_known_relative: bool) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let Some(Value::String(text)) = obj.get_mut(key) else {
        return;
    };
    *text = process_relative_markdown_urls(text, spec_url, keep_known_relative);
    if let Some(normalized) = publisher_native_text_quirk(text) {
        *text = normalized.to_string();
    }
}

pub(crate) fn publisher_native_text_quirk(text: &str) -> Option<&'static str> {
    match text {
        "Codes for tests or services that can be carried out by a designated individual, organization or healthcare service.  For laboratory, LOINC is  (preferred)[http://build.fhir.org/terminologies.html#preferred] and a valueset using LOINC Order codes is available [here](http://hl7.org/fhir/R4/valueset-diagnostic-requests.html)." => {
            Some("Codes for tests or services that can be carried out by a designated individual, organization or healthcare service.  For laboratory, LOINC is  (preferred)[http://build.fhir.org/terminologies.html#preferred].")
        }
        _ => None,
    }
}

pub(crate) fn process_relative_markdown_urls(
    input: &str,
    spec_url: &str,
    keep_known_relative: bool,
) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut copied_until = 0;
    while i < bytes.len() {
        if bytes[i] == b']' && i + 1 < bytes.len() && bytes[i + 1] == b'(' {
            out.push_str(&input[copied_until..i]);
            out.push(']');
            out.push('(');
            i += 2;
            let start = i;
            while i < bytes.len() && bytes[i] != b')' {
                i += 1;
            }
            let target = &input[start..i];
            if is_relative_spec_link(target) {
                if keep_known_relative && publisher_native_keeps_relative_link(target) {
                    out.push_str(target);
                } else {
                    match publisher_native_link_target(target) {
                        Some(absolute) => out.push_str(absolute),
                        None => {
                            out.push_str(spec_url);
                            out.push_str(target);
                        }
                    }
                }
            } else {
                out.push_str(target);
            }
            if i < bytes.len() {
                out.push(')');
                i += 1;
            }
            copied_until = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&input[copied_until..]);
    out
}

pub(crate) fn is_relative_spec_link(target: &str) -> bool {
    !target.is_empty()
        && !target.starts_with('#')
        && !target.contains(':')
        && (target.ends_with(".html") || target.contains(".html#") || target.contains(".html?"))
}

pub(crate) fn publisher_native_link_target(target: &str) -> Option<&'static str> {
    match target {
        "device-mappings.html#udi" => Some("http://hl7.org/fhir/device-mappings.html#udi"),
        "event.html" => Some("http://hl7.org/fhir/event.html"),
        "general-requirements.html#required-bindings-when-slicing-by-valuesets" => {
            Some("http://hl7.org/fhir/general-requirements.html#required-bindings-when-slicing-by-valuesets")
        }
        "servicerequest-example-di.html" => {
            Some("http://hl7.org/fhir/servicerequest-example-di.html")
        }
        "null.html" => Some("http://hl7.org/fhir/extension-bodysite.html"),
        "StructureDefinition-us-ph-composition.html" => {
            Some("http://hl7.org/fhir/StructureDefinition-us-ph-composition.html")
        }
        _ => None,
    }
}

pub(crate) fn publisher_native_keeps_relative_link(target: &str) -> bool {
    matches!(
        target,
        "OperationDefinition-Questionnaire-assemble.html"
            | "operational.html#guidelines-for-estimated-time-to-complete-a-dtr-questionnaire"
            | "StructureDefinition-rendering-markdown.html"
            | "StructureDefinition-rendering-xhtml.html"
            | "StructureDefinition-sdc-questionnaire-subQuestionnaire.html"
            | "codesystem-concept-properties.html#concept-properties-itemWeight"
            | "extraction.html"
            | "workflow-extensions.html#instantiation"
            | "questionnaire.html"
    )
}
