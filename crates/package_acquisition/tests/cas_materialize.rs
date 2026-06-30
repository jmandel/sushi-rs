use package_acquisition::{Coordinate, PackageCas, PackageLock, SourceKind};
use std::fs;

#[test]
fn ingest_local_directory_and_materialize_cache_shape() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("src");
    let package = source.join("package");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("package.json"),
        r#"{"name":"example.fhir.pkg","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(
        package.join("StructureDefinition-Test.json"),
        r#"{"resourceType":"StructureDefinition","id":"Test","url":"http://example.org/StructureDefinition/Test","kind":"resource","type":"Patient","derivation":"constraint"}"#,
    )
    .unwrap();

    let cas = PackageCas::new(temp.path().join("cas"));
    let coord = Coordinate::parse("example.fhir.pkg#1.0.0").unwrap();
    let package_ref = cas.ingest_local_source(&coord, &source).unwrap();
    assert_eq!(package_ref.source, SourceKind::LocalDirectory);
    assert_eq!(package_ref.materialized_label(), "example.fhir.pkg#1.0.0");
    assert_eq!(package_ref.sha256.len(), 64);

    let out = temp.path().join("cache");
    let materialized = cas.materialize_package(&coord, &out).unwrap();
    assert_eq!(materialized.sha256, package_ref.sha256);
    assert!(out
        .join("example.fhir.pkg#1.0.0")
        .join("package")
        .join("package.json")
        .is_file());
    assert!(out
        .join("example.fhir.pkg#1.0.0")
        .join("package")
        .join("StructureDefinition-Test.json")
        .is_file());
    let index: serde_json::Value = serde_json::from_slice(
        &fs::read(
            out.join("example.fhir.pkg#1.0.0")
                .join("package")
                .join(".index.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        index["files"][0]["filename"],
        "StructureDefinition-Test.json"
    );
    assert_eq!(index["files"][0]["derivation"], "constraint");
}

#[test]
fn materialize_lock_uses_locked_digest_without_network() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("src");
    let package = source.join("package");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("package.json"),
        r#"{"name":"example.fhir.pkg","version":"2.0.0"}"#,
    )
    .unwrap();
    fs::write(
        package.join("ValueSet-Test.json"),
        r#"{"resourceType":"ValueSet","id":"Test","url":"http://example.org/ValueSet/Test","status":"draft"}"#,
    )
    .unwrap();

    let cas = PackageCas::new(temp.path().join("cas"));
    let coord = Coordinate::parse("example.fhir.pkg#2.0.0").unwrap();
    let package_ref = cas.ingest_local_source(&coord, &source).unwrap();
    let lock = PackageLock {
        lockfile_version: 1,
        generated_at_unix: 1,
        packages: vec![package_ref.clone()],
    };
    let lock_path = temp.path().join("fhir-deps.lock");
    lock.write(&lock_path).unwrap();

    let cas_entry = cas.root().join("packages").join(&package_ref.sha256);
    make_writable(&cas_entry);
    fs::remove_dir_all(&cas_entry).unwrap();

    let offline = temp.path().join("offline-cache");
    assert!(cas
        .materialize_lock_with_options(&lock_path, &offline, true)
        .is_err());

    let out = temp.path().join("cache");
    let read_back = cas.materialize_lock(&lock_path, &out).unwrap();
    assert_eq!(read_back.packages.len(), 1);
    assert_eq!(read_back.packages[0].sha256, package_ref.sha256);
    assert!(cas_entry.is_dir());
    assert!(out
        .join("example.fhir.pkg#2.0.0")
        .join("package")
        .join("ValueSet-Test.json")
        .is_file());
}

fn make_writable(path: &std::path::Path) {
    if !path.exists() {
        return;
    }
    if path.is_dir() {
        for ent in fs::read_dir(path).unwrap() {
            make_writable(&ent.unwrap().path());
        }
    }
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_readonly(false);
    fs::set_permissions(path, perms).unwrap();
}
