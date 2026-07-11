//! S6 — augmentation. A faithful port of `site-gen/ingest.ts`: builds the
//! Pages / Menu / SiteConfig / Assets rows from the IG page tree, sushi-config
//! `menu:`, images, and referenced includes. PlantUML is OUT OF SCOPE (§2b): a
//! referenced `.svg` that is not present under an include/image dir fails loud.
//! Missing page files fail loud too.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Result};
use prepared_guide::{MenuNode, PageNode, PreparedAsset, PreparedPath};
use serde_json::Value;

use crate::model::{AssetRow, MenuRow, PageRow, SiteConfigRow, SiteDb};

/// A read source for S6 inputs. The disk path (`DiskFiles`) reads the IG project
/// tree via `std::fs`; the wasm/editor path (`MemFiles`) serves the same content
/// from an in-memory `path -> bytes` map (the editor VFS). Keeping S6 behind this
/// trait is what makes the pipeline wasm-runnable (§2b: "S7 is a thin sink,
/// swappable"; the same applies to S6's reads).
pub trait FileSource {
    /// Read a file's bytes at a project-relative-or-absolute path (as joined by
    /// the caller). Returns `None` when the file does not exist.
    fn read(&self, path: &Path) -> Option<Vec<u8>>;
    /// Whether `path` names an existing file.
    fn is_file(&self, path: &Path) -> bool {
        self.read(path).is_some()
    }
    /// Recursively list files under `dir`, returning each file's path relative to
    /// `dir` (POSIX separators), sorted. Directories that do not exist yield `[]`.
    fn list_recursive(&self, dir: &Path) -> Vec<String>;
}

/// Disk-backed `FileSource` (the native producer). Byte-identical to the prior
/// direct `std::fs` calls.
pub struct DiskFiles;

impl FileSource for DiskFiles {
    fn read(&self, path: &Path) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
    fn list_recursive(&self, dir: &Path) -> Vec<String> {
        fn walk(root: &Path, cur: &Path, out: &mut Vec<String>) {
            let Ok(rd) = std::fs::read_dir(cur) else {
                return;
            };
            let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let p = e.path();
                let Ok(ty) = e.file_type() else { continue };
                if ty.is_dir() {
                    walk(root, &p, out);
                } else if ty.is_file() {
                    if let Ok(rel) = p.strip_prefix(root) {
                        out.push(rel.to_string_lossy().replace('\\', "/"));
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }
}

/// In-memory `FileSource` over a `path -> bytes` map (the editor VFS). Keys are
/// the absolute paths the caller joins (`ig_dir` + relative), matching how the
/// disk path addresses files, so `AugmentInputs` dir paths work unchanged.
pub struct MemFiles {
    files: std::collections::BTreeMap<PathBuf, Vec<u8>>,
}

impl MemFiles {
    pub fn new(files: std::collections::BTreeMap<PathBuf, Vec<u8>>) -> Self {
        Self { files }
    }
}

impl FileSource for MemFiles {
    fn read(&self, path: &Path) -> Option<Vec<u8>> {
        self.files.get(path).cloned()
    }
    fn is_file(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }
    fn list_recursive(&self, dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for key in self.files.keys() {
            if let Ok(rel) = key.strip_prefix(dir) {
                let s = rel.to_string_lossy().replace('\\', "/");
                if !s.is_empty() {
                    out.push(s);
                }
            }
        }
        out.sort();
        out
    }
}

/// Inputs for S6 that live in the IG repo (declared input set, §2b).
pub struct AugmentInputs<'a> {
    /// The parsed ImplementationGuide resource (source of definition.page).
    pub ig: &'a Value,
    /// The raw sushi-config.yaml text (consumed verbatim: SiteConfig + menu).
    pub sushi_config_yaml: &'a str,
    /// Project root used to bind every captured asset to its exact source path.
    pub project_root: PathBuf,
    /// input/pagecontent directory.
    pub pagecontent_dir: PathBuf,
    /// input/images directory.
    pub image_dir: PathBuf,
    /// Directories searched for referenced liquid includes (project.liquidAssetDirs).
    pub liquid_asset_dirs: Vec<PathBuf>,
    /// The file source S6 reads through (disk or in-memory VFS).
    pub files: &'a dyn FileSource,
}

/// Renderer-neutral S6 result. Compatibility database rows are projected from
/// the same captured values only after this value is complete.
pub struct PreparedAugmentation {
    pub pages: Vec<PageNode>,
    pub menu: Vec<MenuNode>,
    pub sushi_config: Value,
    pub assets: Vec<PreparedAsset>,
}

struct AugmentationRows {
    pages: Vec<PageRow>,
    menu: Vec<MenuRow>,
    site_config: Vec<SiteConfigRow>,
    assets: Vec<AssetRow>,
}

fn prepared_source_path(project_root: &Path, path: &Path) -> Result<PreparedPath> {
    let rel = path.strip_prefix(project_root).map_err(|_| {
        anyhow::anyhow!(
            "prepared input {} is outside project root {}",
            path.display(),
            project_root.display()
        )
    })?;
    PreparedPath::parse(rel.to_string_lossy().replace('\\', "/"))
        .map_err(|error| anyhow::anyhow!(error))
}

/// ingest.ts:71 — liquidAssetNames: `{% include X %}` / `{% lang-fragment X %}`.
fn liquid_asset_names(body: &str) -> Vec<String> {
    // Port of /{%-?\s*(?:include|lang-fragment)\s+("[^"]+"|'[^']+'|[^\s%]+)[^%]*-?%}/g
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(rel) = body[i..].find("{%") {
        let start = i + rel + 2;
        let Some(close_rel) = body[start..].find("%}") else {
            break;
        };
        let close = start + close_rel;
        let inner = &body[start..close];
        i = close + 2;
        // strip optional leading '-'
        let inner = inner.strip_prefix('-').unwrap_or(inner);
        let inner_trimmed = inner.trim_start();
        for tag in ["include", "lang-fragment"] {
            if let Some(rest) = inner_trimmed.strip_prefix(tag) {
                if rest.starts_with(char::is_whitespace) {
                    let rest = rest.trim_start();
                    // capture ("..."|'...'|[^\s%]+)
                    let name = if let Some(stripped) = rest.strip_prefix('"') {
                        stripped.split('"').next().unwrap_or("").to_string()
                    } else if let Some(stripped) = rest.strip_prefix('\'') {
                        stripped.split('\'').next().unwrap_or("").to_string()
                    } else {
                        rest.split(|c: char| c.is_whitespace() || c == '%')
                            .next()
                            .unwrap_or("")
                            .to_string()
                    };
                    if !name.is_empty() {
                        out.push(name);
                    }
                    break;
                }
            }
        }
    }
    out
}

/// ingest.ts:122 — safeAssetName. Rejects path traversal / absolute / empty.
fn safe_asset_name(name: &str) -> Result<String> {
    let normalized = name.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').collect();
    if normalized.is_empty()
        || normalized.starts_with('/')
        || parts.iter().any(|p| p.is_empty() || *p == "..")
    {
        bail!("Unsafe asset name: {name}");
    }
    Ok(normalized)
}

/// ingest.ts:130 — safePathUnder: resolve relName under root, reject escapes.
fn safe_path_under(root: &Path, rel_name: &str) -> Result<PathBuf> {
    let safe = safe_asset_name(rel_name)?;
    let candidate = normalize_path(&root.join(&safe));
    if !candidate.starts_with(root) {
        bail!("Asset path escapes {}: {rel_name}", root.display());
    }
    Ok(candidate)
}

fn normalize_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// ingest.ts:137 — mimeOf by extension.
fn mime_of(f: &str) -> &'static str {
    let lower = f.to_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".xhtml") || lower.ends_with(".html") {
        "text/html"
    } else if lower.ends_with(".md") {
        "text/markdown"
    } else if lower.ends_with(".txt") {
        "text/plain"
    } else {
        "application/octet-stream"
    }
}

/// ingest.ts:144 — textLikeMime.
fn text_like_mime(mime: &str) -> bool {
    mime.starts_with("text/")
        || mime == "image/svg+xml"
        || mime == "application/xml"
        || mime == "application/xhtml+xml"
}

/// Walk the IG definition.page tree building Pages rows, gathering include names.
struct CapturedPage {
    slug: String,
    name_url: String,
    title: String,
    generation: String,
    depth: i64,
    body: Option<String>,
}

fn walk_pages(
    node: &Value,
    depth: i64,
    pagecontent_dir: &Path,
    files: &dyn FileSource,
    page_include_names: &mut BTreeSet<String>,
    pages: &mut Vec<CapturedPage>,
) -> Result<()> {
    let nodes: Vec<&Value> = match node {
        Value::Array(a) => a.iter().collect(),
        Value::Null => return Ok(()),
        other => vec![other],
    };
    for p in nodes {
        let name_url = p
            .get("nameUrl")
            .and_then(Value::as_str)
            .or_else(|| p.get("name").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        let slug = name_url
            .strip_suffix(".html")
            .unwrap_or(&name_url)
            .to_string();
        if !slug.is_empty() && slug != "toc" {
            let md_path = pagecontent_dir.join(format!("{slug}.md"));
            let xml_path = pagecontent_dir.join(format!("{slug}.xml"));
            let read_text = |p: &Path| -> Result<Option<String>> {
                match files.read(p) {
                    Some(bytes) => Ok(Some(String::from_utf8(bytes)?)),
                    None => Ok(None),
                }
            };
            let body: Option<String> = if let Some(t) = read_text(&md_path)? {
                Some(t)
            } else if let Some(t) = read_text(&xml_path)? {
                Some(t)
            } else {
                // fail loud on missing page file (§2b).
                bail!(
                    "Missing page file for slug '{slug}': expected {} or {}",
                    md_path.display(),
                    xml_path.display()
                );
            };
            if let Some(b) = &body {
                for name in liquid_asset_names(b) {
                    page_include_names.insert(name);
                }
            }
            // Prefer the page's own first H1 (ingest.ts:89).
            let title = first_h1(body.as_deref())
                .or_else(|| p.get("title").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| slug.clone());
            let title = title.trim().to_string();
            let generation = p
                .get("generation")
                .and_then(Value::as_str)
                .unwrap_or("markdown")
                .to_string();
            pages.push(CapturedPage {
                slug: slug.clone(),
                name_url,
                title,
                generation,
                depth,
                body,
            });
        }
        if let Some(children) = p.get("page") {
            walk_pages(
                children,
                depth + 1,
                pagecontent_dir,
                files,
                page_include_names,
                pages,
            )?;
        }
    }
    Ok(())
}

/// ingest.ts:90 — body.match(/^#\s+(.+?)\s*$/m).
fn first_h1(body: Option<&str>) -> Option<String> {
    let body = body?;
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("# ") {
            let t = rest.trim_end();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

fn prepared_page_tree(rows: &[CapturedPage]) -> Result<Vec<PageNode>> {
    fn level(rows: &[CapturedPage], index: &mut usize, depth: i64) -> Result<Vec<PageNode>> {
        let mut result = Vec::new();
        while let Some(row) = rows.get(*index) {
            if row.depth < depth {
                break;
            }
            if row.depth > depth {
                bail!(
                    "page {} jumps from prepared depth {} to {}",
                    row.name_url,
                    depth.saturating_sub(1),
                    row.depth
                );
            }
            *index += 1;
            let child_depth = depth
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("page tree depth overflow"))?;
            let children = level(rows, index, child_depth)?;
            result.push(PageNode {
                name_url: row.name_url.clone(),
                title: row.title.clone(),
                generation: row.generation.clone(),
                body: row.body.clone(),
                children,
            });
        }
        Ok(result)
    }

    let Some(base_depth) = rows.first().map(|row| row.depth) else {
        return Ok(Vec::new());
    };
    if base_depth < 0 {
        bail!("page tree starts at negative depth {base_depth}");
    }
    let mut index = 0;
    let result = level(rows, &mut index, base_depth)?;
    if index != rows.len() {
        bail!("page tree has an invalid depth transition");
    }
    Ok(result)
}

/// ingest.ts:108 — addMenuItems (recursive; groups vs links).
fn add_menu_items(
    node: &serde_yaml::Mapping,
    parent_id: Option<i64>,
    depth: i64,
    path: &[String],
    mid: &mut i64,
    mord: &mut i64,
    rows: &mut Vec<MenuRow>,
) {
    for (k, val) in node {
        let Some(label) = k.as_str() else { continue };
        *mid += 1;
        let id = *mid;
        let href = val.as_str().map(str::to_string);
        let mut item_path = path.to_vec();
        item_path.push(label.to_string());
        let kind = if href.is_some() { "link" } else { "group" };
        rows.push(MenuRow {
            id,
            parent_id,
            ord: *mord,
            depth,
            path: item_path.join("/"),
            label: label.to_string(),
            href,
            kind: kind.to_string(),
        });
        *mord += 1;
        if let Some(map) = val.as_mapping() {
            add_menu_items(map, Some(id), depth + 1, &item_path, mid, mord, rows);
        }
    }
}

fn prepared_menu_items(node: &serde_yaml::Mapping) -> Vec<MenuNode> {
    node.iter()
        .filter_map(|(key, value)| {
            let label = key.as_str()?.to_string();
            Some(MenuNode {
                label,
                href: value.as_str().map(str::to_string),
                items: value
                    .as_mapping()
                    .map(prepared_menu_items)
                    .unwrap_or_default(),
            })
        })
        .collect()
}

struct CapturedAsset {
    name: String,
    mime: String,
    content: Vec<u8>,
    source: PreparedPath,
}

/// Recursively ingest images (ingest.ts:153 ingestImageDir). Sourced through the
/// `FileSource` (disk or in-memory); `list_recursive` yields sorted relative
/// paths, matching the prior sorted `read_dir` walk order.
fn ingest_image_dir(
    root: &Path,
    project_root: &Path,
    files: &dyn FileSource,
    assets: &mut Vec<CapturedAsset>,
) -> Result<usize> {
    let mut count = 0;
    for rel in files.list_recursive(root) {
        let p = safe_path_under(root, &rel)?;
        let Some(content) = files.read(&p) else {
            continue;
        };
        assets.push(CapturedAsset {
            name: safe_asset_name(&rel)?,
            mime: mime_of(&rel).to_string(),
            content,
            source: prepared_source_path(project_root, &p)?,
        });
        count += 1;
    }
    Ok(count)
}

/// Run S6 once, returning renderer-neutral semantics while projecting the same
/// captured values into compatibility rows.
fn capture_augmentation(
    input: &AugmentInputs,
    with_rows: bool,
) -> Result<(PreparedAugmentation, AugmentationRows)> {
    // ---- Pages ----
    let mut page_include_names: BTreeSet<String> = BTreeSet::new();
    let mut pages = Vec::new();
    if let Some(page) = input.ig.pointer("/definition/page") {
        walk_pages(
            page,
            0,
            &input.pagecontent_dir,
            input.files,
            &mut page_include_names,
            &mut pages,
        )?;
    }
    let prepared_pages = prepared_page_tree(&pages)?;
    let page_rows = if with_rows {
        pages
            .iter()
            .enumerate()
            .map(|(ord, page)| PageRow {
                slug: page.slug.clone(),
                name_url: page.name_url.clone(),
                title: page.title.clone(),
                generation: page.generation.clone(),
                ord: ord as i64,
                depth: page.depth,
                body: page.body.clone(),
            })
            .collect()
    } else {
        Vec::new()
    };

    // ---- SiteConfig + Menu (from raw sushi-config.yaml) ----
    let cfg: serde_yaml::Value = serde_yaml::from_str(input.sushi_config_yaml)?;
    // SiteConfig: verbatim yaml -> json, pretty (JSON.stringify(cfg, null, 2)).
    let cfg_json: Value = serde_yaml::from_value(cfg.clone())?;
    let site_config = if with_rows {
        vec![SiteConfigRow {
            name: "sushi-config".to_string(),
            json: serde_json::to_string_pretty(&cfg_json)?,
        }]
    } else {
        Vec::new()
    };
    let mut menu = Vec::new();
    let mut mid = 0i64;
    let mut mord = 0i64;
    if with_rows {
        if let Some(menu_map) = cfg.get("menu").and_then(|m| m.as_mapping()) {
            add_menu_items(menu_map, None, 0, &[], &mut mid, &mut mord, &mut menu);
        }
    }
    let prepared_menu = cfg
        .get("menu")
        .and_then(|value| value.as_mapping())
        .map(prepared_menu_items)
        .unwrap_or_default();

    // ---- Assets: referenced includes (nested), then images ----
    let mut assets: Vec<CapturedAsset> = Vec::new();
    // BTreeSet iteration is sorted+deterministic; we extend the worklist as we
    // discover nested includes (ingest.ts adds to the same set mid-loop).
    let mut pending: Vec<String> = page_include_names.iter().cloned().collect();
    let mut seen_includes: BTreeSet<String> = page_include_names.clone();
    while let Some(include_name) = pending.pop() {
        let safe_name = safe_asset_name(&include_name)?;
        let mut found = false;
        for dir in &input.liquid_asset_dirs {
            let p = safe_path_under(dir, &safe_name)?;
            if let Some(content) = input.files.read(&p) {
                let mime = mime_of(&safe_name);
                if text_like_mime(mime) {
                    if let Ok(text) = std::str::from_utf8(&content) {
                        for nested in liquid_asset_names(text) {
                            if seen_includes.insert(nested.clone()) {
                                pending.push(nested);
                            }
                        }
                    }
                }
                assets.push(CapturedAsset {
                    name: safe_name.clone(),
                    mime: mime.to_string(),
                    content,
                    source: prepared_source_path(&input.project_root, &p)?,
                });
                found = true;
                break;
            }
        }
        if !found {
            // PlantUML OUT OF SCOPE (§2b): a referenced .svg with no committed
            // source fails loud — never silently emit a page missing its diagram.
            if safe_name.to_lowercase().ends_with(".svg") {
                bail!(
                    "Referenced SVG include '{include_name}' not found under any include dir. \
                     PlantUML is out of scope; pre-render and commit the SVG under an include/image dir."
                );
            }
            // Non-SVG includes: mirror ingest.ts, which ingests only includes it
            // finds and silently omits the rest (Publisher-generated `_includes`
            // like dependency-table.xhtml may be absent from a source-only build;
            // the liquid renderer tolerates a missing include). This keeps row
            // parity with the TS augmentation oracle. Missing pages still fail
            // loud (walk_pages); only missing non-SVG *includes* are tolerated.
        }
    }
    ingest_image_dir(
        &input.image_dir,
        &input.project_root,
        input.files,
        &mut assets,
    )?;

    // De-dup by name keeping last-writer (INSERT OR REPLACE semantics), stable.
    let mut order = Vec::new();
    let mut by_name = std::collections::HashMap::new();
    for a in assets {
        if !by_name.contains_key(&a.name) {
            order.push(a.name.clone());
        }
        by_name.insert(a.name.clone(), a);
    }
    let assets: Vec<CapturedAsset> = order
        .into_iter()
        .filter_map(|name| by_name.remove(&name))
        .collect();
    let prepared_assets = assets
        .iter()
        .map(|asset| PreparedAsset {
            path: PreparedPath::parse(asset.name.clone()).expect("safe asset is a PreparedPath"),
            mime: asset.mime.clone(),
            content: asset.content.clone(),
            source_reads: BTreeSet::from([asset.source.clone()]),
        })
        .collect();
    let asset_rows = if with_rows {
        assets
            .into_iter()
            .map(|asset| AssetRow {
                name: asset.name,
                mime: asset.mime,
                content: asset.content,
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok((
        PreparedAugmentation {
            pages: prepared_pages,
            menu: prepared_menu,
            sushi_config: cfg_json,
            assets: prepared_assets,
        },
        AugmentationRows {
            pages: page_rows,
            menu,
            site_config,
            assets: asset_rows,
        },
    ))
}

/// Prepare S6 semantics without constructing a SiteDb.
pub fn prepare(input: &AugmentInputs) -> Result<PreparedAugmentation> {
    capture_augmentation(input, false).map(|(prepared, _)| prepared)
}

pub fn prepare_and_augment(db: &mut SiteDb, input: &AugmentInputs) -> Result<PreparedAugmentation> {
    let (prepared, rows) = capture_augmentation(input, true)?;
    db.pages = rows.pages;
    db.menu = rows.menu;
    db.site_config = rows.site_config;
    db.assets = rows.assets;
    Ok(prepared)
}

/// Compatibility-only convenience for callers that need relational rows.
pub fn augment(db: &mut SiteDb, input: &AugmentInputs) -> Result<()> {
    prepare_and_augment(db, input).map(|_| ())
}
