//! `template_loader` — materialize an IG-Publisher `template/` tree from FHIR
//! template packages, in pure Rust, over a [`PackageSource`].
//!
//! This is the port of the Publisher's `TemplateManager` that makes template
//! handling *truly driven*: pick any `id#version`, walk its `base` chain, union-
//! copy root→leaf, apply the `_append.` concat + the `config.json` deep-merge, and
//! emit the exact `template/` tree the Publisher's Java would stage — WITHOUT any
//! ant / XSLT / plantuml / JVM. The current host flow is documented in
//! `docs/hosting.md`.
//!
//! ## What this reproduces (and what it deliberately does NOT)
//!
//! The Publisher's `TemplateManager.loadTemplate` does two separable things:
//!   1. **Static materialization** — fetch each package in the `base` chain,
//!      unpack root→leaf into one shared dir (last-writer-wins), honor the
//!      `_append.` merge convention, deep-merge each package's `config.json`.
//!      This is ~98% `cp -r` + two trivial merge rules + ZERO XSLT (notes §3).
//!      **This module reproduces exactly that, byte-for-byte.**
//!   2. **Script hooks** — an embedded Apache Ant + Saxon XSLT 2.0 engine that
//!      runs `onLoad`/`onGenerate`/`onCheck`. Every durable, site-feeding output
//!      of those hooks is already produced natively by our Rust fragment
//!      generators (notes §2); every other output is QA/publication tooling the
//!      rendered site never reads. **This module runs NO ant, ever** — see
//!      [`AntHookError`] and [`check_no_active_hooks`] for the firm line.
//!
//! ## Citations
//!
//! Java sources (pinned): the IG-Publisher clone tag v2.2.10 (`TemplateManager`)
//! and fhir-core (`NpmPackage.unPackWithAppend`, `FileUtilities.appendBytesToFile`,
//! `JsonParser.compose`). Each ported rule cites `File.java:line`.
//!
//! ## wasm-cleanliness
//!
//! The core (`materialize`) touches NO `std::fs`: it reads every byte through a
//! [`PackageSource`] and returns an in-memory [`TemplateTree`]. The native `fig`
//! path passes a [`DiskSource`](crate::DiskSource); the browser mounts template
//! packages through the same host bundle path regular packages take.

use crate::source::PackageSource;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The materialized `template/` tree: relative path (forward-slash, from the
/// template root) → file bytes. This is the in-memory equivalent of the on-disk
/// `<rootFolder>/template/` the Publisher stages, minus the ant runtime products.
///
/// Ordering is a `BTreeMap` so the emitted tree is deterministic (path-sorted),
/// which the byte-parity gate and any downstream packing rely on.
#[derive(Debug, Clone, Default)]
pub struct TemplateTree {
    files: BTreeMap<String, Vec<u8>>,
}

impl TemplateTree {
    fn new() -> Self {
        Self::default()
    }

    /// The materialized files, path → bytes, in sorted path order.
    pub fn files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.files
    }

    /// Number of materialized files.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Get one file's bytes by relative path.
    pub fn get(&self, rel: &str) -> Option<&[u8]> {
        self.files.get(rel).map(Vec::as_slice)
    }

    /// Consume into the raw map.
    pub fn into_files(self) -> BTreeMap<String, Vec<u8>> {
        self.files
    }

    /// Overwrite (last-writer-wins) — a plain package file.
    fn put(&mut self, rel: String, bytes: Vec<u8>) {
        self.files.insert(rel, bytes);
    }

    /// The `_append.` merge (fhir-core `NpmPackage.unPack` :1481-1491 +
    /// `FileUtilities.appendBytesToFile` :65-69): for a source file `_append.X`,
    /// the target is `X`. If `X` already exists, append `\r\n` then the bytes; else
    /// write the bytes verbatim (no separator). Additionally — reproducing the
    /// brace bug at `NpmPackage.java:1489-1491` where the trailing
    /// `bytesToFile(...)` runs for BOTH branches — the literal `_append.X` file is
    /// ALSO written (last-writer-wins), which is why the staged tree carries both
    /// the merged `X` and a verbatim `_append.X`.
    fn append(&mut self, append_rel: &str, target_rel: String, bytes: Vec<u8>) {
        match self.files.get_mut(&target_rel) {
            Some(existing) => {
                existing.extend_from_slice(b"\r\n");
                existing.extend_from_slice(&bytes);
            }
            None => {
                self.files.insert(target_rel, bytes.clone());
            }
        }
        // The literal `_append.X` file (the Java brace bug): last-writer-wins.
        self.files.insert(append_rel.to_string(), bytes);
    }
}

/// One resolved package in the `base` chain, root-first.
#[derive(Debug, Clone)]
struct ChainPackage {
    /// `id#version` label (the cache dir name).
    label: String,
    /// Package id (used in the recurse-loop error message and hook attribution).
    #[allow(dead_code)]
    id: String,
}

/// One canonical Rust decision about an exact template base chain. A browser
/// host may fetch only [`missing`](TemplateResolution::missing), mount it through
/// the ordinary package path, and retry. It never parses `package.json.base`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateResolution {
    /// Root-first exact coordinates when the chain is complete. On a miss this
    /// is the already-proven leaf-to-root prefix and is diagnostic only.
    pub chain: Vec<String>,
    pub missing: Option<String>,
}

impl TemplateResolution {
    pub fn satisfied(&self) -> bool {
        self.missing.is_none()
    }
}

/// A loud, structured error naming an ant hook that would compute site-feeding
/// content this loader cannot reproduce. Emitted by [`check_no_active_hooks`] when
/// a template's `config.json` names a script whose targets are NOT in the known
/// QA-only / natively-covered set. The firm line: custom-ant templates require
/// server-side rendering — we never execute ant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AntHookError {
    /// The template package that declares the hook.
    pub package: String,
    /// The `config.script` ant build file named.
    pub script: String,
    /// The specific hook target(s) that fall outside the known set.
    pub unknown_targets: Vec<String>,
}

impl std::fmt::Display for AntHookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "template '{}' names ant script '{}' whose target(s) [{}] would compute \
             site-feeding content this renderer does not reproduce. Custom-ant \
             templates require server-side rendering; we never execute ant.",
            self.package,
            self.script,
            self.unknown_targets.join(", ")
        )
    }
}

impl std::error::Error for AntHookError {}

/// The ant hook *events* the base-template families use whose durable, site-
/// feeding effects are already produced by our native fragment generators (notes
/// §2 "site-feeding artifacts … already produced by our Rust fragment
/// generators") OR are pure QA/publication artifacts the rendered site never
/// reads (`onCheck`, the jira/pubrequest subsystem). Any target OUTSIDE this set
/// is a genuine custom-ant computation we refuse to fake — [`AntHookError`].
///
/// These are the `targets.{onLoad,onGenerate,onJekyll,onCheck}` *values* (the ant
/// target names) used across `fhir.base` / `fhir2.base` / `hl7.base` /
/// `hl7.davinci`. The stock families all funnel through these four canonical
/// event names; a bespoke template that wired a fifth event to computed site
/// content would trip the guard.
const KNOWN_HOOK_TARGETS: &[&str] = &["onLoad", "onGenerate", "onJekyll", "onCheck"];

/// Resolve `id#ver` → the concrete cache label the host has (or will) mount.
///
/// Version resolution for `latest`/`current`/`M.N.x` is the host's job (the SAME
/// [`crate::resolve::VersionIndex`] contract regular packages use — data in,
/// decisions in Rust); by the time we materialize, the caller passes a concrete
/// `id#version` label. This keeps the loader from re-implementing a registry
/// client, exactly as `TemplateManager.loadPackage` delegates to `pcm`
/// (notes §1b: "we do NOT need to port a registry client").
///
/// `root_label` is `id#version`. `root_of` maps a package label to the directory
/// under which that package's *content* lives (see [`TemplatePaths`]).
pub fn materialize(
    source: &dyn PackageSource,
    paths: &TemplatePaths,
    root_label: &str,
) -> anyhow::Result<TemplateTree> {
    let chain = walk_base_chain(source, paths, root_label)?;

    let mut tree = TemplateTree::new();
    // configs in chain order (root first) — matches the Publisher, where the
    // recursive descent adds the ROOT frame's config to `configs` before the
    // caller's (TemplateManager.java:131, configs.get(0) == root).
    let mut configs: Vec<Value> = Vec::new();

    for pkg in &chain {
        let content_root = paths.content_root(source, &pkg.label);
        // Publisher-native layout keeps the package METADATA in a sibling
        // `package/` dir (package.json + `.index.*`); the Publisher stages that
        // folder as an EMPTY dir (its `listFiles()` excludes the metadata), so its
        // files must NOT be materialized. When the content root already IS
        // `package/` (our normalized layout), do not apply this exclusion.
        let skip_package_meta =
            content_root.file_name().and_then(|n| n.to_str()) != Some("package");
        let mut files = list_files_rec(source, &content_root);
        files.sort(); // deterministic staging order within a package.
        for rel in &files {
            // `.index.json` / `.index.db` are the package-cache indexing sidecars
            // the loader (NpmPackage) writes when reading a folder; they are NOT
            // package content and are excluded from the unpacked `template/` tree.
            if is_index_sidecar(rel) {
                continue;
            }
            if skip_package_meta && (rel == "package/package.json" || rel.starts_with("package/")) {
                continue;
            }
            let abs = content_root.join(rel);
            let Ok(bytes) = source.read(&abs) else {
                continue;
            };
            let base = basename(rel);
            if let Some(target_name) = base.strip_prefix("_append.") {
                // target path = same dir, name with `_append.` stripped.
                let target_rel = replace_basename(rel, target_name);
                tree.append(rel, target_rel, bytes);
            } else {
                tree.put(rel.clone(), bytes);
            }
        }

        // config.json layer (top-level `$root/config.json`) — parse into an
        // order-preserving Value (serde_json `preserve_order`), push in chain
        // order (TemplateManager.java:123-131).
        if let Ok(cfg_bytes) = source.read(&content_root.join("config.json")) {
            match serde_json::from_slice::<Value>(&cfg_bytes) {
                Ok(v) => configs.push(v),
                Err(e) => {
                    anyhow::bail!("template '{}' config.json parse error: {e}", pkg.label)
                }
            }
        }
    }

    // Deep-merge the config layers into the root (configs[0]) and re-emit with the
    // HL7 pretty-printer, ONLY when there is more than one layer — matching
    // TemplateManager.java:216 (`if (level==0 && configs.size() > 1)`). With a
    // single layer the root's own config.json is already staged verbatim by the
    // union-copy above, so we leave it untouched (byte-identical to the source).
    if configs.len() > 1 {
        let mut merged = configs[0].clone();
        for delta in &configs[1..] {
            apply_config_changes(&mut merged, delta)?;
        }
        let composed = compose_hl7_pretty(&merged);
        tree.put("config.json".to_string(), composed.into_bytes());
    }

    // Firm line: refuse to silently drop a custom-ant computation.
    check_no_active_hooks(&chain, &configs)?;

    Ok(tree)
}

/// Where a template package's `package.json` and *content* live, relative to a
/// cache root. The two layouts we support:
///
/// - **Publisher-native** (the F0 caches, and what a `FilesystemPackageCacheManager`
///   writes): content at `<cache>/<label>/` top-level, metadata at
///   `<cache>/<label>/package/package.json`.
/// - **Our-acquisition-normalized** (`package_acquisition::normalize_extracted_package`
///   nests everything under `package/`): content at `<cache>/<label>/package/`.
///
/// `package.json` is at `<label>/package/package.json` in BOTH layouts; only the
/// *content root* differs, so we detect it per-package.
#[derive(Debug, Clone)]
pub struct TemplatePaths {
    cache_dir: PathBuf,
}

impl TemplatePaths {
    /// The cache root under which `<id>#<ver>` package dirs live.
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
        }
    }

    /// `<cache>/<label>/package/package.json` (same in both layouts).
    fn package_json_path(&self, label: &str) -> PathBuf {
        self.cache_dir
            .join(label)
            .join("package")
            .join("package.json")
    }

    /// Detect the *content root* for a package: the dir that holds `config.json`
    /// and the `content/`/`includes/`/`layouts/` trees. Publisher-native puts these
    /// at `<label>/`; our normalized layout puts them at `<label>/package/`. We
    /// prefer the top-level when it carries template content, else fall through to
    /// `package/`.
    fn content_root(&self, source: &dyn PackageSource, label: &str) -> PathBuf {
        let top = self.cache_dir.join(label);
        // A publisher-native template root has content dirs / config.json at top
        // level (alongside the `package/` metadata dir). If any of those markers
        // exist at top level, that's the content root.
        let markers = ["config.json", "includes", "content", "layouts", "liquid"];
        let has_top = markers
            .iter()
            .any(|m| source.exists(&top.join(m)) || source.is_dir(&top.join(m)));
        if has_top {
            top
        } else {
            top.join("package")
        }
    }
}

/// Walk the `base` chain from `root_label`, root-first, with a visited-set loop
/// guard. Port of `TemplateManager.installTemplate` :94-112:
/// - parent id = `package.json.base` (:101-102);
/// - parent version = `package.json.dependencies[base]` (:107-111), throw if absent;
/// - loop guard on already-loaded ids (:103-106);
/// - recurse to ROOT before staging (:111 before :116) → root-first order.
fn walk_base_chain(
    source: &dyn PackageSource,
    paths: &TemplatePaths,
    root_label: &str,
) -> anyhow::Result<Vec<ChainPackage>> {
    let resolution = resolve_base_chain(source, paths, root_label)?;
    if let Some(missing) = resolution.missing {
        anyhow::bail!("template package '{missing}' not mounted / no package.json")
    }
    Ok(resolution
        .chain
        .into_iter()
        .map(|label| ChainPackage {
            id: label_id(&label).to_string(),
            label,
        })
        .collect())
}

/// Resolve one exact template chain without materializing it. Missing packages
/// are data, while malformed manifests, non-template types, absent exact parent
/// versions, and recursion remain loud errors.
pub fn resolve_base_chain(
    source: &dyn PackageSource,
    paths: &TemplatePaths,
    root_label: &str,
) -> anyhow::Result<TemplateResolution> {
    let exact = |label: &str| {
        label
            .split_once('#')
            .is_some_and(|(id, version)| !id.is_empty() && !version.is_empty())
    };
    if !exact(root_label) {
        anyhow::bail!("template coordinate must be exact id#version: {root_label}")
    }
    // Leaf → root, then reversed to root → leaf.
    let mut leaf_to_root: Vec<String> = Vec::new();
    let mut visited: Vec<String> = Vec::new(); // preserves order for the error msg.

    let mut current = root_label.to_string();
    loop {
        let pj_path = paths.package_json_path(&current);
        let bytes = match source.read(&pj_path) {
            Ok(bytes) => bytes,
            Err(_) => {
                return Ok(TemplateResolution {
                    chain: leaf_to_root,
                    missing: Some(current),
                })
            }
        };
        let pj: Value = serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("template '{current}' package.json parse error: {e}"))?;

        let id = pj
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_else(|| label_id(&current))
            .to_string();

        // Type gate (installTemplate :97-98): must be a fhir template package.
        if let Some(ty) = pj.get("type").and_then(Value::as_str) {
            if ty != "fhir.template" && ty != "IG Template" {
                anyhow::bail!(
                    "package '{current}' is type '{ty}', not a fhir.template — not a template"
                );
            }
        }

        if visited.iter().any(|v| v == &id) {
            visited.push(id.clone());
            anyhow::bail!("Template parents recurse: {}", visited.join("->"));
        }
        visited.push(id.clone());

        leaf_to_root.push(current.clone());

        // Parent?
        let Some(base) = pj.get("base").and_then(Value::as_str) else {
            break; // root reached (no `base`).
        };
        // Loop guard on the base id BEFORE recursing (installTemplate :103-106).
        if visited.iter().any(|v| v == base) {
            visited.push(base.to_string());
            anyhow::bail!("Template parents recurse: {}", visited.join("->"));
        }
        // Version of the parent from THIS package's dependencies map (:107-111).
        let ver = pj
            .get("dependencies")
            .and_then(Value::as_object)
            .and_then(|deps| deps.get(base))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "template '{current}' declares base '{base}' but does not list it in \
                     dependencies (cannot resolve the parent version)"
                )
            })?;
        current = format!("{base}#{ver}");
    }

    leaf_to_root.reverse(); // → root-first.
    Ok(TemplateResolution {
        chain: leaf_to_root,
        missing: None,
    })
}

/// The `applyConfigChanges` deep-merge (TemplateManager.java:227-246), folding
/// `delta` into `base` in place:
/// - **objects** → recursive deep-merge (:234-235);
/// - **arrays** → APPEND (`addAll`, :236-237) — concatenation, not replace/union;
/// - **primitives** → REPLACE (:238-240);
/// - **new keys** → add (:242-244);
/// - **type mismatch** on an existing key → error (:232-233).
fn apply_config_changes(base: &mut Value, delta: &Value) -> anyhow::Result<()> {
    let (Some(base_obj), Some(delta_obj)) = (base.as_object_mut(), delta.as_object()) else {
        anyhow::bail!("applyConfigChanges expects JSON objects at the top level");
    };
    for (key, new_el) in delta_obj {
        if let Some(base_el) = base_obj.get(key) {
            // Type-mismatch guard (:232-233): array/object/primitive must agree.
            let same_kind = base_el.is_array() == new_el.is_array()
                && base_el.is_object() == new_el.is_object();
            if !same_kind {
                anyhow::bail!(
                    "config merge type mismatch on key '{key}': base is {} but override is {}",
                    kind_name(base_el),
                    kind_name(new_el)
                );
            }
            if new_el.is_object() {
                // Deep-merge in place (order preserved) — objects recurse.
                apply_config_changes(base_obj.get_mut(key).unwrap(), new_el)?;
            } else if new_el.is_array() {
                // Array APPEND (`addAll`, :236-237) — in place, order preserved.
                if let (Some(base_arr), Some(new_arr)) = (
                    base_obj.get_mut(key).unwrap().as_array_mut(),
                    new_el.as_array(),
                ) {
                    base_arr.extend(new_arr.iter().cloned());
                }
            } else {
                // Primitive REPLACE (:238-240): `baseConfig.remove(name)` then
                // `add(name, value)`. In HL7's ordered JsonObject, remove+add moves
                // the key to the END of the property order — so the replaced key is
                // re-appended, NOT updated in place. Replicate with an order-
                // preserving `shift_remove` (NOT the default `swap_remove`, which
                // would drag the last key into the vacated slot) + insert-append.
                base_obj.shift_remove(key);
                base_obj.insert(key.clone(), new_el.clone());
            }
        } else {
            base_obj.insert(key.clone(), new_el.clone());
        }
    }
    Ok(())
}

fn kind_name(v: &Value) -> &'static str {
    match v {
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
    }
}

// ---------------------------------------------------------------------------
// HL7 JsonParser.compose(element, pretty=true) — byte-exact port.
//   JsonParser.java:632-712, escapeJson at Utilities.java:1005 (called with
//   escapeUnicodeWhitespace=false, JsonParser.java:706), LINE_BREAK default '\n'
//   (JsonParser.java:60), ARRAY_NESTING_OFFSET default 0 (JsonParser.java:65).
// ---------------------------------------------------------------------------

/// Compose a JSON value in the HL7 `JsonParser` pretty format the Publisher writes
/// merged `config.json` with (TemplateManager.java:221
/// `JsonParser.compose(config, true)`). Byte-exact so the parity gate holds.
pub fn compose_hl7_pretty(v: &Value) -> String {
    let mut b = String::new();
    write_value(&mut b, v, true, 0);
    b.push('\n'); // trailing LINE_BREAK (JsonParser.java:617-618).
    b
}

const LINE_BREAK: &str = "\n";
const ARRAY_NESTING_OFFSET: usize = 0;

fn pad(b: &mut String, n: usize) {
    for _ in 0..n {
        b.push(' ');
    }
}

fn write_value(b: &mut String, e: &Value, pretty: bool, indent: usize) {
    match e {
        Value::Array(arr) => write_array(b, arr, pretty, indent),
        Value::Object(obj) => write_object(b, obj, pretty, indent),
        Value::Bool(x) => b.push_str(if *x { "true" } else { "false" }),
        Value::Null => b.push_str("null"),
        Value::Number(n) => b.push_str(&n.to_string()),
        Value::String(s) => {
            b.push('"');
            escape_json(b, s);
            b.push('"');
        }
    }
}

fn write_array(b: &mut String, arr: &[Value], pretty: bool, indent: usize) {
    b.push('[');
    // complexity rule (JsonParser.java:638-654): size>6, OR any nested array/object,
    // OR total primitive-json length > 60.
    let mut complex = arr.len() > 6;
    if !complex {
        let mut length = 0usize;
        for i in arr {
            match i {
                Value::Array(_) | Value::Object(_) => {
                    complex = true;
                    break;
                }
                other => length += primitive_json_len(other),
            }
        }
        if !complex && length > 60 {
            complex = true;
        }
    }
    let mut first = true;
    for i in arr {
        if first {
            first = false;
        } else {
            // ", " inline, or "," + break + indent when complex (:659-666).
            b.push_str(if pretty && !complex { ", " } else { "," });
            if pretty && complex {
                b.push_str(LINE_BREAK);
                pad(b, indent + ARRAY_NESTING_OFFSET);
            }
        }
        write_value(b, i, pretty && complex, indent + ARRAY_NESTING_OFFSET);
    }
    b.push(']');
}

fn write_object(b: &mut String, obj: &Map<String, Value>, pretty: bool, indent: usize) {
    b.push('{');
    let mut first = true;
    for (k, val) in obj {
        if first {
            first = false;
        } else {
            b.push(',');
        }
        if pretty {
            b.push_str(LINE_BREAK);
            pad(b, indent + 2);
        }
        b.push('"');
        b.push_str(k); // property NAME is emitted raw (JsonParser.java:693-694).
        b.push_str(if pretty { "\" : " } else { "\":" });
        write_value(b, val, pretty, indent + 2);
    }
    if pretty {
        b.push_str(LINE_BREAK);
        pad(b, indent);
    }
    b.push('}');
}

/// `JsonPrimitive.toJson().length()` for the array-complexity heuristic
/// (JsonParser.java:642-643) — the composed length of a primitive, INCLUDING the
/// surrounding quotes for strings (that is what `toJson()` returns).
fn primitive_json_len(v: &Value) -> usize {
    match v {
        Value::String(s) => {
            let mut tmp = String::new();
            escape_json(&mut tmp, s);
            tmp.len() + 2 // quotes
        }
        Value::Bool(x) => {
            if *x {
                4
            } else {
                5
            }
        }
        Value::Null => 4,
        Value::Number(n) => n.to_string().len(),
        // arrays/objects flip `complex` before length is consulted.
        _ => 0,
    }
}

/// `Utilities.escapeJson(value, escapeUnicodeWhitespace=false)` (Utilities.java:
/// 1005-1031, called with `false` at JsonParser.java:706): escape `\r \n \t " \\`
/// and control chars < 32 as `\uXXXX`; everything else verbatim.
fn escape_json(b: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '\r' => b.push_str("\\r"),
            '\n' => b.push_str("\\n"),
            '\t' => b.push_str("\\t"),
            '"' => b.push_str("\\\""),
            '\\' => b.push_str("\\\\"),
            ' ' => b.push(' '),
            c if (c as u32) < 32 => {
                b.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => b.push(c),
        }
    }
}

// ---------------------------------------------------------------------------
// Firm line: no ant execution, ever.
// ---------------------------------------------------------------------------

/// Verify no template in the chain declares an ant hook that would compute
/// site-feeding content outside the known-QA / natively-covered set. If it does,
/// return a loud [`AntHookError`] rather than silently materializing a tree whose
/// dynamic content will be missing.
///
/// The stock base families (`fhir.base`/`fhir2.base`/`hl7.base`/`hl7.davinci`)
/// wire only the four canonical events ([`KNOWN_HOOK_TARGETS`]); their durable
/// site-feeding effects are reproduced by our native fragment generators, and
/// their `onCheck`/jira outputs are QA-only (notes §2). A template that wires a
/// *different* target — genuine custom-ant computation — trips this guard.
fn check_no_active_hooks(chain: &[ChainPackage], configs: &[Value]) -> anyhow::Result<()> {
    // `configs` is root-first, one entry per package that HAS a config.json; map it
    // back to packages by matching chain order (a package without config.json has
    // no ant hooks).
    // Re-derive per-config the owning package by walking the chain and consuming
    // configs in order — a package contributes a config iff it had one. Since we
    // cannot know which packages contributed here, re-check every config for the
    // `targets` map and validate its values.
    for cfg in configs {
        let Some(script) = cfg.get("script").and_then(Value::as_str) else {
            continue; // no script → no ant hooks (pure content overlay).
        };
        let Some(targets) = cfg.get("targets").and_then(Value::as_object) else {
            continue; // script but no targets map: nothing to invoke.
        };
        let mut unknown: Vec<String> = Vec::new();
        for (_event, target) in targets {
            if let Some(t) = target.as_str() {
                if !KNOWN_HOOK_TARGETS.contains(&t) {
                    unknown.push(t.to_string());
                }
            }
        }
        if !unknown.is_empty() {
            unknown.sort();
            unknown.dedup();
            // Attribute to the best-known package: the leaf that owns a script.
            let package = chain
                .last()
                .map(|p| p.label.clone())
                .unwrap_or_else(|| "<unknown>".to_string());
            return Err(AntHookError {
                package,
                script: script.to_string(),
                unknown_targets: unknown,
            }
            .into());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

/// Recursively list every file under `root`, returning forward-slash relative
/// paths. Directories are descended; symlinks/errors are skipped. Pure
/// [`PackageSource`] I/O (no `std::fs`).
fn list_files_rec(source: &dyn PackageSource, root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = source.read_dir(&dir) else {
            continue;
        };
        for ent in entries {
            let child = dir.join(&ent.file_name);
            if ent.is_file {
                if let Ok(rel) = child.strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            } else {
                // Could be a directory; recurse. (BundleSource/DiskSource both
                // report subdirs with is_file=false.)
                stack.push(child);
            }
        }
    }
    out
}

fn is_index_sidecar(rel: &str) -> bool {
    let base = basename(rel);
    base == ".index.json" || base == ".index.db"
}

fn basename(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// Replace the basename of a forward-slash rel path with `new_base`.
fn replace_basename(rel: &str, new_base: &str) -> String {
    match rel.rfind('/') {
        Some(i) => format!("{}/{}", &rel[..i], new_base),
        None => new_base.to_string(),
    }
}

fn label_id(label: &str) -> &str {
    label.split_once('#').map(|(id, _)| id).unwrap_or(label)
}

/// Whether a staged-tree path is an **ant runtime product** — a file the
/// Publisher's ant hooks (onLoad/onGenerate/onCheck + the jira + translation
/// subsystems) WRITE into the `template/` dir at build time. These are build
/// products, NOT part of static materialization: the loader does not (and cannot,
/// without a JVM) produce them, and — critically — the rendered *site* never reads
/// them (notes §2: onCheck + jira are QA/publication only; the site-feeding
/// onGenerate outputs are produced natively by our fragment generators). Exposed
/// so the byte-parity gate and any packing can classify/exclude them.
///
/// The set is ENUMERATED, not hand-waved. It is the union of:
/// - the us-core §3 runtime set: `onLoad-*`, `onGenerate*`, `onCheck*`, `jira*`,
///   `properties.txt`, `versions.txt`, `*-validation*`;
/// - the plan-net (davinci) onCheck copy-backs: the rendered QA HTML pages
///   (`index.html`, `ChangeHistory.html`, `confexpectations.html`,
///   `downloads.html`, `project.html`, `reading.html`, `security.html`) and the
///   davinci capability/resource dumps (`davinci.*.xml`);
/// - the onGenerate/jira properties + list products (`packagelist.txt`,
///   `root.properties`, `menu.properties`, `package-list.json`, `cache.ini` is
///   NOT here — it is real package content);
/// - the translation subsystem's regenerated `.xml` (`translations/*.xml`), which
///   the ant `.po`→`.xml`→`.json` pipeline synthesizes from the raw `.po`/`.json`.
///   (The raw `translations/*.json` themselves are real package content that the
///   ant step OVERWRITES in place — see [`is_ant_overwritten_source`].)
pub fn is_ant_runtime_product(rel: &str) -> bool {
    let base = basename(rel);
    let lower = base.to_ascii_lowercase();
    let by_prefix = lower.starts_with("onload-")
        || lower.starts_with("onload.")
        || lower.starts_with("ongenerate-")
        || lower.starts_with("ongenerate.")
        || lower.starts_with("oncheck-")
        || lower.starts_with("oncheck.")
        || lower.starts_with("jira-")
        || lower.starts_with("jira.");
    let by_name = matches!(
        lower.as_str(),
        "properties.txt"
            | "versions.txt"
            | "package-list.json"
            | "packagelist.txt"
            | "root.properties"
            | "menu.properties"
            // davinci onCheck copy-backs (rendered QA HTML + capability dumps).
            | "index.html"
            | "changehistory.html"
            | "confexpectations.html"
            | "downloads.html"
            | "project.html"
            | "reading.html"
            | "security.html"
            | "davinci.capabilities.xml"
            | "davinci.resources.xml"
    );
    let by_infix = lower.contains("-validation") || lower.contains("validation-");
    // translation subsystem: regenerated `.xml` under translations/.
    let translation_xml = rel.starts_with("translations/") && lower.ends_with(".xml");
    by_prefix || by_name || by_infix || translation_xml
}

/// Whether a staged file is a real package source that an ant hook OVERWRITES in
/// place (so the staged bytes differ from the materialized/raw bytes, but the file
/// IS genuine materialization content — just build-time-mutated). Currently the
/// translation `strings*.json`: the ant `.po`→`.json` merge re-emits them. Our
/// static materialization correctly stages the RAW `.json` (byte-identical to the
/// package); the gate uses this to classify the "differ" as expected-overwrite
/// rather than a materialization error.
pub fn is_ant_overwritten_source(rel: &str) -> bool {
    rel.starts_with("translations/")
        && basename(rel).ends_with(".json")
        && basename(rel).starts_with("strings")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::BundleSource;

    /// A minimal in-memory template chain: root `r.base` (has config.json +
    /// content) → leaf `r.leaf` (base=r.base, an `_append.` overlay).
    fn mount_chain() -> (BundleSource, TemplatePaths) {
        let mut src = BundleSource::new();
        // Root: package.json under package/, content at TOP LEVEL of the label dir.
        // BundleSource mounts everything under `<label>/package/`, so for this test
        // we mount BOTH the metadata AND the content under package/ (the normalized
        // layout), and TemplatePaths detects the content root at package/.
        src.mount_package(
            "r.base#1.0.0",
            vec![
                (
                    "package.json",
                    br#"{"name":"r.base","version":"1.0.0","type":"fhir.template"}"#.to_vec(),
                ),
                (
                    "config.json",
                    br#"{"a":1,"arr":["x"],"obj":{"k":"v"}}"#.to_vec(),
                ),
                ("includes/fragment-css.html", b"BASE".to_vec()),
            ],
        );
        src.mount_package(
            "r.leaf#1.0.0",
            vec![
                (
                    "package.json",
                    br#"{"name":"r.leaf","version":"1.0.0","type":"fhir.template","base":"r.base","dependencies":{"r.base":"1.0.0"}}"#.to_vec(),
                ),
                (
                    "config.json",
                    br#"{"a":2,"arr":["y"],"obj":{"k2":"v2"},"new":true}"#.to_vec(),
                ),
                ("includes/_append.fragment-css.html", b"LEAF".to_vec()),
            ],
        );
        let paths = TemplatePaths::new(src.cache_root().to_path_buf());
        (src, paths)
    }

    #[test]
    fn walks_base_chain_root_first() {
        let (src, paths) = mount_chain();
        let chain = walk_base_chain(&src, &paths, "r.leaf#1.0.0").unwrap();
        let ids: Vec<&str> = chain.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["r.base", "r.leaf"], "root first, leaf last");
    }

    #[test]
    fn append_uses_crlf_and_writes_literal() {
        let (src, paths) = mount_chain();
        let tree = materialize(&src, &paths, "r.leaf#1.0.0").unwrap();
        // fragment-css.html = BASE + \r\n + LEAF (CRLF separator, base existed).
        assert_eq!(
            tree.get("includes/fragment-css.html").unwrap(),
            b"BASE\r\nLEAF"
        );
        // literal _append file also present (the Java brace bug), = leaf bytes.
        assert_eq!(
            tree.get("includes/_append.fragment-css.html").unwrap(),
            b"LEAF"
        );
    }

    #[test]
    fn config_deep_merge_semantics() {
        let (src, paths) = mount_chain();
        let tree = materialize(&src, &paths, "r.leaf#1.0.0").unwrap();
        let composed = std::str::from_utf8(tree.get("config.json").unwrap()).unwrap();
        let v: Value = serde_json::from_str(composed).unwrap();
        assert_eq!(v["a"], serde_json::json!(2), "primitive replaced");
        assert_eq!(v["arr"], serde_json::json!(["x", "y"]), "array appended");
        assert_eq!(
            v["obj"]["k"],
            serde_json::json!("v"),
            "object deep-merged (kept)"
        );
        assert_eq!(
            v["obj"]["k2"],
            serde_json::json!("v2"),
            "object deep-merged (added)"
        );
        assert_eq!(v["new"], serde_json::json!(true), "new key added");
    }

    #[test]
    fn append_without_base_writes_verbatim() {
        // An `_append.X` with no pre-existing X → verbatim, no separator.
        let mut tree = TemplateTree::new();
        tree.append(
            "d/_append.foo.html",
            "d/foo.html".to_string(),
            b"ONLY".to_vec(),
        );
        assert_eq!(tree.get("d/foo.html").unwrap(), b"ONLY");
        assert_eq!(tree.get("d/_append.foo.html").unwrap(), b"ONLY");
    }

    #[test]
    fn recurse_guard_trips() {
        let mut src = BundleSource::new();
        src.mount_package(
            "a#1.0.0",
            vec![(
                "package.json",
                br#"{"name":"a","type":"fhir.template","base":"b","dependencies":{"b":"1.0.0"}}"#
                    .to_vec(),
            )],
        );
        src.mount_package(
            "b#1.0.0",
            vec![(
                "package.json",
                br#"{"name":"b","type":"fhir.template","base":"a","dependencies":{"a":"1.0.0"}}"#
                    .to_vec(),
            )],
        );
        let paths = TemplatePaths::new(src.cache_root().to_path_buf());
        let err = walk_base_chain(&src, &paths, "a#1.0.0").unwrap_err();
        assert!(err.to_string().contains("recurse"), "got: {err}");
    }

    #[test]
    fn missing_base_dependency_version_errors() {
        let mut src = BundleSource::new();
        src.mount_package(
            "a#1.0.0",
            vec![(
                "package.json",
                br#"{"name":"a","type":"fhir.template","base":"b"}"#.to_vec(),
            )],
        );
        let paths = TemplatePaths::new(src.cache_root().to_path_buf());
        let err = walk_base_chain(&src, &paths, "a#1.0.0").unwrap_err();
        assert!(err.to_string().contains("dependencies"), "got: {err}");
    }

    #[test]
    fn resolution_reports_one_exact_missing_coordinate_without_host_parsing() {
        let mut src = BundleSource::new();
        src.mount_package(
            "leaf#2.0.0",
            vec![(
                "package.json",
                br#"{"name":"leaf","type":"fhir.template","base":"base","dependencies":{"base":"1.0.0"}}"#
                    .to_vec(),
            )],
        );
        let paths = TemplatePaths::new(src.cache_root().to_path_buf());
        let first = resolve_base_chain(&src, &paths, "leaf#2.0.0").unwrap();
        assert!(!first.satisfied());
        assert_eq!(first.missing.as_deref(), Some("base#1.0.0"));

        src.mount_package(
            "base#1.0.0",
            vec![(
                "package.json",
                br#"{"name":"base","type":"fhir.template"}"#.to_vec(),
            )],
        );
        let complete = resolve_base_chain(&src, &paths, "leaf#2.0.0").unwrap();
        assert!(complete.satisfied());
        assert_eq!(complete.chain, vec!["base#1.0.0", "leaf#2.0.0"]);
    }

    #[test]
    fn custom_ant_hook_is_rejected() {
        let chain = vec![ChainPackage {
            label: "custom#1.0.0".into(),
            id: "custom".into(),
        }];
        let configs = vec![serde_json::json!({
            "script": "scripts/custom.xml",
            "targets": {"onWeird": "computeStuff"}
        })];
        let err = check_no_active_hooks(&chain, &configs).unwrap_err();
        assert!(err.to_string().contains("server-side"), "got: {err}");
    }

    #[test]
    fn known_hooks_pass() {
        let chain = vec![ChainPackage {
            label: "fhir.base.template#1.0.0".into(),
            id: "fhir.base.template".into(),
        }];
        let configs = vec![serde_json::json!({
            "script": "scripts/ant.xml",
            "targets": {"onLoad":"onLoad","onGenerate":"onGenerate","onCheck":"onCheck"}
        })];
        assert!(check_no_active_hooks(&chain, &configs).is_ok());
    }

    #[test]
    fn hl7_pretty_small_array_inline() {
        let v = serde_json::json!({"formats":["xml","json","ttl"]});
        let s = compose_hl7_pretty(&v);
        assert_eq!(s, "{\n  \"formats\" : [\"xml\", \"json\", \"ttl\"]\n}\n");
    }

    #[test]
    fn hl7_pretty_complex_array_broken() {
        // 7 items → complex → one-per-line at indent+2 (parent object indent 0 → +2).
        let v = serde_json::json!({"p":["a","b","c","d","e","f","g"]});
        let s = compose_hl7_pretty(&v);
        assert_eq!(
            s,
            "{\n  \"p\" : [\"a\",\n  \"b\",\n  \"c\",\n  \"d\",\n  \"e\",\n  \"f\",\n  \"g\"]\n}\n"
        );
    }

    #[test]
    fn hl7_pretty_object_and_bool() {
        let v = serde_json::json!({"obj":{"java":false}});
        let s = compose_hl7_pretty(&v);
        assert_eq!(s, "{\n  \"obj\" : {\n    \"java\" : false\n  }\n}\n");
    }
}
