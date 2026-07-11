//! Page-shell emission — the validated core (1297/1297 byte-identical vs the
//! publisher's raw `temp/pages/*.html` for US Core F0).
//!
//! Port of `PublisherGenerator.makeTemplates` (PublisherGenerator.java:1019) +
//! `genWrapperInner` (PublisherGenerator.java:1378) + `doReplacements`
//! (IGKnowledgeProvider.java:147).

use std::collections::BTreeMap;

use anyhow::{bail, Result};

use crate::{ProducerInputs, Resource, ResourcePageMetadata, ResourcePageRole};

/// The `{{[...]}}` substitution: `doReplacements(String, FetchedResource, vars,
/// format)` (IGKnowledgeProvider.java:147). `vars` always carries
/// `langsuffix=""` (lang == null path) in a single-language build.
fn do_replacements(s: &str, r: &Resource, fmt: Option<&str>) -> String {
    let name = format!(
        "{}{}-html",
        r.id,
        fmt.map(|f| format!("-{f}")).unwrap_or_default()
    );
    let mut out = s
        .replace("{{[title]}}", &r.title())
        .replace("{{[name]}}", &name)
        .replace("{{[id]}}", &r.id);
    if let Some(f) = fmt {
        out = out.replace("{{[fmt]}}", f);
    }
    out = out
        .replace("{{[type]}}", &r.rt)
        .replace("{{[uid]}}", &format!("{}={}", r.rt, r.id))
        .replace("{{[langsuffix]}}", "");
    out
}

/// Read a layout file relative to the layout root (the publisher resolves layout
/// paths relative to the config-file dir; `template-base` values look like
/// `template/layouts/layout-profile.html`).
fn read_layout(inputs: &ProducerInputs, rel: &str) -> Option<String> {
    inputs.layouts.read(rel)
}

/// Emit one wrapper page (`genWrapperInner`): if the `template-*` property
/// resolves to a non-empty layout path, read it, substitute, and record it under
/// the resolved output filename.
fn emit_one(
    inputs: &ProducerInputs,
    r: &Resource,
    template_prop: &str,
    output_prop: Option<&str>,
    extension: &str,
    fmt: Option<&str>,
    role: ResourcePageRole,
    pages: &mut BTreeMap<String, String>,
    resource_pages: &mut BTreeMap<String, ResourcePageMetadata>,
) -> Result<()> {
    let Some(tmpl) = inputs
        .defaults
        .get_property(r, template_prop)
        .filter(|s| !s.is_empty())
    else {
        return Ok(());
    };
    let Some(layout) = read_layout(inputs, &tmpl) else {
        return Ok(());
    };
    let content = do_replacements(&layout, r, fmt);
    // determineOutputName (PublisherGenerator.java:1444)
    let out = output_prop
        .and_then(|p| inputs.defaults.get_property(r, p))
        .unwrap_or_else(|| {
            let ext = if extension.is_empty() {
                String::new()
            } else {
                format!("-{extension}")
            };
            let f = if fmt.is_some() { ".{{[fmt]}}" } else { "" };
            format!("{{{{[type]}}}}-{{{{[id]}}}}{ext}{f}.html")
        });
    let out = do_replacements(&out, r, fmt);
    // The shell FILE location carries the page-dir prefix (must equal the render
    // surface's page.path); FLAT for native/producer_gate, `en/` for the editor.
    let path = format!("{}{out}", inputs.page_prefix);
    if let Some(existing) = resource_pages.get(&path) {
        bail!(
            "Publisher shell output collision at {path}: {}/{} {:?} and {}/{} {:?}",
            existing.resource_type,
            existing.id,
            existing.role,
            r.rt,
            r.id,
            role
        );
    }
    if pages.contains_key(&path) {
        bail!("Publisher shell output collision at {path}: shell has no matching subject");
    }
    pages.insert(path.clone(), content);
    resource_pages.insert(
        path,
        ResourcePageMetadata {
            resource_type: r.rt.clone(),
            id: r.id.clone(),
            title: r.title(),
            role,
        },
    );
    Ok(())
}

/// `makeTemplates` for every resource: base + definitions + each extraTemplate
/// (skipping `format`/`defns` in the loop, as the publisher does).
pub fn emit_shells(
    inputs: &ProducerInputs,
    pages: &mut BTreeMap<String, String>,
    resource_pages: &mut BTreeMap<String, ResourcePageMetadata>,
) -> Result<()> {
    for r in &inputs.resources {
        // base page
        emit_one(
            inputs,
            r,
            "template-base",
            Some("base"),
            "",
            None,
            ResourcePageRole::Primary,
            pages,
            resource_pages,
        )?;
        // definitions page
        emit_one(
            inputs,
            r,
            "template-defns",
            Some("defns"),
            "definitions",
            None,
            ResourcePageRole::Companion,
            pages,
            resource_pages,
        )?;
        // extraTemplates
        for tn in &inputs.defaults.extra_templates {
            if tn == "format" || tn == "defns" {
                continue;
            }
            let template_prop = format!("template-{tn}");
            emit_one(
                inputs,
                r,
                &template_prop,
                Some(tn),
                tn,
                None,
                ResourcePageRole::Companion,
                pages,
                resource_pages,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use serde_json::json;

    use super::*;

    #[test]
    fn configured_output_names_keep_exact_resource_subjects() {
        let body = json!({
            "resourceType": "StructureDefinition",
            "id": "patient-profile",
            "name": "PatientProfile",
            "type": "Patient",
            "kind": "resource",
            "derivation": "constraint"
        });
        let resource =
            Resource::from_value(body, "StructureDefinition-patient-profile.json", false).unwrap();
        let config = json!({
            "defaults": {
                "StructureDefinition": {
                    "template-base": "layouts/base.html",
                    "base": "profile-{{[id]}}-landing.html",
                    "template-defns": "layouts/definitions.html",
                    "defns": "profile-{{[id]}}-details.html"
                }
            }
        });
        let inputs = ProducerInputs::from_memory(
            vec![resource],
            &config,
            HashMap::from([
                ("layouts/base.html".into(), "base {{[uid]}}".into()),
                (
                    "layouts/definitions.html".into(),
                    "definitions {{[uid]}}".into(),
                ),
            ]),
            &json!({ "resourceType": "ImplementationGuide", "id": "guide" }),
            HashSet::new(),
            "en/",
        )
        .unwrap();

        let output = crate::produce(&inputs).unwrap();
        let primary = &output.resource_pages["en/profile-patient-profile-landing.html"];
        assert_eq!(primary.resource_type, "StructureDefinition");
        assert_eq!(primary.id, "patient-profile");
        assert_eq!(primary.title, "PatientProfile");
        assert_eq!(primary.role, ResourcePageRole::Primary);
        assert_eq!(
            output.resource_pages["en/profile-patient-profile-details.html"].role,
            ResourcePageRole::Companion
        );
    }

    #[test]
    fn two_resource_subjects_cannot_claim_one_configured_shell() {
        let resources = ["first", "second"]
            .into_iter()
            .map(|id| {
                Resource::from_value(
                    json!({
                        "resourceType": "ValueSet",
                        "id": id,
                        "name": id,
                        "status": "draft"
                    }),
                    &format!("ValueSet-{id}.json"),
                    false,
                )
                .unwrap()
            })
            .collect();
        let inputs = ProducerInputs::from_memory(
            resources,
            &json!({"defaults":{"ValueSet":{
                "template-base":"layouts/base.html",
                "base":"shared.html"
            }}}),
            HashMap::from([("layouts/base.html".into(), "{{[uid]}}".into())]),
            &json!({"resourceType":"ImplementationGuide","id":"guide"}),
            HashSet::new(),
            "en/",
        )
        .unwrap();

        let error = crate::produce(&inputs).unwrap_err().to_string();
        assert!(error.contains("Publisher shell output collision at en/shared.html"));
        assert!(error.contains("ValueSet/first"));
        assert!(error.contains("ValueSet/second"));
    }
}
