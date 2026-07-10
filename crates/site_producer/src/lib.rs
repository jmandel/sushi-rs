//! `site_producer` — the missing IG-Publisher piece: synthesize a stock-template
//! site's per-artifact **page shells** and the **`_data/*.json` site-data model**
//! from an IG's source (compiled + predefined resources) + the template's
//! `config.json`. This is what lets the stock template be *produced* from a repo
//! dir tree instead of mounting a pre-baked Java `temp/pages` tree
//! (`-stock.json`), and makes `fig render <ig-source-dir>` work from source.
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
//! Validated: for the US Core F0 build, this reproduces **1297/1297** page
//! shells byte-identical to the publisher's raw `temp/pages/*.html`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;

pub mod config;
pub mod data;
pub mod resource;
pub mod shells;

pub use config::Defaults;
pub use resource::{enumerate_resources, Resource};

/// The full producer output: the `temp/pages` shell + `_data` tree, as an
/// in-memory file map (relative path → bytes). Native callers write it to disk;
/// the wasm/Session surface merges it into the site tree.
#[derive(Debug, Default)]
pub struct SiteProducerOutput {
    /// Page shells, keyed by output filename (e.g. `StructureDefinition-us-core-patient.html`).
    pub pages: BTreeMap<String, String>,
    /// `_data/*.json` files, keyed by bare filename (e.g. `artifacts.json`).
    pub data: BTreeMap<String, String>,
}

impl SiteProducerOutput {
    /// Write the produced tree under `<pages_root>` (typically `<build>/temp/pages`):
    /// shells at the root, `_data/*` under `_data/`. Existing files are overwritten.
    pub fn write_to(&self, pages_root: &Path) -> Result<usize> {
        let data_dir = pages_root.join("_data");
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("mkdir {}", data_dir.display()))?;
        let mut n = 0;
        for (name, body) in &self.pages {
            std::fs::write(pages_root.join(name), body)?;
            n += 1;
        }
        for (name, body) in &self.data {
            std::fs::write(data_dir.join(name), body)?;
            n += 1;
        }
        Ok(n)
    }
}

/// Where layout files are read from. `Dir` resolves `template/layouts/...`
/// paths against a build root (native / `fig`); `Map` serves them from an
/// in-memory `relpath -> contents` table (the wasm / `Session` surface, where
/// the materialized template tree already lives in memory).
pub enum LayoutSource {
    Dir(std::path::PathBuf),
    Map(std::collections::HashMap<String, String>),
}

impl LayoutSource {
    pub fn read(&self, rel: &str) -> Option<String> {
        match self {
            LayoutSource::Dir(root) => std::fs::read_to_string(root.join(rel)).ok(),
            LayoutSource::Map(m) => m
                .get(rel)
                // Config paths are build-root-relative (`template/layouts/x`); an
                // in-memory template tree may be keyed template-root-relative
                // (`layouts/x`). Accept either.
                .or_else(|| m.get(rel.strip_prefix("template/").unwrap_or(rel)))
                .cloned(),
        }
    }
}

/// Inputs to the producer, gathered from a repo/source dir tree.
pub struct ProducerInputs {
    /// Every resource that gets a page: compiled (`fsh-generated/resources`) +
    /// predefined (`input/resources`) + examples (`input/examples`).
    pub resources: Vec<Resource>,
    /// The template's merged `config.json` `defaults` + `extraTemplates`.
    pub defaults: Defaults,
    /// Where layout files come from (build dir or in-memory template map).
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
    /// Output page-directory prefix for the shell file locations AND the
    /// `pages.json` KEYS — these two must be equal to the render surface's
    /// `page.path` (`site.data.pages[page.path]`). Empty (native `fig`,
    /// `producer_gate`: FLAT, byte-exact vs the F0 oracle); `"en/"` for the
    /// editor's `hl7.fhir.template` render (its staged pages live under `en/`).
    /// Only the shell key + `pages.json` key carry it — `artifacts.json` keys,
    /// `structuredefinitions.json.path`, breadcrumb/prev/next/example hrefs stay
    /// FLAT (in-site relative links).
    pub page_prefix: String,
}

impl ProducerInputs {
    /// In-memory constructor for the wasm / Session surface: resources already
    /// parsed, the template's `config.json` as a `Value`, its `layouts/*` as a
    /// `relpath -> contents` map, and the IG resource. `resources` should already
    /// be in the IG `definition.resource[]` order (see [`order_resources`]).
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
            layouts: LayoutSource::Map(layouts),
            ig: IgContext::from_ig(ig),
            ig_json: ig.clone(),
            page_includes,
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

/// Gather producer inputs from a source dir tree. Looks for resources under
/// `fsh-generated/resources`, `input/resources`, `input/examples` and the
/// template config at `template/config.json`, resolving layout paths against the
/// build root (matching the publisher's config-file-relative resolution).
pub fn gather_inputs(build_dir: &Path) -> Result<ProducerInputs> {
    let cfg_path = build_dir.join("template/config.json");
    let defaults = Defaults::load(&cfg_path)
        .with_context(|| format!("load template config {}", cfg_path.display()))?;

    let mut resources = Vec::new();
    let mut implementation_guides = Vec::new();
    let mut ig = IgContext::default();
    let mut ig_json = Value::Null;
    let mut ig_order: Option<Vec<(String, String)>> = None;
    for (sub, is_example) in [
        ("input/resources", false),
        ("input/examples", true),
        ("fsh-generated/resources", false),
    ] {
        let dir = build_dir.join(sub);
        if !dir.is_dir() {
            continue;
        }
        for r in enumerate_resources(&dir, is_example)? {
            if r.rt == "ImplementationGuide" {
                implementation_guides.push(r);
                continue;
            }
            resources.push(r);
        }
    }

    let configured_id = configured_implementation_guide_id(build_dir)?;
    let primary_index = if implementation_guides.is_empty() {
        None
    } else if let Some(id) = configured_id.as_deref() {
        let filename = format!("ImplementationGuide-{id}.json");
        let mut matches: Vec<usize> = implementation_guides
            .iter()
            .enumerate()
            .filter(|(_, guide)| {
                guide.file.file_name().and_then(|name| name.to_str()) == Some(filename.as_str())
            })
            .map(|(index, _)| index)
            .collect();
        if matches.len() > 1 {
            let generated_root = build_dir.join("fsh-generated/resources");
            matches.retain(|index| {
                implementation_guides[*index]
                    .file
                    .starts_with(&generated_root)
            });
        }
        match matches.as_slice() {
            [index] => Some(*index),
            [] if implementation_guides.len() == 1 => Some(0),
            [] => bail!("no ImplementationGuide-{id}.json matches sushi-config id"),
            _ => bail!("multiple ImplementationGuide-{id}.json primary candidates"),
        }
    } else if implementation_guides.len() == 1 {
        Some(0)
    } else {
        bail!("multiple ImplementationGuides without a sushi-config id primary marker")
    };
    if let Some(index) = primary_index {
        let primary = implementation_guides.remove(index);
        let primary_id = primary.id.clone();
        ig = IgContext::from_ig(&primary.json);
        ig_order = ig_resource_order(&primary.json);
        ig_json = primary.json;
        // The same primary may be visible through both generated and authored
        // trees. It is one identity, not an additional IG resource.
        implementation_guides.retain(|guide| guide.id != primary_id);
    }
    // Only the selected primary guide owns index.html. Additional IG instances
    // are ordinary resources and receive their normal resource pages/data rows.
    resources.extend(implementation_guides);

    // The publisher processes resources in the ImplementationGuide's
    // `definition.resource[]` order; artifacts.json key order and the
    // structuredefinitions.json `index` both follow it. Reorder to match.
    if let Some(order) = &ig_order {
        order_resources(&mut resources, order);
    }

    // Page-fragment includes present in source (for pages.json intro/notes gating).
    let mut page_includes = std::collections::HashSet::new();
    for sub in ["input/pagecontent", "input/intro-notes", "input/includes"] {
        let dir = build_dir.join(sub);
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                if let Some(name) = e.file_name().to_str() {
                    page_includes.insert(name.to_string());
                }
            }
        }
    }

    Ok(ProducerInputs {
        resources,
        defaults,
        layouts: LayoutSource::Dir(build_dir.to_path_buf()),
        ig,
        ig_json,
        page_includes,
        page_prefix: String::new(),
    })
}

fn configured_implementation_guide_id(build_dir: &Path) -> Result<Option<String>> {
    let path = ["sushi-config.yaml", "sushi-config.yml"]
        .into_iter()
        .map(|name| build_dir.join(name))
        .find(|path| path.is_file());
    let Some(path) = path else { return Ok(None) };
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    Ok(yaml
        .get("id")
        .and_then(serde_yaml::Value::as_str)
        .map(str::to_string)
        .filter(|id| !id.trim().is_empty()))
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

/// Run the full producer: page shells + the derivable `_data` files.
pub fn produce(inputs: &ProducerInputs) -> Result<SiteProducerOutput> {
    let mut out = SiteProducerOutput::default();
    shells::emit_shells(inputs, &mut out.pages)?;
    data::emit_data(inputs, &mut out.data)?;
    Ok(out)
}
