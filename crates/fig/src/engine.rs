//! Native engine composition — the methods the engine core lacks that a
//! subcommand would otherwise have to compose itself (forbidden by the iron
//! rule). Each lives here so the Session can grow the same composition later;
//! all of it composes the SAME F5/F6 machinery the browser editor uses:
//! `render_sd::engine::FragmentEngine` + `render_page::render_page`, exactly as
//! `crates/render_page/src/bin/pagecorpus.rs` and `wasm_api::render_surface` do.
//!
//! The native render surface reads a completed build tree (a "render root"):
//!   <root>/output/            — snapshot-complete own resources (the IgContext
//!                               own_dir; also the source of static output pages)
//!   <root>/temp/pages/        — the staged Jekyll page tree (en/*.html, _data,
//!                               _includes), the page-pass inputs
//!   <root>/.home/.fhir/packages — the mounted package closure
//!   <root>/input-cache/txcache  — optional tx cache
//!
//! This is exactly the F0-build shape the page corpora gate against, so
//! `fig render` over such a root is byte-identical to the pagecorpus oracle by
//! construction.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use render_page::{
    all_compile_inputs, collect_stock_revision, render_page, stock_input_artifact,
    ArtifactObservation, FragmentEngineArtifactResolver, PageArtifactReadSet, PageProvider,
    SiteData, StockAsset, StockFragmentPolicy, StockInput, StockPage, StockPageOutcome,
    STOCK_PAGE_SOURCE_NAMESPACE, STOCK_RUNTIME_INPUT_NAMESPACE, STOCK_SITE_DATA_NAMESPACE,
    STOCK_STAGED_INCLUDE_NAMESPACE, STOCK_TEMPLATE_INCLUDE_NAMESPACE,
};
use render_sd::context::IgContext;
use render_sd::engine::{FragmentEngine, IgFacts};
use serde::Serialize;

/// A resolved render root — the four input trees the page pass composes over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderRoot {
    /// Jekyll source root (`temp/pages`).
    pub pages_root: PathBuf,
    /// The page INPUT dir (`temp/pages/en` multi-lang, or `temp/pages` flat).
    pub input_dir: PathBuf,
    /// `_data`.
    pub data_dir: PathBuf,
    /// `_includes`.
    pub includes_dir: PathBuf,
    /// own resource dir — the snapshot-complete IG outputs (`output/`).
    pub own_dir: PathBuf,
    /// package cache.
    pub packages_dir: PathBuf,
    /// optional tx cache.
    pub txcache_dir: Option<PathBuf>,
    /// Optional materialized template `includes/` dir (the driven `fig render
    /// --template` path). When set, the page pass consults it as a fallback
    /// include source after the staged `_includes/`. `None` = the frozen/staged
    /// path (staged `_includes/` already carries the template's includes).
    pub template_includes_dir: Option<PathBuf>,
    /// `true` when pages live directly under `temp/pages` (flat single-lang,
    /// page.path = `<name>`); `false` for `temp/pages/en` (page.path = `en/<name>`).
    pub flat: bool,
}

impl RenderRoot {
    /// Auto-detect the render root layout under a build directory. Handles both
    /// the multi-language (`temp/pages/en`) and flat (`temp/pages/*.html`) shapes.
    pub fn detect(build_dir: &Path) -> Result<RenderRoot> {
        let pages_root = build_dir.join("temp/pages");
        if !pages_root.is_dir() {
            bail!(
                "no staged page tree at {} — `fig render` expects a completed build \
                 (temp/pages + output + packages). Run `fig build`/the publisher stage first, \
                 or point at an F0-build root.",
                pages_root.display()
            );
        }
        let en = pages_root.join("en");
        let flat = !en.is_dir();
        let input_dir = if flat { pages_root.clone() } else { en };
        let own_dir = build_dir.join("output");
        let packages_dir = build_dir.join(".home/.fhir/packages");
        let txcache = build_dir.join("input-cache/txcache");
        Ok(RenderRoot {
            data_dir: pages_root.join("_data"),
            includes_dir: pages_root.join("_includes"),
            input_dir,
            pages_root,
            own_dir,
            packages_dir,
            txcache_dir: txcache.is_dir().then_some(txcache),
            template_includes_dir: None,
            flat,
        })
    }

    /// Point the render at a materialized template's `includes/` dir (the driven
    /// `fig render --template` path). `template_dir` is the materialized
    /// `template/` root; its `includes/` subdir becomes the fallback include
    /// source. Returns self for chaining off [`RenderRoot::detect`].
    pub fn with_template_dir(mut self, template_dir: &Path) -> Self {
        self.template_includes_dir = Some(template_dir.join("includes"));
        self
    }
}

/// Options for a render pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderOptions {
    /// The HierarchicalTableGenerator run uuid (quirk #1). The editor mints one
    /// per build; a native render uses a fixed deterministic value unless the
    /// caller supplies one.
    pub run_uuid: String,
    /// The template's `active-tables` param (per-IG). Default false.
    pub active_tables: bool,
    /// Permit registered generated includes to cross the typed fragment resolver
    /// after ordinary file lookup. FALSE reads only staged `_includes/` (the pure
    /// page pass).
    pub engine: bool,
    /// Engine-FIRST include resolution (live fragments shadow staged copies).
    pub engine_first: bool,
    /// Include the `.json.html`/`.xml.html`/`.ttl.html` payload-dump pages.
    pub include_dumps: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            run_uuid: "00000000-0000-4000-8000-fig000000000".to_string(),
            active_tables: false,
            engine: true,
            engine_first: false,
            include_dumps: false,
        }
    }
}

/// A single rendered page result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedPage {
    /// Output rel path / Jekyll page.path (`en/index.html` or `index.html`).
    pub page_path: String,
    /// Final HTML.
    pub html: String,
    /// True if this source had no front matter (a verbatim static copy).
    pub is_static: bool,
    /// Exact typed inputs and generated artifacts used by this page.
    pub reads: PageArtifactReadSet,
}

/// One non-page file in the exact assembled output namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedAsset {
    pub output_path: String,
    pub bytes: Vec<u8>,
}

/// The full render outcome.
#[derive(Debug)]
pub struct RenderOutcome {
    pub pages: Vec<RenderedPage>,
    /// Static output files captured at render time. `write_site` writes these
    /// exact bytes instead of rescanning a mutable input tree.
    pub assets: Vec<RenderedAsset>,
    /// Fragment materializations (engine misses) across the whole pass.
    pub fragment_misses: usize,
    /// Static asset files copied (name -> byte length), see [`copy_assets`].
    pub assets_copied: usize,
    /// Present only when [`render_site_for_revision`] bound this capture to an
    /// explicit predecessor, root, options, and complete output inventory. The
    /// private field prevents callers from fabricating a promotable outcome.
    revision_capture: Option<RevisionCapture>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RevisionCapture {
    predecessor: site_build::BuildId,
    root: RenderRoot,
    options: RenderOptions,
    outcome_seal: site_build::Sha256Digest,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RevisionOutcomeSeal {
    pages: Vec<RevisionPageSeal>,
    assets: Vec<RevisionAssetSeal>,
    fragment_misses: usize,
    assets_copied: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RevisionPageSeal {
    page_path: String,
    html: site_build::ContentRef,
    is_static: bool,
    requested: std::collections::BTreeSet<site_build::ArtifactKey>,
    read: std::collections::BTreeSet<site_build::ArtifactKey>,
    input_reads: std::collections::BTreeSet<site_build::ArtifactKey>,
    input_objects: Vec<(site_build::ArtifactKey, Vec<site_build::ContentRef>)>,
    observations: Vec<(site_build::ArtifactKey, RevisionObservationSeal)>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RevisionAssetSeal {
    output_path: String,
    content: site_build::ContentRef,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum RevisionObservationSeal {
    Ready { content: site_build::ContentRef },
    NotReady { state: site_build::ArtifactState },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CapturedPageSource {
    source_path: PathBuf,
    page_path: String,
    source: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CapturedPublicTree {
    pages: Vec<CapturedPageSource>,
    assets: Vec<RenderedAsset>,
}

/// Harvest the STATIC per-IG `<!--ReleaseHeader-->…<!--EndReleaseHeader-->`
/// block from the build's own output (byte-identical across pages). Returns None
/// when the tree still carries the pre-substitution Jekyll placeholder (no
/// post-pass needed — plan-net). This is the publisher's POST-Jekyll
/// ReleaseHeader substitution stage (us-core's `output/` reflects it), the same
/// harvest the pagecorpus gate performs.
pub fn harvest_release_header(golden_dir: &Path) -> Option<String> {
    let rd = std::fs::read_dir(golden_dir).ok()?;
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("html") {
            continue;
        }
        let Ok(t) = std::fs::read_to_string(&p) else {
            continue;
        };
        let (Some(a), Some(b)) = (
            t.find("<!--ReleaseHeader-->"),
            t.find("<!--EndReleaseHeader-->"),
        ) else {
            continue;
        };
        let end = b + "<!--EndReleaseHeader-->".len();
        if a >= end {
            continue;
        }
        let block = &t[a..end];
        if block.contains("Publish Box goes here") {
            return None; // pre-substitution stage — no post-pass
        }
        return Some(block.to_string());
    }
    None
}

/// Build the FragmentEngine for a render root (the pagecorpus `build_engine`).
pub fn build_engine(root: &RenderRoot, opts: &RenderOptions) -> FragmentEngine {
    let ctx = IgContext::load_with_txcache(
        &root.own_dir,
        &root.packages_dir,
        root.txcache_dir.as_deref(),
    );
    let facts = IgFacts {
        txcache_dir: root.txcache_dir.clone(),
        ig_version: ig_version(&root.own_dir),
        ..Default::default()
    };
    FragmentEngine::new(ctx, opts.run_uuid.clone(), opts.active_tables, facts)
}

/// Render EVERY page in the root. This is the composition `fig render` calls —
/// the exact pagecorpus assembly (SiteData + FragmentEngine + PageProvider +
/// render_page per page), so byte-identical to that gate by construction.
pub fn render_site(root: &RenderRoot, opts: &RenderOptions) -> Result<RenderOutcome> {
    let captured_tree = capture_public_tree(root)?;
    let site = SiteData::load_strict(&root.data_dir)?;
    let engine = opts.engine.then(|| build_engine(root, opts));
    let mut provider = PageProvider::new(&site, &root.includes_dir)
        .with_engine_first(opts.engine_first)
        .with_pages_root(&root.pages_root);
    if let Some(engine) = engine.as_ref() {
        provider = provider.with_artifact_resolver(FragmentEngineArtifactResolver::new(engine));
    }
    if let Some(tinc) = &root.template_includes_dir {
        provider = provider.with_template_includes(tinc);
    }

    // Post-Jekyll ReleaseHeader substitution: applied when the build's output/
    // reflects that later pipeline stage (us-core), a no-op otherwise (plan-net).
    let release_header = harvest_release_header(&root.own_dir);

    let mut pages = Vec::new();
    for input in &captured_tree.pages {
        let name = input
            .source_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                anyhow::anyhow!("page path is not UTF-8: {}", input.source_path.display())
            })?
            .to_string();
        let is_dump = name.ends_with(".json.html")
            || name.ends_with(".xml.html")
            || name.ends_with(".ttl.html");
        if is_dump && !opts.include_dumps {
            continue;
        }
        let src = &input.source;
        let page_path = input.page_path.clone();
        let is_static = !render_page::has_front_matter(src);
        let mut html = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            render_page(src, &page_path, &provider)
        }))
        .map_err(|_| anyhow::anyhow!("page render panicked: {}", name))?;
        let mut reads = provider.page_artifact_reads();
        if let Some(rh) = &release_header {
            let replaced = render_page::apply_release_header(&html, rh);
            if replaced != html {
                reads.add_input_object(
                    stock_input_artifact(STOCK_RUNTIME_INPUT_NAMESPACE, "release-header.html"),
                    rh.as_bytes(),
                );
            }
            html = replaced;
        }
        pages.push(RenderedPage {
            page_path,
            html,
            is_static,
            reads,
        });
    }

    let fragment_misses = *provider.miss_count.borrow();
    // A second complete capture makes additions/removals/content changes during
    // the render fail closed. A transient A→B→A mutation cannot corrupt the
    // revision because rendering and publication use only the first captured
    // bytes.
    if capture_public_tree(root)? != captured_tree {
        bail!("public staged page/asset tree changed while rendering");
    }
    let assets = captured_tree.assets;
    let assets_copied = assets.len();
    Ok(RenderOutcome {
        pages,
        assets,
        fragment_misses,
        assets_copied,
        revision_capture: None,
    })
}

/// Render and bind the immutable capture to the explicit predecessor that may
/// later be passed to [`collect_site_build_revision`]. Ordinary `render_site`
/// remains useful for direct writes but cannot be promoted accidentally.
pub fn render_site_for_revision(
    predecessor: &site_build::SiteBuild,
    root: &RenderRoot,
    opts: &RenderOptions,
) -> Result<RenderOutcome> {
    if predecessor.render_target().mode != site_build::RenderMode::NativeTemplate {
        bail!("stock revision capture requires a native-template predecessor");
    }
    let mut outcome = render_site(root, opts)?;
    let page_paths = outcome
        .pages
        .iter()
        .map(|page| page.page_path.clone())
        .collect::<Vec<_>>();
    let asset_paths = outcome
        .assets
        .iter()
        .map(|asset| asset.output_path.clone())
        .collect::<Vec<_>>();
    assert_unique_inventory("page", &page_paths)?;
    assert_unique_inventory("asset", &asset_paths)?;
    let outcome_seal = seal_revision_outcome(&outcome)?;
    outcome.revision_capture = Some(RevisionCapture {
        predecessor: predecessor.build_id().clone(),
        root: root.clone(),
        options: opts.clone(),
        outcome_seal,
    });
    Ok(outcome)
}

fn seal_revision_outcome(outcome: &RenderOutcome) -> Result<site_build::Sha256Digest> {
    let pages = outcome
        .pages
        .iter()
        .map(|page| {
            let input_objects = page
                .reads
                .input_objects()
                .iter()
                .map(|(key, values)| {
                    (
                        key.clone(),
                        values
                            .iter()
                            .map(|bytes| site_build::ContentRef::of_bytes(bytes, None::<String>))
                            .collect(),
                    )
                })
                .collect();
            let observations = page
                .reads
                .observations()
                .iter()
                .map(|(key, observation)| {
                    let sealed = match observation {
                        ArtifactObservation::Ready { bytes } => RevisionObservationSeal::Ready {
                            content: site_build::ContentRef::of_bytes(bytes, Some("text/html")),
                        },
                        ArtifactObservation::NotReady { error } => {
                            RevisionObservationSeal::NotReady {
                                state: error.artifact_state(),
                            }
                        }
                    };
                    (key.clone(), sealed)
                })
                .collect();
            RevisionPageSeal {
                page_path: page.page_path.clone(),
                html: site_build::ContentRef::of_bytes(page.html.as_bytes(), Some("text/html")),
                is_static: page.is_static,
                requested: page.reads.requested().clone(),
                read: page.reads.read().clone(),
                input_reads: page.reads.input_reads().clone(),
                input_objects,
                observations,
            }
        })
        .collect();
    let assets = outcome
        .assets
        .iter()
        .map(|asset| RevisionAssetSeal {
            output_path: asset.output_path.clone(),
            content: site_build::ContentRef::of_bytes(&asset.bytes, None::<String>),
        })
        .collect();
    Ok(site_build::sha256_canonical(&RevisionOutcomeSeal {
        pages,
        assets,
        fragment_misses: outcome.fragment_misses,
        assets_copied: outcome.assets_copied,
    })?)
}

fn assert_unique_inventory(kind: &str, paths: &[String]) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for path in paths {
        if !seen.insert(path) {
            bail!("stock {kind} inventory repeats {path}");
        }
    }
    Ok(())
}

/// Write a render outcome to `out_dir`, preserving the page.path layout
/// (`en/…` subdir when multi-language), then copy the static assets the Jekyll
/// step consumed. Returns the total files written (pages + assets).
pub fn write_site(_root: &RenderRoot, out: &RenderOutcome, out_dir: &Path) -> Result<usize> {
    let mut written = 0usize;
    for page in &out.pages {
        let dest = out_dir.join(&page.page_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, &page.html).with_context(|| format!("write {}", dest.display()))?;
        written += 1;
    }
    for asset in &out.assets {
        let dest = out_dir.join(&asset.output_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, &asset.bytes).with_context(|| format!("write {}", dest.display()))?;
        written += 1;
    }
    Ok(written)
}

/// Copy the STATIC assets the publisher's Jekyll step consumed — everything the
/// staged page tree carries that is NOT a rendered page or a page-pass input
/// (`_data`, `_includes`, `_layouts`). This is the `assets/`, images, css/js the
/// template ships; the pages already went through the page pass.
pub fn copy_assets(root: &RenderRoot, out_dir: &Path) -> Result<usize> {
    let assets = advertised_assets(root)?;
    for asset in &assets {
        let dest = out_dir.join(&asset.output_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, &asset.bytes)?;
    }
    Ok(assets.len())
}

/// Capture the complete static output set using the same assembly policy as
/// `write_site`. The returned paths are sorted and bytes are immutable inside
/// the render outcome, closing the render/manifest TOCTOU window.
pub fn advertised_assets(root: &RenderRoot) -> Result<Vec<RenderedAsset>> {
    Ok(capture_public_tree(root)?.assets)
}

/// Capture and partition every public staged-tree file exactly once. Control
/// directories are renderer inputs, not outputs. HTML is rendered (front-matter
/// or verbatim); Markdown page sources are rejected until their full Jekyll
/// page semantics are implemented, rather than silently omitted.
fn capture_public_tree(root: &RenderRoot) -> Result<CapturedPublicTree> {
    let mut pages = Vec::new();
    let mut assets = Vec::new();
    let base = &root.pages_root;
    let mut stack = vec![base.clone()];
    while let Some(dir) = stack.pop() {
        let rd = std::fs::read_dir(&dir)
            .with_context(|| format!("read staged output directory {}", dir.display()))?;
        let mut entries = rd
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("enumerate staged output directory {}", dir.display()))?;
        entries.sort_by_key(|entry| entry.file_name());
        for e in entries {
            let p = e.path();
            let file_type = e
                .file_type()
                .with_context(|| format!("inspect staged output {}", p.display()))?;
            if file_type.is_symlink() {
                bail!(
                    "staged public tree may not contain symlinks: {}",
                    p.display()
                );
            }
            let rel_path = p.strip_prefix(base).expect("walk remains under pages_root");
            let rel = relative_path(rel_path)?;
            // Skip the page-pass control dirs (consumed, not copied) and Jekyll
            // internal / underscore-prefixed dirs (Jekyll excludes them from
            // output — `_data`, `_includes`, `_layouts`, `.jekyll-cache`, …).
            let top = rel.split('/').next().unwrap_or("");
            if top.starts_with('_') || top.starts_with('.') {
                continue;
            }
            if file_type.is_dir() {
                stack.push(p);
                continue;
            }
            if !file_type.is_file() {
                bail!(
                    "staged public tree member is not a regular file: {}",
                    p.display()
                );
            }
            let bytes = std::fs::read(&p)
                .with_context(|| format!("capture staged output {}", p.display()))?;
            match p.extension().and_then(|value| value.to_str()) {
                Some("html") => pages.push(CapturedPageSource {
                    source_path: p,
                    page_path: rel,
                    source: String::from_utf8(bytes).map_err(|error| {
                        anyhow::anyhow!("HTML page source is not UTF-8: {error}")
                    })?,
                }),
                Some("md" | "markdown") => bail!(
                    "Markdown staged page source is not yet supported by fig render: {}",
                    p.display()
                ),
                _ => assets.push(RenderedAsset {
                    output_path: rel,
                    bytes,
                }),
            }
        }
    }
    pages.sort_by(|left, right| left.page_path.as_bytes().cmp(right.page_path.as_bytes()));
    assets.sort_by(|left, right| {
        left.output_path
            .as_bytes()
            .cmp(right.output_path.as_bytes())
    });
    Ok(CapturedPublicTree { pages, assets })
}

fn relative_path(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        let std::path::Component::Normal(value) = component else {
            bail!("staged output path is not normalized: {}", path.display());
        };
        parts.push(value.to_str().ok_or_else(|| {
            anyhow::anyhow!("staged output path is not UTF-8: {}", path.display())
        })?);
    }
    Ok(parts.join("/"))
}

/// Promote an already-captured native render outcome into a complete immutable
/// SiteBuild revision. The predecessor is explicit; this function has no
/// process-global or "last session" identity.
pub fn collect_site_build_revision(
    predecessor: &site_build::SiteBuild,
    root: &RenderRoot,
    outcome: &RenderOutcome,
) -> Result<site_build::SiteBuildSuccessor> {
    let capture = outcome.revision_capture.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "render outcome is not predecessor-bound; use render_site_for_revision before promotion"
        )
    })?;
    if capture.predecessor != *predecessor.build_id() {
        bail!(
            "render outcome belongs to predecessor {}, not {}",
            capture.predecessor,
            predecessor.build_id()
        );
    }
    if &capture.root != root {
        bail!("render outcome was captured from a different RenderRoot");
    }
    if seal_revision_outcome(outcome)? != capture.outcome_seal {
        bail!("render outcome payload changed after its predecessor-bound capture");
    }
    let producer = site_build::ProducerRef::new("fig.stock-template", env!("CARGO_PKG_VERSION"));
    let render_attributes = std::collections::BTreeMap::from([
        (
            "activeTables".into(),
            capture.options.active_tables.to_string(),
        ),
        ("engine".into(), capture.options.engine.to_string()),
        (
            "engineFirst".into(),
            capture.options.engine_first.to_string(),
        ),
        (
            "includeDumps".into(),
            capture.options.include_dumps.to_string(),
        ),
        ("runUuid".into(), capture.options.run_uuid.clone()),
    ]);
    let provenance = |recipe: &str| site_build::ArtifactProvenance {
        producer: producer.clone(),
        recipe: recipe.to_string(),
        attributes: render_attributes.clone(),
    };
    let semantic_reads = all_compile_inputs(predecessor);

    // Resolve every non-generated key observed by pages to the exact captured
    // bytes. Only actual reads enter the manifest; an unused mounted template
    // file is not a hidden dependency.
    let mut captured_inputs: std::collections::BTreeMap<site_build::ArtifactKey, Vec<u8>> =
        std::collections::BTreeMap::new();
    for page in &outcome.pages {
        for (key, values) in page.reads.input_objects() {
            if values.len() != 1 {
                bail!("stock input {key:?} changed while one page was rendering");
            }
            let bytes = values.iter().next().expect("one captured value");
            if let Some(existing) = captured_inputs.get(key) {
                if existing != bytes {
                    bail!("stock input {key:?} changed between rendered pages");
                }
            } else {
                captured_inputs.insert(key.clone(), bytes.clone());
            }
        }
        for key in page.reads.input_reads() {
            if !page.reads.input_objects().contains_key(key) {
                bail!("stock input {key:?} was read without captured bytes");
            }
        }
    }
    let mut inputs = Vec::new();
    for (key, bytes) in captured_inputs {
        let site_build::ArtifactKey::Data { namespace, name } = &key else {
            bail!("stock page recorded non-Data input key {key:?}");
        };
        let media_type = match namespace.as_str() {
            STOCK_PAGE_SOURCE_NAMESPACE
            | STOCK_SITE_DATA_NAMESPACE
            | STOCK_STAGED_INCLUDE_NAMESPACE
            | STOCK_TEMPLATE_INCLUDE_NAMESPACE => media_type_for(name),
            STOCK_RUNTIME_INPUT_NAMESPACE if name == "release-header.html" => {
                "text/html".to_string()
            }
            _ => bail!("unknown stock input namespace {namespace} for {name}"),
        };
        inputs.push(StockInput {
            key,
            bytes,
            media_type,
            provenance: provenance("capture-page-input"),
            reads: semantic_reads.clone(),
        });
    }

    let pages = outcome
        .pages
        .iter()
        .map(|page| {
            Ok(StockPage {
                path: site_build::SourcePath::parse(page.page_path.clone())?,
                outcome: StockPageOutcome::Ready {
                    bytes: page.html.as_bytes().to_vec(),
                    reads: page.reads.clone(),
                },
                provenance: provenance("render-page"),
            })
        })
        .collect::<std::result::Result<Vec<_>, site_build::SourcePathError>>()?;

    let assets = outcome
        .assets
        .iter()
        .map(|asset| {
            Ok(StockAsset {
                path: site_build::SourcePath::parse(asset.output_path.clone())?,
                bytes: asset.bytes.clone(),
                media_type: media_type_for(&asset.output_path),
                provenance: provenance("assemble-static-asset"),
                reads: semantic_reads.clone(),
            })
        })
        .collect::<std::result::Result<Vec<_>, site_build::SourcePathError>>()?;

    collect_stock_revision(
        predecessor,
        inputs,
        pages,
        assets,
        StockFragmentPolicy {
            provenance: provenance("render-publisher-fragment"),
            reads: semantic_reads,
        },
    )
    .map_err(anyhow::Error::from)
}

fn media_type_for(path: &str) -> String {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("html" | "xhtml") => "text/html",
        Some("md") => "text/markdown",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("yaml" | "yml") => "application/yaml",
        Some("csv") => "text/csv",
        Some("css") => "text/css",
        Some("js") => "text/javascript",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// The ImplementationGuide.version from the own resource dir (facts input).
fn ig_version(own_dir: &Path) -> String {
    let Ok(rd) = std::fs::read_dir(own_dir) else {
        return String::new();
    };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with("ImplementationGuide-") && n.ends_with(".json") {
            if let Ok(t) = std::fs::read_to_string(e.path()) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                    if let Some(ver) = v.get("version").and_then(|x| x.as_str()) {
                        return ver.to_string();
                    }
                }
            }
        }
    }
    String::new()
}

/// Render ONE fragment through a fresh engine over a render root (the `fig
/// fragment` face + the `-o` files escape hatch use this).
pub fn render_one_fragment(
    root: &RenderRoot,
    opts: &RenderOptions,
    ref_: &str,
    kind: &str,
) -> Result<String> {
    let engine = build_engine(root, opts);
    engine
        .render_fragment(ref_, kind)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Materialize the fragment files for a set of `(ref, kind)` pairs into
/// `out_dir` as `{ref}-{kind}.xhtml` — the publisher's `_includes/` contract
/// (the `-o` escape hatch, §3b). Returns the emitted filenames.
pub fn emit_fragment_files(
    root: &RenderRoot,
    opts: &RenderOptions,
    pairs: &[(String, String)],
    out_dir: &Path,
) -> Result<Vec<String>> {
    std::fs::create_dir_all(out_dir)?;
    let engine = build_engine(root, opts);
    let mut emitted = Vec::new();
    for (ref_, kind) in pairs {
        let body = engine
            .render_fragment(ref_, kind)
            .map_err(|e| anyhow::anyhow!("{ref_}-{kind}: {e}"))?;
        let fname = format!("{ref_}-{kind}.xhtml");
        std::fs::write(out_dir.join(&fname), &body)?;
        emitted.push(fname);
    }
    Ok(emitted)
}

/// Discover the `(ref, kind)` fragment pairs implied by a render root: for the
/// `-o` escape hatch with no explicit list, we enumerate the own SD resources ×
/// a caller-supplied kind set. Kinds default handled by the caller.
pub fn own_structure_definitions(root: &RenderRoot) -> Result<Vec<String>> {
    let mut refs = Vec::new();
    let rd = std::fs::read_dir(&root.own_dir)
        .with_context(|| format!("read own dir {}", root.own_dir.display()))?;
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with("StructureDefinition-") && n.ends_with(".json") {
            refs.push(n.trim_end_matches(".json").to_string());
        }
    }
    refs.sort();
    Ok(refs)
}

/// A page's rendered output plus its typed artifact request/read sets — the
/// [`watch`](crate::watch) dirty-cone boundary. Renders a single page with a
/// per-page tracking provider so the caller learns which artifacts it requested
/// and which requests resolved successfully.
pub fn render_page_tracked(
    root: &RenderRoot,
    engine: Option<&FragmentEngine>,
    site: &SiteData,
    opts: &RenderOptions,
    input_path: &Path,
) -> Result<(String, PageArtifactReadSet)> {
    let name = input_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let src = std::fs::read_to_string(input_path)?;
    let page_path = if root.flat {
        name.clone()
    } else {
        format!("en/{}", name)
    };
    let mut provider = PageProvider::new(site, &root.includes_dir)
        .with_engine_first(opts.engine_first)
        .with_pages_root(&root.pages_root);
    if let Some(engine) = engine {
        provider = provider.with_artifact_resolver(FragmentEngineArtifactResolver::new(engine));
    }
    if let Some(tinc) = &root.template_includes_dir {
        provider = provider.with_template_includes(tinc);
    }
    let html = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        render_page(&src, &page_path, &provider)
    }))
    .map_err(|_| anyhow::anyhow!("page render panicked: {}", name))?;
    let read_set = provider.page_artifact_reads();
    Ok((html, read_set))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use site_build::{
        ArtifactCatalog, PackageLock, ProducerRef, ProjectRevision, RenderMode, RenderPlan,
        RenderTarget, SiteBuild, SourceManifest,
    };

    use super::*;

    fn root(temp: &tempfile::TempDir) -> RenderRoot {
        let pages_root = temp.path().join("temp/pages");
        for directory in [
            pages_root.join("_data"),
            pages_root.join("_includes"),
            temp.path().join("output"),
            temp.path().join("packages"),
        ] {
            std::fs::create_dir_all(directory).unwrap();
        }
        RenderRoot {
            input_dir: pages_root.clone(),
            data_dir: pages_root.join("_data"),
            includes_dir: pages_root.join("_includes"),
            pages_root,
            own_dir: temp.path().join("output"),
            packages_dir: temp.path().join("packages"),
            txcache_dir: None,
            template_includes_dir: None,
            flat: true,
        }
    }

    fn predecessor_build(project_id: &str) -> SiteBuild {
        SiteBuild::new(
            ProjectRevision {
                project_id: project_id.into(),
                revision: "fixture".into(),
                sources: SourceManifest::from_entries(Vec::new()).unwrap(),
            },
            PackageLock::default(),
            RenderTarget {
                renderer: ProducerRef::new("stock-template", "1"),
                mode: RenderMode::NativeTemplate,
                fhir_version: "4.0.1".into(),
                template: None,
                parameters: BTreeMap::new(),
            },
            RenderPlan::default(),
            ArtifactCatalog::from_records(Vec::new()).unwrap(),
            BTreeSet::new(),
        )
        .unwrap()
    }

    fn options() -> RenderOptions {
        RenderOptions {
            engine: false,
            ..Default::default()
        }
    }

    #[test]
    fn public_tree_capture_is_recursive_complete_and_rejects_markdown() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        std::fs::create_dir_all(root.pages_root.join("nested")).unwrap();
        std::fs::create_dir_all(root.pages_root.join("images")).unwrap();
        std::fs::write(root.pages_root.join("index.html"), b"<p>home</p>").unwrap();
        std::fs::write(root.pages_root.join("nested/detail.html"), b"<p>detail</p>").unwrap();
        std::fs::write(root.pages_root.join("images/icon.png"), b"png").unwrap();

        let outcome = render_site(&root, &options()).unwrap();
        assert_eq!(
            outcome
                .pages
                .iter()
                .map(|page| page.page_path.as_str())
                .collect::<Vec<_>>(),
            vec!["index.html", "nested/detail.html"]
        );
        assert_eq!(outcome.assets[0].output_path, "images/icon.png");

        std::fs::write(root.pages_root.join("unsupported.md"), b"# Markdown").unwrap();
        assert!(render_site(&root, &options())
            .unwrap_err()
            .to_string()
            .contains("Markdown staged page source"));
    }

    #[test]
    fn revision_promotion_requires_the_bound_predecessor_root_and_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        std::fs::write(root.pages_root.join("index.html"), b"<p>home</p>").unwrap();
        std::fs::write(root.pages_root.join("site.css"), b"body{}").unwrap();
        let predecessor = predecessor_build("fixture.ig");

        let mut outcome = render_site_for_revision(&predecessor, &root, &options()).unwrap();
        let successor = collect_site_build_revision(&predecessor, &root, &outcome).unwrap();
        successor.site_build().clone().close().unwrap();

        let other = predecessor_build("other.ig");
        assert!(collect_site_build_revision(&other, &root, &outcome)
            .unwrap_err()
            .to_string()
            .contains("belongs to predecessor"));

        let mut other_root = root.clone();
        other_root.input_dir = temp.path().join("other-pages");
        assert!(
            collect_site_build_revision(&predecessor, &other_root, &outcome)
                .unwrap_err()
                .to_string()
                .contains("different RenderRoot")
        );

        outcome.pages[0].html.push_str("changed");
        assert!(collect_site_build_revision(&predecessor, &root, &outcome)
            .unwrap_err()
            .to_string()
            .contains("payload changed"));

        let mut changed_reads = render_site_for_revision(&predecessor, &root, &options()).unwrap();
        changed_reads.pages[0].reads = PageArtifactReadSet::default();
        assert!(
            collect_site_build_revision(&predecessor, &root, &changed_reads)
                .unwrap_err()
                .to_string()
                .contains("payload changed")
        );

        let mut changed_asset = render_site_for_revision(&predecessor, &root, &options()).unwrap();
        changed_asset.assets[0].bytes.push(b'!');
        assert!(
            collect_site_build_revision(&predecessor, &root, &changed_asset)
                .unwrap_err()
                .to_string()
                .contains("payload changed")
        );

        let unbound = render_site(&root, &options()).unwrap();
        assert!(collect_site_build_revision(&predecessor, &root, &unbound)
            .unwrap_err()
            .to_string()
            .contains("not predecessor-bound"));
    }
}
