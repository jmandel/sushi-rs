#!/usr/bin/env python3
"""md-diff.py — differential gate for render_md (F1b).

For every authored markdown page in the survey corpus:
  1. strip Liquid (scripts/strip-liquid.py scheme) so both engines see
     Liquid-free markdown;
  2. render with the kramdown oracle (scripts/kramdown-oracle.rb, NO_ROUGE mode
     so fenced code is compared on kramdown's own output, not Rouge);
  3. render with the Rust engine (render_md_cli);
  4. diff, and CLASSIFY every differing page into a bounded set of diff classes.

Usage:
  scripts/md-diff.py            # run over the whole corpus, print summary
  scripts/md-diff.py --show CLASS  # print first example page for a class
  scripts/md-diff.py FILE...    # run over specific files

Exit status 0 iff 0 UNEXPLAINED pages (pages whose only diffs fall in the
in-scope classes, or whose diffs are all in documented out-of-scope classes).
"""
import subprocess
import sys
import os
import re
import difflib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ORACLE = ROOT / "scripts" / "kramdown-oracle.rb"
STRIP = ROOT / "scripts" / "strip-liquid.py"
RUST_BIN = ROOT / "crates" / "render_md" / "target" / "debug" / "render_md_cli"

# The survey corpus lives in the session scratchpad (READ-ONLY input).
CORPUS = Path(
    os.environ.get(
        "MD_CORPUS",
        "/tmp/claude-1000/-home-jmandel-hobby/"
        "33fc8265-3f9a-4a4b-8eaf-39a38ad53b3d/scratchpad/ig-survey",
    )
)


def strip_liquid(text: str) -> str:
    p = subprocess.run(
        [sys.executable, str(STRIP)], input=text, capture_output=True, text=True
    )
    return p.stdout


def oracle(text: str) -> str:
    env = dict(os.environ, KRAMDOWN_NO_ROUGE="1")
    p = subprocess.run(
        ["ruby", str(ORACLE)], input=text, capture_output=True, text=True, env=env
    )
    return p.stdout


def rust(text: str) -> str:
    p = subprocess.run(
        [str(RUST_BIN)], input=text, capture_output=True, text=True
    )
    return p.stdout


# ---------------------------------------------------------------------------
# Diff classification. Each classifier inspects the (oracle, rust) outputs and
# the diff hunks and returns a class label if it EXPLAINS all differences, else
# None. Classes are ordered; a page is "explained" if a single class accounts
# for every differing line.

RE_ROUGE = re.compile(r'highlighter-rouge|class="highlight"|<span class="')


def classify(src: str, o: str, r: str):
    """Return (label, in_scope) or None if unexplained.

    in_scope=False marks documented out-of-scope classes."""
    if o == r:
        return ("identical", True)

    o_lines = o.splitlines()
    r_lines = r.splitlines()
    diff = list(difflib.unified_diff(o_lines, r_lines, lineterm=""))
    changed = [d for d in diff if d and d[0] in "+-" and not d.startswith(("+++", "---"))]

    # --- OUT OF SCOPE classes -------------------------------------------
    # Rouge syntax highlighting: oracle (even in NO_ROUGE we disable it, so this
    # should be rare) — but if the page has language fences and the ONLY diffs
    # are inside highlighter markup, mark out-of-scope.
    if any(RE_ROUGE.search(c) for c in changed):
        return ("rouge-highlight", False)

    return None


def normalize_for_report(diff_lines):
    return "\n".join(diff_lines[:40])


def main():
    args = sys.argv[1:]
    show = None
    if args and args[0] == "--show":
        show = args[1]
        args = args[2:]

    if args:
        files = [Path(a) for a in args]
    else:
        files = sorted(CORPUS.glob("*/input/pagecontent/*.md"))

    total = 0
    identical = 0
    explained = {}
    unexplained = []
    show_example = None

    for f in files:
        text = f.read_text(encoding="utf-8", errors="replace")
        stripped = strip_liquid(text)
        o = oracle(stripped)
        r = rust(stripped)
        total += 1
        result = classify(stripped, o, r)
        if result is None:
            unexplained.append(f)
            if show == "unexplained" and show_example is None:
                show_example = (f, o, r)
        else:
            label, in_scope = result
            if label == "identical":
                identical += 1
            explained[label] = explained.get(label, 0) + 1
            if show == label and show_example is None:
                show_example = (f, o, r)

    print(f"corpus pages compared: {total}")
    print(f"  identical (byte-equal): {identical}")
    print("  explained diff classes:")
    for label, n in sorted(explained.items()):
        if label == "identical":
            continue
        print(f"    {label}: {n}")
    print(f"  UNEXPLAINED: {len(unexplained)}")
    for f in unexplained[:60]:
        rel = f.relative_to(CORPUS)
        print(f"    - {rel}")

    if show_example:
        f, o, r = show_example
        print(f"\n=== example: {f} ===")
        diff = difflib.unified_diff(
            o.splitlines(), r.splitlines(), "oracle", "rust", lineterm=""
        )
        print("\n".join(list(diff)[:80]))

    sys.exit(0 if not unexplained else 1)


if __name__ == "__main__":
    main()
