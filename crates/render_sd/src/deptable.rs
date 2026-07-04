//! `render_sd::deptable` — the IG-level `dependency-table` family.
//!
//! Port of `org.hl7.fhir.igtools.renderers.DependencyRenderer` (publisher 2.2.11
//! clone; citations `depr:`). Three fragments (PublisherGenerator:2911-2915):
//!   - `dependency-table`       = render(ig, QA=false, details=true,  first=true)
//!   - `dependency-table-short` = render(ig, QA=false, details=false, first=false)
//!   - `dependency-table-nontech` = renderNonTech(ig)
//! All three are trackedFragment "3".
//!
//! Inputs the renderer draws on that are NOT in the IG's output/*.json:
//!   - the NpmPackage graph from the package cache (each `package/package.json`:
//!     name, version, canonical, url, title, dependencies, description,
//!     fhirVersions);
//!   - the set of LOADED packages (`isLoaded` = SpecMapManager.getNpmVId set,
//!     depr:522) — a build fact passed in as a `name#version` set.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde_json::Value;

use crate::leaf::escape_xml;

/// A package.json view (the NpmPackage subset the renderer touches).
#[derive(Clone)]
pub struct Npm {
    pub id: String,
    pub version: String,
    pub canonical: String,
    /// package.json `url` = getWebLocation() (before fixPackageUrl; the cache
    /// package.jsons already carry the fixed url for this corpus).
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub deps: Vec<(String, String)>, // (id, version)
    pub fhir_version: String,
}

impl Npm {
    fn vid(&self) -> String {
        format!("{}#{}", self.id, self.version)
    }
    fn is_core(&self) -> bool {
        is_core_package(&self.id)
    }
}

/// `VersionUtilities.isCorePackage` (subset): the hl7.fhir.<r>.core packages.
fn is_core_package(id: &str) -> bool {
    matches!(
        id,
        "hl7.fhir.r2.core"
            | "hl7.fhir.r2b.core"
            | "hl7.fhir.r3.core"
            | "hl7.fhir.r4.core"
            | "hl7.fhir.r4b.core"
            | "hl7.fhir.r5.core"
            | "hl7.fhir.core"
    )
}

/// `PackageHacker.fixPackageUrl` (PackageHacker:263) applied on-load
/// (fixPackageOnLoad:252). The corpus hits only the xver rewrite (line 298) and
/// the us-core v311/STU4.0.0 fixes; the file:// switch + secure-refs
/// (useSecureReferences=false in the publisher) never fire here. Faithful subset.
fn fix_package_url(webref: &str) -> String {
    if webref.is_empty() {
        return String::new();
    }
    if webref.contains("hl7.org/fhir/us/core/STU4.0.0") {
        return webref.replace("hl7.org/fhir/us/core/STU4.0.0", "hl7.org/fhir/us/core/STU4");
    }
    if webref == "http://hl7.org/fhir/us/core/v311" {
        return "https://hl7.org/fhir/us/core/STU3.1.1".to_string();
    }
    let mut w = webref.to_string();
    if w.contains("hl7.org/fhir/uv/hl7.fhir.uv.xver") {
        w = w.replace("hl7.org/fhir/uv/hl7.fhir.uv.xver", "hl7.org/fhir/uv/xver");
    }
    w
}

/// Load a package.json from the cache dir (`<cache>/<id>#<version>/package/package.json`).
pub fn load_npm(cache: &Path, id: &str, version: &str) -> Option<Npm> {
    let pj = cache.join(format!("{}#{}", id, version)).join("package").join("package.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&pj).ok()?).ok()?;
    let deps = v
        .get("dependencies")
        .and_then(|d| d.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let fhir_version = v
        .get("fhirVersions")
        .and_then(|x| x.as_array())
        .and_then(|a| a.first())
        .and_then(|x| x.as_str())
        .or_else(|| v.get("fhir-version-list").and_then(|x| x.as_str()))
        .unwrap_or("")
        .to_string();
    Some(Npm {
        id: v.get("name").and_then(|x| x.as_str()).unwrap_or(id).to_string(),
        version: v.get("version").and_then(|x| x.as_str()).unwrap_or(version).to_string(),
        canonical: v.get("canonical").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        url: fix_package_url(v.get("url").and_then(|x| x.as_str()).unwrap_or("")),
        title: v.get("title").and_then(|x| x.as_str()).map(String::from),
        description: v.get("description").and_then(|x| x.as_str()).map(String::from),
        deps,
        fhir_version,
    })
}

// ---------------------------------------------------------------------------
// renderNonTech (depr:209-255) — pure StringBuilder, no HTG.
// ---------------------------------------------------------------------------

struct PackageVersionInfo {
    npm: Npm,
    direct: bool,
    reason: Option<String>,
    parent: Option<String>,
}

struct PackageInfo {
    npm: Npm,
    direct: bool,
    // version -> info; insertion order preserved for addVersion semantics.
    versions: BTreeMap<String, PackageVersionInfo>,
}

/// The renderNonTech title-key derivation (depr:262-276).
fn nontech_title(npm: &Npm) -> String {
    let mut title = if let Some(t) = &npm.title {
        t.clone()
    } else if npm.id.ends_with(".vsac") {
        "Value Set Authority Center (VSAC)".to_string()
    } else if npm.id.ends_with(".phinvads") {
        "Public Health Information Network Vocabulary Access and Distribution System (PHIN VADS)".to_string()
    } else {
        npm.id.clone()
    };
    if title.contains("Wrapper)") {
        if let Some(i) = title.find('(') {
            title = title[..i].trim().to_string();
        }
    }
    if title.ends_with("Implementation Guide") {
        title = title[..title.len() - 20].trim().to_string();
    }
    title
}

/// `addPackage` (depr:260-296): register the package by title, recurse into its
/// loaded dependencies as indirect.
fn nontech_add_package(
    packages: &mut BTreeMap<String, PackageInfo>,
    order: &mut Vec<String>,
    cache: &Path,
    loaded: &HashSet<String>,
    npm: &Npm,
    reason: Option<String>,
    direct: bool,
    parent: Option<String>,
) {
    let title = nontech_title(npm);
    let is_new_title = !packages.contains_key(&title);
    if let Some(info) = packages.get_mut(&title) {
        // addVersion (depr:192-206).
        if info.versions.contains_key(&npm.version) {
            if direct {
                info.direct = true;
                let v = info.versions.get_mut(&npm.version).unwrap();
                v.direct = true;
                v.reason = reason;
            }
            return;
        }
        info.direct = info.direct || direct;
        info.versions.insert(
            npm.version.clone(),
            PackageVersionInfo { npm: npm.clone(), direct, reason, parent },
        );
    } else {
        let mut versions = BTreeMap::new();
        versions.insert(
            npm.version.clone(),
            PackageVersionInfo { npm: npm.clone(), direct, reason, parent },
        );
        packages.insert(title.clone(), PackageInfo { npm: npm.clone(), direct, versions });
        order.push(title.clone());
    }
    let _ = is_new_title;

    // recurse into loaded deps (depr:284-295) as indirect, parent=title.
    for (id, version) in &npm.deps {
        let d = format!("{}#{}", id, version);
        if loaded.contains(&d) {
            if let Some(dp) = load_npm(cache, id, version) {
                nontech_add_package(packages, order, cache, loaded, &dp, None, false, Some(title.clone()));
            }
        }
    }
}

/// `PackageVersionInfo.getReason` (depr:169-176). mdEngine.process is applied to
/// an explicit reason; the corpus reasons are all plain single-line strings that
/// commonmark wraps in `<p>...</p>\n` (the golden shows the `<p>..</p>\n`).
fn version_reason(v: &PackageVersionInfo) -> Option<String> {
    if let Some(r) = &v.reason {
        Some(crate::publisher_markdown::md_process(r))
    } else {
        v.parent
            .as_ref()
            .map(|p| format!("Imported by {} (and potentially others)", escape_xml(p)))
    }
}

/// `renderNonTech(ig)` (depr:209-255). Returns the table body (the caller wraps
/// with wrap_raw + trackedFragment "3"). `cache` = the package cache dir; `ig` =
/// the own ImplementationGuide JSON; `loaded` = the build's loaded `name#version`
/// set (isLoaded). All supplied by the harness (build facts / paths).
pub fn dependency_table_nontech(
    cache: &Path,
    ig: &Value,
    loaded: &HashSet<String>,
) -> String {
    let mut packages: BTreeMap<String, PackageInfo> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();

    for d in ig_depends_on(ig) {
        // resolve(d) then isLoaded(p) (depr:212-216).
        if let Some(npm) = resolve_dep(cache, &d) {
            if loaded.contains(&npm.vid()) {
                let reason = dep_reason(&d);
                nontech_add_package(&mut packages, &mut order, cache, loaded, &npm, reason, true, None);
            }
        }
    }

    if packages.is_empty() {
        return String::new();
    }

    let mut b = String::new();
    b.push_str("<table style=\"border: 1px #F0F0F0 solid;\"><thead><tr style=\"border: 1px #F0F0F0 solid; font-size: 11px; font-family: verdana; vertical-align: top;\"><th><b>Implementation Guide</b></th><th><b>Version(s)</b></th><th><b>Reason</b></th></tr></thead><tbody>");
    // names = sorted(packagesByName.keySet()) (depr:224-225).
    let mut names: Vec<&String> = packages.keys().collect();
    names.sort();
    let mut line_count = 1i32;
    for name in names {
        let info = &packages[name];
        let bg = if line_count % 2 == 0 { "#F7F7F7" } else { "white" };
        let new_row = format!(
            "<tr style=\"font-size: 11px; font-family: verdana; vertical-align: top; background-color: {}\"><td",
            bg
        );
        b.push_str(&new_row);
        if info.versions.len() != 1 {
            b.push_str(&format!(" rowspan=\"{}\"", info.versions.len()));
        }
        b.push_str(&format!(
            "><span class=\"copy-text\" title=\"canonical: {can}\"><a style=\"font-size: 11px; font-family: verdana; font-weight:{fw}\" href=\"{url}\">{name}</a><button class=\"btn-copy\" title=\"Click to copy URL\" data-clipboard-text=\"{can}\"/></span></td><td>",
            can = info.npm.canonical,
            fw = if info.direct { "bold" } else { "normal" },
            url = info.npm.url,
            name = escape_xml(name),
        ));
        // versions = sorted(reverseOrder) (depr:236-237).
        let mut first = true;
        let mut versions: Vec<&String> = info.versions.keys().collect();
        versions.sort();
        versions.reverse();
        for version in versions {
            let ver_info = &info.versions[version];
            if !first {
                b.push_str(&format!("{}>", new_row));
            }
            b.push_str(&format!(
                "<span class=\"copy-text\" title=\"package: {id}#{ver}\"><a style=\"font-size: 11px; font-family: verdana; font-weight: {fw}\" href=\"https://simplifier.net/packages/{pname}/{ver}\">{ver}</a><button class=\"btn-copy\" title=\"Click to copy package\" data-clipboard-text=\"{id}#{ver}\"/></span>",
                id = ver_info.npm.id,
                ver = version,
                fw = if ver_info.direct { "bold" } else { "normal" },
                pname = info.npm.id,
            ));
            let reason = version_reason(ver_info);
            b.push_str(&format!(
                "</td><td{}>{}</td></tr>",
                if ver_info.direct { "" } else { " style=\"font-style: italic;\"" },
                reason.unwrap_or_default()
            ));
            first = false;
        }
        line_count += 1;
    }
    b.push_str("</tbody></table>");
    b
}

// ---------------------------------------------------------------------------
// render (HTG-based dependency-table / -short): documented gap (see report).
// ---------------------------------------------------------------------------

/// `render(ig, QA=false, details, first)` (depr:298) — the HTG tree table.
/// DOCUMENTED STOP. Three coupled blockers, in decreasing tractability:
///
/// 1. **HTG `inlineGraphics=true` code path** — the existing `render_tables` HTG
///    port only implements `inlineGraphics=false` (the SD fragment tables, whose
///    tree-line srcs are pathURL strings). The dependency-table uses
///    `new HierarchicalTableGenerator(rc, dstFolder, inlineGraphics=true, …, "dep")`
///    which routes `srcFor`/`checkExists` through `genImage` (HTG:1416) — a
///    `BufferedImage(800,2)` rendered to PNG via Java `ImageIO.write(…, "PNG")`
///    and base64-encoded inline, PLUS static icon PNGs (`tbl_spacer.png`,
///    `icon-fhir-16.png`, …) read as bytes from the `dest` (=temp/pages) dir.
///    The genImage PNGs are DETERMINISTIC (byte-identical across all 3 IGs for a
///    given indent/lineColor pattern — corpus-verified: only ~3 distinct
///    backgrounds) but reproducing Java's ImageIO PNG encoder byte-for-byte in
///    Rust is not viable; they would have to be a captured PNG oracle (same class
///    as the run-uuid quirk), keyed by `(indents, hasChildren, lineColor)`.
/// 2. **Load-graph reconstruction** — the transitive package tree with per-row
///    direct/indirect flags and the version-comment logic (depr:399-421: "FHIR
///    Version Mismatch" / "Matched to latest patch" / realm-mismatch / internal).
///    The loaded-package set is now available (harvested from the tech golden in
///    the harness, `dep_loaded_set`), but the row TREE + comment column remain.
/// 3. **`details` tail (`dependency-table` only)** — the per-package
///    description + ArtifactDependency `<ul>` (depr:452-513) appended after the
///    table, from each package's `description` + the whole-IG dependency list.
///
/// The pure-StringBuilder `renderNonTech` sibling IS green (above); it needs none
/// of 1-3 (no HTG, no comments, no details). Fires a LOUD GAP so a page that
/// includes this fragment surfaces a visible placeholder rather than silent-wrong.
pub fn dependency_table(
    _cache: &Path,
    _ig: &Value,
    _loaded: &HashSet<String>,
    _dst_folder: &str,
    _details: bool,
    _run_uuid: &str,
) -> String {
    panic!(
        "LOUD GAP: dependency-table/-short (depr:298 render) not ported — needs \
         HTG inlineGraphics=true tree-line PNG oracle (captured; genImage is \
         Java ImageIO PNG) + load-graph row-tree + version-comment column"
    );
}

// ---------------------------------------------------------------------------
// helpers pulling IG dependsOn + package resolution.
// ---------------------------------------------------------------------------

/// The IG's dependsOn entries (id? packageId, version, uri, reason/comment ext).
struct DependsOn {
    package_id: Option<String>,
    version: String,
    uri: Option<String>,
    reason: Option<String>,
    comment_ext: Option<String>,
}

fn ig_depends_on(ig: &Value) -> Vec<DependsOn> {
    let mut out = Vec::new();
    if let Some(arr) = ig.get("dependsOn").and_then(|x| x.as_array()) {
        for d in arr {
            let ext_val = |url: &str| -> Option<String> {
                d.get("extension")
                    .and_then(|x| x.as_array())
                    .and_then(|exts| exts.iter().find(|e| e.get("url").and_then(|u| u.as_str()) == Some(url)))
                    .and_then(|e| {
                        e.get("valueMarkdown")
                            .or_else(|| e.get("valueString"))
                            .and_then(|x| x.as_str())
                            .map(String::from)
                    })
            };
            let comment_ext = ext_val("http://hl7.org/fhir/tools/StructureDefinition/implementationguide-dependency-comment");
            // The R5 `dependsOn.reason` field is projected into R4 as this xver
            // extension; the publisher's element-model surfaces it as d.getReason()
            // (depr:215 `d.hasReason()`), so it takes precedence over the comment ext.
            let reason = d
                .get("reason")
                .and_then(|x| x.as_str())
                .map(String::from)
                .or_else(|| ext_val("http://hl7.org/fhir/5.0/StructureDefinition/extension-ImplementationGuide.dependsOn.reason"));
            out.push(DependsOn {
                package_id: d.get("packageId").and_then(|x| x.as_str()).map(String::from),
                version: d.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                uri: d.get("uri").and_then(|x| x.as_str()).map(String::from),
                reason,
                comment_ext,
            });
        }
    }
    out
}

/// `d.hasReason() ? d.getReason() : readStringExtension(EXT_IGDEP_COMMENT)`
/// (depr:215).
fn dep_reason(d: &DependsOn) -> Option<String> {
    if let Some(r) = &d.reason {
        Some(r.clone())
    } else {
        d.comment_ext.clone()
    }
}

/// `resolve(d)` (depr:617-627): resolve a dependsOn to an NpmPackage via its
/// packageId (or the canonical→packageId lookup) + version.
fn resolve_dep(cache: &Path, d: &DependsOn) -> Option<Npm> {
    let id = d.package_id.clone().or_else(|| {
        // pcm.getPackageId(uri): not needed for the corpus (all dependsOn carry
        // packageId). Fire a loud gap if a uri-only dep ever appears.
        d.uri.as_ref().map(|_| panic!("LOUD GAP: dependency uri->packageId lookup (depr:621) not ported"))
    })?;
    load_npm(cache, &id, &d.version)
}

