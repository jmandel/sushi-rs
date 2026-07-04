//! The Session render surface (F6): FragmentEngine + page rendering over the
//! engine's in-memory state — compiled project + mounted package bundles +
//! the mounted site tree (template statics, pagecontent, _data, _includes,
//! optional txcache).
//!
//! Virtual layout (all reads via `TreeSource`):
//!   /own/<Type>-<id>.json      — the last compile()'s resources (incl. the IG)
//!   /site/en/<page>.(md|html)  — page sources (template + staged pagecontent)
//!   /site/_includes/<name>     — template/static includes (fragment kinds are
//!                                 NOT here — they materialize on include-miss)
//!   /site/_data/<name>         — site.data files
//!   /site/txcache/...          — optional tx cache (vs-externals.json etc.)
//!   <bundle cache root>/...    — mounted packages, served by BundleSource
//!
//! One shared fragment cache per generation: the page include loop and the
//! external `render_fragment` hit the SAME map; a new compile() generation
//! drops the whole render state (structural invalidation — staleness is
//! impossible, not merely unlikely).
//!
//! Page sources are the publisher's STAGED shape: `.html` with front matter
//! (every F0 tree stages pages that way — plan-net/us-core/cycle carry no
//! `.md` under temp/pages). `.md` keys are accepted and Liquid-rendered for
//! forward-compat, but the publisher's md→staged-html step is NOT reproduced
//! here — the editor's staging layer owns that (F6 scope 3), gated against
//! the publisher's staging when it lands.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use package_store::bundle::BundleSource;
use package_store::source::PackageSource;
use render_page::{render_page, PageProvider, SiteData};
use render_sd::context::IgContext;
use render_sd::engine::{FragError, FragmentEngine, IgFacts};
use render_sd::tree::{DirEntry, MemTree, TreeSource};
use serde_json::Value;

/// TreeSource over the session state: package paths (under the bundle cache
/// root) go to the BundleSource; everything else to the MemTree.
struct SessionTree {
    mem: MemTree,
    pkg: Option<Rc<BundleSource>>,
    pkg_root: PathBuf,
}

impl TreeSource for SessionTree {
    fn read(&self, path: &Path) -> Option<String> {
        if path.starts_with(&self.pkg_root) {
            let src = self.pkg.as_ref()?;
            let bytes = src.read(path).ok()?;
            return String::from_utf8(bytes).ok();
        }
        self.mem.read(path)
    }
    fn read_bytes(&self, path: &Path) -> Option<Vec<u8>> {
        if path.starts_with(&self.pkg_root) {
            return self.pkg.as_ref()?.read(path).ok();
        }
        self.mem.read_bytes(path)
    }
    fn read_dir(&self, path: &Path) -> Option<Vec<DirEntry>> {
        if path.starts_with(&self.pkg_root) {
            let src = self.pkg.as_ref()?;
            let entries = src.read_dir(path).ok()?;
            let mut out: Vec<DirEntry> = entries
                .into_iter()
                .map(|e| (e.file_name, e.is_file))
                .collect();
            out.sort();
            return Some(out);
        }
        self.mem.read_dir(path)
    }
}

/// Options for the mounted site tree.
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteOptions {
    /// The template config's `active-tables` param (per-IG; publisher
    /// Template config.json). Default false.
    #[serde(default)]
    pub active_tables: bool,
    /// HierarchicalTableGenerator run uuid. The publisher mints one per run
    /// (documented run-context quirk); the session uses a FIXED deterministic
    /// value so re-renders are stable and the ledgers can hash outputs.
    #[serde(default)]
    pub run_uuid: Option<String>,
    /// Include resolution order. TRUE (default; the stock-template path): LIVE
    /// engine fragments shadow staged tree copies. FALSE (custom generators,
    /// e.g. cycle): the mounted `_includes` — the generator's own include
    /// design — win; the engine only serves tree misses.
    #[serde(default = "default_true")]
    pub engine_first_includes: bool,
}

fn default_true() -> bool {
    true
}

/// The per-generation render state.
pub struct RenderState {
    engine_first_includes: bool,
    pub engine: FragmentEngine,
    pub site: SiteData,
    pub tree: Rc<dyn TreeSource>,
    /// The session-shared first-include-miss store.
    pub frag_cache: Rc<RefCell<HashMap<String, Option<String>>>>,
    /// Page source names available under /site/en (stem -> source path).
    pages: Vec<(String, PathBuf)>,
}

const OWN_DIR: &str = "/own";
const SITE_DIR: &str = "/site";

/// Build the render state from session parts.
pub fn build_render_state(
    compiled: &[(PathBuf, Value)],
    bundle: Option<Rc<BundleSource>>,
    site_files: &HashMap<PathBuf, Vec<u8>>,
    options: &SiteOptions,
) -> Result<RenderState, String> {
    let mut mem = MemTree::new();
    // /own: the compiled resources, named {Type}-{id}.json (compile() already
    // produces synthetic paths shaped that way; fall back to the body).
    for (p, v) in compiled {
        let fname = p
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from)
            .or_else(|| {
                let t = v.get("resourceType")?.as_str()?;
                let id = v.get("id")?.as_str()?;
                Some(format!("{}-{}.json", t, id))
            })
            .ok_or_else(|| "compiled resource without a name".to_string())?;
        let text = serde_json::to_string(v).map_err(|e| e.to_string())?;
        mem.insert_text(Path::new(OWN_DIR).join(fname), &text);
    }
    // /site: the mounted tree, verbatim. Page sources = every .md/.html not
    // under the non-page dirs; key = output rel path (.md maps to .html), which
    // is ALSO the Jekyll page.path (`en/<name>` in multi-language layouts,
    // `<name>` in flat ones — the tree shape carries it).
    let mut pages: Vec<(String, PathBuf)> = Vec::new();
    for (p, bytes) in site_files {
        mem.insert(p.clone(), bytes.clone());
        let Ok(rest) = p.strip_prefix(SITE_DIR) else { continue };
        let rel = rest.to_string_lossy().to_string();
        if rel.starts_with("_includes/")
            || rel.starts_with("_data/")
            || rel.starts_with("_layouts/")
            || rel.starts_with("txcache/")
            || rel.starts_with("assets/")
        {
            continue;
        }
        if rel.ends_with(".md") || rel.ends_with(".html") {
            let key = if let Some(stem) = rel.strip_suffix(".md") {
                format!("{}.html", stem)
            } else {
                rel
            };
            pages.push((key, p.clone()));
        }
    }
    pages.sort();

    let has_txcache = site_files
        .keys()
        .any(|p| p.starts_with(format!("{}/txcache", SITE_DIR)));

    let pkg_root = bundle
        .as_ref()
        .map(|b| b.cache_root().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/__no_bundle__"));
    let tree: Rc<dyn TreeSource> = Rc::new(SessionTree {
        mem,
        pkg: bundle,
        pkg_root: pkg_root.clone(),
    });

    let txcache_dir = if has_txcache {
        Some(PathBuf::from(format!("{}/txcache", SITE_DIR)))
    } else {
        None
    };
    let ctx = IgContext::load_with_tree(
        tree.clone(),
        Path::new(OWN_DIR),
        &pkg_root,
        txcache_dir.as_deref(),
    );

    // Facts mirror pagecorpus's page-pass set: version + txcache. Whole-IG
    // aggregate kinds needing richer facts fire their documented loud gaps.
    let ig_version = compiled
        .iter()
        .find(|(_, v)| v.get("resourceType").and_then(|x| x.as_str()) == Some("ImplementationGuide"))
        .and_then(|(_, v)| v.get("version").and_then(|x| x.as_str()))
        .unwrap_or("")
        .to_string();
    let facts = IgFacts {
        txcache_dir,
        ig_version,
        ..Default::default()
    };
    let uuid = options
        .run_uuid
        .clone()
        .unwrap_or_else(|| "00000000-0000-4000-8000-editor000000".to_string());
    let engine = FragmentEngine::new(ctx, uuid, options.active_tables, facts);

    let site = SiteData::load_with_tree(&*tree, &Path::new(SITE_DIR).join("_data"));

    Ok(RenderState {
        engine,
        site,
        tree,
        frag_cache: Rc::new(RefCell::new(HashMap::new())),
        pages,
        engine_first_includes: options.engine_first_includes,
    })
}

impl RenderState {
    fn provider(&self) -> PageProvider<'_> {
        // engine_first: LIVE fragments (from the current compile generation)
        // shadow any staged copies in the mounted tree — the stale-fragment
        // hazard when a harvested site tree carries publisher `_includes`
        // dumps. Unregistered/authored content includes still come from the
        // tree.
        PageProvider::new(
            &self.site,
            &Path::new(SITE_DIR).join("_includes"),
            Some(&self.engine),
        )
        .with_engine_first(self.engine_first_includes)
        .with_pages_root(Path::new(SITE_DIR))
        .with_tree(self.tree.clone())
        .with_shared_cache(self.frag_cache.clone())
    }

    /// ContentApi: render a Liquid source against the session provider —
    /// includes resolve engine-first then from the mounted `/site/_includes`,
    /// `site.data` from the mounted `_data`, plus caller globals from
    /// `data_json` (a JSON object of top-level variables). markdownify is the
    /// kramdown hook (Jekyll semantics), same as the page pass.
    pub fn render_liquid_src(&self, source: &str, data_json: &str) -> Result<String, String> {
        let mut globals: Vec<(String, render_liquid::Value)> = Vec::new();
        if !data_json.trim().is_empty() {
            let v: Value = serde_json::from_str(data_json)
                .map_err(|e| format!("renderLiquid: bad data JSON: {e}"))?;
            let obj = v
                .as_object()
                .ok_or_else(|| "renderLiquid: data must be a JSON object".to_string())?;
            for (k, val) in obj {
                globals.push((k.clone(), render_page::sitedata::json_to_value(val)));
            }
        }
        let provider = self.provider();
        let refs: Vec<(&str, render_liquid::Value)> =
            globals.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        Ok(render_liquid::render_with(
            source,
            &provider,
            &refs,
            render_liquid::Options {
                publisher_raw_quirk: false,
                markdownify: Some(render_page::markdownify),
            },
        ))
    }

    /// Render one fragment through the SHARED first-include-miss store: cache
    /// key = the include name `{ref}-{kind}.xhtml`, the same key the page
    /// include loop uses.
    pub fn render_fragment(&self, ref_: &str, kind: &str) -> Result<String, FragError> {
        let key = format!("{}-{}.xhtml", ref_, kind);
        if let Some(Some(hit)) = self.frag_cache.borrow().get(&key).cloned() {
            return Ok(hit);
        }
        let out = self.engine.render_fragment(ref_, kind)?;
        self.frag_cache
            .borrow_mut()
            .insert(key, Some(out.clone()));
        Ok(out)
    }

    /// Render a page by output rel path (e.g. `en/index.html`, or `index.html`
    /// in a flat layout) — the same string is the Jekyll `page.path`. A source
    /// without front matter is a static file: returned verbatim (the
    /// publisher's Jekyll copies those unrendered).
    pub fn render_page_by_name(&self, name: &str) -> Result<String, String> {
        let (key, src_path) = self
            .pages
            .iter()
            .find(|(k, _)| k == name)
            .ok_or_else(|| format!("no page source for '{}'", name))?;
        let src = self
            .tree
            .read(src_path)
            .ok_or_else(|| format!("page source unreadable: {}", src_path.display()))?;
        // render_page applies Jekyll's front-matter gate itself (no front
        // matter -> verbatim static copy); pass the FULL source.
        let provider = self.provider();
        Ok(render_page(&src, key, &provider))
    }

    /// Output page rel paths, sorted.
    pub fn list_pages(&self) -> Vec<String> {
        self.pages.iter().map(|(k, _)| k.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use render_sd::tree::FsTree;

    const F0_PLANNET: &str = "/home/jmandel/hobby/sushi-rs-snapshot-f0-builds/plan-net";

    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.push(p);
                }
            }
        }
    }

    /// Session-vs-direct equivalence over the REAL plan-net F0 tree: the same
    /// pages rendered (a) the F5-proven native way (FsTree paths) and (b)
    /// through build_render_state's virtual layout (MemTree + BundleSource)
    /// must be byte-identical. Run with:
    ///   cargo test -q --release -p wasm_api -- --ignored session_equiv
    #[test]
    #[ignore = "needs the plan-net F0 build tree; run explicitly in release"]
    fn session_equiv_plannet() {
        let b = Path::new(F0_PLANNET);
        assert!(b.exists(), "plan-net F0 build missing: {}", b.display());

        // ---- direct (native) side: exactly the pagecorpus assembly.
        let own_dir = b.join("output");
        let packages_dir = b.join(".home/.fhir/packages");
        let txcache_dir = b.join("input-cache/txcache");
        let uuid = "00000000-0000-4000-8000-equivtest000".to_string();
        let ctx = render_sd::context::IgContext::load_with_txcache(
            &own_dir,
            &packages_dir,
            Some(&txcache_dir),
        );
        let ig_version = {
            let mut v = String::new();
            if let Some(rd) = FsTree.read_dir(&own_dir) {
                for (n, _) in rd {
                    if n.starts_with("ImplementationGuide-") && n.ends_with(".json") {
                        if let Some(t) = FsTree.read(&own_dir.join(&n)) {
                            if let Ok(j) = serde_json::from_str::<Value>(&t) {
                                if let Some(x) = j.get("version").and_then(|x| x.as_str()) {
                                    v = x.to_string();
                                }
                            }
                        }
                    }
                }
            }
            v
        };
        let facts = IgFacts {
            txcache_dir: Some(txcache_dir.clone()),
            ig_version,
            ..Default::default()
        };
        let engine = FragmentEngine::new(ctx, uuid.clone(), true, facts);
        let site = SiteData::load(&b.join("temp/pages/_data"));
        let provider = PageProvider::new(&site, &b.join("temp/pages/_includes"), Some(&engine))
            .with_engine_first(true)
            .with_pages_root(&b.join("temp/pages"));

        // ---- session side: virtual layout from the same bytes.
        // packages -> BundleSource
        let mut bundle = BundleSource::new();
        for (label, _isf) in FsTree.read_dir(&packages_dir).expect("packages dir") {
            let pdir = packages_dir.join(&label).join("package");
            if !pdir.is_dir() {
                continue;
            }
            let mut files = Vec::new();
            walk(&pdir, &mut files);
            let entries: Vec<(String, Vec<u8>)> = files
                .into_iter()
                .map(|f| {
                    let rel = f.strip_prefix(&pdir).unwrap().to_string_lossy().to_string();
                    (rel, std::fs::read(&f).unwrap())
                })
                .collect();
            bundle.mount_package(&label, entries);
        }
        // own resources -> compiled vec
        let mut compiled: Vec<(PathBuf, Value)> = Vec::new();
        for (n, isf) in FsTree.read_dir(&own_dir).expect("own dir") {
            if !isf || !n.ends_with(".json") {
                continue;
            }
            let Some(t) = FsTree.read(&own_dir.join(&n)) else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
            if v.get("resourceType").and_then(|x| x.as_str()).is_none() {
                continue;
            }
            compiled.push((PathBuf::from(n), v));
        }
        // site tree: temp/pages/** + txcache/**
        let mut site_files: HashMap<PathBuf, Vec<u8>> = HashMap::new();
        let pages_root = b.join("temp/pages");
        let mut all = Vec::new();
        walk(&pages_root, &mut all);
        for f in all {
            let rel = f.strip_prefix(&pages_root).unwrap().to_string_lossy().to_string();
            site_files.insert(
                PathBuf::from(format!("/site/{}", rel)),
                std::fs::read(&f).unwrap(),
            );
        }
        let mut tx = Vec::new();
        walk(&txcache_dir, &mut tx);
        for f in tx {
            let rel = f.strip_prefix(&txcache_dir).unwrap().to_string_lossy().to_string();
            site_files.insert(
                PathBuf::from(format!("/site/txcache/{}", rel)),
                std::fs::read(&f).unwrap(),
            );
        }
        let opts = SiteOptions {
            active_tables: true,
            run_uuid: Some(uuid),
            engine_first_includes: true,
        };
        let rs = build_render_state(&compiled, Some(Rc::new(bundle)), &site_files, &opts)
            .expect("render state");

        // ---- compare: every en/*.html page (skip source dumps, as the gate does).
        let mut pages = 0usize;
        let mut mismatches: Vec<String> = Vec::new();
        for key in rs.list_pages() {
            if !key.starts_with("en/") || !key.ends_with(".html") {
                continue;
            }
            let name = key.trim_start_matches("en/");
            if name.ends_with(".json.html")
                || name.ends_with(".xml.html")
                || name.ends_with(".ttl.html")
            {
                continue;
            }
            let src = FsTree
                .read(&pages_root.join("en").join(name))
                .expect("page source");
            let direct = render_page::render_page(&src, &key, &provider);
            let ours = rs.render_page_by_name(&key).expect("session render");
            pages += 1;
            if direct != ours {
                let dump = std::env::temp_dir().join(format!(
                    "equiv-{}",
                    key.replace('/', "_")
                ));
                let _ = std::fs::write(dump.with_extension("direct.html"), &direct);
                let _ = std::fs::write(dump.with_extension("session.html"), &ours);
                mismatches.push(key);
            }
        }
        assert!(pages > 500, "expected the full plan-net page set, got {pages}");
        assert!(
            mismatches.is_empty(),
            "session/direct divergence on {} of {} pages: {:?}",
            mismatches.len(),
            pages,
            &mismatches[..mismatches.len().min(10)]
        );

        // fragment surface: shared-cache render matches the raw engine.
        let frag = rs
            .render_fragment("StructureDefinition-plannet-Practitioner", "snapshot")
            .expect("fragment");
        assert!(frag.contains("<table"), "fragment shape");
        // second call is the cache hit — identical.
        let frag2 = rs
            .render_fragment("StructureDefinition-plannet-Practitioner", "snapshot")
            .unwrap();
        assert_eq!(frag, frag2);
    }
}
