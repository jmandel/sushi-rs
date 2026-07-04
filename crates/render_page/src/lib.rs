//! `render_page` — F5, the convergence phase: whole PAGES at Publisher parity.
//!
//! The publisher's `jekyll build` over `temp/pages/` is reproduced here as a
//! pure Liquid page pass (see docs/render-worklog.md F5 findings):
//!
//!   strip front-matter  →  render_liquid render  →  (done)
//!
//! There is NO Jekyll `layout:` step in the stock hl7.fhir/base template — the
//! layout is applied by an ordinary `{% include template-page.html %}` (or the
//! profile pages inline the layout-profile chrome). `markdownify` is wired to
//! render_md (kramdown). `{% include %}` bodies come from a [`PageProvider`]
//! that serves the publisher's pre-generated `_includes/*` and falls back to a
//! [`render_sd::engine::FragmentEngine`] on a MISS (first-include-miss, the
//! editor's lazy model). `site.data.*` is served from the build's `_data/*.json`
//! (which IS `site.data`).
//!
//! Gate: `output/en/*.html` (F0 builds) / `temp/pages/en/*.html` re-render
//! (cycle) byte-identical to the publisher's Jekyll output.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use render_liquid::{DataProvider, Options, Value};
use render_sd::engine::FragmentEngine;

pub mod sitedata;
pub use sitedata::SiteData;

/// The kramdown hook for `markdownify`. Uses render_md (the F1b kramdown engine)
/// with `rouge_wrappers` ON — reproducing Jekyll's markdownify, which post-
/// processes kramdown through its default rouge integration
/// (`syntax_highlighter: rouge`): inline code spans get
/// `class="language-plaintext highlighter-rouge"`, plaintext fenced blocks the
/// rouge div wrappers (real-lexer languages are a separate, deferred port).
pub fn markdownify(src: &str) -> String {
    render_md::render_with(
        src,
        &render_md::Options { rouge_wrappers: true, ..Default::default() },
    )
}

/// A page-pass data + include provider. `site.data.*`/`site.*` come from a
/// [`SiteData`] (the `_data/*.json` map); `{% include %}` bodies come from
/// `includes_dir` (the publisher's pre-generated `_includes/`), and on a MISS
/// from the optional [`FragmentEngine`] (materialize-on-first-include).
pub struct PageProvider<'a> {
    site: &'a SiteData,
    includes_dir: PathBuf,
    /// Root of the STAGED PAGES tree (`temp/pages`); `include_relative`
    /// resolves against `<pages_root>/<current page dir>/<name>` (Jekyll).
    pages_root: Option<PathBuf>,
    /// Directory (relative to `pages_root`) of the page being rendered — set
    /// by [`render_page`] per call ("" for a flat layout, "en" for en/…).
    current_page_dir: RefCell<String>,
    /// Read seam for `_includes/` lookups (FsTree natively; MemTree in wasm).
    tree: std::rc::Rc<dyn render_sd::tree::TreeSource>,
    engine: Option<&'a FragmentEngine>,
    /// When true, a registered fragment kind is produced by the FragmentEngine
    /// FIRST (before consulting `_includes/`) — the true first-include-miss path
    /// the editor uses. Proves the engine materializes byte-identical fragments
    /// (unregistered/authored `.xml`/`.md` content includes still come from
    /// `_includes/`).
    engine_first: bool,
    /// Materialized-on-miss fragment cache (first-include-miss store). An Rc so
    /// the session can share ONE map between the page include loop and the
    /// external `render_fragment` surface.
    frag_cache: std::rc::Rc<RefCell<HashMap<String, Option<String>>>>,
    /// Count of includes served from the engine (fragment materializations).
    pub miss_count: RefCell<usize>,
}

impl<'a> PageProvider<'a> {
    pub fn new(site: &'a SiteData, includes_dir: &Path, engine: Option<&'a FragmentEngine>) -> Self {
        PageProvider {
            site,
            includes_dir: includes_dir.to_path_buf(),
            tree: render_sd::tree::fs_tree(),
            engine,
            pages_root: None,
            current_page_dir: RefCell::new(String::new()),
            engine_first: false,
            frag_cache: std::rc::Rc::new(RefCell::new(HashMap::new())),
            miss_count: RefCell::new(0),
        }
    }

    /// Enable the engine-first (true first-include-miss) mode.
    pub fn with_engine_first(mut self, on: bool) -> Self {
        self.engine_first = on;
        self
    }

    /// Enable Jekyll `include_relative` resolution against the staged pages
    /// tree. Without it, include_relative falls back to `_includes/` (the
    /// engine default — pre-us-core corpora never distinguished the paths).
    pub fn with_pages_root(mut self, root: &Path) -> Self {
        self.pages_root = Some(root.to_path_buf());
        self
    }

    /// Set the directory of the page about to render (relative to
    /// pages_root). Called by [`render_page`]; interior-mutable because the
    /// provider is shared across the page loop.
    pub fn set_current_page_dir(&self, page_path: &str) {
        let dir = match page_path.rsplit_once('/') {
            Some((d, _)) => d.to_string(),
            None => String::new(),
        };
        *self.current_page_dir.borrow_mut() = dir;
    }

    /// Use a non-fs read seam (the wasm session's MemTree).
    pub fn with_tree(mut self, tree: std::rc::Rc<dyn render_sd::tree::TreeSource>) -> Self {
        self.tree = tree;
        self
    }

    /// Share (or pre-seed) the materialized-fragment cache across renders —
    /// the session-level first-include-miss store: the internal include loop
    /// and external `render_fragment` hit the SAME map.
    pub fn with_shared_cache(
        mut self,
        cache: std::rc::Rc<RefCell<HashMap<String, Option<String>>>>,
    ) -> Self {
        self.frag_cache = cache;
        self
    }

    /// Try the FragmentEngine for an include name; caches the result.
    fn try_engine(&self, name: &str) -> Option<String> {
        let eng = self.engine?;
        if let Some(hit) = self.frag_cache.borrow().get(name) {
            return hit.clone();
        }
        let stem = name.trim_end_matches(".xhtml").trim_end_matches(".html");
        let produced = FragmentEngine::split_include(stem)
            .and_then(|(ref_, kind)| eng.render_fragment(&ref_, &kind).ok());
        if produced.is_some() {
            *self.miss_count.borrow_mut() += 1;
        }
        self.frag_cache.borrow_mut().insert(name.to_string(), produced.clone());
        produced
    }

    /// Resolve an include body: `_includes/{name}` first (the publisher
    /// pre-generated set), else the FragmentEngine (parse `{name}` into
    /// `(ref_, kind)` and render). With `engine_first`, a registered fragment
    /// kind is produced by the engine before `_includes/` is consulted.
    fn resolve_include(&self, name: &str) -> Option<String> {
        if self.engine_first {
            if let Some(s) = self.try_engine(name) {
                return Some(s);
            }
        }
        // 1. pre-generated file in _includes/ (possibly under en/).
        let p = self.includes_dir.join(name);
        if let Some(s) = self.tree.read(&p) {
            return Some(s);
        }
        // 2. FragmentEngine (first-include-miss materialization).
        if !self.engine_first {
            if let Some(s) = self.try_engine(name) {
                return Some(s);
            }
        }
        None
    }
}

impl<'a> DataProvider for PageProvider<'a> {
    fn site_data(&self, path: &[&str]) -> Option<Value> {
        self.site.site_data(path)
    }
    fn site(&self, path: &[&str]) -> Option<Value> {
        self.site.site(path)
    }
    fn include_source(&self, name: &str) -> Option<String> {
        self.resolve_include(name)
    }
    fn include_source_relative(&self, name: &str) -> Option<String> {
        let root = self.pages_root.as_ref()?;
        let dir = self.current_page_dir.borrow();
        let p = if dir.is_empty() {
            root.join(name)
        } else {
            root.join(&*dir).join(name)
        };
        self.tree.read(&p)
    }
}

/// Strip Jekyll front-matter (a leading `---\n … \n---\n`). Returns the body
/// after the second `---`. Jekyll requires the file to START with `---` for a
/// page to be processed; a file without it is copied verbatim (still passed
/// here as-is). The FHIR template's page inputs all carry an empty `---\n---`.
/// Does the file start with a Jekyll YAML front-matter block? (First line is
/// exactly `---`.) Files without it are static — copied verbatim, not rendered.
pub fn has_front_matter(src: &str) -> bool {
    let s = src.strip_prefix('\u{feff}').unwrap_or(src);
    match s.find('\n') {
        Some(i) => s[..i].trim_end_matches('\r') == "---",
        None => s.trim_end_matches('\r') == "---",
    }
}

pub fn strip_front_matter(src: &str) -> &str {
    let s = src.strip_prefix('\u{feff}').unwrap_or(src);
    // Jekyll's front-matter regex: file must START with a `---` line. The block
    // ends at the next line that is exactly `---`. Everything after that line's
    // trailing newline is the page body. Handles LF and CRLF; the FHIR template
    // uses empty front-matter (`---` immediately followed by the closing `---`).
    let first_nl = match s.find('\n') {
        Some(i) => i,
        None => return src,
    };
    let first_line = s[..first_nl].trim_end_matches('\r');
    if first_line != "---" {
        return src; // no front-matter
    }
    // Scan subsequent lines for a closing `---`.
    let mut pos = first_nl + 1;
    let bytes = s.as_bytes();
    while pos <= s.len() {
        let line_end = s[pos..].find('\n').map(|i| pos + i).unwrap_or(s.len());
        let line = s[pos..line_end].trim_end_matches('\r');
        if line == "---" {
            // body begins after this line's newline (if any).
            if line_end < s.len() {
                return &s[line_end + 1..];
            }
            return "";
        }
        if line_end >= s.len() {
            break;
        }
        pos = line_end + 1;
    }
    let _ = bytes;
    src
}

/// Render one page source (the raw `temp/pages/<page_path>` file) to its final
/// HTML. `page_path` is the Jekyll-relative path (e.g. `en/toc.html`), exposed
/// to Liquid as `page.path`.
pub fn render_page(src: &str, page_path: &str, provider: &PageProvider) -> String {
    provider.set_current_page_dir(page_path);
    // Jekyll semantics: only files with YAML front-matter are Liquid-processed;
    // a file WITHOUT a leading `---` line is a static asset, copied VERBATIM
    // (verified: `searchform.html` has no front-matter and its golden is a
    // byte-for-byte copy carrying `{{title}}` unrendered). `has_front_matter`
    // decides; `strip_front_matter` returns `src` unchanged if absent, so we
    // must gate on presence explicitly.
    if !has_front_matter(src) {
        return src.to_string();
    }
    let body = strip_front_matter(src);
    // `page` global: the FHIR template reads `page.path` (and derives localPage,
    // path from it). Provide the Jekyll-relative path plus `page.name`, the
    // source filename (basename) — Jekyll's `page.name`. Several data-driven
    // include scripts key on it (e.g. `provenance-author-bullet-generator.md`
    // filters the provenance CSV by `item.Path == page.name`).
    let mut page = render_liquid::OrderedMap::new();
    page.insert("path", Value::str(page_path));
    let name = page_path.rsplit('/').next().unwrap_or(page_path);
    page.insert("name", Value::str(name));
    let globals = [("page", Value::Hash(std::rc::Rc::new(page)))];
    let opts = Options { publisher_raw_quirk: false, markdownify: Some(markdownify) };
    render_liquid::render_with(body, provider, &globals, opts)
}

/// The publisher's POST-Jekyll `ReleaseHeader` substitution: it replaces the
/// `<!--ReleaseHeader-->…<!--EndReleaseHeader-->` placeholder that Jekyll emits
/// (`Publish Box goes here`) with the generated publish-box — a STATIC per-IG
/// string (byte-identical on every page; harvested from the goldens, same
/// oracle-input pattern as the HTG run-uuid). Applied only when a build's
/// `output/` reflects this later pipeline stage (us-core does; plan-net's
/// `output/` is the pre-substitution Jekyll stage and needs no post-pass).
///
/// `replacement` is the FULL `<!--ReleaseHeader-->…<!--EndReleaseHeader-->` block.
pub fn apply_release_header(html: &str, replacement: &str) -> String {
    const OPEN: &str = "<!--ReleaseHeader-->";
    const CLOSE: &str = "<!--EndReleaseHeader-->";
    let (Some(a), Some(b)) = (html.find(OPEN), html.find(CLOSE)) else {
        return html.to_string();
    };
    let end = b + CLOSE.len();
    if a >= end {
        return html.to_string();
    }
    let mut out = String::with_capacity(html.len());
    out.push_str(&html[..a]);
    out.push_str(replacement);
    out.push_str(&html[end..]);
    out
}
