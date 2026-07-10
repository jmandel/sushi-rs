# Unified CLI — Plan

> **HISTORICAL SHIPPED PLAN.** Keep this for the derivation and as-built deltas
> of `fig`; it is not the current cross-host architecture or API catalog. See
> the repository [`README.md`](../README.md) and [`hosting.md`](hosting.md).
> In particular, the callback-oriented `fig::runner` and
> `fig render --generator ts:*` described below were later retired; they are not
> available APIs.

> Status: **SHIPPED** (2026-07-04, Consolidation Pass 2). The `fig` crate
> (`crates/fig`) lands the full §2 surface; `crates/api_envelope` is the shared
> envelope. Original intent below preserved; deltas from as-built noted inline
> as **[SHIPPED: …]**.
>
> **Name recommendation:** keep `fig` (built as `fig` everywhere). It is short,
> unclaimed in this space, and reads well as `fig render`/`fig watch`. Renaming
> is a one-line-per-binary change if Josh prefers `igc`.
>
> **As-built deltas:**
> - `fig render <build-dir>` operates on a completed build tree (temp/pages +
>   output/ + packages + txcache — the F0/pagecorpus shape), so it gates
>   byte-for-byte against the page corpora. Composing build→snapshot→sitedb→pages
>   from a *raw* IG dir in one shot is a thin follow-up (the pieces are all
>   subcommands); the headline render + the parity gate are shipped.
> - The render/watch/runner **compositions** live in `fig::engine`/`fig::watch`/
>   `fig::runner` (native engine modules) — the Session can grow the same methods;
>   the F5/F6 machinery they compose (FragmentEngine + render_page) is unchanged.
> - Deletions: `snapshot_gen` + `site_db` binaries folded into `fig` (CLI logic
>   promoted to `snapshot_gen::main_cli` / `site_db::run_cli`; `fig` ships
>   deprecated alias shims for one release). `rust_sushi` kept one release for its
>   dev-oracle subcommands (lex/ast/expand/cas/deps/pkg-fish are gate infra, plan
>   §2); its user-facing build/resolve/bundle are the `fig` equivalents.
> - `fig watch` uses a dependency-free mtime poll (no notify crate) + `tiny_http`
>   for live-reload — matches the workspace's sync/blocking style.
> - Gate evidence, per subcommand, is in the render-worklog "fig" section.

This is Consolidation Pass 2 material — it DELETES binaries, not just adds one.

## 1. The idea

One binary (working name: `fig` — bikeshed later) whose subcommands map onto
the SAME engine core the wasm Session exposes. Three thin skins over one
implementation: CLI (native fs), Session (wasm/JSON envelopes), and the
library API. No subcommand may contain logic — if a subcommand needs
behavior the engine lacks, the behavior moves INTO the engine (where the
Session and editor get it too).

## 2. Surface (maps ~1:1 to Session + composition conveniences)

```
fig build <ig-dir>                 # FSH -> resources (rust_sushi build)
fig snapshot <ig-dir|sd.json>      # walk-engine snapshots (batch or single)
fig resolve <ig-dir>               # dependency closure (compile set + context closure)
fig packages fetch|bundle …        # acquisition / CDN bundle production
fig expand <vs> [--tx <url>]       # tier-1 enumerable + tx-cached fallback
fig sitedb <ig-dir> -o site.db     # S1-S7 producer (site_db build)
fig fragment <ig-dir> <ref> <kind> # render ONE fragment (the CLI face of
                                   #   first-include-miss; debugging + scripting)
fig render <ig-dir> [--template hl7.fhir.template] -o site/
                                   # THE headline: full static site at Publisher
                                   #   parity — the _genonce.sh replacement
fig watch <ig-dir> [--serve :port] # incremental dev loop: ledger-driven
                                   #   rebuilds, <1s warm edits, local server —
                                   #   the native twin of the browser editor
fig version                        # engine + pins
```

Dev/oracle harnesses (corpus, pagecorpus, gate scripts) stay as dev bins/
scripts — they are gate infrastructure, not user surface.

## 3. Decisions

- **`fig render` is the reason this exists**: "IG Publisher output in seconds,
  no Java, no Jekyll" is the whole stack in one command. It composes build →
  snapshot → sitedb → page pass → asset copy, honoring the same template-as-
  data bundles the editor uses, fragments materialized on include-miss only.
- **`fig watch` is the native editor-equivalent**: BuildState (Ledger 1) +
  page read-sets (Ledger 2) already exist; watch = fs events → dirty cone →
  re-render → (optional) live-reload serve. Same machinery as the browser
  demo, zero browser.
- **Envelopes**: human output by default, `--json` emits the SAME apiVersion
  envelope the Session returns (scripting parity; one doc for both).
- **Deletions**: `rust_sushi` CLI subcommands and the `snapshot_gen`/`site_db`
  binaries fold in (kept as deprecated aliases for one release of harness
  compatibility, then removed; harness scripts migrate to `fig`).
- **Distribution**: one static binary; `cargo install` + a release artifact.

## 3b. Non-Rust generators + fragments on the CLI (Josh, 2026-07-04)

**Key fact: the wasm module runs under Bun/Node too** — the same wasm_api
Session as the browser. So TS generators get fragments via an import, not
an IPC protocol.

- **TS-first**: `fig sitedb` then run your generator yourself (cycle's flow,
  works day one, no new contract).
- **fig-orchestrated**: `fig render --generator ts:<adapter.mjs>` — fig
  builds site.db, spawns bun with a runner harness that loads the SAME
  SiteGeneratorAdapter contract as the editor and a FragmentApi shim over
  session.renderFragment (same wasm module). One contract, three hosts
  (browser worker / fig runner / user scripts).
- **Fragments-as-files escape hatch**: `fig fragments <ig> [--kinds ...|
  --used-by-template <tpl>] -o _includes/` materializes publisher-parity
  fragments as files, reproducing the Publisher's own _includes contract —
  ANY tool (real Jekyll, python, make) can consume without knowing our
  stack. On-demand wasm is preferred; files are the compatibility floor.
- **watch + TS adapters**: ledger invalidation is language-independent —
  the adapter's next fragment() call after an edit regenerates only the
  dirty cone.

## 4. Gates

- Every subcommand's output byte-identical to the binary it replaces (the
  existing corpus/harvest/page gates re-pointed at `fig`).
- `fig render` gates on the F5 page corpora (plan-net 678/678, cycle 72/72,
  us-core current floor).
- `fig watch`: the F6 <1s warm-edit gate, native.
- TS-adapter path: cycle's generator run via the fig runner produces pages
  byte-identical to its own-process run (same rows, same fragments).
- `fig fragments -o`: emitted files byte-match the corresponding
  render-goldens entries for the used set.
- `--json` envelopes: schema-identical to Session envelopes (shared tests).

## 5. Open (decide at build time)

- Name. `fig`? `igc`? keep `rust_sushi`? (One name, everywhere.)
- Whether `fig build` keeps SUSHI-CLI flag compatibility for drop-in use.
- Config file (`fig.toml`) vs pure flags — lean pure-flags + sushi-config.
