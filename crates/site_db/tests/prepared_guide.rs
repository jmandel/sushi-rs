use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use prepared_guide::PreparedPath;
use serde_json::json;

#[test]
fn prepared_assets_retain_exact_winning_source_paths() {
    let primary = json!({
        "resourceType": "ImplementationGuide",
        "id": "primary",
        "url": "https://example.org/ig/ImplementationGuide/primary",
        "packageId": "example.primary",
        "version": "1.0.0",
        "status": "draft",
        "fhirVersion": ["4.0.1"],
        "definition": {
            "resource": [
                {"reference":{"reference":"CodeSystem/demo"}},
                {"reference":{"reference":"Patient/p"},"name":"Example patient","description":"Patient narrative"}
            ],
            "page": {
                "nameUrl":"index.html","title":"Home","generation":"markdown",
                "page":[{"nameUrl":"child.html","title":"Child","generation":"markdown"}]
            }
        }
    });
    let code_system = json!({
        "resourceType":"CodeSystem",
        "id":"demo",
        "url":"https://example.org/ig/CodeSystem/demo",
        "status":"draft",
        "content":"complete",
        "concept":[{"code":"a","display":"A","concept":[{"code":"b"}]}]
    });
    let patient = json!({"resourceType":"Patient","id":"p"});
    let generated = vec![primary.clone(), code_system];
    let examples = vec![patient];
    let ig_root = PathBuf::from("/ig");
    let vfs = BTreeMap::from([
        (
            ig_root.join("input/pagecontent/index.md"),
            b"# Home\n{% include snippet.md %}\n".to_vec(),
        ),
        (
            ig_root.join("input/pagecontent/child.md"),
            b"# Child\n".to_vec(),
        ),
        (
            ig_root.join("input/includes/snippet.md"),
            b"included\n".to_vec(),
        ),
        (ig_root.join("input/images/logo.png"), vec![1, 2, 3]),
    ]);
    let config = concat!(
        "id: primary\n",
        "canonical: https://example.org/ig\n",
        "name: PrimaryIG\n",
        "status: draft\n",
        "version: 1.0.0\n",
        "fhirVersion: 4.0.1\n",
        "menu:\n",
        "  Home: index.html\n",
        "  Group:\n",
        "    Child: child.html\n",
    );
    let inputs = site_db::InMemoryInputs {
        generated: &generated,
        primary_implementation_guide: &primary,
        examples: &examples,
        sushi_config_yaml: config,
        build_epoch_secs: 1_700_000_000,
        branch: None,
        revision: None,
        vfs,
        ig_root,
        liquid_asset_rel_dirs: vec!["input/includes".into()],
    };
    let prepared_only = site_db::prepare_from_inputs(&inputs).unwrap();
    let outcome = site_db::build_from_inputs(&inputs).unwrap();
    assert_eq!(prepared_only.prepared_guide, outcome.prepared_guide);

    let assets: BTreeMap<_, _> = outcome
        .prepared_guide
        .assets
        .iter()
        .map(|asset| (asset.path.as_str(), &asset.source_reads))
        .collect();
    assert_eq!(
        assets["snippet.md"],
        &BTreeSet::from([PreparedPath::parse("input/includes/snippet.md").unwrap()])
    );
    assert_eq!(
        assets["logo.png"],
        &BTreeSet::from([PreparedPath::parse("input/images/logo.png").unwrap()])
    );
    assert_eq!(outcome.db.assets.len(), outcome.prepared_guide.assets.len());

    // Independent compatibility oracle: recreate the old parallel row path
    // from the same semantic values, then prove the new one-way
    // PreparedGuide -> SiteDb projection is byte-for-byte identical.
    let cfg_yaml: serde_yaml::Value = serde_yaml::from_str(config).unwrap();
    let cfg: serde_json::Value = serde_yaml::from_value(cfg_yaml).unwrap();
    let resources: Vec<_> = outcome
        .prepared_guide
        .resources
        .iter()
        .map(|resource| resource.resource.clone())
        .collect();
    let mut resource_meta = std::collections::HashMap::new();
    for entry in primary
        .pointer("/definition/resource")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(reference) = entry
            .pointer("/reference/reference")
            .and_then(serde_json::Value::as_str)
        {
            resource_meta.insert(reference.to_string(), entry.clone());
        }
    }
    let identity = site_db::model::ResourceIdentity {
        resource_type: "ImplementationGuide".into(),
        id: "primary".into(),
    };
    let metadata = site_db::rows::derive_metadata_rows(&site_db::rows::MetadataInputs {
        cfg: &cfg,
        ig: &primary,
        gen_date: site_db::timefmt::gen_date(1_700_000_000),
        gen_day: site_db::timefmt::gen_day(1_700_000_000),
        build_epoch_secs: 1_700_000_000,
        branch: None,
        revision: None,
    });
    let json: Vec<_> = resources
        .iter()
        .map(|resource| serde_json::to_string(resource).unwrap())
        .collect();
    let (resource_rows, key_by_ref) =
        site_db::rows::derive_resource_rows(&resources, &resource_meta, &cfg, &json, &identity);
    let concepts = site_db::rows::derive_concept_rows(&resources, &key_by_ref);
    let mut legacy = site_db::SiteDb {
        primary_implementation_guide: Some(identity),
        ..Default::default()
    };
    site_db::rows::populate_core_rows(&mut legacy, metadata, resource_rows, concepts);
    let files = site_db::augment::MemFiles::new(inputs.vfs.clone());
    site_db::augment::augment(
        &mut legacy,
        &site_db::augment::AugmentInputs {
            ig: &primary,
            sushi_config_yaml: config,
            project_root: inputs.ig_root.clone(),
            pagecontent_dir: inputs.ig_root.join("input/pagecontent"),
            image_dir: inputs.ig_root.join("input/images"),
            liquid_asset_dirs: vec![inputs.ig_root.join("input/includes")],
            files: &files,
        },
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&outcome.db).unwrap(),
        serde_json::to_value(&legacy).unwrap(),
        "one-way PreparedGuide compatibility projection drifted from legacy rows"
    );
}
