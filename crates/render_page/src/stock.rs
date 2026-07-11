//! SiteBuild revision collection for the stock/native template renderer.
//!
//! This is deliberately a thin adapter over [`site_build::SiteBuild::successor`]:
//! rendering remains in `render_page`, while immutable revision mechanics stay
//! in `site_build`. The caller supplies an explicit predecessor, the exact
//! non-generated inputs that were mounted, every advertised page/asset output,
//! and the per-page observations captured by [`PageProvider`].

use std::collections::{BTreeMap, BTreeSet};

use site_build::{
    ArtifactKey, ArtifactProvenance, ArtifactResolution, ArtifactState, AssetNamespace, Need,
    ReadDependency, RenderMode, RenderPlan, ResolutionBatch, RevisionError, SiteBuild,
    SiteBuildSuccessor, SourcePath,
};
use thiserror::Error;

use crate::{ArtifactObservation, PageArtifactReadSet};

pub const STOCK_ASSEMBLED_ASSET_NAMESPACE: &str = "stock.assembled";

/// One exact mounted page-pass input. Its key should use the stock input
/// namespaces exported by this crate (`stock.page_source`, `stock.site_data`,
/// `stock.include.staged`, or `stock.include.template`).
#[derive(Clone, Debug)]
pub struct StockInput {
    pub key: ArtifactKey,
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub provenance: ArtifactProvenance,
    pub reads: BTreeSet<ReadDependency>,
}

/// One page advertised by the stock renderer. Non-ready outcomes are required
/// roots too, causing closure to fail with the existing typed `SealBlocker`.
#[derive(Clone, Debug)]
pub struct StockPage {
    pub path: SourcePath,
    pub outcome: StockPageOutcome,
    pub provenance: ArtifactProvenance,
}

#[derive(Clone, Debug)]
pub enum StockPageOutcome {
    Ready {
        bytes: Vec<u8>,
        reads: PageArtifactReadSet,
    },
    NonReady {
        state: ArtifactState,
        /// Reads known before rendering stopped. They remain useful for
        /// invalidation and diagnostics even though the output is not ready.
        reads: BTreeSet<ArtifactKey>,
    },
}

/// One final static output. The key always lives in the single assembled stock
/// namespace, independent of whether its source was authored, template-owned,
/// Publisher runtime, or generated; those origins belong in `reads`.
#[derive(Clone, Debug)]
pub struct StockAsset {
    pub path: SourcePath,
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub provenance: ArtifactProvenance,
    pub reads: BTreeSet<ReadDependency>,
}

/// Provenance and conservative semantic dependencies for generated fragments.
/// A renderer with a finer dependency tracker can pass a smaller set; using
/// [`all_compile_inputs`] is safe and makes invalidation conservative.
#[derive(Clone, Debug)]
pub struct StockFragmentPolicy {
    pub provenance: ArtifactProvenance,
    pub reads: BTreeSet<ReadDependency>,
}

#[derive(Debug, Error)]
pub enum StockPlanError {
    #[error("stock plan requires a native-template render target")]
    WrongRenderMode,
    #[error("stock input key is not a Data artifact: {0:?}")]
    InvalidInputKey(ArtifactKey),
    #[error("stock input name is not a safe normalized relative path: {0:?}")]
    InvalidInputName(ArtifactKey),
    #[error("stock page has a Ready ArtifactState without object bytes: {0}")]
    InvalidPageState(SourcePath),
    #[error("conflicting observations for artifact {0:?}")]
    ConflictingObservation(ArtifactKey),
    #[error("page observed changing bytes for input artifact {0:?}")]
    ConflictingInputBytes(ArtifactKey),
    #[error("captured bytes disagree with input artifact {0:?}")]
    InputBytesMismatch(ArtifactKey),
    #[error("page read input artifact without captured content {0:?}")]
    MissingInputObject(ArtifactKey),
    #[error(transparent)]
    Revision(#[from] RevisionError),
}

/// Conservative read set for a native artifact whose lower-level engine does
/// not yet expose resource/package access tracing. It is explicit and complete:
/// every exact authored source and locked package is named, with no ambient
/// session state hidden behind the record.
pub fn all_compile_inputs(build: &SiteBuild) -> BTreeSet<ReadDependency> {
    build
        .project()
        .sources
        .iter()
        .map(|(path, _)| ReadDependency::Source { path: path.clone() })
        .chain(
            build
                .package_lock()
                .iter()
                .map(|(coordinate, _)| ReadDependency::Package {
                    coordinate: coordinate.clone(),
                }),
        )
        .collect()
}

pub fn assembled_asset_key(path: SourcePath) -> ArtifactKey {
    ArtifactKey::Asset {
        namespace: AssetNamespace::Other {
            name: STOCK_ASSEMBLED_ASSET_NAMESPACE.into(),
        },
        path,
    }
}

/// Promote one complete stock render into an immutable successor revision.
///
/// The render plan names every advertised final page and assembled asset.
/// Page source, `site.data`, staged include, and template include records are
/// pulled into the sealed closure through actual page reads. Successfully read
/// fragments are ready dependencies; failed attempts are cataloged with their
/// typed state but never fabricated as successful page reads.
pub fn collect_stock_revision(
    predecessor: &SiteBuild,
    inputs: impl IntoIterator<Item = StockInput>,
    pages: impl IntoIterator<Item = StockPage>,
    assets: impl IntoIterator<Item = StockAsset>,
    fragment_policy: StockFragmentPolicy,
) -> Result<SiteBuildSuccessor, StockPlanError> {
    if predecessor.render_target().mode != RenderMode::NativeTemplate {
        return Err(StockPlanError::WrongRenderMode);
    }

    let mut pending: BTreeMap<ArtifactKey, ArtifactResolution> = BTreeMap::new();
    let mut required = BTreeSet::new();
    let mut needs = BTreeSet::new();

    for input in inputs {
        if !matches!(input.key, ArtifactKey::Data { .. }) {
            return Err(StockPlanError::InvalidInputKey(input.key));
        }
        if let ArtifactKey::Data { name, .. } = &input.key {
            if SourcePath::parse(name.clone()).is_err() {
                return Err(StockPlanError::InvalidInputName(input.key));
            }
        }
        insert_consistent(
            &mut pending,
            ArtifactResolution::ready(
                input.key,
                input.bytes,
                Some(input.media_type),
                input.provenance,
                input.reads,
            ),
        )?;
    }

    for page in pages {
        let page_key = ArtifactKey::Page {
            path: page.path.clone(),
        };
        required.insert(page_key.clone());
        let resolution = match page.outcome {
            StockPageOutcome::Ready { bytes, reads } => {
                needs.extend(reads.requested().iter().cloned());
                for key in reads.input_reads() {
                    let values = reads
                        .input_objects()
                        .get(key)
                        .ok_or_else(|| StockPlanError::MissingInputObject(key.clone()))?;
                    if values.len() != 1 {
                        return Err(StockPlanError::ConflictingInputBytes(key.clone()));
                    }
                    let observed = values.iter().next().expect("one input value");
                    let matches_pending = pending
                        .get(key)
                        .and_then(ArtifactResolution::object)
                        .is_some_and(|object| object.bytes() == observed);
                    let matches_predecessor = predecessor
                        .artifacts()
                        .get(key)
                        .and_then(|record| match &record.state {
                            ArtifactState::Ready { content } => Some(content),
                            _ => None,
                        })
                        .is_some_and(|content| {
                            content.byte_length == observed.len() as u64
                                && content.sha256 == site_build::Sha256Digest::of_bytes(observed)
                        });
                    if !matches_pending && !matches_predecessor {
                        return Err(StockPlanError::InputBytesMismatch(key.clone()));
                    }
                }
                for (key, observation) in reads.observations() {
                    let resolution = match observation {
                        ArtifactObservation::Ready { bytes } => ArtifactResolution::ready(
                            key.clone(),
                            bytes.clone(),
                            Some("text/html"),
                            fragment_policy.provenance.clone(),
                            fragment_policy.reads.clone(),
                        ),
                        ArtifactObservation::NotReady { error } => ArtifactResolution::non_ready(
                            key.clone(),
                            error.artifact_state(),
                            fragment_policy.provenance.clone(),
                            fragment_policy.reads.clone(),
                        )?,
                    };
                    insert_consistent(&mut pending, resolution)?;
                }
                let dependencies = reads
                    .dependencies()
                    .into_iter()
                    .map(|key| ReadDependency::Artifact { key })
                    .collect();
                ArtifactResolution::ready(
                    page_key,
                    bytes,
                    Some("text/html"),
                    page.provenance,
                    dependencies,
                )
            }
            StockPageOutcome::NonReady { state, reads } => {
                if matches!(state, ArtifactState::Ready { .. }) {
                    return Err(StockPlanError::InvalidPageState(page.path));
                }
                ArtifactResolution::non_ready(
                    page_key,
                    state,
                    page.provenance,
                    reads
                        .into_iter()
                        .map(|key| ReadDependency::Artifact { key })
                        .collect(),
                )?
            }
        };
        insert_consistent(&mut pending, resolution)?;
    }

    for asset in assets {
        let key = assembled_asset_key(asset.path);
        required.insert(key.clone());
        insert_consistent(
            &mut pending,
            ArtifactResolution::ready(
                key,
                asset.bytes,
                Some(asset.media_type),
                asset.provenance,
                asset.reads,
            ),
        )?;
    }

    let batch = ResolutionBatch::new(
        Need::new(needs),
        Some(RenderPlan::new(required)),
        pending.into_values(),
    )?;
    predecessor
        .successor_batch(batch)
        .map_err(StockPlanError::from)
}

fn insert_consistent(
    pending: &mut BTreeMap<ArtifactKey, ArtifactResolution>,
    resolution: ArtifactResolution,
) -> Result<(), StockPlanError> {
    let key = resolution.record().key.clone();
    if let Some(existing) = pending.get(&key) {
        if existing != &resolution {
            return Err(StockPlanError::ConflictingObservation(key));
        }
        return Ok(());
    }
    pending.insert(key, resolution);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use render_sd::tree::{MemTree, TreeSource};
    use site_build::{
        ArtifactCatalog, ContentRef, DiagnosticSeverity, PackageLock, ProducerRef, ProjectRevision,
        RenderTarget, SourceEntry, SourceKind, SourceManifest,
    };

    use super::*;
    use crate::{
        render_page, stock_input_artifact, ArtifactResolveError, ArtifactResolver, PageProvider,
        SiteData, STOCK_PAGE_SOURCE_NAMESPACE, STOCK_SITE_DATA_NAMESPACE,
        STOCK_STAGED_INCLUDE_NAMESPACE,
    };

    const FRAGMENT: &str = "StructureDefinition-test-history.xhtml";

    fn path(value: &str) -> SourcePath {
        SourcePath::parse(value).unwrap()
    }

    fn provenance(recipe: &str) -> ArtifactProvenance {
        ArtifactProvenance {
            producer: ProducerRef::new("test.stock", "1"),
            recipe: recipe.into(),
            attributes: BTreeMap::new(),
        }
    }

    fn predecessor() -> SiteBuild {
        SiteBuild::new(
            ProjectRevision {
                project_id: "test.ig".into(),
                revision: "source-revision".into(),
                sources: SourceManifest::from_entries([(
                    path("input/fsh/test.fsh"),
                    SourceEntry {
                        kind: SourceKind::Fsh,
                        content: ContentRef::of_bytes(
                            b"Profile: Test",
                            Some("text/fhir-shorthand"),
                        ),
                    },
                )])
                .unwrap(),
            },
            PackageLock::default(),
            RenderTarget {
                renderer: ProducerRef::new("stock-template", "1"),
                mode: RenderMode::NativeTemplate,
                fhir_version: "4.0.1".into(),
                template: None,
                parameters: BTreeMap::new(),
            },
            RenderPlan::default(),
            ArtifactCatalog::default(),
            BTreeSet::new(),
        )
        .unwrap()
    }

    struct CountingResolver {
        calls: Rc<Cell<usize>>,
        result: Result<&'static str, ArtifactResolveError>,
    }

    impl ArtifactResolver for CountingResolver {
        fn resolve(&self, _key: &ArtifactKey) -> Result<String, ArtifactResolveError> {
            self.calls.set(self.calls.get() + 1);
            self.result.clone().map(str::to_string)
        }
    }

    fn input(namespace: &str, name: &str, bytes: &[u8], media_type: &str) -> StockInput {
        StockInput {
            key: stock_input_artifact(namespace, name),
            bytes: bytes.to_vec(),
            media_type: media_type.into(),
            provenance: provenance("capture"),
            reads: all_compile_inputs(&predecessor()),
        }
    }

    #[test]
    fn complete_plan_captures_pages_assets_data_includes_and_replays_without_callbacks() {
        let source = concat!(
            "---\n---\n",
            "{% include StructureDefinition-test-history.xhtml %}",
            "{% include note.md %}",
            "{% include template.md %}",
            "{{ site.data.info.title }}"
        );
        let mut tree = MemTree::new();
        tree.insert_text(std::path::Path::new("/site/en/index.html"), source);
        tree.insert_text(std::path::Path::new("/site/_includes/note.md"), "note");
        tree.insert_text(std::path::Path::new("/template/template.md"), "template");
        tree.insert_text(
            std::path::Path::new("/site/_data/info.json"),
            r#"{"title":"Demo"}"#,
        );
        let tree: Rc<dyn TreeSource> = Rc::new(tree);
        let site = SiteData::load_with_tree(&*tree, std::path::Path::new("/site/_data"));
        let calls = Rc::new(Cell::new(0));
        let provider = PageProvider::new(&site, std::path::Path::new("/site/_includes"))
            .with_tree(tree.clone())
            .with_pages_root(std::path::Path::new("/site"))
            .with_template_includes(std::path::Path::new("/template"))
            .with_engine_first(true)
            .with_artifact_resolver(CountingResolver {
                calls: calls.clone(),
                result: Ok("fragment"),
            });
        let html = render_page(source, "en/index.html", &provider);
        assert_eq!(html, "fragmentnotetemplateDemo");
        assert_eq!(calls.get(), 1);
        let reads = provider.page_artifact_reads();
        assert_eq!(reads.read().len(), 1);
        assert_eq!(reads.input_reads().len(), 4);
        let fragment_key = reads.read().iter().next().unwrap().clone();

        let base = predecessor();
        let successor = collect_stock_revision(
            &base,
            [
                input(
                    STOCK_PAGE_SOURCE_NAMESPACE,
                    "en/index.html",
                    source.as_bytes(),
                    "text/html",
                ),
                input(
                    STOCK_STAGED_INCLUDE_NAMESPACE,
                    "note.md",
                    b"note",
                    "text/markdown",
                ),
                input(
                    crate::STOCK_TEMPLATE_INCLUDE_NAMESPACE,
                    "template.md",
                    b"template",
                    "text/markdown",
                ),
                input(
                    STOCK_SITE_DATA_NAMESPACE,
                    "info.json",
                    br#"{"title":"Demo"}"#,
                    "application/json",
                ),
            ],
            [StockPage {
                path: path("en/index.html"),
                outcome: StockPageOutcome::Ready {
                    bytes: html.as_bytes().to_vec(),
                    reads,
                },
                provenance: provenance("page"),
            }],
            [StockAsset {
                path: path("assets/app.css"),
                bytes: b"body{}".to_vec(),
                media_type: "text/css".into(),
                provenance: provenance("asset"),
                reads: all_compile_inputs(&base),
            }],
            StockFragmentPolicy {
                provenance: provenance("fragment"),
                reads: all_compile_inputs(&base),
            },
        )
        .unwrap();
        assert_eq!(
            successor
                .site_build()
                .render_plan()
                .required_artifacts()
                .len(),
            2
        );
        assert!(matches!(
            successor
                .site_build()
                .artifacts()
                .get(&fragment_key)
                .unwrap()
                .state,
            ArtifactState::Ready { .. }
        ));
        let page_key = ArtifactKey::Page {
            path: path("en/index.html"),
        };
        let page_reads = &successor
            .site_build()
            .artifacts()
            .get(&page_key)
            .unwrap()
            .reads;
        for expected in [
            fragment_key.clone(),
            stock_input_artifact(STOCK_PAGE_SOURCE_NAMESPACE, "en/index.html"),
            stock_input_artifact(STOCK_STAGED_INCLUDE_NAMESPACE, "note.md"),
            stock_input_artifact(crate::STOCK_TEMPLATE_INCLUDE_NAMESPACE, "template.md"),
            stock_input_artifact(STOCK_SITE_DATA_NAMESPACE, "info.json"),
        ] {
            assert!(page_reads.contains(&ReadDependency::Artifact { key: expected }));
        }
        let closed = successor.site_build().clone().close().unwrap();

        let objects: BTreeMap<_, _> = successor
            .objects()
            .iter()
            .map(|(digest, object)| (digest.clone(), object.bytes().to_vec()))
            .collect();
        let replay = crate::ClosedBuildArtifactResolver::new(&closed, |content: &ContentRef| {
            objects.get(&content.sha256).cloned()
        });
        let replay_provider = PageProvider::new(&site, std::path::Path::new("/site/_includes"))
            .with_tree(tree)
            .with_pages_root(std::path::Path::new("/site"))
            .with_template_includes(std::path::Path::new("/template"))
            .with_engine_first(true)
            .with_artifact_resolver(replay);
        assert_eq!(render_page(source, "en/index.html", &replay_provider), html);
        assert_eq!(calls.get(), 1, "replay must not invoke the generator");
    }

    #[test]
    fn failed_attempt_is_cataloged_but_staged_fallback_is_the_successful_page_read() {
        let source = format!("---\n---\n{{% include {FRAGMENT} %}}");
        let mut tree = MemTree::new();
        tree.insert_text(
            std::path::Path::new("/site/_includes").join(FRAGMENT),
            "fallback",
        );
        let tree: Rc<dyn TreeSource> = Rc::new(tree);
        let site = SiteData::from_map(&serde_json::json!({}));
        let provider = PageProvider::new(&site, std::path::Path::new("/site/_includes"))
            .with_tree(tree)
            .with_engine_first(true)
            .with_artifact_resolver(CountingResolver {
                calls: Rc::new(Cell::new(0)),
                result: Err(ArtifactResolveError::unsupported(
                    "publisher.fragment.history",
                    "not implemented",
                )),
            });
        let html = render_page(&source, "index.html", &provider);
        assert_eq!(html, "fallback");
        let reads = provider.page_artifact_reads();
        let fragment = reads.requested().iter().next().unwrap().clone();
        assert!(!reads.read().contains(&fragment));
        assert!(reads.input_reads().contains(&stock_input_artifact(
            STOCK_STAGED_INCLUDE_NAMESPACE,
            FRAGMENT
        )));

        let base = predecessor();
        let successor = collect_stock_revision(
            &base,
            [
                input(
                    STOCK_PAGE_SOURCE_NAMESPACE,
                    "index.html",
                    source.as_bytes(),
                    "text/html",
                ),
                input(
                    STOCK_STAGED_INCLUDE_NAMESPACE,
                    FRAGMENT,
                    b"fallback",
                    "text/html",
                ),
            ],
            [StockPage {
                path: path("index.html"),
                outcome: StockPageOutcome::Ready {
                    bytes: html.into_bytes(),
                    reads,
                },
                provenance: provenance("page"),
            }],
            [],
            StockFragmentPolicy {
                provenance: provenance("fragment"),
                reads: all_compile_inputs(&base),
            },
        )
        .unwrap();
        assert!(matches!(
            successor
                .site_build()
                .artifacts()
                .get(&fragment)
                .unwrap()
                .state,
            ArtifactState::Unsupported { .. }
        ));
        successor.into_site_build().close().unwrap();
    }

    #[test]
    fn non_ready_advertised_page_is_a_typed_seal_blocker() {
        let base = predecessor();
        let successor = collect_stock_revision(
            &base,
            [],
            [StockPage {
                path: path("broken.html"),
                outcome: StockPageOutcome::NonReady {
                    state: ArtifactState::Failed {
                        diagnostics: BTreeSet::from([site_build::BuildDiagnostic::new(
                            DiagnosticSeverity::Error,
                            "page.render",
                            "render failed",
                        )]),
                    },
                    reads: BTreeSet::new(),
                },
                provenance: provenance("page"),
            }],
            [],
            StockFragmentPolicy {
                provenance: provenance("fragment"),
                reads: all_compile_inputs(&base),
            },
        )
        .unwrap();
        assert!(matches!(
            successor.into_site_build().close().unwrap_err().blockers(),
            [site_build::SealBlocker::Failed { .. }]
        ));
    }

    #[test]
    fn conflicting_fragment_observations_fail_on_the_same_typed_key_in_any_order() {
        let base = predecessor();
        let fragment = crate::legacy_include_to_artifact_key(FRAGMENT).unwrap();
        let page = |name: &str, bytes: &[u8]| {
            let mut reads = PageArtifactReadSet::default();
            reads.request(fragment.clone());
            reads.record_read(fragment.clone());
            reads.observe(
                fragment.clone(),
                ArtifactObservation::Ready {
                    bytes: bytes.to_vec(),
                },
            );
            reads.add_input_object(
                stock_input_artifact(STOCK_PAGE_SOURCE_NAMESPACE, name),
                name.as_bytes(),
            );
            StockPage {
                path: path(name),
                outcome: StockPageOutcome::Ready {
                    bytes: name.as_bytes().to_vec(),
                    reads,
                },
                provenance: provenance("page"),
            }
        };
        let one = page("one.html", b"one");
        let two = page("two.html", b"two");
        let inputs = || {
            [
                input(
                    STOCK_PAGE_SOURCE_NAMESPACE,
                    "one.html",
                    b"one.html",
                    "text/html",
                ),
                input(
                    STOCK_PAGE_SOURCE_NAMESPACE,
                    "two.html",
                    b"two.html",
                    "text/html",
                ),
            ]
        };
        let policy = || StockFragmentPolicy {
            provenance: provenance("fragment"),
            reads: all_compile_inputs(&base),
        };
        for pages in [
            vec![one.clone(), two.clone()],
            vec![two.clone(), one.clone()],
        ] {
            let error = collect_stock_revision(&base, inputs(), pages, [], policy()).unwrap_err();
            assert!(matches!(
                error,
                StockPlanError::ConflictingObservation(ref key) if key == &fragment
            ));
        }
    }
}
