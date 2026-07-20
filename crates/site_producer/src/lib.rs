//! `site_producer` — the missing IG-Publisher piece: synthesize a stock-template
//! site's per-artifact **page shells** and the **`_data/*.json` site-data model**
//! from a renderer-neutral prepared guide + the template's `config.json` and
//! addressed layout bytes. Filesystem capture and publication belong to host
//! adapters; this crate is a deterministic in-memory projection.
//!
//! Fragment BODIES (`_includes/*-snapshot.xhtml` etc.) are NOT produced here —
//! registered generated includes cross the page renderer's typed artifact
//! resolver into `render_sd::engine::FragmentEngine`. This module produces ONLY the SHELLS
//! (the `.html` pages that `{% include X-snapshot.xhtml %}` from) and the
//! `_data` model those shells read via `site.data.*`.
//!
//! ## Publisher parity model (cited)
//!
//! The whole page-shell pass is the publisher's `makeTemplates` →
//! `genWrapper`/`genWrapperInner`
//! (`org.hl7.fhir.igtools.publisher.PublisherGenerator`, pinned clone):
//!
//! * `makeTemplates` (PublisherGenerator.java:1019) emits, for each resource:
//!   the **base** page (`template-base` layout → `base` filename), the
//!   **definitions** page (`template-defns` → `defns`), and each **extraTemplate**
//!   (`template-<name>` → `<name>`), skipping `format`/`defns` in the loop.
//! * `genWrapperInner` (PublisherGenerator.java:1378): reads the layout file,
//!   runs `IGKnowledgeProvider.doReplacements` (the `{{[id]}}`/`{{[type]}}`/
//!   `{{[fmt]}}`/`{{[title]}}`/`{{[name]}}`/`{{[uid]}}` substitution,
//!   IGKnowledgeProvider.java:147), writes to `<tempDir>/<outputName>`. Layouts
//!   with an empty `template-*` value emit nothing.
//! * Which config applies to a resource: `findConfiguration`
//!   (IGKnowledgeProvider.java:417) → for StructureDefinition the flavor comes
//!   from `getSDType` (IGKnowledgeProvider.java:293: `extension` if type ==
//!   Extension; `resourcedefn` if kind==resource && derivation==specialization;
//!   else kind[+`:abstract`]); examples fall to the `example` default; else the
//!   type's default, else `Any`.
//! * Property resolution precedence: `getProperty` (IGKnowledgeProvider.java:255)
//!   — resource's own config, then `StructureDefinition:<flavor>`, then the
//!   type default, then `Any`.
//!
//! Historical oracle validation: for the US Core F0 build, this reproduced
//! **1297/1297** page shells byte-identically against the publisher's raw
//! `temp/pages/*.html`. Current execution enters through an in-memory
//! `PreparedGuide`.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use serde_json::Value;

pub mod config;
pub mod data;
pub mod menu;
pub mod publisher_runtime;
pub mod resource;
pub mod shells;
mod structural;

pub use config::Defaults;
pub use resource::Resource;

/// Renderer-owned presentation metadata for one emitted resource page. This is
/// adjacent to the shell bytes, not inferred later from an output filename.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourcePageMetadata {
    pub resource_type: String,
    pub id: String,
    pub title: String,
    pub role: ResourcePageRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourcePageRole {
    Primary,
    Companion,
}

/// The full producer output. Keys are complete paths relative to the Publisher
/// `/site` mount (`en/index.html`, `_data/pages.json`, `_includes/menu.xml`).
/// One catalog makes ownership and collision handling uniform across pages,
/// data, and includes; SiteEngine only mounts this map.
#[derive(Debug, Default)]
pub struct SiteProducerOutput {
    pub files: BTreeMap<String, Vec<u8>>,
    /// Already-materialized public outputs that bypass page rendering. Their
    /// paths share the same collision-checked output namespace as rendered
    /// pages, but their bytes do not need to be mounted as Liquid inputs.
    pub public_outputs: BTreeMap<String, ProducedPublicOutput>,
    /// Exact resource subject for resource-owned shells, keyed by the same
    /// final configured path as `files`. Narrative pages are not emitted by
    /// this producer and therefore have no entry.
    pub resource_pages: BTreeMap<String, ResourcePageMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducedPublicOutput {
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub source: String,
}

/// Address-free view of the template layout bytes captured for one immutable
/// project revision.
pub struct LayoutSource(std::collections::HashMap<String, String>);

impl LayoutSource {
    fn new(layouts: std::collections::HashMap<String, String>) -> Self {
        Self(layouts)
    }

    pub fn read(&self, rel: &str) -> Option<String> {
        self.0
            .get(rel)
            // Config paths are build-root-relative (`template/layouts/x`); a
            // captured template tree may be keyed template-root-relative
            // (`layouts/x`). Accept either spelling without consulting a host.
            .or_else(|| self.0.get(rel.strip_prefix("template/").unwrap_or(rel)))
            .cloned()
    }
}

/// Complete in-memory inputs to the Publisher projection.
pub struct ProducerInputs {
    /// Every non-primary resource that gets a page.
    pub resources: Vec<Resource>,
    /// The template's merged `config.json` `defaults` + `extraTemplates`.
    pub defaults: Defaults,
    /// Captured template layout bytes.
    pub layouts: LayoutSource,
    /// IG-level fields used as fallbacks / for the `_data` model (publisher, etc.).
    pub ig: IgContext,
    /// The raw ImplementationGuide resource JSON — the `_data` builders walk its
    /// `definition.page`/`definition.resource`/`definition.parameter` + top-level
    /// `extension`/`contact`/`jurisdiction` to derive pages.json / resources.json /
    /// fhir.json / info.json. `Value::Null` when no IG was found.
    pub ig_json: Value,
    /// The set of page-fragment include filenames actually present (staged
    /// `_includes/*` or source `input/{pagecontent,intro-notes}/*`), e.g.
    /// `StructureDefinition-us-core-patient-intro.md`. `pages.json` emits a
    /// page's `intro`/`notes` key ONLY when the corresponding file is here —
    /// `fragment-notes.html` renders an unconditional `<h3>Notes:</h3>` header
    /// whenever `notes != null`, so emitting a non-existent name would inject a
    /// spurious heading (publisher gates on file existence, PublisherGenerator
    /// `addPageDataRow` :3690).
    pub page_includes: std::collections::HashSet<String>,
    /// Exact prepared PageContent paths. Structural defaults yield ownership
    /// to an authored `toc.md|html` or `artifacts.md|html` instead of relying
    /// on a later, silent map replacement.
    pub authored_page_content: std::collections::HashSet<String>,
    /// Exact prepared Include/ResourceContent paths. Generated structural
    /// fragments likewise yield to an explicit authored path with the same
    /// mounted name.
    pub authored_include_content: std::collections::HashSet<String>,
    /// Prepared semantic navigation menu used to generate `menu.xml`.
    pub menu: Vec<site_build::MenuNode>,
    /// Output page-directory prefix for the shell file locations AND the
    /// `pages.json` KEYS — these two must be equal to the render surface's
    /// `page.path` (`site.data.pages[page.path]`). Empty produces a flat output
    /// tree; `"en/"` produces the standard localized Publisher page tree.
    /// Only the shell key + `pages.json` key carry it — `artifacts.json` keys,
    /// `structuredefinitions.json.path`, breadcrumb/prev/next/example hrefs stay
    /// FLAT (in-site relative links).
    pub page_prefix: String,
}

impl ProducerInputs {
    /// Construct from already-captured resources, template configuration and
    /// layouts, and the primary IG resource. Resources are normalized into the
    /// IG `definition.resource[]` order (see [`order_resources`]).
    pub fn from_memory(
        resources: Vec<Resource>,
        config_json: &Value,
        layouts: std::collections::HashMap<String, String>,
        ig: &Value,
        page_includes: std::collections::HashSet<String>,
        page_prefix: &str,
    ) -> Result<ProducerInputs> {
        let mut resources = resources;
        if let Some(order) = ig_resource_order(ig) {
            order_resources(&mut resources, &order);
        }
        Ok(ProducerInputs {
            resources,
            defaults: Defaults::from_value(config_json)?,
            layouts: LayoutSource::new(layouts),
            ig: IgContext::from_ig(ig),
            ig_json: ig.clone(),
            page_includes,
            authored_page_content: std::collections::HashSet::new(),
            authored_include_content: std::collections::HashSet::new(),
            menu: Vec::new(),
            page_prefix: page_prefix.to_string(),
        })
    }

    /// Direct Publisher projection from the one renderer-neutral handoff.
    /// Primary-guide selection, resource order, example classification, and
    /// authored fragment presence all come from `PreparedGuide`; no ambient
    /// compiled resource or site-file tree is consulted.
    pub fn from_prepared(
        prepared: &site_build::PreparedGuide,
        config_json: &Value,
        layouts: std::collections::HashMap<String, String>,
        page_prefix: &str,
    ) -> Result<ProducerInputs> {
        let primary = prepared
            .resources
            .iter()
            .filter(|resource| resource.key == prepared.guide.implementation_guide)
            .collect::<Vec<_>>();
        let primary = match primary.as_slice() {
            [primary] if primary.key.resource_type == "ImplementationGuide" => *primary,
            [] => bail!(
                "PreparedGuide primary {}/{} is absent",
                prepared.guide.implementation_guide.resource_type,
                prepared.guide.implementation_guide.id
            ),
            [_] => bail!("PreparedGuide primary is not an ImplementationGuide"),
            _ => bail!("PreparedGuide contains duplicate primary ImplementationGuides"),
        };
        let ig = &primary.resource;
        let examples = example_reference_set(ig);
        let mut resources = prepared
            .resources
            .iter()
            .filter(|resource| resource.key != prepared.guide.implementation_guide)
            .map(|resource| {
                let key = &resource.key;
                let reference = (key.resource_type.clone(), key.id.clone());
                let file_name = format!("{}-{}.json", key.resource_type, key.id);
                let resource = Resource::from_value(
                    resource.resource.clone(),
                    &file_name,
                    examples.contains(&reference),
                )
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "PreparedGuide resource {}/{} disagrees with its JSON identity",
                        key.resource_type,
                        key.id
                    )
                })?;
                if resource.rt != key.resource_type || resource.id != key.id {
                    bail!(
                        "PreparedGuide resource {}/{} disagrees with JSON identity {}/{}",
                        key.resource_type,
                        key.id,
                        resource.rt,
                        resource.id
                    );
                }
                Ok(resource)
            })
            .collect::<Result<Vec<_>>>()?;
        if let Some(order) = ig_resource_order(ig) {
            order_resources(&mut resources, &order);
        }

        let page_includes = prepared
            .authored_files
            .iter()
            .filter(|file| {
                matches!(
                    file.role,
                    site_build::AuthoredFileRole::PageContent
                        | site_build::AuthoredFileRole::ResourceContent
                        | site_build::AuthoredFileRole::Include
                )
            })
            .filter_map(|file| file.path.as_str().rsplit('/').next().map(str::to_string))
            .collect();
        let authored_page_content = prepared
            .authored_files
            .iter()
            .filter(|file| file.role == site_build::AuthoredFileRole::PageContent)
            .map(|file| file.path.as_str().to_string())
            .collect();
        let authored_include_content = prepared
            .authored_files
            .iter()
            .filter(|file| {
                matches!(
                    file.role,
                    site_build::AuthoredFileRole::Include
                        | site_build::AuthoredFileRole::ResourceContent
                )
            })
            .map(|file| file.path.as_str().to_string())
            .collect();

        Ok(ProducerInputs {
            resources,
            defaults: Defaults::from_value(config_json)?,
            layouts: LayoutSource::new(layouts),
            ig: IgContext::from_ig(ig),
            ig_json: ig.clone(),
            page_includes,
            authored_page_content,
            authored_include_content,
            menu: prepared.menu.clone(),
            page_prefix: page_prefix.to_string(),
        })
    }
}

/// Reorder resources to the IG `definition.resource[]` processing order.
pub fn order_resources(resources: &mut [Resource], order: &[(String, String)]) {
    let pos: std::collections::HashMap<(&str, &str), usize> = order
        .iter()
        .enumerate()
        .map(|(i, (rt, id))| ((rt.as_str(), id.as_str()), i))
        .collect();
    resources.sort_by_key(|r| {
        pos.get(&(r.rt.as_str(), r.id.as_str()))
            .copied()
            .unwrap_or(usize::MAX)
    });
}

/// IG-level context for `_data` derivation and publisher-inherited fields.
#[derive(Debug, Default, Clone)]
pub struct IgContext {
    pub id: Option<String>,
    pub name: Option<String>,
    pub title: Option<String>,
    pub version: Option<String>,
    pub canonical: Option<String>,
    pub publisher: Option<String>,
}

impl IgContext {
    /// Extract IG context from an ImplementationGuide resource JSON.
    pub fn from_ig(ig: &Value) -> IgContext {
        let s = |k: &str| ig.get(k).and_then(Value::as_str).map(str::to_string);
        IgContext {
            id: s("id"),
            name: s("name"),
            title: s("title"),
            version: s("version"),
            canonical: s("url").map(|u| {
                // canonical = url up to /ImplementationGuide/
                u.split("/ImplementationGuide/")
                    .next()
                    .unwrap_or(&u)
                    .to_string()
            }),
            publisher: s("publisher"),
        }
    }
}

/// The canonical resource processing order = the IG `definition.resource[]`
/// list, as `(resourceType, id)` pairs parsed from each `reference.reference`
/// (`Type/id`). Returns `None` when the IG has no definition.resource.
fn ig_resource_order(ig: &Value) -> Option<Vec<(String, String)>> {
    let arr = ig.get("definition")?.get("resource")?.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for r in arr {
        if let Some(ref_) = r
            .get("reference")
            .and_then(|x| x.get("reference"))
            .and_then(Value::as_str)
        {
            if let Some((rt, id)) = ref_.split_once('/') {
                out.push((rt.to_string(), id.to_string()));
            }
        }
    }
    Some(out)
}

fn example_reference_set(ig: &Value) -> std::collections::HashSet<(String, String)> {
    ig.pointer("/definition/resource")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry.get("exampleBoolean") == Some(&Value::Bool(true))
                || matches!(entry.get("exampleCanonical"), Some(Value::String(_)))
                || matches!(entry.get("profile"), Some(Value::String(_)))
        })
        .filter_map(|entry| {
            entry
                .pointer("/reference/reference")
                .and_then(Value::as_str)
                .and_then(|reference| reference.split_once('/'))
                .map(|(resource_type, id)| (resource_type.to_string(), id.to_string()))
        })
        .collect()
}

/// Run the full producer: page shells + the derivable `_data` files.
pub fn produce(inputs: &ProducerInputs) -> Result<SiteProducerOutput> {
    let model = structural::StructuralModel::from_ig(&inputs.ig_json);
    let mut pages = BTreeMap::new();
    let mut data_files = BTreeMap::new();
    let mut includes = BTreeMap::new();
    let mut resource_pages = BTreeMap::new();
    let mut public_outputs = BTreeMap::new();
    shells::emit_shells(inputs, &mut pages, &mut resource_pages, &mut public_outputs)?;
    data::emit_data(inputs, &model, &mut data_files)?;
    structural::emit_structural_pages(inputs, &model, &mut pages, &mut includes)?;
    if let Some(menu) = menu::menu_xml(&inputs.menu) {
        includes.insert("menu.xml".into(), menu);
    }
    let mut files = BTreeMap::new();
    merge_produced_files(&mut files, "", pages)?;
    merge_produced_files(&mut files, "_data/", data_files)?;
    merge_produced_files(&mut files, "_includes/", includes)?;
    Ok(SiteProducerOutput {
        files,
        public_outputs,
        resource_pages,
    })
}

fn merge_produced_files(
    output: &mut BTreeMap<String, Vec<u8>>,
    prefix: &str,
    files: BTreeMap<String, String>,
) -> Result<()> {
    for (name, body) in files {
        let path = format!("{prefix}{name}");
        if output.insert(path.clone(), body.into_bytes()).is_some() {
            bail!("Publisher producer collision at {path}");
        }
    }
    Ok(())
}
