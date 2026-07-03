//! Feature-class fixture tests.
//!
//! Each `tests/fixtures/<name>.md` is rendered by render_md and compared to its
//! committed `<name>.html` golden. The goldens were produced by the real
//! kramdown oracle (`scripts/kramdown-oracle.rb`, NO_ROUGE mode — see the F1b
//! report) and are NEVER edited to make the engine pass.
//!
//! Regenerate goldens (only when kramdown behavior is the reference of record):
//!   for f in crates/render_md/tests/fixtures/*.md; do
//!     KRAMDOWN_NO_ROUGE=1 ruby scripts/kramdown-oracle.rb < "$f" > "${f%.md}.html"
//!   done

use std::fs;
use std::path::Path;

fn fixtures_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn check(name: &str) {
    let dir = fixtures_dir();
    let md = fs::read_to_string(dir.join(format!("{name}.md"))).expect("read .md");
    let golden = fs::read_to_string(dir.join(format!("{name}.html"))).expect("read golden");
    let got = render_md::render(&md);
    assert_eq!(
        got, golden,
        "\n--- render_md output for {name} did not match kramdown golden ---\n\
         GOT:\n{got}\nEXPECTED:\n{golden}"
    );
}

#[test]
fn headings_autoid() {
    check("headings_autoid");
}

#[test]
fn ial() {
    check("ial");
}

#[test]
fn tables() {
    check("tables");
}

#[test]
fn fenced_code() {
    check("fenced_code");
}

#[test]
fn markdown_re_entry() {
    check("markdown_re_entry");
}

#[test]
fn raw_html_passthrough() {
    check("raw_html_passthrough");
}

#[test]
fn toc() {
    check("toc");
}

#[test]
fn typography() {
    check("typography");
}

#[test]
fn lists() {
    check("lists");
}

#[test]
fn inline() {
    check("inline");
}

#[test]
fn footnotes() {
    check("footnotes");
}
