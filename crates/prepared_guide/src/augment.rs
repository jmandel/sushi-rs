//! Renderer-neutral authored-guide augmentation: builds the
//! page, menu, configuration, and asset values from the IG page tree,
//! sushi-config `menu:`, images, and referenced includes. An include reference
//! is not itself an authored asset: template preprocessing may generate its
//! final bytes (for example, an `input/images-source/*.plantuml` source becoming
//! a temporary `.svg`). Only includes actually found under a declared authored
//! root are captured here. Missing authored Markdown page files still fail
//! loud; generated HTML navigation nodes are retained with no authored body.
//! This crate does not execute PlantUML: without a separate preprocessing stage
//! that figure is omitted, but unsupported figure fidelity is not grounds to
//! reject the renderer-neutral guide.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Result};
use serde_json::Value;

use crate::{
    AugmentInputs, FileSource, MenuNode, PageNode, PreparedAsset, PreparedAugmentation,
    PreparedPath,
};

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
            let generation = p
                .get("generation")
                .and_then(Value::as_str)
                .unwrap_or("markdown")
                .to_string();
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
            } else if generation != "markdown" {
                // Publisher-generated structural pages such as artifacts.html
                // legitimately appear in definition.page with generation=html
                // and have no authored pagecontent file. Preserve the declared
                // navigation node; its final body is generator-owned.
                None
            } else {
                // A Markdown page claims an authored source. Missing it is a
                // real incomplete project rather than a generated page.
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
            pages.push(CapturedPage {
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

/// Run S6 once and return renderer-neutral semantics. Compatibility rows are
/// projected later from the completed PreparedGuide.
fn capture_augmentation(input: &AugmentInputs) -> Result<PreparedAugmentation> {
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

    // ---- SiteConfig + Menu (from raw sushi-config.yaml) ----
    let cfg: serde_yaml::Value = serde_yaml::from_str(input.sushi_config_yaml)?;
    // SiteConfig: verbatim yaml -> json, pretty (JSON.stringify(cfg, null, 2)).
    let cfg_json: Value = serde_yaml::from_value(cfg.clone())?;
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
            // Mirror ingest.ts: capture only an include whose bytes exist under
            // an authored include root. A safe unresolved name is not proof of
            // a missing authored file. Publisher/template preprocessing may own
            // it (mCODE, for example, references an SVG generated from a
            // PlantUML source), or it may name a Publisher-generated include
            // such as dependency-table.xhtml. PreparedGuide is renderer-neutral
            // and must not invent or require that downstream output here.
            //
            // Path safety is intentionally checked before lookup, so this does
            // not turn unsafe names into tolerated misses. Missing authored
            // Markdown sources also remain hard errors in `walk_pages`.
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
    Ok(PreparedAugmentation {
        pages: prepared_pages,
        menu: prepared_menu,
        sushi_config: cfg_json,
        assets: prepared_assets,
    })
}

/// Prepare authored pages, menus, configuration, and assets.
pub fn prepare(input: &AugmentInputs) -> Result<PreparedAugmentation> {
    capture_augmentation(input)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::MemFiles;

    fn inputs<'a>(ig: &'a Value, files: &'a MemFiles) -> AugmentInputs<'a> {
        AugmentInputs {
            ig,
            sushi_config_yaml: "id: test.ig\nfhirVersion: 4.0.1\n",
            project_root: PathBuf::from("/ig"),
            pagecontent_dir: PathBuf::from("/ig/input/pagecontent"),
            image_dir: PathBuf::from("/ig/input/images"),
            liquid_asset_dirs: vec![PathBuf::from("/ig/input/includes")],
            files,
        }
    }

    #[test]
    fn generated_html_navigation_page_needs_no_authored_body() {
        let ig = serde_json::json!({
            "definition": { "page": {
                "nameUrl": "toc.html",
                "title": "Table of Contents",
                "generation": "html",
                "page": [{
                    "nameUrl": "artifacts.html",
                    "title": "Artifacts Summary",
                    "generation": "html"
                }]
            }}
        });
        let files = MemFiles::new(BTreeMap::new());
        let prepared = prepare(&inputs(&ig, &files)).unwrap();
        assert_eq!(prepared.pages.len(), 1);
        assert_eq!(prepared.pages[0].name_url, "artifacts.html");
        assert_eq!(prepared.pages[0].generation, "html");
        assert_eq!(prepared.pages[0].title, "Artifacts Summary");
        assert!(prepared.pages[0].body.is_none());
    }

    #[test]
    fn missing_authored_markdown_page_still_fails_loudly() {
        let ig = serde_json::json!({
            "definition": { "page": {
                "nameUrl": "toc.html",
                "generation": "html",
                "page": [{
                    "nameUrl": "implementation.html",
                    "title": "Implementation",
                    "generation": "markdown"
                }]
            }}
        });
        let files = MemFiles::new(BTreeMap::new());
        let error = prepare(&inputs(&ig, &files))
            .err()
            .expect("missing Markdown source must fail");
        assert!(error
            .to_string()
            .contains("Missing page file for slug 'implementation'"));
    }

    #[test]
    fn generated_svg_reference_is_not_misclassified_as_an_authored_asset() {
        let ig = serde_json::json!({
            "definition": { "page": {
                "nameUrl": "toc.html",
                "generation": "html",
                "page": [{
                    "nameUrl": "conformance-patients.html",
                    "title": "Conformance: Patients",
                    "generation": "markdown"
                }]
            }}
        });
        let files = MemFiles::new(BTreeMap::from([
            (
                PathBuf::from("/ig/input/pagecontent/conformance-patients.md"),
                br#"# Conformance: Patients

<div>{% include patients-with-cancer-condition.svg %}</div>
"#
                .to_vec(),
            ),
            // Real mCODE owns PlantUML source with this shape. The stock
            // Publisher's preprocessing generates the referenced SVG; it is
            // neither an authored include nor an image asset for PreparedGuide
            // to invent.
            (
                PathBuf::from("/ig/input/images-source/patients-with-cancer-condition.plantuml"),
                b"@startuml\nPatient --> Condition\n@enduml\n".to_vec(),
            ),
        ]));

        let prepared = prepare(&inputs(&ig, &files))
            .expect("a safe generator-owned SVG reference must not reject the guide");

        assert_eq!(prepared.pages.len(), 1);
        assert!(prepared.assets.is_empty());
    }

    #[test]
    fn unresolved_include_names_still_enforce_path_safety() {
        let ig = serde_json::json!({
            "definition": { "page": {
                "nameUrl": "toc.html",
                "generation": "html",
                "page": [{
                    "nameUrl": "unsafe.html",
                    "generation": "markdown"
                }]
            }}
        });
        let files = MemFiles::new(BTreeMap::from([(
            PathBuf::from("/ig/input/pagecontent/unsafe.md"),
            b"{% include ../escape.svg %}".to_vec(),
        )]));

        let error = prepare(&inputs(&ig, &files))
            .err()
            .expect("unsafe include name must fail before lookup");
        assert!(error
            .to_string()
            .contains("Unsafe asset name: ../escape.svg"));
    }
}
