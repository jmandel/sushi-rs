//! Lexer parity gate: every fixture's Rust token stream must EXACTLY match the
//! ANTLR oracle golden (kind, channel, text, line, col, start, stop).
//!
//! Goldens are produced by `harness/gen-lex-goldens.sh` (which calls
//! `harness/lex-oracle.cjs`). To add a fixture: drop `tests/fixtures/lex/NN.fsh`,
//! run the gen script, and this test picks it up automatically.

use std::fs;
use std::path::{Path, PathBuf};

use fsh_lexer_parser::lex_document;
use fsh_lexer_parser::token::{Channel, Token};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lex")
}
fn goldens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/lex")
}

#[derive(Debug, PartialEq, Eq)]
struct Row {
    kind: String,
    channel: String,
    text: String,
    line: i64,
    col: i64,
    start: i64,
    stop: i64,
}

fn row_from_token(t: &Token) -> Row {
    Row {
        kind: t.kind.name().to_string(),
        channel: match t.channel {
            Channel::Hidden => "HIDDEN".to_string(),
            Channel::Default => "0".to_string(),
        },
        text: t.text.clone(),
        line: t.line as i64,
        col: t.col as i64,
        start: t.start,
        stop: t.stop,
    }
}

fn row_from_golden(v: &serde_json::Value) -> Row {
    let channel = match &v["channel"] {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(), // number 0 -> "0"
    };
    Row {
        kind: v["type"].as_str().unwrap().to_string(),
        channel,
        text: v["text"].as_str().unwrap().to_string(),
        line: v["line"].as_i64().unwrap(),
        col: v["col"].as_i64().unwrap(),
        start: v["start"].as_i64().unwrap(),
        stop: v["stop"].as_i64().unwrap(),
    }
}

fn check_fixture(name: &str) -> Result<(), String> {
    let fsh = fs::read_to_string(fixtures_dir().join(format!("{name}.fsh")))
        .map_err(|e| format!("read fixture: {e}"))?;
    let golden_raw = fs::read_to_string(goldens_dir().join(format!("{name}.tokens.json")))
        .map_err(|e| format!("read golden: {e}"))?;
    let golden: serde_json::Value = serde_json::from_str(&golden_raw).unwrap();
    let expected: Vec<Row> = golden.as_array().unwrap().iter().map(row_from_golden).collect();

    let actual: Vec<Row> = lex_document(&fsh).iter().map(row_from_token).collect();

    if actual.len() != expected.len() {
        return Err(format!(
            "{name}: token count {} != expected {}\nfirst divergence dump:\n{}",
            actual.len(),
            expected.len(),
            first_diff(&actual, &expected)
        ));
    }
    for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        if a != e {
            return Err(format!("{name}: token #{i} mismatch\n  actual:   {a:?}\n  expected: {e:?}"));
        }
    }
    Ok(())
}

fn first_diff(a: &[Row], e: &[Row]) -> String {
    let n = a.len().max(e.len());
    let mut s = String::new();
    for i in 0..n {
        let av = a.get(i).map(|r| format!("{} {:?}", r.kind, r.text)).unwrap_or_else(|| "<none>".into());
        let ev = e.get(i).map(|r| format!("{} {:?}", r.kind, r.text)).unwrap_or_else(|| "<none>".into());
        let mark = if av == ev { "  " } else { "!=" };
        s.push_str(&format!("  [{i}] {mark} actual={av}  expected={ev}\n"));
        if i > 40 { break; }
    }
    s
}

macro_rules! parity_tests {
    ($($fn_name:ident => $fixture:literal),+ $(,)?) => {
        $(#[test] fn $fn_name() { if let Err(e) = check_fixture($fixture) { panic!("{e}"); } })+
    };
}

parity_tests! {
    profile_basic    => "01_profile_basic",
    indent_softindex => "02_indent_softindex",
    codes_refs       => "03_codes_refs",
    numbers_strings  => "04_numbers_strings",
    vs_cs            => "05_vs_cs",
    ruleset_insert   => "06_ruleset_insert",
    context_chars    => "07_context_chars",
    caret_regex      => "08_caret_regex",
}
