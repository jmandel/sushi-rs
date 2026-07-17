//! Publisher-owned structural pages which do not have authored pagecontent.
//!
//! The Java Publisher always creates a wrapper plus generated include for the
//! table of contents and artifact summary. They are ordinary immutable site
//! outputs: the host must not invent them after publication, and the preview
//! Service Worker must not serve an unauthenticated fallback in their place.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use serde_json::Value;

use crate::ProducerInputs;

/// Exact generic wrapper emitted by the standard Publisher pipeline.
const STRUCTURAL_PAGE_SHELL: &str = "---\r\n---\r\n{% include template-page.html %}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StructuralPageKind {
    TableOfContents,
    Artifacts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StructuralPageDescriptor {
    pub kind: StructuralPageKind,
    pub html_name: &'static str,
    pub include_name: &'static str,
    pub title: &'static str,
}

pub(crate) const TABLE_OF_CONTENTS: StructuralPageDescriptor = StructuralPageDescriptor {
    kind: StructuralPageKind::TableOfContents,
    html_name: "toc.html",
    include_name: "toc.xml",
    title: "Table of Contents",
};

pub(crate) const ARTIFACTS: StructuralPageDescriptor = StructuralPageDescriptor {
    kind: StructuralPageKind::Artifacts,
    html_name: "artifacts.html",
    include_name: "artifacts.xml",
    title: "Artifacts Summary",
};

pub(crate) const STRUCTURAL_PAGES: [StructuralPageDescriptor; 2] = [TABLE_OF_CONTENTS, ARTIFACTS];

/// One owned interpretation of the ImplementationGuide navigation, resource
/// registration, and grouping model. Structural bodies and `pages.json` both
/// consume this value; neither independently re-walks permissive JSON.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StructuralModel {
    pub(crate) navigation: Option<NavigationPage>,
    pub(crate) resources: Vec<RegisteredResource>,
    pub(crate) groups: Vec<RegisteredGroup>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NavigationPage {
    pub(crate) name: String,
    pub(crate) title: Option<String>,
    pub(crate) children: Vec<NavigationPage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegisteredResource {
    pub(crate) reference: String,
    pub(crate) href: String,
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) group: Option<String>,
    pub(crate) example_canonical: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegisteredGroup {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
}

impl StructuralModel {
    pub(crate) fn from_ig(ig: &Value) -> Self {
        let definition = ig.get("definition").unwrap_or(&Value::Null);
        let navigation = definition
            .get("page")
            .filter(|page| !page.is_null())
            .map(NavigationPage::from_json);
        let resources = definition
            .get("resource")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(RegisteredResource::from_json)
            .collect();
        let groups = definition
            .get("grouping")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(RegisteredGroup::from_json)
            .collect();
        Self {
            navigation,
            resources,
            groups,
        }
    }

    pub(crate) fn root_name(&self) -> &str {
        self.navigation
            .as_ref()
            .map(|page| page.name.as_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(TABLE_OF_CONTENTS.html_name)
    }

    pub(crate) fn root_title(&self) -> &str {
        self.navigation
            .as_ref()
            .and_then(|page| page.title.as_deref())
            .unwrap_or(TABLE_OF_CONTENTS.title)
    }

    pub(crate) fn resource_names(&self) -> BTreeMap<String, String> {
        self.resources
            .iter()
            .filter_map(|resource| {
                if resource.reference.is_empty() {
                    return None;
                }
                Some((resource.reference.clone(), resource.name.as_ref()?.clone()))
            })
            .collect()
    }

    pub(crate) fn example_groups(&self) -> BTreeMap<String, Vec<(String, String)>> {
        let mut groups: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for resource in &self.resources {
            let Some(profile) = resource.example_canonical.as_ref() else {
                continue;
            };
            groups
                .entry(profile.clone())
                .or_default()
                .push((resource.href.clone(), resource.example_title().to_string()));
        }
        for examples in groups.values_mut() {
            examples.sort();
        }
        groups
    }
}

impl NavigationPage {
    fn from_json(page: &Value) -> Self {
        Self {
            name: page_name(page).unwrap_or_default().to_string(),
            // Preserve the Publisher page-data distinction: a missing title is
            // empty there, while structural display falls back to the name.
            title: page
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string),
            children: page
                .get("page")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(Self::from_json)
                .collect(),
        }
    }

    fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.name)
    }

    fn contains(&self, name: &str) -> bool {
        self.name == name || self.children.iter().any(|child| child.contains(name))
    }
}

impl RegisteredResource {
    fn from_json(resource: &Value) -> Self {
        let reference = resource
            .pointer("/reference/reference")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let fallback_href = reference
            .split_once('/')
            .map(|(resource_type, id)| format!("{resource_type}-{id}.html"))
            .unwrap_or_default();
        let href = implementation_guide_page(resource)
            .unwrap_or(&fallback_href)
            .to_string();
        let name = resource
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string);
        Self {
            reference,
            href,
            name,
            description: resource
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            group: resource
                .get("groupingId")
                .or_else(|| resource.get("package"))
                .and_then(Value::as_str)
                .map(str::to_string),
            example_canonical: resource
                .get("exampleCanonical")
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }

    fn title(&self) -> &str {
        self.name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.reference)
    }

    fn example_title(&self) -> &str {
        self.name.as_deref().unwrap_or_else(|| {
            self.reference
                .split_once('/')
                .map(|(_, id)| id)
                .unwrap_or("")
        })
    }
}

impl RegisteredGroup {
    fn from_json(group: &Value) -> Option<Self> {
        Some(Self {
            id: group.get("id")?.as_str()?.to_string(),
            name: group
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Other")
                .to_string(),
            description: group
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }
}

pub(crate) fn emit_structural_pages(
    inputs: &ProducerInputs,
    model: &StructuralModel,
    pages: &mut BTreeMap<String, String>,
    includes: &mut BTreeMap<String, String>,
) -> Result<()> {
    for descriptor in STRUCTURAL_PAGES {
        if !authored_page(inputs, descriptor.html_name) {
            insert_page(pages, &inputs.page_prefix, descriptor.html_name)?;
        }
        if !authored_include(inputs, descriptor.include_name) {
            let body = match descriptor.kind {
                StructuralPageKind::TableOfContents => toc_fragment(model),
                StructuralPageKind::Artifacts => artifacts_fragment(model),
            };
            insert_include(includes, &inputs.page_prefix, descriptor.include_name, body)?;
        }
    }
    Ok(())
}

fn authored_page(inputs: &ProducerInputs, html_name: &str) -> bool {
    inputs.authored_page_content.contains(html_name)
        || html_name
            .strip_suffix(".html")
            .is_some_and(|stem| inputs.authored_page_content.contains(&format!("{stem}.md")))
}

fn authored_include(inputs: &ProducerInputs, name: &str) -> bool {
    inputs
        .authored_include_content
        .contains(&format!("{}{name}", inputs.page_prefix))
}

fn insert_page(pages: &mut BTreeMap<String, String>, prefix: &str, name: &str) -> Result<()> {
    let path = format!("{prefix}{name}");
    match pages.get(&path) {
        None => {
            pages.insert(path.clone(), STRUCTURAL_PAGE_SHELL.into());
            Ok(())
        }
        Some(existing) if existing == STRUCTURAL_PAGE_SHELL => Ok(()),
        Some(_) => bail!("Publisher structural page collision at {path}"),
    }
}

fn insert_include(
    includes: &mut BTreeMap<String, String>,
    prefix: &str,
    name: &str,
    body: String,
) -> Result<()> {
    let path = format!("{prefix}{name}");
    match includes.get(&path) {
        None => {
            includes.insert(path.clone(), body);
            Ok(())
        }
        Some(existing) if existing == &body => Ok(()),
        Some(_) => bail!("Publisher structural include collision at {path}"),
    }
}

fn page_name(page: &Value) -> Option<&str> {
    page.get("nameUrl")
        .or_else(|| page.get("name"))
        .and_then(Value::as_str)
}

fn toc_fragment(model: &StructuralModel) -> String {
    fn emit(page: &NavigationPage, label: &str, out: &mut String) {
        if page.name.is_empty() {
            return;
        }
        out.push_str("<li><a href=\"");
        out.push_str(&escape_attr(&page.name));
        out.push_str("\"><span class=\"toc-label\">");
        out.push_str(&escape_text(label));
        out.push_str("</span> ");
        out.push_str(&escape_text(page.display_title()));
        out.push_str("</a>");
        if !page.children.is_empty() {
            out.push_str("<ul>");
            for (index, child) in page.children.iter().enumerate() {
                let child_label = if label == "0" {
                    (index + 1).to_string()
                } else {
                    format!("{label}.{}", index + 1)
                };
                emit(child, &child_label, out);
            }
            out.push_str("</ul>");
        }
        out.push_str("</li>");
    }

    let mut out = String::from("<div class=\"markdown-toc publisher-toc\"><ul>");
    if let Some(root) = model.navigation.as_ref() {
        emit(root, "0", &mut out);
    }

    // The Publisher augments the navigation tree with an implicit Artifacts
    // Summary node and the registered resource pages. The compiled IG supplied
    // to a browser build need not already contain that mutable Java-side
    // augmentation, so project it here from the same immutable resource list.
    if !model
        .navigation
        .as_ref()
        .is_some_and(|root| root.contains("artifacts.html"))
    {
        let index = model
            .navigation
            .as_ref()
            .map(|root| root.children.len() + 1)
            .unwrap_or(1);
        out.push_str("<li><a href=\"artifacts.html\"><span class=\"toc-label\">");
        out.push_str(&index.to_string());
        out.push_str("</span> Artifacts Summary</a>");
        if !model.resources.is_empty() {
            out.push_str("<ul>");
            for (resource_index, resource) in model.resources.iter().enumerate() {
                out.push_str("<li><a href=\"");
                out.push_str(&escape_attr(&resource.href));
                out.push_str("\"><span class=\"toc-label\">");
                out.push_str(&format!("{index}.{}", resource_index + 1));
                out.push_str("</span> ");
                out.push_str(&escape_text(resource.title()));
                out.push_str("</a></li>");
            }
            out.push_str("</ul>");
        }
        out.push_str("</li>");
    }
    out.push_str("</ul></div>");
    out
}

fn artifacts_fragment(model: &StructuralModel) -> String {
    let mut visible_groups: Vec<(&str, Option<&str>, Vec<&RegisteredResource>)> = Vec::new();
    let mut claimed = std::collections::BTreeSet::new();
    for group in &model.groups {
        let members = model
            .resources
            .iter()
            .enumerate()
            .filter(|(_, resource)| resource.group.as_deref() == Some(group.id.as_str()))
            .map(|(index, resource)| {
                claimed.insert(index);
                resource
            })
            .collect::<Vec<_>>();
        if !members.is_empty() {
            visible_groups.push((group.name.as_str(), group.description.as_deref(), members));
        }
    }
    let ungrouped = model
        .resources
        .iter()
        .enumerate()
        .filter(|(index, _)| !claimed.contains(index))
        .map(|(_, resource)| resource)
        .collect::<Vec<_>>();
    if !ungrouped.is_empty() {
        visible_groups.push(("Other", None, ungrouped));
    }

    let mut out = String::from(
        "<!--DO NOT EDIT THIS FILE - generated from the immutable ImplementationGuide resource--><div class=\"markdown-toc\"><p>Contents:</p><ul>",
    );
    for (index, (name, _, _)) in visible_groups.iter().enumerate() {
        out.push_str("<li><a href=\"#");
        out.push_str(&(index + 1).to_string());
        out.push_str("\">");
        out.push_str(&escape_text(name));
        out.push_str("</a></li>");
    }
    out.push_str("</ul></div><div><p>This page provides a list of the FHIR artifacts defined as part of this implementation guide.</p>");

    for (index, (name, description, members)) in visible_groups.iter().enumerate() {
        out.push_str("<a name=\"");
        out.push_str(&(index + 1).to_string());
        out.push_str("\"> </a><h3>");
        out.push_str(&escape_text(name));
        out.push_str("</h3>");
        if let Some(description) = description {
            out.push_str("{% capture grouping_desc %}");
            out.push_str(description);
            out.push_str("{% endcapture %}{{ grouping_desc | markdownify }}");
        }
        let show_descriptions = members
            .iter()
            .any(|resource| resource.description.is_some());
        out.push_str("<table class=\"grid\"><col style=\"width:20%\"/><tbody>");
        for resource in members {
            out.push_str("<tr><td style=\"column-width:30%\"><a href=\"");
            out.push_str(&escape_attr(&resource.href));
            out.push_str("\" title=\"");
            out.push_str(&escape_attr(&resource.reference));
            out.push_str("\">");
            out.push_str(&escape_text(resource.title()));
            out.push_str("</a></td>");
            if show_descriptions {
                out.push_str("<td>");
                if let Some(description) = resource.description.as_deref() {
                    out.push_str("{% capture desc %}");
                    out.push_str(description);
                    out.push_str("{% endcapture %}{{ desc | markdownify }}");
                }
                out.push_str("</td>");
            }
            out.push_str("</tr>");
        }
        out.push_str("</tbody></table>");
    }
    out.push_str("</div>");
    out
}

fn implementation_guide_page(resource: &Value) -> Option<&str> {
    resource
        .get("extension")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|extension| {
            extension.get("url").and_then(Value::as_str)
                == Some("http://hl7.org/fhir/StructureDefinition/implementationguide-page")
        })
        .and_then(|extension| extension.get("valueUri"))
        .and_then(Value::as_str)
}

fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(value: &str) -> String {
    escape_text(value).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture_inputs() -> ProducerInputs {
        ProducerInputs::from_memory(
            Vec::new(),
            &json!({"defaults":{}}),
            std::collections::HashMap::new(),
            &json!({
                "resourceType":"ImplementationGuide",
                "id":"guide",
                "definition":{
                    "page":{
                        "nameUrl":"toc.html",
                        "title":"Table of Contents",
                        "generation":"html",
                        "page":[
                            {"nameUrl":"guidance.html","title":"Guidance","generation":"markdown"}
                        ]
                    }
                }
            }),
            std::collections::HashSet::new(),
            "en/",
        )
        .unwrap()
    }

    #[test]
    fn toc_preserves_navigation_hierarchy_and_labels() {
        let root = json!({
            "nameUrl":"toc.html", "title":"Table of Contents", "page":[
                {"nameUrl":"index.html", "title":"Home"},
                {"nameUrl":"guidance.html", "title":"Guidance", "page":[
                    {"nameUrl":"details.html", "title":"Details"}
                ]}
            ]
        });
        let model = StructuralModel::from_ig(&json!({"definition":{"page":root}}));
        let body = toc_fragment(&model);
        assert!(body.contains("<span class=\"toc-label\">0</span> Table of Contents"));
        assert!(body.contains("<span class=\"toc-label\">1</span> Home"));
        assert!(body.contains("<span class=\"toc-label\">2.1</span> Details"));
        assert!(body.contains("Artifacts Summary"));
    }

    #[test]
    fn toc_is_generated_for_an_arbitrary_navigation_root() {
        let mut inputs = fixture_inputs();
        inputs.ig_json["definition"]["page"]["nameUrl"] = json!("index.html");
        inputs.ig_json["definition"]["page"]["title"] = json!("Home");
        let mut pages = BTreeMap::new();
        let mut includes = BTreeMap::new();

        let model = StructuralModel::from_ig(&inputs.ig_json);
        emit_structural_pages(&inputs, &model, &mut pages, &mut includes).unwrap();

        assert!(pages.contains_key("en/toc.html"));
        let toc = &includes["en/toc.xml"];
        assert!(toc.contains("href=\"index.html\""));
        assert!(toc.contains("Home"));
    }

    #[test]
    fn structural_pages_are_complete_without_a_declared_navigation_root() {
        let mut inputs = fixture_inputs();
        inputs.ig_json["definition"] = json!({
            "resource":[{"reference":{"reference":"StructureDefinition/one"},"name":"One"}]
        });
        let mut pages = BTreeMap::new();
        let mut includes = BTreeMap::new();
        let model = StructuralModel::from_ig(&inputs.ig_json);
        emit_structural_pages(&inputs, &model, &mut pages, &mut includes).unwrap();

        assert!(pages.contains_key("en/toc.html"));
        assert!(pages.contains_key("en/artifacts.html"));
        assert!(includes["en/toc.xml"].contains("Artifacts Summary"));
        assert!(includes["en/artifacts.xml"].contains("StructureDefinition-one.html"));

        let mut data = BTreeMap::new();
        crate::data::emit_data(&inputs, &model, &mut data).unwrap();
        let metadata: Value = serde_json::from_str(&data["pages.json"]).unwrap();
        assert_eq!(metadata["en/toc.html"]["title"], "Table of Contents");
        assert_eq!(metadata["en/artifacts.html"]["title"], "Artifacts Summary");
    }

    #[test]
    fn authored_structural_content_has_explicit_precedence() {
        let mut inputs = fixture_inputs();
        inputs.authored_page_content.insert("artifacts.md".into());
        inputs.authored_include_content.insert("en/toc.xml".into());
        let mut pages = BTreeMap::new();
        let mut includes = BTreeMap::new();

        let model = StructuralModel::from_ig(&inputs.ig_json);
        emit_structural_pages(&inputs, &model, &mut pages, &mut includes).unwrap();

        assert!(pages.contains_key("en/toc.html"));
        assert!(!pages.contains_key("en/artifacts.html"));
        assert!(!includes.contains_key("en/toc.xml"));
        assert!(includes.contains_key("en/artifacts.xml"));
    }

    #[test]
    fn artifact_summary_uses_declared_page_and_fallback_page() {
        let model = StructuralModel::from_ig(&json!({"definition":{
            "grouping":[{"id":"g","name":"Profiles","description":"Own profiles"}],
            "resource":[
                {"reference":{"reference":"StructureDefinition/one"},"name":"One","groupingId":"g",
                 "extension":[{"url":"http://hl7.org/fhir/StructureDefinition/implementationguide-page","valueUri":"custom-one.html"}]},
                {"reference":{"reference":"StructureDefinition/two"},"name":"Two","groupingId":"g"}
            ]
        }}));
        let body = artifacts_fragment(&model);
        assert!(body.contains("href=\"custom-one.html\""));
        assert!(body.contains("href=\"StructureDefinition-two.html\""));
        assert!(body.contains("Own profiles"));
    }
}
