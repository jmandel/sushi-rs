use std::fs;

use content_store::{
    ContentRef, ContentStore, FileContentStore, Sha256Digest, StoreError, VerificationError,
};

#[test]
fn round_trip_preserves_exact_reference_and_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileContentStore::create(temp.path().join("objects")).unwrap();
    let content = ContentRef::of_bytes(b"hello", Some("text/plain; charset=utf-8"));

    store.put(&content, b"hello").unwrap();
    let loaded = store.read(&content).unwrap();

    assert_eq!(loaded.content_ref(), &content);
    assert_eq!(loaded.bytes(), b"hello");
    assert!(store.contains(&content).unwrap());
}

#[test]
fn publication_rejects_digest_length_and_media_mismatches() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileContentStore::create(temp.path()).unwrap();
    let correct = ContentRef::of_bytes(b"hello", Some("text/plain"));

    let mut wrong_length = correct.clone();
    wrong_length.byte_length += 1;
    assert!(matches!(
        store.put(&wrong_length, b"hello"),
        Err(StoreError::Verification(VerificationError::Length { .. }))
    ));

    let mut wrong_digest = correct.clone();
    wrong_digest.sha256 = Sha256Digest::of_bytes(b"other");
    assert!(matches!(
        store.put(&wrong_digest, b"hello"),
        Err(StoreError::Verification(VerificationError::Digest { .. }))
    ));

    let mut invalid_media = correct;
    invalid_media.media_type = Some("  ".into());
    assert!(matches!(
        store.put(&invalid_media, b"hello"),
        Err(StoreError::Verification(
            VerificationError::InvalidMediaType
        ))
    ));
}

#[test]
fn reads_detect_corruption_and_do_not_replace_it() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileContentStore::create(temp.path()).unwrap();
    let content = ContentRef::of_bytes(b"expected", None::<String>);
    fs::write(store.object_path(&content.sha256), b"corrupt!").unwrap();

    assert!(matches!(
        store.read(&content),
        Err(StoreError::Verification(VerificationError::Digest { .. }))
    ));
    assert!(store.put(&content, b"expected").is_err());
    assert_eq!(
        fs::read(store.object_path(&content.sha256)).unwrap(),
        b"corrupt!"
    );
}

#[test]
#[cfg(unix)]
fn rejects_symlink_objects() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let store = FileContentStore::create(temp.path().join("objects")).unwrap();
    let content = ContentRef::of_bytes(b"elsewhere", None::<String>);
    let elsewhere = temp.path().join("elsewhere");
    fs::write(&elsewhere, b"elsewhere").unwrap();
    symlink(&elsewhere, store.object_path(&content.sha256)).unwrap();

    assert!(matches!(
        store.read(&content),
        Err(StoreError::InvalidObject(_))
    ));
}

#[test]
fn serde_wire_shape_is_stable() {
    let content = ContentRef::of_bytes(b"x", Some("application/octet-stream"));
    let json = serde_json::to_value(&content).unwrap();
    assert_eq!(json["byteLength"], 1);
    assert_eq!(json["mediaType"], "application/octet-stream");
    assert_eq!(json["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(serde_json::from_value::<ContentRef>(json).unwrap(), content);
}
