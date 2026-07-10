//! `fig watch [--serve :port]` — the incremental dev loop, the native twin of
//! the browser editor.
//!
//! fs poll (dependency-free mtime scan) → dirty cone → re-render only dirtied
//! pages → serve with live-reload. The dirty cone is derived from the SAME
//! read-set boundary the editor uses:
//!   - each page's typed artifact request/read sets are captured by
//!     `render_page_tracked`; a resource edit re-renders pages with that exact
//!     resource scope, while a whole-IG request invalidates on any resource edit;
//!   - a page SOURCE edit re-renders that page;
//!   - a `_data`/`_includes` edit re-renders every page (coarse, correct).
//!
//! Warm-edit-to-updated-page is the gate (< 1s on us-core natively). The cost is
//! the dirty-cone recompute, not event latency, so an mtime poll is sufficient.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result};
use render_page::{publisher_reference_to_resource_key, PageArtifactReadSet, SiteData};
use render_sd::context::IgContext;
use render_sd::engine::{FragmentEngine, IgFacts};
use site_build::{ArtifactKey, FragmentScope, ResourceKey};

use crate::engine::{RenderOptions, RenderRoot};

/// A rendered page + its typed semantic artifact dependency signature.
struct PageState {
    /// Source input file.
    input: PathBuf,
    /// Rendered HTML.
    html: String,
    /// Typed semantic requests attempted and artifacts successfully read.
    artifacts: PageArtifactReadSet,
}

/// The live watch state: all pages, keyed by page.path.
pub struct WatchState {
    root: RenderRoot,
    opts: RenderOptions,
    pages: HashMap<String, PageState>,
    /// A monotonically-bumped generation the live-reload client polls.
    generation: u64,
}

impl WatchState {
    /// Do the initial full render, capturing every page's read-set.
    pub fn initial(root: RenderRoot, opts: RenderOptions) -> Result<WatchState> {
        let mut st = WatchState {
            root,
            opts,
            pages: HashMap::new(),
            generation: 1,
        };
        st.render_all()?;
        Ok(st)
    }

    fn build_engine(&self) -> FragmentEngine {
        let ctx = IgContext::load_with_txcache(
            &self.root.own_dir,
            &self.root.packages_dir,
            self.root.txcache_dir.as_deref(),
        );
        let facts = IgFacts {
            txcache_dir: self.root.txcache_dir.clone(),
            ..Default::default()
        };
        FragmentEngine::new(
            ctx,
            self.opts.run_uuid.clone(),
            self.opts.active_tables,
            facts,
        )
    }

    fn page_inputs(&self) -> Result<Vec<PathBuf>> {
        let mut inputs: Vec<PathBuf> = std::fs::read_dir(&self.root.input_dir)
            .with_context(|| format!("read {}", self.root.input_dir.display()))?
            .flatten()
            .map(|e| e.path())
            .filter(|f| f.is_file() && f.extension().and_then(|x| x.to_str()) == Some("html"))
            .filter(|f| {
                let n = f.file_name().unwrap().to_string_lossy();
                self.opts.include_dumps
                    || !(n.ends_with(".json.html")
                        || n.ends_with(".xml.html")
                        || n.ends_with(".ttl.html"))
            })
            .collect();
        inputs.sort();
        Ok(inputs)
    }

    /// Full render — every page, read-sets captured.
    fn render_all(&mut self) -> Result<()> {
        let engine = self.opts.engine.then(|| self.build_engine());
        let site = SiteData::load(&self.root.data_dir);
        let inputs = self.page_inputs()?;
        self.pages.clear();
        for inp in inputs {
            let (html, reads) = crate::engine::render_page_tracked(
                &self.root,
                engine.as_ref(),
                &site,
                &self.opts,
                &inp,
            )?;
            let name = inp.file_name().unwrap().to_string_lossy().to_string();
            let page_path = if self.root.flat {
                name.clone()
            } else {
                format!("en/{}", name)
            };
            self.pages.insert(
                page_path.clone(),
                PageState {
                    input: inp,
                    html,
                    artifacts: reads,
                },
            );
        }
        Ok(())
    }

    /// Re-render only the pages dirtied by a set of changed paths (relative to
    /// the watched roots). Returns the number of pages re-rendered.
    pub fn on_change(&mut self, changed: &[PathBuf]) -> Result<usize> {
        // Classify the change. A change under _data/_includes, or to an own
        // resource that whole-IG kinds consult, invalidates broadly.
        let mut data_or_include_changed = false;
        let mut own_changed = false;
        let mut changed_resources: std::collections::BTreeSet<ResourceKey> =
            std::collections::BTreeSet::new();
        let mut changed_page_sources: Vec<PathBuf> = Vec::new();
        for p in changed {
            let s = p.to_string_lossy();
            if s.contains("/_data/") || s.contains("/_includes/") || s.contains("/_layouts/") {
                data_or_include_changed = true;
            } else if p.starts_with(&self.root.own_dir) {
                own_changed = true;
                if let Some(stem) = p.file_stem().and_then(|x| x.to_str()) {
                    if let Some(resource) = publisher_reference_to_resource_key(stem) {
                        changed_resources.insert(resource);
                    }
                }
            } else if p.starts_with(&self.root.input_dir)
                && p.extension().and_then(|x| x.to_str()) == Some("html")
            {
                changed_page_sources.push(p.clone());
            }
        }

        // Compute the dirty page set.
        let dirty: Vec<String> = if data_or_include_changed {
            // Coarse but correct: _data / _includes feed every page.
            self.pages.keys().cloned().collect()
        } else {
            let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            // Own-resource edits: a page is dirty if its read-set names that
            // resource (per-resource kind) OR pulled any whole-IG kind.
            if own_changed {
                for (path, st) in &self.pages {
                    if requests_depend_on_own_change(st.artifacts.requested(), &changed_resources) {
                        set.insert(path.clone());
                    }
                }
            }
            // Page-source edits: exactly that page.
            for src in &changed_page_sources {
                let name = src.file_name().unwrap().to_string_lossy().to_string();
                let pp = if self.root.flat {
                    name
                } else {
                    format!("en/{}", name)
                };
                set.insert(pp);
            }
            set.into_iter().collect()
        };

        if dirty.is_empty() {
            return Ok(0);
        }

        // Re-render the dirty cone (fresh engine so own-resource edits are seen).
        let engine = self.opts.engine.then(|| self.build_engine());
        let site = SiteData::load(&self.root.data_dir);
        let mut count = 0usize;
        for pp in &dirty {
            let input = match self.pages.get(pp) {
                Some(st) => st.input.clone(),
                None => continue,
            };
            let (html, reads) = crate::engine::render_page_tracked(
                &self.root,
                engine.as_ref(),
                &site,
                &self.opts,
                &input,
            )?;
            if let Some(st) = self.pages.get_mut(pp) {
                st.html = html;
                st.artifacts = reads;
                count += 1;
            }
        }
        self.generation += 1;
        Ok(count)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Look up a rendered page by its output path (`en/index.html`, `index.html`,
    /// or `/` → the first page). Returns the HTML.
    pub fn page_html(&self, req_path: &str) -> Option<&str> {
        let p = req_path.trim_start_matches('/');
        if p.is_empty() || p == "index.html" {
            // Prefer a top-level or en/ index.
            for cand in ["index.html", "en/index.html", "toc.html", "en/toc.html"] {
                if let Some(st) = self.pages.get(cand) {
                    return Some(&st.html);
                }
            }
            return self.pages.values().next().map(|s| s.html.as_str());
        }
        self.pages.get(p).map(|s| s.html.as_str())
    }
}

/// Typed dirty-cone predicate. Requests, rather than only successful reads,
/// participate so an artifact that previously failed/deferred is retried when
/// its semantic inputs change.
fn requests_depend_on_own_change(
    requested: &std::collections::BTreeSet<ArtifactKey>,
    changed_resources: &std::collections::BTreeSet<ResourceKey>,
) -> bool {
    requested.iter().any(|key| {
        let ArtifactKey::Fragment { scope, .. } = key else {
            return false;
        };
        match scope {
            FragmentScope::WholeIg => true,
            FragmentScope::Resource { resource } => changed_resources.contains(resource),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use render_page::legacy_include_to_artifact_key;

    use super::*;

    #[test]
    fn typed_dirty_cone_distinguishes_resource_and_whole_ig_scope() {
        let patient = publisher_reference_to_resource_key("StructureDefinition-patient").unwrap();
        let observation =
            publisher_reference_to_resource_key("StructureDefinition-observation").unwrap();
        let changed = BTreeSet::from([patient.clone()]);

        let patient_snapshot = BTreeSet::from([legacy_include_to_artifact_key(
            "StructureDefinition-patient-snapshot.xhtml",
        )
        .unwrap()]);
        assert!(requests_depend_on_own_change(&patient_snapshot, &changed));

        let observation_snapshot = BTreeSet::from([legacy_include_to_artifact_key(
            "StructureDefinition-observation-snapshot.xhtml",
        )
        .unwrap()]);
        assert!(!requests_depend_on_own_change(
            &observation_snapshot,
            &changed
        ));

        let whole_ig = BTreeSet::from([legacy_include_to_artifact_key(
            "StructureDefinition-observation-uses.xhtml",
        )
        .unwrap()]);
        assert!(requests_depend_on_own_change(&whole_ig, &changed));
        assert_ne!(patient, observation);
    }
}

/// Snapshot the mtimes of every watched file (page inputs + _data + _includes +
/// own resources). Cheap enough to poll.
pub fn scan_mtimes(root: &RenderRoot) -> HashMap<PathBuf, SystemTime> {
    let mut out = HashMap::new();
    for dir in [
        &root.input_dir,
        &root.data_dir,
        &root.includes_dir,
        &root.own_dir,
    ] {
        walk_mtimes(dir, &mut out);
    }
    out
}

fn walk_mtimes(dir: &Path, out: &mut HashMap<PathBuf, SystemTime>) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(md) = e.metadata() {
                if let Ok(mt) = md.modified() {
                    out.insert(p, mt);
                }
            }
        }
    }
}

/// The paths that changed between two mtime scans (added/modified).
pub fn diff_mtimes(
    prev: &HashMap<PathBuf, SystemTime>,
    cur: &HashMap<PathBuf, SystemTime>,
) -> Vec<PathBuf> {
    let mut changed = Vec::new();
    for (p, t) in cur {
        match prev.get(p) {
            Some(pt) if pt == t => {}
            _ => changed.push(p.clone()),
        }
    }
    changed
}

const LIVE_RELOAD: &str = r#"<script>
(function(){var g=null;setInterval(function(){
 fetch('/__fig_gen').then(function(r){return r.text();}).then(function(t){
  if(g===null){g=t;return;} if(t!==g){location.reload();}
 }).catch(function(){});
},400);})();
</script>"#;

/// Run the watch loop with an optional live-reload HTTP server.
pub fn serve(state: WatchState, addr: Option<&str>, poll_ms: u64) -> Result<()> {
    let root = state.root.clone();
    let shared = Arc::new(Mutex::new(state));

    // HTTP server thread (optional).
    if let Some(addr) = addr {
        let server =
            tiny_http::Server::http(addr).map_err(|e| anyhow::anyhow!("bind {addr}: {e}"))?;
        eprintln!("fig watch: serving http://{addr}/ (live-reload on)");
        let srv_state = shared.clone();
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let url = req.url().to_string();
                let st = srv_state.lock().unwrap();
                if url == "/__fig_gen" {
                    let body = st.generation().to_string();
                    let _ = req.respond(tiny_http::Response::from_string(body));
                    continue;
                }
                match st.page_html(&url) {
                    Some(html) => {
                        // Inject the live-reload poller before </body>.
                        let injected = if let Some(i) = html.rfind("</body>") {
                            format!("{}{}{}", &html[..i], LIVE_RELOAD, &html[i..])
                        } else {
                            format!("{html}{LIVE_RELOAD}")
                        };
                        let header = tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            &b"text/html; charset=utf-8"[..],
                        )
                        .unwrap();
                        let _ = req.respond(
                            tiny_http::Response::from_string(injected).with_header(header),
                        );
                    }
                    None => {
                        let _ = req.respond(
                            tiny_http::Response::from_string(format!(
                                "fig watch: no page for {url}"
                            ))
                            .with_status_code(404),
                        );
                    }
                }
            }
        });
    }

    // Poll loop.
    let mut prev = scan_mtimes(&root);
    eprintln!(
        "fig watch: watching {} (poll {poll_ms}ms). Ctrl-C to stop.",
        root.pages_root.display()
    );
    loop {
        std::thread::sleep(Duration::from_millis(poll_ms));
        let cur = scan_mtimes(&root);
        let changed = diff_mtimes(&prev, &cur);
        if changed.is_empty() {
            continue;
        }
        prev = cur;
        let t0 = Instant::now();
        let mut st = shared.lock().unwrap();
        let n = match st.on_change(&changed) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("fig watch: re-render error: {e:#}");
                continue;
            }
        };
        let ms = t0.elapsed().as_millis();
        drop(st);
        if n > 0 {
            eprintln!(
                "fig watch: {} file(s) changed → {n} page(s) re-rendered in {ms}ms",
                changed.len()
            );
        }
    }
}
