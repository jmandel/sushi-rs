#!/usr/bin/env python3
"""Cluster unexplained diffs by a signature of the changed lines, to reveal the
biggest systematic categories."""
import subprocess, sys, os, difflib, re
from pathlib import Path
from collections import Counter, defaultdict

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
import importlib.util
spec = importlib.util.spec_from_file_location("mddiff", ROOT / "scripts" / "md-diff.py")
md = importlib.util.module_from_spec(spec); spec.loader.exec_module(md)

files = sorted(md.CORPUS.glob("*/input/pagecontent/*.md"))
if len(sys.argv) > 1:
    files = [Path(a) for a in sys.argv[1:]]

sig_counter = Counter()
sig_example = {}

def signature(o_line, r_line):
    def norm(s):
        s = re.sub(r'id="[^"]*"', 'id="X"', s)
        s = re.sub(r'href="[^"]*"', 'href="X"', s)
        s = re.sub(r'\d+', 'N', s)
        s = re.sub(r'>[^<]+<', '>T<', s)
        return s.strip()[:80]
    return (norm(o_line), norm(r_line))

for f in files:
    text = f.read_text(encoding="utf-8", errors="replace")
    stripped = md.strip_liquid(text)
    o = md.oracle(stripped); r = md.rust(stripped)
    if o == r: continue
    if md.classify(stripped, o, r) is not None: continue
    sm = difflib.SequenceMatcher(None, o.splitlines(), r.splitlines())
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal": continue
        ol = o.splitlines()[i1:i2]
        rl = r.splitlines()[j1:j2]
        omax = max(len(ol), 1);
        for k in range(max(len(ol), len(rl))):
            oo = ol[k] if k < len(ol) else "<<none>>"
            rr = rl[k] if k < len(rl) else "<<none>>"
            sig = signature(oo, rr)
            sig_counter[sig] += 1
            if sig not in sig_example:
                sig_example[sig] = (str(f.relative_to(md.CORPUS)), oo, rr)

print("Top diff signatures (oracle-line | rust-line):")
for sig, n in sig_counter.most_common(30):
    f, oo, rr = sig_example[sig]
    print(f"\n[{n}x] {f}")
    print(f"  ORA: {oo!r}")
    print(f"  RST: {rr!r}")
