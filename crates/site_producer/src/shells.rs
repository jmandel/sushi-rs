//! Page-shell emission — the validated core (1297/1297 byte-identical vs the
//! publisher's raw `temp/pages/*.html` for US Core F0).
//!
//! Port of `PublisherGenerator.makeTemplates` (PublisherGenerator.java:1019) +
//! `genWrapperInner` (PublisherGenerator.java:1378) + `doReplacements`
//! (IGKnowledgeProvider.java:147).

use std::collections::BTreeMap;

use anyhow::Result;

use crate::{ProducerInputs, Resource};

/// The `{{[...]}}` substitution: `doReplacements(String, FetchedResource, vars,
/// format)` (IGKnowledgeProvider.java:147). `vars` always carries
/// `langsuffix=""` (lang == null path) in a single-language build.
fn do_replacements(s: &str, r: &Resource, fmt: Option<&str>) -> String {
    let name = format!("{}{}-html", r.id, fmt.map(|f| format!("-{f}")).unwrap_or_default());
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
    pages: &mut BTreeMap<String, String>,
) {
    let Some(tmpl) = inputs.defaults.get_property(r, template_prop).filter(|s| !s.is_empty()) else {
        return;
    };
    let Some(layout) = read_layout(inputs, &tmpl) else {
        return;
    };
    let content = do_replacements(&layout, r, fmt);
    // determineOutputName (PublisherGenerator.java:1444)
    let out = output_prop
        .and_then(|p| inputs.defaults.get_property(r, p))
        .unwrap_or_else(|| {
            let ext = if extension.is_empty() { String::new() } else { format!("-{extension}") };
            let f = if fmt.is_some() { ".{{[fmt]}}" } else { "" };
            format!("{{{{[type]}}}}-{{{{[id]}}}}{ext}{f}.html")
        });
    let out = do_replacements(&out, r, fmt);
    // The shell FILE location carries the page-dir prefix (must equal the render
    // surface's page.path); FLAT for native/producer_gate, `en/` for the editor.
    pages.insert(format!("{}{out}", inputs.page_prefix), content);
}

/// `makeTemplates` for every resource: base + definitions + each extraTemplate
/// (skipping `format`/`defns` in the loop, as the publisher does).
pub fn emit_shells(inputs: &ProducerInputs, pages: &mut BTreeMap<String, String>) -> Result<()> {
    for r in &inputs.resources {
        // base page
        emit_one(inputs, r, "template-base", Some("base"), "", None, pages);
        // definitions page
        emit_one(inputs, r, "template-defns", Some("defns"), "definitions", None, pages);
        // extraTemplates
        for tn in &inputs.defaults.extra_templates {
            if tn == "format" || tn == "defns" {
                continue;
            }
            let template_prop = format!("template-{tn}");
            emit_one(inputs, r, &template_prop, Some(tn), tn, None, pages);
        }
    }
    Ok(())
}
