//! `fig render --generator ts:<adapter.mjs>` — the Bun runner harness.
//!
//! fig produces the render inputs (a mounted site tree / the compiled project),
//! then spawns `bun` with a runner script that loads the SAME
//! `SiteGeneratorAdapter` contract the editor uses and a FragmentApi/ContentApi
//! shim over the wasm module's `Session` (the same wasm_api the browser worker
//! loads). One contract, three hosts: browser worker / fig runner / user scripts.
//!
//! The runner script is emitted to a temp dir and given, via env, the paths to:
//!   - the adapter .mjs (imported for its default/named `SiteGeneratorAdapter`),
//!   - the wasm-bindgen NODEJS-target module dir (`wasm_api.js` + `_bg.wasm`),
//!   - the project inputs (config + files + predefined + siteFiles as JSON), and
//!   - the mounted package bundles (as the Session `init` shape).
//! It builds the `AdapterContext` exactly as the editor's App.tsx does, then
//! drives `init → listPages → renderPage(*)` and writes pages to the out dir.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Inputs for a generator run.
pub struct GeneratorRun<'a> {
    /// Path to the adapter `.mjs`.
    pub adapter: &'a Path,
    /// Directory holding the wasm-bindgen NODEJS-target build
    /// (`wasm_api.js` + `wasm_api_bg.wasm`). fig requires this because the
    /// FragmentApi/ContentApi shims run the SAME wasm module as the browser.
    pub wasm_dir: &'a Path,
    /// The project input JSON path (the AdapterProject shape:
    /// `{ projectId, config, files, predefined, siteFiles, buildEpochSecs }`).
    pub project_json: &'a Path,
    /// The mounted-bundles JSON path (the Session `init` shape:
    /// `[{ label, files: { name: b64 } }]`).
    pub bundles_json: &'a Path,
    /// Output directory for rendered pages.
    pub out_dir: &'a Path,
}

/// The embedded runner script. It mirrors the editor's App.tsx `adapter.init`
/// ctx construction and the engine.worker.ts `unwrap` envelope check, so a
/// generator driven here behaves identically to its browser/worker run.
const RUNNER_MJS: &str = include_str!("runner/adapter-runner.mjs");

/// Locate `bun` on PATH.
pub fn bun_available() -> bool {
    Command::new("bun")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run a generator adapter through the bun harness. Returns the number of pages
/// the adapter reported rendering (parsed from the runner's stdout summary).
pub fn run_generator(run: &GeneratorRun) -> Result<usize> {
    if !bun_available() {
        bail!("`bun` is not on PATH — `fig render --generator ts:<adapter.mjs>` needs Bun");
    }
    if !run.adapter.is_file() {
        bail!("adapter not found: {}", run.adapter.display());
    }
    if !run.wasm_dir.join("wasm_api.js").is_file() {
        bail!(
            "no wasm module at {} — build the nodejs-target wasm_api first \
             (see demo/wasm-p0/README.md); the runner loads the SAME module as the browser",
            run.wasm_dir.display()
        );
    }
    std::fs::create_dir_all(run.out_dir)?;

    // Emit the runner script beside the out dir so relative imports are stable.
    let runner_path = run.out_dir.join(".fig-adapter-runner.mjs");
    std::fs::write(&runner_path, RUNNER_MJS)
        .with_context(|| format!("write runner {}", runner_path.display()))?;

    let status = Command::new("bun")
        .arg(&runner_path)
        .env("FIG_ADAPTER", abspath(run.adapter)?)
        .env("FIG_WASM_DIR", abspath(run.wasm_dir)?)
        .env("FIG_PROJECT_JSON", abspath(run.project_json)?)
        .env("FIG_BUNDLES_JSON", abspath(run.bundles_json)?)
        .env("FIG_OUT_DIR", abspath(run.out_dir)?)
        .status()
        .context("spawn bun")?;
    if !status.success() {
        bail!("bun runner exited with {status}");
    }

    // The runner writes a manifest of the pages it produced.
    let manifest = run.out_dir.join(".fig-pages.json");
    let count = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
        .map(|v| v.len())
        .unwrap_or(0);
    Ok(count)
}

fn abspath(p: &Path) -> Result<PathBuf> {
    Ok(std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
}
