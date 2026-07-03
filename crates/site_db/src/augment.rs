//! S6 — augmentation. A faithful port of `site-gen/ingest.ts`: builds the
//! Pages / Menu / SiteConfig / Assets rows from the IG page tree, sushi-config
//! `menu:`, images, and referenced includes. PlantUML is OUT OF SCOPE (§2b): a
//! referenced `.svg` that is not present under an include/image dir fails loud.
//! Missing page files fail loud too.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Result};
use serde_json::Value;

use crate::model::{AssetRow, MenuRow, PageRow, SiteConfigRow, SiteDb};

/// Inputs for S6 that live in the IG repo (declared input set, §2b).
pub struct AugmentInputs<'a> {
    /// The parsed ImplementationGuide resource (source of definition.page).
    pub ig: &'a Value,
    /// The raw sushi-config.yaml text (consumed verbatim: SiteConfig + menu).
    pub sushi_config_yaml: &'a str,
    /// input/pagecontent directory.
    pub pagecontent_dir: PathBuf,
    /// input/images directory.
    pub image_dir: PathBuf,
    /// Directories searched for referenced liquid includes (project.liquidAssetDirs).
    pub liquid_asset_dirs: Vec<PathBuf>,
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
fn walk_pages(
    node: &Value,
    depth: i64,
    ord: &mut i64,
    pagecontent_dir: &Path,
    page_include_names: &mut BTreeSet<String>,
    pages: &mut Vec<PageRow>,
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
        let slug = name_url.strip_suffix(".html").unwrap_or(&name_url).to_string();
        if !slug.is_empty() && slug != "toc" {
            let md_path = pagecontent_dir.join(format!("{slug}.md"));
            let xml_path = pagecontent_dir.join(format!("{slug}.xml"));
            let body: Option<String> = if md_path.exists() {
                Some(std::fs::read_to_string(&md_path)?)
            } else if xml_path.exists() {
                Some(std::fs::read_to_string(&xml_path)?)
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
            pages.push(PageRow {
                slug: slug.clone(),
                name_url,
                title,
                generation,
                ord: *ord,
                depth,
                body,
            });
            *ord += 1;
        }
        if let Some(children) = p.get("page") {
            walk_pages(
                children,
                depth + 1,
                ord,
                pagecontent_dir,
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

/// Recursively ingest images (ingest.ts:153 ingestImageDir).
fn ingest_image_dir(root: &Path, rel: &str, assets: &mut Vec<AssetRow>) -> Result<usize> {
    if !root.exists() {
        return Ok(0);
    }
    let dir = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    let mut count = 0;
    let mut entries: Vec<_> = std::fs::read_dir(&dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        let next = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let p = safe_path_under(root, &next)?;
        if e.file_type()?.is_dir() {
            count += ingest_image_dir(root, &next, assets)?;
        } else if e.file_type()?.is_file() {
            assets.push(AssetRow {
                name: safe_asset_name(&next)?,
                mime: mime_of(&next).to_string(),
                content: std::fs::read(&p)?,
            });
            count += 1;
        }
    }
    Ok(count)
}

/// Run S6. Populates db.pages/menu/site_config/assets.
pub fn augment(db: &mut SiteDb, input: &AugmentInputs) -> Result<()> {
    // ---- Pages ----
    let mut ord: i64 = 0;
    let mut page_include_names: BTreeSet<String> = BTreeSet::new();
    let mut pages = Vec::new();
    if let Some(page) = input.ig.pointer("/definition/page") {
        walk_pages(
            page,
            0,
            &mut ord,
            &input.pagecontent_dir,
            &mut page_include_names,
            &mut pages,
        )?;
    }
    db.pages = pages;

    // ---- SiteConfig + Menu (from raw sushi-config.yaml) ----
    let cfg: serde_yaml::Value = serde_yaml::from_str(input.sushi_config_yaml)?;
    // SiteConfig: verbatim yaml -> json, pretty (JSON.stringify(cfg, null, 2)).
    let cfg_json: Value = serde_yaml::from_value(cfg.clone())?;
    db.site_config = vec![SiteConfigRow {
        name: "sushi-config".to_string(),
        json: serde_json::to_string_pretty(&cfg_json)?,
    }];
    let mut menu = Vec::new();
    let mut mid = 0i64;
    let mut mord = 0i64;
    if let Some(menu_map) = cfg.get("menu").and_then(|m| m.as_mapping()) {
        add_menu_items(menu_map, None, 0, &[], &mut mid, &mut mord, &mut menu);
    }
    db.menu = menu;

    // ---- Assets: referenced includes (nested), then images ----
    let mut assets: Vec<AssetRow> = Vec::new();
    // BTreeSet iteration is sorted+deterministic; we extend the worklist as we
    // discover nested includes (ingest.ts adds to the same set mid-loop).
    let mut pending: Vec<String> = page_include_names.iter().cloned().collect();
    let mut seen_includes: BTreeSet<String> = page_include_names.clone();
    while let Some(include_name) = pending.pop() {
        let safe_name = safe_asset_name(&include_name)?;
        let mut found = false;
        for dir in &input.liquid_asset_dirs {
            let p = safe_path_under(dir, &safe_name)?;
            if p.is_file() {
                let mime = mime_of(&safe_name);
                let content = std::fs::read(&p)?;
                if text_like_mime(mime) {
                    if let Ok(text) = std::str::from_utf8(&content) {
                        for nested in liquid_asset_names(text) {
                            if seen_includes.insert(nested.clone()) {
                                pending.push(nested);
                            }
                        }
                    }
                }
                assets.push(AssetRow {
                    name: safe_name.clone(),
                    mime: mime.to_string(),
                    content,
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
    ingest_image_dir(&input.image_dir, "", &mut assets)?;

    // De-dup by name keeping last-writer (INSERT OR REPLACE semantics), stable.
    let mut by_name: indexmap_lite::OrderMap = indexmap_lite::OrderMap::new();
    for a in assets {
        by_name.insert(a.name.clone(), a);
    }
    db.assets = by_name.into_values();
    Ok(())
}

/// A tiny order-preserving map so we don't add an indexmap dep just for asset
/// de-dup. INSERT OR REPLACE keeps the LAST value for a name; order is first-seen.
mod indexmap_lite {
    use crate::model::AssetRow;
    pub struct OrderMap {
        order: Vec<String>,
        map: std::collections::HashMap<String, AssetRow>,
    }
    impl OrderMap {
        pub fn new() -> Self {
            Self {
                order: Vec::new(),
                map: std::collections::HashMap::new(),
            }
        }
        pub fn insert(&mut self, key: String, value: AssetRow) {
            if !self.map.contains_key(&key) {
                self.order.push(key.clone());
            }
            self.map.insert(key, value);
        }
        pub fn into_values(mut self) -> Vec<AssetRow> {
            self.order
                .into_iter()
                .filter_map(|k| self.map.remove(&k))
                .collect()
        }
    }
}
