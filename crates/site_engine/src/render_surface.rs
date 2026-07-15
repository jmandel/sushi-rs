//! The Session render surface (F6) has two deliberately separate lifetimes:
//! [`RenderSemantics`] is the semantic FHIR core (compiled resources, packages,
//! optional txcache, and fragment options), while [`RenderState`] is the cheap
//! per-site projection (page sources, layouts/includes, and `site.data`). A
//! page/template overlay rebuilds only `RenderState`; a semantic compile input
//! or output, package, txcache, or semantic-option change rebuilds both.
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
//! One shared fragment cache per site generation: the page include loop and the
//! external `render_fragment` hit the SAME map. The FragmentEngine itself can
//! span site generations because its TreeSource intentionally contains only
//! semantic inputs (`/own`, package files, and `/site/txcache`) — never mutable
//! page/template bytes.
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

#[cfg(test)]
use package_store::bundle::BundleSource;
use package_store::source::PackageSource;
#[cfg(test)]
use render_page::{legacy_include_to_artifact_key, ArtifactCacheEntry};
use render_page::{
    render_page, FragmentEngineArtifactResolver, PageArtifactReadSet, PageProvider,
    SharedArtifactCache, SiteData,
};
use render_sd::context::{IgContext, ResourceIdentity};
#[cfg(test)]
use render_sd::engine::FragError;
use render_sd::engine::{FragmentEngine, IgFacts};
use render_sd::tree::{DirEntry, MemTree, TreeSource};
use serde_json::Value;

use crate::PackageView;

/// TreeSource over the session state: package paths (under the bundle cache
/// root) go to the BundleSource; everything else to the MemTree.
struct SessionTree {
    mem: MemTree,
    pkg: Option<PackageView>,
    pkg_root: PathBuf,
    base: Option<Rc<dyn TreeSource>>,
}

/// Immutable per-generation site overlay. Publisher preparation already owns
/// the exact file map; retaining it by `Rc` avoids copying every template,
/// asset, page, include, and data byte into a second MemTree on prose edits.
struct SiteMapTree {
    files: Rc<HashMap<PathBuf, Vec<u8>>>,
    base: Rc<dyn TreeSource>,
    pkg_root: PathBuf,
}

impl TreeSource for SiteMapTree {
    fn read(&self, path: &Path) -> Option<String> {
        self.files
            .get(path)
            .and_then(|bytes| String::from_utf8(bytes.clone()).ok())
            .or_else(|| self.base.read(path))
    }

    fn read_bytes(&self, path: &Path) -> Option<Vec<u8>> {
        self.files
            .get(path)
            .cloned()
            .or_else(|| self.base.read_bytes(path))
    }

    fn read_dir(&self, path: &Path) -> Option<Vec<DirEntry>> {
        if path.starts_with(&self.pkg_root) {
            return self.base.read_dir(path);
        }
        let inherited = self.base.read_dir(path);
        let mut merged = std::collections::BTreeMap::new();
        for (name, is_file) in inherited.into_iter().flatten() {
            merged.insert(name, is_file);
        }
        let mut found_local = false;
        for candidate in self.files.keys() {
            let Ok(rest) = candidate.strip_prefix(path) else {
                continue;
            };
            let mut components = rest.components();
            let Some(first) = components.next() else {
                continue;
            };
            found_local = true;
            let name = first.as_os_str().to_string_lossy().into_owned();
            let is_file = components.next().is_none();
            merged
                .entry(name)
                .and_modify(|existing| *existing = *existing && is_file)
                .or_insert(is_file);
        }
        if merged.is_empty() && !found_local {
            None
        } else {
            Some(merged.into_iter().collect())
        }
    }
}

impl TreeSource for SessionTree {
    fn read(&self, path: &Path) -> Option<String> {
        if path.starts_with(&self.pkg_root) {
            if let Some(src) = &self.pkg {
                let bytes = src.read(path).ok()?;
                return String::from_utf8(bytes).ok();
            }
            return self.base.as_ref()?.read(path);
        }
        self.mem
            .read(path)
            .or_else(|| self.base.as_ref()?.read(path))
    }
    fn read_bytes(&self, path: &Path) -> Option<Vec<u8>> {
        if path.starts_with(&self.pkg_root) {
            return match &self.pkg {
                Some(src) => src.read(path).ok(),
                None => self.base.as_ref()?.read_bytes(path),
            };
        }
        self.mem
            .read_bytes(path)
            .or_else(|| self.base.as_ref()?.read_bytes(path))
    }
    fn read_dir(&self, path: &Path) -> Option<Vec<DirEntry>> {
        if path.starts_with(&self.pkg_root) {
            return match &self.pkg {
                Some(src) => {
                    let entries = src.read_dir(path).ok()?;
                    let mut out: Vec<DirEntry> = entries
                        .into_iter()
                        .map(|e| (e.file_name, e.is_file))
                        .collect();
                    out.sort();
                    Some(out)
                }
                None => self.base.as_ref()?.read_dir(path),
            };
        }
        let local = self.mem.read_dir(path);
        let inherited = self.base.as_ref().and_then(|base| base.read_dir(path));
        if local.is_none() && inherited.is_none() {
            return None;
        }
        let mut merged = std::collections::BTreeMap::new();
        for (name, is_file) in inherited.into_iter().flatten() {
            merged.insert(name, is_file);
        }
        for (name, is_file) in local.into_iter().flatten() {
            merged.insert(name, is_file);
        }
        Some(merged.into_iter().collect())
    }
}

/// Options for the mounted site tree.
#[derive(Debug, Clone, serde::Deserialize)]
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
    /// Include resolution order. TRUE (default; the stock-template path): live
    /// engine fragments shadow staged tree copies. FALSE: mounted `_includes`
    /// win and the resolver is consulted only afterward.
    #[serde(default = "default_true")]
    pub engine_first_includes: bool,
    /// Permit registered Publisher fragment includes to cross the typed
    /// ArtifactResolver boundary. TRUE by default for the stock native
    /// template path. A callback-free native consumer can set this FALSE: a
    /// missing include then remains a missing file and cannot invoke the
    /// FragmentEngine regardless of include precedence. Cycle does not mount
    /// this Rust render surface at all.
    #[serde(default = "default_true")]
    pub artifact_resolution: bool,
}

fn default_true() -> bool {
    true
}

impl Default for SiteOptions {
    fn default() -> Self {
        Self {
            active_tables: false,
            run_uuid: None,
            engine_first_includes: true,
            artifact_resolution: true,
        }
    }
}

/// The expensive, reusable semantic half of rendering.
///
/// The snapshot-complete semantic tree is retained so rebuilding a page tree
/// after a site-only overlay neither reserializes compiled resources nor
/// reloads the FragmentEngine's `IgContext`.
pub struct RenderSemantics {
    pub engine: Rc<FragmentEngine>,
    packages: Option<PackageView>,
    tree: Rc<dyn TreeSource>,
}

/// The per-generation render state.
pub struct RenderState {
    engine_first_includes: bool,
    artifact_resolution: bool,
    pub engine: Rc<FragmentEngine>,
    pub site: SiteData,
    pub tree: Rc<dyn TreeSource>,
    /// The session-shared typed artifact cache.
    pub frag_cache: SharedArtifactCache,
    /// Page source names available under /site/en (stem -> source path).
    pages: Vec<(String, PathBuf)>,
}

const OWN_DIR: &str = "/own";
const SITE_DIR: &str = "/site";

fn insert_compiled(mem: &mut MemTree, compiled: &[(PathBuf, Value)]) -> Result<(), String> {
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
    Ok(())
}

fn package_root(bundle: &Option<PackageView>) -> PathBuf {
    bundle
        .as_ref()
        .map(|bundle| bundle.root().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/__no_bundle__"))
}

/// Build the semantic FHIR core. Its tree intentionally excludes ordinary site
/// files, making reuse across page/template overlays correct by construction.
pub fn build_render_semantics(
    compiled: Vec<(PathBuf, Value)>,
    bundle: Option<PackageView>,
    site_files: &HashMap<PathBuf, Vec<u8>>,
    options: &SiteOptions,
) -> Result<RenderSemantics, String> {
    let explicit_guides: Vec<ResourceIdentity> = compiled
        .iter()
        .filter(|(path, value)| {
            path.starts_with("/__ig__")
                && value.get("resourceType").and_then(Value::as_str) == Some("ImplementationGuide")
        })
        .filter_map(|(_, value)| {
            value
                .get("id")
                .and_then(Value::as_str)
                .map(|id| ResourceIdentity {
                    resource_type: "ImplementationGuide".into(),
                    id: id.to_string(),
                })
        })
        .collect();
    if explicit_guides.len() > 1 {
        return Err("render semantics has multiple explicit primary ImplementationGuides".into());
    }
    let primary_guide = explicit_guides.first();
    let mut mem = MemTree::new();
    insert_compiled(&mut mem, &compiled)?;

    // Terminology cache bytes are semantic inputs even though the public mount
    // surface places them under /site. Copy only this subtree into the semantic
    // tree; retaining an old semantic core can therefore never retain stale
    // pages, layouts, includes, data, or assets.
    let mut has_txcache = false;
    let txcache_root = Path::new(SITE_DIR).join("txcache");
    for (path, bytes) in site_files {
        if path.starts_with(&txcache_root) {
            has_txcache = true;
            mem.insert(path.clone(), bytes.clone());
        }
    }
    let pkg_root = package_root(&bundle);
    let tree: Rc<dyn TreeSource> = Rc::new(SessionTree {
        mem,
        pkg: bundle.clone(),
        pkg_root: pkg_root.clone(),
        base: None,
    });

    let txcache_dir = if has_txcache {
        Some(PathBuf::from(format!("{}/txcache", SITE_DIR)))
    } else {
        None
    };
    let ctx = IgContext::load_with_tree_and_primary(
        tree.clone(),
        Path::new(OWN_DIR),
        &pkg_root,
        txcache_dir.as_deref(),
        primary_guide,
    );

    // Facts mirror pagecorpus's page-pass set: version + txcache. Whole-IG
    // aggregate kinds needing richer facts fire their documented loud gaps.
    let ig_candidates: Vec<&Value> = compiled
        .iter()
        .filter(|(_, value)| {
            value.get("resourceType").and_then(Value::as_str) == Some("ImplementationGuide")
        })
        .map(|(_, value)| value)
        .collect();
    let ig = match primary_guide {
        Some(primary) => ig_candidates
            .iter()
            .copied()
            .find(|value| value.get("id").and_then(Value::as_str) == Some(primary.id.as_str())),
        None if ig_candidates.len() == 1 => ig_candidates.first().copied(),
        _ => None,
    };
    let ig_version = ig
        .and_then(|v| v.get("version").and_then(|x| x.as_str()))
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
    let engine = Rc::new(FragmentEngine::new(ctx, uuid, options.active_tables, facts));

    Ok(RenderSemantics {
        engine,
        packages: bundle,
        tree,
    })
}

/// Build the cheap page/template half over an already-loaded semantic core.
pub fn build_render_state_from_semantics(
    semantics: &RenderSemantics,
    site_files: Rc<HashMap<PathBuf, Vec<u8>>>,
    options: &SiteOptions,
) -> Result<RenderState, String> {
    // /site: the mounted tree, verbatim. Page sources = every .md/.html not
    // under the non-page dirs; key = output rel path (.md maps to .html), which
    // is ALSO the Jekyll page.path (`en/<name>` in multi-language layouts,
    // `<name>` in flat ones — the tree shape carries it).
    let mut pages: Vec<(String, PathBuf)> = Vec::new();
    for p in site_files.keys() {
        let Ok(rest) = p.strip_prefix(SITE_DIR) else {
            continue;
        };
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

    let pkg_root = package_root(&semantics.packages);
    let tree: Rc<dyn TreeSource> = Rc::new(SiteMapTree {
        files: site_files,
        base: semantics.tree.clone(),
        pkg_root,
    });

    let site = SiteData::load_with_tree(&*tree, &Path::new(SITE_DIR).join("_data"));

    Ok(RenderState {
        engine: semantics.engine.clone(),
        site,
        tree,
        frag_cache: Rc::new(RefCell::new(Default::default())),
        pages,
        engine_first_includes: options.engine_first_includes,
        artifact_resolution: options.artifact_resolution,
    })
}

/// Convenience constructor for native callers that do not manage semantic and
/// site lifetimes separately. Session/Engine uses the split constructors above.
#[cfg(test)]
pub fn build_render_state(
    compiled: &[(PathBuf, Value)],
    bundle: Option<Rc<BundleSource>>,
    site_files: &HashMap<PathBuf, Vec<u8>>,
    options: &SiteOptions,
) -> Result<RenderState, String> {
    let bundle = bundle.map(|source| {
        let root = source.cache_root().to_path_buf();
        PackageView::new(source, root, None)
    });
    let semantics = build_render_semantics(compiled.to_vec(), bundle, site_files, options)?;
    build_render_state_from_semantics(&semantics, Rc::new(site_files.clone()), options)
}

impl RenderState {
    fn provider(&self) -> PageProvider<'_> {
        // engine_first: LIVE fragments (from the current compile generation)
        // shadow any staged copies in the mounted tree — the stale-fragment
        // hazard when a harvested site tree carries publisher `_includes`
        // dumps. Unregistered/authored content includes still come from the
        // tree.
        let mut provider = PageProvider::new(&self.site, &Path::new(SITE_DIR).join("_includes"))
            .with_engine_first(self.engine_first_includes)
            .with_pages_root(Path::new(SITE_DIR))
            .with_tree(self.tree.clone())
            .with_shared_cache(self.frag_cache.clone());
        if self.artifact_resolution {
            provider = provider
                .with_artifact_resolver(FragmentEngineArtifactResolver::new(self.engine.as_ref()));
        }
        provider
    }

    /// Render one fragment through the SHARED typed artifact cache. Its key is
    /// the fragment artifact translated from
    /// `{ref}-{kind}.xhtml`, the same key the page include loop uses.
    #[cfg(test)]
    pub fn render_fragment(&self, ref_: &str, kind: &str) -> Result<String, FragError> {
        let include_name = if ref_.is_empty() {
            format!("{kind}.xhtml")
        } else {
            format!("{ref_}-{kind}.xhtml")
        };
        let artifact_key = legacy_include_to_artifact_key(&include_name);
        if let Some(key) = artifact_key.as_ref() {
            if let Some(ArtifactCacheEntry::Ready(hit)) = self.frag_cache.borrow().get(key).cloned()
            {
                return Ok(hit);
            }
        }
        let out = self.engine.render_fragment(ref_, kind)?;
        if let Some(key) = artifact_key {
            self.frag_cache
                .borrow_mut()
                .insert(key, ArtifactCacheEntry::Ready(out.clone()));
        }
        Ok(out)
    }

    /// Render a page by output rel path (e.g. `en/index.html`, or `index.html`
    /// in a flat layout) — the same string is the Jekyll `page.path`. A source
    /// without front matter is a static file: returned verbatim (the
    /// publisher's Jekyll copies those unrendered).
    #[cfg(any(not(feature = "dependency-observation"), test))]
    pub(crate) fn render_page_by_name(&self, name: &str) -> Result<String, String> {
        self.render_page_tracked_by_name(name).map(|(html, _)| html)
    }

    /// Render one page and retain its typed artifact read set for diagnostics
    /// and closure tests. The immutable SiteBuild never changes while outputs
    /// are rendered or memoized.
    pub(crate) fn render_page_tracked_by_name(
        &self,
        name: &str,
    ) -> Result<(String, PageArtifactReadSet), String> {
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
        let html = render_page(&src, key, &provider);
        Ok((html, provider.take_page_artifact_reads()))
    }

    /// Output page rel paths, sorted.
    pub fn list_pages(&self) -> Vec<String> {
        self.pages.iter().map(|(k, _)| k.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use render_liquid::DataProvider;
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

    fn minimal_fragment_state(artifact_resolution: bool) -> RenderState {
        let compiled = vec![(
            PathBuf::from("StructureDefinition-test.json"),
            serde_json::json!({
                "resourceType": "StructureDefinition",
                "id": "test",
                "url": "http://example.org/StructureDefinition/test",
                "name": "Test",
                "status": "draft",
                "fhirVersion": "4.0.1",
                "kind": "resource",
                "abstract": false,
                "type": "Patient",
                "baseDefinition": "http://hl7.org/fhir/StructureDefinition/Patient",
                "derivation": "constraint",
                "snapshot": { "element": [{ "id": "Patient", "path": "Patient" }] }
            }),
        )];
        build_render_state(
            &compiled,
            None,
            &HashMap::new(),
            &SiteOptions {
                artifact_resolution,
                ..Default::default()
            },
        )
        .expect("minimal render state")
    }

    #[test]
    fn explicit_primary_guide_controls_render_context_and_additional_guides_remain_own() {
        let compiled = vec![
            (
                PathBuf::from("/__compiled__/ImplementationGuide-aaa-example.json"),
                serde_json::json!({
                    "resourceType": "ImplementationGuide",
                    "id": "aaa-example",
                    "packageId": "wrong.example",
                    "url": "https://wrong.example/ImplementationGuide/aaa-example",
                    "version": "9.0.0",
                    "fhirVersion": ["5.0.0"]
                }),
            ),
            (
                PathBuf::from("/__ig__/ImplementationGuide-primary.json"),
                serde_json::json!({
                    "resourceType": "ImplementationGuide",
                    "id": "primary",
                    "packageId": "example.primary",
                    "url": "https://example.org/ImplementationGuide/primary",
                    "version": "1.0.0",
                    "fhirVersion": ["4.0.1"]
                }),
            ),
        ];
        let semantics =
            build_render_semantics(compiled, None, &HashMap::new(), &SiteOptions::default())
                .unwrap();
        let context = semantics.engine.ctx();
        assert_eq!(context.own_package_id(), Some("example.primary"));
        assert_eq!(
            context.own_canonical_prefix().as_deref(),
            Some("https://example.org")
        );
        let own = context.own_resources();
        assert!(own.iter().any(
            |resource| resource.rtype == "ImplementationGuide" && resource.id == "aaa-example"
        ));
        assert!(!own
            .iter()
            .any(|resource| resource.rtype == "ImplementationGuide" && resource.id == "primary"));
    }

    #[test]
    fn artifact_resolution_option_is_explicit_and_can_make_includes_callback_free() {
        let defaults: SiteOptions = serde_json::from_str("{}").unwrap();
        assert!(defaults.artifact_resolution);
        let disabled_wire: SiteOptions =
            serde_json::from_str(r#"{"artifactResolution":false}"#).unwrap();
        assert!(!disabled_wire.artifact_resolution);

        let include = "StructureDefinition-test-history.xhtml";
        let disabled = minimal_fragment_state(false);
        let disabled_provider = disabled.provider();
        assert_eq!(disabled_provider.include_source(include), None);
        assert!(matches!(
            disabled.frag_cache.borrow().values().next(),
            Some(ArtifactCacheEntry::NotReady(error))
                if matches!(error.failure(), render_page::ArtifactResolveFailure::Unsupported { .. })
        ));

        let enabled = minimal_fragment_state(true);
        let enabled_provider = enabled.provider();
        assert_eq!(
            enabled_provider.include_source(include).as_deref(),
            Some("{% raw %}{% endraw %}")
        );
        assert_eq!(enabled.frag_cache.borrow().len(), 1);
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
        let provider = PageProvider::new(&site, &b.join("temp/pages/_includes"))
            .with_artifact_resolver(FragmentEngineArtifactResolver::new(&engine))
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
            let Some(t) = FsTree.read(&own_dir.join(&n)) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(&t) else {
                continue;
            };
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
            let rel = f
                .strip_prefix(&pages_root)
                .unwrap()
                .to_string_lossy()
                .to_string();
            site_files.insert(
                PathBuf::from(format!("/site/{}", rel)),
                std::fs::read(&f).unwrap(),
            );
        }
        let mut tx = Vec::new();
        walk(&txcache_dir, &mut tx);
        for f in tx {
            let rel = f
                .strip_prefix(&txcache_dir)
                .unwrap()
                .to_string_lossy()
                .to_string();
            site_files.insert(
                PathBuf::from(format!("/site/txcache/{}", rel)),
                std::fs::read(&f).unwrap(),
            );
        }
        let opts = SiteOptions {
            active_tables: true,
            run_uuid: Some(uuid),
            engine_first_includes: true,
            artifact_resolution: true,
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
                let dump = std::env::temp_dir().join(format!("equiv-{}", key.replace('/', "_")));
                let _ = std::fs::write(dump.with_extension("direct.html"), &direct);
                let _ = std::fs::write(dump.with_extension("session.html"), &ours);
                mismatches.push(key);
            }
        }
        assert!(
            pages > 500,
            "expected the full plan-net page set, got {pages}"
        );
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
