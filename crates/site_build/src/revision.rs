//! Pure, immutable SiteBuild revision transitions.
//!
//! SiteBuild v1 deliberately stores content references rather than embedding
//! object bytes.  A demand-driven renderer therefore needs to do two things at
//! the same boundary: publish newly produced bytes to a CAS and replace the
//! corresponding catalog records in a new, re-hashed [`SiteBuild`].  This
//! module models that boundary without mutable session identity.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::{
    ArtifactCatalog, ArtifactKey, ArtifactProvenance, ArtifactRecord, ArtifactState, BuildId,
    ContentRef, ReadDependency, RenderPlan, Sha256Digest, SiteBuild, SiteBuildError,
};

/// Immutable bytes to publish at `objects/sha256/<digest>`.
///
/// Construction derives digest and length from the bytes. [`verify`](Self::verify)
/// is still public so a host can recheck an object after crossing a trust
/// boundary or reading it back from storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentObject {
    sha256: Sha256Digest,
    byte_length: u64,
    bytes: Vec<u8>,
}

impl ContentObject {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        let bytes = bytes.into();
        Self {
            sha256: Sha256Digest::of_bytes(&bytes),
            byte_length: bytes.len() as u64,
            bytes,
        }
    }

    pub fn sha256(&self) -> &Sha256Digest {
        &self.sha256
    }

    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }

    pub fn content_ref(&self, media_type: Option<impl Into<String>>) -> ContentRef {
        ContentRef {
            sha256: self.sha256.clone(),
            byte_length: self.byte_length,
            media_type: media_type.map(Into::into),
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn verify(&self) -> Result<(), RevisionError> {
        let actual_sha256 = Sha256Digest::of_bytes(&self.bytes);
        let actual_length = self.bytes.len() as u64;
        if actual_sha256 != self.sha256 || actual_length != self.byte_length {
            return Err(RevisionError::ObjectContentMismatch {
                expected_sha256: self.sha256.clone(),
                actual_sha256,
                expected_length: self.byte_length,
                actual_length,
            });
        }
        Ok(())
    }
}

/// One catalog replacement in a revision transition.
///
/// A ready resolution always carries its exact object bytes.  Non-ready states
/// carry no object, so a host cannot accidentally publish stale bytes beside a
/// deferred, unsupported, or failed record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactResolution {
    record: ArtifactRecord,
    object: Option<ContentObject>,
}

impl ArtifactResolution {
    pub fn ready(
        key: ArtifactKey,
        bytes: impl Into<Vec<u8>>,
        media_type: Option<impl Into<String>>,
        provenance: ArtifactProvenance,
        reads: BTreeSet<ReadDependency>,
    ) -> Self {
        let object = ContentObject::new(bytes);
        let content = object.content_ref(media_type);
        let record = ArtifactRecord {
            key,
            state: ArtifactState::Ready { content },
            provenance,
            reads,
        };
        Self {
            record,
            object: Some(object),
        }
    }

    pub fn non_ready(
        key: ArtifactKey,
        state: ArtifactState,
        provenance: ArtifactProvenance,
        reads: BTreeSet<ReadDependency>,
    ) -> Result<Self, RevisionError> {
        if matches!(state, ArtifactState::Ready { .. }) {
            return Err(RevisionError::ReadyWithoutObject(key));
        }
        Ok(Self {
            record: ArtifactRecord {
                key,
                state,
                provenance,
                reads,
            },
            object: None,
        })
    }

    pub fn record(&self) -> &ArtifactRecord {
        &self.record
    }

    pub fn object(&self) -> Option<&ContentObject> {
        self.object.as_ref()
    }

    fn verify(&self) -> Result<(), RevisionError> {
        match (&self.record.state, &self.object) {
            (ArtifactState::Ready { content }, Some(object)) => {
                object.verify()?;
                let actual = object.content_ref(content.media_type.clone());
                if content != &actual {
                    return Err(RevisionError::ContentMismatch {
                        expected: content.clone(),
                        actual,
                    });
                }
            }
            (ArtifactState::Ready { .. }, None) => {
                return Err(RevisionError::ReadyWithoutObject(self.record.key.clone()));
            }
            (_, Some(_)) => {
                return Err(RevisionError::ObjectForNonReady(self.record.key.clone()));
            }
            (_, None) => {}
        }
        Ok(())
    }
}

/// Result of applying a resolution batch to an explicit predecessor.
///
/// `objects` contains only objects introduced by this transition.  Objects
/// inherited from any predecessor source, package, or ready-artifact reference
/// stay in the caller's CAS. The predecessor id is informational proof of the
/// explicit input to this pure operation; it is not an ambient "last build".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteBuildSuccessor {
    predecessor: BuildId,
    site_build: SiteBuild,
    objects: BTreeMap<Sha256Digest, ContentObject>,
}

impl SiteBuildSuccessor {
    pub fn predecessor(&self) -> &BuildId {
        &self.predecessor
    }

    pub fn site_build(&self) -> &SiteBuild {
        &self.site_build
    }

    pub fn into_site_build(self) -> SiteBuild {
        self.site_build
    }

    pub fn objects(&self) -> &BTreeMap<Sha256Digest, ContentObject> {
        &self.objects
    }

    /// Verify both the successor manifest and every object emitted by this
    /// transition.  This is useful immediately before atomic publication.
    pub fn verify(&self) -> Result<(), RevisionError> {
        self.site_build.verify()?;
        for (digest, object) in &self.objects {
            object.verify()?;
            if digest != object.sha256() {
                return Err(RevisionError::ObjectKeyMismatch {
                    key: digest.clone(),
                    content: object.sha256().clone(),
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum RevisionError {
    #[error("resolution batch contains duplicate artifact {0:?}")]
    DuplicateResolution(ArtifactKey),
    #[error("ready resolution for {0:?} has no content object")]
    ReadyWithoutObject(ArtifactKey),
    #[error("non-ready resolution for {0:?} unexpectedly carries a content object")]
    ObjectForNonReady(ArtifactKey),
    #[error("resolved content mismatch: expected {expected:?}, got {actual:?}")]
    ContentMismatch {
        expected: ContentRef,
        actual: ContentRef,
    },
    #[error(
        "CAS object bytes mismatch: expected {expected_sha256}/{expected_length}, got {actual_sha256}/{actual_length}"
    )]
    ObjectContentMismatch {
        expected_sha256: Sha256Digest,
        actual_sha256: Sha256Digest,
        expected_length: u64,
        actual_length: u64,
    },
    #[error("CAS object key {key} does not match its content digest {content}")]
    ObjectKeyMismatch {
        key: Sha256Digest,
        content: Sha256Digest,
    },
    #[error("two resolved artifacts produced different bytes for CAS digest {0}")]
    DigestCollision(Sha256Digest),
    #[error(
        "predecessor references CAS digest {digest} with conflicting lengths {first_length} and {second_length}"
    )]
    PredecessorDigestLengthConflict {
        digest: Sha256Digest,
        first_length: u64,
        second_length: u64,
    },
    #[error(transparent)]
    Build(#[from] SiteBuildError),
}

impl SiteBuild {
    /// Apply a deterministic, atomic resolution batch to this explicit build.
    ///
    /// Existing records with the same keys are replaced; all other fields are
    /// copied.  `render_plan` may promote newly declared outputs to required
    /// roots.  The predecessor is never mutated, and resolution iteration order
    /// cannot affect the successor id or object set.
    pub fn successor(
        &self,
        render_plan: Option<RenderPlan>,
        resolutions: impl IntoIterator<Item = ArtifactResolution>,
    ) -> Result<SiteBuildSuccessor, RevisionError> {
        let mut inherited_objects = BTreeMap::<Sha256Digest, u64>::new();
        let mut inherit = |content: &ContentRef| -> Result<(), RevisionError> {
            if let Some(first_length) =
                inherited_objects.insert(content.sha256.clone(), content.byte_length)
            {
                if first_length != content.byte_length {
                    return Err(RevisionError::PredecessorDigestLengthConflict {
                        digest: content.sha256.clone(),
                        first_length,
                        second_length: content.byte_length,
                    });
                }
            }
            Ok(())
        };
        for (_, source) in self.project().sources.iter() {
            inherit(&source.content)?;
        }
        for (_, package) in self.package_lock().iter() {
            inherit(&package.content)?;
        }
        for (_, record) in self.artifacts().iter() {
            if let ArtifactState::Ready { content } = &record.state {
                inherit(content)?;
            }
        }

        let mut replacements = BTreeMap::new();
        let mut objects: BTreeMap<Sha256Digest, ContentObject> = BTreeMap::new();
        for resolution in resolutions {
            resolution.verify()?;
            let key = resolution.record.key.clone();
            if replacements
                .insert(key.clone(), resolution.record)
                .is_some()
            {
                return Err(RevisionError::DuplicateResolution(key));
            }
            if let Some(object) = resolution.object {
                let digest = object.sha256().clone();
                if let Some(inherited_length) = inherited_objects.get(&digest) {
                    if *inherited_length != object.byte_length() {
                        return Err(RevisionError::PredecessorDigestLengthConflict {
                            digest,
                            first_length: *inherited_length,
                            second_length: object.byte_length(),
                        });
                    }
                    continue;
                }
                if let Some(existing) = objects.get(&digest) {
                    if existing.bytes() != object.bytes()
                        || existing.byte_length() != object.byte_length()
                    {
                        return Err(RevisionError::DigestCollision(digest));
                    }
                } else {
                    objects.insert(digest, object);
                }
            }
        }

        let mut records: BTreeMap<ArtifactKey, ArtifactRecord> = self
            .artifacts()
            .iter()
            .map(|(key, record)| (key.clone(), record.clone()))
            .collect();
        records.extend(replacements);
        let catalog = ArtifactCatalog::from_records(records.into_values())
            .expect("BTreeMap keys make duplicate records impossible");
        let site_build = SiteBuild::new(
            self.project().clone(),
            self.package_lock().clone(),
            self.render_target().clone(),
            render_plan.unwrap_or_else(|| self.render_plan().clone()),
            catalog,
            self.diagnostics().clone(),
        )?;
        let successor = SiteBuildSuccessor {
            predecessor: self.build_id().clone(),
            site_build,
            objects,
        };
        successor.verify()?;
        Ok(successor)
    }
}
