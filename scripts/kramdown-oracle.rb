#!/usr/bin/env ruby
# frozen_string_literal: true
#
# kramdown-oracle.rb — render markdown from stdin to HTML using kramdown with
# EXACTLY the options the HL7 FHIR IG Publisher's Jekyll config puts in play.
#
# This is the reference ("oracle") for the render_md Rust crate's differential
# gate (task F1b of docs/stock-template-renderer-plan.md).
#
# ---------------------------------------------------------------------------
# Jekyll config source of truth
# ---------------------------------------------------------------------------
# The stock FHIR IG template
# (scratchpad/ig-template-base/config/_config.yml) sets ONLY these kramdown
# options:
#
#   kramdown:
#     toc_levels:    1..3
#     smart_quotes: ["apos", "apos", "quot", "quot"]
#     typographic_symbols:
#       "laquo": "<<"
#       "raquo": ">>"
#
# Everything else is Jekyll's own kramdown defaults. Jekyll (>= 3.x) forces
# `input: GFM` via the `jekyll-kramdown` bridge: Jekyll sets the kramdown
# `input` to "GFM" by default (Jekyll's default `markdown_ext`/`kramdown`
# config carries `input: GFM`), and ships kramdown-parser-gfm. That is why IG
# authored pages rely on GFM pipe tables and fenced code even though the
# template's _config.yml never names GFM. We therefore run kramdown with the
# GFM parser, Jekyll's kramdown defaults, plus the three template overrides.
#
# Reference for Jekyll defaults:
#   Jekyll::Configuration::DEFAULTS[:kramdown] =>
#     { auto_ids: true, toc_levels: (1..6), entity_output: 'as_char',
#       smart_quotes: 'lsquo,rsquo,ldquo,rdquo', input: 'GFM',
#       hard_wrap: false, guess_lang: true, footnote_nr: 1,
#       show_warnings: false }
#   (jekyll/lib/jekyll/configuration.rb)
# ---------------------------------------------------------------------------

require "kramdown"
require "kramdown-parser-gfm"

# ---------------------------------------------------------------------------
# Syntax highlighting boundary.
#
# Jekyll's default is `syntax_highlighter: rouge`, so a fenced code block with
# a language (```json) is rendered by kramdown into full Rouge token markup
# (<div class="language-json highlighter-rouge">...<span class="...">). Rouge
# is a standalone syntax-highlighting LIBRARY, not part of markdown semantics;
# porting Rouge lexers to Rust is out of scope for the render_md kramdown
# engine (task F1b). The render_md crate emits kramdown's own, un-highlighted
# fence form (`<pre><code class="language-X">...`).
#
# So the oracle offers both:
#   default            -> Jekyll parity (rouge on): the ground truth Jekyll
#                         emits. Used to MEASURE the rouge boundary.
#   KRAMDOWN_NO_ROUGE=1 -> syntax_highlighter disabled: the baseline the Rust
#                         engine actually targets for code blocks.
# The differential harness runs the NO_ROUGE oracle for the in-scope gate and
# classifies any rouge-only fence divergence as a documented out-of-scope diff.
# ---------------------------------------------------------------------------

# Jekyll's default kramdown options, merged with the FHIR template overrides.
OPTIONS = {
  # ---- Jekyll defaults (jekyll/lib/jekyll/configuration.rb DEFAULTS) ----
  input: "GFM",
  auto_ids: true,
  entity_output: "as_char",
  hard_wrap: false,
  footnote_nr: 1,
  show_warnings: false,
  # smart_quotes default in Jekyll is lsquo,rsquo,ldquo,rdquo, but the
  # template overrides it below.

  # ---- FHIR ig-template-base/config/_config.yml overrides ----
  toc_levels: "1..3",
  smart_quotes: %w[apos apos quot quot],
  typographic_symbols: { "laquo" => "<<", "raquo" => ">>" },
}.dup

if ENV["KRAMDOWN_NO_ROUGE"] == "1"
  OPTIONS[:syntax_highlighter] = nil
end
OPTIONS.freeze

input = $stdin.read
doc = Kramdown::Document.new(input, OPTIONS)
$stdout.write(doc.to_html)
