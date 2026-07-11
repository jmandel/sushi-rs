//! Explicit compatibility projection from renderer-neutral [`PreparedGuide`]
//! semantics into the legacy Cycle `SiteDb` row model.
//!
//! This module is intentionally one-way. SiteDb is never consulted to construct
//! PreparedGuide or a v2 SiteBuild; consumers that still require SQLite/row
//! spelling opt into this projection after semantic preparation has completed.

use std::collections::HashMap;

use anyhow::{bail, Result};
use prepared_guide::{MenuNode, PageNode, PreparedGuide, SemanticResource};
use serde_json::Value;

use crate::model::{
    AssetRow, MenuRow, MetadataRow, PageRow, ResourceIdentity, ResourceRow, SiteConfigRow, SiteDb,
    ValueSetCodeRow,
};
use crate::rows::derive_concept_rows;

fn scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn field(resource: &Value, name: &str) -> Option<String> {
    resource.get(name).and_then(scalar_string)
}

fn canonical(resource: &Value) -> bool {
    resource
        .get("url")
        .and_then(Value::as_str)
        .is_some_and(|url| !url.is_empty())
}

fn metadata(prepared: &PreparedGuide) -> Vec<MetadataRow> {
    let guide = &prepared.guide;
    let source = guide.source_control.as_ref();
    let revision = source
        .and_then(|source| source.revision.clone())
        .unwrap_or_else(|| "unknown".into());
    let branch = source
        .and_then(|source| source.branch.clone())
        .unwrap_or_else(|| "unknown".into());
    let publisher = prepared.publisher_compatibility.as_ref();
    let values = [
        ("path", guide.fhir_publication_base.clone()),
        ("canonical", guide.canonical.clone().unwrap_or_default()),
        ("igId", guide.package_id.clone()),
        ("igName", guide.name.clone().unwrap_or_default()),
        ("packageId", guide.package_id.clone()),
        ("igVer", guide.version.clone().unwrap_or_default()),
        (
            "errorCount",
            publisher
                .map(|publisher| publisher.error_count.clone())
                .unwrap_or_else(|| "0".into()),
        ),
        ("version", guide.fhir_version.clone()),
        (
            "releaseLabel",
            guide
                .release_label
                .clone()
                .unwrap_or_else(|| "ci-build".into()),
        ),
        ("revision", revision.clone()),
        (
            "versionFull",
            if guide.fhir_version.is_empty() {
                revision
            } else {
                format!("{}-{revision}", guide.fhir_version)
            },
        ),
        (
            "toolingVersion",
            publisher
                .map(|publisher| publisher.tooling_version.clone())
                .unwrap_or_else(|| "site-gen.publisher".into()),
        ),
        (
            "toolingRevision",
            publisher
                .map(|publisher| publisher.tooling_revision.clone())
                .unwrap_or_else(|| "0".into()),
        ),
        (
            "toolingVersionFull",
            publisher
                .map(|publisher| publisher.tooling_version_full.clone())
                .unwrap_or_else(|| "site-gen.publisher experiment".into()),
        ),
        ("genDate", guide.generated.date.clone()),
        ("genDay", guide.generated.day.clone()),
        ("gitstatus", branch),
    ];
    values
        .into_iter()
        .enumerate()
        .map(|(index, (name, value))| MetadataRow {
            key: (index + 1) as i64,
            name: name.into(),
            value,
        })
        .collect()
}

fn resources(prepared: &PreparedGuide) -> Result<(Vec<ResourceRow>, HashMap<String, i64>)> {
    let primary = &prepared.guide.implementation_guide;
    let mut keys = HashMap::new();
    let mut rows = Vec::with_capacity(prepared.resources.len());
    for (index, semantic) in prepared.resources.iter().enumerate() {
        let resource = &semantic.resource;
        let resource_type = resource
            .get("resourceType")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let id = resource
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if resource_type != semantic.key.resource_type || id != semantic.key.id {
            bail!(
                "prepared resource {}/{} disagrees with its JSON identity {resource_type}/{id}",
                semantic.key.resource_type,
                semantic.key.id
            );
        }
        let key = (index + 1) as i64;
        keys.insert(format!("{resource_type}/{id}"), key);
        let primary_guide = &semantic.key == primary;
        let row_id = if primary_guide {
            prepared.guide.package_id.clone()
        } else {
            id.to_string()
        };
        let is_canonical = canonical(resource);
        let publication = semantic.publication.as_ref();
        rows.push(ResourceRow {
            key,
            type_: resource_type.into(),
            custom: 0,
            id: row_id.clone(),
            web: if primary_guide {
                "index.html".into()
            } else {
                format!("{resource_type}-{row_id}.html")
            },
            url: if primary_guide {
                prepared.guide.canonical.as_ref().map(|canonical| {
                    format!(
                        "{}/ImplementationGuide/{row_id}",
                        canonical.trim_end_matches('/')
                    )
                })
            } else if is_canonical {
                resource
                    .get("url")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            } else {
                None
            },
            version: is_canonical.then(|| field(resource, "version")).flatten(),
            status: field(resource, "status"),
            date: is_canonical.then(|| field(resource, "date")).flatten(),
            name: publication
                .and_then(|publication| publication.display_name.clone())
                .or_else(|| field(resource, "name")),
            title: field(resource, "title"),
            experimental: resource
                .get("experimental")
                .and_then(Value::as_bool)
                .map(|value| value.to_string()),
            realm: None,
            description: publication
                .and_then(|publication| publication.description.clone())
                .or_else(|| field(resource, "description")),
            purpose: field(resource, "purpose"),
            copyright: field(resource, "copyright"),
            copyright_label: field(resource, "copyrightLabel"),
            derivation: field(resource, "derivation"),
            standard_status: is_canonical
                .then(|| publication.and_then(|publication| publication.standard_status.clone()))
                .flatten(),
            kind: (resource_type == "StructureDefinition")
                .then(|| field(resource, "kind"))
                .flatten(),
            sd_type: (resource_type == "StructureDefinition")
                .then(|| field(resource, "type"))
                .flatten(),
            base: (resource_type == "StructureDefinition")
                .then(|| publication.and_then(|publication| publication.base_definition.clone()))
                .flatten(),
            content: field(resource, "content"),
            supplements: field(resource, "supplements"),
            json: serde_json::to_string(resource)?,
        });
    }
    Ok((rows, keys))
}

fn flatten_pages(nodes: &[PageNode], depth: i64, rows: &mut Vec<PageRow>) {
    for node in nodes {
        rows.push(PageRow {
            slug: node
                .name_url
                .strip_suffix(".html")
                .unwrap_or(&node.name_url)
                .into(),
            name_url: node.name_url.clone(),
            title: node.title.clone(),
            generation: node.generation.clone(),
            ord: rows.len() as i64,
            depth,
            body: node.body.clone(),
        });
        flatten_pages(&node.children, depth + 1, rows);
    }
}

fn flatten_menu(
    nodes: &[MenuNode],
    parent_id: Option<i64>,
    depth: i64,
    parent_path: &[String],
    rows: &mut Vec<MenuRow>,
) {
    for node in nodes {
        let id = (rows.len() + 1) as i64;
        let mut path = parent_path.to_vec();
        path.push(node.label.clone());
        rows.push(MenuRow {
            id,
            parent_id,
            ord: rows.len() as i64,
            depth,
            path: path.join("/"),
            label: node.label.clone(),
            href: node.href.clone(),
            kind: if node.href.is_some() { "link" } else { "group" }.into(),
        });
        flatten_menu(&node.items, Some(id), depth + 1, &path, rows);
    }
}

fn value_set_codes(
    prepared: &PreparedGuide,
    resource_keys: &HashMap<String, i64>,
) -> Result<Vec<ValueSetCodeRow>> {
    let mut rows = Vec::new();
    for expansion in &prepared.expansions {
        let reference = format!(
            "{}/{}",
            expansion.value_set.resource_type, expansion.value_set.id
        );
        let Some(resource_key) = resource_keys.get(&reference).copied() else {
            bail!("prepared expansion references missing resource {reference}");
        };
        for code in &expansion.codes {
            rows.push(ValueSetCodeRow {
                key: (rows.len() + 1) as i64,
                resource_key,
                value_set_uri: expansion.url.clone(),
                value_set_version: expansion.version.clone().unwrap_or_default(),
                system: code.system.clone(),
                version: code.version.clone(),
                code: code.code.clone(),
                display: code.display.clone(),
            });
        }
    }
    Ok(rows)
}

/// Materialize the legacy row spelling from one already-prepared semantic
/// value. No compiler, snapshot generator, renderer, or filesystem callback is
/// reachable from this projection.
pub fn project_prepared(prepared: &PreparedGuide) -> Result<SiteDb> {
    let (resource_rows, resource_keys) = resources(prepared)?;
    let resource_values: Vec<Value> = prepared
        .resources
        .iter()
        .map(|resource: &SemanticResource| resource.resource.clone())
        .collect();
    let concepts = derive_concept_rows(&resource_values, &resource_keys);
    let mut pages = Vec::new();
    flatten_pages(&prepared.pages, 0, &mut pages);
    let mut menu = Vec::new();
    flatten_menu(&prepared.menu, None, 0, &[], &mut menu);

    Ok(SiteDb {
        primary_implementation_guide: Some(ResourceIdentity {
            resource_type: prepared.guide.implementation_guide.resource_type.clone(),
            id: prepared.guide.implementation_guide.id.clone(),
        }),
        metadata: metadata(prepared),
        resources: resource_rows,
        concepts,
        value_set_codes: value_set_codes(prepared, &resource_keys)?,
        pages,
        menu,
        site_config: vec![SiteConfigRow {
            name: "sushi-config".into(),
            json: serde_json::to_string_pretty(&prepared.sushi_config)?,
        }],
        assets: prepared
            .assets
            .iter()
            .map(|asset| AssetRow {
                name: asset.path.as_str().into(),
                mime: asset.mime.clone(),
                content: asset.content.clone(),
            })
            .collect(),
    })
}
