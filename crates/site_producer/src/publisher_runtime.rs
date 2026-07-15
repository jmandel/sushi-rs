//! Closed static-asset and HTML-compatibility policy for the Rust Publisher
//! renderer.
//!
//! This is renderer implementation detail, not another handoff value. It
//! selects bytes already present in the mounted core/template packages and a
//! small, audited set of third-party files that those packages reference but do
//! not contain. The result is a complete, deterministic output inventory. No
//! host-supplied runtime byte bag or browser/editor asset catalog is involved.

use std::collections::BTreeMap;
#[cfg(feature = "dependency-observation")]
use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use package_store::template_loader::TemplateTree;
use package_store::PackageSource;
use site_build::{PackageCoordinate, Sha256Digest};

const RUNTIME_RECIPE: &str = "publisher-runtime/v2";
const NOTICE_PATH: &str = "_fhir-publisher-runtime/THIRD-PARTY-NOTICES.txt";
const JQUERY_PATH: &str = "assets/js/jquery.js";
const JQUERY_SHA256: &str = "d8f9afbf492e4c139e9d2bcb9ba6ef7c14921eb509fb703bc7a3f911b774eff8";
const JQUERY_UI_PATH: &str = "assets/js/jquery-ui.min.js";
const JQUERY_UI_SHA256: &str = "4dd865e0f9932d4c8e31ad8c04f1271116dad7462455e4fb3fea8c46ebdd7075";
const JQUERY_SHIM_PATH: &str = "_fhir-ig-editor/compat/jquery-3.7.0-ui-tabs-1.11.1.js";
const JQUERY_SHIM_SHA256: &str = "5ff89b5b73dd144e3bed959ab75251275127e501a290c4db7f1070c82d7b8f53";
const JQUERY_COMPAT_ID: &str = "jquery-3.7.0-ui-tabs-1.11.1";
const TABLE_SCRIPT_SOURCE_SHA256: &str =
    "06fd9b830c57cb0a727c37b438afc8162f136d651300550834d653b503976eb7";
const TABLE_SCRIPT_OUTPUT_SHA256: &str =
    "03c51f09901ea7496c927b48980943a6dbdde08f646a8150011f88ddcea48898";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublisherRuntimeProvenance {
    pub source: String,
    pub license: String,
    pub source_path: String,
    pub transformation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublisherRuntimeFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub provenance: PublisherRuntimeProvenance,
}

/// One private, immutable Publisher renderer runtime. Files are already
/// selected with `runtime < template` precedence; authored project images are
/// applied by the caller last because they are part of the PreparedGuide, not
/// part of this runtime.
#[derive(Clone, Debug)]
pub struct PublisherRuntime {
    files: BTreeMap<String, PublisherRuntimeFile>,
    recipe_sha256: Sha256Digest,
}

#[derive(Clone, Copy)]
struct EmbeddedAsset {
    path: &'static str,
    bytes: &'static [u8],
    source: &'static str,
    license: &'static str,
    source_path: &'static str,
}

macro_rules! embedded_asset {
    ($path:literal, $source:literal, $license:literal, $source_path:literal) => {
        EmbeddedAsset {
            path: $path,
            bytes: include_bytes!(concat!("../assets/publisher-runtime/", $path)),
            source: $source,
            license: $license,
            source_path: $source_path,
        }
    };
}

const EMBEDDED_ASSETS: &[EmbeddedAsset] = &[
    embedded_asset!(
        "assets/css/images/ui-bg_diagonals-thick_18_b81900_40x40.png",
        "jquery-ui#1.11.1",
        "MIT",
        "themes/ui-lightness/images/ui-bg_diagonals-thick_18_b81900_40x40.png"
    ),
    embedded_asset!(
        "assets/css/images/ui-bg_diagonals-thick_20_666666_40x40.png",
        "jquery-ui#1.11.1",
        "MIT",
        "themes/ui-lightness/images/ui-bg_diagonals-thick_20_666666_40x40.png"
    ),
    embedded_asset!(
        "assets/css/images/ui-bg_flat_10_000000_40x100.png",
        "jquery-ui#1.11.1",
        "MIT",
        "themes/ui-lightness/images/ui-bg_flat_10_000000_40x100.png"
    ),
    embedded_asset!(
        "assets/css/images/ui-bg_glass_100_f6f6f6_1x400.png",
        "jquery-ui#1.11.1",
        "MIT",
        "themes/ui-lightness/images/ui-bg_glass_100_f6f6f6_1x400.png"
    ),
    embedded_asset!(
        "assets/css/images/ui-bg_glass_100_fdf5ce_1x400.png",
        "jquery-ui#1.11.1",
        "MIT",
        "themes/ui-lightness/images/ui-bg_glass_100_fdf5ce_1x400.png"
    ),
    embedded_asset!(
        "assets/css/images/ui-bg_glass_65_ffffff_1x400.png",
        "jquery-ui#1.11.1",
        "MIT",
        "themes/ui-lightness/images/ui-bg_glass_65_ffffff_1x400.png"
    ),
    embedded_asset!(
        "assets/css/images/ui-bg_gloss-wave_35_f6a828_500x100.png",
        "jquery-ui#1.11.1",
        "MIT",
        "themes/ui-lightness/images/ui-bg_gloss-wave_35_f6a828_500x100.png"
    ),
    embedded_asset!(
        "assets/css/images/ui-bg_highlight-soft_100_eeeeee_1x100.png",
        "jquery-ui#1.11.1",
        "MIT",
        "themes/ui-lightness/images/ui-bg_highlight-soft_100_eeeeee_1x100.png"
    ),
    embedded_asset!(
        "assets/css/images/ui-bg_highlight-soft_75_ffe45c_1x100.png",
        "jquery-ui#1.11.1",
        "MIT",
        "themes/ui-lightness/images/ui-bg_highlight-soft_75_ffe45c_1x100.png"
    ),
    embedded_asset!(
        "assets/css/images/ui-icons_222222_256x240.png",
        "jquery-ui#1.11.1",
        "MIT",
        "themes/ui-lightness/images/ui-icons_222222_256x240.png"
    ),
    embedded_asset!(
        "assets/css/images/ui-icons_228ef1_256x240.png",
        "jquery-ui#1.11.1",
        "MIT",
        "themes/ui-lightness/images/ui-icons_228ef1_256x240.png"
    ),
    embedded_asset!(
        "assets/css/images/ui-icons_ef8c08_256x240.png",
        "jquery-ui#1.11.1",
        "MIT",
        "themes/ui-lightness/images/ui-icons_ef8c08_256x240.png"
    ),
    embedded_asset!(
        "assets/css/images/ui-icons_ffd27a_256x240.png",
        "jquery-ui#1.11.1",
        "MIT",
        "themes/ui-lightness/images/ui-icons_ffd27a_256x240.png"
    ),
    embedded_asset!(
        "assets/css/images/ui-icons_ffffff_256x240.png",
        "jquery-ui#1.11.1",
        "MIT",
        "themes/ui-lightness/images/ui-icons_ffffff_256x240.png"
    ),
    embedded_asset!(
        "assets/font/fontawesome-webfont.eot",
        "font-awesome#3.0.1",
        "OFL-1.1",
        "font/fontawesome-webfont.eot"
    ),
    embedded_asset!(
        "assets/font/fontawesome-webfont.ttf",
        "font-awesome#3.0.1",
        "OFL-1.1",
        "font/fontawesome-webfont.ttf"
    ),
    embedded_asset!(
        "assets/font/fontawesome-webfont.woff",
        "font-awesome#3.0.1",
        "OFL-1.1",
        "font/fontawesome-webfont.woff"
    ),
    embedded_asset!(
        "assets/images/usa.svg",
        "hl7.fhir.us.core#9.0.0-published",
        "CC0-1.0",
        "assets/images/usa.svg"
    ),
    embedded_asset!(
        "tree-filter.png",
        "hl7.fhir.us.core#9.0.0-published",
        "CC0-1.0",
        "tree-filter.png"
    ),
    embedded_asset!(
        "tbl_vjoin-open.png",
        "hl7.fhir.r5.core#5.0.0",
        "CC0-1.0",
        "other/tbl_vjoin-open.png"
    ),
    embedded_asset!(
        "tbl_vjoin_end-open.png",
        "hl7.fhir.r5.core#5.0.0",
        "CC0-1.0",
        "other/tbl_vjoin_end-open.png"
    ),
    embedded_asset!(
        "tbl_vjoin_end_slicer-open.png",
        "hl7.fhir.r5.core#5.0.0",
        "CC0-1.0",
        "other/tbl_vjoin_end_slicer-open.png"
    ),
    embedded_asset!(
        "tbl_vjoin_slicer-open.png",
        "hl7.fhir.r5.core#5.0.0",
        "CC0-1.0",
        "other/tbl_vjoin_slicer-open.png"
    ),
    embedded_asset!(
        "assets/images/theme/up.png",
        "publisher-runtime",
        "CC0-1.0",
        "generated:back-to-top-arrow-v1"
    ),
    embedded_asset!(
        "_fhir-ig-editor/compat/jquery-3.7.0-ui-tabs-1.11.1.js",
        "publisher-runtime",
        "CC0-1.0",
        "generated:jquery-3.7.0-ui-tabs-1.11.1-v1"
    ),
];

const OPEN_SANS_NAMES: &[&str] = &[
    "OpenSans-CondBold-webfont.eot",
    "OpenSans-CondBold-webfont.svg",
    "OpenSans-CondBold-webfont.ttf",
    "OpenSans-CondBold-webfont.woff",
    "OpenSans-CondLight-webfont.eot",
    "OpenSans-CondLight-webfont.svg",
    "OpenSans-CondLight-webfont.ttf",
    "OpenSans-CondLight-webfont.woff",
];

impl PublisherRuntime {
    /// Assemble the complete fixed/template static namespace from bytes already
    /// mounted in Rust. `core` must name the exact target core package in the
    /// build lock; no hidden package acquisition occurs here.
    pub fn assemble(
        source: &dyn PackageSource,
        cache_root: &Path,
        core: &PackageCoordinate,
        template: &TemplateTree,
    ) -> Result<Self> {
        let mut files = BTreeMap::new();

        // The small irreducible set comes first. Exact target core/template
        // bytes below replace it on collision.
        for asset in EMBEDDED_ASSETS {
            insert(
                &mut files,
                asset.path,
                asset.bytes.to_vec(),
                provenance(asset.source, asset.license, asset.source_path, None),
            );
        }

        add_core_assets(&mut files, source, cache_root, core)?;
        add_template_derived_runtime(&mut files, template)?;

        // Template static files have higher precedence than Publisher runtime.
        for (source_path, bytes) in template.files() {
            let Some(public_path) = template_public_path(source_path) else {
                continue;
            };
            insert(
                &mut files,
                &public_path,
                bytes.clone(),
                provenance(
                    "selected-template-chain",
                    "package-declared",
                    source_path,
                    None,
                ),
            );
        }

        let notice = third_party_notice(core);
        insert(
            &mut files,
            NOTICE_PATH,
            notice.into_bytes(),
            provenance(
                "publisher-runtime",
                "Apache-2.0",
                "generated:third-party-notices-v1",
                None,
            ),
        );

        let recipe_sha256 = recipe_digest(&files, core);
        Ok(Self {
            files,
            recipe_sha256,
        })
    }

    /// Reconstitute the exact runtime already committed as Publisher-runtime
    /// artifacts in a closed `SiteBuild`.
    ///
    /// This does not reselect or regenerate assets. It validates the closed
    /// inventory, recomputes the same compatibility-transform recipe, and
    /// refuses a build whose claimed recipe does not match its bytes and
    /// provenance. Fresh-process executors use this constructor so HTML
    /// finishing is identical to the live preparation that created the build.
    pub fn from_closed_files(
        files: impl IntoIterator<Item = PublisherRuntimeFile>,
        core: &PackageCoordinate,
        expected_recipe_sha256: &Sha256Digest,
    ) -> Result<Self> {
        let mut indexed = BTreeMap::new();
        for file in files {
            if file.path.is_empty()
                || file.path.starts_with('/')
                || file.path.contains(['\\', '\0'])
                || file
                    .path
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
            {
                bail!("closed Publisher runtime has unsafe path {:?}", file.path);
            }
            if file.media_type != mime_for(&file.path) {
                bail!(
                    "closed Publisher runtime {} has media type {:?}, expected {:?}",
                    file.path,
                    file.media_type,
                    mime_for(&file.path)
                );
            }
            if file.provenance.source.trim().is_empty()
                || file.provenance.license.trim().is_empty()
                || file.provenance.source_path.trim().is_empty()
            {
                bail!(
                    "closed Publisher runtime {} has incomplete provenance",
                    file.path
                );
            }
            let path = file.path.clone();
            if indexed.insert(path.clone(), file).is_some() {
                bail!("closed Publisher runtime repeats {path}");
            }
        }
        let recipe_sha256 = recipe_digest(&indexed, core);
        if &recipe_sha256 != expected_recipe_sha256 {
            bail!(
                "closed Publisher runtime recipe mismatch: expected {}, reconstructed {}",
                expected_recipe_sha256,
                recipe_sha256
            );
        }
        Ok(Self {
            files: indexed,
            recipe_sha256,
        })
    }

    pub fn files(&self) -> impl Iterator<Item = &PublisherRuntimeFile> {
        self.files.values()
    }

    pub fn get(&self, path: &str) -> Option<&PublisherRuntimeFile> {
        self.files.get(path)
    }

    pub fn recipe_sha256(&self) -> &Sha256Digest {
        &self.recipe_sha256
    }

    /// Apply the two page-output transforms owned by this exact runtime: novel
    /// table-background materialization and the byte-pair-gated jQuery bridge.
    /// Both are idempotent and run after Liquid but before ContentRef creation.
    pub fn finish_html(&self, html: &str) -> String {
        let tables = materialize_missing_table_backgrounds(html, &self.files, None);
        inject_jquery_compatibility(&tables, &self.files, None)
    }

    /// Observation-only form of [`Self::finish_html`]. It executes the same
    /// transforms while retaining exact runtime-path attempts and winning
    /// bytes. This is absent from default builds and never influences output.
    #[cfg(feature = "dependency-observation")]
    pub fn finish_html_observed(&self, html: &str) -> (String, FinishHtmlObservation) {
        let mut observation = FinishHtmlObservation::default();
        let tables =
            materialize_missing_table_backgrounds(html, &self.files, Some(&mut observation));
        let html = inject_jquery_compatibility(&tables, &self.files, Some(&mut observation));
        (html, observation)
    }
}

#[cfg(feature = "dependency-observation")]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FinishHtmlObservation {
    pub attempted: BTreeSet<String>,
    pub ready: BTreeMap<String, Sha256Digest>,
    pub missing: BTreeSet<String>,
    pub generated_table_backgrounds: BTreeSet<String>,
}

#[cfg(not(feature = "dependency-observation"))]
struct FinishHtmlObservation;

#[cfg(feature = "dependency-observation")]
impl FinishHtmlObservation {
    fn observe(&mut self, path: &str, file: Option<&PublisherRuntimeFile>) -> Option<Sha256Digest> {
        self.attempted.insert(path.to_string());
        match file {
            Some(file) => {
                let digest = Sha256Digest::of_bytes(&file.bytes);
                self.ready.insert(path.to_string(), digest.clone());
                Some(digest)
            }
            None => {
                self.missing.insert(path.to_string());
                None
            }
        }
    }
}

fn provenance(
    source: impl Into<String>,
    license: impl Into<String>,
    source_path: impl Into<String>,
    transformation: Option<&str>,
) -> PublisherRuntimeProvenance {
    PublisherRuntimeProvenance {
        source: source.into(),
        license: license.into(),
        source_path: source_path.into(),
        transformation: transformation.map(str::to_string),
    }
}

fn insert(
    files: &mut BTreeMap<String, PublisherRuntimeFile>,
    path: &str,
    bytes: Vec<u8>,
    provenance: PublisherRuntimeProvenance,
) {
    files.insert(
        path.to_string(),
        PublisherRuntimeFile {
            path: path.to_string(),
            media_type: mime_for(path).to_string(),
            bytes,
            provenance,
        },
    );
}

fn add_core_assets(
    files: &mut BTreeMap<String, PublisherRuntimeFile>,
    source: &dyn PackageSource,
    cache_root: &Path,
    core: &PackageCoordinate,
) -> Result<()> {
    let package_root = cache_root.join(core.as_str());
    let normalized = package_root.join("package/other");
    let native = package_root.join("other");
    let root = if source.is_dir(&normalized) {
        normalized
    } else if source.is_dir(&native) {
        native
    } else {
        bail!("Publisher runtime: {} has no other/ asset directory", core);
    };
    let mut entries = source
        .read_dir(&root)
        .with_context(|| format!("list {}/other", core))?;
    entries.sort_by(|left, right| left.file_name.cmp(&right.file_name));
    for entry in entries {
        if !entry.is_file || !is_core_runtime_asset(&entry.file_name) {
            continue;
        }
        let bytes = source
            .read(&root.join(&entry.file_name))
            .with_context(|| format!("read {}/other/{}", core, entry.file_name))?;
        insert(
            files,
            &entry.file_name,
            bytes,
            provenance(
                core.to_string(),
                "CC0-1.0",
                format!("other/{}", entry.file_name),
                None,
            ),
        );
    }
    for required in ["fhir.css", "icon_element.gif", "tbl_spacer.png"] {
        if !files.contains_key(required) {
            bail!("Publisher runtime: {} is missing required {required}", core);
        }
    }
    Ok(())
}

fn is_core_runtime_asset(name: &str) -> bool {
    let icon = name.starts_with("icon_") && (name.ends_with(".gif") || name.ends_with(".png"));
    let table = name.starts_with("tbl_") && name.ends_with(".png");
    icon || table
        || matches!(
            name,
            "cc0.png" | "external.png" | "help16.png" | "strip.png" | "watermark.png" | "fhir.css"
        )
}

fn add_template_derived_runtime(
    files: &mut BTreeMap<String, PublisherRuntimeFile>,
    template: &TemplateTree,
) -> Result<()> {
    for name in OPEN_SANS_NAMES {
        let source_path = format!("content/assets/fonts/{name}");
        if let Some(bytes) = template.get(&source_path) {
            insert(
                files,
                name,
                bytes.to_vec(),
                provenance(
                    "selected-template-chain",
                    "Apache-2.0",
                    source_path,
                    Some("root-font-alias/v1"),
                ),
            );
        }
    }

    let source_path = "content/assets/js/fhir-table-scripts.js";
    if let Some(bytes) = template.get(source_path) {
        let (bytes, transformation) = patch_table_script(bytes)?;
        insert(
            files,
            "fhir-table-scripts.js",
            bytes,
            provenance(
                "selected-template-chain",
                "CC0-1.0",
                source_path,
                transformation,
            ),
        );
    }
    Ok(())
}

fn patch_table_script(bytes: &[u8]) -> Result<(Vec<u8>, Option<&'static str>)> {
    if Sha256Digest::of_bytes(bytes).as_str() != TABLE_SCRIPT_SOURCE_SHA256 {
        return Ok((bytes.to_vec(), None));
    }
    let text = std::str::from_utf8(bytes).context("pinned fhir-table-scripts.js is not UTF-8")?;
    let old =
        "let classes = childElement.getAttribute('class');\n        if (classes.includes(prop)) {";
    let new = "if (childElement.classList.contains(prop)) {";
    if !text.contains(old) {
        bail!("pinned fhir-table-scripts.js no longer matches null-safe patch");
    }
    let out = text.replace(old, new).into_bytes();
    if Sha256Digest::of_bytes(&out).as_str() != TABLE_SCRIPT_OUTPUT_SHA256 {
        bail!("fhir-table-scripts.js compatibility output digest changed");
    }
    Ok((out, Some("null-safe-class-filter/v1")))
}

fn template_public_path(path: &str) -> Option<String> {
    let public = path
        .strip_prefix("content/")
        .map(str::to_string)
        .or_else(|| {
            path.strip_prefix("assets/")
                .map(|rest| format!("assets/{rest}"))
        })
        .or_else(|| path.strip_prefix("includes/").map(str::to_string))?;
    is_static_asset(&public).then_some(public)
}

fn is_static_asset(path: &str) -> bool {
    matches!(
        path.rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .as_deref(),
        Some(
            "css"
                | "js"
                | "png"
                | "svg"
                | "jpg"
                | "jpeg"
                | "gif"
                | "webp"
                | "ico"
                | "woff"
                | "woff2"
                | "ttf"
                | "otf"
                | "eot"
        )
    )
}

fn mime_for(path: &str) -> &'static str {
    match path
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("css") => "text/css",
        Some("js") => "text/javascript",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("eot") => "application/vnd.ms-fontobject",
        Some("txt") => "text/plain",
        _ => "application/octet-stream",
    }
}

fn recipe_digest(
    files: &BTreeMap<String, PublisherRuntimeFile>,
    core: &PackageCoordinate,
) -> Sha256Digest {
    let mut bytes = Vec::new();
    for value in [
        RUNTIME_RECIPE,
        core.as_str(),
        "table-background-inline-svg/v1",
        JQUERY_COMPAT_ID,
        JQUERY_SHA256,
        JQUERY_UI_SHA256,
        JQUERY_SHIM_SHA256,
        TABLE_SCRIPT_SOURCE_SHA256,
        TABLE_SCRIPT_OUTPUT_SHA256,
        "precedence:runtime<template<authored",
    ] {
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(0);
    }
    for file in files.values() {
        let content_sha256 = Sha256Digest::of_bytes(&file.bytes).to_string();
        for value in [
            file.path.as_str(),
            file.media_type.as_str(),
            file.provenance.source.as_str(),
            file.provenance.license.as_str(),
            file.provenance.source_path.as_str(),
            file.provenance.transformation.as_deref().unwrap_or(""),
            content_sha256.as_str(),
        ] {
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }
    }
    Sha256Digest::of_bytes(&bytes)
}

fn third_party_notice(core: &PackageCoordinate) -> String {
    format!(
        "FHIR Publisher Rust runtime asset notices\n\n\
         Target core package: {core} (CC0-1.0, HL7 Inc.)\n\
         Template-derived files retain their package-declared license.\n\
         US jurisdiction flag and tree filter: hl7.fhir.us.core#9.0.0 published output (CC0-1.0).\n\
         R5 open-state tree joins: hl7.fhir.r5.core#5.0.0 (CC0-1.0).\n\
         Generated compatibility files: fhir-publisher-rs project (CC0-1.0).\n\n\
         jQuery UI 1.11.1 images (MIT)\n\
         =================================\n{}\n\
         Font Awesome 3.0.1 fonts (SIL Open Font License 1.1)\n\
         ====================================================\n{}\n\
         Open Sans Condensed fonts (Apache License 2.0)\n\
         ===============================================\n{}",
        include_str!("../assets/licenses/JQUERY-UI-MIT.txt"),
        include_str!("../assets/licenses/OFL-1.1.txt"),
        include_str!("../assets/licenses/Apache-2.0.txt"),
    )
}

fn materialize_missing_table_backgrounds(
    html: &str,
    files: &BTreeMap<String, PublisherRuntimeFile>,
    observation: Option<&mut FinishHtmlObservation>,
) -> String {
    #[cfg(feature = "dependency-observation")]
    let mut observation = observation;
    #[cfg(not(feature = "dependency-observation"))]
    let _ = observation;
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find("url(") {
        let start = cursor + relative;
        let Some(close_relative) = lower[start + 4..].find(')') else {
            break;
        };
        let close = start + 4 + close_relative;
        let raw = html[start + 4..close].trim();
        let raw = if (raw.starts_with('\'') && raw.ends_with('\''))
            || (raw.starts_with('"') && raw.ends_with('"'))
        {
            &raw[1..raw.len() - 1]
        } else {
            raw
        };
        let name = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
        let table_background = is_table_background_name(name);
        #[cfg(feature = "dependency-observation")]
        if table_background {
            if let Some(observation) = observation.as_deref_mut() {
                observation.observe(name, files.get(name));
            }
        }
        if table_background && !files.contains_key(name) {
            #[cfg(feature = "dependency-observation")]
            if let Some(observation) = observation.as_deref_mut() {
                observation
                    .generated_table_backgrounds
                    .insert(name.to_string());
            }
            out.push_str(&html[cursor..start]);
            out.push_str("url(\"");
            out.push_str(&table_background_data_uri(name).expect("validated table background"));
            out.push_str("\")");
            cursor = close + 1;
        } else {
            out.push_str(&html[cursor..close + 1]);
            cursor = close + 1;
        }
    }
    out.push_str(&html[cursor..]);
    out
}

fn is_table_background_name(name: &str) -> bool {
    name.strip_prefix("tbl_bck")
        .and_then(|value| value.strip_suffix(".png"))
        .is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| (b'0'..=b'5').contains(&byte))
        })
}

fn table_background_data_uri(name: &str) -> Option<String> {
    if !is_table_background_name(name) {
        return None;
    }
    let digits = &name[7..name.len() - 4];
    let colors = ["#000000", "#0ed145", "#d4a815"];
    let mut rects = String::new();
    for (index, digit) in digits.bytes().enumerate() {
        let value = usize::from(digit - b'0');
        if value % 2 == 1 {
            rects.push_str(&format!(
                "<rect x=\"{}\" y=\"0\" width=\"1\" height=\"1\" fill=\"{}\"/>",
                12 + index * 16,
                colors[value / 2]
            ));
        }
    }
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"800\" height=\"2\" viewBox=\"0 0 800 2\" shape-rendering=\"crispEdges\">{rects}</svg>"
    );
    Some(format!(
        "data:image/svg+xml;base64,{}",
        base64(svg.as_bytes())
    ))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        out.push(ALPHABET[(a >> 2) as usize] as char);
        out.push(ALPHABET[(((a & 0x03) << 4) | (b >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(((b & 0x0f) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(c & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[derive(Debug)]
struct ScriptTag {
    end: usize,
    src: String,
}

fn inject_jquery_compatibility(
    html: &str,
    files: &BTreeMap<String, PublisherRuntimeFile>,
    observation: Option<&mut FinishHtmlObservation>,
) -> String {
    inject_jquery_compatibility_with_hashes(
        html,
        files,
        [JQUERY_SHA256, JQUERY_UI_SHA256, JQUERY_SHIM_SHA256],
        observation,
    )
}

fn inject_jquery_compatibility_with_hashes(
    html: &str,
    files: &BTreeMap<String, PublisherRuntimeFile>,
    expected_hashes: [&str; 3],
    observation: Option<&mut FinishHtmlObservation>,
) -> String {
    #[cfg(feature = "dependency-observation")]
    let mut observation = observation;
    #[cfg(not(feature = "dependency-observation"))]
    let _ = observation;
    if html.contains(&format!(
        "data-fhir-ig-editor-preview-compat=\"{JQUERY_COMPAT_ID}\""
    )) {
        return html.to_string();
    }
    for ((path, _), expected) in [
        (JQUERY_PATH, JQUERY_SHA256),
        (JQUERY_UI_PATH, JQUERY_UI_SHA256),
        (JQUERY_SHIM_PATH, JQUERY_SHIM_SHA256),
    ]
    .into_iter()
    .zip(expected_hashes)
    {
        let Some(file) = files.get(path) else {
            #[cfg(feature = "dependency-observation")]
            if let Some(observation) = observation.as_deref_mut() {
                observation.observe(path, None);
            }
            return html.to_string();
        };
        let digest = Sha256Digest::of_bytes(&file.bytes);
        #[cfg(feature = "dependency-observation")]
        if let Some(observation) = observation.as_deref_mut() {
            observation.attempted.insert(path.to_string());
            observation.ready.insert(path.to_string(), digest.clone());
        }
        if digest.as_str() != expected {
            return html.to_string();
        }
    }
    let tags = external_script_tags(html);
    let Some((index, jquery)) = tags
        .iter()
        .enumerate()
        .find(|(_, tag)| script_loads_path(&tag.src, JQUERY_PATH))
    else {
        return html.to_string();
    };
    if !tags[index + 1..]
        .iter()
        .any(|tag| script_loads_path(&tag.src, JQUERY_UI_PATH))
    {
        return html.to_string();
    }
    let shim = format!(
        "\n<script src=\"{JQUERY_SHIM_PATH}\" data-fhir-ig-editor-preview-compat=\"{JQUERY_COMPAT_ID}\" data-producer=\"publisher-runtime\"></script>"
    );
    let mut out = String::with_capacity(html.len() + shim.len());
    out.push_str(&html[..jquery.end]);
    out.push_str(&shim);
    out.push_str(&html[jquery.end..]);
    out
}

fn external_script_tags(html: &str) -> Vec<ScriptTag> {
    let lower = html.to_ascii_lowercase();
    let mut tags = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find("<script") {
        let start = cursor + relative;
        let Some(open_end_relative) = lower[start..].find('>') else {
            break;
        };
        let open_end = start + open_end_relative;
        let Some(close_relative) = lower[open_end + 1..].find("</script") else {
            break;
        };
        let close_start = open_end + 1 + close_relative;
        let Some(close_end_relative) = lower[close_start..].find('>') else {
            break;
        };
        let end = close_start + close_end_relative + 1;
        if let Some(src) = attribute_value(&html[start..=open_end], "src") {
            tags.push(ScriptTag { end, src });
        }
        cursor = end;
    }
    tags
}

fn attribute_value(tag: &str, expected: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        while cursor < bytes.len() && !bytes[cursor].is_ascii_alphabetic() {
            cursor += 1;
        }
        let start = cursor;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric() || matches!(bytes[cursor], b'-' | b'_'))
        {
            cursor += 1;
        }
        if start == cursor {
            continue;
        }
        let name = &tag[start..cursor];
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b'=' {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            return None;
        }
        let quote = bytes[cursor];
        let (value_start, value_end) = if matches!(quote, b'\'' | b'"') {
            cursor += 1;
            let end = bytes[cursor..].iter().position(|byte| *byte == quote)? + cursor;
            (cursor, end)
        } else {
            let end = bytes[cursor..]
                .iter()
                .position(|byte| byte.is_ascii_whitespace() || *byte == b'>')
                .map(|value| cursor + value)
                .unwrap_or(bytes.len());
            (cursor, end)
        };
        cursor = value_end.saturating_add(1);
        if name.eq_ignore_ascii_case(expected) {
            return Some(tag[value_start..value_end].to_string());
        }
    }
    None
}

fn script_loads_path(src: &str, expected: &str) -> bool {
    let mut path = src
        .split(['?', '#'])
        .next()
        .unwrap_or(src)
        .replace('\\', "/");
    if path.starts_with("//") || path.contains("://") {
        return false;
    }
    path = path.trim_start_matches('/').to_string();
    while let Some(rest) = path.strip_prefix("../") {
        path = rest.to_string();
    }
    while let Some(rest) = path.strip_prefix("./") {
        path = rest.to_string();
    }
    path == expected || path.ends_with(&format!("/{expected}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use package_store::BundleSource;

    use super::*;

    fn mounted_fixture() -> (BundleSource, PathBuf, PackageCoordinate, TemplateTree) {
        let mut source = BundleSource::new();
        let core = PackageCoordinate::new("hl7.fhir.r4.core", "4.0.1").unwrap();
        source.mount_package(
            core.as_str(),
            BTreeMap::<String, Vec<u8>>::from([
                (
                    "package.json".into(),
                    br#"{"name":"hl7.fhir.r4.core","version":"4.0.1"}"#.to_vec(),
                ),
                ("other/fhir.css".into(), b"body{}".to_vec()),
                ("other/icon_element.gif".into(), b"icon".to_vec()),
                ("other/tbl_spacer.png".into(), b"spacer".to_vec()),
                ("other/tbl_bck1.png".into(), b"fixed".to_vec()),
                ("other/tbl_vjoin-open.png".into(), b"core-open".to_vec()),
            ]),
        );

        let template_label = "test.template#1.0.0";
        source.mount_package(
            template_label,
            BTreeMap::<String, Vec<u8>>::from([
                (
                    "package.json".into(),
                    br#"{"name":"test.template","version":"1.0.0","type":"fhir.template"}"#
                        .to_vec(),
                ),
                ("config.json".into(), b"{}".to_vec()),
                (
                    "content/tree-filter.png".into(),
                    b"template-filter".to_vec(),
                ),
                ("content/assets/css/test.css".into(), b"body{}".to_vec()),
                (
                    "content/assets/fonts/OpenSans-CondBold-webfont.woff".into(),
                    b"open-sans".to_vec(),
                ),
            ]),
        );
        let root = source.cache_root().to_path_buf();
        let paths = package_store::template_loader::TemplatePaths::new(&root);
        let template =
            package_store::template_loader::materialize(&source, &paths, template_label).unwrap();
        (source, root, core, template)
    }

    #[test]
    fn inventory_is_closed_deterministic_and_uses_runtime_then_core_then_template() {
        let (source, root, core, template) = mounted_fixture();
        let runtime = PublisherRuntime::assemble(&source, &root, &core, &template).unwrap();
        let repeat = PublisherRuntime::assemble(&source, &root, &core, &template).unwrap();
        assert_eq!(runtime.recipe_sha256(), repeat.recipe_sha256());
        assert_eq!(
            runtime.get("tbl_vjoin-open.png").unwrap().bytes,
            b"core-open"
        );
        assert_eq!(
            runtime.get("tree-filter.png").unwrap().bytes,
            b"template-filter"
        );
        assert_eq!(
            runtime.get("tree-filter.png").unwrap().provenance.source,
            "selected-template-chain"
        );
        assert_eq!(
            runtime.get("OpenSans-CondBold-webfont.woff").unwrap().bytes,
            b"open-sans"
        );
        assert!(runtime.get("assets/css/test.css").is_some());
        assert!(runtime.get(NOTICE_PATH).unwrap().bytes.len() > 10_000);
        for file in runtime.files() {
            assert!(!file.path.is_empty());
            assert!(!file.bytes.is_empty());
            assert_ne!(file.media_type, "application/octet-stream");
            assert!(!file.provenance.license.is_empty());
        }
    }

    #[test]
    fn embedded_runtime_payload_is_the_audited_150112_bytes() {
        assert_eq!(EMBEDDED_ASSETS.len(), 25);
        assert_eq!(
            EMBEDDED_ASSETS
                .iter()
                .map(|asset| asset.bytes.len())
                .sum::<usize>(),
            150_112
        );
        let mut bytes = Vec::new();
        let mut assets = EMBEDDED_ASSETS.iter().collect::<Vec<_>>();
        assets.sort_by_key(|asset| asset.path);
        for asset in assets {
            bytes.extend_from_slice(asset.path.as_bytes());
            bytes.push(0);
            bytes.extend_from_slice(Sha256Digest::of_bytes(asset.bytes).to_string().as_bytes());
            bytes.push(0);
        }
        assert_eq!(
            Sha256Digest::of_bytes(&bytes).as_str(),
            "31089c4d250c8dbac367356adc6f28ed4c6796a325663f80f10a782f948b4588"
        );
    }

    #[test]
    fn table_background_generation_is_deterministic_and_preserves_fixed_core_bytes() {
        let mut files = BTreeMap::new();
        files.insert(
            "tbl_bck1.png".into(),
            PublisherRuntimeFile {
                path: "tbl_bck1.png".into(),
                bytes: b"fixed".to_vec(),
                media_type: "image/png".into(),
                provenance: provenance("test", "CC0-1.0", "test", None),
            },
        );
        let html = "<i style='background:url(tbl_bck1.png)'></i><i style='background:url(assets/tbl_bck134.png)'></i>";
        let out = materialize_missing_table_backgrounds(html, &files, None);
        assert!(out.contains("url(tbl_bck1.png)"));
        assert!(out.contains("data:image/svg+xml;base64,"));
        assert!(!out.contains("assets/tbl_bck134.png"));
        let uri = table_background_data_uri("tbl_bck134.png").unwrap();
        assert_eq!(uri, table_background_data_uri("tbl_bck134.png").unwrap());
    }

    #[test]
    fn jquery_transform_is_hash_order_and_idempotence_gated() {
        let mut files = BTreeMap::new();
        for (path, bytes) in [
            (JQUERY_PATH, b"jquery".as_slice()),
            (JQUERY_UI_PATH, b"jquery-ui".as_slice()),
            (JQUERY_SHIM_PATH, b"shim".as_slice()),
        ] {
            files.insert(
                path.into(),
                PublisherRuntimeFile {
                    path: path.into(),
                    bytes: bytes.to_vec(),
                    media_type: "text/javascript".into(),
                    provenance: provenance("test", "MIT", path, None),
                },
            );
        }
        let hashes = [
            Sha256Digest::of_bytes(b"jquery").to_string(),
            Sha256Digest::of_bytes(b"jquery-ui").to_string(),
            Sha256Digest::of_bytes(b"shim").to_string(),
        ];
        let expected = [hashes[0].as_str(), hashes[1].as_str(), hashes[2].as_str()];
        let html = "<script src='./assets/js/jquery.js?v=3'></script><script src='assets/js/jquery-ui.min.js'></script>";
        let out = inject_jquery_compatibility_with_hashes(html, &files, expected, None);
        assert!(out.contains(JQUERY_SHIM_PATH));
        assert_eq!(
            out,
            inject_jquery_compatibility_with_hashes(&out, &files, expected, None)
        );
        files.get_mut(JQUERY_PATH).unwrap().bytes.push(b'!');
        assert_eq!(
            html,
            inject_jquery_compatibility_with_hashes(html, &files, expected, None)
        );
        let reversed = "<script src='assets/js/jquery-ui.min.js'></script><script src='assets/js/jquery.js'></script>";
        assert_eq!(
            reversed,
            inject_jquery_compatibility_with_hashes(reversed, &files, expected, None)
        );
    }

    #[cfg(feature = "dependency-observation")]
    #[test]
    fn html_finish_observation_retains_conditional_runtime_reads_and_misses() {
        let mut files = BTreeMap::new();
        for (path, bytes) in [
            (JQUERY_PATH, b"jquery".as_slice()),
            (JQUERY_UI_PATH, b"jquery-ui".as_slice()),
            (JQUERY_SHIM_PATH, b"shim".as_slice()),
        ] {
            files.insert(
                path.into(),
                PublisherRuntimeFile {
                    path: path.into(),
                    bytes: bytes.to_vec(),
                    media_type: "text/javascript".into(),
                    provenance: provenance("test", "MIT", path, None),
                },
            );
        }
        let hashes = [
            Sha256Digest::of_bytes(b"jquery").to_string(),
            Sha256Digest::of_bytes(b"jquery-ui").to_string(),
            Sha256Digest::of_bytes(b"shim").to_string(),
        ];
        let expected = [hashes[0].as_str(), hashes[1].as_str(), hashes[2].as_str()];
        let html = "<i style='background:url(tbl_bck134.png)'></i><script src='assets/js/jquery.js'></script><script src='assets/js/jquery-ui.min.js'></script>";
        let mut observation = FinishHtmlObservation::default();
        let tables = materialize_missing_table_backgrounds(html, &files, Some(&mut observation));
        let output = inject_jquery_compatibility_with_hashes(
            &tables,
            &files,
            expected,
            Some(&mut observation),
        );
        assert!(output.contains(JQUERY_SHIM_PATH));
        assert!(observation.missing.contains("tbl_bck134.png"));
        assert!(observation
            .generated_table_backgrounds
            .contains("tbl_bck134.png"));
        assert_eq!(observation.ready.len(), 3);
        assert_eq!(observation.attempted.len(), 4);
    }
}
