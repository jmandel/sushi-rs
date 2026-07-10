//! ORACLE gate for `compiler::terminology::expand_enumerable` (editor spec §6
//! tier 1, cycle-plan §3).
//!
//! For every fixture, the tier-1 evaluator's expansion is compared against the
//! COMMITTED tx.fhir.org `$expand` golden (fetched deliberately by
//! `scripts/refresh-terminology-goldens.py`; server + date in
//! `tests/fixtures/terminology/README.md`). CI never calls tx.
//!
//! ## Explicit normalizations (never silent — see the fixtures README)
//! 1. **Hierarchy shape.** tx returns a NESTED `contains` tree for hierarchical
//!    code systems; the evaluator returns a flat list. Both sides are flattened
//!    (recursively collecting every `contains[*]`) before comparison — the
//!    evaluator's flat form is spec-valid (`contains` MAY be nested or flat).
//! 2. **Ordering.** tx uses authored/insertion order; the evaluator uses a
//!    stable `(system, code)` sort. Both sides are sorted by `(system, code)`.
//! 3. **Displays on EXTERNAL systems.** For SNOMED etc. the tier-1 evaluator has
//!    no CS to look up displays, so it passes AUTHORED displays through; tx
//!    substitutes the server's canonical display. The gate asserts the CODE SET
//!    is identical and reports display differences on external systems as
//!    informational (EXPECTED — does not fail). For LOCAL/synthetic systems
//!    supplied to both sides, displays MUST match exactly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use compiler::terminology::{expand_enumerable, MapResolver};
use serde_json::Value as J;

fn dir(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(sub)
        .join("terminology")
}

fn load(sub: &str, name: &str) -> J {
    let p = dir(sub).join(name);
    serde_json::from_str(
        &std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display())),
    )
    .unwrap()
}

/// Recursively flatten an expansion's `contains` into `(system, code) -> display`.
fn flatten_contains(contains: &J, out: &mut BTreeMap<(String, String), Option<String>>) {
    let Some(arr) = contains.as_array() else {
        return;
    };
    for c in arr {
        let system = c
            .get("system")
            .and_then(J::as_str)
            .unwrap_or("")
            .to_string();
        let code = c.get("code").and_then(J::as_str).unwrap_or("").to_string();
        let display = c.get("display").and_then(J::as_str).map(String::from);
        // A node that is purely a grouper (abstract, no leaf meaning) still has a
        // code here; tx includes it in `total`, so we keep it.
        if !code.is_empty() {
            out.entry((system, code)).or_insert(display);
        }
        if let Some(children) = c.get("contains") {
            flatten_contains(children, out);
        }
    }
}

fn golden_members(golden: &J) -> BTreeMap<(String, String), Option<String>> {
    let mut out = BTreeMap::new();
    if let Some(contains) = golden.get("expansion").and_then(|e| e.get("contains")) {
        flatten_contains(contains, &mut out);
    }
    out
}

fn eval_members(exp_json: &J) -> BTreeMap<(String, String), Option<String>> {
    let mut out = BTreeMap::new();
    if let Some(contains) = exp_json.get("contains") {
        flatten_contains(contains, &mut out);
    }
    out
}

/// One oracle case: the VS fixture, the local CodeSystems the evaluator resolves,
/// and whether displays are authored-locally (compared exactly) or come from an
/// EXTERNAL system (display diffs are informational).
struct Case {
    golden: &'static str,
    vs: &'static str,
    css: &'static [&'static str],
    /// systems whose displays are authored locally (compared exactly). Any code
    /// whose system is NOT in this list has its display treated as informational.
    local_systems: &'static [&'static str],
}

const CASES: &[Case] = &[
    Case {
        golden: "cycle-menstrual-flow",
        vs: "cycle-menstrual-flow.vs.json",
        css: &["cycle.cs.json"],
        local_systems: &["https://cycle.fhir.me/CodeSystem/cycle"],
    },
    Case {
        golden: "cycle-common-tracker-symptoms",
        vs: "cycle-common-tracker-symptoms.vs.json",
        css: &[],
        local_systems: &[],
    }, // SNOMED external
    Case {
        golden: "ips-pregnancy-status",
        vs: "ips-pregnancy-status.vs.json",
        css: &[],
        local_systems: &[],
    },
    Case {
        golden: "mcode-condition-status-trend",
        vs: "mcode-condition-status-trend.vs.json",
        css: &[],
        local_systems: &[],
    },
    Case {
        golden: "syn-isa-bear",
        vs: "syn-isa-bear.vs.json",
        css: &["zoo.cs.json"],
        local_systems: &["https://ex.org/zoo"],
    },
    Case {
        golden: "syn-descendent-animal",
        vs: "syn-descendent-animal.vs.json",
        css: &["zoo.cs.json"],
        local_systems: &["https://ex.org/zoo"],
    },
    Case {
        golden: "syn-whole-zoo",
        vs: "syn-whole-zoo.vs.json",
        css: &["zoo.cs.json"],
        local_systems: &["https://ex.org/zoo"],
    },
    Case {
        golden: "syn-prop-carnivore",
        vs: "syn-prop-carnivore.vs.json",
        css: &["zoo.cs.json"],
        local_systems: &["https://ex.org/zoo"],
    },
    Case {
        golden: "syn-enum-exclude",
        vs: "syn-enum-exclude.vs.json",
        css: &["zoo.cs.json"],
        local_systems: &["https://ex.org/zoo"],
    },
];

fn run_case(case: &Case) -> Result<Vec<String>, String> {
    let vs = load("fixtures", case.vs);
    let mut resolver = MapResolver::new();
    for cs in case.css {
        resolver.insert(load("fixtures", cs));
    }
    let exp = expand_enumerable(&vs, &resolver).map_err(|e| {
        format!(
            "{}: evaluator REFUSED an enumerable fixture: {e}",
            case.golden
        )
    })?;
    let mine = eval_members(&exp.to_expansion_json());

    let golden = load("goldens", &format!("{}.golden.json", case.golden));
    let theirs = golden_members(&golden);

    // (1) CODE SET must match exactly (the shared domain).
    let mine_codes: Vec<_> = mine.keys().cloned().collect();
    let their_codes: Vec<_> = theirs.keys().cloned().collect();
    if mine_codes != their_codes {
        let only_mine: Vec<_> = mine.keys().filter(|k| !theirs.contains_key(*k)).collect();
        let only_theirs: Vec<_> = theirs.keys().filter(|k| !mine.contains_key(*k)).collect();
        return Err(format!(
            "{}: CODE SET differs.\n  only in evaluator: {:?}\n  only in tx golden: {:?}",
            case.golden, only_mine, only_theirs
        ));
    }

    // (2) Displays: exact for local systems, informational for external.
    let mut notes = Vec::new();
    for (key, my_disp) in &mine {
        let their_disp = theirs.get(key).unwrap();
        let (system, code) = key;
        let is_local = case.local_systems.iter().any(|s| s == system);
        if my_disp != their_disp {
            if is_local {
                return Err(format!(
                    "{}: LOCAL display mismatch for {code}: evaluator={:?} tx={:?}",
                    case.golden, my_disp, their_disp
                ));
            } else {
                notes.push(format!(
                    "  display differs (external system, informational): {code} authored={:?} tx={:?}",
                    my_disp, their_disp
                ));
            }
        }
    }
    Ok(notes)
}

#[test]
fn tier1_evaluator_matches_tx_oracle() {
    let mut failures = Vec::new();
    for case in CASES {
        match run_case(case) {
            Ok(notes) => {
                eprintln!(
                    "[PASS] {} ({} members)",
                    case.golden,
                    golden_members(&load("goldens", &format!("{}.golden.json", case.golden))).len()
                );
                for n in notes {
                    eprintln!("       {n}");
                }
            }
            Err(e) => failures.push(e),
        }
    }
    assert!(
        failures.is_empty(),
        "oracle gate failures:\n{}",
        failures.join("\n")
    );
}
