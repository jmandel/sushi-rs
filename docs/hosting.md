# Hosting the engine — CLI, wasm Session, and custom generators

> One engine, three skins. This doc is how to **host** it: from a browser
> Worker, from Bun/Node with a custom TypeScript generator, from a non-JS
> language, and as a zero-code template renderer. Every code block here has a
> runnable twin under [`examples/`](../examples/) that CI executes
> (`scripts/examples-gate.sh`) — the docs can't rot.

The engine is the `render_*` + compiler + snapshot_gen + site_db crates. Its
three skins:

| Skin | Where | Entry |
|---|---|---|
| **CLI** (`fig`) | native, fs | `fig <subcommand>` (`crates/fig`) |
| **Session** (wasm) | browser / Bun / Node | `new Session()` (`crates/wasm_api`) |
| **library** | native, in-process | the crate APIs directly |

All three share the **apiVersion envelope** (§4) — one schema, one implementation
(`crates/api_envelope`), verified schema-identical by
`crates/fig/tests/json_envelope.rs`.

---

## 1. Browser worker (the editor path)

The editor loads the wasm-bindgen **web-target** module in a Worker and calls
`Session`:

```js
// engine.worker.ts
const mod = await import(`${BASE}pkg/wasm_api.js`);
await mod.default(`${BASE}pkg/wasm_api_bg.wasm`);   // web target: init with the .wasm
const session = new mod.Session();

function unwrap(json) {                              // the ONE envelope check
  const e = JSON.parse(json);
  if (e.apiVersion !== 1) throw new Error(`apiVersion ${e.apiVersion}`);
  if (!e.ok) throw new Error(`${e.op}: ${e.error.message}`);
  return e.result;
}

unwrap(session.init(bundlesJson));                   // mount package bundles
unwrap(session.mountSite(filesJson, optionsJson));   // mount the staged site tree
const { pages } = unwrap(session.listPages());
const { html } = unwrap(session.renderPage('en/index.html'));
```

Every `Session` method returns one envelope string the caller `JSON.parse`s.
Domain failures are `ok:false` envelopes — methods never throw for them.

Build the module with the scratch wasm toolchain (recipe:
`demo/wasm-p0/README.md`): `cargo build -p wasm_api --target wasm32-unknown-unknown
--release` then `wasm-bindgen --target web` (browser) or `--target nodejs`
(Bun/Node, §2).

## 2. Bun / Node — a ~30-line custom generator

The same wasm module runs under Bun/Node. A **custom site generator** is a
`SiteGeneratorAdapter` (the SAME contract the editor uses). `fig` hosts it for
you: `fig render --generator ts:<adapter.mjs>` spawns Bun with a runner that
builds the adapter context — `{ engine, fragments, content, project }` — over the
wasm `Session`, exactly as the editor's `App.tsx` does, then drives
`init → listPages → renderPage`.

Your generator brings the chrome; the engine brings the semantics: `content`
(Liquid + kramdown via `Session.renderLiquid`/`renderMarkdown`) and `fragments`
(publisher-grade snapshot/diff/dict tables via first-include-miss). Full runnable
adapter: [`examples/custom-generator/generator.mjs`](../examples/custom-generator/generator.mjs).

```js
export default {
  id: 'my-generator',
  label: 'Minimal custom generator',
  ctx: null,
  async init(ctx) { this.ctx = ctx; },
  async listPages() { return [{ file: 'index.html' }, { file: 'guidance.html' }]; },
  async renderPage(file) {
    const c = this.ctx.content;
    if (file === 'index.html') {
      const body = await c.renderMarkdown('# My IG\n\nWelcome to the *custom* site.');
      return { html: chrome('Home', body) };
    }
    const body = await c.renderLiquid('<p>{{ tool }} rendered this.</p>', { tool: 'fig' });
    return { html: chrome('Guidance', body) };
  },
  async assetBytes() { return null; },
};
```

Run it:

```
fig render . -o site/ \
  --generator ts:generator.mjs \
  --wasm-dir path/to/nodejs-wasm-build \
  --project-json project.json \
  --bundles-json bundles.json
```

`--wasm-dir` is a **nodejs-target** wasm-bindgen build (`wasm_api.js` +
`wasm_api_bg.wasm`); `--project-json` is the `AdapterProject`
(`{ projectId, config, files, predefined, siteFiles, buildEpochSecs }`);
`--bundles-json` is the `Session.init` bundle set (`[{ label, files:{name:b64} }]`).
The adapter's output is byte-identical to a direct `Session` call — it IS the
same wasm module (proven in the examples gate + `crates/fig/src/runner`).

The `fragments` surface exposes publisher-grade fragments to any generator:
`await ctx.fragments.fragment('StructureDefinition-my-profile', 'snapshot')`
returns the exact snapshot table the Publisher would emit — embed it in your own
page chrome without reimplementing it.

## 3. Non-JS hosts — WASI or shell-to-fig

A language with no wasm bindings still drives the engine: **shell out to `fig`**
and parse its `--json` envelope. No FFI, no wasm. Full runnable example:
[`examples/shell-to-fig/render.py`](../examples/shell-to-fig/render.py).

```python
import json, subprocess

def fig_json(fig, *args):
    out = subprocess.run([fig, *args, "--json"], capture_output=True, text=True)
    env = json.loads(out.stdout.strip().splitlines()[-1])
    assert env["apiVersion"] == 1                    # the shared envelope contract
    if not env["ok"]:
        raise RuntimeError(f"{env['op']}: {env['error']['message']}")
    return env["result"]

ver  = fig_json("fig", "version")                    # engine identity
frag = fig_json("fig", "fragment", build_dir, "StructureDefinition-us-core-patient", "snapshot")
print(len(frag["html"]), "bytes of publisher-parity snapshot table")
```

Every `fig` subcommand takes `--json` and emits the §4 envelope, so the contract
is identical across languages. For an in-process non-JS host (Python, Go via
wasmtime, …), a **WASI** build of the engine is the other path; the `Session`
surface is WASI-clean (no browser APIs), and the CLI is the reference for the op
set. Until a concrete WASI consumer lands, shell-to-`fig` is the supported,
tested non-JS path.

## 4. The envelope schema (shared with `--json`)

One result/error shape for the CLI and the Session
([`examples/envelope/schema.json`](../examples/envelope/schema.json)):

```jsonc
// success
{ "apiVersion": 1, "ok": true,  "op": "render", "result": { /* payload */ } }
// failure (domain errors — never thrown/panicked)
{ "apiVersion": 1, "ok": false, "op": "snapshot", "error": { "message": "…" } }
```

`apiVersion` bumps only on a breaking change to the envelope **shape**, not to any
op's payload. One implementation lives in `crates/api_envelope`; both `wasm_api`
(Session) and `crates/fig` (`--json`) call it, and
`crates/fig/tests/json_envelope.rs` pins the two schema-identical. Validate your
host's parsing with [`examples/envelope/check.py`](../examples/envelope/check.py).

## 5. Custom-generator walkthrough (the contract in full)

The `SiteGeneratorAdapter` (from the editor's `app/src/adapters/types.ts`, the
one contract all three hosts share):

```ts
interface SiteGeneratorAdapter {
  id: string; label: string;
  init(ctx: AdapterContext): Promise<void>;
  listPages(): Promise<PageInfo[]>;
  renderPage(file: string): Promise<{ html: string }>;
  assetBytes(name: string): Promise<{ name; mime; base64 } | null>;
}
interface AdapterContext {
  engine:    EngineClient;   // the full Session op surface (mountSite/renderPage/…)
  fragments: { fragment(ref, kind): Promise<string> };            // first-include-miss
  content:   { renderLiquid(src, data?): Promise<string>;          // ContentApi
               renderMarkdown(md, opts?): Promise<string> };
  project:   { projectId; config; files; predefined; siteFiles; buildEpochSecs };
}
```

- `init(ctx)` stashes the context. Stock-template-style adapters call
  `ctx.engine.mountSite(tree, {activeTables, runUuid})` then
  `ctx.engine.listSitePages()`; generator-style adapters (like cycle) call
  `ctx.engine.buildSite(...)` and drive their own page module.
- `renderPage(file)` returns the HTML. Reach `ctx.fragments`/`ctx.content` for
  engine-backed tables and content anywhere in your chrome.
- The fig runner (`crates/fig/src/runner/adapter-runner.mjs`) constructs this
  exact ctx and loads your adapter's default (or named) export. An adapter that
  reaches the editor's private React page module (cycle) needs `FIG_EDITOR_APP`
  set to the editor `app/` dir; a self-contained adapter (the example above)
  needs nothing beyond the wasm module.

## 6. Template-as-data — the zero-code path

The stock FHIR template (`hl7.fhir.template` / `fhir.base.template`) is **data**,
not code: `fig render` interprets the template's layouts/includes/`_data` and
generates fragments on include-miss. No adapter, no TypeScript:

```
fig render <build-dir> -o site/            # us-core, plan-net, any staged IG
```

`<build-dir>` is a completed build tree (`temp/pages` staged pages + `_data` +
`_includes`, `output/` snapshot-complete resources, `.home/.fhir/packages`,
`input-cache/txcache`) — the F0-build shape. One engine yields every stock-style
template: a new template is a bundle + zero code. This is byte-identical to the
Publisher's Jekyll output: **plan-net 678/678, us-core 1332/1334 (+2 classified)**
(`crates/render_page/src/bin/pagecorpus.rs` is the oracle;
`examples/cli-quickstart` byte-checks it in CI).

---

### Building the wasm module (for §1, §2, §5)

The bun-runner / browser examples need a wasm-bindgen build. The scratch
toolchain recipe is in `demo/wasm-p0/README.md`. In short:

```
cargo build -p wasm_api --target wasm32-unknown-unknown --release
wasm-bindgen target/wasm32-unknown-unknown/release/wasm_api.wasm \
  --target nodejs --out-dir pkg --out-name wasm_api   # Bun/Node
#  --target web  … for the browser
```

`scripts/examples-gate.sh` skips the bun-runner example (with a note) when no
`FIG_WASM_DIR` is provided, so the gate stays green without the wasm toolchain
while still executing every fs-only example.
