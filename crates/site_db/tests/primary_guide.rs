use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;

#[test]
fn explicit_primary_guide_survives_additional_generated_ig_instances() {
    let primary = json!({
        "resourceType": "ImplementationGuide",
        "id": "primary",
        "url": "https://example.org/ig/ImplementationGuide/primary",
        "packageId": "example.primary",
        "version": "1.0.0",
        "status": "draft",
        "fhirVersion": ["4.0.1"],
        "definition": { "resource": [] }
    });
    let example = json!({
        "resourceType": "ImplementationGuide",
        "id": "aaa-example",
        "status": "draft"
    });
    let generated = vec![example, primary.clone()];
    let outcome = site_db::build_from_inputs(&site_db::InMemoryInputs {
        generated: &generated,
        primary_implementation_guide: &primary,
        examples: &[],
        sushi_config_yaml: concat!(
            "id: primary\n",
            "canonical: https://example.org/ig\n",
            "name: PrimaryIG\n",
            "status: draft\n",
            "version: 1.0.0\n",
            "fhirVersion: 4.0.1\n",
        ),
        build_epoch_secs: 1_700_000_000,
        branch: None,
        revision: None,
        vfs: BTreeMap::new(),
        ig_root: PathBuf::from("/ig"),
        liquid_asset_rel_dirs: Vec::new(),
    })
    .unwrap();

    assert_eq!(
        outcome.db.primary_implementation_guide,
        Some(site_db::model::ResourceIdentity {
            resource_type: "ImplementationGuide".into(),
            id: "primary".into(),
        })
    );
    assert_eq!(
        outcome
            .db
            .resources
            .iter()
            .filter(|row| row.type_ == "ImplementationGuide")
            .count(),
        2
    );
}
