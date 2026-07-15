//! Transport-neutral progress and failure vocabulary for the four build
//! operations. These values report execution; they never participate in build
//! identity or functional results.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum BuildStage {
    Wasm,
    Manifest,
    ProjectCacheHit,
    ProjectVerify,
    ProjectUnpack,
    ProjectStore,
    Compile,
    Snapshot,
    SiteBuild,
    PreviewPublish,
    Resolve,
    BundleFetch,
    BundleCacheHit,
    BundleUnpack,
    BundleMount,
    RegistryFetch,
    PackageBlocked,
    Ready,
    LazyFetch,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "wire-contract", ts(optional_fields))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildEvent {
    /// Functional operation when this observation belongs to an immutable
    /// Build. Lifecycle/package/project-source events omit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "BuildOperation"))]
    pub operation: Option<BuildOperation>,
    /// Immutable build identity when one exists. Acquisition events that occur
    /// before prepare establishes an identity omit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
    pub build_id: Option<String>,
    /// Stable machine-readable phase within the operation. `message` remains
    /// presentation text and must not be parsed by benchmarks.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
    pub phase: Option<String>,
    /// Clock domain which measured this event. Browser hosts align Window and
    /// Worker clocks through `performance.timeOrigin`; native hosts may omit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "BuildEventSource"))]
    pub source: Option<BuildEventSource>,
    /// Unix-epoch milliseconds at the beginning of this measured span. This is
    /// observational only and never participates in build identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "f64"))]
    pub start_ms: Option<f64>,
    pub stage: BuildStage,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", ts(optional, type = "number"))]
    #[cfg_attr(feature = "wire-contract", schemars(with = "u64"))]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", ts(optional, type = "number"))]
    #[cfg_attr(feature = "wire-contract", schemars(with = "u64"))]
    pub total_bytes: Option<u64>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "f64"))]
    pub fraction: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "bool"))]
    pub from_cache: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "f64"))]
    pub duration_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", ts(optional, type = "number"))]
    #[cfg_attr(feature = "wire-contract", schemars(with = "u64"))]
    pub input_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", ts(optional, type = "number"))]
    #[cfg_attr(feature = "wire-contract", schemars(with = "u64"))]
    pub output_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", ts(optional, type = "number"))]
    #[cfg_attr(feature = "wire-contract", schemars(with = "u64"))]
    pub file_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "BTreeMap<String, f64>"))]
    pub metrics: Option<BTreeMap<String, f64>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum BuildEventSource {
    Window,
    Worker,
    Rust,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum BuildOperation {
    Lifecycle,
    Prepare,
    Outputs,
    Render,
    Finalize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum BuildErrorPhase {
    Lifecycle,
    Input,
    PackageResolution,
    PackageTransport,
    Compilation,
    Preparation,
    Renderer,
    ContentStore,
    Publication,
    Finalization,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum BuildErrorCode {
    InvalidInput,
    Unavailable,
    Integrity,
    CompileFailed,
    RendererFailed,
    UnknownBuild,
    Cancelled,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "wire-contract", ts(optional_fields))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildError<C> {
    pub operation: BuildOperation,
    pub phase: BuildErrorPhase,
    pub code: BuildErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "C"))]
    pub successful_compilation: Option<C>,
}

impl<C> BuildError<C> {
    pub fn new(
        operation: BuildOperation,
        phase: BuildErrorPhase,
        code: BuildErrorCode,
        message: impl Into<String>,
    ) -> Self {
        Self {
            operation,
            phase,
            code,
            message: message.into(),
            retryable: false,
            successful_compilation: None,
        }
    }

    pub fn with_successful_compilation(mut self, compilation: C) -> Self {
        self.successful_compilation = Some(compilation);
        self
    }

    pub fn map_compilation<D>(self, map: impl FnOnce(C) -> D) -> BuildError<D> {
        BuildError {
            operation: self.operation,
            phase: self.phase,
            code: self.code,
            message: self.message,
            retryable: self.retryable,
            successful_compilation: self.successful_compilation.map(map),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_build_event_observations_are_omitted() {
        let event = BuildEvent {
            operation: None,
            build_id: None,
            phase: None,
            source: None,
            start_ms: None,
            stage: BuildStage::Compile,
            label: None,
            bytes: None,
            total_bytes: None,
            message: "Compiling.".into(),
            fraction: None,
            from_cache: None,
            duration_ms: None,
            input_bytes: None,
            output_bytes: None,
            file_count: None,
            metrics: None,
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({ "stage": "compile", "message": "Compiling." })
        );
    }

    #[test]
    fn build_event_serializes_aligned_machine_span() {
        let event = BuildEvent {
            operation: Some(BuildOperation::Lifecycle),
            build_id: None,
            phase: Some("engine.session.init".into()),
            source: Some(BuildEventSource::Worker),
            start_ms: Some(1234.5),
            stage: BuildStage::Wasm,
            label: None,
            bytes: None,
            total_bytes: None,
            message: "Initialized compiler session.".into(),
            fraction: None,
            from_cache: None,
            duration_ms: Some(7.25),
            input_bytes: None,
            output_bytes: None,
            file_count: None,
            metrics: None,
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({
                "operation": "lifecycle",
                "phase": "engine.session.init",
                "source": "worker",
                "startMs": 1234.5,
                "stage": "wasm",
                "message": "Initialized compiler session.",
                "durationMs": 7.25
            })
        );
    }
}

impl<C> std::fmt::Display for BuildError<C> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl<C: std::fmt::Debug> std::error::Error for BuildError<C> {}
