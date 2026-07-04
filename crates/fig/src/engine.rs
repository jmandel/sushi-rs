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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use render_page::{render_page, PageProvider, SiteData};
use render_sd::context::IgContext;
use render_sd::engine::{FragmentEngine, IgFacts};

/// A resolved render root — the four input trees the page pass composes over.
#[derive(Debug, Clone)]
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
            flat,
        })
    }
}

/// Options for a render pass.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// The HierarchicalTableGenerator run uuid (quirk #1). The editor mints one
    /// per build; a native render uses a fixed deterministic value unless the
    /// caller supplies one.
    pub run_uuid: String,
    /// The template's `active-tables` param (per-IG). Default false.
    pub active_tables: bool,
    /// Route include misses through the FragmentEngine (first-include-miss).
    /// TRUE is the real editor path; FALSE reads all includes from the staged
    /// `_includes/` (pure page pass).
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
pub struct RenderedPage {
    /// Output rel path / Jekyll page.path (`en/index.html` or `index.html`).
    pub page_path: String,
    /// Final HTML.
    pub html: String,
    /// True if this source had no front matter (a verbatim static copy).
    pub is_static: bool,
}

/// The full render outcome.
pub struct RenderOutcome {
    pub pages: Vec<RenderedPage>,
    /// Fragment materializations (engine misses) across the whole pass.
    pub fragment_misses: usize,
    /// Static asset files copied (name -> byte length), see [`copy_assets`].
    pub assets_copied: usize,
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
        let Ok(t) = std::fs::read_to_string(&p) else { continue };
        let (Some(a), Some(b)) =
            (t.find("<!--ReleaseHeader-->"), t.find("<!--EndReleaseHeader-->"))
        else {
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
    let ctx = IgContext::load_with_txcache(&root.own_dir, &root.packages_dir, root.txcache_dir.as_deref());
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
    let site = SiteData::load(&root.data_dir);
    let engine = opts.engine.then(|| build_engine(root, opts));
    let provider = PageProvider::new(&site, &root.includes_dir, engine.as_ref())
        .with_engine_first(opts.engine_first)
        .with_pages_root(&root.pages_root);

    let mut inputs: Vec<PathBuf> = std::fs::read_dir(&root.input_dir)
        .with_context(|| format!("read page input dir {}", root.input_dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|f| f.is_file() && f.extension().and_then(|x| x.to_str()) == Some("html"))
        .collect();
    inputs.sort();

    // Post-Jekyll ReleaseHeader substitution: applied when the build's output/
    // reflects that later pipeline stage (us-core), a no-op otherwise (plan-net).
    let release_header = harvest_release_header(&root.own_dir);

    let mut pages = Vec::new();
    for inp in &inputs {
        let name = inp.file_name().unwrap().to_string_lossy().to_string();
        let is_dump = name.ends_with(".json.html")
            || name.ends_with(".xml.html")
            || name.ends_with(".ttl.html");
        if is_dump && !opts.include_dumps {
            continue;
        }
        let src = std::fs::read_to_string(inp)
            .with_context(|| format!("read page {}", inp.display()))?;
        let page_path = if root.flat { name.clone() } else { format!("en/{}", name) };
        let is_static = !render_page::has_front_matter(&src);
        let mut html = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            render_page(&src, &page_path, &provider)
        }))
        .map_err(|_| anyhow::anyhow!("page render panicked: {}", name))?;
        if let Some(rh) = &release_header {
            html = render_page::apply_release_header(&html, rh);
        }
        pages.push(RenderedPage { page_path, html, is_static });
    }

    let fragment_misses = *provider.miss_count.borrow();
    Ok(RenderOutcome { pages, fragment_misses, assets_copied: 0 })
}

/// Write a render outcome to `out_dir`, preserving the page.path layout
/// (`en/…` subdir when multi-language), then copy the static assets the Jekyll
/// step consumed. Returns the total files written (pages + assets).
pub fn write_site(root: &RenderRoot, out: &RenderOutcome, out_dir: &Path) -> Result<usize> {
    let mut written = 0usize;
    for page in &out.pages {
        let dest = out_dir.join(&page.page_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, &page.html)
            .with_context(|| format!("write {}", dest.display()))?;
        written += 1;
    }
    written += copy_assets(root, out_dir)?;
    Ok(written)
}

/// Copy the STATIC assets the publisher's Jekyll step consumed — everything the
/// staged page tree carries that is NOT a rendered page or a page-pass input
/// (`_data`, `_includes`, `_layouts`). This is the `assets/`, images, css/js the
/// template ships; the pages already went through the page pass.
pub fn copy_assets(root: &RenderRoot, out_dir: &Path) -> Result<usize> {
    let mut copied = 0usize;
    // The staged pages tree's non-page, non-input files are the static asset set
    // Jekyll copies verbatim (assets/, images, favicon, etc.). Page sources
    // (.html/.md at a page location) and the page-pass control dirs are skipped.
    let base = &root.pages_root;
    let mut stack = vec![base.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
            // Skip the page-pass control dirs (consumed, not copied) and Jekyll
            // internal / underscore-prefixed dirs (Jekyll excludes them from
            // output — `_data`, `_includes`, `_layouts`, `.jekyll-cache`, …).
            let top = rel.split('/').next().unwrap_or("");
            if top.starts_with('_') || top.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            // Page sources are rendered, not copied. A page source is an .html/.md
            // file at a page location (top level or under en/). Non-page static
            // files (css/js/png/…) are copied.
            let is_page_source = matches!(
                p.extension().and_then(|x| x.to_str()),
                Some("html") | Some("md")
            );
            if is_page_source {
                continue;
            }
            let dest = out_dir.join(&rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&p, &dest)?;
            copied += 1;
        }
    }
    Ok(copied)
}

/// The ImplementationGuide.version from the own resource dir (facts input).
fn ig_version(own_dir: &Path) -> String {
    let Ok(rd) = std::fs::read_dir(own_dir) else { return String::new() };
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

/// A page's rendered output plus the include/fragment read-set it consulted —
/// the [`watch`](crate::watch) dirty-cone boundary. Renders a single page with a
/// per-page tracking provider so the caller learns which fragments it pulled.
pub fn render_page_tracked(
    root: &RenderRoot,
    engine: Option<&FragmentEngine>,
    site: &SiteData,
    opts: &RenderOptions,
    input_path: &Path,
) -> Result<(String, HashMap<String, Option<String>>)> {
    let name = input_path.file_name().unwrap().to_string_lossy().to_string();
    let src = std::fs::read_to_string(input_path)?;
    let page_path = if root.flat { name.clone() } else { format!("en/{}", name) };
    let shared: std::rc::Rc<std::cell::RefCell<HashMap<String, Option<String>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(HashMap::new()));
    let provider = PageProvider::new(site, &root.includes_dir, engine)
        .with_engine_first(opts.engine_first)
        .with_pages_root(&root.pages_root)
        .with_shared_cache(shared.clone());
    let html = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        render_page(&src, &page_path, &provider)
    }))
    .map_err(|_| anyhow::anyhow!("page render panicked: {}", name))?;
    let read_set = shared.borrow().clone();
    Ok((html, read_set))
}
