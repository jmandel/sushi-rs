use std::fs;

#[test]
fn configured_primary_guide_does_not_hide_additional_guides() {
    let root = std::env::temp_dir().join(format!(
        "site-producer-primary-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(root.join("template")).unwrap();
    fs::create_dir_all(root.join("fsh-generated/resources")).unwrap();
    fs::create_dir_all(root.join("input/resources")).unwrap();
    fs::write(root.join("template/config.json"), r#"{"defaults":{}}"#).unwrap();
    fs::write(root.join("sushi-config.yaml"), "id: primary\n").unwrap();
    fs::write(
        root.join("fsh-generated/resources/ImplementationGuide-primary.json"),
        r#"{"resourceType":"ImplementationGuide","id":"primary","packageId":"example.primary","url":"https://example.org/ImplementationGuide/primary","definition":{"resource":[]}}"#,
    )
    .unwrap();
    fs::write(
        root.join("input/resources/ImplementationGuide-aaa-example.json"),
        r#"{"resourceType":"ImplementationGuide","id":"aaa-example","status":"draft"}"#,
    )
    .unwrap();
    fs::write(
        root.join("input/resources/ImplementationGuide-primary.json"),
        r#"{"resourceType":"ImplementationGuide","id":"primary","status":"draft"}"#,
    )
    .unwrap();

    let inputs = site_producer::gather_inputs(&root).unwrap();
    assert_eq!(inputs.ig_json["id"], "primary");
    assert!(inputs
        .resources
        .iter()
        .any(|resource| resource.rt == "ImplementationGuide" && resource.id == "aaa-example"));
    assert!(!inputs
        .resources
        .iter()
        .any(|resource| resource.rt == "ImplementationGuide" && resource.id == "primary"));

    fs::remove_dir_all(root).unwrap();
}
