//! package_store: the READ side of fhir-package-loader that SUSHI's
//! `FHIRDefinitions` exposes. Resolves a project's FHIR dependency graph from a
//! local package cache and fishes resources by canonical/id/name/type.
//!
//! HARD RULE: the cache dir is ALWAYS explicit (never default to ~/.fhir).
//! See docs/specs/{06-package-fhirdefs.md,package-store-notes.md}.
//! Gate: `harness/package-oracle.cjs` (run under the isolated cache).

use serde_json::Value;

/// Fishing type (mirrors `sushi-ts/src/utils/Fishable.ts` `Type`). Only the
/// variants the package side can return are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FishType {
    Resource,
    Type,
    Profile,
    Extension,
    ValueSet,
    CodeSystem,
    Logical,
    Instance,
}

/// The default search order used by SUSHI's untyped `fishForFHIR`.
pub const ALL_FISH_TYPES: &[FishType] = &[
    FishType::Resource,
    FishType::Type,
    FishType::Profile,
    FishType::Extension,
    FishType::ValueSet,
    FishType::CodeSystem,
    FishType::Logical,
];

/// Reads package `.index.json` files and resolves canonical/id/name → resource.
pub struct PackageStore;

impl PackageStore {
    /// Resolve the project's dependency graph (auto FHIR core for `fhirVersion`,
    /// `sushi-config.yaml` `dependencies`, and transitive `package.json` deps),
    /// then index every package under `cache_dir`. `cache_dir` MUST be explicit.
    ///
    /// IMPLEMENTATION PENDING.
    pub fn for_project(_ig_dir: &str, _cache_dir: &str) -> anyhow::Result<Self> {
        anyhow::bail!("package_store::for_project: under construction")
    }

    /// `fishForFHIR(item, ...types)` — returns the full resource JSON.
    pub fn fish_for_fhir(&self, _item: &str, _types: &[FishType]) -> Option<Value> {
        None
    }

    /// `fishForMetadata(item, ...types)` — id/name/sdType/url/parent/abstract/version/...
    pub fn fish_for_metadata(&self, _item: &str, _types: &[FishType]) -> Option<Value> {
        None
    }
}
