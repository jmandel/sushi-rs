//! SQLite-free row model. The pipeline (S5/S6) produces these; the writer (S7)
//! is a thin sink that inserts them. Keeping the model sqlite-free is the wasm
//! requirement from the plan (§5): a wasm build can serialize these rows to JSON
//! for a JS-side writer without pulling in a C sqlite. No rusqlite types leak in.

/// Metadata (Key, Name, Value).
#[derive(Clone, Debug)]
pub struct MetadataRow {
    pub key: i64,
    pub name: String,
    pub value: String,
}

/// Resources — scalar projections + the full resource JSON (snapshot-complete for SDs).
#[derive(Clone, Debug)]
pub struct ResourceRow {
    pub key: i64,
    pub type_: String,
    pub custom: i64,
    pub id: String,
    pub web: String,
    pub url: Option<String>,
    pub version: Option<String>,
    pub status: Option<String>,
    pub date: Option<String>,
    pub name: Option<String>,
    pub title: Option<String>,
    pub experimental: Option<String>,
    pub realm: Option<String>,
    pub description: Option<String>,
    pub purpose: Option<String>,
    pub copyright: Option<String>,
    pub copyright_label: Option<String>,
    pub derivation: Option<String>,
    pub standard_status: Option<String>,
    pub kind: Option<String>,
    pub sd_type: Option<String>,
    pub base: Option<String>,
    pub content: Option<String>,
    pub supplements: Option<String>,
    pub json: String,
}

/// Concepts — flattened CodeSystem concept[] with ParentKey hierarchy.
#[derive(Clone, Debug)]
pub struct ConceptRow {
    pub key: i64,
    pub resource_key: i64,
    pub parent_key: Option<i64>,
    pub code: Option<String>,
    pub display: Option<String>,
    pub definition: Option<String>,
}

/// ValueSet_Codes — expansion rows (S4; empty in the cycle corpus, deferred).
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug)]
pub struct PageRow {
    pub slug: String,
    pub name_url: String,
    pub title: String,
    pub generation: String,
    pub ord: i64,
    pub depth: i64,
    pub body: Option<String>,
}

/// Menu — curated top-nav from sushi-config.yaml `menu:`.
#[derive(Clone, Debug)]
pub struct MenuRow {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub ord: i64,
    pub depth: i64,
    pub path: String,
    pub label: String,
    pub href: Option<String>,
    pub kind: String,
}

/// SiteConfig — parsed source config (verbatim yaml -> json).
#[derive(Clone, Debug)]
pub struct SiteConfigRow {
    pub name: String,
    pub json: String,
}

/// Assets — images + referenced includes.
#[derive(Clone, Debug)]
pub struct AssetRow {
    pub name: String,
    pub mime: String,
    pub content: Vec<u8>,
}

/// The complete row set for a site.db. Produced entirely without sqlite.
#[derive(Clone, Debug, Default)]
pub struct SiteDb {
    pub metadata: Vec<MetadataRow>,
    pub resources: Vec<ResourceRow>,
    pub concepts: Vec<ConceptRow>,
    pub value_set_codes: Vec<ValueSetCodeRow>,
    pub pages: Vec<PageRow>,
    pub menu: Vec<MenuRow>,
    pub site_config: Vec<SiteConfigRow>,
    pub assets: Vec<AssetRow>,
}
