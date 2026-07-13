//! Canonical package-transport values shared by native/WASM hosts.
//!
//! These are transport observations and authenticated carrier metadata, not a
//! second package model. Package resolution and mounted package authority stay
//! in the ordinary package_store types.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
pub struct BundleInput {
    pub label: String,
    pub files: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PackageMountResult {
    pub mounted: u32,
    pub added: u32,
    pub packages: u32,
    pub manifest_json_bytes: u64,
    pub artifact_bytes: u64,
    pub retained_blob_bytes: u64,
    pub indexed_members: u64,
    pub member_body_copies: u64,
    pub manifest_parse_ms: f64,
    pub decode_validate_ms: f64,
    pub mount_ms: f64,
    #[serde(flatten)]
    #[cfg_attr(feature = "wire-contract", ts(flatten))]
    pub compression: crate::BundleCompressionMetrics,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PreparedStageResult {
    pub label: String,
    pub staged: u32,
    pub artifact_bytes: u64,
    pub indexed_members: u64,
    pub decode_validate_ms: f64,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PreparedExport {
    pub label: String,
    pub cache_key: String,
    pub artifact_sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PrepareMountResult {
    pub mounted: u32,
    pub added: u32,
    pub artifacts: Vec<PreparedExport>,
    pub artifact_bytes: u64,
    pub prepared_members: u64,
    pub input_json_bytes: u64,
    pub base64_bytes: u64,
    pub decoded_source_bytes: u64,
    pub normalized_bytes: u64,
    pub mount_member_body_copies: u64,
    pub json_parse_ms: f64,
    pub base64_decode_ms: f64,
    pub normalization_ms: f64,
    pub indexing_ms: f64,
    pub artifact_encode_ms: f64,
    pub decode_validate_prepare_ms: f64,
    pub mount_ms: f64,
}
