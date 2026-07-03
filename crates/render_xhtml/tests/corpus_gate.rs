//! C3 round-trip byte-parity gate over a fragment corpus.
//!
//! For each `_includes/*.xhtml` golden (excluding `-en.xhtml` language dupes):
//! strip the `{% raw %}...{% endraw %}` wrapper, parse the inner xhtml, and
//! re-compose it. The fragment round-trips iff SOME composer config reproduces
//! the exact bytes.
//!
//! Corpus dir resolution:
//!   * `RENDER_XHTML_CORPUS` env var, if set, or
//!   * the parallel-agent goldens under `render-goldens/` (repo-relative), if
//!     present, or
//!   * the cycle corpus at the hardcoded dev path (present in this workspace).
//! If none exist, the test is skipped (so CI without the corpus still passes).
//!
//! ## What "pass" means
//!
//! A non-empty golden that does NOT round-trip is only acceptable if it belongs
//! to a documented class of RAW-STRING fragments that fhir-core's OWN
//! XhtmlParser+XhtmlComposer also cannot round-trip (verified out-of-band with a
//! Java oracle against fhir-core 6.9.10-SNAPSHOT — see the task report). Those
//! fragments are hand-assembled strings (syntax-highlighted json/xml/ttl dumps,
//! pseudo-* templates, markdown ip-statements, ant-injected `<!--$$N$$-->`
//! placeholder tables, and StatusRenderer's malformed `class="` tables) that
//! never passed through the composer, so no serializer reproduces them. The gate
//! asserts every non-parity file matches this documented allow-list.

use std::path::{Path, PathBuf};

use render_xhtml::{is_known_non_roundtrippable, roundtrip_fragment_multi, strip_raw_wrapper};

fn resolve_corpus() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("RENDER_XHTML_CORPUS") {
        let p = PathBuf::from(d);
        if p.is_dir() {
            return Some(p);
        }
    }
    // repo-relative render-goldens/ (lands later from the F0 agent)
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for up in [
        manifest.join("../../render-goldens"),
        manifest.join("../render-goldens"),
    ] {
        if up.is_dir() {
            return Some(up);
        }
    }
    // cycle corpus dev path
    let cycle = PathBuf::from("/home/jmandel/hobby/periodicity-impl/cycle/temp/pages/_includes");
    if cycle.is_dir() {
        return Some(cycle);
    }
    None
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("xhtml") {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.ends_with("-en.xhtml") {
                continue;
            }
            out.push(p);
        }
    }
}

#[test]
fn corpus_round_trips_byte_exact() {
    let dir = match resolve_corpus() {
        Some(d) => d,
        None => {
            eprintln!("no corpus dir found; skipping corpus_gate");
            return;
        }
    };
    eprintln!("corpus: {}", dir.display());

    let mut files = Vec::new();
    collect(&dir, &mut files);
    files.sort();
    assert!(!files.is_empty(), "corpus dir had no .xhtml files");

    let mut ok = 0usize;
    let mut empty = 0usize;
    let mut unexpected: Vec<String> = Vec::new();

    for f in &files {
        let content = match std::fs::read_to_string(f) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let inner = match strip_raw_wrapper(&content) {
            Some(i) => i,
            None => continue,
        };
        if inner.trim().is_empty() {
            empty += 1;
            continue;
        }
        let basename = f.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let parity = matches!(roundtrip_fragment_multi(inner), Ok((_, Some(_))));
        if parity {
            ok += 1;
        } else if !is_known_non_roundtrippable(basename) {
            // A NEW non-parity file that is not in the documented raw-string
            // allow-list -> a real regression.
            unexpected.push(basename.to_string());
        }
    }

    eprintln!(
        "files={} empty={} parity={} allowed-raw-string-misses documented",
        files.len(),
        empty,
        ok
    );

    assert!(
        unexpected.is_empty(),
        "unexpected non-round-trip fragments (not in documented raw-string \
         allow-list): {:?}",
        unexpected
    );
    // Sanity floor: the cycle corpus has >1000 non-empty round-trippable frags.
    // For an unknown corpus we only require that *some* parity was achieved.
    assert!(ok > 0, "no fragments round-tripped; something is broken");
}
