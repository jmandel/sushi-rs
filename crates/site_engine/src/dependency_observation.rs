//! Private aggregation of `dependency-observation/v1` evidence.
//!
//! Compiled only for the explicit non-default `dependency-observation` feature.
//! This module observes the canonical full build; it never selects, skips, or
//! reuses work.

use std::collections::BTreeMap;

use ::dependency_observation::{
    Edge, EdgeKind, Evidence, EvidenceScope, Node, NodeId, NodeKind, ObservationGraph,
    PackageLookupObservation, PackageLookupTrace, SourceLocation,
};

use crate::{CompilationOutcome, OutputDescriptor};

pub(crate) struct BuildDependencyObservation {
    graph: ObservationGraph,
    sources_by_path: BTreeMap<String, NodeId>,
}

impl BuildDependencyObservation {
    pub(crate) fn capture(
        compilation: &CompilationOutcome,
        package_lookups: &PackageLookupTrace,
        prepared: &site_build::PreparedGuide,
        build: &site_build::ClosedSiteBuild,
        catalog: &[OutputDescriptor],
    ) -> Self {
        Self::capture_checked(compilation, package_lookups, prepared, build, catalog)
            .unwrap_or_else(|message| {
                Self::unavailable(format!("dependency observation capture failed: {message}"))
            })
    }

    fn capture_checked(
        compilation: &CompilationOutcome,
        package_lookups: &PackageLookupTrace,
        prepared: &site_build::PreparedGuide,
        build: &site_build::ClosedSiteBuild,
        catalog: &[OutputDescriptor],
    ) -> Result<Self, String> {
        let mut observation = Self {
            graph: ObservationGraph::default(),
            sources_by_path: BTreeMap::new(),
        };
        observation.capture_sources(build)?;
        observation.capture_compilation(compilation)?;
        observation.capture_package_lookups(build, &package_lookups.observations)?;
        if package_lookups.overflowed {
            observation
                .graph
                .record_global_unknown("package lookup observation exceeded its bounded capacity");
        }
        observation.capture_prepared(prepared)?;
        observation.capture_build(build)?;
        observation.capture_catalog(catalog)?;

        // These are factual gaps, not TODOs disguised as broad dependencies.
        // Their presence makes every incremental decision fail closed while
        // still preserving the exact edges already observed above.
        for reason in [
            "compiler alias/rule/RuleSet provenance is incomplete",
            "compiler composite local/predefined/package lookup precedence and negative namespaces are incomplete",
            "snapshot candidate, winner, miss, and cached transitive reads are incomplete",
            "PreparedGuide field-level derivation is conservative",
            "fragment resource/package/terminology reads are unobserved",
            "site namespace enumeration and negative Liquid reads are incomplete",
        ] {
            observation.graph.record_global_unknown(reason);
        }
        Ok(observation)
    }

    pub(crate) fn restored(
        build: &site_build::ClosedSiteBuild,
        catalog: &[OutputDescriptor],
    ) -> Self {
        Self::restored_checked(build, catalog).unwrap_or_else(|message| {
            Self::unavailable(format!(
                "dependency observation rehydration failed: {message}"
            ))
        })
    }

    fn restored_checked(
        build: &site_build::ClosedSiteBuild,
        catalog: &[OutputDescriptor],
    ) -> Result<Self, String> {
        let mut observation = Self {
            graph: ObservationGraph::default(),
            sources_by_path: BTreeMap::new(),
        };
        observation.capture_sources(build)?;
        observation.capture_build(build)?;
        observation.capture_catalog(catalog)?;
        observation.graph.record_global_unknown(
            "restored build carries no compiler/PreparedGuide observation trace",
        );
        Ok(observation)
    }

    pub(crate) fn unavailable(reason: impl Into<String>) -> Self {
        let mut graph = ObservationGraph::default();
        graph.record_global_unknown(reason);
        Self {
            graph,
            sources_by_path: BTreeMap::new(),
        }
    }

    fn contain_failure(&mut self, phase: &str, result: Result<(), String>) -> Result<(), String> {
        if let Err(message) = result {
            *self = Self::unavailable(format!("dependency observation {phase} failed: {message}"));
        }
        Ok(())
    }

    fn capture_sources(&mut self, build: &site_build::ClosedSiteBuild) -> Result<(), String> {
        for (path, source) in build.site_build().project().sources.iter() {
            let id = typed_id("source", path)?;
            let mut node = Node::new(id.clone(), NodeKind::Source, path.to_string());
            node.content_digest = Some(source.content.sha256.to_string());
            node.attributes
                .insert("kind".into(), format!("{:?}", source.kind));
            self.graph.insert_node(node).map_err(display)?;
            self.graph
                .record_evidence(id.clone(), Evidence::Complete)
                .map_err(display)?;
            self.sources_by_path.insert(path.to_string(), id);
        }
        Ok(())
    }

    fn capture_compilation(&mut self, compilation: &CompilationOutcome) -> Result<(), String> {
        for resource in &compilation.resources {
            let compiled_id = typed_id("compiled", &resource.filename)?;
            let mut compiled = Node::new(
                compiled_id.clone(),
                NodeKind::CompiledResource,
                resource.filename.clone(),
            );
            compiled.content_digest =
                Some(site_build::Sha256Digest::of_bytes(resource.text.as_bytes()).to_string());
            if let Some(resource_type) = &resource.resource_type {
                compiled
                    .attributes
                    .insert("resourceType".into(), resource_type.clone());
            }
            if let Some(id) = &resource.id {
                compiled.attributes.insert("id".into(), id.clone());
            }
            self.graph.insert_node(compiled).map_err(display)?;

            if let Some(definition) = &resource.definition {
                let definition_id = typed_id(
                    "declaration",
                    &(
                        &definition.path,
                        &definition.kind,
                        &resource.resource_type,
                        &resource.id,
                        &resource.filename,
                    ),
                )?;
                let mut declaration = Node::new(
                    definition_id.clone(),
                    NodeKind::Declaration,
                    format!(
                        "{}:{}:{}",
                        definition.path, definition.line, definition.column
                    ),
                );
                declaration.location = Some(SourceLocation {
                    path: definition.path.clone(),
                    line: Some(definition.line),
                    column: Some(definition.column),
                });
                self.graph.insert_node(declaration).map_err(display)?;
                if let Some(source_id) = self.sources_by_path.get(&definition.path) {
                    self.graph
                        .insert_edge(Edge {
                            from: definition_id.clone(),
                            to: source_id.clone(),
                            kind: EdgeKind::Reads,
                        })
                        .map_err(display)?;
                } else {
                    self.graph.record_global_unknown(format!(
                        "compiled declaration {} has no captured project source",
                        definition.path
                    ));
                }
                self.graph
                    .insert_edge(Edge {
                        from: compiled_id.clone(),
                        to: definition_id.clone(),
                        kind: EdgeKind::Derives,
                    })
                    .map_err(display)?;
                self.graph
                    .record_evidence(
                        definition_id,
                        Evidence::Unknown {
                            phase: "compile".into(),
                            reason: "rules, aliases, RuleSets, and lookup namespaces are not yet attributed to this declaration"
                                .into(),
                            location: Some(SourceLocation {
                                path: definition.path.clone(),
                                line: Some(definition.line),
                                column: Some(definition.column),
                            }),
                        },
                    )
                    .map_err(display)?;
            }
            self.graph
                .record_evidence(
                    compiled_id.clone(),
                    Evidence::Unknown {
                        phase: "compile".into(),
                        reason: "rule, alias, RuleSet, and composite lookup edges are incomplete"
                            .into(),
                        location: resource
                            .definition
                            .as_ref()
                            .map(|definition| SourceLocation {
                                path: definition.path.clone(),
                                line: Some(definition.line),
                                column: Some(definition.column),
                            }),
                    },
                )
                .map_err(display)?;
        }
        for diagnostic in &compilation.diagnostics {
            if diagnostic.owner_definition.is_none() {
                self.graph.record_global_unknown(format!(
                    "unattributed compiler diagnostic: {}",
                    diagnostic.message
                ));
            }
        }
        Ok(())
    }

    fn capture_package_lookups(
        &mut self,
        build: &site_build::ClosedSiteBuild,
        lookups: &[PackageLookupObservation],
    ) -> Result<(), String> {
        let carrier = build
            .site_build()
            .package_lock()
            .iter()
            .map(|(coordinate, package)| {
                (coordinate.to_string(), package.content.sha256.to_string())
            })
            .collect::<BTreeMap<_, _>>();
        for lookup in lookups {
            let lookup_id = typed_id(
                "lookup",
                &(
                    format!("{:?}", lookup.operation),
                    &lookup.query,
                    &lookup.requested_types,
                ),
            )?;
            let mut lookup_node =
                Node::new(lookup_id.clone(), NodeKind::Lookup, lookup.query.clone());
            lookup_node
                .attributes
                .insert("operation".into(), format!("{:?}", lookup.operation));
            lookup_node
                .attributes
                .insert("requestedTypes".into(), lookup.requested_types.join(","));
            self.graph.insert_node(lookup_node).map_err(display)?;

            for candidate in &lookup.candidates {
                let candidate_id = typed_id(
                    "package-resource",
                    &(
                        &candidate.package,
                        &candidate.member,
                        &candidate.resource_type,
                        &candidate.id,
                        &candidate.url,
                        &candidate.version,
                    ),
                )?;
                let authority = match (&candidate.package, &candidate.member) {
                    (Some(package), Some(member)) => format!("{package}/{member}"),
                    _ => format!(
                        "unattributed package candidate {}/{}",
                        candidate.resource_type, candidate.id
                    ),
                };
                let mut node =
                    Node::new(candidate_id.clone(), NodeKind::PackageResource, authority);
                node.attributes
                    .insert("resourceType".into(), candidate.resource_type.clone());
                node.attributes.insert("id".into(), candidate.id.clone());
                node.attributes
                    .insert("fishType".into(), candidate.fish_type.clone());
                if let Some(digest) = &candidate.member_digest {
                    node.content_digest = Some(digest.clone());
                }
                let authenticated =
                    match (candidate.package.as_deref(), candidate.member.as_deref()) {
                        (Some(package), Some(_)) if carrier.contains_key(package) => {
                            node.attributes
                                .insert("carrierSha256".into(), carrier[package].clone());
                            true
                        }
                        // The compiler's built-in virtual support definitions have
                        // no PackageLock carrier.
                        (Some("sushi-r5forR4#1.0.0"), Some(_))
                            if candidate.member_digest.is_some() =>
                        {
                            true
                        }
                        (Some("sushi-r5forR4#1.0.0"), _) => {
                            self.graph.record_global_unknown(format!(
                                "embedded lookup candidate {} has incomplete member authority",
                                candidate.load_sequence
                            ));
                            false
                        }
                        (Some(package), Some(_)) => {
                            self.graph.record_global_unknown(format!(
                                "lookup candidate {} has no locked carrier",
                                package
                            ));
                            false
                        }
                        _ => {
                            self.graph.record_global_unknown(format!(
                                "lookup candidate {} has no package/member authority",
                                candidate.load_sequence
                            ));
                            false
                        }
                    };
                self.graph.insert_node(node).map_err(display)?;
                self.graph
                    .record_evidence(
                        candidate_id.clone(),
                        if authenticated {
                            Evidence::Complete
                        } else {
                            Evidence::Unknown {
                                phase: "package-lookup".into(),
                                reason: "candidate bytes lack exact package/member authority"
                                    .into(),
                                location: None,
                            }
                        },
                    )
                    .map_err(display)?;
                self.graph
                    .insert_edge(Edge {
                        from: lookup_id.clone(),
                        to: candidate_id.clone(),
                        kind: EdgeKind::Candidate,
                    })
                    .map_err(display)?;
                if lookup.eligible.contains(candidate) {
                    self.graph
                        .insert_edge(Edge {
                            from: lookup_id.clone(),
                            to: candidate_id.clone(),
                            kind: EdgeKind::Eligible,
                        })
                        .map_err(display)?;
                }
                if lookup.winner.as_ref() == Some(candidate) {
                    self.graph
                        .insert_edge(Edge {
                            from: lookup_id.clone(),
                            to: candidate_id,
                            kind: EdgeKind::Winner,
                        })
                        .map_err(display)?;
                }
            }
            if lookup.winner.is_none() {
                let namespace_id = typed_id(
                    "lookup-namespace",
                    &(&lookup.query, &lookup.requested_types),
                )?;
                self.graph
                    .insert_node(Node::new(
                        namespace_id.clone(),
                        NodeKind::LookupNamespace,
                        format!("{} [{}]", lookup.query, lookup.requested_types.join(",")),
                    ))
                    .map_err(display)?;
                self.graph
                    .record_evidence(
                        namespace_id.clone(),
                        Evidence::Conservative {
                            scope: EvidenceScope::Namespace(format!(
                                "package:{} [{}]",
                                lookup.query,
                                lookup.requested_types.join(",")
                            )),
                            reason: "negative package lookup is exact; candidate namespace changes must invalidate it"
                                .into(),
                        },
                    )
                    .map_err(display)?;
                self.graph
                    .insert_edge(Edge {
                        from: lookup_id.clone(),
                        to: namespace_id,
                        kind: EdgeKind::Miss,
                    })
                    .map_err(display)?;
            }
            let evidence = match (&lookup.winner, lookup.body_read) {
                (Some(_), ::dependency_observation::BodyReadOutcome::Ready)
                | (None, ::dependency_observation::BodyReadOutcome::NotAttempted) => {
                    Evidence::Complete
                }
                (Some(_), ::dependency_observation::BodyReadOutcome::MissingOrInvalid) => {
                    Evidence::Unknown {
                        phase: "package-lookup".into(),
                        reason: "winning package member was missing, unreadable, or invalid JSON"
                            .into(),
                        location: None,
                    }
                }
                (Some(_), ::dependency_observation::BodyReadOutcome::NotAttempted) => {
                    Evidence::Unknown {
                        phase: "package-lookup".into(),
                        reason: "winning package member was not read".into(),
                        location: None,
                    }
                }
                (None, ::dependency_observation::BodyReadOutcome::Ready)
                | (None, ::dependency_observation::BodyReadOutcome::MissingOrInvalid) => {
                    Evidence::Unknown {
                        phase: "package-lookup".into(),
                        reason: "body-read outcome exists without a winning candidate".into(),
                        location: None,
                    }
                }
            };
            self.graph
                .record_evidence(lookup_id, evidence)
                .map_err(display)?;
        }
        Ok(())
    }

    fn capture_prepared(&mut self, prepared: &site_build::PreparedGuide) -> Result<(), String> {
        let guide_id = typed_id(
            "prepared-guide",
            &(&prepared.guide.package_id, &prepared.guide.version),
        )?;
        self.graph
            .insert_node(Node::new(
                guide_id.clone(),
                NodeKind::PreparedGuide,
                prepared.guide.package_id.clone(),
            ))
            .map_err(display)?;
        self.graph
            .record_evidence(
                guide_id.clone(),
                Evidence::Conservative {
                    scope: EvidenceScope::Node(guide_id.clone()),
                    reason: "field-level resource/snapshot/augmentation reads are not complete"
                        .into(),
                },
            )
            .map_err(display)?;
        for resource in &prepared.resources {
            let digest = site_build::sha256_canonical(&resource.resource)
                .map_err(|error| format!("hash PreparedGuide resource: {error}"))?;
            let resource_id = typed_id(
                "prepared-resource",
                &(&resource.key.resource_type, &resource.key.id),
            )?;
            let mut node = Node::new(
                resource_id.clone(),
                NodeKind::PreparedResource,
                format!("{}/{}", resource.key.resource_type, resource.key.id),
            );
            node.content_digest = Some(digest.to_string());
            self.graph.insert_node(node).map_err(display)?;
            self.graph
                .insert_edge(Edge {
                    from: guide_id.clone(),
                    to: resource_id.clone(),
                    kind: EdgeKind::Contains,
                })
                .map_err(display)?;
            self.graph
                .record_evidence(
                    resource_id,
                    Evidence::Unknown {
                        phase: "snapshot/prepared-guide".into(),
                        reason:
                            "compiled-to-snapshot and snapshot-to-prepared reads are incomplete"
                                .into(),
                        location: None,
                    },
                )
                .map_err(display)?;
        }
        Ok(())
    }

    fn capture_build(&mut self, build: &site_build::ClosedSiteBuild) -> Result<(), String> {
        let site_build = build.site_build();
        for (key, record) in site_build.artifacts().iter() {
            let artifact_id = self.ensure_artifact(key)?;
            for read in &record.reads {
                let dependency_id = match read {
                    site_build::ReadDependency::Source { path } => site_build
                        .project()
                        .sources
                        .get(path)
                        .map(|_| typed_id("source", path))
                        .transpose()?
                        .ok_or_else(|| {
                            format!("closed artifact references absent source {path}")
                        })?,
                    site_build::ReadDependency::Package { coordinate } => {
                        let package =
                            site_build.package_lock().get(coordinate).ok_or_else(|| {
                                format!("closed artifact references absent package {coordinate}")
                            })?;
                        let id = typed_id("package", coordinate)?;
                        let mut node = Node::new(
                            id.clone(),
                            NodeKind::LookupNamespace,
                            coordinate.to_string(),
                        );
                        node.content_digest = Some(package.content.sha256.to_string());
                        self.graph.insert_node(node).map_err(display)?;
                        id
                    }
                    site_build::ReadDependency::Artifact { key } => self.ensure_artifact(key)?,
                    site_build::ReadDependency::Content { sha256 } => {
                        let id = typed_id("content", sha256)?;
                        let mut node =
                            Node::new(id.clone(), NodeKind::Artifact, sha256.to_string());
                        node.content_digest = Some(sha256.to_string());
                        self.graph.insert_node(node).map_err(display)?;
                        id
                    }
                };
                self.graph
                    .insert_edge(Edge {
                        from: artifact_id.clone(),
                        to: dependency_id,
                        kind: EdgeKind::Reads,
                    })
                    .map_err(display)?;
            }
            self.graph
                .record_evidence(
                    artifact_id.clone(),
                    Evidence::Conservative {
                        scope: EvidenceScope::Node(artifact_id),
                        reason: format!(
                            "SiteBuild reads are exact, but producer {} may report a broader or incomplete causal set",
                            record.provenance.producer.id
                        ),
                    },
                )
                .map_err(display)?;
        }
        Ok(())
    }

    fn capture_catalog(&mut self, catalog: &[OutputDescriptor]) -> Result<(), String> {
        for output in catalog {
            let id = page_id(&output.path)?;
            let kind = if output.kind == crate::OutputKind::Page {
                NodeKind::Page
            } else {
                NodeKind::FinalOutput
            };
            let mut node = Node::new(id.clone(), kind, output.path.to_string());
            if let Some(content) = &output.content {
                node.content_digest = Some(content.sha256.to_string());
            }
            node.attributes
                .insert("kind".into(), format!("{:?}", output.kind));
            node.attributes
                .insert("mediaType".into(), output.media_type.clone());
            if let Some(title) = &output.title {
                node.attributes.insert("title".into(), title.clone());
            }
            if let Some(subject) = &output.subject {
                node.attributes.insert(
                    "subject".into(),
                    format!("{}/{}", subject.resource_type, subject.id),
                );
            }
            if let Some(subject_page) = output.subject_page {
                node.attributes
                    .insert("subjectPage".into(), format!("{subject_page:?}"));
            }
            if let Some(page_kind) = output.page_kind {
                node.attributes
                    .insert("pageKind".into(), format!("{page_kind:?}"));
            }
            self.graph.insert_node(node).map_err(display)?;
            self.graph
                .record_evidence(
                    id.clone(),
                    if output.kind == crate::OutputKind::Page {
                        Evidence::Unknown {
                            phase: "output-catalog".into(),
                            reason: "page membership depends on incompletely observed namespace enumeration"
                                .into(),
                            location: None,
                        }
                    } else {
                        Evidence::Conservative {
                            scope: EvidenceScope::Node(id.clone()),
                            reason: "output bytes are exact, but ready-asset origin and alias enumeration are not yet linked"
                                .into(),
                        }
                    },
                )
                .map_err(display)?;
        }
        Ok(())
    }

    pub(crate) fn record_page(
        &mut self,
        path: &site_build::OutputPath,
        reads: render_page::PageArtifactReadSet,
    ) -> Result<(), String> {
        let result = self.record_page_checked(path, reads);
        self.contain_failure("page capture", result)
    }

    fn record_page_checked(
        &mut self,
        path: &site_build::OutputPath,
        reads: render_page::PageArtifactReadSet,
    ) -> Result<(), String> {
        let page = page_id(path)?;
        let dependencies = reads.dependencies();
        for key in &dependencies {
            let dependency = self.ensure_artifact(key)?;
            self.graph
                .insert_edge(Edge {
                    from: page.clone(),
                    to: dependency.clone(),
                    kind: EdgeKind::Reads,
                })
                .map_err(display)?;
            if !matches!(key, site_build::ArtifactKey::Fragment { .. }) {
                self.graph
                    .record_evidence(dependency, Evidence::Complete)
                    .map_err(display)?;
            }
        }
        for key in reads.requested().difference(&dependencies) {
            if reads.observations().contains_key(key) {
                continue;
            }
            let dependency = self.ensure_artifact(key)?;
            self.graph
                .insert_edge(Edge {
                    from: page.clone(),
                    to: dependency.clone(),
                    kind: EdgeKind::Miss,
                })
                .map_err(display)?;
            self.graph
                .record_evidence(
                    dependency,
                    Evidence::Conservative {
                        scope: EvidenceScope::Namespace(format!("{key:?}")),
                        reason: "negative lookup is exact for this key; namespace membership changes must invalidate it"
                            .into(),
                    },
                )
                .map_err(display)?;
        }
        for (key, outcome) in reads.observations() {
            let artifact = self.ensure_artifact(key)?;
            match outcome {
                render_page::ArtifactObservation::Ready { bytes } => {
                    let digest = site_build::Sha256Digest::of_bytes(bytes);
                    let content_id = typed_id("content", &digest)?;
                    let mut content =
                        Node::new(content_id.clone(), NodeKind::Artifact, digest.to_string());
                    content.content_digest = Some(digest.to_string());
                    self.graph.insert_node(content).map_err(display)?;
                    self.graph
                        .record_evidence(content_id.clone(), Evidence::Complete)
                        .map_err(display)?;
                    self.graph
                        .insert_edge(Edge {
                            from: artifact.clone(),
                            to: content_id,
                            kind: EdgeKind::Publishes,
                        })
                        .map_err(display)?;
                    self.graph
                        .record_evidence(
                            artifact,
                            Evidence::Conservative {
                                scope: EvidenceScope::Build,
                                reason: "fragment result bytes are exact; resource, package, and terminology reads inside the fragment engine are not observed"
                                    .into(),
                            },
                        )
                        .map_err(display)?;
                }
                render_page::ArtifactObservation::NotReady { error } => {
                    self.graph
                        .insert_edge(Edge {
                            from: page.clone(),
                            to: artifact.clone(),
                            kind: EdgeKind::Attempts,
                        })
                        .map_err(display)?;
                    let reason = match error.failure() {
                        render_page::ArtifactResolveFailure::Deferred { reason } => {
                            format!("deferred fragment: {reason}")
                        }
                        render_page::ArtifactResolveFailure::Unsupported { capability, reason } => {
                            format!("unsupported fragment capability {capability}: {reason}")
                        }
                        render_page::ArtifactResolveFailure::Failed { code, message } => {
                            format!("failed fragment {code}: {message}")
                        }
                    };
                    self.graph
                        .record_evidence(
                            artifact,
                            Evidence::Unknown {
                                phase: "fragment".into(),
                                reason,
                                location: None,
                            },
                        )
                        .map_err(display)?;
                }
            }
        }
        for (key, values) in reads.input_objects() {
            let input = self.ensure_artifact(key)?;
            if values.len() != 1 {
                self.graph
                    .record_evidence(
                        input.clone(),
                        Evidence::Unknown {
                            phase: "page-render".into(),
                            reason: format!(
                                "one logical page input produced {} distinct byte bodies",
                                values.len()
                            ),
                            location: None,
                        },
                    )
                    .map_err(display)?;
            }
            for bytes in values {
                let digest = site_build::Sha256Digest::of_bytes(bytes);
                let object_id = typed_id("content", &digest)?;
                let mut object =
                    Node::new(object_id.clone(), NodeKind::Artifact, digest.to_string());
                object.content_digest = Some(digest.to_string());
                self.graph.insert_node(object).map_err(display)?;
                self.graph
                    .record_evidence(object_id.clone(), Evidence::Complete)
                    .map_err(display)?;
                self.graph
                    .insert_edge(Edge {
                        from: input.clone(),
                        to: object_id,
                        kind: EdgeKind::Publishes,
                    })
                    .map_err(display)?;
            }
        }
        self.graph
            .record_evidence(
                page.clone(),
                Evidence::Unknown {
                    phase: "page-render".into(),
                    reason: "tracked successes and misses are exact; namespace enumeration and fragment internals remain incomplete"
                        .into(),
                    location: None,
                },
            )
            .map_err(display)?;
        Ok(())
    }

    pub(crate) fn record_html_post_pass(
        &mut self,
        path: &site_build::OutputPath,
        observation: &site_producer::publisher_runtime::FinishHtmlObservation,
    ) -> Result<(), String> {
        let result = self.record_html_post_pass_checked(path, observation);
        self.contain_failure("HTML post-pass capture", result)
    }

    fn record_html_post_pass_checked(
        &mut self,
        path: &site_build::OutputPath,
        observation: &site_producer::publisher_runtime::FinishHtmlObservation,
    ) -> Result<(), String> {
        let page = page_id(path)?;
        for runtime_path in &observation.attempted {
            if let Some(digest) = observation.ready.get(runtime_path) {
                let id = typed_id("runtime-input", runtime_path)?;
                let mut node = Node::new(id.clone(), NodeKind::RuntimePass, runtime_path.clone());
                node.content_digest = Some(digest.to_string());
                self.graph.insert_node(node).map_err(display)?;
                self.graph
                    .record_evidence(id.clone(), Evidence::Complete)
                    .map_err(display)?;
                self.graph
                    .insert_edge(Edge {
                        from: page.clone(),
                        to: id,
                        kind: EdgeKind::PostProcesses,
                    })
                    .map_err(display)?;
            } else {
                let id = typed_id("runtime-miss", runtime_path)?;
                self.graph
                    .insert_node(Node::new(
                        id.clone(),
                        NodeKind::LookupNamespace,
                        runtime_path.clone(),
                    ))
                    .map_err(display)?;
                self.graph
                    .record_evidence(
                        id.clone(),
                        Evidence::Conservative {
                            scope: EvidenceScope::Namespace(runtime_path.clone()),
                            reason: "negative runtime lookup is exact for this path; runtime namespace changes must invalidate it"
                                .into(),
                        },
                    )
                    .map_err(display)?;
                self.graph
                    .insert_edge(Edge {
                        from: page.clone(),
                        to: id,
                        kind: EdgeKind::Miss,
                    })
                    .map_err(display)?;
            }
        }
        for name in &observation.generated_table_backgrounds {
            let id = typed_id("runtime-generated-table-background", name)?;
            let mut node = Node::new(id.clone(), NodeKind::RuntimePass, name.clone());
            node.attributes.insert(
                "recipe".into(),
                "publisher-runtime.table-background-data-uri/v1".into(),
            );
            self.graph.insert_node(node).map_err(display)?;
            self.graph
                .record_evidence(id.clone(), Evidence::Complete)
                .map_err(display)?;
            self.graph
                .insert_edge(Edge {
                    from: page.clone(),
                    to: id,
                    kind: EdgeKind::PostProcesses,
                })
                .map_err(display)?;
        }
        Ok(())
    }

    pub(crate) fn record_page_output(
        &mut self,
        path: &site_build::OutputPath,
        content: &site_build::ContentRef,
    ) -> Result<(), String> {
        let result = self.record_page_output_checked(path, content);
        self.contain_failure("page-output capture", result)
    }

    fn record_page_output_checked(
        &mut self,
        path: &site_build::OutputPath,
        content: &site_build::ContentRef,
    ) -> Result<(), String> {
        let page = page_id(path)?;
        let content_id = typed_id("page-output-content", path)?;
        let mut node = Node::new(content_id.clone(), NodeKind::FinalOutput, path.to_string());
        node.content_digest = Some(content.sha256.to_string());
        node.attributes
            .insert("byteLength".into(), content.byte_length.to_string());
        if let Some(media_type) = &content.media_type {
            node.attributes
                .insert("mediaType".into(), media_type.clone());
        }
        self.graph.insert_node(node).map_err(display)?;
        self.graph
            .record_evidence(content_id.clone(), Evidence::Complete)
            .map_err(display)?;
        self.graph
            .insert_edge(Edge {
                from: page,
                to: content_id,
                kind: EdgeKind::Publishes,
            })
            .map_err(display)
    }

    #[cfg(test)]
    pub(crate) fn decision_for_page(
        &self,
        path: &site_build::OutputPath,
    ) -> Result<::dependency_observation::RebuildDecision, String> {
        Ok(self.graph.assess([page_id(path)?], []))
    }

    #[cfg(test)]
    pub(crate) fn graph(&self) -> &ObservationGraph {
        &self.graph
    }

    fn ensure_artifact(&mut self, key: &site_build::ArtifactKey) -> Result<NodeId, String> {
        let id = typed_id("artifact", key)?;
        let kind = if matches!(key, site_build::ArtifactKey::Fragment { .. }) {
            NodeKind::Fragment
        } else {
            NodeKind::Artifact
        };
        self.graph
            .insert_node(Node::new(id.clone(), kind, format!("{key:?}")))
            .map_err(display)?;
        Ok(id)
    }
}

fn page_id(path: &site_build::OutputPath) -> Result<NodeId, String> {
    typed_id("output", path)
}

fn typed_id(prefix: &str, value: &impl serde::Serialize) -> Result<NodeId, String> {
    let digest = site_build::sha256_canonical(value)
        .map_err(|error| format!("hash dependency node {prefix}: {error}"))?;
    NodeId::new(format!("{prefix}:{digest}")).map_err(display)
}

fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use ::dependency_observation::{EdgeKind, RebuildDecision};
    use serde_json::json;

    use super::*;

    fn closed_build() -> site_build::ClosedSiteBuild {
        let source_path = site_build::SourcePath::parse("input/fsh/test.fsh").unwrap();
        let source = site_build::ContentRef::of_bytes(b"hello", Some("text/markdown"));
        let project = site_build::ProjectIdentity {
            project_id: "test".into(),
            revision: "a".into(),
            sources: site_build::SourceManifest::from_entries([(
                source_path.clone(),
                site_build::SourceEntry {
                    kind: site_build::SourceKind::Page,
                    content: source,
                },
            )])
            .unwrap(),
        };
        let artifact_key = site_build::ArtifactKey::Data {
            namespace: "test".into(),
            name: "page".into(),
        };
        let artifact_bytes = b"artifact";
        let artifact = site_build::ArtifactRecord {
            key: artifact_key.clone(),
            state: site_build::ArtifactState::Ready {
                content: site_build::ContentRef::of_bytes(artifact_bytes, Some("text/plain")),
            },
            provenance: site_build::ArtifactProvenance {
                producer: site_build::ProducerRef::new("test", "1"),
                recipe: "test/v1".into(),
                attributes: BTreeMap::new(),
            },
            reads: BTreeSet::from([site_build::ReadDependency::Source { path: source_path }]),
        };
        site_build::SiteBuild::new(
            project,
            site_build::PackageLock::default(),
            site_build::RenderTarget {
                renderer: site_build::ProducerRef::new("test", "1"),
                mode: site_build::RenderMode::NativeTemplate,
                fhir_version: "4.0.1".into(),
                template: None,
                parameters: BTreeMap::new(),
            },
            site_build::RenderPlan::new([artifact_key]),
            site_build::ArtifactCatalog::from_records([artifact]).unwrap(),
            BTreeSet::new(),
        )
        .unwrap()
        .close()
        .unwrap()
    }

    fn prepared_guide() -> site_build::PreparedGuide {
        site_build::PreparedGuide {
            guide: site_build::GuideIdentity {
                implementation_guide: site_build::SemanticResourceKey {
                    resource_type: "ImplementationGuide".into(),
                    id: "test".into(),
                },
                package_id: "test".into(),
                canonical: None,
                name: None,
                version: Some("1".into()),
                fhir_version: "4.0.1".into(),
                release_label: None,
                fhir_publication_base: "http://hl7.org/fhir/R4/".into(),
                generated: site_build::GeneratedIdentity {
                    epoch_seconds: 0,
                    date: "1970-01-01T00:00:00Z".into(),
                    day: "1970-01-01".into(),
                },
                source_control: None,
            },
            resources: vec![site_build::SemanticResource {
                key: site_build::SemanticResourceKey {
                    resource_type: "ImplementationGuide".into(),
                    id: "test".into(),
                },
                resource: json!({"resourceType":"ImplementationGuide","id":"test"}),
                publication: None,
            }],
            publisher_compatibility: None,
            expansions: Vec::new(),
            pages: Vec::new(),
            menu: Vec::new(),
            sushi_config: json!({}),
            authored_files: Vec::new(),
        }
    }

    fn compilation_outcome() -> CompilationOutcome {
        CompilationOutcome {
            resources: vec![crate::CompilationResource {
                filename: "StructureDefinition-test.json".into(),
                text: "{\"resourceType\":\"StructureDefinition\",\"id\":\"test\"}".into(),
                body: json!({"resourceType":"StructureDefinition","id":"test"}),
                resource_type: Some("StructureDefinition".into()),
                id: Some("test".into()),
                url: None,
                definition: Some(crate::CompilationDefinition {
                    kind: crate::CompilationDefinitionKind::FshDeclaration,
                    path: "input/fsh/test.fsh".into(),
                    line: 1,
                    column: 0,
                }),
            }],
            diagnostics: Vec::new(),
        }
    }

    fn page_descriptor(path: &site_build::OutputPath, media_type: &str) -> OutputDescriptor {
        OutputDescriptor {
            path: path.clone(),
            kind: crate::OutputKind::Page,
            media_type: media_type.into(),
            content: None,
            title: None,
            subject: None,
            subject_page: None,
            page_kind: None,
        }
    }

    fn embedded_candidate(load_sequence: usize) -> ::dependency_observation::LookupCandidate {
        ::dependency_observation::LookupCandidate {
            package: Some("sushi-r5forR4#1.0.0".into()),
            member: Some("StructureDefinition-Broken.json".into()),
            member_digest: Some("deadbeef".into()),
            resource_type: "StructureDefinition".into(),
            id: "Broken".into(),
            name: None,
            url: None,
            version: None,
            fish_type: "Resource".into(),
            load_sequence,
        }
    }

    #[test]
    fn captures_exact_existing_edges_but_fails_closed_on_known_gaps() {
        let build = closed_build();
        let prepared = prepared_guide();
        let path = site_build::OutputPath::parse("index.html").unwrap();
        let catalog = [page_descriptor(&path, "text/html")];
        let package_trace = ::dependency_observation::PackageLookupTrace {
            observations: vec![::dependency_observation::PackageLookupObservation {
                sequence: 0,
                operation: ::dependency_observation::LookupOperation::Fhir,
                query: "Broken".into(),
                requested_types: vec!["Resource".into()],
                candidates: vec![embedded_candidate(0)],
                eligible: Vec::new(),
                winner: Some(embedded_candidate(0)),
                body_read: ::dependency_observation::BodyReadOutcome::MissingOrInvalid,
            }],
            retained_bytes: 1,
            overflowed: false,
        };
        let mut observation = BuildDependencyObservation::capture(
            &compilation_outcome(),
            &package_trace,
            &prepared,
            &build,
            &catalog,
        );
        assert!(observation
            .graph()
            .edges()
            .iter()
            .any(|edge| edge.kind == EdgeKind::Reads));
        assert!(
            observation
                .graph()
                .edges()
                .iter()
                .filter(|edge| edge.kind == EdgeKind::Reads)
                .count()
                >= 2
        );
        assert!(observation
            .graph()
            .edges()
            .iter()
            .any(|edge| edge.kind == EdgeKind::Derives));
        assert!(observation
            .graph()
            .evidence()
            .values()
            .flatten()
            .any(|evidence| matches!(
                evidence,
                ::dependency_observation::Evidence::Unknown { phase, .. }
                    if phase == "package-lookup"
            )));
        let input_key = site_build::ArtifactKey::Data {
            namespace: "test.input".into(),
            name: "unstable".into(),
        };
        let mut reads = render_page::PageArtifactReadSet::default();
        reads.add_input_object(input_key, b"a".to_vec());
        reads.add_input_object(
            site_build::ArtifactKey::Data {
                namespace: "test.input".into(),
                name: "unstable".into(),
            },
            b"b".to_vec(),
        );
        observation.record_page(&path, reads).unwrap();
        let output = site_build::ContentRef::of_bytes(b"rendered", Some("text/html"));
        observation.record_page_output(&path, &output).unwrap();
        assert!(observation
            .graph()
            .evidence()
            .values()
            .flatten()
            .any(|evidence| matches!(
                evidence,
                ::dependency_observation::Evidence::Unknown { reason, .. }
                    if reason.contains("2 distinct byte bodies")
            )));
        assert!(observation
            .graph()
            .edges()
            .iter()
            .any(|edge| edge.kind == EdgeKind::Publishes));
        assert!(matches!(
            observation.decision_for_page(&path).unwrap(),
            RebuildDecision::FullBuild { .. }
        ));
    }

    #[test]
    fn duplicate_candidate_ordinals_share_authority_and_preserve_the_winner() {
        let first = embedded_candidate(3);
        let winner = embedded_candidate(7);
        let lookup = ::dependency_observation::PackageLookupObservation {
            sequence: 0,
            operation: ::dependency_observation::LookupOperation::Fhir,
            query: "Broken".into(),
            requested_types: vec!["Resource".into()],
            candidates: vec![first.clone(), winner.clone()],
            eligible: vec![first, winner.clone()],
            winner: Some(winner),
            body_read: ::dependency_observation::BodyReadOutcome::Ready,
        };
        let mut observation = BuildDependencyObservation {
            graph: ObservationGraph::default(),
            sources_by_path: BTreeMap::new(),
        };

        observation
            .capture_package_lookups(&closed_build(), &[lookup])
            .unwrap();

        let package_nodes = observation
            .graph()
            .nodes()
            .values()
            .filter(|node| node.kind == NodeKind::PackageResource)
            .collect::<Vec<_>>();
        assert_eq!(package_nodes.len(), 1);
        assert!(!package_nodes[0].attributes.contains_key("loadSequence"));
        assert!(observation
            .graph()
            .edges()
            .iter()
            .any(|edge| { edge.kind == EdgeKind::Winner && edge.to == package_nodes[0].id }));
    }

    #[test]
    fn capture_and_rehydration_conflicts_degrade_without_failing_the_build() {
        let build = closed_build();
        let path = site_build::OutputPath::parse("index.html").unwrap();
        let conflicting_catalog = [
            page_descriptor(&path, "text/html"),
            page_descriptor(&path, "application/xhtml+xml"),
        ];
        assert!(BuildDependencyObservation::capture_checked(
            &compilation_outcome(),
            &PackageLookupTrace::default(),
            &prepared_guide(),
            &build,
            &conflicting_catalog,
        )
        .is_err());

        let mut captured = BuildDependencyObservation::capture(
            &compilation_outcome(),
            &PackageLookupTrace::default(),
            &prepared_guide(),
            &build,
            &conflicting_catalog,
        );
        let RebuildDecision::FullBuild { reasons } = captured.decision_for_page(&path).unwrap()
        else {
            panic!("contained capture failure must force a full build")
        };
        assert!(reasons
            .iter()
            .any(|reason| reason.contains("capture failed")));
        // A later page observation against the deliberately minimal graph is
        // contained too; it cannot turn the already-successful canonical build
        // into a render error.
        captured
            .record_page(&path, render_page::PageArtifactReadSet::default())
            .unwrap();

        assert!(
            BuildDependencyObservation::restored_checked(&build, &conflicting_catalog).is_err()
        );
        let restored = BuildDependencyObservation::restored(&build, &conflicting_catalog);
        assert!(matches!(
            restored.decision_for_page(&path).unwrap(),
            RebuildDecision::FullBuild { .. }
        ));
        assert!(!build.site_build().build_id().as_str().is_empty());
    }
}
