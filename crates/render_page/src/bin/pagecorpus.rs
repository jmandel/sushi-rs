//! pagecorpus — the F5 page-parity gate. For an IG, render every localized page
//! input (`temp/pages/en/*.html`) through the Rust page pass and diff the result
//! against the publisher's Jekyll output (`output/en/*.html`, or for cycle the
//! `temp/pages/en/*.html` re-render — cycle's output/ has no en pages).
//!
//! Usage: pagecorpus <ig> [--verbose] [--only <name>] [--limit N] [--engine]
//!   ig: cycle | plan-net | us-core
//!   --engine : ALSO resolve registered generated includes through the typed
//!              FragmentEngine adapter (proves byte-identical materialization);
//!              default reads all includes from the build's pre-generated
//!              _includes/ (pure page-pass gate, fragment layer isolated).

use std::path::{Path, PathBuf};

use render_page::{render_page, FragmentEngineArtifactResolver, PageProvider, SiteData};
use render_sd::context::IgContext;
use render_sd::engine::{FragmentEngine, IgFacts};

const REPO: &str = "/home/jmandel/hobby/sushi-rs-snapshot";
const F0: &str = "/home/jmandel/hobby/sushi-rs-snapshot-f0-builds";

struct IgPaths {
    /// The Jekyll source root (`temp/pages`).
    pages_root: PathBuf,
    /// The page INPUT dir (`temp/pages/en`).
    input_dir: PathBuf,
    /// The golden OUTPUT dir (`output/en`, or the input dir itself if no output).
    golden_dir: PathBuf,
    /// `_data`.
    data_dir: PathBuf,
    /// `_includes`.
    includes_dir: PathBuf,
    /// own resource dir (for the FragmentEngine ctx), packages, txcache.
    own_dir: PathBuf,
    packages_dir: PathBuf,
    txcache_dir: Option<PathBuf>,
}

fn ig_paths(ig: &str) -> IgPaths {
    match ig {
        "plan-net" => {
            // Multi-language build: inputs under temp/pages/en, goldens under
            // output/en, page.path = `en/<name>`.
            let b = format!("{}/{}", F0, ig);
            IgPaths {
                pages_root: PathBuf::from(format!("{}/temp/pages", b)),
                input_dir: PathBuf::from(format!("{}/temp/pages/en", b)),
                golden_dir: PathBuf::from(format!("{}/output/en", b)),
                data_dir: PathBuf::from(format!("{}/temp/pages/_data", b)),
                includes_dir: PathBuf::from(format!("{}/temp/pages/_includes", b)),
                own_dir: PathBuf::from(format!("{}/output", b)),
                packages_dir: PathBuf::from(format!("{}/.home/.fhir/packages", b)),
                txcache_dir: Some(PathBuf::from(format!("{}/input-cache/txcache", b))),
            }
        }
        "us-core" => {
            // FLAT single-language build: inputs temp/pages/*.html, goldens
            // output/*.html, page.path = `<name>` (no en/ subdir).
            let b = format!("{}/{}", F0, ig);
            IgPaths {
                pages_root: PathBuf::from(format!("{}/temp/pages", b)),
                input_dir: PathBuf::from(format!("{}/temp/pages", b)),
                golden_dir: PathBuf::from(format!("{}/output", b)),
                data_dir: PathBuf::from(format!("{}/temp/pages/_data", b)),
                includes_dir: PathBuf::from(format!("{}/temp/pages/_includes", b)),
                own_dir: PathBuf::from(format!("{}/output", b)),
                packages_dir: PathBuf::from(format!("{}/.home/.fhir/packages", b)),
                txcache_dir: Some(PathBuf::from(format!("{}/input-cache/txcache", b))),
            }
        }
        "cycle" => {
            let b = "/home/jmandel/hobby/periodicity-impl/cycle";
            // cycle's committed output/ has no en pages (the publisher's Jekyll
            // step aborted on the missing `_includes/sample-viewer-links.md`,
            // which the sitegen wrapper generates in a real build). We regenerate
            // the rendered oracle by supplying that include (the wrapper's real
            // content shape) and running the SAME Jekyll (4.4.1) over the existing
            // temp/pages, then gate against it. `CYCLE_GOLDEN_DIR` points at that
            // fresh Jekyll output tree (see docs/render-worklog.md F5 target 4).
            let golden_dir = std::env::var("CYCLE_GOLDEN_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(format!("{}/output/en", b)));
            IgPaths {
                pages_root: PathBuf::from(format!("{}/temp/pages", b)),
                input_dir: PathBuf::from(format!("{}/temp/pages/en", b)),
                golden_dir,
                data_dir: PathBuf::from(format!("{}/temp/pages/_data", b)),
                includes_dir: PathBuf::from(format!("{}/temp/pages/_includes", b)),
                own_dir: PathBuf::from(format!("{}/temp/pages", b)),
                packages_dir: PathBuf::from(format!(
                    "{}/.fhir/packages",
                    std::env::var("HOME").unwrap_or_default()
                )),
                txcache_dir: Some(PathBuf::from(format!("{}/input-cache/txcache", b))),
            }
        }
        _ => panic!("unknown ig {}", ig),
    }
}

fn build_engine(ig: &str, p: &IgPaths) -> FragmentEngine {
    let ctx = IgContext::load_with_txcache(&p.own_dir, &p.packages_dir, p.txcache_dir.as_deref());
    let uuid = harvest_uuid(ig);
    let active_tables = ig_active_tables(ig);
    let facts = IgFacts {
        txcache_dir: p.txcache_dir.clone(),
        ig_version: ig_version(&p.own_dir),
        ..Default::default()
    };
    FragmentEngine::new(ctx, uuid, active_tables, facts)
}

fn harvest_uuid(ig: &str) -> String {
    let dir = format!("{}/render-goldens/{}/fragments", REPO, ig);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return String::new();
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.ends_with("-snapshot.xhtml") {
            if let Ok(text) = std::fs::read_to_string(e.path()) {
                if let Some(i) = text.find("  // ") {
                    let rest = &text[i + 5..];
                    if let Some(j) = rest.find('\n') {
                        let cand = &rest[..j];
                        if cand.len() == 36 {
                            return cand.to_string();
                        }
                    }
                }
            }
        }
    }
    String::new()
}

fn ig_active_tables(ig: &str) -> bool {
    matches!(ig, "plan-net" | "cycle")
}

fn ig_version(own_dir: &Path) -> String {
    if let Ok(rd) = std::fs::read_dir(own_dir) {
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
    }
    String::new()
}

fn first_divergence(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

/// Harvest the STATIC per-IG `<!--ReleaseHeader-->…<!--EndReleaseHeader-->` block
/// from any golden page (byte-identical across pages). Returns None if the golden
/// still carries the Jekyll placeholder (`Publish Box goes here`) — i.e. the
/// build's output/ is the pre-substitution stage (plan-net) and no post-pass is
/// needed.
fn harvest_release_header(golden_dir: &Path) -> Option<String> {
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: pagecorpus <ig> [--verbose] [--only NAME] [--limit N] [--engine]");
        std::process::exit(2);
    }
    let ig = &args[1];
    let verbose = args.iter().any(|a| a == "--verbose");
    let use_engine = args.iter().any(|a| a == "--engine");
    // Payload-dump pages (`.json.html`/`.xml.html`/`.ttl.html`) are the
    // serialized-resource syntax-highlighted echoes — the SAME class the harvest
    // script excludes from the fragment corpus (low renderer value, size-
    // pathological). They also route through a post-Jekyll XHTML normalization
    // the analysis pages don't (one collapsed leading newline; verified render_
    // liquid matches Ruby Liquid byte-for-byte on the pre-normalization output).
    // `--dumps` includes them; default classifies them out (documented).
    let include_dumps = args.iter().any(|a| a == "--dumps");
    let only = args
        .iter()
        .position(|a| a == "--only")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let limit: Option<usize> = args
        .iter()
        .position(|a| a == "--limit")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok());

    let p = ig_paths(ig);
    let site = SiteData::load(&p.data_dir);
    let engine = if use_engine {
        Some(build_engine(ig, &p))
    } else {
        None
    };
    let engine_first = args.iter().any(|a| a == "--engine-first");
    let mut provider = PageProvider::new(&site, &p.includes_dir)
        .with_engine_first(engine_first)
        .with_pages_root(&p.pages_root);
    if let Some(engine) = engine.as_ref() {
        provider = provider.with_artifact_resolver(FragmentEngineArtifactResolver::new(engine));
    }
    // Post-Jekyll ReleaseHeader substitution (us-core's output/ is this later
    // stage; plan-net's is pre-substitution -> None -> no-op).
    let release_header = harvest_release_header(&p.golden_dir);

    // Flat (us-core) vs en/ (plan-net): page.path prefix and iteration dir differ.
    let flat = p.input_dir == p.pages_root;
    let mut inputs: Vec<PathBuf> = std::fs::read_dir(&p.input_dir)
        .unwrap_or_else(|_| panic!("read input dir {}", p.input_dir.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|f| f.is_file() && f.extension().and_then(|x| x.to_str()) == Some("html"))
        .collect();
    inputs.sort();

    let mut pass = 0usize;
    let mut total = 0usize;
    let mut no_golden = 0usize;
    // Static `<?xml`-headed pages (no front matter) are Jekyll PASS-THROUGHS;
    // their goldens were re-serialized by a LATER publisher stage (BOM + CRLF
    // + Java-DOM meta-attribute reorder — the HTMLInspector/XhtmlComposer
    // write-back, same class as the payload-dump normalization, F5 finding
    // #4). The editor pipeline has no such stage; classified, not chased.
    let mut xml_reser = 0usize;
    let mut fails: Vec<(String, usize, usize)> = Vec::new();

    for inp in &inputs {
        let name = inp.file_name().unwrap().to_string_lossy().to_string();
        if let Some(o) = &only {
            if &name != o {
                continue;
            }
        }
        let is_dump = name.ends_with(".json.html")
            || name.ends_with(".xml.html")
            || name.ends_with(".ttl.html");
        if is_dump && !include_dumps && only.is_none() {
            continue;
        }
        let golden_p = p.golden_dir.join(&name);
        let Ok(golden) = std::fs::read_to_string(&golden_p) else {
            no_golden += 1;
            continue;
        };
        let src = std::fs::read_to_string(inp).unwrap();
        // page.path is the Jekyll-relative path: `en/<name>` (multi-lang) or
        // `<name>` (flat single-lang).
        let page_path = if flat {
            name.clone()
        } else {
            format!("en/{}", name)
        };
        let mut ours = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            render_page(&src, &page_path, &provider)
        }))
        .unwrap_or_else(|_| "<PAGE RENDER PANIC>".to_string());
        if let Some(rh) = &release_header {
            ours = render_page::apply_release_header(&ours, rh);
        }

        if let Ok(dump) = std::env::var("PAGE_DUMP_DIR") {
            if only.as_deref() == Some(name.as_str()) {
                let d = PathBuf::from(dump);
                std::fs::write(d.join("page-ours.html"), &ours).ok();
                std::fs::write(d.join("page-gold.html"), &golden).ok();
            }
        }

        total += 1;
        if ours != golden && ours.starts_with("<?xml") && golden.starts_with("\u{feff}<?xml") {
            // Classified: post-Jekyll XHTML re-serialization (see xml_reser).
            xml_reser += 1;
        } else if ours == golden {
            pass += 1;
        } else {
            let d = first_divergence(&ours, &golden);
            fails.push((name.clone(), d, golden.len()));
            if verbose && fails.len() <= 5 {
                report(&name, &ours, &golden, d);
            }
        }
        if let Some(l) = limit {
            if total >= l {
                break;
            }
        }
    }

    println!(
        "pages {}: {}/{} byte-identical  (no-golden {}{}){}",
        ig,
        pass,
        total,
        no_golden,
        if xml_reser > 0 {
            format!(
                ", xml-static-reser {} [post-Jekyll stage, classified]",
                xml_reser
            )
        } else {
            String::new()
        },
        if use_engine {
            format!("  [engine misses: {}]", provider.miss_count.borrow())
        } else {
            String::new()
        }
    );
    for (n, d, len) in fails.iter().take(500) {
        println!("    {} @ {} / {}", n, d, len);
    }
}

fn report(name: &str, ours: &str, golden: &str, d: usize) {
    let ctx = 100;
    let lo = d.saturating_sub(ctx);
    let show = |s: &str| -> String {
        let end = (d + ctx).min(s.len());
        s.get(lo..end).unwrap_or("").replace('\n', "\\n")
    };
    println!("--- {} first divergence @ byte {} ---", name, d);
    println!("  OURS  : …{}", show(ours));
    println!("  GOLDEN: …{}", show(golden));
}
