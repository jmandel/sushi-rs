use serde::Serialize;
use thiserror::Error;

use crate::Sha256Digest;

/// Failure to produce the canonical JSON representation used for hashes.
#[derive(Debug, Error)]
pub enum CanonicalError {
    #[error("cannot represent SiteBuild value as JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Serialize as compact JSON after recursively sorting every object key.
///
/// Arrays retain their order because some sequences (for example a renderer's
/// declared precedence) may be meaningful. Unordered contract collections are
/// represented by `BTreeMap`/`BTreeSet` before reaching this function.
pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, CanonicalError> {
    let value = serde_json::to_value(value)?;
    serde_json::to_vec(&sort_json(value)).map_err(Into::into)
}

/// SHA-256 of [`canonical_json_bytes`].
pub fn sha256_canonical<T: Serialize>(value: &T) -> Result<Sha256Digest, CanonicalError> {
    let bytes = canonical_json_bytes(value)?;
    Ok(Sha256Digest::of_bytes(&bytes))
}

fn sort_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(sort_json).collect())
        }
        serde_json::Value::Object(values) => {
            let mut entries: Vec<_> = values.into_iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, sort_json(value)))
                    .collect(),
            )
        }
        scalar => scalar,
    }
}
