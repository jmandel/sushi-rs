//! DEPRECATED ALIAS SHIM (Consolidation Pass 2) — `site_db` is now `fig sitedb`.
//! Preserves the old binary's exact behavior + byte output for ONE release
//! (harness scripts keep working); prints a migration note to stderr and
//! delegates to the unchanged `site_db::run_cli` (the SAME code `fig sitedb`
//! composes). Removed next release.

#[cfg(not(target_family = "wasm"))]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> anyhow::Result<()> {
    eprintln!("note: `site_db` is deprecated — use `fig sitedb` (this alias is kept for one release).");
    let args: Vec<String> = std::env::args().collect();
    site_db::run_cli(&args)
}
