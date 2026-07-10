//! SQLite-free row model. The pipeline (S5/S6) produces these; the writer (S7)
//! is a thin sink that inserts them. Keeping the model sqlite-free is the wasm
//! requirement from the plan (§5): a wasm build can serialize these rows to JSON
//! for a JS-side writer without pulling in a C sqlite. No rusqlite types leak in.
//!
//! The rows are `Serialize` so the wasm build can hand the whole [`SiteDb`] to a
//! JS-side row store as one JSON string (the editor's `build_site_db` export).
//! Serde field names use the SQLite/`core/db.ts` column casing (`Key`, `Type`,
//! `Json`, ...) so the JS row store reads them directly; `AssetRow.content` is
//! base64 (the JS side decodes text assets via `TextDecoder`).

use serde::Serialize;

/// Metadata (Key, Name, Value).
#[derive(Clone, Debug, Serialize)]
pub struct MetadataRow {
    #[serde(rename = "Key")]
    pub key: i64,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Value")]
    pub value: String,
}

/// Resources — scalar projections + the full resource JSON (snapshot-complete for SDs).
/// Serde field names match the SQLite/`core/db.ts` column names exactly (the mixed
/// casing is the Publisher's schema, not ours).
#[derive(Clone, Debug, Serialize)]
pub struct ResourceRow {
    #[serde(rename = "Key")]
    pub key: i64,
    #[serde(rename = "Type")]
    pub type_: String,
    #[serde(rename = "Custom")]
    pub custom: i64,
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Web")]
    pub web: String,
    #[serde(rename = "Url")]
    pub url: Option<String>,
    #[serde(rename = "Version")]
    pub version: Option<String>,
    #[serde(rename = "Status")]
    pub status: Option<String>,
    #[serde(rename = "Date")]
    pub date: Option<String>,
    #[serde(rename = "Name")]
    pub name: Option<String>,
    #[serde(rename = "Title")]
    pub title: Option<String>,
    #[serde(rename = "Experimental")]
    pub experimental: Option<String>,
    #[serde(rename = "Realm")]
    pub realm: Option<String>,
    #[serde(rename = "Description")]
    pub description: Option<String>,
    #[serde(rename = "Purpose")]
    pub purpose: Option<String>,
    #[serde(rename = "Copyright")]
    pub copyright: Option<String>,
    #[serde(rename = "CopyrightLabel")]
    pub copyright_label: Option<String>,
    #[serde(rename = "derivation")]
    pub derivation: Option<String>,
    #[serde(rename = "standardStatus")]
    pub standard_status: Option<String>,
    #[serde(rename = "kind")]
    pub kind: Option<String>,
    #[serde(rename = "sdType")]
    pub sd_type: Option<String>,
    #[serde(rename = "base")]
    pub base: Option<String>,
    #[serde(rename = "content")]
    pub content: Option<String>,
    #[serde(rename = "supplements")]
    pub supplements: Option<String>,
    #[serde(rename = "Json")]
    pub json: String,
}

/// Concepts — flattened CodeSystem concept[] with ParentKey hierarchy.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ConceptRow {
    pub key: i64,
    pub resource_key: i64,
    pub parent_key: Option<i64>,
    pub code: Option<String>,
    pub display: Option<String>,
    pub definition: Option<String>,
}

/// ValueSet_Codes — expansion rows (S4; empty in the cycle corpus, deferred).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ValueSetCodeRow {
    pub key: i64,
    pub resource_key: i64,
    pub value_set_uri: String,
    pub value_set_version: String,
    pub system: String,
    pub version: Option<String>,
    pub code: String,
    pub display: Option<String>,
}

/// Pages — page tree (from IG definition.page) + body (from input/pagecontent).
#[derive(Clone, Debug, Serialize)]
pub struct PageRow {
    #[serde(rename = "Slug")]
    pub slug: String,
    #[serde(rename = "NameUrl")]
    pub name_url: String,
    #[serde(rename = "Title")]
    pub title: String,
    #[serde(rename = "Generation")]
    pub generation: String,
    #[serde(rename = "Ord")]
    pub ord: i64,
    #[serde(rename = "Depth")]
    pub depth: i64,
    #[serde(rename = "Body")]
    pub body: Option<String>,
}

/// Menu — curated top-nav from sushi-config.yaml `menu:`.
#[derive(Clone, Debug, Serialize)]
pub struct MenuRow {
    #[serde(rename = "Id")]
    pub id: i64,
    #[serde(rename = "ParentId")]
    pub parent_id: Option<i64>,
    #[serde(rename = "Ord")]
    pub ord: i64,
    #[serde(rename = "Depth")]
    pub depth: i64,
    #[serde(rename = "Path")]
    pub path: String,
    #[serde(rename = "Label")]
    pub label: String,
    #[serde(rename = "Href")]
    pub href: Option<String>,
    #[serde(rename = "Kind")]
    pub kind: String,
}

/// SiteConfig — parsed source config (verbatim yaml -> json).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SiteConfigRow {
    pub name: String,
    pub json: String,
}

/// Assets — images + referenced includes. `content` serializes as base64 for the
/// JS boundary (`Content` is a BLOB in SQLite; the JS row store base64-decodes and
/// serves text assets via `TextDecoder`, binary assets as bytes).
#[derive(Clone, Debug, Serialize)]
pub struct AssetRow {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Mime")]
    pub mime: String,
    #[serde(rename = "Content", serialize_with = "ser_base64")]
    pub content: Vec<u8>,
}

fn ser_base64<S: serde::Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&base64_encode(bytes))
}

/// Standard-alphabet base64 with `=` padding (no external dep, mirrors the wasm_api
/// decoder's alphabet).
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHA[((n >> 18) & 63) as usize] as char);
        out.push(ALPHA[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHA[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHA[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// The complete row set for a site.db. Produced entirely without sqlite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceIdentity {
    pub resource_type: String,
    pub id: String,
}

/// The complete row set for a site.db. Produced entirely without sqlite.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteDb {
    /// The generated guide selected before examples are merged and rows are
    /// sorted. This semantic identity is intentionally not part of the legacy
    /// row/SQLite serialization; typed projections consume it in memory.
    #[serde(skip)]
    pub primary_implementation_guide: Option<ResourceIdentity>,
    pub metadata: Vec<MetadataRow>,
    pub resources: Vec<ResourceRow>,
    pub concepts: Vec<ConceptRow>,
    pub value_set_codes: Vec<ValueSetCodeRow>,
    pub pages: Vec<PageRow>,
    pub menu: Vec<MenuRow>,
    pub site_config: Vec<SiteConfigRow>,
    pub assets: Vec<AssetRow>,
}
