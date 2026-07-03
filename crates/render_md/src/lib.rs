//! render_md — a kramdown-subset markdown engine that reproduces the HTML the
//! HL7 FHIR IG Publisher's Jekyll pipeline produces for authored IG pages.
//!
//! # Target semantics
//!
//! The target is *kramdown as Jekyll/the FHIR IG Publisher configures it*, not
//! generic Markdown. Concretely (see `scripts/kramdown-oracle.rb` for the
//! byte-for-byte reference and the jekyll-config citation):
//!
//! * `input: GFM` — GFM pipe tables, fenced code, tilde/backtick fences.
//! * `auto_ids: true` — headings get kramdown-algorithm ids.
//! * `entity_output: as_char` — `&`,`<`,`>` escaped; quotes left literal.
//! * `smart_quotes: [apos, apos, quot, quot]` — the FHIR template maps all
//!   curly quotes back to ASCII `'` and `"` (i.e. smart quotes *disabled*).
//! * `typographic_symbols: {laquo: "<<", raquo: ">>"}`.
//! * IALs: block IAL before/after a block, span IAL after a span element,
//!   list-item-start IAL, heading `{#id}` — with kramdown's attribute
//!   emission order.
//! * `{:toc}` / `no_toc`, footnotes, indented + fenced code (with kramdown's
//!   lazy-join quirks), pipe tables (headerless, footer, code-span-aware cell
//!   split, nbsp/space cell padding), GFM task lists, link reference
//!   definitions, `markdown="1"/0/span/block"` HTML re-entry per content
//!   model, stateful `{::options parse_block_html}`, raw-HTML passthrough
//!   with kramdown's re-serialization (innermost-close matching, auto-close,
//!   attribute normalization, smart-amp).
//!
//! Differential gate (task F1b): all 459 authored `input/pagecontent/*.md`
//! pages of the 16-IG survey corpus render BYTE-IDENTICAL to kramdown 2.5.0 +
//! kramdown-parser-gfm 1.1.0 under the publisher's Jekyll config
//! (`scripts/md-diff.py`; Liquid stripped first — that layer belongs to the
//! `liquid` crate).
//!
//! # Out of scope (documented boundary — see the F1b report)
//!
//! * Rouge syntax highlighting of ```` ```lang ````/`~~~lang` fences. render_md
//!   emits kramdown's own `<pre><code class="language-X">` form; Rouge token
//!   markup is a separate library layered on by Jekyll
//!   (`syntax_highlighter: rouge`). 20 corpus pages carry language fences
//!   whose full-Jekyll output differs only inside the highlighter markup;
//!   this is confronted at F5 with real page goldens.
//! * kramdown features not exercised by the survey corpus (definition lists,
//!   math, abbreviations, `{::comment}`/`{::nomarkdown}` bodies, EOB markers,
//!   ALDs `{:name: ...}`).

mod block;
mod ial;
mod inline;
mod render;
mod util;

pub use render::{render, Options};
