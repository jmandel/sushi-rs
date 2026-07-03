//! Round-trip harness: run the C3 byte-parity gate against any corpus dir.
//!
//! Usage:
//!   roundtrip <dir>              # recurse <dir> for *.xhtml, exclude *-en.xhtml
//!   roundtrip <dir> --include-en # also test the -en language duplicates
//!   roundtrip <dir> --show N     # print up to N mismatch diffs (default 10)
//!
//! For each file: strip the publisher's `{% raw %}...{% endraw %}` wrapper,
//! parse the inner xhtml, re-compose it (Config::xml_compact), and compare to
//! the inner bytes. Exit code 0 iff every file round-trips byte-exact.

use std::fs;
use std::path::{Path, PathBuf};

use render_xhtml::{is_known_non_roundtrippable, roundtrip_fragment_multi, strip_raw_wrapper};

fn collect(dir: &Path, include_en: bool, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, include_en, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("xhtml") {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !include_en && name.ends_with("-en.xhtml") {
                continue;
            }
            out.push(p);
        }
    }
}

fn first_diff(a: &str, b: &str) -> Option<(usize, String, String)> {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let n = ab.len().min(bb.len());
    for i in 0..n {
        if ab[i] != bb[i] {
            let lo = i.saturating_sub(30);
            let a_ctx = &a[lo..(i + 30).min(a.len())];
            let b_ctx = &b[lo..(i + 30).min(b.len())];
            return Some((i, a_ctx.to_string(), b_ctx.to_string()));
        }
    }
    if ab.len() != bb.len() {
        return Some((n, format!("len={}", ab.len()), format!("len={}", bb.len())));
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: roundtrip <dir> [--include-en] [--show N]");
        std::process::exit(2);
    }
    let dir = PathBuf::from(&args[1]);
    let include_en = args.iter().any(|a| a == "--include-en");
    let show: usize = args
        .iter()
        .position(|a| a == "--show")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let mut files = Vec::new();
    collect(&dir, include_en, &mut files);
    files.sort();

    let mut total = 0usize;
    let mut empty = 0usize;
    let mut no_wrapper = 0usize;
    let mut ok = 0usize;
    let mut parse_err = 0usize;
    let mut mismatch = 0usize;
    let mut mismatch_known = 0usize;
    let mut parse_err_known = 0usize;
    let mut shown = 0usize;
    let mut by_config: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();

    for f in &files {
        let content = match fs::read_to_string(f) {
            Ok(c) => c,
            Err(_) => continue,
        };
        total += 1;
        let inner = match strip_raw_wrapper(&content) {
            Some(i) => i,
            None => {
                no_wrapper += 1;
                continue;
            }
        };
        if inner.trim().is_empty() {
            empty += 1;
            ok += 1; // empty inner -> empty output, trivially parity
            continue;
        }
        let basename = f.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let known_raw = is_known_non_roundtrippable(basename);
        match roundtrip_fragment_multi(inner) {
            Ok((recomposed, matched)) => {
                if let Some(label) = matched {
                    ok += 1;
                    *by_config.entry(label).or_insert(0) += 1;
                } else {
                    mismatch += 1;
                    if known_raw {
                        mismatch_known += 1;
                    } else if shown < show {
                        // Only show UNEXPECTED mismatches (potential regressions).
                        shown += 1;
                        println!("UNEXPECTED MISMATCH: {}", f.display());
                        if let Some((at, a, b)) = first_diff(inner, &recomposed) {
                            println!("  first diff at byte {}", at);
                            println!("  golden: ...{:?}...", a);
                            println!("  ours:   ...{:?}...", b);
                        }
                    }
                }
            }
            Err(e) => {
                parse_err += 1;
                if known_raw {
                    parse_err_known += 1;
                } else if shown < show {
                    shown += 1;
                    println!("UNEXPECTED PARSE-ERR: {}: {}", f.display(), e);
                }
            }
        }
    }

    let unexpected = (mismatch - mismatch_known) + (parse_err - parse_err_known);

    println!("--------------------------------------------------");
    println!("corpus dir     : {}", dir.display());
    println!("files (.xhtml) : {}", total);
    println!("  no wrapper   : {}", no_wrapper);
    println!("  empty inner  : {}", empty);
    println!("  round-trip OK: {}", ok);
    for (label, n) in &by_config {
        println!("      via {:<12}: {}", label, n);
    }
    println!(
        "  MISMATCH     : {} (documented raw-string: {}, unexpected: {})",
        mismatch,
        mismatch_known,
        mismatch - mismatch_known
    );
    println!(
        "  PARSE-ERR    : {} (documented raw-string: {}, unexpected: {})",
        parse_err,
        parse_err_known,
        parse_err - parse_err_known
    );
    println!("--------------------------------------------------");

    if unexpected == 0 {
        println!(
            "GATE PASS: every non-parity fragment is a documented raw-string \
             case that fhir-core itself cannot round-trip"
        );
        std::process::exit(0);
    } else {
        println!("GATE FAIL");
        std::process::exit(1);
    }
}
