//! Canonical loaded-StructureDefinition dependency projection.
//!
//! The actual load operation is owned by `PackageContext`, because it alone
//! knows the selected resource, local/package load mode, owning package, and
//! authenticated carrier. Fresh snapshot resolution and manifest revalidation
//! both call that one operation. This module owns the canonical conversion
//! helpers and the exact post-load semantic projection retained as evidence.

use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) use crate::walk::resolve::{fix_loaded_resource, lenient_r5_read_r4, to_r5_internal};

const SNAPSHOT_LOADED_SD_SCHEMA: &[u8] = b"snapshot-gen.loaded-sd-input/v1";
const SNAPSHOT_LOADED_SD_FIELDS: &[&str] = &[
    "resourceType",
    "id",
    "url",
    "version",
    "name",
    "fhirVersion",
    "kind",
    "abstract",
    "type",
    "baseDefinition",
    "derivation",
    "snapshot",
    "differential",
];

struct SnapshotLoadedProjection<'a>(&'a Value);

impl Serialize for SnapshotLoadedProjection<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let Some(object) = self.0.as_object() else {
            return self.0.serialize(serializer);
        };
        let retained = object
            .keys()
            .filter(|field| SNAPSHOT_LOADED_SD_FIELDS.contains(&field.as_str()))
            .count();
        let mut map = serializer.serialize_map(Some(retained))?;
        for (field, value) in object {
            if SNAPSHOT_LOADED_SD_FIELDS.contains(&field.as_str()) {
                map.serialize_entry(field, value)?;
            }
        }
        map.end()
    }
}

pub(crate) fn snapshot_dependency_digest(value: &Value) -> [u8; 32] {
    let mut bytes = SNAPSHOT_LOADED_SD_SCHEMA.to_vec();
    bytes.push(0);
    serde_json::to_writer(&mut bytes, &SnapshotLoadedProjection(value))
        .expect("serde_json::Value always serializes");
    Sha256::digest(bytes).into()
}
