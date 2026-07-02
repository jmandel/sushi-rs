# The FHIR StructureDefinition Snapshot Algorithm — A Specification

**Status:** Working draft. Reverse-engineered from the HL7 FHIR "core" reference
implementation (`org.hl7.fhir.r5`) and the IG Publisher, which together are the de-facto
normative implementation. This document specifies precisely what the FHIR specification itself
describes only in prose.

**Source anchors.** Normative statements cite the implementation as provenance, not as part of
the requirements:
- `PU:n` → `fhir-core .../conformance/profile/ProfileUtilities.java:n`
- `PPP:n` → `fhir-core .../conformance/profile/ProfilePathProcessor.java:n`
- `PB:n` → `ig-publisher .../publisher/PublisherBase.java:n`
- `PIL:n` → `ig-publisher .../publisher/PublisherIGLoader.java:n`

**Conformance language.** MUST, MUST NOT, SHALL, SHOULD, MAY are per RFC 2119/8174.

---

## How to read this document

The single most important thing to understand — and the thing FHIR's own docs never say
plainly — is that **"generating a snapshot" is really two different jobs done by two
different layers:**

> **Layer A — the structural algorithm.** A self-contained, policy-free function that merges a
> profile's *differential* onto its base's *snapshot* to produce a complete element list. Same
> inputs → same output. This is "the snapshot algorithm" proper.
>
> **Layer B — the policy & orchestration passes.** Everything the *tooling* (the IG Publisher)
> does *around* that function: deciding when to run it, feeding it context, and then **editing
> the result** to apply project policy — most notably **pinning canonical URLs to specific
> versions** (`url|1.2.3`). These are separate passes over the finished artifact.

The document is laid out around that split, and within each layer around its **passes**,
because that is how the real system is built:

- **Part 0** — the architecture: the two layers, the pass pipeline, and exactly where the
  boundary sits. *Read this first; it reframes everything.*
- **Part I** — the approachable conceptual model of Layer A (what a snapshot is, the core idea).
- **Part II** — terminology and notation.
- **Part III** — Layer A specified normatively, pass by pass.
- **Part IV** — Layer B specified: the policy passes, with version-pinning as the worked example.
- **Roadmap** — deep-dive parts for each Layer-A subsystem.

A reader can stop after Part I and have a correct mental model; each later part adds precision.

---

# Part 0 — Architecture: Two Layers and the Boundary

## 0.1 The big picture

When you look at a published snapshot and ask "how did this element list come to be?", the
answer is a **chain of passes**, not a single function call:

```
        ┌─────────────────────────── LAYER B: policy & orchestration (IG Publisher) ──────────────────────────┐
        │                                                                                                       │
  load resources ──▶ install context ──▶ [ LAYER A: structural algorithm ] ──▶ pin canonicals ──▶ narrative ──▶ validate ──▶ …
                     (expansion params)    generateSnapshot(base, derived)      url → url|x.y.z
        │                                                                                                       │
        └───────────────────────────────────────────────────────────────────────────────────────────────────-┘
```

Layer A is the box in the middle. Everything else is Layer B. The crucial, non-obvious facts:

1. **Layer A never pins versions.** It copies every canonical reference (`type.profile`,
   `binding.valueSet`, extension URLs, …) through **verbatim**, as opaque strings.
2. **The `|version` you see in a published snapshot is written by a *later* Layer-B pass**, not
   by the algorithm.
3. **The snapshot is generated once** (idempotent), then **post-edited in place** by policy
   passes. It is *not* regenerated repeatedly to accumulate policy.

## 0.2 Layer A — the structural algorithm (what it is)

Layer A is `ProfileUtilities.generateSnapshot` (PU:740). Its contract:

- **Input:** a `base` StructureDefinition (with a complete snapshot), a `derived`
  StructureDefinition (with a differential), and a small set of declared **configuration knobs**
  (§2.7) plus a **resolution context** (the set of loaded StructureDefinitions/ValueSets it may
  look things up in).
- **Output:** `derived.snapshot`, the complete element list.
- **Guarantees:** deterministic and **policy-free**. Given the same inputs, knobs, and context,
  it always yields the same snapshot. It encodes *FHIR's structural rules* (inheritance,
  slicing, type expansion, contentReference) and nothing about any particular project's
  publishing choices.

**What Layer A deliberately does NOT do** (all of these are Layer B):
- It does not pin or normalize canonical versions.
- It does not generate narrative.
- It does not read IG configuration files or decide *which* resources to process.
- It does not validate instances (it validates only its own structural output, §3.6).

**The one value Layer A itself writes about versions:** a single `EXT_VERSION_BASE` extension
recording the *base's* business version (PU:1092). That is bookkeeping, not pinning.

**Honesty about "pure":** Layer A is policy-free and deterministic, but it is *parameterized*
by the resolution context — it **reads** from the loaded package set to resolve and expand
types and value sets. So it is a pure function of *(base, derived, knobs, context)*. It also has
read-then-write effects on `derived` (it sets `derived.snapshot`). "Pure" here means *policy-free
and reproducible*, not side-effect-free in the strict sense.

## 0.3 Layer B — policy & orchestration (what it is)

Layer B is the IG Publisher's loading pipeline. It owns everything that depends on *project
configuration* or *the wider build*. Its responsibilities:

- **Decide what and when:** iterate the resources, skip already-processed ones, order the work.
- **Install context:** load packages, and — importantly — push **expansion parameters** into
  the terminology context *before* Layer A runs (the one place config reaches into Layer A;
  §0.4).
- **Invoke Layer A** once per StructureDefinition.
- **Apply policy passes** to the finished resource, each of which may *mutate the snapshot in
  place*:
  - **canonical version pinning** (the headline example; §4.4),
  - narrative regeneration, instance validation, provenance, cleanup, … (§4.5).

Layer B's policy is driven by IG configuration. For pinning, that is the `pin-canonicals`
parameter with values `pin-none` / `pin-all` / `pin-multiples`, plus an optional `pin-manifest`
(PIL:739).

## 0.4 How they fit — the pipeline and the single coupling

The conformance-loading phase (`loadConformance1`, PIL:4025) is the join point:

```
loadConformance1():
    load metadata resources                                    // PIL:4034
    if pin-manifest configured:
        install its Parameters as context expansion parameters // PIL:4037-4041  ← THE COUPLING
    generateSnapshots():                                       // PIL:4043
        for each StructureDefinition sd:
            if not sd.isSnapshotted:
                generateSnapshot(...)            // ── LAYER A ──   PB:908   (idempotent, PB:906/761)
            checkCanonicalsForVersions(sd, ...)  // ── LAYER B ──   PB:913   (the pin pass)
    mark all files loaded
```

Then the top-level publish flow layers further Layer-B passes over the same resources:
*Validate Resources → Regenerate Narratives → Process Provenance → clean → output*
(Publisher orchestration, ~PB-level).

**The boundary is almost perfectly clean, with exactly one coupling:** configuration flows into
Layer A *only* through the **resolution context** (the expansion parameters installed at
PIL:4037 can change how Layer A *resolves/expands* a value set during generation). Config never
flows into Layer A's *logic* and never makes Layer A pin. So:

- **Config → context → (read by) Layer A.**  ✅ exists, one channel.
- **Config → Layer A's behavior directly.**  ❌ does not exist.

> **Why this matters for the spec.** Because the boundary is this clean, we can specify Layer A
> as a closed, deterministic algorithm and treat *all* policy (pinning included) as separately
> documented passes. Anyone reimplementing Layer A in another language needs none of Part IV;
> anyone debugging "why is there a `|version` in my snapshot" needs only Part IV.

## 0.5 Pinning is general, not snapshot-specific

A useful tell that pinning is *not* part of snapshot generation: the same pin pass
(`checkCanonicalsForVersions`) is applied to **every** canonical resource the publisher loads —
ValueSets, CodeSystems, etc. — at PB:913, PIL:4228, and PIL:4277, not only to
StructureDefinitions. Snapshots are merely one kind of resource its generic datatype-walk
happens to traverse.

---

# Part I — Conceptual Model (Layer A)

## 1.1 What a snapshot is

A `StructureDefinition` describes a resource or data type as an ordered list of **element
definitions** (`ElementDefinition`, "ED"). It can carry that list two ways:

- **`differential`** — a *sparse* list. Only the elements the author wants to *say something
  about*, and for each, only the properties being introduced or constrained relative to the
  base. Human-authored; the normative expression of intent.
- **`snapshot`** — a *complete* list. Every element an instance may contain, each fully
  populated with all properties (inherited from base, overlaid with the differential).
  Mechanically derived; what tools actually consume.

**Snapshot generation** (Layer A) is the function:

```
generateSnapshot : (base.snapshot, derived.differential) → derived.snapshot
```

> **Completeness invariant.** Every element of `base.snapshot` is represented in the output
> (possibly renamed, multiplied into slices, or pruned to a constrained type's children), and
> every element of `derived.differential` contributes to exactly one output element (§3.8, PC-1).

## 1.2 The central idea: clone-and-overlay by a lockstep walk

At heart, Layer A is a **merge of two ordered lists keyed by element path**:

1. Start from a deep copy of `base.snapshot` (the "base list"). Anything the author does not
   mention survives unchanged — that is how inheritance happens.
2. Walk the base list top to bottom. Keep a separate cursor into the differential. The two
   lists are *not* the same length and *not* index-aligned: the differential is sparse and may
   mention one base element *several times* — that is what slicing is.
3. For each base element `B`, gather the differential elements that "talk about" `B` (the
   **diffMatches**, Part V/§roadmap). Then:
   - **nothing matches** → copy `B` through unchanged, but still recurse into its children (a
     descendant might be mentioned).
   - **one match, not sliced** → merge that diff element onto a copy of `B`, emit, recurse.
   - **the diff slices `B`** → emit a slicing "anchor" then one fully-merged element per slice.
   - **`B`'s children live elsewhere** (a data type, or a `contentReference` target) → walk into
     that structure's snapshot to get the children, rewriting their paths to sit under `B`.
4. Emit in base order. The result is the snapshot.

This is *why* the FHIR rules about differentials hold: a differential may only mention paths
that exist in the base (you cannot invent elements), and slices of a path MUST be consecutive
and base-compatible in order — because the generator consumes them with a **forward-only diff
cursor scoped by path**.

## 1.3 Two derivation modes

`derived.derivation` selects one of two modes, and several passes behave differently:

- **`SPECIALIZATION`** — `derived` defines a *new* type (resource, complex type, or logical
  model) on top of `base`. The base list is cloned **with its type name rewritten** (e.g.
  `Resource.id` → `MyResource.id`, PU:828-832). The differential may introduce the new type's
  own elements, and a dedicated pass (§3.5) inserts any differential element the main walk did
  not place.
- **`CONSTRAINT`** — `derived` is a *profile*: same type as base, only narrowing. Types MUST
  match (PU:759). Every differential element MUST correspond to an existing base element; an
  unmatched one is an error (§3.8, PC-1).

## 1.4 Layer A is itself a small pipeline of passes

Even the "pure" algorithm is not one monolithic loop. It is a short internal pipeline, and
Part III specifies each pass:

| Pass | Name | Job |
|---|---|---|
| **A0** | Base backfill & guards | Ensure `base` has a snapshot (recurse if not); reject circular/in-progress generation. |
| **A1** | Preprocessing | Clone the differential; push "trailing" slice-group properties down into each slice. |
| **A2** | The lockstep walk | The heart: walk base × differential, merging, slicing, expanding types, resolving contentReferences. |
| **A3** | Completion | Specialization fill-in, prune prohibited type-slices, assign element ids. |
| **A4** | Self-validation & normalization | Reconcile every differential against the output, check slice cardinality and path discipline, normalize mappings/constraint sources. |

(Pass A0 is also why generation is **recursive bottom-up** along the derivation chain — see
§3.2. That recursion is internal to Layer A and unrelated to Layer B's pass layering.)

## 1.5 What makes Layer A hard (five complexities)

Each gets its own deep-dive part later:

1. **Path matching is not string equality** — `[x]` choice elements, choice-type renaming
   (`value[x]` → `valueQuantity`), and slice names make correspondence a small *relation*.
2. **Slicing is multiple inheritance over a shared path** — one base element becomes an anchor
   plus N slices; slices may be re-sliced; anchor properties distribute into each slice.
3. **Type expansion is recursive snapshot generation** — walking into a data type or profiled
   type can itself trigger Layer A, guarded by a stack.
4. **`contentReference` is structural recursion** — elements pointing back at an ancestor's
   structure; resolution rewrites paths through a redirection stack.
5. **The merge has non-obvious precedence** — types replace wholesale, constraints accumulate,
   bindings only narrow, text may *append* via a `"..."` convention, and the merge sometimes
   mutates its own inputs. Much state rides an invisible `userData` side-channel (§2.6).

---

# Part II — Terminology and Notation

## 2.1 Core objects

| Term | Meaning |
|---|---|
| **ED** | `ElementDefinition`: `path`, `id`, `sliceName`, `min`/`max`, `type[]`, `slicing`, `binding`, `constraint[]`, `base`, … |
| **base list** | The working copy of `base.snapshot.element`, type-renamed under SPECIALIZATION. The walk consumes this. |
| **diff list** | A *clone* of `derived.differential.element` (PU:824), post-preprocessing. Mutated during processing; results migrated back at the end. |
| **output / snapshot** | `derived.snapshot.element`, built in base order. |
| **outcome** | One ED being constructed for the output, typically `B.copy()` before merge. |
| **cursor** | A position in a list; the walk pairs base/diff cursors with limits (`ProfilePathProcessorState`). |

## 2.2 Paths and segments

A **path** is `.`-separated (e.g. `Observation.component.value[x]`). `head(p)` = all but the
last segment; `tail(p)` = last segment (PU:1201, PU:3581). A name segment ending `[x]` is a
**choice (polymorphic) segment**; in instances it is renamed by capitalizing the type onto the
stem (`value[x]` + `Quantity` → `valueQuantity`). `B` is a **child of** `A` when `B.path =
A.path + "." + <one segment>` (PPP:1052); a **descendant** at any depth.

## 2.3 Slicing vocabulary

- **slicing anchor** — an ED carrying a `slicing` component (discriminator, rules, ordered),
  sharing its `path` with the slices that follow; usually has no `sliceName`.
- **slice** — an ED with the same `path` as its anchor plus a `sliceName`.
- **type slice** — slicing where each slice is one type of a choice/multi-type element; a single
  `type` discriminator on `$this` (PU:2401). A type slice's `max=1` means "this type appears at
  most once," not "repeats" (PU:996).
- **discriminator** — the `(type, path)` pair(s) distinguishing slices.

## 2.4 contentReference

An ED may carry `contentReference` = `#id` (internal) or `url#id` (external) instead of `type`,
meaning "my children are structurally identical to that element." Resolution substitutes the
referenced subtree with rewritten paths (deep-dive part).

## 2.5 Derivation and resolution

- **resolution context** — the set of canonical resources Layer A may look up (loaded packages
  + the IG under construction). Layer A *reads* it; Layer B *populates* it.
- **resolve(canonical)** — fetch the target resource for a canonical URL from the context,
  honoring a `|version` if present.

## 2.6 The `userData` side-channel (normative)

`ElementDefinition` carries an untyped `userData` map; Layer A threads essential state through
it. A reimplementation MUST provide an equivalent side-channel. Load-bearing keys
(`utils/UserDataNames.java`):

| Key | Meaning |
|---|---|
| `SNAPSHOT_GENERATED_IN_SNAPSHOT` | back-pointer diff→output. Its **absence** at the end marks an unmatched differential element (an error). |
| `SNAPSHOT_diff_source` | link from the diff *clone* back to the original differential element, so results migrate back (PU:913-919). |
| `SNAPSHOT_BASE_MODEL` / `SNAPSHOT_BASE_PATH` | the structure/path an output element was inherited from (PU:2005). |
| `SNAPSHOT_DERIVATION_POINTER` / `_EQUALS` / `_DIFF` | provenance cross-links between diff and snapshot (PU:918, 930). |
| `SNAPSHOT_SORT_ed_index` | original differential order, to stabilize sorting. |
| `SNAPSHOT_auto_added_slicing` | marks slicing the generator synthesized (vs. authored); changes whether slice-cardinality mismatch auto-corrects or errors (PU:998). |
| `SNAPSHOT_PREPROCESS_INJECTED` | marks a differential row the preprocessor *injected* (a shared slice-group property pushed into a slice, Part VII). Such a row has no original to migrate provenance to, so it is skipped in A4.1's back-migration — but it is **not** exempt from the PC-1 match requirement. |

## 2.7 Configuration knobs of Layer A

Layer B configures Layer A through a **small, declared parameter set**. These modulate output
but do not break determinism (same knobs + inputs + context → same output). A conforming
implementation MUST state any non-default values:

| Knob | Set by | Effect |
|---|---|---|
| `trimDifferential` | caller | when set, the merge deletes from the *differential* every property equal to base (minimizing it); affects the cross-linked diff, not snapshot values. |
| `forPublication` | `PB:883` (publisher sets `true`) | promotes certain slice-cardinality findings from INFORMATION to ERROR (PU:1003). |
| `mappingMergeMode` | caller | how `element.mapping` entries from base+derived combine. |
| `newSlicingProcessing` | `PB:887` (R4+) | selects the modern slicing pass. |
| `xver` | `PB:882` | cross-version extension manager, enabling resolution of `[x]`-version extensions. |

> These knobs live *inside* Layer A but originate in Layer B. They are the second, narrow way
> configuration touches the algorithm (the first being the resolution context, §0.4). They are
> declared and finite — unlike Layer-B policy, which can rewrite the artifact arbitrarily.

## 2.8 Pseudocode conventions

`clone(x)` = deep copy. Lists 0-indexed. `emit(e)` appends to output. `error(msg)` records a
validation message (severity per config; may or may not abort). `fail(msg)` raises a fatal
exception aborting generation. `e.has(field)` tests presence.

---

# Part III — Layer A Specified: The Structural Algorithm

This part specifies `generateSnapshot` (PU:740-1097) as the five-pass pipeline of §1.4. The
top-level procedure (§3.7) lays the passes out as labelled phases.

## 3.1 Signature and contract

```
generateSnapshot(base, derived, url, webUrl, profileName)
    requires: base.hasSnapshot (else Pass A0 builds it); base.hasType; derived.hasType;
              derived.hasDerivation; if CONSTRAINT then base.type == derived.type
    ensures:  derived.snapshot is complete (PC-1..PC-4, §3.8), or generation fails atomically
    determinism: a pure function of (base, derived, knobs §2.7, resolution context)
    termination: guaranteed by the snapshotStack guard (§3.2)
```

## 3.2 Pass A0 — base backfill and guards

```
A0.1  precondition checks (PU:741-761): non-null; checkNotGenerating(base/derived);
      both have type; derived has derivation; CONSTRAINT ⇒ types match.
A0.2  base-snapshot backfill (PU:762-768):
        if not base.hasSnapshot:
            sdb := resolve(base.baseDefinition)
            generateSnapshot(sdb, base, ...)        // RECURSE — bottom-up along derivation chain
A0.3  fixTypeOfResourceId(base); if base has EXT_TYPE_PARAMETER → checkTypeParameters (PU:769-772)
A0.4  circular guard (PU:774-778):
        if snapshotStack contains derived.url → fail "circular snapshot"
        snapshotStack.add(derived.url); derived.generatingSnapshot := true
A0.5  derived.snapshot := new empty list                                    (PU:788)
```

Two distinct guards, both normative:
- **`checkNotGenerating`** — a structure mid-generation MUST NOT be used as base or focus
  (prevents consuming a half-built snapshot, PU:1694).
- **`snapshotStack`** — `generateSnapshot` MUST refuse re-entry for a URL already on the stack.
  Because type expansion and A0.2 recurse into `generateSnapshot`, this stack is what guarantees
  **termination** on cyclic data.

## 3.3 Pass A1 — preprocessing

```
A1.1  checkDifferential + checkDifferentialBaseType (PU:791-792)            // structural legality
A1.2  copyInheritedExtensions; findInheritedObligationProfiles (PU:808-810)
A1.3  clear SNAPSHOT_GENERATED_IN_SNAPSHOT on every differential element     (PU:820-821)
A1.4  diff := clone(derived.differential)                                    (PU:824)
A1.5  SnapshotGenerationPreProcessor.process(diff, derived)                  (PU:825)
        // pushes "trailing" slice-group properties down into each slice (multiple-inheritance edge)
A1.6  baseSnapshot := base.snapshot
      if SPECIALIZATION: baseSnapshot := cloneSnapshot(baseSnapshot, base.type, derived.type)  // rename
```

The differential is cloned (A1.4) because later passes **mutate it**; provenance is migrated
back to the original in Pass A4.

## 3.4 Pass A2 — the lockstep walk (the heart)

```
A2.1  mappingDetails := new MappingAssistant(mappingMergeMode, base, derived, version, …)  (PU:837)
A2.2  ProfilePathProcessor.processPaths(this, base, derived, url, webUrl, diff, baseSnapshot, mappingDetails)
                                                                                            (PU:839)
```

`processPaths` is the clone-and-overlay walk of §1.2. Its dispatch (simple / sliced-base /
empty-diff / type-constraining), cursor state machine, child-span computation, recursion into
children/types/contentReferences, and the per-element merge are specified in deep-dive Parts
V-IX. This is the bulk of the algorithm's essential complexity.

## 3.5 Pass A3 — completion

```
A3.1  checkGroupConstraints (PU:841)
A3.2  if SPECIALIZATION (PU:842-867): for each diff element not yet generated, with "." in path:
          if an element already exists at its path → merge onto it
          else → copy, URL-fix, insert at correct child position; if it walks into a (single)
                 type → append that type's inherited elements (addInheritedElementsForSpecialization)
A3.3  for each output element with >1 type: drop any type whose type-slice is prohibited (PU:869-881)
A3.4  if kind != LOGICAL and output[0] has a type → fail        // root MUST be untyped (PU:882)
A3.5  mappingDetails.update(); setIds(derived, false)            // assign every element.id (PU:884-886)
```

## 3.6 Pass A4 — self-validation and normalization

```
A4.1  reconcile every diff element against the output (PU:908-948):
          if the element has SNAPSHOT_diff_source (an original author-written row, not a
              preprocessor-injected one): migrate SNAPSHOT_DERIVATION_* userData back to its original
          if the element has no SNAPSHOT_GENERATED_IN_SNAPSHOT:        // unmatched — applies to ALL rows
              record it; if it has an id → ERROR "no match in snapshot"        // ★ PC-1
          else: cross-link snapshot→diff via SNAPSHOT_DERIVATION_DIFF
A4.2  normalize (PU:949-968): trim mapping.map whitespace; absolutize constraint.source to canonical URLs
A4.3  if SPECIALIZATION: ensure every output element has a .base (PU:969-975)
A4.4  slice + path validation (PU:976-1036):
          track open slice groups (ElementDefinitionCounter per anchor);
          on close, check slice min/max vs anchor (auto-correct if generator-added, else message);
          every non-root path MUST start with derived.type + "." (else fail);
          sliceName with no open group → error; duplicate sliceName → error.
A4.5  profile/targetProfile reference validation (PU:1038-1077):
          resolve each (incl. cross-version); unresolved → WARNING; else check type compatibility.
```

Note A4.5 *resolves* canonicals (reading the context) but **does not** pin them — consistent
with §0.2.

## 3.7 The top-level procedure (passes as phases)

```
procedure generateSnapshot(base, derived, url, webUrl, profileName):
    // Pass A0 — backfill & guards
    A0.1 preconditions; A0.2 backfill base snapshot (recurse); A0.3 fix Resource.id / type params;
    A0.4 circular guard + enter; A0.5 fresh empty snapshot
    enable userData copying for the duration; normalize webUrl                  (PU:779-787)

    try:
        // Pass A1 — preprocessing
        A1.1 checkDifferential; A1.2 inherit extensions/obligations;
        A1.3 clear gen flags; A1.4 diff := clone(differential);
        A1.5 preprocess(diff); A1.6 prepare base list (rename if SPECIALIZATION)

        // Pass A2 — the walk
        A2.1 mappingDetails; A2.2 processPaths(...)

        // Pass A3 — completion
        A3.1 group constraints; A3.2 specialization fill-in; A3.3 prune prohibited type-slices;
        A3.4 assert root untyped; A3.5 mappings.update + setIds

        // Pass A4 — self-validation & normalization
        A4.1 reconcile diff↔output (★ PC-1); A4.2 normalize; A4.3 ensure .base (SPECIALIZATION);
        A4.4 slice + path validation; A4.5 profile reference checks
    catch any exception:
        derived.snapshot := null                         // failure atomicity (PU:1078-1085)
        rethrow
    finally:
        restore userData-copy flag; derived.generatingSnapshot := false; snapshotStack.remove(derived.url)

    if base.version present: stamp EXT_VERSION_BASE on the snapshot            // PU:1091-1092 (only version write)
    derived.generatedSnapshot := true; attach generation messages
```

## 3.8 Post-conditions

- **PC-1 (every differential is consumed).** After generation, every differential element —
  author-written *and* preprocessor-injected — MUST carry `SNAPSHOT_GENERATED_IN_SNAPSHOT`,
  i.e. it produced exactly one output element. Any element lacking it is unmatched; if it has an
  id this is an ERROR (under CONSTRAINT) (A4.1). Injected rows are exempt only from provenance
  back-migration, **not** from this requirement. *This is the primary correctness signal and the
  usual symptom of an illegal differential (bad path, bad order, slicing not set up).*
- **PC-2 (root untyped).** Unless `kind == LOGICAL`, output[0] has no `type` (A3.4).
- **PC-3 (path discipline).** Every non-root output path starts with `derived.type + "."` (A4.4).
- **PC-4 (slice cardinality coherence).** For each repeating slicing anchor, slice cardinalities
  are consistent with the anchor, or auto-corrected when the generator introduced the slicing
  (A4.4).

## 3.9 Failure atomicity (normative)

If any pass A1-A4 fails, the generator MUST discard the partial snapshot
(`derived.snapshot := null`) so no consumer sees a half-built result, and MUST clear the
`generatingSnapshot` flag and stack entry in all cases.

---

# Part IV — Layer B Specified: Policy & Orchestration Passes

Layer B is *not* part of the snapshot algorithm; it is the tooling that runs Layer A and then
applies project policy to the artifact. It is specified here because (a) it is where observable
behaviors like version-pinning actually originate, and (b) the boundary must be stated precisely
so Layer A stays clean.

## 4.1 The pipeline and ordering

Within `loadConformance1` (PIL:4025):

```
1. load metadata resources                                            (PIL:4034)
2. if pin-manifest configured: install Parameters as context
   expansion parameters                                               (PIL:4037-4041)   ← coupling into Layer A
3. generateSnapshots():                                               (PIL:4043)
      for each StructureDefinition sd:
          if not sd.isSnapshotted: generateSnapshot(...)  // Layer A   (PB:908)  idempotent (PB:906/761)
          checkCanonicalsForVersions(sd, snapshotMode=false)          (PB:913)  // pin pass
4. mark files loaded
```

Subsequent top-level passes over the same resources: **Validate → Regenerate Narratives →
Process Provenance → clean → output**.

**Idempotency (normative for Layer B).** A StructureDefinition's snapshot MUST be generated at
most once; the `isSnapshotted` flag guards re-entry (PB:906, set PB:761). Policy passes edit the
*existing* snapshot; they MUST NOT trigger regeneration.

## 4.2 Pass — load & context installation

Layer B loads packages and the IG's own resources into the resolution context, establishing
what Layer A's `resolve()` can see. If a `pin-manifest` is configured, its `Parameters` are
installed as the context's **expansion parameters** *before* snapshot generation (PIL:4037-4041)
— the single sanctioned channel by which configuration influences Layer A (it can change value
set *expansion/resolution*, never element *structure*).

## 4.3 Pass — snapshot generation invocation

Layer B calls Layer A once per StructureDefinition (PB:908, via PB:685 `generateSnapshot` which
resolves the base via `fetchSnapshotted`, optionally `sortDifferential` first, then delegates to
`ProfileUtilities.generateSnapshot`). Output and messages are attached to the resource.

## 4.4 Pass — canonical version pinning (the worked example)

**What it does.** After a resource's snapshot exists, `checkCanonicalsForVersions` (PB:766)
walks **every `CanonicalType`-valued field** of the resource via a generic `DataTypeVisitor` and
may rewrite each `url` to `url|version`. It is policy, applied *on top of* the structural output.

**Policy selector** (IG `pin-canonicals`, PIL:739):

| Value | `PinningPolicy` | Behavior |
|---|---|---|
| `pin-none` | `NO_ACTION` | never pin (the visitor still normalizes `*`/`\|*` wildcards). |
| `pin-all` | `FIX` | pin every resolvable, versioned canonical. |
| `pin-multiples` | `WHEN_MULTIPLE_CHOICES` | pin only when ≥2 versions of the target are known in context. |

**Per-canonical decision** (`CanonicalVisitor.visit`, PB:1352):

```
if url contains "|"                                   → skip (already pinned)
if url has CANONICAL_RESOLUTION_METHOD extension       → skip (resolution deferred)
tgt := context.fetchResourceRaw(url)                   // depends on loaded packages
if tgt is null or not tgt.hasVersion                   → skip
if tgt is a THO "not-present" CodeSystem               → skip (its version is unreliable)
if path under ImplementationGuide.dependsOn            → skip
switch policy:
  FIX:                  pin := true
  WHEN_MULTIPLE_CHOICES: pin := (versionMap(url).size ≥ 2)   // genuine ambiguity from context
if pin:
  if pin-manifest configured: record (type,url,version) in the manifest, do NOT mutate the value
  else:                       url := url + "|" + tgt.version          (PB:1392 / PB:1409)
```

**Inverse normalization** (`CanonicalVisitorLatest`, PB:1418): `|*` wildcard → strip version and
attach `CANONICAL_RESOLUTION_METHOD = latest` (PB:1445/872).

**`snapshotMode` flag.** When the pass runs in snapshot context it still pins but suppresses the
informational message and pin-count (PB:1383, 1400); the publisher's main passes call it with
`snapshotMode=false`.

**Where pins land.** Because this runs *after* Layer A copied canonicals verbatim, a `|version`
in a published snapshot's `type.profile` / `binding.valueSet` was written *here*, not by the
algorithm.

**Generality.** The same pass applies to ValueSets, CodeSystems, and other canonical resources
(PB:913, PIL:4228, PIL:4277) — confirming it is a resource-level policy, not snapshot logic.

## 4.5 Other policy passes (named, deferred)

The same layering covers **narrative regeneration**, **instance validation**, and **provenance
processing**, each a separate pass over the artifact, each driven by IG configuration, none part
of Layer A. Future installments may specify these; for now they are named to delimit Layer A.

## 4.6 The boundary restated (normative)

- Layer A MUST treat canonical references as opaque strings and copy them verbatim; it MUST NOT
  pin or version-normalize them. Its only version write is `EXT_VERSION_BASE` (PU:1092).
- Configuration MUST reach Layer A only via (a) the resolution context, and (b) the declared
  knobs of §2.7 — never as ad-hoc policy inside the algorithm.
- Layer B MUST NOT depend on Layer A to apply policy, and MUST treat the snapshot as
  generate-once-then-edit.

---

# Part V — Path Correspondence

Everything in the walk hinges on one question: *given a base element, which differential
elements are talking about it?* Because of polymorphic (`[x]`) elements and choice-type
renaming, the answer is **not** string equality — it is a small, precisely-defined relation.
This part specifies it. (Anchors: PU:1141, 2027, 2023, 2487, 2444, 2491, 2032.)

## 5.1 The choice-renaming rule (the source of all the subtlety)

A polymorphic element is authored in the base with a `[x]` suffix (`value[x]`). In a concrete
profile or instance it is **renamed** by removing `[x]` and appending the capitalized type name:

```
value[x]  +  type Quantity   →   valueQuantity
value[x]  +  type string     →   valueString
```

So a differential element named `valueQuantity` *corresponds to* the base element `value[x]`.
Every matching predicate below exists to make that correspondence work in one direction or
another. The recurring shape is: *strip the `[x]`, then test the concrete name against the
stem, ensuring we don't accidentally cross into a child.*

## 5.2 Single-name matching predicates

Let `stem(n)` be `n` with a trailing `[x]` removed.

**`pathMatches(path, ed)`** (PU:1141) — "does element `ed` sit at `path`?", where `path` may be
a `[x]` form:
```
pathMatches(path, ed):
    if ed.path == path: return true
    if path ends with "[x]":
        s := stem(path)
        return ed.path startsWith s  and  |ed.path| > |s|  and  ed.path[|s|:] has no "."
    return false
```
> The "no `.` after the stem" clause is essential: `value[x]` matches `valueQuantity` but
> **not** `valueQuantity.unit` (that is a child, not the choice element itself).

**`pathMatches(p1, p2)`** (PU:2027) — the pure-string twin, with the `[x]` on `p2`:
```
p1 == p2  or  (p2 ends "[x]"  and  p1 startsWith stem(p2)  and  p1[|stem(p2)|:] has no ".")
```

**`pathStartsWith(p1, p2)`** (PU:2023) — "is `p1` at or below the prefix `p2`?", honoring a
`[x].` prefix:
```
p1 startsWith p2  or  (p2 ends "[x]."  and  p1 startsWith p2 without the trailing "[x].")
```
> So `value[x].` is a valid prefix of `valueQuantity.unit`. Used for descendant/scope tests.

**`isSameBase(p, sp)`** (PU:2487) — the **symmetric** single-segment choice match; *either* side
may carry the `[x]`:
```
(p ends "[x]"  and  sp startsWith stem(p))  or  (sp ends "[x]"  and  p startsWith stem(sp))
```

## 5.3 The correspondence routine: `getDiffMatches`

This is the function the walk actually calls to gather the differential elements for a base
path (PU:2444). Given the differential, a target `path`, and a cursor window `[start, end]`:

```
getDiffMatches(diff, path, start, end):
    p := path.split(".")                       // segments of the base path (in context coords)
    result := []
    for i in start..end:
        sp := diff.element[i].path.split(".")
        if |sp| == |p|  and  for all j: (p[j] == sp[j]  or  isSameBase(p[j], sp[j])):
            result.add(diff.element[i])
    return result
```

**Normative consequences:**
- **Same depth only.** `getDiffMatches` requires `|sp| == |p|`. It returns only elements at the
  *same path depth* as the base element — never descendants. Descendants are reached by
  *recursion* into the child block (Part VI), not by this match.
- **Segment-wise choice tolerance.** Each segment must be equal or choice-compatible
  (`isSameBase`), so `Observation.value[x]` matches a differential `Observation.valueQuantity`,
  and intermediate `[x]` segments also resolve.
- **Multiplicity is slicing.** `getDiffMatches` may return **several** elements (several
  differential rows share one base path). That multiplicity is precisely what signals slicing
  to the dispatcher (Part VI, §6.3).
- **Window-scoped.** Matching is confined to `[start, end]`, the cursor window the walk
  maintains. This is how forward-only ordering is enforced: a differential element earlier than
  the cursor is invisible.

## 5.4 Child-block boundaries: `findEndOfElement`

To recurse into an element's children, the walk needs the *extent* of that element's subtree in
a list. For a cursor `c`:
```
findEndOfElement(list, c):                       // PU:2491 (diff) / 2501 (snapshot)
    path := list[c].path + "."
    r := c
    while r+1 in range and list[r+1].path startsWith path: r := r+1
    return r                                       // index of the last descendant of list[c]
```
`findEndOfElementNoSlices` (PU:2509) is the same but **stops at the first element bearing a
`sliceName`** — because slices share their anchor's path and would otherwise be swept in.

## 5.5 Context rewriting: `fixedPathSource` / `fixedPathDest`

When the walk descends into a data type's own snapshot (Part VIII) or follows a
`contentReference` (Part IX), the base element's native path (e.g. `Quantity.value`) must be
rewritten into the *current* path coordinates (e.g. `Observation.valueQuantity.value`).
`fixedPathSource` (PU:2032) produces the path used for matching; `fixedPathDest` (PU:2051)
produces the path written into the output:
```
fixedPathSource(contextPath, p, redirector):
    if contextPath is null: return p                         // top level: identity
    if redirector non-empty:
        tail := (|contextPath| >= |p|) ? p after its 1st segment : p after contextPath's length
        return redirector.last.path + "." + tail             // rewrite under the redirect target
    else:
        return contextPath + "." + (p after its 1st segment) // rewrite under the context element
```
`fixedPathDest` is structurally identical but keyed off `redirectSource` for the tail
computation. These two functions are the entire mechanism by which "the same Quantity
definition" gets re-homed under every element that uses it. (The redirector stack is specified
in Part IX.)

## 5.6 Worked example

Base (`Observation` snapshot, fragment) and a profile differential:

```
base.snapshot:   Observation.value[x]            [0..1]   type: Quantity | string | CodeableConcept
                 Observation.component            [0..*]
                 Observation.component.value[x]   [0..1]   type: Quantity | string

derived.diff:    Observation.valueQuantity        (constrain: unit required)
                 Observation.component.valueQuantity
```

- At base `Observation.value[x]`: `getDiffMatches` splits to `[Observation, value[x]]`. The
  diff row `Observation.valueQuantity` splits to `[Observation, valueQuantity]`; segment 0 equal,
  segment 1 `isSameBase("value[x]","valueQuantity")` → true. **One match** → merge (Part VI §6.2).
- The diff row `Observation.component.valueQuantity` has **3** segments and does *not* match the
  2-segment `value[x]` path (depth differs) — correctly deferred until the walk recurses into
  the `Observation.component` child block, where the window narrows and the same logic applies.

---

# Part VI — The Lockstep Walk (`processPaths`)

This part specifies Pass A2 (§3.4): the recursive walk that consumes the base list and the
differential together and emits the snapshot. It is the structural heart of Layer A. (Anchors:
PPP:155, 191, 283, 307, 1196, 1723; state in `ProfilePathProcessorState`.)

## 6.1 State

The walk's state is split in two:

**Per-recursion cursors** (`ProfilePathProcessorState`, mutable within one call frame):

| Field | Meaning |
|---|---|
| `base` | the snapshot element list being walked (the source structure's, possibly a data type's). |
| `baseCursor` | index of the base element currently in focus. |
| `diffCursor` | forward-only index into the differential; the low end of the match window. |
| `contextName`, `resultPathBase` | diagnostic / path-base context. |

**Per-frame processor configuration** (set via a builder at each recursion, PPP:166-181):
`result` (the output list), `differential`, `baseLimit`/`diffLimit` (the high ends of the
windows), `contextPathSource`/`contextPathTarget` (for §5.5 rewriting), `redirector` (the
contentReference stack), `slicing` (`PathSlicingParams` — the slice context), `trimDifferential`,
`url`/`webUrl`/`profileName`, `sourceStructureDefinition`, `derived`.

> **Why two scopes.** `baseLimit`/`diffLimit` plus `contextPath*` and `redirector` are *rebuilt*
> for each recursion (each child block, each type expansion), while `baseCursor`/`diffCursor`
> advance *within* a frame. This is exactly what bounds a child's processing to its own window.

## 6.2 The main loop

Entry (`processPaths` static, PPP:155) initializes cursors at `(baseCursor=0, diffCursor=0)`,
`baseLimit = |base|-1`, `diffLimit = |diff|-1`, empty `redirector`, empty `slicing`. Then:

```
processPaths(cursors):                                                   // PPP:191
    res := null;  first := true
    while cursors.baseCursor <= baseLimit  and  cursors.baseCursor < |cursors.base|:
        currentBase     := cursors.base[cursors.baseCursor]
        currentBasePath := fixedPathSource(contextPathSource, currentBase.path, redirector)   // §5.5
        diffMatches     := getDiffMatches(differential, currentBasePath,
                                          cursors.diffCursor, diffLimit)                       // §5.3
        dc := cursors.diffCursor

        if not currentBase.hasSlicing  or  currentBasePath == slicing.path:    // base is NOT sliced
            currentRes := processSimplePath(currentBase, currentBasePath, diffMatches, ...)    // §6.3
            res := res ?? currentRes
        else:                                                                  // base IS already sliced
            processPathWithSlicedBase(currentBase, currentBasePath, diffMatches, ...)          // §6.4

        if diffMatches non-empty  and  cursors.diffCursor == dc:               // advance past consumed
            cursors.diffCursor += |diffMatches|
        first := false

    checkAllElementsOK()                          // every emitted element has a non-null min (PPP:237)
    return res
```

Two structural rules fall out and are **normative**:

- **R-WALK-1 (forward-only diff cursor).** The differential is consumed strictly forward. After
  a base element's matches are processed, if no inner step advanced the cursor, it advances by
  the number of matches (PPP:225-228). A differential element behind the cursor can never match
  again — this is why differential element **order matters** and why misordered differentials
  fail PC-1.
- **R-WALK-2 (window scoping).** Matching and recursion are bounded by `[diffCursor, diffLimit]`
  and `[baseCursor, baseLimit]`. Recursion into a child block (or a type) creates a *new* frame
  with narrowed limits computed by `findEndOfElement` (§5.4).

> **Caution (fragile area).** The cursor-advance in R-WALK-1 is the subject of an explicit
> implementer note (PPP:216-229, "GDG 28-July 2025"): some inner branches do not advance the
> cursor themselves, so this blanket advance compensates. A reimplementation MUST reproduce the
> "advance by `|diffMatches|` iff unchanged" behavior or it will mis-handle inner content under
> slices.

## 6.3 Dispatch for an unsliced base element: `processSimplePath`

When the base element is not itself already sliced, `processSimplePath` (PPP:283) chooses one of
four branches by the shape of `diffMatches`:

```
processSimplePath(currentBase, path, diffMatches, ...):
    if diffMatches is empty:
        → processSimplePathWithEmptyDiffMatches            // §6.3.1  copy-through
    else if oneMatchingElementInDifferential(slicing.done, path, diffMatches):
        → processSimplePathWithOneMatchingElementInDifferential   // §6.3.2  the merge case
    else if diffsConstrainTypes(diffMatches, path, typeList):
        → processSimplePathWhereDiffsConstrainTypes        // §6.3.3  implicit type slicing on [x]  (Part VIII)
    else:
        → processSimplePathDefault                         // §6.3.4  the diff SLICES this element  (Part VII)
```

**`oneMatchingElementInDifferential`** (PPP:1723) — the predicate that separates "a single
constraint" from "the start of slicing":
```
returns true  iff  |diffMatches| == 1  and
                   ( slicing.done                                          // already inside a slice
                     or  ( not isImplicitSlicing(diffMatches[0], path)     // not a [x] type-slice
                           and not diffMatches[0].hasSlicing               // not opening a slice
                           and not (isExtension(diffMatches[0]) and diffMatches[0].hasSliceName) ) )
```

### 6.3.1 Empty diff — copy-through
The differential says nothing about this element. The walk copies `currentBase` into the output
(re-homing its path via `fixedPathDest`), then **still recurses into its child block**, because
a descendant may be constrained even though the parent is not. (`processSimplePathWithEmptyDiffMatches`,
PPP:1061.)

### 6.3.2 One match — the merge case
The common path: exactly one differential element constrains this base element and is *not*
opening slicing. The walk produces `outcome := clone(currentBase)`, applies
`updateFromDefinition(outcome, diffMatch, ...)` (the property-by-property merge, Part X), records
the diff→output back-pointer (`SNAPSHOT_GENERATED_IN_SNAPSHOT`), emits, and recurses into
children — descending into the diff's child window in parallel. (PPP:674.)

### 6.3.3 Diffs constrain types — implicit type slicing
The matches are constraining individual types of a choice/multi-type element (e.g. several rows
under `value[x]` each pinning a different type). `diffsConstrainTypes` (PU:1806) recognizes this
and builds a `typeList`; processing is specified in Part VIII.

### 6.3.4 Default — the diff slices the base
The remaining case: the differential is *introducing slicing* on a base element that was not
previously sliced. Before proceeding, two preconditions are enforced (`processSimplePathDefault`,
PPP:307):
- **Slicing a non-repeating element is illegal** unless the slices collectively cap at one
  (`isSlicedToOneOnly`) or it is type slicing (`isTypeSlicing`) — else
  `fail("attempt to slice an element that does not repeat")` (PPP:309-312).
- **The first slice row MUST define the slicing** (`hasSlicing`), unless the base is an extension
  — else `fail("differential does not have a slice")` (PPP:313).

The slice machinery itself (anchor synthesis, per-slice emission, re-slicing) is Part VII.

## 6.4 Dispatch for an already-sliced base element: `processPathWithSlicedBase`

When the base element *already* carries slicing (e.g. profiling a profile that sliced, or a
core element that ships sliced), the walk takes `processPathWithSlicedBase` (PPP:1196), which
mirrors §6.3 but in slice-aware form: it pairs each existing base slice with the differential's
slices, applies the same empty/one-match/type-constraining/default sub-dispatch per slice, and
handles slices the differential adds or leaves untouched. Detailed in Part VII.

## 6.5 Termination and completeness within the walk

- **Termination.** Each frame strictly advances `baseCursor` to `baseLimit`; recursion operates
  on strictly smaller windows (child spans from `findEndOfElement`). Combined with the Part III
  `snapshotStack` guard against re-entrant *type* expansion, the walk terminates.
- **Completeness.** `checkAllElementsOK` (PPP:237) asserts every emitted element has a non-null
  `min` — a cheap structural tripwire. The real completeness guarantee (every differential row
  consumed) is enforced *after* the walk by PC-1 (§3.6 / §3.8), not here.

---

# Part VII — Slicing

Slicing is the single largest source of complexity in Layer A. This part specifies it in full:
the conceptual model, the preprocessor pass that prepares it, the compatibility rules that
govern refinement, the two emission paths (introducing slicing vs. refining existing slicing),
slice-body construction, re-slicing, and cardinality validation. (Anchors: PPP:307, 674, 955,
1196, 1225, 1494, 1657; PU:157, 2351-2416; `SnapshotGenerationPreProcessor`.)

## 7.1 Conceptual model

**Slicing splits one repeating element into named cases.** Where the base has one element
(`Observation.component [0..*]`), a profile may carve it into named *slices*
(`component:systolic`, `component:diastolic`), each with its own constraints, while keeping the
general element too. In the snapshot this appears as:

```
Observation.component                    ← the SLICING ANCHOR (carries .slicing, no sliceName)
Observation.component  (sliceName=systolic)   ← slice 1
Observation.component   ... children ...
Observation.component  (sliceName=diastolic)  ← slice 2
Observation.component   ... children ...
```

Three structural invariants hold in every conforming snapshot, and the algorithm enforces them:

- **INV-S1 — anchor first, exactly once.** The anchor (the element bearing the `slicing`
  component, with **no** `sliceName`) is emitted before any of its slices, exactly once.
- **INV-S2 — a slice never carries `slicing`.** Each slice has a `sliceName` and its
  `slicing` is cleared (`outcome.setSlicing(null)`, PPP:819).
- **INV-S3 — base order preserved.** Anchor and slices appear in the base element's position,
  in slice order; new slices are appended after existing ones.

**The central mechanism (important and non-obvious): slices are produced by recursion, not
inline.** The slice loop does *not* build slice bodies directly. For each slice it issues a
**recursive `processPaths`** call whose *base window is the same single base element + its
children* and whose *diff window is that slice's differential subtree*, with the slicing context
flagged `done = true`. Inside that recursion the lone diff match flows to the ordinary
one-match merge (§7.7). So "build N slices from one base element" = "re-run the walk over that
base element N times, once per slice."

> **Non-obvious build fact.** `APPLY_PROPERTIES_FROM_SLICER = false` (PPP:58). The helper
> `merge(src, slicer)` therefore degenerates to `src.copy()` — in current builds the **anchor
> contributes no min/max/properties to its slices**. Each slice is cloned from the *base
> element*, not from the anchor. A reimplementation MUST NOT fold anchor properties into slices.

## 7.2 Pass A1.5 — the preprocessor (trailing slice-property pushdown)

Before the walk runs, `SnapshotGenerationPreProcessor.process` (the Pass A1.5 of §3.3) handles
a multiple-inheritance edge case: **shared constraints written once for a whole slice group must
be distributed into each slice.** Authors may write, after the slicing anchor but *before the
first named slice*, deep constraints meant to apply to every slice:

```
component            (slicing)         ← anchor
component.code       (shared: must be LOINC)   ← "trailing" group property (sliceStuff)
component:systolic
component:diastolic
```

The preprocessor pushes `component.code` into both `systolic` and `diastolic`.

**Algorithm** (`processSlices` → `mergeElements`):

```
// Pass 1 — partition the differential into slice groups (SliceInfo):
//   a group opens at an element with non-extension .slicing;
//   an element with the group's path + a sliceName starts a named slice;
//   group-level deep elements seen BEFORE the first named slice become the group's `sliceStuff`.
//   (Elements placed AFTER the first slice are NOT captured — INV assumption.)
// Pass 2 — guard: if any sliceStuff element itself carries non-extension .slicing,
//   emit UNSUPPORTED_SLICING_COMPLEXITY and ABORT the entire pushdown (global, not per-group).
// Pass 3 — pushdown (iterate groups backward so insertions don't shift pending indices):
for each group g with non-empty g.sliceStuff and g.slices != null:
    for each slice s in g.slices:
        mergeElements(diff.element, g.sliceStuff, s, g.slicer)
```

`mergeElements(elements, sharedProps, slice, slicer)`:
```
[start,end] := the row span of `slice` (slice row .. last descendant before next sibling)
for each shared property p in sharedProps:
    if some row in [start,end] matches p (path + sliceName equivalence):
        merge p into that row              // additive, focus-wins: copy only props the row lacks
    else:                                  // INJECT a new row
        copy := p.copy()
        copy.id := p.id with slicer.id replaced by slice.id
        copy.userData[SNAPSHOT_PREPROCESS_INJECTED] := true
        insert copy at the base-definition-ordered position within [start,end]   // determineInsertionPoint
```

**Normative notes:**
- **Extension slicing is excluded** (`isExtensionSlicing`): standard `url`/VALUE/OPEN extension
  slices are not treated as pushdown groups. *(Implementation bug to be aware of: the check
  misspells `"modiferExtension"`, so modifier-extension slicing is not recognized — `SnapshotGenerationPreProcessor` line 1078.)*
- **Injected rows carry `SNAPSHOT_PREPROCESS_INJECTED`** and are exempt from provenance
  back-migration only (§2.6), not from PC-1.
- **In-place mutation**: the differential clone is edited directly; backward iteration keeps
  indices stable.
- **Separate conditional subsystem.** When the source SD carries `EXT_ADDITIONAL_BASE`
  extensions, `process` additionally performs a full "additional base" multiple-inheritance
  merge (`mergeElementsFromAdditionalBase`). This is a distinct, partially-implemented feature
  (many `"Not done yet"` guards) and is **out of scope** for the core slicing model; a profile
  without `EXT_ADDITIONAL_BASE` never invokes it.

## 7.3 Slicing-refinement compatibility (the match triad)

When a differential restates a `slicing` component on an element the base *already* sliced, the
restatement must be a legal **refinement**. Three independent, null-tolerant predicates are
ANDed (PU:2372-2392):

- **`orderMatches(diffOrdered, baseOrdered)`** — conflict only when *both* are set and disagree;
  any unset side matches.
- **`discriminatorMatches(diffDiscs, baseDiscs)`** — empty on either side matches; otherwise the
  diff must be a **positional prefix-superset**: `|diff| ≥ |base|` and `diff[i] == base[i]`
  (equal `type` *and* `path`) for every base index `i`. The diff may *append* discriminators but
  never drop or reorder the base's.
- **`ruleMatches(diffRule, baseRule)`** — allowed transitions (base → diff): `OPEN → anything`;
  `CLOSED → CLOSED` or `CLOSED → OPENATEND`; `OPENATEND → OPENATEND`; any unset side matches.
  > Note the asymmetry: `CLOSED → OPEN` is forbidden (cannot re-open), but `CLOSED → OPENATEND`
  > is explicitly allowed; `OPENATEND → CLOSED` is **not** allowed by this predicate.

If all three pass, **`updateFromSlicing(baseSlicingCopy, diffSlicing)`** merges the refinement:
`ordered` and `rules` **overwrite** when the diff sets them; discriminators are **unioned** by
`(type,path)` string equality (not object identity). Failure of any predicate is a hard error
(§7.10).

## 7.4 Type slicing and default slicings

- **`isTypeSlicing(e)`** (PU:2401): true iff exactly **one** discriminator with `type == TYPE`
  and `path == "$this"`. Type slices are special: a type slice's `max == 1` means "this type
  occurs at most once," not "non-repeating," and a type-slicing anchor's `slicing` is **forced**
  to `type@$this / CLOSED / unordered` regardless of what the differential wrote (PPP:596,
  1585).
- **`isImplicitSlicing(ed, path)`** (PU:1796): true when `path` ends `[x]`, `ed.path != path`,
  and `ed.path` starts with `stem(path)` — i.e. a single diff row like `valueQuantity` against a
  base `value[x]` *implicitly* opens a type slice (handled via `diffsConstrainTypes`, Part VIII).
- **`makeExtensionSlicing()`** (PU:2408): the hard-coded default for extension elements —
  discriminator `{type: VALUE, path: "url"}`, `ordered = false`, `rules = OPEN`. Used whenever a
  differential opens extension slicing without stating its own `slicing` component.
- **`isExtension(base)`** (PU:2416): path-suffix test — ends with `.extension` or
  `.modifierExtension`.

## 7.5 Introducing slicing on an unsliced base (`processSimplePathDefault`)

Reached from §6.3.4: the base element is not yet sliced and the differential opens slicing.
(PPP:307-448.)

```
// Preconditions (else fail, §7.10):
//   base must repeat, unless the slices cap at 1 (isSlicedToOneOnly) or it is type slicing.
//   the first diff row must define the slicing, unless base is an extension.

newBaseLimit := findEndOfElement(base, baseCursor)        // base element + its children (reused for every slice)

// ── EMIT THE ANCHOR (once, first) ──
if (diff0 has slicing and there is a "default with inline children" preceding the slices):
    // BRANCH A: recurse to build the default element e from base+diff0, then attach slicing
    e := processPaths(base window × diff0 window, slicing=done)
    e.setSlicing(diff0.slicing);  slicerElement := e;  consume diff0
else:
    // BRANCH B: build the anchor directly
    outcome := updateFromBase(updateURLs(base.copy()))
    outcome.slicing := diff0.hasSlicing ? diff0.slicing.copy() : makeExtensionSlicing()  // mark auto-added if synthesized
    addToResult(outcome)                                  // ← ANCHOR EMITTED HERE, before any slice
    slicerElement := outcome
    if diff0 has no sliceName:                            // diff0 is the group DEFAULT → merge it into the anchor, consume it
        updateFromDefinition(outcome, diff0); handle inline children / contentReference expansion

// ── EMIT EACH SLICE (by recursion) ──
for each remaining diff slice row d_i:
    processPaths( base[baseCursor..newBaseLimit] × diff[d_i .. findEndOfElement(diff,d_i)],
                  slicing = { done:true, elem:slicerElement, path:null }.withDiffs(diffMatches) )
baseCursor := newBaseLimit + 1;  diffCursor := lastSliceDiffLimit + 1
```

The anchor's `slicing` comes from the first diff row (or `makeExtensionSlicing()`); sibling diff
rows that also declare slicing are cross-checked with `slicingMatches` and produce
ERROR/INFORMATION *messages* (not exceptions) on mismatch.

## 7.6 Refining an already-sliced base (`processPathWithSlicedBase` family)

Reached from §6.4: the base element already carries slicing (profiling a profile, or a core
element that ships sliced). Sub-dispatch (PPP:1196): empty diff → pass-through; type-constraining
→ §7.6.3; else → default (§7.6.1). Contract: definition order preserved, slice names matched,
**new slices appended at the end, existing base slices may not be re-sliced** (PPP:1203-1207).

### 7.6.1 Default refinement (`processPathWithSlicedBaseDefault`, PPP:1225)

```
// 1. compatibility: orderMatches / discriminatorMatches / ruleMatches vs base.slicing (§7.3) — else fail
// 2. EMIT ANCHOR: outcome := base.copy(); updateFromSlicing(outcome.slicing, diff0.slicing);
//                 updateFromDefinition(outcome, diff0, trim=closed); addToResult(outcome)
// 3. reconcile EXISTING base slices (getSiblings), in base order:
for each base slice bs:
    if next diff slice names bs:  recurse to merge bs × its diff   // matched → refine
    else:                         copy bs through unchanged        // unmentioned → inherit verbatim
// 4. NEW slices (leftover diff rows), appended at the end:
if base.slicing == CLOSED and there are leftover diff slices and path not "[x]":
    fail THE_BASE_SNAPSHOT_MARKS_A_SLICING_AS_CLOSED               // cannot extend a closed slicing
for each leftover diff slice d:
    if d.sliceName equals an existing base slice name: fail NAMED_ITEMS_ARE_OUT_OF_ORDER
    outcome := base.copy(); outcome.slicing := null; outcome.min := 0
    addToResult(outcome); updateFromDefinition(outcome, d); recurse into children/type
```

Matching of diff slices to base slices is **positional and name-keyed**: a diff slice is consumed
only when its `sliceName` equals the current base slice's; otherwise the base slice is copied
through and the diff cursor holds.

### 7.6.2 Empty diff (`processPathWithSlicedBaseAndEmptyDiffMatches`, PPP:1657)

If the differential touches the subtree's children but not the element → copy the anchor, recurse
into children/type. If it says nothing at all → **copy the entire sliced group (anchor + all base
slices + children) verbatim, in base order**.

### 7.6.3 Type-constraining an already-sliced base (`...WhereDiffsConstrainTypes`, PPP:1494)

Emits a forced `type@$this / CLOSED / unordered` anchor, then one slice per constrained type,
matching to base type-slices where present and **re-emitting untouched base type-slices via a
synthetic empty differential** so they survive. Validations enforce the type-slice discriminator
shape (§7.4) and reject `min > 0` on one of several type slices. (Shares logic with Part VIII.)

## 7.7 Slice-body construction (`processSimplePathWithOneMatchingElementInDifferential`)

This is where a slice body is actually built — reached by the per-slice recursion of §7.5/§7.6
with `slicing.done = true`, so the lone diff match (the slice row) is treated as a plain element
(PPP:674-938). Slicing-relevant sequence:

```
template := base.copy()                          // merge(base, slicer) == base.copy() — §7.1 non-obvious fact
template := updateURLs(fixPath(template))
checkToSeeIfSlicingExists(diff0, template)        // lazily emit a missing anchor — §7.8
outcome.setSliceName(diff0.sliceName)
if diff0 has no min:                              // SLICE MIN DEFAULTING
    if (slicer == null or slicer.slicing.rules != CLOSED) and base has no sliceName
       and path not ".extension.value[x]":  outcome.min := 0
    elif slicer.slicing.rules == CLOSED and slicer has >1 slice:  outcome.min := 0
updateFromDefinition(outcome, diff0)              // apply the slice's own constraints (Part X)
outcome.setSlicing(null)                          // INV-S2
addToResult(outcome)
recurse into the slice's children / data type
```

> **Slice min defaulting (normative).** A slice whose differential omits `min` defaults to
> `min = 0`, so slices do not inherit the anchor's minimum. The anchor's overall minimum is
> reconciled separately by cardinality validation (§7.9).

## 7.8 Lazy anchor synthesis (`checkToSeeIfSlicingExists`, PPP:955)

If a slice row reaches §7.7 with **no anchor already emitted** for its path, an anchor is
synthesized on the spot:
- path ends `.extension` → emit anchor with `OPEN`, unordered, discriminator `value@url`;
- "jumping into type slicing" (`isJumpingIntoTypeSlicing`: path ends `[x]`, no sliceName/slicing
  on the template, the diff has a sliceName and a type) → emit anchor with `CLOSED`, unordered,
  discriminator `type@$this`;
- otherwise emit nothing.

This is why a profile can declare an extension slice without explicitly writing the extension
slicing anchor — the generator supplies it.

## 7.9 Slice cardinality validation (`ElementDefinitionCounter`, Pass A4.4)

After the walk, slice cardinalities are reconciled against their anchor (PU:976-1036, using
`ElementDefinitionCounter`, PU:157). Per anchor, accumulate over its slices:
- `countMin += slice.min`; `countMax += slice.max` (saturating at unbounded once any slice is `*`);
- duplicate `sliceName` → error.

Then (only for a **repeating** anchor, i.e. base `max != 1`):
- `checkMin()` returns the summed min if it **exceeds** the anchor's min (else −1). On violation:
  if the anchor's slicing was generator-auto-added (`SNAPSHOT_auto_added_slicing`), **set the
  anchor's min to the sum** (auto-correct); otherwise emit a message (ERROR if `forPublication`,
  else INFORMATION).
- `checkMax()` returns the summed max if it **exceeds** the anchor's max (else −1) → INFORMATION.
- `checkMinMax()` (`countMin ≤ countMax`) → WARNING if violated.

This realizes post-condition **PC-4** (§3.8).

## 7.10 The slicing context flags (`PathSlicingParams`)

The walk threads a slicing context `{ done, elementDefinition (the anchor), path, slices }`:
- **`path`** — when `currentBasePath == slicing.path`, an otherwise-sliced base element is routed
  to the *simple* path (§6.2): "we already established this slicing; treat the anchor as a plain
  element, don't re-enter slicing."
- **`done`** — consumed by `oneMatchingElementInDifferential` (§6.3): when set, a single diff
  match is unconditionally a plain element/slice body rather than a new slicing opener. This is
  what lets the per-slice recursions (which set `done = true`) emit bodies without re-slicing.
- **`elementDefinition`** / **`slices`** — the anchor and sibling slice diffs, used for slice min
  defaulting (§7.7).

## 7.11 Error catalog (slicing)

| Trigger | Outcome |
|---|---|
| Slice a non-repeating element to >1 (not type-slice, not capped-at-1) | fail `ATTEMPT_TO_A_SLICE_AN_ELEMENT_THAT_DOES_NOT_REPEAT` |
| Diff opens a slice but states no `slicing` (non-extension) | fail `DIFFERENTIAL_DOES_NOT_HAVE_A_SLICE` |
| Diff slicing order/discriminator/rule incompatible with base | fail `SLICING_RULES_..._ORDER` / `_DISCRIMINATOR` / `_RULE` |
| Extend a `CLOSED` base slicing with new slices (non-`[x]`) | fail `THE_BASE_SNAPSHOT_MARKS_A_SLICING_AS_CLOSED` |
| New slice duplicates an existing base slice name | fail `NAMED_ITEMS_ARE_OUT_OF_ORDER_IN_THE_SLICE` |
| Type slice with `ordered=true` / wrong discriminator count/type/path | fail `..._SLICINGORDERED_TRUE` / `..._SLICINGDISCRIMINATOR*` |
| `min > 0` on one of multiple type slices | fail `INVALID_SLICING_..._MIN_1` |
| Slice name mismatch / slice has >1 type / wrong type | fail `SLICE_NAME_MUST_BE` / `SLICE_FOR_TYPE_HAS_MORE_THAN_ONE_TYPE` / `..._HAS_WRONG_TYPE` |
| Emitted element path escapes `resultPathBase` | fail `ADDING_WRONG_PATH` |
| Nested non-extension slicing inside shared group props (preprocessor) | warn `UNSUPPORTED_SLICING_COMPLEXITY`, abort pushdown |
| Multiple type profiles on a new slice / unsupported reslice depth | `"Not handled"` errors |

## 7.12 Worked example — `Observation.component` blood pressure

```
base.snapshot:   Observation.component            [0..*]
                 Observation.component.code        [1..1]
                 Observation.component.value[x]    [0..1]

derived.diff:    Observation.component             (slicing: discriminator value@code, rules=open)
                 Observation.component             (sliceName=systolic; code fixed 8480-6; value[x]→Quantity)
                 Observation.component             (sliceName=diastolic; code fixed 8462-4; value[x]→Quantity)
```

1. Walk reaches base `Observation.component`; `getDiffMatches` returns **3** rows (same path) →
   not "one match" → not type-constraining → `processSimplePathDefault` (§7.5).
2. `newBaseLimit` spans `component` + `code` + `value[x]`.
3. Anchor emitted first: `component.copy()` with `slicing = {value@code, open}` (INV-S1).
4. Slice loop, two recursions over the *same* base window:
   - `systolic`: recursion with `done=true` → `processSimplePathWithOneMatchingElementInDifferential`
     → clone base `component`, `setSliceName(systolic)`, default `min=0`, merge fixed code + type
     constraint, `setSlicing(null)` (INV-S2), emit; recurse into `code`/`value[x]` children.
   - `diastolic`: same.
5. A4.4 cardinality check: anchor `component [0..*]` repeats; slice mins sum to 0 — no violation.

Result: anchor + systolic(+children) + diastolic(+children), in base order (INV-S3).

---

# Part VIII — Type and Choice (`[x]`) Expansion

A polymorphic `[x]` element and a complex-typed element both raise the same question: *what are
this element's children, and how does a profile constrain them by type?* Layer A answers with
**two distinct mechanisms**, both keyed off the `[x]`/type machinery but producing different
output:

- **Type slicing** (§8.1–8.4): a profile splits a choice/multi-type element into per-type
  cases (`value[x]` → a `valueQuantity` slice, a `valueString` slice). Output: a slicing group
  with a `type@$this` discriminator.
- **Type expansion** (§8.5–8.8): a profile constrains the *children* of a complex type the base
  did not enumerate (`Observation.value[x]` as a `Quantity`, reaching `…value`, `…unit`). Output:
  the type's child elements, re-homed under the current path.

The two compose: a `valueQuantity` type slice that further constrains `valueQuantity.unit` uses
type slicing to make the slice and type expansion to reach into `Quantity`. (Anchors: PU:1612,
1788, 1806, 2073, 2117, 3590; PPP:493, 828, 1043, 1077; `TypeSlice`, `BaseTypeSlice`.)

## 8.1 Detecting type slicing: `diffsConstrainTypes`

The dispatcher (§6.3.3) calls `diffsConstrainTypes(diffMatches, cPath, typeList)` (PU:1806) to
decide whether a set of differential rows constrain *individual types* of a choice element, and
to build the `typeList` of `(differential row, resolved type)` pairs.

```
diffsConstrainTypes(diffMatches, cPath, typeList):
    if not diffMatches[0].path ends "[x]"  and not cPath ends "[x]":  return false   // GATE
    rn := stem(tail(cPath))                              // base choice name minus "[x]", e.g. "value"
    for ed in diffMatches:
        n := tail(ed.path);  if not n startsWith rn: return false
        s := n[len(rn):]                                 // suffix: "" | "[x]" | "Quantity" | "String" | ...
        if s contains ".":  continue                     // a child of a type slice — ignored here
        // derive this row's type:
        if ed.hasSliceName and ed.type.size == 1:    add TypeSlice(ed, ed.type[0].code)         // declared wins
        elif ed.hasSliceName and ed.type.size == 0:  derive from suffix s (datatype, else primitive
                                                       via uncapitalize), else from sliceName tail
        elif not ed.hasSliceName and s != "[x]":     derive from s (datatype; isConstrainedDataType→baseType
                                                       e.g. SimpleQuantity→Quantity; else primitive)
        elif not ed.hasSliceName and s == "[x]":     add TypeSlice(ed, null)   // the bare choice row (anchor)
    return true
```

**Normative notes:**
- **Type-derivation priority:** an explicitly declared single `type` wins; otherwise the type is
  read from the *renamed suffix* (`valueQuantity` → `Quantity`); otherwise from the `sliceName`
  tail. Primitives are matched via `uncapitalize` and stored lowercased (`valueString` → `string`).
  Constrained datatypes collapse to their base (`SimpleQuantity` → `Quantity`) only in the
  no-sliceName branch.
- `diffsConstrainTypes` may return **true with an empty `typeList`** (e.g. only the bare
  `value[x]` row matched). It does **not** require ≥2 matches.
- Rows whose suffix contains `.` (children of a type slice) are skipped here and handled by
  recursion.

`TypeSlice` = `(defn: the differential row, type: resolved code or null)`. `BaseTypeSlice` =
`(defn, type, start, end, handled)` — a pre-existing type slice found in the *base* snapshot
(`findBaseSlices`, PU:1742), with an index span and a consumed-flag for reconciliation.

## 8.2 Emitting type slices: `processSimplePathWhereDiffsConstrainTypes` (PPP:493)

```
newBaseLimit := findEndOfElement(base, baseCursor)            // choice element + children; reused per slice
shortCut := typeList non-empty and typeList[0].type != null  // diff dove straight to concrete types

// (A) ensure a slicing ANCHOR row exists at the head:
if shortCut:                                                  // synthesize one
    anchor.path := determineTypeSlicePath(path, cPath)        // normalize back to ".../value[x]"
    if version < R4 or not newSlicingProcessing:              // R3/legacy: anchor lists exactly the sliced types
        for ts in typeList: anchor.addType(ts.type)
    // R4+: anchor lists NO types (all base types still allowed)
    anchor.slicing := { discriminator: [type@$this], rules: CLOSED, ordered: false }
    insert anchor at head of diffMatches and differential;  elementToRemove := anchor
else:                                                         // explicit anchor present
    if tail(cPath) != tail(path): fail ED_PATH_WRONG_TYPE_MATCH

// (B) validate anchor slicing: ordered=true → fail; discriminator must be exactly one type@$this
// (C) normalize each type slice: sliceName := stem + Capitalize(type) (e.g. "valueQuantity");
//     ensure exactly one matching type; mismatches fail (SLICE_NAME_MUST_BE / SLICE_FOR_TYPE_*)

// (D) emit the anchor by recursion, then FORCE its slicing:
e := processPaths(base window × anchor diff window, slicing = done)
if e is null: fail DID_NOT_FIND_TYPE_ROOT
e.slicing := { discriminator: [type@$this], rules: CLOSED, ordered: false }   // CLOSED always, overriding diff
slicerElement := e.copy();  if e.type.size > 1: slicerElement.min := 0

// (E) emit each concrete type slice by recursion over the SAME base window:
for each type-slice row d_i:
    if d_i.min > 0: if other slices exist → fail INVALID_SLICING_..._MIN_1; else e.min := 1; fixedType := type(d_i)
    processPaths(base window × d_i diff window, slicing = {done, anchor:e}.withDiffs(diffMatches))
    if more than one slice: d_i outcome .min := 0

// (F) remove the synthesized anchor from the differential (if any)
// (G) prune base element types to fixedType (if a single mandatory slice)
// (H) open/closed decision: leftover (unsliced) allowed types?
//     Extension.value[x] shortcut → DROP the leftover types from the element;
//     otherwise → flip anchor.slicing.rules := OPEN  (unmentioned types remain valid)
baseCursor := newBaseLimit + 1;  diffCursor := newDiffLimit + 1
```

**Key normative points:**
- **Type slicing is always CLOSED by construction** (forced at emit, overriding whatever the
  differential wrote), *unless* §8.2(H) finds unsliced types still allowed, in which case it is
  reopened to OPEN. A differential "open" on a type slice means only "I am not constraining the
  unmentioned types."
- **The R3/legacy vs R4+ anchor difference** is observable: pre-R4 the synthesized anchor lists
  exactly the sliced types; R4+ lists none. This is gated by the `newSlicingProcessing` knob
  (§2.7), set true for R4+ by the publisher.
- **Cardinality:** the slicer is optional (`min 0`) when the base choice has >1 type; a single
  mandatory slice raises the element's `min` to 1, but two mandatory type slices are illegal.
- The already-sliced-base counterpart is §7.6.3.

## 8.3 Output structure: type slice vs. value slice

| Aspect | **Type slice** | **Ordinary value slice** |
|---|---|---|
| slice path | stays `value[x]` (canonical choice path) | the element's path |
| discriminator | exactly `type@$this` (forced) | value/pattern path from the diff |
| `slicing.rules` | CLOSED (OPEN only if unsliced types remain) | the differential's own rules |
| `sliceName` | synthesized `stem + Capitalize(type)` (e.g. `valueQuantity`) | author-supplied |
| `type` | exactly one, matching the slice | unconstrained by the slice machinery |
| base element `type[]` | pruned/trimmed or left with OPEN slicing | untouched |

These markers are how a consumer distinguishes the two in a finished snapshot.

## 8.4 Note on `determineTypeSlicePath` (PU:1788)

`determineTypeSlicePath(path, cPath) = head(path) + "." + tail(cPath)` — rewrites a concrete diff
path (`Observation.valueQuantity`) back onto the canonical `[x]` leaf taken from the base
(`Observation.value[x]`). This is why type-slice anchors and slices keep the `value[x]` path.

## 8.5 When type *expansion* happens (the gate)

The walk descends into a type's own snapshot to materialize children iff **the differential
addresses children here** *and* **the base does not already enumerate them**:

```
walkIntoType  ⇔  hasInnerDiffMatches(currentBasePath, ...)   // some diff path startsWith currentBasePath + "."
              and not baseHasChildren(currentBase)           // next base element is not a child of this one
              and outcome.path contains "."                  // not the root
              and ( isDataType(outcome.type) or isBaseResource(outcome.type) or hasContentReference() )
```

The type gates (PU):
- **`isDataType(List)`** (2117) — **all-or-nothing**: every type's code must be a datatype or
  primitive, else false.
- **`isDataType(String)`** (3590) — `kind == COMPLEXTYPE and derivation == SPECIALIZATION` (a
  *base* complex type, not a constraint), with a hardcoded fallback list when the SD isn't loaded.
- **`isPrimitive(String)`** (3617) — `kind == PRIMITIVETYPE` (fallback list otherwise).

A primitive leaf with no further diff children is simply copied; no recursion.

## 8.6 Resolving the type to walk into

**`getTypeForElement`** (PU:1612) picks the single structure to descend into:
```
if outcome.type.size == 0:  fail (contentReference → ...CONTENT_REFERENCE...; else ...NO_CHILDREN_AND_NO_TYPES...)
if outcome.type.size > 1:   for each type: if code != "Reference" → fail ...CHILDREN_AND_MULTIPLE_TYPES...
dt := getProfileForDataType(outcome.type[0], webUrl, srcSD)        // ALWAYS the first type
if dt is null: fail UNKNOWN_TYPE_AT_
```
> **Why multi-type is refused unless all are `Reference`.** Constraining children requires a
> *single* concrete child structure. `Reference` is the one exception: every `Reference{...}`
> has the identical child structure (`reference`/`type`/`identifier`/`display`) regardless of
> target, so descending is unambiguous. Any other heterogeneous type set has no single child
> structure.

**`getProfileForDataType`** (PU:2073) resolves the SD: the first declared `profile` (else the
bare type via `fetchTypeDefinition`). It has one recursion site — the **cross-version (xver)
branch** (PU:2078-2081): for a synthetic cross-version Extension profile that has no snapshot
yet, it calls `generateSnapshot(Extension, sd, …)` recursively before the walk can iterate it.

> **Normative:** walking into a type triggers nested snapshot generation **only** in the xver
> branch (an unsnapshotted synthetic extension). For all ordinary types the resolved SD is
> already snapshotted and no generation occurs. (This is the second source of bottom-up
> recursion, after §3.2's base backfill — both are bounded by the `snapshotStack` guard.)

## 8.7 The expansion recursion (root-skip + re-homing)

Every walk-into-a-type site (there are five, structurally identical — PPP:1077, 377, 828, 1260,
1658) uses the canonical recursion:

```
start := diffCursor
while differential[diffCursor].path startsWith currentBasePath + ".":  diffCursor++   // consume child diffs
processPaths over a NEW frame:
    base        := dt.snapshot
    baseCursor  := 1                       // ★ SKIP the type's root element[0] (e.g. "Quantity")
    diffCursor  := start
    baseLimit   := |dt.snapshot| - 1       // the whole type
    diffLimit   := diffCursor - 1           // only the consumed child window
    contextPathSource := currentBasePath    // base side, e.g. "Observation.value[x]"
    contextPathTarget := outcome.path        // result side, e.g. "Observation.valueQuantity"
    resultPathBase    := unchanged           // keeps output rooted in the profile
    slicing           := fresh (empty)
```

- **Root skip:** `baseCursor = 1` means the type's root (`Quantity`) is never emitted — the
  current element (`Observation.valueQuantity`) already stands in for it.
- **Re-homing:** child paths are rewritten by `fixedPathSource`/`fixedPathDest` (§5.5) using the
  two context paths: `Quantity.value` → match against `Observation.value[x].value` (source) and
  emit as `Observation.valueQuantity.value` (target). The parent's own `[x]` → concrete rename
  (`value[x]` → `valueQuantity`) is done earlier in the merge, not here.
- When a `redirector` is active (contentReference, Part IX) the recursion additionally pushes a
  redirector frame and may switch the source structure to the reference target.

## 8.8 Error catalog (type handling)

| Trigger | Outcome |
|---|---|
| Element has 0 types and no contentReference | fail `..._HAS_NO_CHILDREN__AND_NO_TYPES_IN_PROFILE_` |
| Element has 0 types but a contentReference (wrong context) | fail `UNABLE_TO_RESOLVE_CONTENT_REFERENCE...` |
| >1 type, not all `Reference`, with child content | fail `..._HAS_CHILDREN__AND_MULTIPLE_TYPES...` |
| Type profile unresolvable (`dt == null`) | fail `UNKNOWN_TYPE__AT_` / `..._CANT_FIND_TYPE` |
| Diff walks into an element whose base has no children and whose type count ≠ 1 | fail `DIFFERENTIAL_WALKS_INTO..._NOT_A_SINGLE_FIXED_TYPE` |
| Type-slice anchor `ordered=true` / wrong discriminator | fail `..._SLICINGORDERED_TRUE` / `..._SLICINGDISCRIMINATOR*` |
| Type-slice name ≠ `stem+Capitalize(type)` (no auto-fix) | fail `SLICE_NAME_MUST_BE` |
| Type slice has >1 type / wrong type | fail `SLICE_FOR_TYPE_HAS_MORE_THAN_ONE_TYPE` / `..._HAS_WRONG_TYPE` |
| `min > 0` on one of several type slices | fail `INVALID_SLICING_..._MIN_1` |
| Re-homed child path escapes `resultPathBase` | fail `ADDING_WRONG_PATH` |
| >1 profile canonical on a type | error `"Not handled: multiple profiles"` |

## 8.9 Worked example — `Observation.value[x]` profiled as `Quantity`

```
base.snapshot:   Observation.value[x]   [0..1]   type: Quantity | string | CodeableConcept | ...
                 (Quantity's children are NOT enumerated under Observation)

derived.diff:    Observation.valueQuantity            (constrain type to Quantity)
                 Observation.valueQuantity.unit  [1..1] (constrain a child)
```

1. At base `Observation.value[x]`, `getDiffMatches` returns the `valueQuantity` rows.
   `diffsConstrainTypes` fires (suffix `Quantity` after stem `value`) → `typeList = [(row, Quantity)]`,
   `shortCut = true`.
2. **Type slice** (§8.2): synthesize a `type@$this`/CLOSED anchor at `Observation.value[x]`;
   emit the `valueQuantity` slice (path stays `value[x]`, `sliceName=valueQuantity`, one type
   `Quantity`); base types pruned/opened per §8.2(H).
3. **Type expansion** (§8.5–8.7): the slice has a child diff (`…unit`) but the base doesn't
   enumerate `Quantity`'s children → `getTypeForElement` resolves the `Quantity` SD → recurse
   over `Quantity.snapshot` from index 1 with `contextPathSource=Observation.value[x]`,
   `contextPathTarget=Observation.valueQuantity`. `Quantity.unit` → emitted as
   `Observation.valueQuantity.unit` and merged with the `[1..1]` constraint; `Quantity.value`,
   `Quantity.system`, etc. emitted unchanged.

---

# Part IX — contentReference Resolution

`contentReference` is how FHIR expresses **recursive structure**: an element declares that its
children are identical to some other element's, rather than re-listing them. `Questionnaire.item`
has children, and `Questionnaire.item.item` simply points back at `Questionnaire.item` with
`contentReference = "#Questionnaire.item"`. This part specifies how Layer A resolves such
references, rewrites the borrowed paths, and — most importantly — why it terminates on genuinely
recursive structures. (Anchors: PU:510, 1852, 1870, 3524; PPP:455, 459; `ElementRedirection`.)

## 9.1 Conceptual model and the central insight

A naive expander would loop forever: expanding `item.item` into a copy of `item` produces another
`item.item`, and so on. Layer A avoids this with one principle:

> **INSIGHT (lazy, diff-bounded expansion).** A `contentReference` is **never expanded eagerly.**
> It is expanded *only* where the differential actually constrains a child beneath it, and only
> as deep as the differential reaches. Wherever the differential is silent, the output element
> **re-emits the `contentReference` verbatim**, unexpanded. Because a differential is a finite
> document, expansion is finite.

So the base `Questionnaire` snapshot is finite (`item.item` is one element carrying a
contentReference, not an infinite tree), and a profile's snapshot expands the recursion only to
the depth the profile cares about, leaving a `contentReference` at the frontier.

## 9.2 Shape and invariant

`ElementDefinition.contentReference` is a `uri` in one of two forms:
- **internal** — `#<id>`, where `<id>` is an element id in the *same* snapshot (e.g. `#Questionnaire.item`);
- **external** — `<url>#<id>`, an element id inside *another* StructureDefinition's snapshot.

> **INV-CR (no type).** An element carrying a `contentReference` has **no `type`**. The algorithm
> both relies on this and defensively re-asserts it: after processing, `if hasContentReference and
> hasType → clear type` (PPP:1477). Dispatch branches treat "has contentReference" and "has type"
> as mutually exclusive (PPP:858 vs 897; the `type.size==0 && !hasContentReference` case is an
> error, PPP:1094).

## 9.3 Resolution

**`getElementById(source, elements, contentRefElement)`** (PU:3524) resolves a reference to an
`ElementDefinitionResolution` = `(target StructureDefinition, target ElementDefinition)`:
```
cr := contentRef.value
if cr has "#" but doesn't start with "#":             // external
    url := cr before "#";  cr := "#" + (cr after "#")
    if url != source.url:  source := resolve(url);  if null → (caller fails);  elements := source.snapshot
for ed in elements:
    if ("#" + ed.id) == cr:  return (source, ed)       // anchored match on "#id"
return null
```
For internal refs the source SD is unchanged; for external refs it is swapped to the referenced
SD so the caller knows which snapshot to walk.

**`getChildMap`'s contentReference branch** (PU:510-548) resolves and recurses to obtain the
target's children — but only behind the **already-expanded guard**:
```
walksIntoElement := the next snapshot element is a child of `element`
if element.hasContentReference and not walksIntoElement:
    resolve (#id internal → this snapshot; url#id external → resolve url); recurse getChildMap(target, chaseTypes=true)
else:
    read the inline children normally
```
> **The guard matters:** if the snapshot generator already materialized the referenced subtree
> inline (the next element *is* a child), `getChildMap` must read those inline children rather
> than re-resolving the reference — preventing double expansion.

## 9.4 The two expansion paths

**(a) Referencing element resolved during the walk (`replaceFromContentReference` + redirector
recursion).** When the differential walks into a contentReference element, the walk:
```
replaceFromContentReference(outcome, target):       // PU:1870
    outcome.contentReference := null
    outcome.type.clear();  outcome.type.addAll(target.type)   // adopt target's TYPES (INV-CR)
// then recurse processPaths over the target's subtree, pushing a redirector frame (§9.5)
```
`replaceFromContentReference` adopts the target's *types* in place; the **children** come from the
subsequent recursive `processPaths` over the target's subtree, not from this call.

**(b) Base element carries a contentReference (`resolveContentReference` + inline copy).** When
the *base* element being profiled has a contentReference and the differential walks into it
(PPP:403-419), the walk finds the target by scanning **backward, skipping slices**:
```
resolveContentReference(base, currentBase):          // PPP:459
    path := currentBase.contentReference after "#"
    for res from indexOf(currentBase)-1 down to 0:
        if base[res].path == path and not base[res].hasSliceName():  return res   // the canonical element, not a slice
    return -1                                          // not found → caller copies an empty range (no-op)
```
> The `!hasSliceName()` test is essential: slices share their base element's path, so without it
> the scan could land on a slice instead of the canonical target.

Each descendant of the target is then copied into the output (id nulled) with its path rewritten
by `fixForRedirect` then `fixedPathDest` (§5.5).

## 9.5 The redirector stack

`ElementRedirection` is a 2-field frame `{ element: the produced outcome, path: the source path
at the redirect site }`. `redirectorStack(redirector, outcome, path)` (PU:1852) pushes a frame
immutably (copy-and-append). A frame is pushed whenever the walk recurses **into** a resolved
contentReference target (PPP:876, 893, 1137, 1154) or into a datatype under an existing redirect
(PPP:1187).

The top frame is consumed by the path rewriters (§5.5):
- `fixedPathSource` — with a non-empty redirector, re-roots a path *from the target's namespace
  back to the referencing namespace* (using `redirector.top.path` + the tail).
- `fixedPathDest` — symmetric, strips the `redirectSource` root and re-roots under the
  destination path. (Note: the top frame's own path is intentionally *not* re-inserted here —
  PU:2064 has it commented out; only stack non-emptiness selects the stripping branch.)

## 9.6 `fixForRedirect` and its latent fragility

```
fixForRedirect(path, rootPath, redirect):  return path.replace(redirect, rootPath)   // PPP:455
```
It re-roots a descendant path from the *referenced* element (`redirect`, e.g.
`Questionnaire.item`) onto the *referencing* element (`rootPath`, e.g. `Questionnaire.item.item`):
`Questionnaire.item.text` → `Questionnaire.item.item.text`.

> **Latent issue (flag, do not "fix" silently).** This is an unanchored, global
> `String.replace` — not a segment-anchored prefix re-root. It rewrites *every* occurrence of the
> `redirect` substring and ignores `.`-segment boundaries. For genuinely recursive paths
> (`item.item.item…`) multiple occurrences are all rewritten, which can corrupt deeper segments;
> and a `redirect` that is a substring of a longer token would match mid-token. The intended
> semantics are `if path startsWith redirect+"." then rootPath + path[len(redirect):]`. It works
> in the common Questionnaire case because the first occurrence is the relevant one, but a
> conforming reimplementation SHOULD use the anchored form.

## 9.7 Termination (normative)

> **THEOREM (termination of contentReference handling).** Snapshot generation terminates on any
> contentReference graph, including self-recursive ones, because:
> 1. **No eager expansion.** A contentReference is expanded only when `hasInnerDiffMatches` holds
>    (the differential constrains a child beneath it).
> 2. **Diff-bounded depth.** Each expansion recursion sets `diffLimit` to the triggering
>    differential window (`withDiffLimit(diffCursor-1)`), so it can descend only as deep as the
>    differential explicitly addresses. Each recursion consumes a strictly smaller diff window.
> 3. **Emit-the-reference-when-silent.** Where the differential says nothing,
>    `processSimplePathWithEmptyDiffMatches` copies the base element **with its contentReference
>    intact** and descends no further.
>
> Since a differential is finite, (1)+(2)+(3) bound the total expansion. The recursive element
> appears in the output *as a contentReference at the frontier*, never as an infinite subtree.

## 9.8 Error catalog (contentReference)

| Trigger | Outcome |
|---|---|
| External ref, target SD not found (`getChildMap`) | fail `"unable to process contentReference ..."` (PU:535) |
| Malformed value (neither `#…` nor `…#…`) | fail `"unable to process contentReference ..."` (PU:541) |
| Internal id not found in target snapshot (`getChildMap`) | fail `UNABLE_TO_RESOLVE_NAME_REFERENCE__AT_PATH_` (PU:548) |
| `getElementById` returns null during the walk | fail `UNABLE_TO_RESOLVE_REFERENCE_TO_` (PPP:861, 1121) |
| Element ends up with neither type nor contentReference (non-root) | fail `_HAS_NO_CHILDREN__AND_NO_TYPES...` / `NOT_DONE_YET` (PPP:1095, 374) |
| `resolveContentReference` returns −1 | *not* an exception — caller copies an empty range (silent no-op) |

## 9.9 Worked example — `Questionnaire.item.item` constrained

```
base.snapshot:   Questionnaire.item              [0..*]
                 Questionnaire.item.linkId        [1..1]
                 Questionnaire.item.text          [0..1]
                 ... Questionnaire.item.item      [0..*]  contentReference = "#Questionnaire.item"   (no type)

derived.diff:    Questionnaire.item.item.text     [1..1]   (require text on nested items)
```

- At base `Questionnaire.item.item` (a contentReference, no type), `hasInnerDiffMatches` is true
  (the diff has `…item.item.text`), so the walk expands: `resolveContentReference` scans backward,
  skips the `item` slices if any, finds canonical `Questionnaire.item`, and copies its descendants,
  rewriting `Questionnaire.item.text` → `Questionnaire.item.item.text` (via `fixForRedirect` +
  `fixedPathDest`), merging the `[1..1]` constraint.
- The next recursion level, `Questionnaire.item.item.item`, is **not** addressed by the
  differential → it is emitted as a single element with `contentReference = "#Questionnaire.item"`,
  unexpanded. Termination achieved (§9.7).

---

# Part X — The Element Merge (`updateFromDefinition`)

Every prior part eventually says "merge the differential element onto the base element (Part X)."
This is that merge: `updateFromDefinition` (PU:2585, ~530 lines), the single most intricate method
in Layer A. It is **not** a uniform overwrite — each property has its own precedence rule (replace,
accumulate, or narrow-only), and the method mutates *both* its inputs. (Anchors: PU:2585, 3134,
3262, 3362; constants PU:232, 428.)

## 10.1 Mental model

```
updateFromDefinition(dest, source, ..., trimDifferential, ..., srcSD, derivedSrc, path, mappings, fromSlicer)
    base    := dest        // a CLONE of the base element; we WRITE INTO it
    derived := source      // the differential element; we COPY FROM it "over the top"
```
Orienting comment (PU:2587): *"we start with a clone of the base profile ('dest') and we copy
from the profile ('source') over the top for anything the source has."* Two provenance stamps are
planted immediately:
- `source.userData[SNAPSHOT_GENERATED_IN_SNAPSHOT] := dest` (PU:2586) — the diff→output link that
  PC-1 later checks (§3.8).
- `derived.userData[SNAPSHOT_DERIVATION_POINTER] := base` (PU:2591) — every differential element
  points at the base element it constrained.

Structure: a **preamble** (extension inheritance, obligations, profile-on-type resolution), then
one large `if derived != null` block that is a **flat sequence of per-property `if derived.hasX()`
guards**, then a **postamble** (fixed/pattern type checks).

## 10.2 The recurring three-way precedence idiom

Most scalar properties follow this skeleton (e.g. `short`, PU:2694):
```
if derived.hasX():
    if not compareDeep(derived.X, base.X):     // they DIFFER
        base.X := derived.X.copy()             //   → derived wins (copy / merge / validate-then-set)
    elif trimDifferential:                     // EQUAL, and we are minimizing the differential
        derived.X := null                      //   → DELETE the property from the differential in place
    else:                                      // EQUAL, not trimming
        derived.X.userData[SNAPSHOT_DERIVATION_EQUALS] := true   // mark redundant-equals-base
```
So: **differ → derived overrides; equal+trim → the differential is pruned; equal+keep → marked
redundant.** This idiom is why `trimDifferential` mutates the caller's differential (§10.9).

## 10.3 Property merge categories

| Category | Properties (line) | Rule |
|---|---|---|
| **Replace wholesale** | `type` (3053), `binding` (2929, overlay-rebuild), `fixed[x]` (2779), `pattern[x]` (2788), `maxLength` (2829), `minValue`/`maxValue` (2847/2838) | differ → base value cleared and replaced by derived's. Types are **not** merged element-wise (§10.5). |
| **Accumulate (additive)** | `constraint` (3084), `condition` (3099), `alias` (2744), `valueAlternatives` (2893), `example` (2798), `mapping` (3082) | derived entries appended; never replace. Constraints: *"cumulative, there is no replacing"* — add only if `!base.hasConstraint(key)` (base wins on key clash); existing base constraints tagged `SNAPSHOT_IS_DERIVED` and given a source URL. |
| **Narrow-only (validated)** | `min` (2757), `max` (2768), binding `strength` (2956), `mustSupport` (2872), `mustHaveValue` (2884) | derived is applied, but widening emits an ERROR message (see §10.4, §10.6). |
| **Overwrite if present** | `sliceName` (2690, unconditional), `short` (2694), `definition` (2703), `comment` (2712), `label` (2721), `requirements` (2730) | standard idiom; the text fields use the `"..."` append convention (§10.7). |
| **Inherited, not mergeable** | `defaultValue`, `meaningWhenMissing` (no block) | profiles cannot change these — silently inherited from base. |
| **Extension-only** | `isModifier`/`isModifierReason` (2906) | applied **only when the element is an extension**; ordinary profiles cannot change isModifier. |

## 10.4 Cardinality (min/max) — narrow but don't block

```
min (2757): if derived.min < base.min and not derived.hasSliceName():  ERROR "derived min cannot be less than base min"
            base.min := derived.min                       // set REGARDLESS of the error
max (2768): if isLargerMax(derived.max, base.max):        ERROR "derived max cannot be greater than base max"
            base.max := derived.max                       // set REGARDLESS
```
> **Normative subtlety:** the narrowing checks emit ERROR ValidationMessages but **do not block
> the assignment** — the base is updated to the derived value either way. Slices are exempt from
> the min check ("in a slice, minimum cardinality rules do not apply"). `isLargerMax` (PU:3380):
> base `*` ⇒ never larger; derived `*` ⇒ larger; else integer compare.

## 10.5 Types — wholesale replace + derivation check + bindable guard

```
if derived.hasType() and types differ:
    if base.hasType():
        for ts in derived.type:  checkTypeDerivation(...)     // narrowing legality (PU:3262)
    base.type.clear();  base.type.addAll(derived.type.copy()) // ★ WHOLESALE REPLACE
```
- **`checkTypeDerivation`** (PU:3262): each derived type must match a base type — by working-code
  equality, or by walking the derived type's `baseDefinition` chain to an abstract/logical base
  type. On match it copies must-support/pattern/obligation extensions from the base type and, for
  `targetProfile`s, verifies each via `sdConformsToTargets`. No match across all base types →
  `fail ILLEGAL_CONSTRAINED_TYPE` (unless ignorable-exceptions suppressed).
- **Bindable-type guard ("task 8477", PU:3106):** after the type merge, `if dest.hasBinding() and
  not hasBindableType(dest): dest.binding := null`. So narrowing types down to non-bindable ones
  (e.g. to `integer`) **silently deletes the inherited binding**. `hasBindableType` (PU:3362):
  type ∈ {Coding, CodeableConcept, Quantity, uri, string, code, CodeableReference}, or the type's
  SD declares a binding style / `can-bind` characteristic.

## 10.6 Binding — overlay rebuild, strength narrowing, subset check

Binding is rebuilt as a copy of the base binding with derived facets layered on (PU:2929-3037):
```
nb := base.binding.copy();  (clear nb extensions unless COPY_BINDING_EXTENSIONS — which is false, PU:428)
nb.description := null;  copy derived binding extensions;  override strength/description/valueSet from derived
base.binding := nb
```
Rules:
- **Strength can only narrow** (PU:2956): `base REQUIRED and derived not REQUIRED → ERROR` "illegal
  attempt to change the binding." (The historical hard `throw` is now a message.)
- **Required-subset check** (PU:2960): when both are REQUIRED with value sets, both are expanded and
  derived must be a subset of base (`checkSubset`) → ERROR if not; WARNING when a value set can't be
  located/expanded, or expansion is too costly/too large to check.
- **Extension inheritance:** binding extensions in `NON_INHERITED_ED_URLS` (PU:232 — binding
  definition, standards-status, fmm, wg, etc.) are stripped on inheritance; obligation extensions
  are *not* in that list, so they inherit.

## 10.7 The `"..."` text-append convention

Text fields (`definition`, `comment`, `requirements` via `mergeMarkdown` PU:3134; `label` via
`mergeStrings` PU:3152) support **append** instead of replace:
```
if derived text starts with "...":  merged := appendDerivedTextToBase(base text, derived text)
else if derived empty:              merged := base text          // inherit
else:                               merged := derived text        // override
// then merge base extensions in; translation extensions matched PER-LANGUAGE (findMatchingExtension, PU:3170)
```
So a profile author writing `"...and must be LOINC"` *extends* the inherited definition rather than
replacing it. Translation (`EXT_TRANSLATION`) extensions are merged independently per `lang`.

> **Implementation quirk to preserve carefully:** `mergeMarkdown` and `mergeStrings` pass the
> base/derived arguments to `appendDerivedTextToBase` in **opposite order** (PU:3139 vs 3157). A
> reimplementation should match field-by-field behavior rather than assume a single convention.

## 10.8 The profile-on-type override hack (PU:2619-2688)

When the differential's type carries a `profile` that resolves to a **resource** or **Extension**
StructureDefinition, the base element's *documentation* is overwritten **from that profile's root
element** (PU:2650-2671): `definition`, `binding.description`, `short`, `comment`, `requirements`
(URL-rewritten via `processRelativeUrls` against the profile's web root), and `alias`/`mapping`
(cleared then replaced).

> **The deliberate disable branch (PU:2643):** if the resolved profile is *not* Extension-typed and
> its kind is *not* RESOURCE/LOGICAL, the code sets `profile := null; msg := false` — *"we sometimes
> want the details from the profile to override the inherited attributes, and sometimes not."* So a
> profile on an ordinary datatype does **not** override the documentation. This asymmetry is
> intentional and load-bearing.

For a cross-version (xver) profile URL, resolution may recursively `generateSnapshot(Extension,
profile, …)` (PU:2638) to materialize the synthetic extension before applying the override — the
same bottom-up recursion noted in §8.6, bounded by `snapshotStack`.

## 10.9 Obligations and mappings

- **Obligations (PU:2608):** obligation extensions (`EXT_OBLIGATION_CORE`/`_TOOLS`) from inherited
  obligation profiles are injected into `dest`, **normalized to the core obligation URL** on copy.
- **Mappings (PU:3082):** `mappings.merge(derived, base)` (note the reversed argument names, per the
  source comment) delegates to `MappingAssistant`, which unions base+derived element mappings without
  duplicates, honoring suppression (`EXT_SUPPRESSED`), identity renames, and the `mappingMergeMode`
  knob (§2.7).

## 10.10 Downgrade rules and the one hard throw

| Property | Rule |
|---|---|
| `mustSupport` (2872) | aggregated with obligation-profile values; `base true → derived false` (non-slicer) → ERROR. |
| `mustHaveValue` (2884) | `base true → derived false` (non-slicer) → ERROR. |
| `isModifier` (2906) | applied only for extensions; default modifier-reason synthesized if base.isModifier and none given. |
| **`isSummary` (3039)** | if it differs and `base.hasIsSummary()` (and version ≠ `1.4.0`) → **`throw Error`** `ERROR_IN_PROFILE..._ISSUMMARY`. |

> Note the asymmetry: nearly every violation in the merge is a non-fatal ValidationMessage, but a
> change to `isSummary` is a **hard exception** that aborts generation.

## 10.11 In-place mutation of the inputs (caution)

`updateFromDefinition` is **not** side-effect-free on `source`/`derived`:
- It stamps `SNAPSHOT_GENERATED_IN_SNAPSHOT`, `SNAPSHOT_DERIVATION_POINTER`, and per-property
  `SNAPSHOT_DERIVATION_EQUALS` on the differential.
- With `trimDifferential`, it **nulls or clears** every differential property equal to base (short,
  definition, min, max, fixed, pattern, type, binding, isSummary, …) — minimizing the stored
  differential.
- The `sdf-9` rule (PU:2739) clears `requirements` on *both* derived and base for root elements;
  example deletion removes entries from both lists.

A reimplementation MUST treat the differential as mutable here, or replicate the effects on a copy
and migrate provenance back (as Layer A does in Pass A4.1, §3.6).

## 10.12 Failure catalog (merge)

| Trigger | Outcome |
|---|---|
| xver extension url bad-version / invalid / unknown | `throw FHIRException` (PU:2631-2635) |
| resolved profile's snapshot empty | `throw DefinitionException SNAPSHOT_IS_EMPTY` (PU:2652) |
| Extension/type profile unresolvable and `allowUnknownProfile` forbids | `throw DefinitionException` (PU:2679, 2684) |
| `isSummary` changed when base has isSummary | `throw Error ERROR_IN_PROFILE..._ISSUMMARY` (PU:3042) |
| derived type not a legal narrowing of any base type | `fail ILLEGAL_CONSTRAINED_TYPE` (checkTypeDerivation) |
| min widened / max widened | ERROR message (non-fatal); value still applied |
| binding strength weakened from REQUIRED / not a subset | ERROR message (non-fatal) |
| mustSupport / mustHaveValue downgraded true→false | ERROR message (non-fatal) |
| value set unlocatable / unexpandable / too costly | WARNING message |

---

# Part XI — Identity and Ordering

Two operations give the snapshot its final shape independent of element *content*: **sorting** the
differential into base order, and **assigning `id`** to every element. Both live in
`ProfileUtilities`, but they sit at different points in the layering — and sorting is, perhaps
surprisingly, **not part of `generateSnapshot` at all.** (Anchors: PU:3815, 3720, 3888, 4256,
4326; PB:685.)

## 11.1 Sorting is a Layer-B pre-pass (a layering subtlety)

> **Normative placement.** `ProfileUtilities.generateSnapshot` does **not** sort the differential.
> The structural algorithm (Part III) *assumes the differential is already in base order* — its
> forward-only diff cursor (§6.2, R-WALK-1) depends on it. Sorting is performed by the **publisher**
> (Layer B) as a pre-pass, by calling `ProfileUtilities.sortDifferential` *before*
> `generateSnapshot`.

This is a clean example of the Part 0 boundary: `sortDifferential` is *implemented* in Layer A's
class (it is deterministic, structural code) but is *invoked* as a Layer-B orchestration step. The
publisher's per-StructureDefinition pre-pass (PB:685-753) is:
```
if sd.kind != LOGICAL or sd.derivation == CONSTRAINT:     // pure logical models are NOT sorted
    if not sd.hasSnapshot:
        sortDifferential(base, sd, …, errorIfChanges=true)   // PB:716  — reorder differential
        setIds(sd, checkFirst=true)                          // PB:724  — assign ids
        ensure a root element exists
        generateSnapshot(base, sd, …)                        // PB:734  — Layer A (which calls setIds again, §11.4)
```
So a malformed-order differential is *repaired* (or flagged) by Layer B before Layer A ever sees
it. A caller that invokes `generateSnapshot` directly on an unsorted differential will get PC-1
failures (§3.8), because the structural walk will not find the out-of-order elements.

## 11.2 `sortDifferential` (PU:3815)

```
sortDifferential(base, diff, name, errors, errorIfChanges):
    for each diff element: stamp userData[SNAPSHOT_SORT_ed_index] := its original index   // for diagnostics
    original := copy(diff.element)
    root := ElementDefinitionHolder(diff[0] or a synthetic root placeholder)
    processElementsIntoTree(root, diff.element)              // §11.3 — flat list → path-nested tree
    sortElements(root, ElementDefinitionComparer(base snapshot order))   // §11.3 — order siblings by base
    newDiff := writeElements(root)                          // serialize tree back to a flat list (drop placeholders)
    if errorIfChanges: compareDiffs(original, newDiff, errors)   // report the first moved element
    diff.element.clear(); diff.element.addAll(newDiff)     // ★ REWRITE THE DIFFERENTIAL IN PLACE
    if original count != new count: error "Sort failed … path … is illegal"
```
`compareDiffs` (PU:3871) walks both lists positionally and, at the first index where the path
changed, reports *"element X @diff[original-index] is out of order"* (using the stashed index). The
publisher passes `errorIfChanges = true`, so reordering a published profile's differential is
itself a reported error (the author is expected to write elements in order).

## 11.3 Ordering mechanics

**`processElementsIntoTree`** (PU:3888) nests the flat list into a tree purely by `.`-prefix of
`path`. When the differential names a grandchild without its parent, a **placeholder** node is
synthesized for the missing level (dropped on serialization). The tree's siblings are what get
sorted.

**`ElementDefinitionComparer`** (PU:3720) orders siblings by **their position in the base
snapshot**: `compare(a,b) = baseIndex(a) − baseIndex(b)`, where `baseIndex` is lazily resolved by
`find(path)` and cached on the holder (with `0` doubling as the "unresolved" sentinel — safe only
because the root is a placeholder). `find` (PU:3755):
- translates the diff path into base-snapshot coordinates and scans for an exact or `[x]`-compatible
  match (both directions, §5.2);
- **chases `contentReference`**: when the path runs through a contentReference element, it rewrites
  `path`/`actual` in place and rescans from the top, guarded by `MAX_RECURSION_LIMIT = 10`
  (PU:3786) against cycles;
- when sorting descends into a child governed by a *different* structure (a datatype/extension/
  resource profile), **`getComparer`** (PU:3930) builds a fresh comparer rooted on that structure's
  snapshot (with `prefixLength` re-based), or returns `null` (and the subtree is skipped) when the
  profile isn't loaded yet.

> **Caution (silent diagnostics).** The comparer collects "differential contains path … not found
> in base" errors, but they are flushed to the caller **only when `debug` is on** (PU:3916).
> Non-debug runs silently swallow them. A reimplementation that wants these surfaced must not gate
> them on debug.

## 11.4 `setIds` — element id assignment (PU:4256)

`setIds(sd, checkFirst)` (re)generates ids for both differential and snapshot lists; `checkFirst`
true means "only if some element lacks an id." It is invoked **twice** in a normal build: by the
publisher pre-pass (`checkFirst=true`, PB:724) and at the end of `generateSnapshot` itself
(`checkFirst=false`, PU:886 — this is Pass A3.5, §3.5). So id assignment *is* part of Layer A;
sorting is not.

**`generateIdForElement`** (PU:4326) constructs the id, which differs from the path in three ways:
```
id := path[0]                                  // root segment, never sliced
for each non-root segment s at path level i:
    id += "." + fixChars(s)                    // fixChars: "_" → "-"
    if segment i is a slice:  id += ":" + sliceName        // slice suffix
```
- **Path vs id:** path is raw segment names joined by `.`; id additionally maps `_`→`-` and appends
  `:sliceName` to every sliced segment.
- **Ancestor slices inherit:** a `SliceList` (PU:4278) tracks active slice names by accumulated
  path, so a child's id carries its ancestors' slices — e.g. `Patient.extension:race.extension:ombCategory`.
- **Duplicate id** → ERROR `SAME_ID_ON_MULTIPLE_ELEMENTS` (PU:4355).
- **Local contentReference absolutized:** `#frag` is rewritten to `<canonical-base>#frag` (PU:4359).

---

# Part XII — The Validation Surface

This part consolidates Layer A's structural checks — the ones that are *not* the per-element merge
(Part X) or slicing/type rules (Parts VII–VIII), which carry their own catalogs. First the severity
model (which is itself non-obvious), then the catalog. (Anchors: PU:1217, 1413, 1317, 1948, 1333,
976, 1038, 3342, 908.)

## 12.1 The severity model (read first)

The word "error" in this code rarely means "abort." Three layers decide what actually happens:

- **`handleError(url, msg)`** (PU:1217) records a fixed-`ERROR` ValidationMessage. **It does not
  throw.** It delegates to `addMessage`.
- **`addMessage`** (PU:1221) throws a `DefinitionException` **only if `wantThrowExceptions` is set**
  — and that flag is true **only when the caller constructed `ProfileUtilities` with a `null`
  messages list** (PU:452/462/481). In normal IG-publisher use a real messages list is supplied, so
  ERROR messages are **collected, not thrown**; generation continues.
- **`forPublication`** changes severity in exactly **one** place: the slice min-count mismatch
  (INFORMATION → ERROR, PU:1003). It is otherwise not a severity lever.

> **Normative consequence.** Most "errors" in Layer A are *recorded diagnostics on a finished (if
> imperfect) snapshot*, not fatal stops. Only a handful of conditions are hard exceptions (the
> `checkDifferential` family, `checkDifferentialBaseType`, the path-discipline check, `isSummary`
> change, illegal type derivation, and — when configured with a null message sink — any ERROR).

Configuration levers:

| Flag | Effect |
|---|---|
| `wantThrowExceptions` (null message sink) | any ERROR message becomes a thrown `DefinitionException` |
| `forPublication` | slice min-count mismatch: INFORMATION → ERROR (PU:1003) |
| `isSuppressIgnorableExceptions()` | downgrades the `checkTypeDerivation` illegal-type throw to silent (PU:3312) |
| `allowUnknownProfile` | whether unresolved Extension/type profiles throw (PU:2678) |
| `setIgnorableError(true)` | marks slice-min / sliceName-without-slicing / duplicate-sliceName as ignorable (PU:1003/1026/1032) |

## 12.2 Pre-flight checks (Pass A1)

| Check (line) | Validates | Severity |
|---|---|---|
| **`checkDifferential`** (1413) | every element `hasPath`; non-null path value; **prefix discipline** (root = type tail, every other path starts `type + "."`); per-segment name rules: non-empty, ≤64 chars, no Unicode whitespace, no illegal punctuation `,:;'"/\|?!@#$%^&*(){}`, ASCII range only, `[`/`]` only as a trailing `[x]` | **throw `FHIRException`** |
| **`checkDifferentialBaseType`** (1317) | the first (root) differential element's explicit `type`: auto-clears it if it matches the base ancestor type (`wantFixDifferentialFirstElementType`); else (non-LOGICAL) | **throw `Error`** `TYPE_ON_FIRST_DIFFERENTIAL_ELEMENT` |

> `checkDifferential` enforces path *prefix discipline* but **not ordering** — ordering is enforced
> indirectly by PC-1 (§12.4) once the (already-sorted, §11.1) differential is walked.

## 12.3 In-walk and completion checks

| Check (line) | Validates / does | Severity |
|---|---|---|
| **`checkExtensionDoco`** (1948, "task 3970") | for `.extension`/`.modifierExtension` elements, **overwrites** `definition`="An Extension", `short`="Extension", and **clears** comment/requirements/alias/mapping (so generic Element doco isn't inherited) | mutation (no message) |
| **`checkGroupConstraints`** / `checkForChildrenInGroup` (1333/1344) | choice (xor) groups: duplicate child name → `Error("huh?")`; **>1 mandatory member** → `Error`; otherwise forces the other members to `max=0` and prunes their subtrees | **throw `Error`** / mutation |
| **Slice cardinality** (976-1036, `ElementDefinitionCounter`, §7.9) | summed slice min/max vs anchor | min: auto-fix or `forPublication?ERROR:INFORMATION` (ignorable); max: INFORMATION; min>max: WARNING |
| **Path discipline** (1020) | every non-root output path starts with `type + "."` | **throw `Error`** |
| **sliceName without slicing** (1023) | a `sliceName` with no slicing anchor for its path | ERROR (ignorable) |
| **Duplicate sliceName** (1028) | repeated slice name in a group | ERROR (ignorable) |
| **`checkTypeOk`** (3342) | a `fixed[x]`/`pattern[x]` value's fhirType is one of the element's allowed types | ERROR |
| **Profile/targetProfile refs** (1038-1077) | resolve each `type.profile` (incl. xver); unresolved → WARNING; `EXT_PROFILE_ELEMENT` sub-element missing → ERROR; type incompatibility (`isCompatibleType`) → ERROR | WARNING / ERROR |

## 12.4 The reconciliation check (PC-1)

The single most important check (PU:908-948, Pass A4.1, §3.6):
```
for each differential element e:
    if e lacks SNAPSHOT_GENERATED_IN_SNAPSHOT:        // it produced no output element
        append to report; ce++
        if e.hasId():  ERROR (per-element)            // id-less elements counted but not individually errored
if any unmatched:  handleError(aggregate "N elements … don't have a matching element … (including order)")
```
This is what turns an illegal differential (bad path, wrong order, slicing not set up) into a
diagnostic. Preprocessor-injected rows are exempt from provenance migration but **not** from this
check (§2.6). Whether the aggregate `handleError` throws depends solely on `wantThrowExceptions`
(§12.1).

## 12.5 Summary: the hard-stop conditions

For implementers, the complete list of conditions that **abort** Layer A (independent of the
null-sink throw mode):

1. Missing base / derived; missing type / derivation; CONSTRAINT type mismatch (§3.2).
2. Circular snapshot reference (`snapshotStack`, §3.2).
3. Any `checkDifferential` legality violation (§12.2).
4. `checkDifferentialBaseType` on a non-LOGICAL root (§12.2).
5. Output path-discipline violation; `checkGroupConstraints` violations (§12.3).
6. `isSummary` change against a base that sets it (§10.10).
7. Illegal type derivation, unless suppressed (§10.5).
8. Unresolvable contentReference / type profile under a forbidding `allowUnknownProfile` (§9.8, §10.8).
9. Root element carries a type when `kind != LOGICAL` (§3.5, PC-2).

Everything else is a recorded diagnostic on a completed snapshot.

---

# Specification Status

**Layer A (the structural snapshot algorithm) is now fully specified** across Parts 0–XII:

| Part | Subsystem | Status |
|---|---|---|
| 0 | Architecture: two layers and the boundary | ✅ |
| I | Conceptual model | ✅ |
| II | Terminology and notation | ✅ |
| III | Layer A pass-by-pass (A0–A4) | ✅ |
| IV | Layer B policy passes (pinning worked example) | ✅ |
| V | Path correspondence | ✅ |
| VI | The lockstep walk | ✅ |
| VII | Slicing | ✅ |
| VIII | Type & choice (`[x]`) expansion | ✅ |
| IX | contentReference resolution | ✅ |
| X | The element merge (`updateFromDefinition`) | ✅ |
| XI | Identity and ordering | ✅ |
| XII | The validation surface | ✅ |

**Deliberately out of scope (Layer B policy passes, §4.5):** canonical version pinning is fully
specified as the worked example (§4.4); narrative regeneration, instance validation, and provenance
processing are named and bounded but not specified — they are not part of the algorithm.

**Known implementation quirks flagged for any reimplementer** (each cited inline at its section):
the `merge(src, slicer)` no-op (`APPLY_PROPERTIES_FROM_SLICER = false`, §7.1); the
`"modiferExtension"` misspelling in the preprocessor (§7.2); the unanchored `fixForRedirect` string
replace (§9.6); the `mergeMarkdown`/`mergeStrings` argument-order inconsistency (§10.7); the
bindable-type guard silently deleting bindings (§10.5); and the debug-gated swallowing of comparer
diagnostics (§11.3).

**Validated against** the R5 reference implementation (`org.hl7.fhir.r5`) and the IG Publisher at
the line numbers cited throughout. The companion files `INDEX.md`, `NON-OBVIOUS-BEHAVIORS.md`, and
the `files/` source extract provide the underlying evidence.

*End of specification.*
