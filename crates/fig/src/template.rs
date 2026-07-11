//! `fig::template` — the native template-loader surface: acquire a
//! `template#version` chain via the SAME acquisition machinery regular packages
//! use, then materialize it with [`package_store::template_loader`].
//!
//! This is the native half of "make template handling truly driven" (task #39):
//! `fig render --template <id#ver>` fetches + materializes on the fly, replacing
//! the frozen-F0-snapshot path. A pre-materialized dir stays available as the
//! explicit `--template-dir` escape hatch (see [`materialized_dir_or_acquire`]).
//!
//! Rust decides (the `base`-chain walk, the merge rules); the host fetches (the
//! acquisition registry client) — the same split the package resolver uses.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use package_acquisition::{default_registries, Coordinate, PackageCas};
use package_store::template_loader::{materialize, TemplatePaths, TemplateTree};
use package_store::DiskSource;
use serde_json::Value;

/// Acquire a template package chain (root `coord`, walking `base` links) into
/// `cache_dir` using the CAS, then materialize the `template/` tree.
///
/// The chain is walked iteratively: acquire the current package, read its
/// `package.json` for `base` + `dependencies[base]`, acquire the parent, repeat to
/// the root (no `base`). A visited-set guards against a recursive chain. Each
/// package lands under `<cache_dir>/<id>#<ver>/package/` (the acquisition-
/// normalized layout, which [`TemplatePaths`] detects).
///
/// Returns the materialized [`TemplateTree`] (in-memory; the caller writes it
/// wherever the render path expects it).
pub fn acquire_and_materialize(
    root_coord: &str,
    cache_dir: &Path,
    cas: &PackageCas,
    registries: &[String],
    offline: bool,
) -> Result<TemplateTree> {
    std::fs::create_dir_all(cache_dir)?;

    let mut current = Coordinate::parse(root_coord)?;
    let mut visited: Vec<String> = Vec::new();

    loop {
        let label = current.label();
        // Acquire (or read from CAS) + materialize into the template cache dir.
        cas.materialize_package_resolving(&current, cache_dir, registries, offline)
            .with_context(|| format!("acquire template package {label}"))?;

        let pj_path = cache_dir.join(&label).join("package").join("package.json");
        let bytes = std::fs::read(&pj_path)
            .with_context(|| format!("read package.json for template {label}"))?;
        let pj: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse package.json for template {label}"))?;

        let id = pj
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(current.name.as_str())
            .to_string();
        if visited.iter().any(|v| v == &id) {
            visited.push(id.clone());
            anyhow::bail!("Template parents recurse: {}", visited.join("->"));
        }
        visited.push(id);

        let Some(base) = pj.get("base").and_then(Value::as_str) else {
            break; // root reached.
        };
        let ver = pj
            .get("dependencies")
            .and_then(Value::as_object)
            .and_then(|d| d.get(base))
            .and_then(Value::as_str)
            .with_context(|| {
                format!(
                    "template {label} declares base '{base}' but does not list it in dependencies"
                )
            })?;
        current = Coordinate::parse(&format!("{base}#{ver}"))?;
    }

    let root_label = Coordinate::parse(root_coord)?.label();
    let paths = TemplatePaths::new(cache_dir);
    materialize(&DiskSource, &paths, &root_label)
}

/// Write a [`TemplateTree`] out to `dir` as an on-disk `template/`-shaped tree
/// (path-preserving). Returns the number of files written.
pub fn write_tree(tree: &TemplateTree, dir: &Path) -> Result<usize> {
    let mut n = 0;
    for (rel, bytes) in tree.files() {
        let dest = dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, bytes).with_context(|| format!("write {}", dest.display()))?;
        n += 1;
    }
    Ok(n)
}

/// Resolve the render-time template source directory for the `fig render`
/// `--template`/`--template-dir` options:
///
/// - `--template-dir <dir>` (escape hatch) → use the pre-materialized dir as-is.
/// - `--template <id#ver>` (the default, driven path) → acquire + materialize into
///   `<workdir>/template` and return that.
/// - neither → `Ok(None)` (the render path falls back to the staged tree).
///
/// `workdir` is where the driven path writes the materialized tree (typically the
/// build dir); `cache_dir` is the template package cache root the acquisition
/// writes into (defaults to `<workdir>/.fig-template-cache`).
pub fn materialized_dir_or_acquire(
    template: Option<&str>,
    template_dir: Option<&str>,
    workdir: &Path,
    cache_dir: Option<&Path>,
    offline: bool,
) -> Result<Option<PathBuf>> {
    if let Some(dir) = template_dir {
        return Ok(Some(PathBuf::from(dir)));
    }
    let Some(coord) = template else {
        return Ok(None);
    };
    let cache = cache_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workdir.join(".fig-template-cache"));
    let cas = PackageCas::new(PackageCas::default_root()?);
    let registries = default_registries();
    let tree = acquire_and_materialize(coord, &cache, &cas, &registries, offline)?;
    let out = workdir.join("template");
    write_tree(&tree, &out)?;
    Ok(Some(out))
}
