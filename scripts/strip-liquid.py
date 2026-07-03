#!/usr/bin/env python3
"""strip-liquid.py — replace Liquid constructs with stable inert placeholders.

Liquid evaluation belongs to the `liquid` crate (T1+T2), NOT to render_md.
For the render_md differential gate we must feed BOTH engines (kramdown oracle
and the Rust engine) markdown that contains NO Liquid, so that any diff is
attributable to markdown semantics alone.

Placeholder scheme (documented, deterministic, markdown-inert):

  {% ... %}   (tag, incl. whitespace-control {%- -%})
              -> the tag is REMOVED entirely. Rationale: Liquid tags such as
                 {% include %}, {% if %}, {% for %}, {% assign %}, {% raw %}
                 emit no literal text of their own; at render time they are
                 replaced by their evaluated body or by nothing. Removing the
                 tag marker (but keeping surrounding authored text) is the
                 closest Liquid-free approximation and keeps block structure
                 intact. Whole-line tags leave a blank line, matching how the
                 evaluated include/control-flow output would sit between
                 blocks.

  {{ ... }}   (output/interpolation)
              -> replaced with the placeholder token  LIQVAR
                 (a bare alphanumeric word: markdown-inert, no smart-quote /
                 IAL / table interaction). Rationale: an interpolation expands
                 to some inline string; a neutral word preserves inline flow
                 without introducing markdown-significant characters.

This is intentionally lossy but LOSSLESS w.r.t. markdown block/inline
STRUCTURE, which is all the render_md gate cares about. Pages whose markdown
structure genuinely depends on Liquid-produced text (e.g. a pipe table whose
rows are emitted by a {% for %}) are flagged separately by the harness and
excluded from the "unexplained diff" count (documented boundary).
"""
import re
import sys

# Match {% ... %} including {%- ... -%} whitespace-control variants, non-greedy.
TAG = re.compile(r"\{%-?.*?-?%\}", re.DOTALL)
# Match {{ ... }} output expressions, non-greedy.
OUT = re.compile(r"\{\{.*?\}\}", re.DOTALL)


def strip(text: str) -> str:
    # Remove tags first (they may contain {{ }} inside, e.g. dynamic includes).
    text = TAG.sub("", text)
    text = OUT.sub("LIQVAR", text)
    return text


def main() -> None:
    data = sys.stdin.read()
    sys.stdout.write(strip(data))


if __name__ == "__main__":
    main()
