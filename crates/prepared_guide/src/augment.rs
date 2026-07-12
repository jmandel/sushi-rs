//! Renderer-neutral authored-guide augmentation: builds page, menu, and
//! configuration semantics and captures the complete Publisher-facing authored
//! inputs. Typed roles distinguish page/resource prose, `_data`, declared
//! include roots, public images, and image-generator sources. An include
//! reference is not itself proof of an authored file: template preprocessing
//! may generate its final bytes (for example, an
//! `input/images-source/*.plantuml` source becoming a temporary `.svg`). Every
//! file that does exist beneath a declared root is captured whether referenced
//! or not. Missing authored Markdown page files still fail loud; generated HTML
//! navigation nodes are retained with no authored body or source. This crate
//! captures but does not execute PlantUML.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Result};
use serde_json::Value;

use crate::{
    AugmentInputs, AuthoredFile, AuthoredFileRole, FileSource, MenuNode, PageNode,
    PreparedAugmentation, PreparedPath,
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
    } else if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".xml") {
        "application/xml"
    } else if lower.ends_with(".css") {
        "text/css"
    } else if lower.ends_with(".js") {
        "text/javascript"
    } else if lower.ends_with(".liquid") {
        "text/plain"
    } else if lower.ends_with(".txt") {
        "text/plain"
    } else {
        "application/octet-stream"
    }
}

/// Walk the IG definition.page tree building Pages rows, gathering include names.
struct CapturedPage {
    name_url: String,
    title: String,
    generation: String,
    depth: i64,
    body: Option<String>,
    source: Option<PreparedPath>,
}

fn walk_pages(
    node: &Value,
    depth: i64,
    pagecontent_dirs: &[PathBuf],
    project_root: &Path,
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
            let read_text = |p: &Path| -> Result<Option<String>> {
                match files.read(p) {
                    Some(bytes) => Ok(Some(String::from_utf8(bytes)?)),
                    None => Ok(None),
                }
            };
            let candidates = pagecontent_dirs
                .iter()
                .flat_map(|root| {
                    [
                        root.join(format!("{slug}.md")),
                        root.join(format!("{slug}.xml")),
                    ]
                })
                .filter(|path| files.is_file(path))
                .collect::<Vec<_>>();
            if candidates.len() > 1 {
                bail!(
                    "page slug '{slug}' has multiple authored sources: {}",
                    candidates
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            let (body, source): (Option<String>, Option<PreparedPath>) = if let Some(path) =
                candidates.first()
            {
                let body = read_text(path)?.ok_or_else(|| {
                    anyhow::anyhow!("listed authored page {} is unreadable", path.display())
                })?;
                (Some(body), Some(prepared_source_path(project_root, path)?))
            } else if generation != "markdown" {
                // Publisher-generated structural pages such as artifacts.html
                // legitimately appear in definition.page with generation=html
                // and have no authored pagecontent file. Preserve the declared
                // navigation node; its final body is generator-owned.
                (None, None)
            } else {
                // A Markdown page claims an authored source. Missing it is a
                // real incomplete project rather than a generated page.
                bail!("Missing page file for slug '{slug}' under any declared page-content root");
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
                source,
            });
        }
        if let Some(children) = p.get("page") {
            walk_pages(
                children,
                depth + 1,
                pagecontent_dirs,
                project_root,
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
                source: row.source.clone(),
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

struct CapturedFile {
    role: AuthoredFileRole,
    name: String,
    mime: String,
    content: Vec<u8>,
    source: PreparedPath,
}

/// Capture every file beneath a declared Publisher input root. The logical
/// path is role-relative; the exact project-relative source is retained
/// separately. Unsafe listings fail before a read and are never normalized
/// into a different file.
fn capture_root(
    role: AuthoredFileRole,
    root: &Path,
    project_root: &Path,
    files: &dyn FileSource,
    authored_files: &mut Vec<CapturedFile>,
) -> Result<usize> {
    let root = normalize_path(root);
    let project_root = normalize_path(project_root);
    if !root.starts_with(&project_root) {
        bail!(
            "prepared input root {} is outside project root {}",
            root.display(),
            project_root.display()
        );
    }
    let mut count = 0;
    for rel in files.list_recursive(&root) {
        let p = safe_path_under(&root, &rel)?;
        let content = files.read(&p).ok_or_else(|| {
            anyhow::anyhow!("listed authored input {} is unreadable", p.display())
        })?;
        authored_files.push(CapturedFile {
            role: role.clone(),
            name: safe_asset_name(&rel)?,
            mime: mime_of(&rel).to_string(),
            content,
            source: prepared_source_path(&project_root, &p)?,
        });
        count += 1;
    }
    Ok(count)
}

/// Run authored augmentation once and return renderer-neutral semantics.
fn capture_augmentation(input: &AugmentInputs) -> Result<PreparedAugmentation> {
    // ---- Pages ----
    let mut page_include_names: BTreeSet<String> = BTreeSet::new();
    let mut pages = Vec::new();
    let pagecontent_dirs = vec![
        input.pagecontent_dir.clone(),
        input.project_root.join("input/pages"),
        input.project_root.join("input/resource-docs"),
    ];
    if let Some(page) = input.ig.pointer("/definition/page") {
        walk_pages(
            page,
            0,
            &pagecontent_dirs,
            &input.project_root,
            input.files,
            &mut page_include_names,
            &mut pages,
        )?;
    }
    // A reference may be generator-owned and therefore absent from authored
    // include roots, but it is never allowed to smuggle an unsafe path into a
    // later renderer.
    for name in &page_include_names {
        safe_asset_name(name)?;
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

    // ---- Complete authored Publisher inputs ----
    let mut authored_files: Vec<CapturedFile> = Vec::new();
    capture_root(
        AuthoredFileRole::PageContent,
        &input.pagecontent_dir,
        &input.project_root,
        input.files,
        &mut authored_files,
    )?;
    capture_root(
        AuthoredFileRole::PageContent,
        &input.project_root.join("input/pages"),
        &input.project_root,
        input.files,
        &mut authored_files,
    )?;
    capture_root(
        AuthoredFileRole::ResourceContent,
        &input.project_root.join("input/intro-notes"),
        &input.project_root,
        input.files,
        &mut authored_files,
    )?;
    capture_root(
        AuthoredFileRole::ResourceContent,
        &input.project_root.join("input/resource-docs"),
        &input.project_root,
        input.files,
        &mut authored_files,
    )?;
    capture_root(
        AuthoredFileRole::Data,
        &input.project_root.join("input/data"),
        &input.project_root,
        input.files,
        &mut authored_files,
    )?;
    for dir in &input.liquid_asset_dirs {
        capture_root(
            AuthoredFileRole::Include,
            dir,
            &input.project_root,
            input.files,
            &mut authored_files,
        )?;
    }
    capture_root(
        AuthoredFileRole::Image,
        &input.image_dir,
        &input.project_root,
        input.files,
        &mut authored_files,
    )?;
    capture_root(
        AuthoredFileRole::ImageSource,
        &input.project_root.join("input/images-source"),
        &input.project_root,
        input.files,
        &mut authored_files,
    )?;

    // Roles are independent namespaces. A repeated declaration of the exact
    // same source is harmless; two sources claiming one role-relative path are
    // ambiguous and fail rather than depending on traversal order.
    let mut by_name: BTreeMap<(AuthoredFileRole, String), CapturedFile> = BTreeMap::new();
    for a in authored_files {
        let key = (a.role.clone(), a.name.clone());
        if let Some(existing) = by_name.get(&key) {
            if existing.source != a.source
                || existing.content != a.content
                || existing.mime != a.mime
            {
                bail!(
                    "authored {:?} path '{}' is declared by both {} and {}",
                    a.role,
                    a.name,
                    existing.source,
                    a.source
                );
            }
            continue;
        }
        by_name.insert(key, a);
    }
    let authored_files = by_name
        .into_values()
        .map(|asset| AuthoredFile {
            role: asset.role,
            path: PreparedPath::parse(asset.name).expect("safe asset is a PreparedPath"),
            mime: asset.mime,
            content: asset.content,
            source_reads: BTreeSet::from([asset.source]),
        })
        .collect();
    Ok(PreparedAugmentation {
        pages: prepared_pages,
        menu: prepared_menu,
        sushi_config: cfg_json,
        authored_files,
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
        assert!(prepared.pages[0].source.is_none());
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
        assert_eq!(
            prepared.pages[0].source.as_ref().map(PreparedPath::as_str),
            Some("input/pagecontent/conformance-patients.md")
        );
        assert!(prepared.authored_files.iter().all(|asset| {
            asset.role != AuthoredFileRole::Image && asset.role != AuthoredFileRole::Include
        }));
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

    #[test]
    fn captures_complete_typed_publisher_inputs_with_exact_sources() {
        let ig = serde_json::json!({
            "definition": { "page": {
                "nameUrl": "toc.html",
                "generation": "html",
                "page": [
                    {
                        "nameUrl": "home.html",
                        "generation": "markdown"
                    },
                    {
                        "nameUrl": "resource-guide.html",
                        "generation": "markdown"
                    }
                ]
            }}
        });
        let files = MemFiles::new(BTreeMap::from([
            (
                PathBuf::from("/ig/input/pagecontent/home.md"),
                b"# Home".to_vec(),
            ),
            (
                PathBuf::from("/ig/input/pagecontent/unlisted.md"),
                b"unused page".to_vec(),
            ),
            (
                PathBuf::from("/ig/input/pages/secondary.md"),
                b"secondary page root".to_vec(),
            ),
            (
                PathBuf::from("/ig/input/intro-notes/StructureDefinition-demo-intro.md"),
                b"resource introduction".to_vec(),
            ),
            (
                PathBuf::from("/ig/input/resource-docs/resource-guide.md"),
                b"# Resource guide".to_vec(),
            ),
            (
                PathBuf::from("/ig/input/data/nested/site.json"),
                b"{}".to_vec(),
            ),
            (
                PathBuf::from("/ig/input/includes/unreferenced.liquid"),
                b"complete include root".to_vec(),
            ),
            (
                PathBuf::from("/ig/input/images/nested/logo.svg"),
                b"<svg/>".to_vec(),
            ),
            (
                PathBuf::from("/ig/input/images-source/diagram.plantuml"),
                b"@startuml\n@enduml".to_vec(),
            ),
        ]));

        let prepared = prepare(&inputs(&ig, &files)).unwrap();
        assert_eq!(prepared.pages[0].body.as_deref(), Some("# Home"));
        assert_eq!(
            prepared.pages[0].source.as_ref().map(PreparedPath::as_str),
            Some("input/pagecontent/home.md")
        );
        assert_eq!(
            prepared.pages[1].source.as_ref().map(PreparedPath::as_str),
            Some("input/resource-docs/resource-guide.md")
        );

        let captured = prepared
            .authored_files
            .iter()
            .map(|asset| {
                (
                    asset.role.clone(),
                    asset.path.as_str(),
                    asset
                        .source_reads
                        .iter()
                        .next()
                        .expect("one exact source")
                        .as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            captured,
            vec![
                (
                    AuthoredFileRole::PageContent,
                    "home.md",
                    "input/pagecontent/home.md"
                ),
                (
                    AuthoredFileRole::PageContent,
                    "secondary.md",
                    "input/pages/secondary.md"
                ),
                (
                    AuthoredFileRole::PageContent,
                    "unlisted.md",
                    "input/pagecontent/unlisted.md"
                ),
                (
                    AuthoredFileRole::ResourceContent,
                    "StructureDefinition-demo-intro.md",
                    "input/intro-notes/StructureDefinition-demo-intro.md"
                ),
                (
                    AuthoredFileRole::ResourceContent,
                    "resource-guide.md",
                    "input/resource-docs/resource-guide.md"
                ),
                (
                    AuthoredFileRole::Data,
                    "nested/site.json",
                    "input/data/nested/site.json"
                ),
                (
                    AuthoredFileRole::Include,
                    "unreferenced.liquid",
                    "input/includes/unreferenced.liquid"
                ),
                (
                    AuthoredFileRole::Image,
                    "nested/logo.svg",
                    "input/images/nested/logo.svg"
                ),
                (
                    AuthoredFileRole::ImageSource,
                    "diagram.plantuml",
                    "input/images-source/diagram.plantuml"
                ),
            ]
        );
    }

    #[test]
    fn conflicting_declared_include_roots_fail_loudly() {
        let ig = serde_json::json!({});
        let files = MemFiles::new(BTreeMap::from([
            (
                PathBuf::from("/ig/input/includes/shared.md"),
                b"first".to_vec(),
            ),
            (
                PathBuf::from("/ig/input/other-includes/shared.md"),
                b"second".to_vec(),
            ),
        ]));
        let mut input = inputs(&ig, &files);
        input.liquid_asset_dirs = vec![
            PathBuf::from("/ig/input/includes"),
            PathBuf::from("/ig/input/other-includes"),
        ];

        let error = prepare(&input).err().expect("colliding roots must fail");
        assert!(error
            .to_string()
            .contains("authored Include path 'shared.md' is declared by both"));
    }

    #[test]
    fn declared_roots_outside_project_are_rejected() {
        let ig = serde_json::json!({});
        let files = MemFiles::new(BTreeMap::from([(
            PathBuf::from("/external/includes/shared.md"),
            b"outside".to_vec(),
        )]));
        let mut input = inputs(&ig, &files);
        input.liquid_asset_dirs = vec![PathBuf::from("/external/includes")];

        let error = prepare(&input)
            .err()
            .expect("out-of-project roots must fail");
        assert!(error.to_string().contains("is outside project root /ig"));
    }
}
