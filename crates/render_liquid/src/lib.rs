//! render_liquid — a just-enough Jekyll/Liquid (T1+T2) engine at Publisher
//! parity, for the stock-template renderer (docs/stock-template-renderer-plan.md
//! task F1c).
//!
//! # Scope
//! This engine reproduces the surface measured in
//! docs/ig-jekyll-surface-survey.md, tiers T1 + T2 (US-Core layer IN scope per
//! Josh's 2026-07-03 scope decision):
//!
//! * **T1**: `{% include %}` (registry-resolved), `assign`, `capture`, `raw`,
//!   `comment`, `{{ output }}` with the ~17 string filters, and
//!   `site.data.*` / `page.*` / `include.*` variable resolution through a
//!   pluggable [`DataProvider`].
//! * **T2**: `{% for %}` (incl. `forloop.*`, `offset:`, `limit:`, `reversed`,
//!   `break`/`continue`), full `if`/`elsif`/`else`/`unless` with
//!   `==`/`!=`/`<`/`>`/`<=`/`>=`/`contains`/`.size` and `and`/`or`, the array
//!   filters `split | where | sort | uniq | map | join | size | first | last |
//!   reverse | compact | concat`, and parameterized includes
//!   (`{% include x.md k="v" %}` + `include.k`).
//!
//! Behavior matches **Jekyll 4.4.1's Liquid 4.0.4** (Jekyll wins over spec on
//! divergence). See `scripts/liquid-oracle.rb` for the reference and
//! `tests/differential.rs` for the gate.
//!
//! # The {% raw %} Publisher quirk (registry)
//! The Java IG Publisher evaluates `{% fragment %}` / `{% include %}` even
//! inside `{% raw %}` (survey nasty #4; cycle liquid.ts:213-220). This engine
//! does the CORRECT thing by default (raw body emitted verbatim); set
//! [`Options::publisher_raw_quirk`] to reproduce the Publisher's wart, in which
//! case a `raw` body is re-parsed and evaluated.

mod ast;
mod filters;
mod lexer;
mod parser;
mod provider;
mod render;
mod value;

pub use provider::{DataProvider, JsonProvider};
pub use render::Options;
pub use value::{OrderedMap, Value};

/// Parse + render `src` against `provider`, seeding the given top-level context
/// variables (e.g. `page`, and for standalone includes an `include` hash).
///
/// Returns the rendered string. Parse errors return the error text prefixed
/// with `LIQUID PARSE ERROR:` (Jekyll in warn mode surfaces a similar marker);
/// callers on the gate treat any such marker as a diff.
pub fn render_with(
    src: &str,
    provider: &dyn DataProvider,
    globals: &[(&str, Value)],
    opts: Options,
) -> String {
    let tpl = match parser::parse(src) {
        Ok(t) => t,
        Err(e) => return format!("LIQUID PARSE ERROR: {}", e.0),
    };
    let mut r = render::Renderer::new(provider, opts);
    for (k, v) in globals {
        r.set_global(k, v.clone());
    }
    r.render(&tpl)
}

/// The set of tags this engine recognizes, and how it handles each. Used for
/// documentation and to fail-loud on unmodeled constructs when a host wants
/// that policy.
pub fn tag_registry() -> Vec<TagDoc> {
    use TagKind::*;
    vec![
        TagDoc { name: "assign", kind: Supported, note: "T1: single `=` RHS with filters; writes to root scope (persists past blocks), matching Liquid." },
        TagDoc { name: "capture", kind: Supported, note: "T1: captures rendered body into a variable (root scope)." },
        TagDoc { name: "comment", kind: Supported, note: "T1: body discarded." },
        TagDoc { name: "raw", kind: Supported, note: "T1: body emitted VERBATIM by default; with Options::publisher_raw_quirk the body is re-parsed+evaluated to mirror the Java Publisher (survey nasty #4)." },
        TagDoc { name: "include", kind: Supported, note: "T1/T2: registry-resolved via DataProvider::include_source; supports parameterized includes `k=\"v\"` exposed as include.*, dynamic `{{path}}.md` names, and recursive re-render." },
        TagDoc { name: "include_relative", kind: Supported, note: "Treated identically to include (corpus does not distinguish load paths)." },
        TagDoc { name: "if", kind: Supported, note: "T2: elsif/else, ==/!=/<>/</>/<=/>=, contains, .size operands, and/or (right-assoc, no precedence — Jekyll/Liquid)." },
        TagDoc { name: "unless", kind: Supported, note: "T2: desugars to negated if." },
        TagDoc { name: "for", kind: Supported, note: "T2: forloop.{index,index0,rindex,rindex0,first,last,length}; offset:/limit:/reversed; break/continue; for-else." },
        TagDoc { name: "break", kind: Supported, note: "T2." },
        TagDoc { name: "continue", kind: Supported, note: "T2." },
        TagDoc { name: "increment", kind: Supported, note: "Liquid counter (separate namespace, prints pre-increment)." },
        TagDoc { name: "decrement", kind: Supported, note: "Liquid counter (prints post-decrement, starts at -1)." },
        TagDoc { name: "lang-fragment", kind: Passthrough, note: "Localization tag: emits nothing (host may register a handler); out-of-core." },
        TagDoc { name: "fragment", kind: Passthrough, note: "Publisher fragment tag: emits nothing here (F4 fragment store handles it); QUIRK: Publisher evaluates it inside raw — see publisher_raw_quirk." },
        TagDoc { name: "sql", kind: OutOfScope, note: "IG-Guidance-only documented feature; emits nothing (cycle-specific extension per survey (d))." },
        TagDoc { name: "case/when", kind: OutOfScope, note: "Measured ZERO in corpus (survey b) — not implemented; would parse as UnknownTag (emits nothing)." },
        TagDoc { name: "highlight/tablerow/cycle", kind: OutOfScope, note: "Measured ZERO in corpus — not implemented." },
        TagDoc { name: "layout", kind: OutOfScope, note: "Layout inheritance is the `page` crate's job (F5), not Liquid-core; out of scope here." },
    ]
}

pub struct TagDoc {
    pub name: &'static str,
    pub kind: TagKind,
    pub note: &'static str,
}

#[derive(Debug, PartialEq)]
pub enum TagKind {
    Supported,
    Passthrough,
    OutOfScope,
}
