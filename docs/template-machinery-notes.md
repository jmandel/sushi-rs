# Template Machinery Notes — porting the Publisher's TemplateManager (task #38)

> READ-ONLY investigation. Question (Josh): our stock-template renderer already
> interprets template CONTENT as data (liquid/_includes/_data — genuinely
> driven), but the *mounted tree* is a Java-materialized snapshot frozen at F0.
> The IG Publisher's template machinery — fetch template package → walk the
> `extends`/`base` chain → merge config.json → stage `_includes`/`_layouts`/
> assets → run script hooks — was never ported. What would it take to make it
> truly driven: "select any `template#version` and it just works"?
>
> Evidence base: publisher clone at
> `…/scratchpad/ig-publisher` (git tag **v2.2.10**, commit `37a39a2c`); F0
> builds at `/home/jmandel/hobby/sushi-rs-snapshot-f0-builds/{us-core,plan-net}`;
> pinned template caches under those builds' `.home/.fhir/packages/`.
> Every claim cited `file:line`.
>
> **Note on fhir-core source.** The `NpmPackage.unPackWithAppend` /
> `NpmPackage.load` code lives in the fhir-core dependency, which is **not**
> checked out in the scratchpad (searched all of `/tmp/claude-1000` and
> `/home/jmandel/hobby`; only `plantuml.jar` present, no fhir-core jar/source).
> The `_append.` merge semantics below are therefore reconstructed
> **empirically** from the F0-staged output vs the raw packages (byte-exact,
> §3), not from reading the Java. This is the one lifecycle box proven by
> observation rather than source.

---

## 1. Lifecycle map (with citations)

All paths below are relative to
`scratchpad/ig-publisher/org.hl7.fhir.publisher.core/src/main/java/org/hl7/fhir/igtools/templates/`.

### 1a. Entry + directory prep — `TemplateManager.loadTemplate`
`TemplateManager.java:65-88`

- `templateDir = <rootFolder>/template` (`:67`). Unless the template is the
  literal in-place sentinel `#template` (`:68`), the dir is **created and
  cleared** (`:70-71`) — so materialization is a clean rebuild every run.
- Calls `installTemplate(template, …, level=0)` (`:78`) which does the recursive
  chain walk, then constructs a `Template` object (`:87`) that owns the runtime
  script hooks.
- **Trust gate** (`:81-86`): `canExecute` is forced `true` when `!autoMode`
  ("locally, we'll build whatever people give us", `:82`). Only the ci-build
  (autoMode) enforces the script allowlist. **For our use case (local editor)
  this gate is a no-op** — every template is trusted.

### 1b. Package resolution — `TemplateManager.loadPackage`
`TemplateManager.java:321-378`

Resolution order for a template coordinate:
1. `#folder` sentinel → local dir as `NpmPackage.fromFolder(…, IG_TEMPLATE)` (`:323-329`).
2. Matches `PACKAGE_REGEX` (bare id) → `pcm.loadPackage(template, null)` (`:331-333`).
3. Matches `PACKAGE_VERSION_REGEX` (`id#ver`) → `pcm.loadPackage(id, ver)` (`:334-337`).
   **This is the normal path** (`ig.ini`'s `template = hl7.fhir.template#1.0.0`).
4. Local file/dir path (`:338-348`).
5. `https://github.…` URL → downloads `archive/…zip`, `NpmPackage.fromZip` (`:349-359`).

- **Registry / version pinning**: acquisition is delegated to
  `FilesystemPackageCacheManager pcm` (field `:50`). The registry is whatever
  `pcm` is configured with (default `packages.fhir.org` + `packages2`); version
  resolution (`latest`/`current`/wildcards) is the cache manager's, **not**
  TemplateManager's. TemplateManager only ever passes an explicit `id`/`ver`
  string. **Implication for us: we do NOT need to port a registry client** — we
  already have `package_store::resolve` (host-index-driven version resolution,
  §4) + a host `acquire+mount` contract.
- **Type check** (`installTemplate:97-98`): the package must have
  `type == IG_TEMPLATE` or it throws.

### 1c. Extends-chain walk — `TemplateManager.installTemplate`
`TemplateManager.java:94-225` (recursive)

- The parent is declared by the **`base`** field in the template's
  `package.json` (`:101-102`). Empirically confirmed (`package.json`s in both
  caches):
  - **us-core chain** (`ig.ini`: `hl7.fhir.template#1.0.0`):
    `hl7.fhir.template#1.0.0` →`base`→ `hl7.base.template#1.0.0`
    →`base`→ `fhir.base.template#1.0.0` (root, no `base`). **3 packages.**
  - **plan-net chain** (`ig.ini`: `hl7.davinci.template#current`):
    `hl7.davinci.template` → `hl7.fhir.template` → `hl7.base.template`
    → `fhir2.base.template` (root). **4 packages.** (Note the root is
    `fhir2.base.template`, the newer base — different from us-core's chain.)
- **Version of the parent** is read from the child's `dependencies` map, keyed
  by the base id (`:107-111`); if the base id is absent from `dependencies`,
  it throws (`:107-109`). So the chain is fully version-pinned by each package.
- **Loop guard**: `loadedIds` list; if a base id is already in it → throws
  `"Template parents recurse: a->b->…"` (`:103-106`). Simple visited-set, no
  depth limit.
- **Order**: the recursion descends to the ROOT **before** unpacking
  (`installTemplate` recurses at `:111`, then `npm.unPackWithAppend` at `:116`
  runs *after* the recursive call returns). Net effect: **root unpacks first,
  leaf unpacks last** → later (more-derived) packages overwrite / append onto
  earlier ones. Confirmed by staged output (§3).

### 1d. Staging — `npm.unPackWithAppend(templateDir, files)`
`TemplateManager.java:116`

Each package's tree is unpacked into the single shared `templateDir`, appending
to the running `files` list. The package layout (verified in cache):
top-level `content/`, `includes/`, `layouts/`, `liquid/`, `data/`, `config/`,
`scripts/`, `translations/`, plus `ig.ini`, `README.md`, `config.json`, and a
`package/` dir holding only `package.json`+index. **The `$root` prefix**
(`config.json:123`, `:126`) maps to the top-level of the package.

`unPackWithAppend` (fhir-core) implements the **`_append.` merge convention**.
Reconstructed empirically in §3, then **pinned against fhir-core SOURCE in task
#39** (`NpmPackage.unPack:1464-1494` + `FileUtilities.appendBytesToFile:65-69` at
`/home/jmandel/hobby/fhir-perf/repos/fhir-core`): a package file named `_append.X`
has its bytes **appended** to file `X` produced by earlier packages in the chain,
rather than overwriting. All other files **overwrite** by relative path
(last-writer-wins = leaf wins).

> **M2 CORRECTION (task #39, from source):** the separator is **`\r\n` (CRLF)**,
> not a plain `\n` — `appendBytesToFile` writes `byte[]{13,10}` unconditionally
> BEFORE the append bytes, and ONLY when the target already exists (`if
> (f.exists())`, `NpmPackage.java:1485`); when the target does not yet exist the
> append file becomes the target **verbatim, no separator**. Additionally the
> literal `_append.X` file is ALSO staged (a Java brace-bug at
> `NpmPackage.java:1489-1491`: the trailing `bytesToFile` runs for both branches),
> carrying the last layer's bytes (last-writer-wins). All three points are
> byte-verified and gate-green in `template_materialization_gate.rs`.

### 1e. config.json merge — `TemplateManager.applyConfigChanges`
`TemplateManager.java:216-246`

- Each package that has a top-level `config.json` (`$root/config.json`, `:123`)
  is parsed and pushed onto `configs` (`:126-131`), in **chain order** (root
  config is `configs.get(0)`, leaf last — because parse happens after the
  recursive descent returns... **NO**: parse happens in the same `installTemplate`
  frame, *after* `unPackWithAppend` at `:116`, which is after the recursive
  call at `:111`. So the ROOT frame's config is added first → `configs.get(0)`
  is the ROOT/base config; the leaf's config is appended last).
- At `level==0` only (`:216`), the merge folds all deltas into `configs.get(0)`
  (base): `for i in 1..n: applyConfigChanges(base, configs[i])` (`:217-220`),
  then writes the merged result to `<templateDir>/config.json` (`:221-223`).
- **Merge semantics** (`applyConfigChanges:227-246`):
  - **Objects** → recursive deep-merge (`:234-235`).
  - **Arrays** → **APPEND** (`baseArray.addAll(newArray)`, `:236-237`). *Not*
    replace, *not* set-union — plain concatenation.
  - **Primitives** → **REPLACE** (`remove`+`add`, `:238-240`).
  - **Type mismatch** across layers on the same key → **throws** (`:232-233`).
  - New keys → added (`:242-244`).
- Empirically confirmed in the F0 staged `config.json` (us-core):
  `template-parameters` is the concatenation of base's params +
  `["jira-code"]` from `hl7.base.template` (array-append); `script` is the
  single primitive `scripts/ant-hl7.xml` (leaf's `hl7.base` value replaced
  base's `scripts/ant.xml`) — see §2 for why that is the correct, intended
  result.

### 1f. The `Template` object + script-hook wiring — `Template` ctor
`Template.java:114-186`

The merged `config.json` is re-read (`:126`) and drives everything:
- If `config.script` present (`:127`): read `targets.{onLoad,onGenerate,onJekyll,onCheck}`
  (`:131-139`), then **construct an Apache Ant `Project`** from the named build
  file (`:140-143`, `import org.apache.tools.ant.*` `:40-42`). Ant properties
  are seeded: `ig.root`, `ig.template`, `ig.scripts`, `ig.networkprohibited`
  (`:153-158`).
- `template-parameters*` arrays → `templateParams` set (`:160-166`).
- `script-mappings`, `defaults`, `extraTemplates`, `pre-process`, `summaryRows`
  parsed (`:167-184`).
- `loadFragmentTypes()` (`:185`, `:188-190`) scans the staged tree for the set
  of fragment types the template's liquid actually references (rapido opt).

### 1g. The SCRIPT HOOKS — events, engine, I/O
Executor: **Apache Ant**, embedded in-process (`Template.java:141` `new Project()`,
`:397` `antProject.executeTarget(target)`). The events, all in `Template.java`:

| Event | Method | Ant target | IG passed in? | IG modified? | I/O contract |
|---|---|---|---|---|---|
| **onLoad** | `onLoadEvent` `:549-554` | `targetOnLoad` | yes | **yes** (`IG_ANY`, may replace IG) | writes IG as R4 xml/json to `<target>-ig-working.*`, ant reads it, writes `<target>-ig-updated.*`, publisher re-parses (`runScriptTarget:377-454`) |
| **onGenerate** (before Jekyll) | `beforeGenerateEvent` `:556-588` | `targetOnGenerate` | yes | no-resource (`IG_NO_RESOURCE`, may edit definition/manifest only, `loadModifiedIg:456-470`) | **also copies `template/content/*` into `tempDir`** (`:557-580`) before running ant; ant emits `_data/*.json`, `_includes/artifacts.xml`, svgs, jira files |
| **onJekyll** | `beforeJekyllEvent` `:590-597` | `targetOnJekyll` | no | no (`IG_NONE`) | file-list only |
| **onCheck** | `onCheckEvent` `:599-606` | `targetOnCheck` | no | no (`IG_NONE`) | emits `*-validation.json` OperationOutcomes |

- **IG round-trip** (`runScriptTarget:381-395`): the R5 IG is down-converted to
  **R4** (`VersionConvertorFactory_40_50`) and written to disk as the ant input;
  ant's XSLT operates on R4 XML; output is re-parsed and up-converted (`:415-451`).
- **Validation messages**: ant writes `<target>-validation.{json,xml}` as a FHIR
  `OperationOutcome`; publisher loads them into the QA report
  (`loadValidationMessages:480-494`); a `FATAL` issue aborts the build (`:488-489`).
- **File-list back-channel**: ant sets an ant property `<target>.files`
  (`;`-separated), publisher reads it into `newFileList` (`:398-406`).
- **Guardrail**: templates may NOT add/remove IG resources
  (`loadModifiedIg:456-465` throws if resource count changes).

### 1h. Ant capabilities actually used (the real cost surface)
Read of the three ant scripts + the XSLT set:
- `<xslt>` — **Saxon XSLT 2.0** transforms. This is the workhorse: ~14 XSLTs in
  `fhir.base`, +7 jira/pkg-list in `hl7.base`, +17 in `hl7.davinci`.
- `<java jar="plantuml.jar">` — renders `.plantuml` → `.svg`
  (`fhir.base ant.xml:98-108`). An **11.8 MB Java jar**.
- `<get src="http…">` — **network fetches at build time**: FHIR schemas
  (`ant.xml:36-45,58-72`), and — critically — the **JIRA-Spec-Artifacts**
  workgroups + jiraspec XML from GitHub (`ant-hl7.xml:21-22,46`).
- `<scriptdef language="javascript">` — **embedded JS** (Rhino/Nashorn) for
  `onGenerate.files` list munging (`ant.xml:156-159,187-190`).
- `<copy>`, `<concat>`, `<replace>`, `<loadproperties>`, `<loadfile>`,
  `<condition>`, `<filesmatch>`, `extension-point`/`extensionOf` dependency
  graph.

---

## 2. Per-package script-hook inventory + classification

Classification key: **(a)** file staging/copy · **(b)** trivial substitution ·
**(c)** real computation (XSLT/JS/plantuml/network). "Loader-port" = handled by
just staging files; "phase" = needs the compute engine.

### `fhir.base.template#1.0.0` (root of us-core chain) — `config.script = scripts/ant.xml`
The real engine. 14 XSLT + `plantuml.jar` + `ant.xml`.
Targets → events (`config.json` targets: onLoad, onGenerate, onCheck):
- **onLoad** (`ant.xml:4-35`): `onLoad.xslt` (21.9 KB) supplements the IG with
  standard config, spreadsheet list. → **(c)** XSLT over the IG resource.
- **onGenerate** (`ant.xml:47-172`), a big `extension-point` graph:
  - `onGenerate.data.xslt` → `_data/artifacts.json` — **(c)**.
  - `onGenerate.genJson.xslt` → `_data/info.json` — **(c)**.
  - `onGenerate.group.xslt` + `groupings.txt` inject + `groupSort.xslt` +
    `final.xslt` → the processed IG (`:114-119`) — **(c)** multi-stage XSLT.
  - `onGenerate.qa.xslt` → igqa validation — **(c)**.
  - `createArtifactSummary.xslt` → `_includes/artifacts.xml` — **(c)** (this is
    the artifacts table every IG shows).
  - `getSchemas` `<get>` R2–R5 XSD from build.fhir.org — **(c)+network**.
  - `plantUml` `<java jar>` — **(c)+11.8 MB jar** (only fires if the IG has
    `.plantuml` sources; us-core/plan-net do **not**, so inert here).
  - `processIncludes` / `copyDataFiles` — `<copy>` template includes/data →
    temp — **(a)**.
- **onCheck** (`ant.xml:174-186`): just concatenates validation jsons — **(b)**.
- `pre-process` (config, `:` array): stages `input/pagecontent`, `input/pages`,
  `input/intro-notes` into `_includes` **via `processPages.xslt`**
  (a per-page XSLT wrap) + `input/includes`,`input/data` as plain copies.
  → mix of **(a)** and **(c)** (processPages is a real transform).

### `hl7.base.template#1.0.0` (mid) — `config.script = scripts/ant-hl7.xml`
`ant-hl7.xml` does `<import file="ant.xml"/>` (`:6`) then hooks
`onGenerate.jira` in via `extensionOf="onGenerate.extend"` (`:127`).
- **onGenerate.jira** (`ant-hl7.xml:7-127`): builds the **JIRA spec-artifacts
  tracking file**. `genProperties.xslt`, `package-list.xslt`,
  `process-pkg-list.xslt`, `jira.xslt` (13 KB), `normalize.xslt`,
  `pubrequest.xslt`. **Fetches** `_workgroups.xml` + `<jiraSpecFile>.xml` from
  GitHub (`:21-22`). Compares generated vs upstream, emits a warning if drift.
  → **(c)+network**. **Pure QA/publication tooling** — its *only* durable
  outputs are `jira-new.xml`, `jira-current.xml`, `<jiraSpecFile>.xml` at the IG
  root and validation warnings. **None of these are consumed by the rendered
  site.** (Confirmed: the 37 runtime files excluded in §3 are exactly these.)
- Content overlay: `_append.fragment-css.html` (adds `hl7.css` link),
  `_append.fragment-header.html` (adds HL7 logo nav). → **(a)** append-staging.

### `hl7.fhir.template#1.0.0` (leaf of us-core chain) — **NO config.json, NO scripts**
Pure content overlay: 3 `content/`, 3 `includes/` files. The includes are
`_append.fragment-{css,footer,header}.html` (add FHIR logo, version-history +
license footer links). → **100% (a)** append-staging. **Zero computation.**
This is the leaf us-core actually names — so **the us-core template's own
top layer adds no compute at all.**

### `fhir2.base.template#current` (root of plan-net chain) — `config.script = scripts/ant.xml`
Same shape as `fhir.base` but newer/larger (16 XSLT, 79 liquid vs 33, adds
`onGenerate.extractArtifacts.xslt`, `qa.checkMenu.xslt`, a `translations/` dir
of 22 files, `multilanguage-format` config). Classification identical: onLoad/
onGenerate XSLT chain = **(c)**; copies = **(a)**; onCheck concat = **(b)**.

### `hl7.davinci.template#current` (leaf of plan-net chain) — `config.script = scripts/ant-davinci.xml`
`ant-davinci.xml` `<import file="ant-hl7.xml"/>` (`:6`).
- **onLoad override** (`:17-22`): `onLoad.davinci.xslt` (1 KB, extra params).
  → **(c)** small.
- **onCheck.davinci** (`:23-148`, hooked via `extensionOf="onCheck.extend"`):
  **17 XSLTs** that copy the *rendered output HTML* back
  (`output/en/index.html`, etc., `:45,50,55…`), strip doctype, and run
  link/menu/conformance/security/download checks → `oncheck-validation-*.json`.
  → **(c)**, but again **pure QA**: reads finished output, emits validation
  messages. **No site content produced.**
- Content: 1 `content/`, 4 `includes/`, an `images-source/`. → **(a)**.

### Cross-cutting classification verdict
- **All durable, site-affecting template compute is onLoad + onGenerate in the
  `*.base.template` root.** onCheck (both hl7.base jira + davinci) and the jira
  machinery produce **QA/publication artifacts only** — none feed the rendered
  pages. For a *renderer* (not a publisher), **onCheck and the entire
  jira/pubrequest subsystem can be dropped.**
- The onGenerate outputs that DO feed the site: `_data/artifacts.json`,
  `_data/info.json`, `_includes/artifacts.xml`, plantuml `.svg`s, and the
  processed-IG grouping/sorting (which the fragment generators depend on).
- Everything else — every `_append.fragment-*`, every `layout`, `liquid/`,
  `content/`, `assets/` — is **(a) pure staging**, which is the 97.6% in §3.

---

## 3. Union-copy vs transform audit (the numbers)

Method (`scratchpad/audit.py`): for the **us-core** F0-staged
`…/us-core/template/` tree, hash every file and compare against the union of all
3 chain packages' files (by relative path). Ant *runtime* outputs
(`onLoad-*`, `onGenerate*`, `onCheck-*`, `jira-*`, `properties.txt`,
`versions.txt`, `*-validation*`) are separated out — those are build products,
not part of materialization.

```
STAGED files total: 201
  runtime-generated (ant output, NOT materialization): 37
  --- materialization files: 164 ---
  byte-identical to a chain package file (UNION-COPY): 160  (97.6%)
  present in chain but content differs (MERGED/TRANSFORM): 4 (2.4%)
  not found in any chain package (generated/new):        0  (0.0%)
```

The **4** non-identical files are the *entire* non-copy surface of materialization:
- `config.json` — the deep-merge (§1e).
- `includes/fragment-css.html`, `fragment-footer.html`, `fragment-header.html`
  — the `_append.` cumulative concat (§1d).

**The `_append.` rule** (from these 3 files; separator CORRECTED to CRLF per M2
source-pin above): `staged X = base_X + "\r\n" + append_layer1_X + "\r\n" +
append_layer2_X …` in chain staging order (root→leaf). Verified per file: staged
`fragment-css.html` =
base placeholder + `hl7.base`'s `hl7.css` line + `hl7.fhir`'s `fhir-ig.css`
line; staged `fragment-header.html` = base placeholder + `hl7.base`'s
`hl7-nav` div + `hl7.fhir`'s `family-nav`+`search` divs.

### Bottom line for §3
**Materialization = union-copy along the chain + a deep-merged `config.json` +
an `_append.` concat for a handful of fragment stubs.** That is the *whole*
static "TemplateManager output" the renderer's mounted tree needs. It is
**≈98% `cp -r` with last-writer-wins, ~2% two trivial merge rules, and ZERO
XSLT.** The XSLT/ant/plantuml machinery (§2) produces *runtime build products*
(`_data/*.json`, `artifacts.xml`, jira files), which in our architecture are
already covered by the **Rust fragment generators**, not by template staging.

---

## 4. Editor + fig angle — what a `template_loader` would be

### What exists today (grounding)
- **Frozen path**: today the mounted template tree is the F0 snapshot; there is
  no `--template` flag in `fig` (grep of `crates/fig/src` for template args:
  none) and templates are curated by hand into
  `scratchpad/bundles-curated` / the F0 `temp/pages/_includes` (15,867 files —
  the fully materialized Jekyll input, template + IG fragments merged).
- **Reusable machinery already in-tree**:
  - `crates/package_store/src/resolve.rs` — host-index-driven version
    resolution, dependency-chain walk, and an explicit *"the host must acquire +
    mount"* contract (`resolve.rs:70,102` — "NEVER a guess"). This is exactly
    the shape TemplateManager's `pcm.loadPackage` + `base`-chain walk needs.
  - `crates/package_store/src/{bundle.rs,source.rs}` — `mount_package`
    (`bundle.rs:134`) mounts a package dir's entries; `Session.mount`
    (`crates/wasm_api/src/lib.rs:162,708`) is the wasm entry point.
  - The stock-template renderer plan (`docs/stock-template-renderer-plan.md`)
    already declares "**We interpret the real template, we don't reimplement
    it… read at render time**" and a `TemplateBundle` slot in the adapter
    `init(ctx)` API. The loader is the missing producer of that `TemplateBundle`.

### The proposed module
A new engine module `template_loader` (in `package_store`, reusing `resolve`):

```
Session.mountTemplate("hl7.fhir.template#1.0.0")
fig render --template <id#ver>
```
Pipeline (all pure-Rust, mirrors §1):
1. **resolve** `id#ver` → concrete version via existing `resolve::resolve_version`
   over the host index.
2. **acquire+mount** the package via the existing host `PackageSource`/`acquire`
   contract (same path packages already take — no new registry client).
3. **walk `base`** chain (read `package.json.base` + `dependencies[base]`),
   loop-guard on visited ids (port of `installTemplate:101-112`), acquire each.
4. **stage** root→leaf into an in-memory/VFS `template/` tree: last-writer-wins
   overwrite + the `_append.` concat rule (§3) — **~120 lines, no XSLT**.
5. **merge `config.json`**: deep-merge objects / **append** arrays / replace
   primitives (direct port of `applyConfigChanges:227-246`) — **~40 lines**.
6. Expose the staged tree as the `TemplateBundle` the render adapters already
   consume.

### The oracle strategy (we hold both sides)
- **Gate 1 (byte-parity):** loader's staged `template/` tree, byte-compared
  against the Java-materialized F0 trees at
  `…/f0-builds/{us-core,plan-net}/template/` (excluding the 37 runtime files
  per §3). Target: **byte-identical** for all 164 us-core materialization files.
  This is a hard, cheap, deterministic gate — we already have the Java output.
- **Gate 2 (generalization):** the **plan-net** chain (4 packages, `fhir2.base`
  root, davinci leaf, `translations/` dir, `multilanguage-format`) as an
  independent second chain — proves the loader isn't overfit to us-core.
- Both gates are pure file-tree diffs; no publisher run needed at gate time.

---

## 5. Risks / unknowns

1. **The script hooks are Apache Ant + Saxon XSLT 2.0 + a JS engine + an
   11.8 MB plantuml.jar + build-time network fetches** (§1h). This is a genuine
   JVM stack. **We must NOT port an ant/XSLT runner into wasm.** The honest
   finding: the ant hooks compute two classes of thing —
   - **(A) QA/publication artifacts** (all of `onCheck`, all jira/pubrequest):
     *not consumed by the site* → **drop entirely** for a renderer.
   - **(B) site-feeding artifacts** (`onLoad` IG supplementation,
     `onGenerate` `_data/artifacts.json`, `info.json`, `artifacts.xml`,
     grouping/sort, plantuml svgs): these are **already produced by our Rust
     fragment generators** (per `docs/rust-fragment-generator-feasibility.md`
     and the stock-template plan's fragment store), so we reimplement the
     *effect* natively, not the ant script. plantuml is an optional edge
     (fires only when the IG ships `.plantuml`; neither us-core nor plan-net
     do) — punt to "unsupported / pre-render server-side".
   So the **wasm story is: loader stages files (pure, trivially wasm-safe);
   script-hook *effects* are covered by existing native fragment generators;
   we never run ant.** No script runner, no sandbox, no JVM.
2. **Network at template-load time.** Package *acquisition* needs the registry
   (host-side, already the model — wasm never does I/O directly). The ant
   scripts' own `<get>` calls (schemas, jira) are in the dropped/native-covered
   set, so **the loader itself needs no build-time network** beyond package
   fetch.
3. **`_append.` semantics reconstructed empirically, not from source** (fhir-core
   not in scratchpad). Low risk — the rule is dead simple and the byte-parity
   gate (§4) will catch any deviation immediately — but worth pinning against
   the real `NpmPackage.unPackWithAppend` before shipping. There may be sibling
   conventions (`_prepend.`, `_insert.`?) not exercised by these two chains;
   the grep for `_prepend` found nothing in-scope, so unknown.
4. **Version churn / `current`.** plan-net's chain is pinned to `current`
   (a moving tag) — "just works for any version" means the host index must
   resolve `current`→concrete at mount time (already `resolve.rs`'s job) and
   the byte-parity oracle only holds against the *specific* snapshot we
   captured. Compat matrix: templates in the wild use ~10 base families
   (`TemplateManager.java:279-304` allowlist) — us-core+plan-net cover the
   `fhir.base`/`fhir2.base`/`hl7.base`/`hl7.fhir`/`davinci` spine; the loader is
   family-agnostic (it only reads `base`+`_append`+config), so new families
   should work if they follow the same conventions.
5. **Template-wise work OUTSIDE TemplateManager.** Not investigated exhaustively
   here, but flagged: the Publisher also synthesizes Jekyll's `_config.yml`,
   the `ReleaseHeader`/history, and `pre-process` page staging happens in the
   *publisher core* (driven by `config.pre-process`, §2) not TemplateManager.
   `pre-process` with `transform: processPages.xslt` (config.json:`pre-process`)
   is a real XSLT that wraps authored pages — that one **does** feed the site
   and is **not** pure staging. It's small (one XSLT, `processPages.xslt` 6.6 KB)
   but must be accounted for (likely already covered by our page/markdown
   pipeline, but verify). This is the one genuine "outside TemplateManager,
   site-feeding, XSLT" item to not overlook.

---

## 6. Sized proposal + recommendation

### Scope decomposition
| Piece | Size | Justification |
|---|---|---|
| **Loader core** (resolve→acquire→walk `base`→stage→merge config) | **S** | ~200 LOC of pure Rust on top of existing `package_store::resolve`/`bundle`. Chain walk + loop guard is a direct port of `installTemplate:94-225`; config merge is a direct port of `applyConfigChanges:227-246`; `_append` is ~30 LOC. No XSLT, no ant. |
| **`Session.mountTemplate` + `fig render --template`** wiring | **S** | thread the staged `TemplateBundle` into the already-defined adapter `init(ctx.template)` slot. |
| **Byte-parity oracle harness** (us-core + plan-net gates) | **S** | pure file-tree diff vs F0 `template/`; both sides already on disk. |
| **`pre-process`/`processPages.xslt` page-wrap parity** | **M** | one real XSLT that feeds the site; needs confirming our page pipeline reproduces it (or a small native port). Risk item §5.5. |
| **Ant script-hook *effects*** (onLoad/onGenerate site artifacts) | **(already built)** | covered by existing Rust fragment generators; loader does NOT own this. If any effect is *missing*, that's fragment-generator scope, not loader scope. |
| **plantuml / arbitrary custom-template ant** | **XL / out-of-scope** | would require a JVM+ant+Saxon+plantuml runner; explicitly **not** ported. Templates using bespoke ant compute beyond the base families are "unsupported in-browser; render server-side." |

### Overall verdict: **the "make it truly driven" loader is SMALL (S, ~2–3 days).**
The scary part (ant/XSLT/plantuml) is a **decoy for a renderer**: §3 proves
materialization is 98% copy + 2% two trivial merge rules with **zero XSLT**, and
§2 proves every XSLT hook produces either (a) QA artifacts we drop or (b)
site artifacts our fragment generators already produce. The honest total-scope
picture is **S for the loader itself + M for the one `pre-process` XSLT page-wrap
we must not overlook + a firm "no ant runner, ever" line.**

### Recommendation
1. **Build the `template_loader` (S).** It genuinely delivers "select any
   `template#version` and it just works" for the whole `fhir.base`/`fhir2.base`/
   `hl7.base`/`hl7.fhir`/`davinci` family — i.e. essentially all HL7 IGs — because
   those templates carry **no site-feeding compute of their own** beyond the
   base's onGenerate, which we already reproduce.
2. **Gate it byte-exact** against the two F0 `template/` trees (S). This
   converts "frozen snapshot" into "regenerable from packages" with a
   deterministic oracle we already hold both sides of.
3. **Verify the `pre-process`/`processPages` page-wrap (M)** is reproduced by the
   existing page pipeline; port that single XSLT natively if not.
4. **Do NOT port ant/XSLT/plantuml.** Declare custom-ant templates
   "server-side render only." The compat matrix that "just works" is defined by
   the `_append`+`base`+`config.json` conventions, which the loader is fully
   agnostic to.

**Net:** this is a high-leverage, low-cost win. The loader is small; the
oracle is free (both sides on disk); the only real diligence item is the one
`processPages` XSLT and pinning the empirically-derived `_append.` rule against
the real `NpmPackage` source once fhir-core is available.

## Addendum (2026-07-04, coordinator): "template 2" identified + demo default verified

- **Demo default version check**: packages.fhir.org `dist-tags.latest` for
  hl7.fhir.template = **1.0.0** (versions end at 1.0.0), so the demo's
  defaultVersion=1.0.0 IS the newest published version, not a fallback
  artifact. Check closed.
- **"Template 2" = `fhir2.base.template`** (repo HL7/ig-template-base2,
  "New release that supports languages and translations", published 0.1.0,
  actively developed — updated 2026-06-26). It is a different PACKAGE NAME,
  not a version — and it is a BASE template: users don't select it directly;
  it arrives via a leaf template's `base` chain. **We already cover it**:
  the plan-net oracle chain (hl7.davinci.template → hl7.fhir.template →
  hl7.base.template → fhir2.base.template) is the loader's generalization
  gate — and its language machinery is exactly why the plan-net pipeline
  has the multi-language layout (temp/pages/en, output/en, langs redirect
  stubs) that F5 ported. When leaf templates repoint their base to fhir2,
  the chain walk handles it with zero loader changes.
- ig-guidance's official leaf-template set also includes hl7.cda.template,
  hl7.ehrs.template, hl7.other.template — candidates for the curated
  dropdown later (cda likely carries custom generators; verify hooks via
  AntHookError before promoting).
