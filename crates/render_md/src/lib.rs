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
//! * IAL `{: .class #id key="v"}` on the line after a block (or a span IAL).
//! * `{:toc}` / `no_toc`, footnotes, `markdown="1"` HTML-block re-entry, and
//!   raw-HTML passthrough.
//!
//! # Out of scope (documented boundary — see tests/README and the F1b report)
//!
//! * Rouge syntax highlighting of `~~~lang` / ```lang` fences. render_md emits
//!   kramdown's own `<pre><code class="language-X">` form; Rouge token markup
//!   is a separate library, gated as an out-of-scope diff class.
//! * kramdown features not exercised by the survey corpus (definition lists,
//!   math, `abbreviations`, `{::comment}` extensions, end-of-block `^`, etc.).

mod block;
mod ial;
mod inline;
mod render;
mod util;

pub use render::{render, Options};
