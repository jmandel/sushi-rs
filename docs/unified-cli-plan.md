# Unified CLI — Plan

> Status: PLANNED (Josh, 2026-07-04). Timing: after F6 lands (the Session API
> + fragment surface it mirrors must be frozen first). This is Consolidation
> Pass 2 material — it DELETES binaries, not just adds one.

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
