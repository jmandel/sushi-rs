//! Emit the TypeScript declarations for the canonical public site wire values.
//!
//! This binary is feature-gated so the generator has no production or WASM
//! dependency. The umbrella editor checks in its output and gates drift.

use ts_rs::{Config, TS};

fn declaration<T: TS>(config: &Config) -> String {
    format!("export {}\n", T::decl(config))
}

fn schema<T: schemars::JsonSchema>(serialize: bool) -> schemars::Schema {
    let settings = if serialize {
        schemars::generate::SchemaSettings::draft2020_12().for_serialize()
    } else {
        schemars::generate::SchemaSettings::draft2020_12()
    };
    settings.into_generator().into_root_schema_for::<T>()
}

fn main() {
    if std::env::args().any(|argument| argument == "--schema") {
        let document = serde_json::json!({
            "schemaVersion": "fhir-site-wire-schemas/v1",
            "schemas": {
                "ProjectRevision": schema::<site_engine::ProjectRevision>(false),
                "CompilationOutcome": schema::<site_engine::CompilationOutcome>(true),
                "PreparedProjectResult": schema::<site_engine::PreparedProjectResult>(true),
                "TemplateResolution": schema::<site_engine::TemplateResolution>(true),
                "ResolutionStep": schema::<package_store::ResolutionStep>(true),
                "VersionIndex": schema::<package_store::VersionIndex>(false),
                "PackageMountResult": schema::<package_store::PackageMountResult>(true),
                "PrepareMountResult": schema::<package_store::PrepareMountResult>(true),
                "GeneratorSpec": schema::<site_engine::GeneratorSpec>(false),
                "ClosedSiteBuild": schema::<site_build::ClosedSiteBuild>(true),
                "ContentRef": schema::<content_store::ContentRef>(true),
                "OutputCatalog": schema::<site_engine::OutputCatalog>(true),
                "SiteOutput": schema::<site_build::SiteOutput>(true),
                "BuildEvent": schema::<site_engine::BuildEvent>(true),
                "BuildError": schema::<site_engine::BuildError<site_engine::CompilationOutcome>>(true),
            }
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&document).expect("serialize wire schemas")
        );
        return;
    }
    let config = Config::default().with_large_int("number");
    let declarations = vec![
        declaration::<site_engine::ProjectRevision>(&config),
        declaration::<site_engine::CompilationDefinitionKind>(&config),
        declaration::<site_engine::CompilationDefinition>(&config),
        declaration::<site_engine::CompilationDiagnosticSeverity>(&config),
        declaration::<site_engine::CompilationResource>(&config),
        declaration::<site_engine::CompilationDiagnostic>(&config),
        declaration::<site_engine::CompilationOutcome>(&config),
        declaration::<site_engine::TemplateResolution>(&config),
        declaration::<site_engine::GeneratorKind>(&config),
        declaration::<site_engine::PrepareResult>(&config),
        declaration::<site_engine::PreparedProjectResult>(&config),
        declaration::<package_store::PackageRequest>(&config),
        declaration::<package_store::RequestedSet>(&config),
        declaration::<package_store::MissingReason>(&config),
        declaration::<package_store::MissingPackage>(&config),
        declaration::<package_store::MutableVersionRequest>(&config),
        declaration::<package_store::ResolutionStep>(&config),
        declaration::<package_store::VersionIndex>(&config),
        declaration::<package_store::BundleInput>(&config),
        declaration::<package_store::BundleCompressionMetrics>(&config),
        declaration::<package_store::PreparedExport>(&config),
        declaration::<package_store::PreparedStageResult>(&config),
        declaration::<package_store::PrepareMountResult>(&config),
        declaration::<package_store::PackageMountResult>(&config),
        declaration::<api_envelope::ApiMessageError>(&config),
        declaration::<api_envelope::ApiSuccess<ts_rs::Dummy>>(&config),
        declaration::<api_envelope::ApiFailure<ts_rs::Dummy>>(&config),
        declaration::<api_envelope::ApiEnvelope<ts_rs::Dummy, ts_rs::Dummy>>(&config),
        declaration::<content_store::Sha256Digest>(&config),
        declaration::<content_store::ContentRef>(&config),
        declaration::<site_build::BuildId>(&config),
        declaration::<site_build::SchemaVersion>(&config),
        declaration::<site_build::SourcePath>(&config),
        declaration::<site_build::SourceKind>(&config),
        declaration::<site_build::SourceEntry>(&config),
        declaration::<site_build::SourceManifest>(&config),
        declaration::<site_build::ProjectIdentity>(&config),
        declaration::<site_build::PackageCoordinate>(&config),
        declaration::<site_build::LockedPackage>(&config),
        declaration::<site_build::PackageLock>(&config),
        declaration::<site_build::ProducerRef>(&config),
        declaration::<site_build::RenderMode>(&config),
        declaration::<site_build::RenderTarget>(&config),
        declaration::<site_build::ResourceKey>(&config),
        declaration::<site_build::FragmentKind>(&config),
        declaration::<site_build::FragmentScope>(&config),
        declaration::<site_build::AssetNamespace>(&config),
        declaration::<site_build::ArtifactKey>(&config),
        declaration::<site_build::ReadDependency>(&config),
        declaration::<site_build::ArtifactProvenance>(&config),
        declaration::<site_build::DiagnosticSeverity>(&config),
        declaration::<site_build::SourceLocation>(&config),
        declaration::<site_build::BuildDiagnostic>(&config),
        declaration::<site_build::ArtifactState>(&config),
        declaration::<site_build::ArtifactRecord>(&config),
        declaration::<site_build::ArtifactCatalog>(&config),
        declaration::<site_build::RenderPlan>(&config),
        declaration::<site_build::SiteBuild>(&config),
        declaration::<site_build::ClosedSiteBuild>(&config),
        declaration::<site_build::RendererImplementation>(&config),
        declaration::<site_build::OutputProducer>(&config),
        declaration::<site_build::SiteOutputFile>(&config),
        declaration::<site_build::SiteOutput>(&config),
        declaration::<site_engine::GeneratorSpec>(&config),
        declaration::<site_engine::OutputResourceSubject>(&config),
        declaration::<site_engine::OutputSubjectPage>(&config),
        declaration::<site_engine::OutputKind>(&config),
        declaration::<site_engine::OutputPageKind>(&config),
        declaration::<site_engine::OutputDescriptor>(&config),
        declaration::<site_engine::OutputCatalog>(&config),
        declaration::<site_engine::BuildStage>(&config),
        declaration::<site_engine::BuildEvent>(&config),
        declaration::<site_engine::BuildOperation>(&config),
        declaration::<site_engine::BuildErrorPhase>(&config),
        declaration::<site_engine::BuildErrorCode>(&config),
        declaration::<site_engine::BuildError<ts_rs::Dummy>>(&config),
    ]
    .concat();
    print!(
        "// @generated by site_engine/export_typescript; do not edit.\n\n\
export const SITE_BUILD_SCHEMA = \"site-build/v2\" as const;\n\
export const API_VERSION = {} as const;\n\
export const PREPARED_PACKAGE_MEDIA_TYPE = \"{}\" as const;\n\
export const PREPARED_PACKAGE_FORMAT_VERSION = {} as const;\n\
export const SITE_OUTPUT_SCHEMA = \"{}\" as const;\n\
export const SITE_OUTPUT_MANIFEST_PATH = \"{}\" as const;\n\
export const RESOLVER_SCHEMA = {} as const;\n\n{}",
        api_envelope::API_VERSION,
        site_build::PREPARED_PACKAGE_MEDIA_TYPE,
        package_store::PREPARED_PACKAGE_FORMAT_VERSION,
        site_build::SITE_OUTPUT_SCHEMA,
        site_build::SITE_OUTPUT_MANIFEST_PATH,
        package_store::RESOLVER_SCHEMA,
        declarations,
    );
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::{json, Value};

    use super::schema;

    fn schema_json<T: schemars::JsonSchema>(serialize: bool) -> Value {
        serde_json::to_value(schema::<T>(serialize)).expect("schema serializes")
    }

    fn schema_accepts_explicit_null(value: &Value) -> bool {
        match value {
            Value::Bool(value) => *value,
            Value::Object(object) => {
                if object.is_empty()
                    || object.get("const").is_some_and(Value::is_null)
                    || object
                        .get("enum")
                        .and_then(Value::as_array)
                        .is_some_and(|values| values.iter().any(Value::is_null))
                    || match object.get("type") {
                        Some(Value::String(value)) => value == "null",
                        Some(Value::Array(values)) => {
                            values.iter().any(|value| value.as_str() == Some("null"))
                        }
                        _ => false,
                    }
                {
                    return true;
                }
                ["anyOf", "oneOf"]
                    .iter()
                    .filter_map(|key| object.get(*key).and_then(Value::as_array))
                    .flatten()
                    .any(schema_accepts_explicit_null)
            }
            _ => false,
        }
    }

    fn collect_nullable_properties(value: &Value, path: &str, result: &mut BTreeSet<String>) {
        match value {
            Value::Object(object) => {
                if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                    for (name, property) in properties {
                        let property_path = format!("{path}/properties/{name}");
                        if schema_accepts_explicit_null(property) {
                            result.insert(property_path.clone());
                        }
                        collect_nullable_properties(property, &property_path, result);
                    }
                }
                for (name, child) in object {
                    if name != "properties" {
                        collect_nullable_properties(child, &format!("{path}/{name}"), result);
                    }
                }
            }
            Value::Array(values) => {
                for (index, child) in values.iter().enumerate() {
                    collect_nullable_properties(child, &format!("{path}/{index}"), result);
                }
            }
            _ => {}
        }
    }

    fn nullable_properties(value: &Value) -> BTreeSet<String> {
        let mut result = BTreeSet::new();
        collect_nullable_properties(value, "", &mut result);
        result
    }

    fn assert_optional_non_null(value: &Value, object_path: &str, field: &str) {
        let object = value
            .pointer(object_path)
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("missing schema object at {object_path}"));
        let required = object
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>();
        assert!(
            !required.contains(field),
            "{object_path}/properties/{field} must be optional"
        );
        let property = object
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|properties| properties.get(field))
            .unwrap_or_else(|| panic!("missing {object_path}/properties/{field}"));
        assert!(
            !schema_accepts_explicit_null(property),
            "{object_path}/properties/{field} must reject explicit null: {property}"
        );
    }

    #[test]
    fn serialization_schemas_reject_unintended_explicit_nulls() {
        let schemas = [
            (
                "CompilationOutcome",
                schema_json::<site_engine::CompilationOutcome>(true),
            ),
            (
                "PreparedProjectResult",
                schema_json::<site_engine::PreparedProjectResult>(true),
            ),
            (
                "TemplateResolution",
                schema_json::<site_engine::TemplateResolution>(true),
            ),
            (
                "PackageMountResult",
                schema_json::<package_store::PackageMountResult>(true),
            ),
            (
                "PrepareMountResult",
                schema_json::<package_store::PrepareMountResult>(true),
            ),
            (
                "ClosedSiteBuild",
                schema_json::<site_build::ClosedSiteBuild>(true),
            ),
            ("ContentRef", schema_json::<content_store::ContentRef>(true)),
            (
                "OutputCatalog",
                schema_json::<site_engine::OutputCatalog>(true),
            ),
            ("SiteOutput", schema_json::<site_build::SiteOutput>(true)),
            ("BuildEvent", schema_json::<site_engine::BuildEvent>(true)),
            (
                "BuildError",
                schema_json::<site_engine::BuildError<site_engine::CompilationOutcome>>(true),
            ),
        ];
        for (name, schema) in schemas {
            assert_eq!(
                nullable_properties(&schema),
                BTreeSet::new(),
                "{name} serialization schema permits explicit null"
            );
        }
    }

    #[test]
    fn nullable_fields_are_an_explicit_input_and_resolution_allowlist() {
        let resolution = nullable_properties(&schema_json::<package_store::ResolutionStep>(true));
        assert_eq!(
            resolution,
            BTreeSet::from([
                "/$defs/MutableVersionRequest/properties/resolved_version".to_string(),
            ])
        );

        let generator = nullable_properties(&schema_json::<site_engine::GeneratorSpec>(false));
        assert_eq!(
            generator,
            BTreeSet::from([
                "/oneOf/0/properties/branch".to_string(),
                "/oneOf/0/properties/revision".to_string(),
                "/oneOf/1/properties/runUuid".to_string(),
            ])
        );
    }

    #[test]
    fn omitted_option_fields_are_optional_and_non_null_when_present() {
        let compilation = schema_json::<site_engine::CompilationOutcome>(true);
        for field in ["resourceType", "id", "url", "definition"] {
            assert_optional_non_null(&compilation, "/$defs/CompilationResource", field);
        }
        for field in ["file", "line", "ownerDefinition"] {
            assert_optional_non_null(&compilation, "/$defs/CompilationDiagnostic", field);
        }

        let template = schema_json::<site_engine::TemplateResolution>(true);
        assert_optional_non_null(&template, "", "missing");

        let content = schema_json::<content_store::ContentRef>(true);
        assert_optional_non_null(&content, "", "mediaType");

        let build = schema_json::<site_build::ClosedSiteBuild>(true);
        assert_optional_non_null(&build, "/$defs/RenderTarget", "template");
        assert_optional_non_null(&build, "/$defs/BuildDiagnostic", "location");

        let catalog = schema_json::<site_engine::OutputCatalog>(true);
        for field in ["content", "title", "subject", "subjectPage", "pageKind"] {
            assert_optional_non_null(&catalog, "/$defs/OutputDescriptor", field);
        }

        let output = schema_json::<site_build::SiteOutput>(true);
        for field in ["source", "owner"] {
            assert_optional_non_null(&output, "/$defs/SiteOutputFile", field);
        }

        let event = schema_json::<site_engine::BuildEvent>(true);
        for field in [
            "operation",
            "buildId",
            "label",
            "bytes",
            "totalBytes",
            "fraction",
            "fromCache",
            "durationMs",
            "inputBytes",
            "outputBytes",
            "fileCount",
            "metrics",
        ] {
            assert_optional_non_null(&event, "", field);
        }

        let error = schema_json::<site_engine::BuildError<site_engine::CompilationOutcome>>(true);
        assert_optional_non_null(&error, "", "successfulCompilation");
        assert_eq!(
            error.pointer("/properties/successfulCompilation/$ref"),
            Some(&Value::String("#/$defs/CompilationOutcome".to_string())),
            "BuildError must carry the concrete CompilationOutcome schema"
        );
    }

    #[test]
    fn build_event_omits_absent_observations_and_requires_only_core_fields() {
        let event = site_engine::BuildEvent {
            operation: None,
            build_id: None,
            stage: site_engine::BuildStage::Ready,
            label: None,
            bytes: None,
            total_bytes: None,
            message: "Ready.".to_string(),
            fraction: None,
            from_cache: None,
            duration_ms: None,
            input_bytes: None,
            output_bytes: None,
            file_count: None,
            metrics: None,
        };
        assert_eq!(
            serde_json::to_value(event).expect("BuildEvent serializes"),
            json!({ "stage": "ready", "message": "Ready." })
        );

        let schema = schema_json::<site_engine::BuildEvent>(true);
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .expect("BuildEvent schema has required fields")
            .iter()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(required, BTreeSet::from(["message", "stage"]));
    }
}
