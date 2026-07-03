# Simplification Ledger

> Standing directive (Josh, 2026-07-03): complexity-by-accretion is tracked
> and collapsed with the SAME priority as performance work. Rules: candidates
> land here as they're spotted (agents flag them in reports; coordinator
> aggregates); a consolidation pass runs at phase boundaries (like the #12
> clarity pass): one coherent change-set per gate cycle, full gates green,
> correctness never traded. "Simpler but different output" is not simpler.

## Open candidates (spotted → collapse when owning agents quiesce)

1. **wasm_api surface accretion** — init / compile / set_local_resources /
   generate_snapshot / build_site_db / expand_enumerable / mount_bundles,
   with resolve_project + render_fragment/render_page incoming. Collapse into
   a coherent session-object API (one Engine handle, grouped methods, one
   error/JSON envelope) BEFORE F6 freezes the editor against it.
2. **Editor worker protocol** — grew organically across M1/M2/#22/#32.
   Unify message envelope + progress reporting; one place for engine-call
   marshalling.
3. **Standalone-crate workspaces** — render_xhtml/render_liquid/render_md/
   render_tables/render_sd each carry their own [workspace] (deliberate, for
   parallel-agent isolation). Once churn quiets: fold into the root workspace
   in ONE commit (single lock, shared target/, workspace-wide test sweep).
4. **package-deps.cjs** — retire fully once #32's Rust-vs-cjs parity gate has
   soaked; harness scripts point at the native resolver bin.
5. **Two file-abstraction traits** — PackageSource (package_store) and
   site_db's augment FileSource. Unify or document why two.
6. **scripts/ sprawl** — harvest/gate/oracle scripts across snapshot/,
   scripts/, demo/*: one README index, kill dead ones (post-F5).
7. **Docs overlap** — wasm-editor-plan P-phases vs stock-template-renderer
   F-phases now partially supersede each other; PUBLISH.md accretes per-task
   sequences. One pass to mark superseded sections + a current-state map.
8. **Editor M2 shim layering** — vite resolveId dbShim + @cycle aliases +
   process.env stubs; revisit when the adapter contract (F6) lands — the
   contract should DELETE shims, not wrap them.
9. **cycle rust-feed-spike branch** — carries spike wiring + fixture regen;
   fold what's permanent into a clean PR to cycle main, drop the rest.

## Done

- (2026-07-03) #12 clarity pass: CAS derived-index deleted three probing
  code paths; fetch Rc; dead-code sweep — the template for these passes.
- (2026-07-03) F0 interim byte-scans (perf pass) deleted same-day by the
  CAS index — accretion lived exactly one commit.
