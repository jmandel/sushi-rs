//! Minimal `sushi-config.yaml` parsing for export metadata defaults.
//!
//! SUSHI reads the IG configuration and uses `canonical`, `version`, `status`,
//! `FSHOnly`, etc. as defaults when building resource metadata
//! (`ValueSetExporter.setMetadata` / `CodeSystemExporter.setMetadata`). For
//! ValueSet/CodeSystem export we only need a small subset.

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    pub canonical: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, rename = "FSHOnly")]
    pub fsh_only: bool,
    #[serde(default = "default_true", rename = "applyExtensionMetadataToRoot")]
    pub apply_extension_metadata_to_root: bool,
    #[serde(default, rename = "fhirVersion")]
    pub fhir_version: Option<serde_yaml::Value>,
    #[serde(default, rename = "instanceOptions")]
    pub instance_options: Option<InstanceOptions>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InstanceOptions {
    #[serde(default, rename = "manualSliceOrdering")]
    pub manual_slice_ordering: bool,
    #[serde(default, rename = "setMetaProfile")]
    pub set_meta_profile: Option<String>,
    #[serde(default, rename = "setId")]
    pub set_id: Option<String>,
}

impl Config {
    /// `config.instanceOptions?.manualSliceOrdering ?? false`.
    pub fn manual_slice_ordering(&self) -> bool {
        self.instance_options
            .as_ref()
            .map(|o| o.manual_slice_ordering)
            .unwrap_or(false)
    }
}

fn default_true() -> bool {
    true
}

impl Config {
    pub fn from_yaml(text: &str) -> anyhow::Result<Config> {
        let mut cfg: Config = serde_yaml::from_str(text)?;
        // `canonical` may contain trailing whitespace in the wild (epi config).
        cfg.canonical = cfg.canonical.trim().to_string();
        if let Some(s) = &cfg.status {
            cfg.status = Some(s.trim().to_string());
        }
        Ok(cfg)
    }

    /// The effective `status` SUSHI assigns (`this.tank.config.status`).
    pub fn status(&self) -> &str {
        self.status.as_deref().unwrap_or("draft")
    }

    /// `tank.config.fhirVersion?.[0]` — the configured FHIR version string.
    /// In sushi-config.yaml `fhirVersion` may be a scalar (`4.0.1`) or a
    /// sequence; we take the first element / the scalar.
    pub fn fhir_version(&self) -> Option<String> {
        match self.fhir_version.as_ref()? {
            serde_yaml::Value::String(s) => Some(s.trim().to_string()),
            serde_yaml::Value::Sequence(seq) => seq
                .first()
                .and_then(|v| v.as_str().map(|s| s.trim().to_string())),
            _ => None,
        }
    }
}
