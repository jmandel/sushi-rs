//! S7 — the SQLite sink. A thin writer over the sqlite-free `SiteDb` row model.
//! Schema is a verbatim port of `site-gen/publisher/schema.ts` (the package.db
//! tables) plus the four augmentation tables `ingest.ts` adds (Pages/Menu/
//! SiteConfig/Assets). rusqlite is confined to this module (wasm requirement §5).

use anyhow::Result;
use rusqlite::{params, Connection};

use crate::model::SiteDb;

/// The package.db schema (schema.ts) + the ingest.ts augmentation tables.
const SCHEMA_SQL: &str = r#"
DROP TABLE IF EXISTS CodeSystemList;
DROP TABLE IF EXISTS CodeSystemListOIDs;
DROP TABLE IF EXISTS CodeSystemListRefs;
DROP TABLE IF EXISTS ConceptMappings;
DROP TABLE IF EXISTS ConceptProperties;
DROP TABLE IF EXISTS Concepts;
DROP TABLE IF EXISTS Designations;
DROP TABLE IF EXISTS Metadata;
DROP TABLE IF EXISTS Properties;
DROP TABLE IF EXISTS Resources;
DROP TABLE IF EXISTS ValueSetList;
DROP TABLE IF EXISTS ValueSetListOIDs;
DROP TABLE IF EXISTS ValueSetListRefs;
DROP TABLE IF EXISTS ValueSetListSources;
DROP TABLE IF EXISTS ValueSetListSystems;
DROP TABLE IF EXISTS ValueSet_Codes;

CREATE TABLE CodeSystemList (
CodeSystemListKey integer NOT NULL,
ViewType          integer NOT NULL,
ResourceKey       integer NULL,
Url               nvarchar NULL,
Version           nvarchar NULL,
Status            nvarchar NULL,
Name              nvarchar NULL,
Title             nvarchar NULL,
Description       nvarchar NULL,
PRIMARY KEY (CodeSystemListKey));

CREATE TABLE CodeSystemListOIDs (
CodeSystemListKey integer NOT NULL,
OID               nvarchar NOT NULL,
PRIMARY KEY (CodeSystemListKey,OID));

CREATE TABLE CodeSystemListRefs (
CodeSystemListKey integer NOT NULL,
Type              nvarchar NOT NULL,
Id                nvarchar NOT NULL,
ResourceKey       integer NULL,
Title             nvarchar NULL,
Web               nvarchar NULL,
PRIMARY KEY (CodeSystemListKey,Type,Id));

CREATE TABLE ConceptMappings (
Key           integer NOT NULL,
ResourceKey   integer NOT NULL,
SourceSystem  varchar NULL,
SourceVersion varchar NULL,
SourceCode    varchar NULL,
Relationship  varchar NULL,
TargetSystem  varchar NULL,
TargetVersion varchar NULL,
TargetCode    varchar NULL,
PRIMARY KEY (Key));

CREATE TABLE ConceptProperties (
Key          integer NOT NULL,
ResourceKey  integer NOT NULL,
ConceptKey   integer NOT NULL,
PropertyKey  integer NULL,
Code         varchar NULL,
Value        varchar NULL,
PRIMARY KEY (Key));

CREATE TABLE Concepts (
Key          integer NOT NULL,
ResourceKey  integer NOT NULL,
ParentKey    integer NULL,
Code         varchar NULL,
Display      varchar NULL,
Definition   varchar NULL,
PRIMARY KEY (Key));

CREATE TABLE Designations (
Key          integer NOT NULL,
ResourceKey  integer NOT NULL,
ConceptKey   integer NOT NULL,
UseSystem    varchar NULL,
UseCode      varchar NULL,
Lang         varchar NULL,
Value        text NULL,
PRIMARY KEY (Key));

CREATE TABLE Metadata (
Key    integer NOT NULL,
Name   nvarchar NOT NULL,
Value  nvarchar NOT NULL,
PRIMARY KEY (Key));

CREATE TABLE Properties (
Key          integer NOT NULL,
ResourceKey  integer NOT NULL,
Code         varchar NOT NULL,
Uri          varchar NULL,
Description  varchar NULL,
Type         varchar NULL,
PRIMARY KEY (Key));

CREATE TABLE Resources (
Key             integer NOT NULL,
Type            nvarchar NOT NULL,
Custom          integer NOT NULL,
Id              nvarchar NOT NULL,
Web             nvarchar NOT NULL,
Url             nvarchar NULL,
Version         nvarchar NULL,
Status          nvarchar NULL,
Date            nvarchar NULL,
Name            nvarchar NULL,
Title           nvarchar NULL,
Experimental    nvarchar NULL,
Realm           nvarchar NULL,
Description     nvarchar NULL,
Purpose         nvarchar NULL,
Copyright       nvarchar NULL,
CopyrightLabel  nvarchar NULL,
derivation      nvarchar NULL,
standardStatus  nvarchar NULL,
kind            nvarchar NULL,
sdType          nvarchar NULL,
base            nvarchar NULL,
content         nvarchar NULL,
supplements     nvarchar NULL,
Json            nvarchar NOT NULL,
PRIMARY KEY (Key));

CREATE TABLE ValueSetList (
ValueSetListKey   integer NOT NULL,
ViewType          integer NOT NULL,
ResourceKey       integer NULL,
Url               nvarchar NULL,
Version           nvarchar NULL,
Status            nvarchar NULL,
Name              nvarchar NULL,
Title             nvarchar NULL,
Description       nvarchar NULL,
PRIMARY KEY (ValueSetListKey));

CREATE TABLE ValueSetListOIDs (
ValueSetListKey   integer NOT NULL,
OID               nvarchar NOT NULL,
PRIMARY KEY (ValueSetListKey,OID));

CREATE TABLE ValueSetListRefs (
ValueSetListKey   integer NOT NULL,
Type              nvarchar NOT NULL,
Id                nvarchar NOT NULL,
ResourceKey       integer NULL,
Title             nvarchar NULL,
Web               nvarchar NULL,
PRIMARY KEY (ValueSetListKey,Type,Id));

CREATE TABLE ValueSetListSources (
ValueSetListKey   integer NOT NULL,
Source            nvarchar NOT NULL,
PRIMARY KEY (ValueSetListKey,Source));

CREATE TABLE ValueSetListSystems (
ValueSetListKey   integer NOT NULL,
URL               nvarchar NOT NULL,
PRIMARY KEY (ValueSetListKey,URL));

CREATE TABLE ValueSet_Codes (
Key             integer NOT NULL,
ResourceKey     integer NOT NULL,
ValueSetUri     nvarchar NOT NULL,
ValueSetVersion nvarchar NOT NULL,
System          nvarchar NOT NULL,
Version         nvarchar NULL,
Code            nvarchar NOT NULL,
Display         nvarchar NULL,
PRIMARY KEY (Key));

-- ingest.ts augmentation tables (site.db boundary, §2b)
CREATE TABLE Pages (Slug TEXT PRIMARY KEY, NameUrl TEXT, Title TEXT, Generation TEXT, Ord INTEGER, Depth INTEGER, Body TEXT);
CREATE TABLE Menu (Id INTEGER PRIMARY KEY, ParentId INTEGER, Ord INTEGER, Depth INTEGER, Path TEXT, Label TEXT, Href TEXT, Kind TEXT);
CREATE TABLE SiteConfig (Name TEXT PRIMARY KEY, Json TEXT NOT NULL);
CREATE TABLE Assets (Name TEXT PRIMARY KEY, Mime TEXT, Content TEXT);
"#;

/// Write the full `SiteDb` row model to a SQLite file at `out_path`.
pub fn write_site_db(out_path: &std::path::Path, db: &SiteDb) -> Result<()> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", out_path.display(), suffix));
    }
    let mut conn = Connection::open(out_path)?;
    let tx = conn.transaction()?;
    tx.execute_batch(SCHEMA_SQL)?;

    {
        let mut ins = tx.prepare("INSERT INTO Metadata (Key, Name, Value) VALUES (?,?,?)")?;
        for r in &db.metadata {
            ins.execute(params![r.key, r.name, r.value])?;
        }
    }
    {
        let mut ins = tx.prepare(
            "INSERT INTO Resources (Key, Type, Custom, Id, Web, Url, Version, Status, Date, Name, Title, Experimental, Realm, Description, Purpose, Copyright, CopyrightLabel, derivation, standardStatus, kind, sdType, base, content, supplements, Json) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )?;
        for r in &db.resources {
            ins.execute(params![
                r.key,
                r.type_,
                r.custom,
                r.id,
                r.web,
                r.url,
                r.version,
                r.status,
                r.date,
                r.name,
                r.title,
                r.experimental,
                r.realm,
                r.description,
                r.purpose,
                r.copyright,
                r.copyright_label,
                r.derivation,
                r.standard_status,
                r.kind,
                r.sd_type,
                r.base,
                r.content,
                r.supplements,
                r.json,
            ])?;
        }
    }
    {
        let mut ins = tx.prepare(
            "INSERT INTO Concepts (Key, ResourceKey, ParentKey, Code, Display, Definition) VALUES (?,?,?,?,?,?)",
        )?;
        for r in &db.concepts {
            ins.execute(params![
                r.key,
                r.resource_key,
                r.parent_key,
                r.code,
                r.display,
                r.definition
            ])?;
        }
    }
    {
        let mut ins = tx.prepare(
            "INSERT INTO ValueSet_Codes (Key, ResourceKey, ValueSetUri, ValueSetVersion, System, Version, Code, Display) VALUES (?,?,?,?,?,?,?,?)",
        )?;
        for r in &db.value_set_codes {
            ins.execute(params![
                r.key,
                r.resource_key,
                r.value_set_uri,
                r.value_set_version,
                r.system,
                r.version,
                r.code,
                r.display
            ])?;
        }
    }
    {
        let mut ins = tx.prepare(
            "INSERT OR REPLACE INTO Pages (Slug, NameUrl, Title, Generation, Ord, Depth, Body) VALUES (?,?,?,?,?,?,?)",
        )?;
        for r in &db.pages {
            ins.execute(params![
                r.slug,
                r.name_url,
                r.title,
                r.generation,
                r.ord,
                r.depth,
                r.body
            ])?;
        }
    }
    {
        let mut ins = tx.prepare(
            "INSERT INTO Menu (Id, ParentId, Ord, Depth, Path, Label, Href, Kind) VALUES (?,?,?,?,?,?,?,?)",
        )?;
        for r in &db.menu {
            ins.execute(params![
                r.id,
                r.parent_id,
                r.ord,
                r.depth,
                r.path,
                r.label,
                r.href,
                r.kind
            ])?;
        }
    }
    {
        let mut ins = tx.prepare("INSERT OR REPLACE INTO SiteConfig (Name, Json) VALUES (?,?)")?;
        for r in &db.site_config {
            ins.execute(params![r.name, r.json])?;
        }
    }
    {
        let mut ins =
            tx.prepare("INSERT OR REPLACE INTO Assets (Name, Mime, Content) VALUES (?,?,?)")?;
        for r in &db.assets {
            // ingest.ts stores raw bytes via readFileSync(path) (a Buffer). bun's
            // sqlite writes that as a BLOB; core/db.ts decodes text assets via
            // TextDecoder. Store BLOB to match byte-for-byte.
            ins.execute(params![r.name, r.mime, r.content])?;
        }
    }

    tx.commit()?;
    Ok(())
}
