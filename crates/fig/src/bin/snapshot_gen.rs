//! DEPRECATED ALIAS SHIM (Consolidation Pass 2) — `snapshot_gen` is now
//! `fig snapshot`. This shim preserves the old binary's exact behavior and byte
//! output for ONE release so harness scripts keep working; it prints a migration
//! note to stderr and delegates to the unchanged `snapshot_gen::main_cli`
//! (the SAME engine `fig snapshot` composes). Removed next release.

#[cfg(not(target_family = "wasm"))]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> anyhow::Result<()> {
    eprintln!(
        "note: `snapshot_gen` is deprecated — use `fig snapshot` (this alias is kept for one release)."
    );
    snapshot_gen::main_cli()
}
