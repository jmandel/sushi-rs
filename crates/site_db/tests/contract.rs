//! Gate (i): the package.db contract — a Rust port of
//! `site-gen/publisher/contract.ts::PACKAGE_DB_CONTRACT`. A site.db written by
//! `writer::write_site_db` must have every contract table with every contract
//! column. This asserts the schema the TS renderer's `assertPackageDbContract`
//! checks at startup, without needing bun.

use rusqlite::Connection;
use site_db::model::*;
use site_db::writer::write_site_db;

/// The exact contract from contract.ts:3-33 (table -> required columns).
const CONTRACT: &[(&str, &[&str])] = &[
    ("Metadata", &["Key", "Name", "Value"]),
    (
        "Resources",
        &[
            "Key",
            "Type",
            "Id",
            "Web",
            "Url",
            "Version",
            "Status",
            "Date",
            "Name",
            "Title",
            "Description",
            "derivation",
            "standardStatus",
            "kind",
            "sdType",
            "base",
            "content",
            "supplements",
            "Json",
        ],
    ),
    (
        "Concepts",
        &["Key", "ResourceKey", "ParentKey", "Code", "Display", "Definition"],
    ),
    (
        "ValueSet_Codes",
        &[
            "Key",
            "ResourceKey",
            "ValueSetUri",
            "ValueSetVersion",
            "System",
            "Version",
            "Code",
            "Display",
        ],
    ),
    (
        "ValueSetList",
        &[
            "ValueSetListKey",
            "ViewType",
            "ResourceKey",
            "Url",
            "Version",
            "Status",
            "Name",
            "Title",
            "Description",
        ],
    ),
    (
        "ValueSetListRefs",
        &["ValueSetListKey", "Type", "Id", "ResourceKey", "Title", "Web"],
    ),
    ("ValueSetListSystems", &["ValueSetListKey", "URL"]),
    (
        "CodeSystemList",
        &[
            "CodeSystemListKey",
            "ViewType",
            "ResourceKey",
            "Url",
            "Version",
            "Status",
            "Name",
            "Title",
            "Description",
        ],
    ),
    (
        "CodeSystemListRefs",
        &["CodeSystemListKey", "Type", "Id", "ResourceKey", "Title", "Web"],
    ),
];

fn table_columns(conn: &Connection, table: &str) -> Option<Vec<String>> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?",
            [table],
            |_| Ok(()),
        )
        .is_ok();
    if !exists {
        return None;
    }
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info(\"{}\")", table.replace('"', "\"\"")))
        .ok()?;
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .ok()?
        .filter_map(Result::ok)
        .collect();
    Some(cols)
}

fn contract_errors(conn: &Connection) -> Vec<String> {
    let mut errors = Vec::new();
    for (table, columns) in CONTRACT {
        match table_columns(conn, table) {
            None => errors.push(format!("missing table {table}")),
            Some(actual) => {
                for c in *columns {
                    if !actual.iter().any(|a| a == c) {
                        errors.push(format!("missing column {table}.{c}"));
                    }
                }
            }
        }
    }
    errors
}

#[test]
fn empty_site_db_satisfies_contract() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("site.db");
    let db = SiteDb::default();
    write_site_db(&out, &db).unwrap();
    let conn = Connection::open(&out).unwrap();
    let errors = contract_errors(&conn);
    assert!(errors.is_empty(), "contract violations: {errors:?}");
}

#[test]
fn populated_site_db_satisfies_contract() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("site.db");
    let mut db = SiteDb::default();
    db.metadata.push(MetadataRow {
        key: 1,
        name: "packageId".into(),
        value: "x".into(),
    });
    db.resources.push(ResourceRow {
        key: 1,
        type_: "CodeSystem".into(),
        custom: 0,
        id: "cs".into(),
        web: "CodeSystem-cs.html".into(),
        url: Some("http://x/cs".into()),
        version: Some("1".into()),
        status: Some("draft".into()),
        date: None,
        name: Some("Cs".into()),
        title: None,
        experimental: None,
        realm: None,
        description: None,
        purpose: None,
        copyright: None,
        copyright_label: None,
        derivation: None,
        standard_status: None,
        kind: None,
        sd_type: None,
        base: None,
        content: Some("complete".into()),
        supplements: None,
        json: "{}".into(),
    });
    db.concepts.push(ConceptRow {
        key: 1,
        resource_key: 1,
        parent_key: None,
        code: Some("a".into()),
        display: Some("A".into()),
        definition: None,
    });
    write_site_db(&out, &db).unwrap();
    let conn = Connection::open(&out).unwrap();
    assert!(contract_errors(&conn).is_empty());
    let n: i64 = conn
        .query_row("SELECT count(*) FROM Resources", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}
