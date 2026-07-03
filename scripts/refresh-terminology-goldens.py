#!/usr/bin/env python3
"""Deliberate maintainer refresh of the tier-1 terminology oracle goldens.

ONE $expand request per fixture against tx.fhir.org/r4 (POST), committing each
response WITH its expansion.parameter (cycle-plan §3 cache discipline). CI never
runs this; a maintainer runs it and commits the goldens.

Usage:  python3 scripts/refresh-terminology-goldens.py
"""
import json
import os
import sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
FIX = os.path.join(ROOT, "crates/compiler/tests/fixtures/terminology")
GOLD = os.path.join(ROOT, "crates/compiler/tests/goldens/terminology")
TX = "https://tx.fhir.org/r4/ValueSet/$expand"

# (golden name, valueset fixture, [supporting CodeSystem fixtures to supply])
FIXTURES = [
    # cycle IG (built with rust_sushi): local complete CS + external SNOMED enum
    ("cycle-menstrual-flow", "cycle-menstrual-flow.vs.json", ["cycle.cs.json"]),
    ("cycle-common-tracker-symptoms", "cycle-common-tracker-symptoms.vs.json", []),
    # harvested corpus: enumerated external SNOMED concept lists
    ("ips-pregnancy-status", "ips-pregnancy-status.vs.json", []),
    ("mcode-condition-status-trend", "mcode-condition-status-trend.vs.json", []),
    # synthetic local-CS logic, supplied to tx via tx-resource
    ("syn-isa-bear", "syn-isa-bear.vs.json", ["zoo.cs.json"]),
    ("syn-descendent-animal", "syn-descendent-animal.vs.json", ["zoo.cs.json"]),
    ("syn-whole-zoo", "syn-whole-zoo.vs.json", ["zoo.cs.json"]),
    ("syn-prop-carnivore", "syn-prop-carnivore.vs.json", ["zoo.cs.json"]),
    ("syn-enum-exclude", "syn-enum-exclude.vs.json", ["zoo.cs.json"]),
]


def load(name):
    with open(os.path.join(FIX, name)) as f:
        return json.load(f)


def expand(vs, css):
    params = [{"name": "valueSet", "resource": vs}]
    # Force server not to add designations/definitions we don't model.
    params.append({"name": "includeDefinition", "valueBoolean": False})
    params.append({"name": "activeOnly", "valueBoolean": False})
    for cs in css:
        params.append({"name": "tx-resource", "resource": cs})
    body = json.dumps({"resourceType": "Parameters", "parameter": params}).encode()
    req = urllib.request.Request(
        TX, data=body,
        headers={"Content-Type": "application/fhir+json",
                 "Accept": "application/fhir+json"},
        method="POST")
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.load(resp)


def main():
    os.makedirs(GOLD, exist_ok=True)
    for name, vsfile, csfiles in FIXTURES:
        vs = load(vsfile)
        css = [load(c) for c in csfiles]
        print(f"-> {name} ... ", end="", flush=True)
        result = expand(vs, css)
        rt = result.get("resourceType")
        if rt != "ValueSet":
            print(f"FAIL: got {rt}: {json.dumps(result)[:300]}")
            sys.exit(1)
        out = os.path.join(GOLD, f"{name}.golden.json")
        with open(out, "w") as f:
            json.dump(result, f, indent=2, ensure_ascii=False)
            f.write("\n")
        exp = result.get("expansion", {})
        print(f"total={exp.get('total')} committed {os.path.relpath(out, ROOT)}")


if __name__ == "__main__":
    main()
