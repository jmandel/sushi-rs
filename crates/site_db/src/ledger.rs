//! §2c BuildState ledger. A `node_key -> input_hash -> output_hash` table,
//! populated per stage (snapshot/resource/page/config/asset nodes). Granularity
//! is coarse (a full recompute is allowed), but the hashes are REAL: a no-op
//! rebuild compares this ledger against the prior one and, when every node is
//! clean, writes nothing (gate v). The ledger persists as a sidecar JSON next to
//! the site.db so the next run can load it.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::Digest;

/// One ledger row.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LedgerNode {
    pub input_hash: String,
    pub output_hash: String,
}

/// The full ledger: node_key -> {input_hash, output_hash}. BTreeMap keeps the
/// serialization deterministic (sorted keys) so ledger-to-ledger comparison and
/// on-disk bytes are stable.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BuildLedger {
    pub nodes: BTreeMap<String, LedgerNode>,
}

/// Per-node dirtiness classification vs a prior ledger.
#[derive(Clone, Debug, Default)]
pub struct LedgerReport {
    pub ledger: BuildLedger,
    /// Nodes whose output_hash changed (or are new) vs the prior ledger.
    pub dirty: Vec<String>,
    /// Nodes present+identical in both ledgers.
    pub clean: Vec<String>,
    /// True iff there was a prior ledger and nothing is dirty (and no node was
    /// dropped): a proven no-op rebuild.
    pub no_op: bool,
}

impl BuildLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hex sha256 of bytes (mirrors package_acquisition's idiom).
    pub fn hash(bytes: &[u8]) -> String {
        hex::encode(sha2::Sha256::digest(bytes))
    }

    /// Record a node. `input_hash` may be empty for derived nodes whose input is
    /// captured upstream; the output_hash is what dirtiness keys on.
    pub fn record(&mut self, node_key: &str, input_hash: &str, output_hash: &str) {
        self.nodes.insert(
            node_key.to_string(),
            LedgerNode {
                input_hash: input_hash.to_string(),
                output_hash: output_hash.to_string(),
            },
        );
    }

    /// Compare against a prior ledger to classify clean/dirty and detect a no-op.
    pub fn finish(self, prior: Option<&BuildLedger>) -> LedgerReport {
        let mut dirty = Vec::new();
        let mut clean = Vec::new();
        for (key, node) in &self.nodes {
            match prior.and_then(|p| p.nodes.get(key)) {
                Some(prev) if prev == node => clean.push(key.clone()),
                _ => dirty.push(key.clone()),
            }
        }
        // A node the prior ledger had but this build dropped also breaks no-op.
        let dropped = prior
            .map(|p| p.nodes.keys().any(|k| !self.nodes.contains_key(k)))
            .unwrap_or(false);
        let no_op = prior.is_some() && dirty.is_empty() && !dropped;
        LedgerReport {
            ledger: self,
            dirty,
            clean,
            no_op,
        }
    }

    pub fn load(path: &std::path::Path) -> Option<BuildLedger> {
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}
