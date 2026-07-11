//! `resolve` — the ONE Rust package-resolution API, native-first and wasm-clean.
//!
//! This is the single home for BOTH resolution sets the editor / snapshot pipeline
//! need, so there is no resolution logic outside Rust (task #32 DRY mandate):
//!
//! - **compile_set** — stock SUSHI's COMPILE load set: configured deps + automatic
//!   deps + FHIR core, in stock load order, NON-transitive. This is exactly
//!   [`resolve_load_order_with`] (the same function `PackageStore::for_project`
//!   drives), reused here — never forked. Verified against stock SUSHI; see
//!   AGENTS.md §Phase-1.
//!
//! - **context_closure** — the SNAPSHOT / RENDER context set: the TRANSITIVE
//!   `package.json` dependency closure rooted at every exact compile-set member,
//!   including automatic tools/terminology/extensions packages, walked over the
//!   *currently mounted* packages with an R4-compatibility filter and a
//!   `4.0.0 -> 4.0.1` core canonicalization. The traversal rules port
//!   `snapshot/package-deps.cjs`; using the full compile set adapts its published-
//!   package root to an in-memory project whose automatic inputs are not listed
//!   in `sushi-config.yaml`.
//!
//! The API is **iterative by design**: the transitive dependencies of a package
//! that is not yet mounted are unknowable until it is mounted. So the host loop is
//! `resolve -> fetch missing -> mount -> resolve -> ...` to a fixpoint. Callers
//! feed the current mounted [`PackageSource`] each round; [`ResolutionStep::missing`]
//! lists exactly the coordinates to fetch next, and [`ResolutionStep::satisfied`]
//! reports whether the closure is complete (nothing left to fetch).
//!
//! `latest`/`current`/`dev` and `M.N.x` version selection is resolved against an
//! optional host-supplied [`VersionIndex`] (data in, decisions in Rust). With no
//! index and a `latest`/`current` request the resolver records a PRECISE error in
//! [`ResolutionStep::missing`] (never a silent guess).

use crate::source::PackageSource;
use crate::{parse_config_text, resolve_load_order_with, PackageRequest, ProjectConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// A host-supplied index of the versions available for a package id, used to
/// resolve `latest` / `current` / `M.N.x` requests without the resolver doing any
/// I/O of its own (data in, decisions in Rust). The host populates this from
/// whatever it can see — a registry manifest, the set of cached/mounted dirs, a
/// committed lockfile — and the resolver picks the winning version by the SAME
/// rule stock SUSHI/FPL use (highest semver, release over pre-release).
///
/// Absent an entry for a package whose request needs resolution, the resolver
/// reports a precise `missing` reason rather than guessing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VersionIndex {
    /// package id -> the list of available version strings (order irrelevant; the
    /// resolver sorts).
    #[serde(default)]
    pub versions: BTreeMap<String, Vec<String>>,
}

impl VersionIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the versions available for a package id.
    pub fn insert(&mut self, id: impl Into<String>, versions: Vec<String>) {
        self.versions.insert(id.into(), versions);
    }

    fn get(&self, id: &str) -> Option<&[String]> {
        self.versions.get(id).map(Vec::as_slice)
    }
}

/// One resolvable package coordinate the host must acquire + mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingPackage {
    pub package_id: String,
    /// The concrete version the resolver selected (if it could resolve one), or the
    /// raw requested coordinate when it could not (`latest`/`current` with no index).
    pub version: String,
    /// Why this package is in `missing`: the exact, host-renderable reason.
    pub reason: MissingReason,
    /// The set that referenced it (`compile` or `context`), for the host's UI.
    pub set: RequestedSet,
}

/// Which resolution set surfaced a missing package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestedSet {
    /// Needed for the SUSHI compile load (configured/auto deps + core).
    Compile,
    /// Needed for the transitive snapshot/render context closure.
    Context,
}

/// The precise reason a package could not be satisfied from the mounted set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MissingReason {
    /// The coordinate is fully resolved (concrete version) but not yet mounted —
    /// the host should fetch + mount it, then resolve again.
    NotMounted,
    /// The request needs version resolution (`latest`/`current`/`M.N.x`) and no
    /// host [`VersionIndex`] entry (or no matching version) was available. The host
    /// must supply an index (registry query) and resolve again. NEVER a guess.
    UnresolvedVersion { requested: String },
}

/// One iteration's answer: the two well-defined sets, what is still missing, and
/// whether the closure is complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionStep {
    /// Schema for the resolver semantics represented by this result. Hosts may
    /// persist an exact closure as an optimization, but must discard it when
    /// this value changes and must still re-run this resolver before use.
    pub resolver_schema: u32,
    /// Stock-SUSHI COMPILE load set (non-transitive), in stock load order. Every
    /// entry is a concrete `pkg#ver` the compile needs mounted.
    pub compile_set: Vec<PackageRequest>,
    /// Transitive snapshot/render context closure rooted at the exact compile
    /// set and walked over the MOUNTED packages (R4-compat filtered,
    /// core-canonicalized), with core first then discovery order.
    pub context_closure: Vec<PackageRequest>,
    /// Mounted package manifests read while proving `context_closure`, including
    /// packages later excluded by compatibility rules. Replaying a cached
    /// resolution must mount these witnesses too; the final closure alone is
    /// not sufficient to prove that an excluded dependency remains excluded.
    pub resolution_support: Vec<PackageRequest>,
    /// Packages referenced by either set that are not yet mounted (or whose version
    /// could not be resolved). Fetch these, mount, and resolve again.
    pub missing: Vec<MissingPackage>,
    /// True iff nothing is missing — the mounted set fully satisfies both closures
    /// and the host loop has reached its fixpoint.
    pub satisfied: bool,
    /// Every non-concrete version request which influenced this step. A cached
    /// closure is not fresh merely because its concrete packages still exist:
    /// the host must refresh the candidate universe for these requests before
    /// asking Rust to verify the closure again.
    pub mutable_requests: Vec<MutableVersionRequest>,
}

/// A version request whose answer can change without the project config
/// changing (`latest`, `current`, `dev`, `x`, or `*`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutableVersionRequest {
    pub package_id: String,
    pub requested: String,
    pub resolved_version: Option<String>,
    pub set: RequestedSet,
}

pub const RESOLVER_SCHEMA: u32 = 3;

impl ResolutionStep {
    /// Serialize to the JSON string the wasm surface returns.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("ResolutionStep serializes")
    }
}

// ---------------------------------------------------------------------------
// Version resolution (shared rule for `latest`/`current`/`M.N.x`)
// ---------------------------------------------------------------------------

/// Does a version string require resolution against a set of candidates?
/// (`package-deps.cjs` `needsVersionResolution`, lines 38-40: `latest`, `current`,
/// a `x`/`*` wildcard segment.)
fn needs_version_resolution(version: &str) -> bool {
    if version == "latest" || version == "current" || version == "dev" {
        return true;
    }
    // /(^|[.])x($|[.])|\*/i — an `x` segment or any `*`.
    if version.contains('*') {
        return true;
    }
    version.split('.').any(|seg| seg.eq_ignore_ascii_case("x"))
}

/// Canonicalize a coordinate exactly as `package-deps.cjs` `canonicalVersion`
/// (lines 42-45): `hl7.fhir.r4.core#4.0.0 -> 4.0.1`.
fn canonical_version(id: &str, version: &str) -> String {
    if id == "hl7.fhir.r4.core" && version == "4.0.0" {
        "4.0.1".to_string()
    } else {
        version.to_string()
    }
}

/// Does a concrete `version` satisfy a `pattern` (which may carry `x`/`*` segments)?
/// Port of `package-deps.cjs` `versionMatches` (lines 47-57).
fn version_matches(version: &str, pattern: &str) -> bool {
    if pattern == "latest" || pattern == "current" {
        return true;
    }
    let parts: Vec<&str> = pattern.split('.').collect();
    let vparts: Vec<&str> = version.split('.').collect();
    for (i, part) in parts.iter().enumerate() {
        let lower = part.to_ascii_lowercase();
        if lower == "x" || lower == "*" {
            return true;
        }
        if vparts.get(i) != Some(part) {
            return false;
        }
    }
    true
}

/// Compare two version strings exactly as `package-deps.cjs` `compareVersions`
/// (lines 59-74): split on `.`/`-`, numeric segments compared numerically (a
/// numeric segment outranks a non-numeric one at the same position), else lexical.
/// Returns Ordering of `l` vs `r`.
fn compare_versions(l: &str, r: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let lp: Vec<&str> = l.split(['.', '-']).collect();
    let rp: Vec<&str> = r.split(['.', '-']).collect();
    let len = lp.len().max(rp.len());
    for i in 0..len {
        let a = lp.get(i).copied().unwrap_or("0");
        let b = rp.get(i).copied().unwrap_or("0");
        let an = a
            .parse::<i64>()
            .ok()
            .filter(|_| a.bytes().all(|c| c.is_ascii_digit()));
        let bn = b
            .parse::<i64>()
            .ok()
            .filter(|_| b.bytes().all(|c| c.is_ascii_digit()));
        match (an, bn) {
            (Some(x), Some(y)) if x != y => return x.cmp(&y),
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            _ => {
                if a != b {
                    return a.cmp(b);
                }
            }
        }
    }
    Ordering::Equal
}

/// Resolve a `(id, requested)` coordinate to a concrete version using the host
/// [`VersionIndex`] when resolution is needed. Mirrors `package-deps.cjs`
/// `resolveSpec` (lines 76-89) but takes its candidate list from the host index
/// instead of a `readdirSync`.
///
/// Returns:
/// - `Ok(version)` — a concrete version (either the request was already concrete
///   after canonicalization, or the index yielded a match).
/// - `Err(requested)` — resolution was needed but no index entry / no match.
pub fn resolve_version(
    index: Option<&VersionIndex>,
    id: &str,
    requested: &str,
) -> Result<String, String> {
    let requested = canonical_version(id, requested);
    if !needs_version_resolution(&requested) {
        return Ok(requested);
    }
    let candidates = index.and_then(|ix| ix.get(id)).unwrap_or(&[]);
    let mut matches: Vec<&String> = candidates
        .iter()
        .filter(|c| version_matches(c, &requested))
        .collect();
    matches.sort_by(|a, b| compare_versions(a, b));
    match matches.last() {
        Some(v) => Ok((*v).clone()),
        None => Err(requested),
    }
}

// ---------------------------------------------------------------------------
// package.json reading over a mounted source (the `<cache>/<id>#<ver>/package`
// layout the BundleSource / DiskSource share)
// ---------------------------------------------------------------------------

/// The subset of `package.json` the closure walk reads.
struct PkgJson {
    fhir_versions: Vec<String>,
    dependencies: Vec<(String, String)>,
}

/// Read + parse `<cache>/<id>#<ver>/package/package.json` through the mounted
/// source, if present. Returns `None` when the package is not mounted (or has no
/// readable `package.json`) — that is exactly the "unknowable until mounted" case.
fn read_package_json(source: &dyn PackageSource, cache: &Path, label: &str) -> Option<PkgJson> {
    let path = cache.join(label).join("package").join("package.json");
    let bytes = source.read(&path).ok()?;
    let json: Value = serde_json::from_slice(&bytes).ok()?;
    let fhir_versions = json
        .get("fhirVersions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut dependencies = Vec::new();
    if let Some(obj) = json.get("dependencies").and_then(Value::as_object) {
        for (id, ver) in obj {
            let v = match ver {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => continue,
            };
            dependencies.push((id.clone(), v));
        }
    }
    Some(PkgJson {
        fhir_versions,
        dependencies,
    })
}

/// R4-compatibility filter — `package-deps.cjs` `isR4CompatiblePackage` (lines
/// 96-99): a package with no `fhirVersions` is kept; otherwise it is kept only if
/// SOME declared FHIR version starts with `4.` (R4 / R4B).
fn is_r4_compatible(pkg: &PkgJson) -> bool {
    if pkg.fhir_versions.is_empty() {
        return true;
    }
    pkg.fhir_versions.iter().any(|v| v.starts_with("4."))
}

// ---------------------------------------------------------------------------
// The public resolver
// ---------------------------------------------------------------------------

/// Resolve a project's two package sets against the currently mounted packages.
///
/// `config_text` is the `sushi-config.yaml` text (the SAME parse `PackageStore`
/// uses). `mounted` is the currently-mounted [`PackageSource`]; `cache_dir` is the
/// synthetic/real cache root the `<id>#<ver>/package` dirs hang under (for a
/// [`crate::BundleSource`], pass its `cache_root()`). `index` is the optional
/// host version index for `latest`/`current`/`M.N.x` resolution.
///
/// See the module docs for the iterative host loop this is designed to drive.
pub fn resolve_project(
    config_text: &str,
    mounted: &dyn PackageSource,
    cache_dir: &Path,
    index: Option<&VersionIndex>,
) -> anyhow::Result<ResolutionStep> {
    let cfg = parse_config_text(config_text)?;
    Ok(resolve_project_from_config(&cfg, mounted, cache_dir, index))
}

fn is_mounted(source: &dyn PackageSource, cache: &Path, label: &str) -> bool {
    source.is_dir(&cache.join(label).join("package"))
}

fn resolve_project_from_config(
    cfg: &ProjectConfig,
    mounted: &dyn PackageSource,
    cache: &Path,
    index: Option<&VersionIndex>,
) -> ResolutionStep {
    let mut missing: Vec<MissingPackage> = Vec::new();
    let mut mutable_requests: Vec<MutableVersionRequest> = Vec::new();

    // Record the mutable inputs independently of whether this particular round
    // can resolve them. This is freshness metadata for hosts which cache the
    // concrete closure; it is not an alternate source of resolver decisions.
    for dep in cfg.dependencies() {
        let Some(requested) = dep.version() else {
            continue;
        };
        let canonical = canonical_version(dep.package_id(), requested);
        if needs_version_resolution(&canonical) {
            push_mutable_request(
                &mut mutable_requests,
                MutableVersionRequest {
                    package_id: dep.package_id().to_string(),
                    requested: canonical.clone(),
                    resolved_version: resolve_version(index, dep.package_id(), &canonical).ok(),
                    set: RequestedSet::Compile,
                },
            );
        }
    }
    for (package_id, requested) in cfg.auto_dep_coordinates() {
        push_mutable_request(
            &mut mutable_requests,
            MutableVersionRequest {
                resolved_version: resolve_version(index, &package_id, &requested).ok(),
                package_id,
                requested,
                set: RequestedSet::Compile,
            },
        );
    }

    // ---- compile_set: stock non-transitive load order (reuse, do not fork). ----
    // The version resolver is the SHARED `resolve_version` over the host index; a
    // `latest`/`current` request with no index yields `None`, which the load-order
    // builder skips — we detect that skip here and surface a precise `missing`.
    let resolve_ver = |id: &str, ver: Option<&str>| -> Option<String> {
        match ver {
            Some(v) => resolve_version(index, id, v).ok(),
            // A `None` configured version is a stock error+skip; leave it out.
            None => None,
        }
    };
    let load_order = resolve_load_order_with(cfg, &resolve_ver);
    let compile_set: Vec<PackageRequest> = load_order
        .into_iter()
        .map(|(package_id, version)| PackageRequest {
            package_id,
            version,
        })
        .collect();

    // Record compile-set members that are unresolved (needed a version we could not
    // pick) or resolved-but-unmounted. `resolve_load_order_with` already dropped
    // unresolved auto-deps; re-derive the requested coordinates to report them.
    record_compile_missing(cfg, index, &compile_set, mounted, cache, &mut missing);

    for req in &compile_set {
        let label = format!("{}#{}", req.package_id, req.version);
        if !is_mounted(mounted, cache, &label) {
            push_missing(
                &mut missing,
                MissingPackage {
                    package_id: req.package_id.clone(),
                    version: req.version.clone(),
                    reason: MissingReason::NotMounted,
                    set: RequestedSet::Compile,
                },
            );
        }
    }

    // ---- context_closure: transitive package.json walk over MOUNTED packages. ----
    // Root at the exact compile set, walk transitively, R4-compat filter,
    // canonicalize core, prepend r4.core if the project is R4 and it is absent,
    // then core-first sort.
    let (context_closure, resolution_support, mut ctx_missing, ctx_mutable) =
        context_closure(cfg, &compile_set, mounted, cache, index);
    missing.append(&mut ctx_missing);
    for request in ctx_mutable {
        push_mutable_request(&mut mutable_requests, request);
    }

    // Dedup missing (a package can be referenced by both sets); keep first.
    dedup_missing(&mut missing);

    let satisfied = missing.is_empty();
    ResolutionStep {
        resolver_schema: RESOLVER_SCHEMA,
        compile_set,
        context_closure,
        resolution_support,
        missing,
        satisfied,
        mutable_requests,
    }
}

fn push_mutable_request(requests: &mut Vec<MutableVersionRequest>, request: MutableVersionRequest) {
    if !requests.iter().any(|existing| existing == &request) {
        requests.push(request);
    }
}

/// Re-derive the configured + auto-dep coordinates and report any whose version
/// could not be resolved (so the host knows to supply a [`VersionIndex`]).
fn record_compile_missing(
    cfg: &ProjectConfig,
    index: Option<&VersionIndex>,
    _compile_set: &[PackageRequest],
    _mounted: &dyn PackageSource,
    _cache: &Path,
    missing: &mut Vec<MissingPackage>,
) {
    // Configured deps with a version that needed resolution but could not be picked.
    for dep in cfg.dependencies() {
        let Some(v) = dep.version() else { continue };
        if needs_version_resolution(&canonical_version(dep.package_id(), v)) {
            if resolve_version(index, dep.package_id(), v).is_err() {
                push_missing(
                    missing,
                    MissingPackage {
                        package_id: dep.package_id().to_string(),
                        version: v.to_string(),
                        reason: MissingReason::UnresolvedVersion {
                            requested: v.to_string(),
                        },
                        set: RequestedSet::Compile,
                    },
                );
            }
        }
    }
    // Automatic deps request `latest`; without an index they are unresolvable.
    // resolve_load_order_with drops them silently, so surface them here.
    for (id, requested) in cfg.auto_dep_coordinates() {
        if resolve_version(index, &id, &requested).is_err() {
            push_missing(
                missing,
                MissingPackage {
                    package_id: id,
                    version: requested.clone(),
                    reason: MissingReason::UnresolvedVersion { requested },
                    set: RequestedSet::Compile,
                },
            );
        }
    }
}

/// The transitive closure walk. Returns `(closure, missing)` where `missing`
/// carries packages the walk reached but could not read (unmounted) or resolve.
fn context_closure(
    cfg: &ProjectConfig,
    compile_set: &[PackageRequest],
    mounted: &dyn PackageSource,
    cache: &Path,
    index: Option<&VersionIndex>,
) -> (
    Vec<PackageRequest>,
    Vec<PackageRequest>,
    Vec<MissingPackage>,
    Vec<MutableVersionRequest>,
) {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut missing: Vec<MissingPackage> = Vec::new();
    let mut mutable_requests: Vec<MutableVersionRequest> = Vec::new();
    let mut resolution_support: Vec<PackageRequest> = Vec::new();

    // `add(spec)` — `package-deps.cjs` lines 104-113: skip if seen, read
    // package.json, R4-compat filter, push, recurse over its dependencies.
    // Implemented iteratively with an explicit work stack to stay wasm-safe.
    let mut stack: Vec<(String, String)> = Vec::new();

    // Every exact compile input is a context root. This is broader than merely
    // seeding configured dependencies: SUSHI's automatic tools/terminology/
    // extensions packages are executable compiler inputs whose manifests can
    // carry exact transitive edges needed by snapshot/render preparation. A
    // project such as Cycle has no configured dependencies, so omitting these
    // roots would incorrectly report a core-only satisfied closure without ever
    // reading tools.r4's exact extensions.r4#5.2.0 dependency.
    //
    // Push in reverse so the explicit LIFO walk observes compile load order.
    for request in compile_set.iter().rev() {
        stack.push((request.package_id.clone(), request.version.clone()));
    }

    // Mutable requests that selected context roots remain explicit freshness
    // inputs in both the compile and context sets.
    for dep in cfg.dependencies() {
        let Some(v) = dep.version() else { continue };
        let canonical = canonical_version(dep.package_id(), v);
        if needs_version_resolution(&canonical) {
            push_mutable_request(
                &mut mutable_requests,
                MutableVersionRequest {
                    package_id: dep.package_id().to_string(),
                    requested: canonical,
                    resolved_version: resolve_version(index, dep.package_id(), v).ok(),
                    set: RequestedSet::Context,
                },
            );
        }
    }
    for (package_id, requested) in cfg.auto_dep_coordinates() {
        push_mutable_request(
            &mut mutable_requests,
            MutableVersionRequest {
                resolved_version: resolve_version(index, &package_id, &requested).ok(),
                package_id,
                requested,
                set: RequestedSet::Context,
            },
        );
    }

    while let Some((id, version)) = stack.pop() {
        let label = format!("{id}#{version}");
        if !seen.insert(label.clone()) {
            continue;
        }
        let Some(pkg) = read_package_json(mounted, cache, &label) else {
            // Not mounted yet: the host must fetch it, then resolve again. Its own
            // transitive deps are unknowable until then.
            push_missing(
                &mut missing,
                MissingPackage {
                    package_id: id.clone(),
                    version: version.clone(),
                    reason: MissingReason::NotMounted,
                    set: RequestedSet::Context,
                },
            );
            continue;
        };
        resolution_support.push(PackageRequest {
            package_id: id.clone(),
            version: version.clone(),
        });
        // R4-compat filter (`.cjs` line 108): drop non-R4 packages (do NOT recurse).
        if !is_r4_compatible(&pkg) {
            continue;
        }
        out.push((id.clone(), version.clone()));
        // Recurse over dependencies (`.cjs` lines 110-112) — process in stable
        // order (the `.cjs` uses Object.entries insertion order; we push in reverse
        // so the pop order matches insertion order).
        for (dep_id, dep_ver) in pkg.dependencies.iter().rev() {
            let canonical = canonical_version(dep_id, dep_ver);
            if needs_version_resolution(&canonical) {
                push_mutable_request(
                    &mut mutable_requests,
                    MutableVersionRequest {
                        package_id: dep_id.clone(),
                        requested: canonical,
                        resolved_version: resolve_version(index, dep_id, dep_ver).ok(),
                        set: RequestedSet::Context,
                    },
                );
            }
            match resolve_version(index, dep_id, dep_ver) {
                Ok(ver) => stack.push((dep_id.clone(), ver)),
                Err(requested) => push_missing(
                    &mut missing,
                    MissingPackage {
                        package_id: dep_id.clone(),
                        version: dep_ver.clone(),
                        reason: MissingReason::UnresolvedVersion { requested },
                        set: RequestedSet::Context,
                    },
                ),
            }
        }
    }

    // Prepend r4.core if the project is R4 and it is not already present
    // (`.cjs` lines 120-125). The `.cjs` reads the ROOT package's fhirVersions;
    // the project analogue is the config `fhirVersion`.
    let root_is_r4 = cfg.fhir_version().starts_with("4.");
    if root_is_r4 && !out.iter().any(|(id, _)| id == "hl7.fhir.r4.core") {
        out.insert(0, ("hl7.fhir.r4.core".to_string(), "4.0.1".to_string()));
    }

    // core-first stable sort (`.cjs` lines 127-133): r4.core sorts before all else;
    // everything else keeps discovery order.
    out.sort_by(|a, b| {
        let ac = a.0 == "hl7.fhir.r4.core";
        let bc = b.0 == "hl7.fhir.r4.core";
        match (ac, bc) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        }
    });

    let closure = out
        .into_iter()
        .map(|(package_id, version)| PackageRequest {
            package_id,
            version,
        })
        .collect();
    (closure, resolution_support, missing, mutable_requests)
}

/// The transitive R4 context closure for a SINGLE root package `id#ver` mounted in
/// `source` under `cache` — the exact set `snapshot/package-deps.cjs` prints for a
/// published IG package. This is the DRY consolidation point: the `.cjs` is (or
/// gates against) this function.
///
/// Seeds from the root package's OWN `dependencies` (not the root itself, matching
/// the `.cjs` which starts its walk from `rootJson.dependencies`), walks
/// transitively with the same R4-compat filter + core canonicalization, prepends
/// `hl7.fhir.r4.core#4.0.1` when the root declares an R4 `fhirVersions` and it is
/// absent, and core-first sorts. Version resolution uses `index` (for cached
/// `latest`/`x` deps) exactly like the `.cjs` `readdirSync`-backed `resolveSpec`.
///
/// Returns the closure labels in `.cjs` output order. The root package MUST be
/// mounted (its `package.json` readable); the walk skips deps that are not mounted
/// (the `.cjs` would `readdirSync`-fail — here they simply don't contribute, and
/// the caller is expected to have a complete cache, as the `.cjs` assumes).
pub fn context_closure_for_root(
    source: &dyn PackageSource,
    cache: &Path,
    root_label: &str,
    index: Option<&VersionIndex>,
) -> anyhow::Result<Vec<PackageRequest>> {
    let root = read_package_json(source, cache, root_label).ok_or_else(|| {
        anyhow::anyhow!("root package not mounted / no package.json: {root_label}")
    })?;

    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    // The `.cjs` seeds `seen` with the root spec so the root never re-enters.
    seen.insert(root_label.to_string());

    // Depth-first, dependencies in insertion order (matching the `.cjs` recursion).
    fn add(
        source: &dyn PackageSource,
        cache: &Path,
        index: Option<&VersionIndex>,
        id: &str,
        version: &str,
        seen: &mut BTreeSet<String>,
        out: &mut Vec<(String, String)>,
    ) {
        let label = format!("{id}#{version}");
        if !seen.insert(label) {
            return;
        }
        let Some(pkg) = read_package_json(source, cache, &format!("{id}#{version}")) else {
            return;
        };
        if !is_r4_compatible(&pkg) {
            return;
        }
        out.push((id.to_string(), version.to_string()));
        for (dep_id, dep_ver) in &pkg.dependencies {
            if let Ok(ver) = resolve_version(index, dep_id, dep_ver) {
                add(source, cache, index, dep_id, &ver, seen, out);
            }
        }
    }

    for (dep_id, dep_ver) in &root.dependencies {
        if let Ok(ver) = resolve_version(index, dep_id, dep_ver) {
            add(source, cache, index, dep_id, &ver, &mut seen, &mut out);
        }
    }

    // Prepend r4.core if the root is R4 and it is absent (`.cjs` lines 120-125).
    let root_is_r4 = root.fhir_versions.iter().any(|v| v.starts_with("4."));
    if root_is_r4 && !out.iter().any(|(id, _)| id == "hl7.fhir.r4.core") {
        out.insert(0, ("hl7.fhir.r4.core".to_string(), "4.0.1".to_string()));
    }

    // core-first stable sort (`.cjs` lines 127-133).
    out.sort_by(|a, b| {
        let ac = a.0 == "hl7.fhir.r4.core";
        let bc = b.0 == "hl7.fhir.r4.core";
        match (ac, bc) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        }
    });

    Ok(out
        .into_iter()
        .map(|(package_id, version)| PackageRequest {
            package_id,
            version,
        })
        .collect())
}

/// Build a [`VersionIndex`] from the package dirs present in a cache root (the
/// `<id>#<ver>` directory names), replicating the `.cjs` `readdirSync`-backed
/// candidate list. Used by the native `resolve` bin so its version resolution
/// matches the `.cjs` exactly (candidates = cached dirs).
pub fn version_index_from_cache(source: &dyn PackageSource, cache: &Path) -> VersionIndex {
    let mut index = VersionIndex::new();
    let Ok(entries) = source.read_dir(cache) else {
        return index;
    };
    let mut by_id: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for ent in entries {
        if let Some((id, ver)) = ent.file_name.split_once('#') {
            by_id
                .entry(id.to_string())
                .or_default()
                .push(ver.to_string());
        }
    }
    for (id, versions) in by_id {
        index.insert(id, versions);
    }
    index
}

fn push_missing(missing: &mut Vec<MissingPackage>, m: MissingPackage) {
    if !missing
        .iter()
        .any(|e| e.package_id == m.package_id && e.version == m.version)
    {
        missing.push(m);
    }
}

fn dedup_missing(missing: &mut Vec<MissingPackage>) {
    let mut seen = BTreeSet::new();
    missing.retain(|m| seen.insert((m.package_id.clone(), m.version.clone())));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::DiskSource;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("resolve_{name}_{}_{}", std::process::id(), nanos))
    }

    /// Write a package.json into `<cache>/<id>#<ver>/package/package.json`.
    fn write_pkg(cache: &Path, id: &str, ver: &str, fhir: &[&str], deps: &[(&str, &str)]) {
        let dir = cache.join(format!("{id}#{ver}")).join("package");
        std::fs::create_dir_all(&dir).unwrap();
        let fhir_versions: Vec<String> = fhir.iter().map(|s| s.to_string()).collect();
        let deps_map: BTreeMap<String, String> = deps
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let json = serde_json::json!({
            "name": id, "version": ver,
            "fhirVersions": fhir_versions,
            "dependencies": deps_map,
        });
        std::fs::write(dir.join("package.json"), serde_json::to_vec(&json).unwrap()).unwrap();
    }

    #[test]
    fn version_rules_match_cjs() {
        assert!(needs_version_resolution("latest"));
        assert!(needs_version_resolution("current"));
        assert!(needs_version_resolution("4.0.x"));
        assert!(needs_version_resolution("4.x"));
        assert!(needs_version_resolution("*"));
        assert!(!needs_version_resolution("4.0.1"));
        assert_eq!(canonical_version("hl7.fhir.r4.core", "4.0.0"), "4.0.1");
        assert_eq!(canonical_version("hl7.fhir.r4.core", "4.0.1"), "4.0.1");
        assert_eq!(canonical_version("other", "4.0.0"), "4.0.0");
        assert!(version_matches("4.0.10", "4.0.x"));
        assert!(!version_matches("4.1.0", "4.0.x"));
        assert_eq!(
            compare_versions("4.0.10", "4.0.2"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_versions("5.3.0", "5.3.0-ballot"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn resolve_version_uses_index_for_wildcards() {
        let mut ix = VersionIndex::new();
        ix.insert("p", vec!["4.0.2".into(), "4.0.10".into(), "4.1.0".into()]);
        assert_eq!(resolve_version(Some(&ix), "p", "4.0.x").unwrap(), "4.0.10");
        assert_eq!(resolve_version(Some(&ix), "p", "latest").unwrap(), "4.1.0");
        assert_eq!(resolve_version(Some(&ix), "p", "4.0.1").unwrap(), "4.0.1");
        // No index + latest => error (never a guess).
        assert!(resolve_version(None, "p", "latest").is_err());
        assert!(resolve_version(None, "p", "4.0.x").is_err());
    }

    #[test]
    fn context_closure_transitive_r4_filter_and_core_first() {
        let cache = tmp("closure");
        // root -> a (R4) -> b (R4); root -> c (R5, dropped).
        write_pkg(
            &cache,
            "root",
            "1.0.0",
            &["4.0.1"],
            &[("a", "1.0.0"), ("c", "1.0.0")],
        );
        write_pkg(&cache, "a", "1.0.0", &["4.0.1"], &[("b", "1.0.0")]);
        write_pkg(&cache, "b", "1.0.0", &["4.0.1"], &[]);
        write_pkg(&cache, "c", "1.0.0", &["5.0.0"], &[]); // R5 => dropped
        let index = version_index_from_cache(&DiskSource, &cache);

        let closure =
            context_closure_for_root(&DiskSource, &cache, "root#1.0.0", Some(&index)).unwrap();
        let labels: Vec<String> = closure
            .iter()
            .map(|r| format!("{}#{}", r.package_id, r.version))
            .collect();
        // r4.core prepended (root is R4, absent), a + b present, c dropped.
        assert_eq!(labels[0], "hl7.fhir.r4.core#4.0.1");
        assert!(labels.contains(&"a#1.0.0".to_string()));
        assert!(labels.contains(&"b#1.0.0".to_string()));
        assert!(!labels.iter().any(|l| l.starts_with("c#")));
        std::fs::remove_dir_all(&cache).ok();
    }

    #[test]
    fn resolve_project_reports_missing_and_reaches_fixpoint() {
        let cache = tmp("project");
        // Config depends on `dep#1.0.0`; dep depends on transitive `t#1.0.0`.
        // Mount r4.core + dep + t; auto-deps (tools/terminology/extensions) missing.
        write_pkg(&cache, "hl7.fhir.r4.core", "4.0.1", &["4.0.1"], &[]);
        write_pkg(&cache, "dep", "1.0.0", &["4.0.1"], &[("t", "1.0.0")]);
        write_pkg(&cache, "t", "1.0.0", &["4.0.1"], &[]);
        let index = version_index_from_cache(&DiskSource, &cache);

        let config = "fhirVersion: 4.0.1\ndependencies:\n  dep: 1.0.0\n";
        let step = resolve_project(config, &DiskSource, &cache, Some(&index)).unwrap();

        assert_eq!(step.resolver_schema, RESOLVER_SCHEMA);
        assert!(step.mutable_requests.iter().any(|request| {
            request.package_id == "hl7.fhir.uv.tools.r4"
                && request.requested == "latest"
                && request.set == RequestedSet::Compile
        }));

        // compile_set: stock non-transitive. dep + r4.core present; the R4 auto-deps
        // (tools.r4/terminology.r4/extensions.r4) requested at `latest` — the index
        // has no such dirs, so they land in `missing` as UnresolvedVersion.
        assert!(step
            .compile_set
            .iter()
            .any(|r| r.package_id == "dep" && r.version == "1.0.0"));
        assert!(step
            .compile_set
            .iter()
            .any(|r| r.package_id == "hl7.fhir.r4.core"));
        // context closure walked transitively: dep + t + core.
        let ctx: Vec<String> = step
            .context_closure
            .iter()
            .map(|r| format!("{}#{}", r.package_id, r.version))
            .collect();
        assert!(ctx.contains(&"dep#1.0.0".to_string()));
        assert!(ctx.contains(&"t#1.0.0".to_string()));
        assert_eq!(ctx[0], "hl7.fhir.r4.core#4.0.1");
        // The auto-deps are unresolvable without them in the index -> not satisfied.
        assert!(!step.satisfied);
        assert!(step.missing.iter().any(|m| m.package_id.contains("tools")
            && matches!(m.reason, MissingReason::UnresolvedVersion { .. })));
        std::fs::remove_dir_all(&cache).ok();
    }

    #[test]
    fn resolve_project_satisfied_when_all_mounted() {
        let cache = tmp("sat");
        write_pkg(&cache, "hl7.fhir.r4.core", "4.0.1", &["4.0.1"], &[]);
        write_pkg(&cache, "hl7.fhir.uv.tools.r4", "1.1.2", &["4.0.1"], &[]);
        write_pkg(&cache, "hl7.terminology.r4", "7.2.0", &["4.0.1"], &[]);
        write_pkg(
            &cache,
            "hl7.fhir.uv.extensions.r4",
            "5.3.0",
            &["4.0.1"],
            &[],
        );
        let index = version_index_from_cache(&DiskSource, &cache);
        // A pure-core project (no configured deps) with all auto-deps cached.
        let config = "fhirVersion: 4.0.1\n";
        let step = resolve_project(config, &DiskSource, &cache, Some(&index)).unwrap();
        assert!(
            step.satisfied,
            "all auto-deps + core mounted => satisfied; missing={:?}",
            step.missing
        );
        assert!(step
            .mutable_requests
            .iter()
            .all(|request| request.resolved_version.is_some()));
        std::fs::remove_dir_all(&cache).ok();
    }

    #[test]
    fn automatic_compile_roots_discover_exact_transitive_support_before_fixpoint() {
        let cache = tmp("automatic-context-roots");
        write_pkg(&cache, "hl7.fhir.r4.core", "4.0.1", &["4.0.1"], &[]);
        write_pkg(
            &cache,
            "hl7.fhir.uv.tools.r4",
            "1.1.2",
            &["4.0.1"],
            &[
                ("hl7.fhir.r4.core", "4.0.1"),
                ("hl7.terminology.r4", "7.1.0"),
                ("hl7.fhir.uv.extensions.r4", "5.2.0"),
            ],
        );
        write_pkg(&cache, "hl7.terminology.r4", "7.2.0", &["4.0.1"], &[]);
        write_pkg(
            &cache,
            "hl7.fhir.uv.extensions.r4",
            "5.3.0",
            &["4.0.1"],
            &[],
        );
        let index = version_index_from_cache(&DiskSource, &cache);
        let config = "id: cycle-like\nfhirVersion: 4.0.1\n";

        let incomplete = resolve_project(config, &DiskSource, &cache, Some(&index)).unwrap();
        assert!(!incomplete.satisfied);
        for (package_id, version) in [
            ("hl7.terminology.r4", "7.1.0"),
            ("hl7.fhir.uv.extensions.r4", "5.2.0"),
        ] {
            assert!(incomplete.missing.iter().any(|missing| {
                missing.package_id == package_id
                    && missing.version == version
                    && missing.set == RequestedSet::Context
                    && matches!(missing.reason, MissingReason::NotMounted)
            }));
        }

        write_pkg(&cache, "hl7.terminology.r4", "7.1.0", &["4.0.1"], &[]);
        write_pkg(
            &cache,
            "hl7.fhir.uv.extensions.r4",
            "5.2.0",
            &["4.0.1"],
            &[],
        );
        let complete = resolve_project(config, &DiskSource, &cache, Some(&index)).unwrap();
        assert!(complete.satisfied, "missing={:?}", complete.missing);
        for (package_id, version) in [
            ("hl7.fhir.uv.tools.r4", "1.1.2"),
            ("hl7.terminology.r4", "7.1.0"),
            ("hl7.fhir.uv.extensions.r4", "5.2.0"),
        ] {
            assert!(complete
                .context_closure
                .iter()
                .any(|request| { request.package_id == package_id && request.version == version }));
            assert!(complete
                .resolution_support
                .iter()
                .any(|request| { request.package_id == package_id && request.version == version }));
        }
        std::fs::remove_dir_all(&cache).ok();
    }

    #[test]
    fn resolution_step_exposes_mutable_inputs_and_their_exact_answers() {
        let cache = tmp("mutable-metadata");
        write_pkg(&cache, "hl7.fhir.r4.core", "4.0.1", &["4.0.1"], &[]);
        write_pkg(&cache, "hl7.fhir.uv.tools.r4", "1.1.2", &["4.0.1"], &[]);
        write_pkg(&cache, "hl7.terminology.r4", "7.2.0", &["4.0.1"], &[]);
        write_pkg(
            &cache,
            "hl7.fhir.uv.extensions.r4",
            "5.3.0",
            &["4.0.1"],
            &[],
        );
        write_pkg(&cache, "example", "1.0.0", &["4.0.1"], &[]);
        write_pkg(
            &cache,
            "example",
            "2.0.0",
            &["4.0.1"],
            &[("filtered-r5", "1.0.0")],
        );
        write_pkg(&cache, "filtered-r5", "1.0.0", &["5.0.0"], &[]);
        let index = version_index_from_cache(&DiskSource, &cache);
        let step = resolve_project(
            "fhirVersion: 4.0.1\ndependencies:\n  example: latest\n",
            &DiskSource,
            &cache,
            Some(&index),
        )
        .unwrap();
        assert!(step.satisfied);
        assert!(step.mutable_requests.iter().any(|request| {
            request.package_id == "example"
                && request.requested == "latest"
                && request.resolved_version.as_deref() == Some("2.0.0")
                && request.set == RequestedSet::Compile
        }));
        assert!(!step
            .context_closure
            .iter()
            .any(|request| request.package_id == "filtered-r5"));
        assert!(step
            .resolution_support
            .iter()
            .any(|request| request.package_id == "filtered-r5"));
        assert!(step.mutable_requests.iter().any(|request| {
            request.package_id == "example"
                && request.resolved_version.as_deref() == Some("2.0.0")
                && request.set == RequestedSet::Context
        }));
        // Replaying only the final closure cannot prove why filtered-r5 was
        // excluded. The explicit resolution-support witness is therefore part
        // of the persistent acquisition plan.
        std::fs::remove_dir_all(cache.join("filtered-r5#1.0.0")).unwrap();
        let closure_only = resolve_project(
            "fhirVersion: 4.0.1\ndependencies:\n  example: latest\n",
            &DiskSource,
            &cache,
            Some(&index),
        )
        .unwrap();
        assert!(!closure_only.satisfied);
        assert!(closure_only
            .missing
            .iter()
            .any(|request| request.package_id == "filtered-r5"));
        std::fs::remove_dir_all(&cache).ok();
    }
}
