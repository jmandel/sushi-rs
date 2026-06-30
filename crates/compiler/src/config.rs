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
}
