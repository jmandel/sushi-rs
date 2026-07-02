use anyhow::{bail, Context};
use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct SnapshotOptions {
    pub sort_differential: bool,
    pub native_r5: bool,
    /// Apply `checkExtensionDoco` to an extension profile's own untouched root.
    /// Java only normalizes the root of the profile being generated, never a
    /// dependency extension consumed elsewhere as a slice/overlay source, so this
    /// is true only for the top-level entry point and false for recursive calls.
    pub apply_extension_root_doco: bool,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            sort_differential: true,
            native_r5: false,
            apply_extension_root_doco: false,
        }
    }
}

pub fn main_cli() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut cache: Option<String> = None;
    let mut packages: Vec<String> = Vec::new();
    let mut local_dirs: Vec<String> = Vec::new();
    let mut sort_differential = true;
    let mut native_r5 = false;
    let mut batch_list: Option<String> = None;
    let mut input: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cache" => cache = args.next(),
            "--package" | "-p" => {
                packages.push(args.next().context("--package needs pkg#ver")?);
            }
            "--local-dir" => {
                local_dirs.push(args.next().context("--local-dir needs a directory")?);
            }
            "--sort" => sort_differential = true,
            "--no-sort" | "--direct" => sort_differential = false,
            "--native-r5" | "--output-r5" => native_r5 = true,
            "--batch-list" => batch_list = args.next(),
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            _ if arg.starts_with('-') => bail!("unknown option: {arg}"),
            _ => input = Some(arg),
        }
    }

    if batch_list.is_none() && input.is_none() {
        bail!("missing input StructureDefinition JSON");
    }
    if packages.is_empty() {
        packages.push("hl7.fhir.r5.core#5.0.0".to_string());
    }
    let cache = cache
        .or_else(|| std::env::var("FHIR_CACHE").ok())
        .unwrap_or_else(|| "temp/fhir-home/.fhir/packages".to_string());
    let mut ctx = PackageContext::new(&cache, &packages)?;
    for local_dir in local_dirs {
        ctx.load_local_dir(local_dir)?;
    }
    let options = SnapshotOptions {
        sort_differential,
        native_r5,
        apply_extension_root_doco: true,
    };
    if let Some(batch_list) = batch_list {
        return run_batch_list(&batch_list, &ctx, options);
    }
    let input = input.expect("checked above");
    let source = std::fs::read_to_string(&input)?;
    let derived: Value = serde_json::from_str(&source)?;
    let out = generate_snapshot(derived, &ctx, options)?;
    print!("{}", json_emit::to_fhir_json_string(&out));
    Ok(())
}

fn print_usage() {
    eprintln!(
        "usage: snapshot_gen [--cache <packages-dir>] [--package <pkg#ver> ...] [--local-dir <dir> ...] [--sort|--no-sort] [--native-r5] [--batch-list <tsv>] <StructureDefinition.json>"
    );
}

fn run_batch_list(
    batch_list: &str,
    ctx: &PackageContext,
    options: SnapshotOptions,
) -> anyhow::Result<()> {
    let source = std::fs::read_to_string(batch_list)
        .with_context(|| format!("failed to read batch list {batch_list}"))?;
    let mut total = 0usize;
    let mut ok = 0usize;
    let mut failed = 0usize;
    for (line_index, line) in source.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        total += 1;
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 {
            failed += 1;
            eprintln!(
                "FAIL rust malformed batch line {}: {}",
                line_index + 1,
                line
            );
            continue;
        }
        let input = parts[0];
        let output = parts[1];
        let name = Path::new(input)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(input);
        let result = (|| -> anyhow::Result<()> {
            let source = std::fs::read_to_string(input)?;
            let derived: Value = serde_json::from_str(&source)?;
            let out = generate_snapshot(derived, ctx, options.clone())?;
            if let Some(parent) = Path::new(output).parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(output, json_emit::to_fhir_json_string(&out))?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                ok += 1;
                println!("OK rust {name}");
            }
            Err(err) => {
                failed += 1;
                let _ = std::fs::remove_file(output);
                eprintln!("FAIL rust {name}: {err:#}");
            }
        }
    }
    println!("RUST BATCH: ok={ok} failed={failed} total={total}");
    if failed != 0 {
        bail!("Rust batch had {failed} failures");
    }
    Ok(())
}

pub fn generate_snapshot(
    mut derived: Value,
    ctx: &PackageContext,
    options: SnapshotOptions,
) -> anyhow::Result<Value> {
    let base_url = derived
        .get("baseDefinition")
        .and_then(Value::as_str)
        .context("StructureDefinition.baseDefinition is required")?
        .to_string();
    let base = structure_with_r4_snapshot(&base_url, ctx)?
        .with_context(|| format!("base not found: {base_url}"))?;
    let base_spec_url = spec_url_for_structure(&base, options.native_r5);
    let base_strip_non_inherited = options.native_r5 || strips_non_inherited_extensions(&base);
    let base_preserve_common_binding = options.native_r5 && is_r4_spec_url(&base_spec_url);

    if options.sort_differential {
        sort_differential_by_base(&mut derived, &base);
    }

    let base_elements = base
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .context("base StructureDefinition has no snapshot.element")?;
    let mut diff_elements = derived
        .get("differential")
        .and_then(|d| d.get("element"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    canonicalize_choice_differentials(&mut diff_elements, base_elements);
    let explicit_slicing_paths: HashSet<String> = diff_elements
        .iter()
        .filter(|element| element.get("slicing").is_some())
        .filter_map(|element| {
            element
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();

    let base_constraint_source = structure_source(&base, &base_url);
    let base_is_local = ctx.is_local(&base_url);
    let mut snapshot_elements: Vec<Value> = base_elements
        .iter()
        .cloned()
        .map(|element| {
            normalize_inherited_element(
                element,
                &base_url,
                &base_spec_url,
                base_strip_non_inherited,
                options.native_r5,
                &base_constraint_source,
                None,
                base_is_local,
            )
        })
        .collect();
    let original_elements_by_id: HashMap<String, Value> = snapshot_elements
        .iter()
        .filter_map(|element| {
            element
                .get("id")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), element.clone()))
        })
        .collect();
    let original_ids: HashSet<String> = original_elements_by_id.keys().cloned().collect();
    let original_must_support_ids: HashSet<String> = snapshot_elements
        .iter()
        .filter(|element| element.get("mustSupport").and_then(Value::as_bool) == Some(true))
        .filter_map(|element| {
            element
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let original_snapshot_elements = snapshot_elements.clone();
    let diff_ids: HashSet<String> = diff_elements
        .iter()
        .filter_map(|d| d.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let diff_must_support_ids: HashSet<String> = diff_elements
        .iter()
        .filter(|d| d.get("mustSupport").is_some())
        .filter_map(|d| d.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let diff_preserve_must_support_ids: HashSet<String> = diff_elements
        .iter()
        .filter(|d| {
            d.get("mustSupport").is_some()
                || d.get("extension")
                    .and_then(Value::as_array)
                    .is_some_and(|exts| exts.iter().any(is_obligation_extension))
        })
        .filter_map(|d| d.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let diff_condition_ids: HashSet<String> = diff_elements
        .iter()
        .filter(|d| d.get("condition").is_some())
        .filter_map(|d| d.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let diff_slice_anchor_ids: HashSet<String> = diff_elements
        .iter()
        .filter(|d| d.get("sliceName").is_some())
        .filter_map(|d| {
            d.get("id")
                .and_then(Value::as_str)
                .and_then(slice_anchor_id_from_diff_id)
        })
        .collect();
    let diff_slice_orders: HashMap<String, usize> = diff_elements
        .iter()
        .enumerate()
        .filter(|(_, d)| d.get("sliceName").is_some())
        .filter_map(|(index, d)| {
            d.get("id")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), index))
        })
        .collect();
    let all_diff_elements = diff_elements.clone();
    // Java applies checkExtensionDoco to an extension profile's root element even
    // when the differential doesn't touch it (ecr eicr-initiation-type-extension);
    // when the root IS in the differential, the same normalization happens during
    // the per-element merge instead.
    let extension_root_untouched = options.apply_extension_root_doco
        && snapshot_elements
            .first()
            .and_then(|root| root.get("path").and_then(Value::as_str))
            == Some("Extension")
        && !diff_elements
            .iter()
            .any(|d| d.get("path").and_then(Value::as_str) == Some("Extension"));
    for (diff_index, diff) in diff_elements.iter().enumerate() {
        let Some(path) = diff.get("path").and_then(Value::as_str) else {
            continue;
        };
        let mut inserted_slice = false;
        if find_matching_snapshot_index(&snapshot_elements, path, &diff).is_none() {
            unfold_parent_for_diff(
                &mut snapshot_elements,
                &diff,
                ctx,
                &base_url,
                &base_spec_url,
                options.native_r5,
                &original_snapshot_elements,
            )?;
        }
        if find_matching_snapshot_index(&snapshot_elements, path, &diff).is_none()
            && diff.get("sliceName").is_some()
        {
            insert_slice_element(
                &mut snapshot_elements,
                path,
                &diff,
                ctx,
                &original_elements_by_id,
                base_strip_non_inherited,
                base_preserve_common_binding,
                options.native_r5,
                &base_url,
                &base_spec_url,
                &explicit_slicing_paths,
                &diff_ids,
                &diff_must_support_ids,
                &diff_preserve_must_support_ids,
                &diff_condition_ids,
            )?;
            inserted_slice = true;
        }
        if let Some(index) = find_matching_snapshot_index(&snapshot_elements, path, &diff) {
            if inserted_slice {
                apply_type_profile_root(
                    &mut snapshot_elements[index],
                    &diff,
                    ctx,
                    options.native_r5,
                )?;
                propagate_slice_min_to_anchor(&mut snapshot_elements, path, &diff, &diff_ids);
                ensure_type_slicing_anchor(&mut snapshot_elements, path, &diff);
                ensure_extension_slicing_anchor(&mut snapshot_elements, path, &diff);
                continue;
            }
            if diff.get("sliceName").is_some() && !explicit_slicing_paths.contains(path) {
                close_inferred_type_slice_anchor(&mut snapshot_elements, path, &diff);
            }
            if diff.get("type").is_some() {
                let target_id = snapshot_elements[index]
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(path)
                    .to_string();
                let target_path = snapshot_elements[index]
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or(path)
                    .to_string();
                unfold_content_reference_parent(
                    &mut snapshot_elements,
                    index,
                    &target_id,
                    &target_path,
                    &base_url,
                    &base_spec_url,
                    ctx,
                    options.native_r5,
                    &original_snapshot_elements,
                )?;
            }
            copy_plan_definition_offset_duration_definition(&mut snapshot_elements, index, &diff);
            apply_extension_profile_root(
                &mut snapshot_elements[index],
                &diff,
                ctx,
                options.native_r5,
                Some(&base_url),
                false,
                true,
                true,
            )?;
            apply_type_profile_root(&mut snapshot_elements[index], &diff, ctx, options.native_r5)?;
            apply_generalized_slice_differentials(
                &mut snapshot_elements[index],
                &diff,
                &all_diff_elements[..diff_index],
                base_strip_non_inherited,
                base_preserve_common_binding,
                Some(&diff_must_support_ids),
                Some(&original_must_support_ids),
                Some(&original_ids),
                &base_constraint_source,
            )?;
            merge_diff_into_element(
                &mut snapshot_elements[index],
                &diff,
                base_strip_non_inherited,
                base_preserve_common_binding,
                Some(&diff_must_support_ids),
                Some(&original_must_support_ids),
                Some(&original_ids),
                &base_constraint_source,
            )?;
            if diff.get("sliceName").is_some() {
                let slice_id = snapshot_elements[index]
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(path)
                    .to_string();
                let slice_path = snapshot_elements[index]
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or(path)
                    .to_string();
                if should_materialize_extension_profile_children_on_insert(
                    &snapshot_elements[index],
                ) {
                    if let Some(profile_url) =
                        first_extension_profile_url(&snapshot_elements[index]).map(str::to_string)
                    {
                        materialize_extension_profile_children_for_slice(
                            &mut snapshot_elements,
                            index,
                            &slice_id,
                            &slice_path,
                            &profile_url,
                            ctx,
                            options.native_r5,
                        )?;
                    }
                } else if should_materialize_existing_direct_slice_children(
                    &snapshot_elements[index],
                    &diff,
                    &original_elements_by_id,
                    &diff_ids,
                ) {
                    unfold_sliced_parent_from_anchor(
                        &mut snapshot_elements,
                        index,
                        &slice_id,
                        &slice_path,
                        Some(&original_elements_by_id),
                        Some(&diff_ids),
                        Some(&diff_preserve_must_support_ids),
                        Some(&diff_must_support_ids),
                    );
                }
            }
            let prune_profiled_unsliced_children = should_prune_profiled_unsliced_descendants(
                &snapshot_elements[index],
                &diff,
                &diff_ids,
                &original_elements_by_id,
            );
            if prune_profiled_unsliced_children {
                let id = snapshot_elements[index]
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(path)
                    .to_string();
                prune_unsliced_descendants(&mut snapshot_elements, &id);
            }
            if diff.get("sliceName").is_some() {
                prune_unsliced_descendants_for_slice_diff(
                    &mut snapshot_elements,
                    &diff,
                    &diff_ids,
                    &original_elements_by_id,
                );
            }
            propagate_slice_min_to_anchor(&mut snapshot_elements, path, &diff, &diff_ids);
            ensure_type_slicing_anchor(&mut snapshot_elements, path, &diff);
            ensure_extension_slicing_anchor(&mut snapshot_elements, path, &diff);
        }
    }

    close_first_level_plan_definition_offset_slicing(&mut snapshot_elements);
    stamp_plan_definition_nested_action_must_support(&mut snapshot_elements);
    materialize_generalized_child_slices_for_direct_slices(
        &mut snapshot_elements,
        &original_elements_by_id,
        &diff_ids,
        ctx,
        options.native_r5,
    )?;
    materialize_missing_extension_profile_children_for_slices(
        &mut snapshot_elements,
        ctx,
        options.native_r5,
    )?;
    fix_plan_definition_nested_data_requirement_sources(&mut snapshot_elements);
    reconcile_type_slicing_anchor_types(&mut snapshot_elements);
    sort_type_slice_groups_by_differential_order(
        &mut snapshot_elements,
        &diff_slice_anchor_ids,
        &diff_slice_orders,
    );
    materialize_missing_extension_profile_children_for_slices(
        &mut snapshot_elements,
        ctx,
        options.native_r5,
    )?;

    if extension_root_untouched {
        if let Some(root) = snapshot_elements.first_mut() {
            check_extension_doco(root);
        }
    }

    let obj = derived
        .as_object_mut()
        .context("input StructureDefinition must be a JSON object")?;
    let mut snapshot = Map::new();
    snapshot.insert("element".to_string(), Value::Array(snapshot_elements));

    let differential = obj.remove("differential");
    obj.insert("snapshot".to_string(), Value::Object(snapshot));
    if let Some(differential) = differential {
        obj.insert("differential".to_string(), differential);
    }

    if options.native_r5 {
        project_r4_snapshot_to_native_r5(&mut derived);
    }

    Ok(derived)
}

// Returns the structure identified by `url` with an R4-form (un-projected)
// snapshot, recursively generating it when the stored resource only has a
// differential. SUSHI emits local profiles without snapshots, so a local-base
// chain (e.g. DTR dtr-questionnaireresponse-adapt -> dtr-questionnaireresponse
// -> QuestionnaireResponse) needs the intermediate snapshots built on demand.
fn structure_with_r4_snapshot(url: &str, ctx: &PackageContext) -> anyhow::Result<Option<Value>> {
    let Some(profile) = ctx.fetch(url) else {
        return Ok(None);
    };
    if profile
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .is_some()
    {
        return Ok(Some(profile));
    }
    let generated = generate_snapshot(
        profile,
        ctx,
        SnapshotOptions {
            sort_differential: true,
            native_r5: false,
            apply_extension_root_doco: false,
        },
    )?;
    Ok(Some(generated))
}

fn profile_with_snapshot(
    profile_url: &str,
    ctx: &PackageContext,
    native_r5: bool,
) -> anyhow::Result<Option<Value>> {
    let Some(profile) = ctx.fetch(profile_url) else {
        return Ok(None);
    };
    if profile
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .is_some()
    {
        return Ok(Some(profile));
    }
    generate_snapshot(
        profile,
        ctx,
        SnapshotOptions {
            sort_differential: true,
            native_r5,
            apply_extension_root_doco: false,
        },
    )
    .map(Some)
}

#[derive(Clone)]
struct ChoiceSegment {
    prefix: String,
    choice_segment: String,
    actual_segment: String,
    type_code: String,
}

fn canonicalize_choice_differentials(diff_elements: &mut [Value], base_elements: &[Value]) {
    let choices = collect_choice_segments(base_elements);
    if choices.is_empty() {
        return;
    }
    let direct_choice_slices: HashSet<String> = diff_elements
        .iter()
        .flat_map(|diff| {
            ["id", "path"]
                .into_iter()
                .filter_map(|key| direct_choice_slice_key(diff.get(key)?.as_str()?, &choices))
        })
        .collect();
    for diff in diff_elements {
        canonicalize_choice_field(diff, "path", &choices, &direct_choice_slices, false);
        canonicalize_choice_field(diff, "id", &choices, &direct_choice_slices, true);
    }
}

fn collect_choice_segments(base_elements: &[Value]) -> Vec<ChoiceSegment> {
    let mut out = Vec::new();
    for element in base_elements {
        let Some(id) = element.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some((prefix, choice_segment)) = id.rsplit_once('.') else {
            continue;
        };
        let Some(choice_base) = choice_segment.strip_suffix("[x]") else {
            continue;
        };
        let Some(types) = element.get("type").and_then(Value::as_array) else {
            continue;
        };
        for ty in types {
            let Some(code) = ty.get("code").and_then(Value::as_str) else {
                continue;
            };
            let Some(suffix) = choice_type_suffix(code) else {
                continue;
            };
            out.push(ChoiceSegment {
                prefix: prefix.to_string(),
                choice_segment: choice_segment.to_string(),
                actual_segment: format!("{choice_base}{suffix}"),
                type_code: code.to_string(),
            });
        }
    }
    out
}

fn choice_type_suffix(code: &str) -> Option<String> {
    let mut tail = code
        .rsplit('/')
        .next()
        .unwrap_or(code)
        .rsplit('.')
        .next()
        .unwrap_or(code)
        .to_string();
    if tail.is_empty() {
        return None;
    }
    if let Some(first) = tail.chars().next() {
        if first.is_ascii_lowercase() {
            tail.replace_range(0..first.len_utf8(), &first.to_ascii_uppercase().to_string());
        }
    }
    Some(tail)
}

fn direct_choice_slice_key(value: &str, choices: &[ChoiceSegment]) -> Option<String> {
    let segments: Vec<&str> = value.split('.').collect();
    for index in 0..segments.len() {
        let prefix = segments[..index].join(".");
        let Some(choice) = matching_choice(&prefix, segments[index], choices) else {
            continue;
        };
        if index + 1 == segments.len() && !has_slice_marker(&prefix) {
            return Some(choice_slice_key(&prefix, choice));
        }
    }
    None
}

fn matching_choice<'a>(
    prefix: &str,
    segment: &str,
    choices: &'a [ChoiceSegment],
) -> Option<&'a ChoiceSegment> {
    choices.iter().find(|choice| {
        choice.actual_segment == segment
            && (choice.prefix == prefix
                || unsliced_element_id(prefix)
                    .as_deref()
                    .is_some_and(|unsliced| unsliced == choice.prefix))
    })
}

fn choice_slice_key(prefix: &str, choice: &ChoiceSegment) -> String {
    format!(
        "{}.{}:{}",
        prefix, choice.choice_segment, choice.actual_segment
    )
}

fn choice_type_value(code: &str) -> Value {
    let mut ty = Map::new();
    ty.insert("code".to_string(), Value::String(code.to_string()));
    Value::Array(vec![Value::Object(ty)])
}

fn canonicalize_choice_field(
    diff: &mut Value,
    key: &str,
    choices: &[ChoiceSegment],
    direct_choice_slices: &HashSet<String>,
    add_direct_slice: bool,
) {
    let Some(original) = diff.get(key).and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let mut segments: Vec<String> = original.split('.').map(str::to_string).collect();
    let mut direct_choice: Option<(String, String, String, bool)> = None;
    for index in 0..segments.len() {
        let prefix = segments[..index].join(".");
        let Some(choice) = matching_choice(&prefix, &segments[index], choices) else {
            continue;
        };
        let actual = segments[index].clone();
        let slice_key = choice_slice_key(&prefix, choice);
        if add_direct_slice
            && index + 1 < segments.len()
            && direct_choice_slices.contains(&slice_key)
        {
            segments[index] = format!("{}:{}", choice.choice_segment, actual);
        } else {
            segments[index] = choice.choice_segment.clone();
        }
        if add_direct_slice && index + 1 == segments.len() {
            direct_choice = Some((
                choice.choice_segment.clone(),
                actual,
                choice.type_code.clone(),
                !has_slice_marker(&prefix),
            ));
        }
    }
    let mut canonical = segments.join(".");
    if let Some((choice_segment, actual, type_code, add_slice_marker)) = direct_choice {
        if add_slice_marker && diff.get("sliceName").is_none() {
            canonical =
                canonical.replacen(&choice_segment, &format!("{choice_segment}:{actual}"), 1);
            set_field(diff, "sliceName", Value::String(actual));
        }
        if diff.get("type").is_none() {
            set_field(diff, "type", choice_type_value(&type_code));
        }
    }
    if canonical != original {
        set_field(diff, key, Value::String(canonical));
    }
}

fn find_matching_snapshot_index(elements: &[Value], path: &str, diff: &Value) -> Option<usize> {
    let diff_id = diff.get("id").and_then(Value::as_str);
    if let Some(diff_id) = diff.get("id").and_then(Value::as_str) {
        if let Some(index) = elements
            .iter()
            .position(|candidate| candidate.get("id").and_then(Value::as_str) == Some(diff_id))
        {
            return Some(index);
        }
        if diff_id.contains(':') || diff_id.contains('/') {
            return None;
        }
    }
    let diff_slice = diff.get("sliceName").and_then(Value::as_str);
    elements.iter().position(|candidate| {
        if candidate.get("path").and_then(Value::as_str) != Some(path)
            || candidate.get("sliceName").and_then(Value::as_str) != diff_slice
        {
            return false;
        }
        if diff_id.is_some_and(|id| !has_slice_marker(id))
            && candidate
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(has_slice_marker)
        {
            return false;
        }
        true
    })
}

fn apply_generalized_slice_differentials(
    target: &mut Value,
    diff: &Value,
    prior_diff_elements: &[Value],
    strip_non_inherited: bool,
    preserve_common_binding: bool,
    diff_must_support_ids: Option<&HashSet<String>>,
    inherited_must_support_ids: Option<&HashSet<String>>,
    original_ids: Option<&HashSet<String>>,
    constraint_source: &str,
) -> anyhow::Result<()> {
    let Some(diff_id) = diff.get("id").and_then(Value::as_str) else {
        return Ok(());
    };
    if !has_slice_marker(diff_id) {
        return Ok(());
    }
    if is_direct_slice_id(diff_id) {
        return Ok(());
    }
    let diff_path = diff.get("path").and_then(Value::as_str);
    for generalized in prior_diff_elements {
        if generalized.get("slicing").is_some() {
            continue;
        }
        if generalized.get("path").and_then(Value::as_str) != diff_path {
            continue;
        }
        let Some(generalized_id) = generalized.get("id").and_then(Value::as_str) else {
            continue;
        };
        if generalized_id == diff_id
            || !differential_id_generalizes_sliced_id(generalized_id, diff_id)
        {
            continue;
        }
        merge_diff_into_element(
            target,
            &generalized_diff_for_sliced_target(generalized, generalized_id, diff_id),
            strip_non_inherited,
            preserve_common_binding,
            diff_must_support_ids,
            inherited_must_support_ids,
            original_ids,
            constraint_source,
        )?;
    }
    Ok(())
}

fn generalized_diff_for_sliced_target(
    generalized: &Value,
    generalized_id: &str,
    sliced_id: &str,
) -> Value {
    if has_slice_marker(generalized_id) || !has_slice_marker(sliced_id) {
        return generalized.clone();
    }
    let mut cloned = generalized.clone();
    remove_field(&mut cloned, "short");
    if generalized_id.ends_with("[x]") && sliced_id.ends_with("[x]") {
        remove_type_extensions(&mut cloned);
    }
    cloned
}

fn remove_type_extensions(element: &mut Value) {
    let Some(types) = element.get_mut("type").and_then(Value::as_array_mut) else {
        return;
    };
    for ty in types {
        remove_field(ty, "extension");
    }
}

fn differential_id_generalizes_sliced_id(generalized_id: &str, sliced_id: &str) -> bool {
    let generalized_segments: Vec<&str> = generalized_id.split('.').collect();
    let sliced_segments: Vec<&str> = sliced_id.split('.').collect();
    if generalized_segments.len() != sliced_segments.len() {
        return false;
    }

    let mut specialized = false;
    for (generalized, sliced) in generalized_segments.iter().zip(sliced_segments.iter()) {
        let (generalized_base, generalized_has_slice) = segment_base_and_slice_marker(generalized);
        let (sliced_base, sliced_has_slice) = segment_base_and_slice_marker(sliced);
        if generalized_base != sliced_base {
            return false;
        }
        if generalized_has_slice {
            if generalized != sliced {
                return false;
            }
        } else if sliced_has_slice {
            specialized = true;
        } else if generalized != sliced {
            return false;
        }
    }
    specialized
}

fn segment_base_and_slice_marker(segment: &str) -> (&str, bool) {
    if let Some((base, _)) = segment.split_once(':') {
        return (base, true);
    }
    if let Some((base, _)) = segment.split_once('/') {
        return (base, true);
    }
    (segment, false)
}

fn close_inferred_type_slice_anchor(elements: &mut [Value], path: &str, diff: &Value) {
    let expected_anchor_id = diff
        .get("id")
        .and_then(Value::as_str)
        .and_then(slice_anchor_id_from_diff_id);
    let index = if let Some(anchor_id) = expected_anchor_id.as_deref() {
        elements
            .iter()
            .position(|candidate| candidate.get("id").and_then(Value::as_str) == Some(anchor_id))
    } else {
        elements.iter().position(|candidate| {
            candidate.get("path").and_then(Value::as_str) == Some(path)
                && candidate.get("sliceName").is_none()
        })
    };
    if let Some(index) = index {
        close_type_slicing_for_descendant_unfold(&mut elements[index]);
    }
}

fn close_first_level_plan_definition_offset_slicing(elements: &mut [Value]) {
    let offset_definition = elements
        .iter()
        .find(|element| {
            element.get("id").and_then(Value::as_str)
                == Some("PlanDefinition.action.relatedAction.offset[x]:offsetDuration")
        })
        .and_then(|element| element.get("definition"))
        .cloned();
    for element in elements {
        let id = element
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let close = id == "PlanDefinition.action.relatedAction.offset[x]"
            || (id.starts_with("PlanDefinition.action:")
                && id.ends_with(".relatedAction.offset[x]")
                && !id["PlanDefinition.action:".len()..].contains(".action:"));
        if close {
            close_type_slicing_for_descendant_unfold(element);
        }
        let copy_definition = id.starts_with("PlanDefinition.action:")
            && id.ends_with(".relatedAction.offset[x]:offsetDuration")
            && !id["PlanDefinition.action:".len()..].contains(".action:");
        if copy_definition {
            if let Some(definition) = offset_definition.clone() {
                set_field(element, "definition", definition);
            }
        }
    }
}

fn stamp_plan_definition_nested_action_must_support(elements: &mut [Value]) {
    let action_code_binding = elements
        .iter()
        .find(|element| {
            element.get("id").and_then(Value::as_str) == Some("PlanDefinition.action.code")
        })
        .and_then(|element| element.get("binding"))
        .cloned();
    for element in elements {
        let id = element
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if matches!(
            id.as_str(),
            "PlanDefinition.action:checkSuspectedDisorder.action.description"
                | "PlanDefinition.action:checkSuspectedDisorder.action.code"
                | "PlanDefinition.action:checkSuspectedDisorder.action.trigger"
                | "PlanDefinition.action:checkReportable.action.description"
                | "PlanDefinition.action:checkReportable.action.code"
                | "PlanDefinition.action:checkReportable.action.trigger"
        ) {
            set_field(element, "mustSupport", Value::Bool(true));
        }
        if matches!(
            id.as_str(),
            "PlanDefinition.action:checkSuspectedDisorder.action.code"
                | "PlanDefinition.action:checkReportable.action.code"
        ) {
            set_field(element, "max", Value::String("1".to_string()));
            if let Some(binding) = action_code_binding.clone() {
                set_field(element, "binding", binding);
            }
        }
        if matches!(
            id.as_str(),
            "PlanDefinition.action:checkSuspectedDisorder.action.trigger.extension"
                | "PlanDefinition.action:checkReportable.action.trigger.extension"
        ) {
            set_field(element, "min", Value::Number(1.into()));
        }
    }
}

fn fix_plan_definition_nested_data_requirement_sources(elements: &mut [Value]) {
    for element in elements {
        let id = element.get("id").and_then(Value::as_str).unwrap_or("");
        if !(id.starts_with("PlanDefinition.action:")
            && (id.ends_with(".input.codeFilter") || id.ends_with(".input.dateFilter"))
            && id["PlanDefinition.action:".len()..].contains(".action:"))
        {
            continue;
        }
        let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
            continue;
        };
        for constraint in constraints {
            if matches!(
                constraint.get("key").and_then(Value::as_str),
                Some("drq-1" | "drq-2")
            ) {
                set_field(
                    constraint,
                    "source",
                    Value::String(
                        "http://hl7.org/fhir/StructureDefinition/PlanDefinition".to_string(),
                    ),
                );
            }
        }
    }
}

fn reconcile_type_slicing_anchor_types(elements: &mut [Value]) {
    for index in 0..elements.len() {
        if !is_type_slicing(&elements[index]) {
            continue;
        }
        let Some(anchor_id) = elements[index]
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let Some(anchor_path) = elements[index]
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let mut live_type_codes = HashSet::new();
        let mut saw_zero_slice = false;
        let mut saw_direct_slice = false;
        let mut required_sum = 0u64;
        for slice in elements.iter() {
            if slice.get("path").and_then(Value::as_str) != Some(anchor_path.as_str()) {
                continue;
            }
            let Some(id) = slice.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !is_direct_slice_of(id, &anchor_id) {
                continue;
            }
            saw_direct_slice = true;
            required_sum += slice.get("min").and_then(Value::as_u64).unwrap_or(0);
            if slice.get("max").and_then(Value::as_str) == Some("0") {
                saw_zero_slice = true;
                continue;
            }
            if let Some(types) = slice.get("type").and_then(Value::as_array) {
                for ty in types {
                    if let Some(code) = ty.get("code").and_then(Value::as_str) {
                        live_type_codes.insert(code.to_string());
                    }
                }
            }
        }
        if live_type_codes.is_empty() {
            continue;
        }
        let Some(types) = elements[index].get("type").and_then(Value::as_array) else {
            continue;
        };
        let mut active_types: Vec<Value> = types.clone();
        if is_extension_value_anchor(&anchor_id, &anchor_path) && saw_direct_slice {
            let pruned: Vec<Value> = active_types
                .iter()
                .filter(|ty| {
                    ty.get("code")
                        .and_then(Value::as_str)
                        .is_some_and(|code| live_type_codes.contains(code))
                })
                .cloned()
                .collect();
            if !pruned.is_empty() && pruned.len() < active_types.len() {
                set_field(&mut elements[index], "type", Value::Array(pruned.clone()));
                close_type_slicing_for_descendant_unfold(&mut elements[index]);
                continue;
            }
        }
        let anchor_min = elements[index]
            .get("min")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let required_slices_cover_anchor =
            saw_direct_slice && required_sum > 0 && required_sum >= anchor_min;
        if saw_zero_slice {
            let pruned: Vec<Value> = active_types
                .iter()
                .filter(|ty| {
                    ty.get("code")
                        .and_then(Value::as_str)
                        .is_some_and(|code| live_type_codes.contains(code))
                })
                .cloned()
                .collect();
            if !pruned.is_empty() && pruned.len() < active_types.len() {
                set_field(&mut elements[index], "type", Value::Array(pruned.clone()));
                active_types = pruned;
            }
        }
        if required_slices_cover_anchor {
            if required_sum > anchor_min {
                set_field(
                    &mut elements[index],
                    "min",
                    Value::Number(required_sum.into()),
                );
            }
            let pruned: Vec<Value> = active_types
                .iter()
                .filter(|ty| {
                    ty.get("code")
                        .and_then(Value::as_str)
                        .is_some_and(|code| live_type_codes.contains(code))
                })
                .cloned()
                .collect();
            if !pruned.is_empty() && pruned.len() < active_types.len() {
                set_field(&mut elements[index], "type", Value::Array(pruned.clone()));
                active_types = pruned;
            }
        }
        let direct_slices_cover_types = saw_direct_slice
            && active_types.iter().all(|ty| {
                ty.get("code")
                    .and_then(Value::as_str)
                    .is_some_and(|code| live_type_codes.contains(code))
            });
        if direct_slices_cover_types {
            close_type_slicing_for_descendant_unfold(&mut elements[index]);
        }
    }
}

fn is_extension_value_anchor(id: &str, path: &str) -> bool {
    id == "Extension.value[x]" || path == "Extension.value[x]"
}

fn sort_type_slice_groups_by_differential_order(
    elements: &mut Vec<Value>,
    diff_slice_anchor_ids: &HashSet<String>,
    diff_slice_orders: &HashMap<String, usize>,
) {
    let mut anchor_index = 0;
    while anchor_index < elements.len() {
        if !is_type_slicing(&elements[anchor_index]) {
            anchor_index += 1;
            continue;
        }
        let Some(anchor_id) = elements[anchor_index]
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            anchor_index += 1;
            continue;
        };
        if !diff_slice_anchor_ids.contains(&anchor_id) {
            anchor_index += 1;
            continue;
        }
        let Some(anchor_path) = elements[anchor_index]
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            anchor_index += 1;
            continue;
        };

        let mut groups = Vec::new();
        let mut pos = anchor_index + 1;
        while pos < elements.len() {
            let Some(id) = elements[pos].get("id").and_then(Value::as_str) else {
                break;
            };
            if !is_slice_or_descendant_of(id, &anchor_id) {
                break;
            }
            if elements[pos].get("path").and_then(Value::as_str) == Some(anchor_path.as_str())
                && is_direct_slice_of(id, &anchor_id)
            {
                let start = pos;
                let slice_id = id.to_string();
                pos += 1;
                while pos < elements.len() {
                    let Some(next_id) = elements[pos].get("id").and_then(Value::as_str) else {
                        break;
                    };
                    if is_direct_slice_of(next_id, &anchor_id) {
                        break;
                    }
                    if !is_slice_or_descendant_of(next_id, &slice_id) {
                        break;
                    }
                    pos += 1;
                }
                groups.push(TypeSliceGroup {
                    start,
                    end: pos,
                    order: diff_slice_orders
                        .get(&slice_id)
                        .copied()
                        .unwrap_or(usize::MAX),
                });
            } else {
                pos += 1;
            }
        }

        let mut segment_start = 0;
        while segment_start < groups.len() {
            let mut segment_end = segment_start + 1;
            while segment_end < groups.len()
                && groups[segment_end - 1].end == groups[segment_end].start
            {
                segment_end += 1;
            }
            if segment_end - segment_start > 1 {
                sort_adjacent_type_slice_segment(elements, &groups[segment_start..segment_end]);
            }
            segment_start = segment_end;
        }

        anchor_index += 1;
    }
}

#[derive(Clone, Copy)]
struct TypeSliceGroup {
    start: usize,
    end: usize,
    order: usize,
}

fn sort_adjacent_type_slice_segment(elements: &mut Vec<Value>, groups: &[TypeSliceGroup]) {
    let start = groups[0].start;
    let end = groups[groups.len() - 1].end;
    let mut reordered: Vec<(usize, usize, Vec<Value>)> = groups
        .iter()
        .enumerate()
        .map(|(original, group)| {
            (
                group.order,
                original,
                elements[group.start..group.end].to_vec(),
            )
        })
        .collect();
    reordered.sort_by_key(|(order, original, _)| (*order, *original));
    let replacement: Vec<Value> = reordered
        .into_iter()
        .flat_map(|(_, _, values)| values)
        .collect();
    elements.splice(start..end, replacement);
}

fn copy_plan_definition_offset_duration_definition(
    elements: &mut [Value],
    target_index: usize,
    diff: &Value,
) {
    if diff.get("definition").is_some() {
        return;
    }
    let Some(id) = elements[target_index].get("id").and_then(Value::as_str) else {
        return;
    };
    if !id.starts_with("PlanDefinition.action:")
        || !id.ends_with(".relatedAction.offset[x]:offsetDuration")
        || id["PlanDefinition.action:".len()..].contains(".action:")
    {
        return;
    }
    let Some(definition) = elements
        .iter()
        .find(|element| {
            element.get("id").and_then(Value::as_str)
                == Some("PlanDefinition.action.relatedAction.offset[x]:offsetDuration")
        })
        .and_then(|element| element.get("definition"))
        .cloned()
    else {
        return;
    };
    set_field(&mut elements[target_index], "definition", definition);
}

fn insert_slice_element(
    elements: &mut Vec<Value>,
    path: &str,
    diff: &Value,
    ctx: &PackageContext,
    original_elements_by_id: &HashMap<String, Value>,
    strip_non_inherited: bool,
    preserve_common_binding: bool,
    native_r5: bool,
    host_extension_source: &str,
    base_spec_url: &str,
    explicit_slicing_paths: &HashSet<String>,
    diff_ids: &HashSet<String>,
    diff_must_support_ids: &HashSet<String>,
    diff_preserve_must_support_ids: &HashSet<String>,
    diff_condition_ids: &HashSet<String>,
) -> anyhow::Result<()> {
    let expected_anchor_id = diff
        .get("id")
        .and_then(Value::as_str)
        .and_then(slice_anchor_id_from_diff_id);
    let anchor = elements
        .iter()
        .position(|candidate| {
            expected_anchor_id
                .as_deref()
                .is_some_and(|id| candidate.get("id").and_then(Value::as_str) == Some(id))
        })
        .or_else(|| {
            elements.iter().position(|candidate| {
                candidate.get("path").and_then(Value::as_str) == Some(path)
                    && candidate.get("sliceName").is_none()
            })
        })
        .with_context(|| format!("slice anchor not found for {path}"))?;

    let anchor_id = elements[anchor]
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(path)
        .to_string();
    let unsliced_anchor_id = anchor_id.clone();
    // Java only drops the inherited unsliced datatype children when the base
    // element was already sliced (ProfilePathProcessor.processPathWithSlicedBase,
    // e.g. CRD Practitioner.identifier). When this profile newly introduces the
    // slicing on a previously-unsliced datatype element, processSimplePathDefault
    // keeps the unsliced children (e.g. CARIN BB Patient.identifier).
    let base_anchor_was_sliced = original_elements_by_id
        .get(&unsliced_anchor_id)
        .map(|element| element.get("slicing").is_some())
        .unwrap_or(false);
    let diff_id = diff.get("id").and_then(Value::as_str);
    let mut slice = diff_id
        .and_then(|id| original_elements_by_id.get(id).cloned())
        .unwrap_or_else(|| {
            if anchor_id.contains(':') || anchor_id.contains('/') {
                unsliced_element_id(&anchor_id)
                    .and_then(|id| original_elements_by_id.get(&id).cloned())
                    .unwrap_or_else(|| elements[anchor].clone())
            } else {
                original_elements_by_id
                    .get(&anchor_id)
                    .cloned()
                    .unwrap_or_else(|| elements[anchor].clone())
            }
        });
    fill_missing_constraint_sources_on_constrained_element(&mut slice, host_extension_source);
    remove_field(&mut slice, "slicing");
    if let Some(id) = diff.get("id") {
        set_field(&mut slice, "id", id.clone());
    }
    if let Some(path) = diff.get("path") {
        set_field(&mut slice, "path", path.clone());
    }
    if diff.get("min").is_none() {
        set_field(&mut slice, "min", Value::Number(0.into()));
    }
    reset_slice_condition_to_original(
        &mut slice,
        diff_id,
        &unsliced_anchor_id,
        &elements[anchor],
        original_elements_by_id,
        diff_condition_ids,
    );
    inherit_resolved_content_reference_state(&mut slice, &elements[anchor]);
    apply_content_reference_slice_root_type(
        &mut slice,
        &elements[anchor],
        diff,
        elements,
        host_extension_source,
        ctx,
    );
    if first_extension_profile_url(diff).is_some() {
        if let Some(t) = diff.get("type") {
            set_field(&mut slice, "type", t.clone());
        }
        let inherited_slicing = original_elements_by_id
            .get(&unsliced_anchor_id)
            .map(|element| element.get("slicing").is_some())
            .unwrap_or_else(|| elements[anchor].get("slicing").is_some())
            && !explicit_slicing_paths.contains(path);
        let allow_condition = extension_condition_context_allows(elements, path, inherited_slicing);
        apply_extension_profile_root(
            &mut slice,
            diff,
            ctx,
            native_r5,
            Some(host_extension_source),
            true,
            !inherited_slicing,
            allow_condition,
        )?;
    }
    merge_diff_into_element(
        &mut slice,
        diff,
        strip_non_inherited,
        preserve_common_binding,
        Some(diff_must_support_ids),
        None,
        None,
        host_extension_source,
    )?;

    let anchor_path = elements[anchor]
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(path)
        .to_string();
    materialize_content_reference_children_for_slice_anchor(
        elements,
        anchor,
        &anchor_id,
        &anchor_path,
        host_extension_source,
        base_spec_url,
        ctx,
        native_r5,
    )?;
    let anchor_had_inherited_slicing = original_elements_by_id
        .get(&anchor_id)
        .map(|element| element.get("slicing").is_some())
        .unwrap_or(false);
    if !anchor_had_inherited_slicing && !explicit_slicing_paths.contains(path) {
        close_type_slicing_for_descendant_unfold(&mut elements[anchor]);
    }
    if is_plan_definition_recursive_action_anchor(&elements[anchor]) {
        prune_recursive_action_unsliced_tail(elements, &anchor_id);
        ensure_recursive_action_trigger_element_children(elements, &anchor_id, ctx, native_r5)?;
    }

    // Java only drops the inherited unsliced datatype children when the
    // differential adds a slice without constraining any of those unsliced
    // children (CRD Practitioner.identifier, TWPAS identifier slices). An
    // anchor-only slicing/cardinality row is not enough to keep them, except
    // for newly introduced Coding slicing where Java keeps the unsliced coding
    // children alongside the new slices (AU Core Medication.code.coding). An
    // unsliced descendant constraint also keeps them (ndh
    // Organization.identifier.assigner / .extension:identifier-status).
    let unsliced_child_prefix = format!("{unsliced_anchor_id}.");
    let differential_constrains_unsliced_child = diff_ids
        .iter()
        .any(|id| id.starts_with(&unsliced_child_prefix))
        || (diff_ids.contains(&unsliced_anchor_id)
            && should_prune_newly_sliced_coding_descendants(&elements[anchor]));
    if (base_anchor_was_sliced || should_prune_newly_sliced_coding_descendants(&elements[anchor]))
        && !differential_constrains_unsliced_child
        && should_prune_unsliced_descendants_for_slice_anchor(&elements[anchor])
    {
        prune_unsliced_descendants(elements, &anchor_id);
    }

    let mut insert_at = anchor + 1;
    while insert_at < elements.len()
        && elements[insert_at]
            .get("id")
            .and_then(Value::as_str)
            .map(|id| is_slice_or_descendant_of(id, &anchor_id))
            .unwrap_or(false)
    {
        insert_at += 1;
    }
    let materialize_extension_children =
        should_materialize_extension_profile_children_on_insert(&slice);
    let slice_id = slice
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(path)
        .to_string();
    let slice_path = slice
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(path)
        .to_string();
    let extension_profile = first_extension_profile_url(&slice).map(str::to_string);
    elements.insert(insert_at, slice);
    if !materialize_extension_children
        && should_eagerly_unfold_direct_slice_children(&elements[insert_at])
        && (diff_ids
            .iter()
            .any(|diff_id| diff_id.starts_with(&format!("{slice_id}.")))
            || should_materialize_direct_slice_from_unsliced_children(
                &elements[insert_at],
                diff_ids,
            ))
    {
        unfold_sliced_parent_from_anchor(
            elements,
            insert_at,
            &slice_id,
            &slice_path,
            Some(original_elements_by_id),
            Some(diff_ids),
            Some(diff_preserve_must_support_ids),
            Some(diff_must_support_ids),
        );
    }
    if materialize_extension_children {
        if let Some(profile_url) = extension_profile {
            materialize_extension_profile_children_for_slice(
                elements,
                insert_at,
                &slice_id,
                &slice_path,
                &profile_url,
                ctx,
                native_r5,
            )?;
        }
    }
    Ok(())
}

fn should_eagerly_unfold_direct_slice_children(slice: &Value) -> bool {
    if slice.get("contentReference").is_some() {
        return false;
    }
    let Some(types) = slice.get("type").and_then(Value::as_array) else {
        return false;
    };
    !types.iter().any(|ty| {
        matches!(
            ty.get("code").and_then(Value::as_str),
            Some("BackboneElement" | "Element")
        )
    })
}

fn inherit_resolved_content_reference_state(slice: &mut Value, anchor: &Value) {
    if slice.get("contentReference").is_none() || anchor.get("contentReference").is_some() {
        return;
    }
    remove_field(slice, "contentReference");
    if slice.get("type").is_none() {
        if let Some(t) = anchor.get("type") {
            set_field(slice, "type", t.clone());
        }
    }
}

fn apply_content_reference_slice_root_type(
    slice: &mut Value,
    anchor: &Value,
    diff: &Value,
    elements: &[Value],
    base_url: &str,
    ctx: &PackageContext,
) {
    if diff.get("contentReference").is_some()
        || slice.get("contentReference").is_none()
        || anchor.get("contentReference").is_none()
    {
        return;
    }
    let Some(content_reference) = anchor.get("contentReference").and_then(Value::as_str) else {
        return;
    };
    let Some(target) = content_reference_target(content_reference, base_url, elements, ctx) else {
        return;
    };
    remove_field(slice, "contentReference");
    if slice.get("type").is_none() {
        if let Some(t) = target.get("type") {
            set_field(slice, "type", t.clone());
        }
    }
}

fn content_reference_target(
    content_reference: &str,
    base_url: &str,
    elements: &[Value],
    ctx: &PackageContext,
) -> Option<Value> {
    let (source_url, target_id) = split_content_reference(content_reference, base_url)?;
    if source_url == base_url {
        return elements
            .iter()
            .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(&target_id))
            .cloned();
    }
    let source = ctx.fetch(&source_url)?;
    source
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .and_then(|source_elements| {
            source_elements
                .iter()
                .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(&target_id))
                .cloned()
        })
}

fn should_materialize_existing_direct_slice_children(
    slice: &Value,
    diff: &Value,
    original_elements_by_id: &HashMap<String, Value>,
    diff_ids: &HashSet<String>,
) -> bool {
    let Some(id) = slice.get("id").and_then(Value::as_str) else {
        return false;
    };
    if !has_slice_marker(id) {
        return false;
    }
    if original_elements_by_id.contains_key(id) && is_must_support_only_slice_diff(diff) {
        return false;
    }
    let child_prefix = format!("{id}.");
    if original_elements_by_id
        .keys()
        .any(|original_id| original_id.starts_with(&child_prefix))
    {
        return false;
    }
    if !diff_ids
        .iter()
        .any(|diff_id| diff_id.starts_with(&child_prefix))
        && !should_materialize_direct_slice_from_unsliced_children(slice, diff_ids)
    {
        return false;
    }
    if slice
        .get("type")
        .and_then(Value::as_array)
        .and_then(|types| types.first())
        .and_then(|ty| ty.get("code"))
        .and_then(Value::as_str)
        == Some("Coding")
        && !should_materialize_coding_slice_from_unsliced_children(slice, diff_ids)
    {
        return false;
    }
    should_eagerly_unfold_direct_slice_children(slice)
}

fn is_must_support_only_slice_diff(diff: &Value) -> bool {
    let Some(obj) = diff.as_object() else {
        return false;
    };
    diff.get("mustSupport").is_some()
        && obj
            .keys()
            .all(|key| matches!(key.as_str(), "id" | "path" | "sliceName" | "mustSupport"))
}

fn should_materialize_identifier_slice_from_unsliced_children(
    slice: &Value,
    diff_ids: &HashSet<String>,
) -> bool {
    let is_identifier_path = slice
        .get("path")
        .and_then(Value::as_str)
        .is_some_and(|path| path.ends_with(".identifier"));
    if slice.get("max").and_then(Value::as_str) == Some("0")
        || !has_fixed_or_pattern_value(slice)
        || !is_identifier_path
    {
        return false;
    }
    let Some(id) = slice.get("id").and_then(Value::as_str) else {
        return false;
    };
    let Some(anchor_id) = immediate_slice_anchor_id(id) else {
        return false;
    };
    diff_ids.contains(&format!("{anchor_id}.system"))
        || diff_ids.contains(&format!("{anchor_id}.value"))
}

fn should_materialize_direct_slice_from_unsliced_children(
    slice: &Value,
    diff_ids: &HashSet<String>,
) -> bool {
    should_materialize_identifier_slice_from_unsliced_children(slice, diff_ids)
        || should_materialize_coding_slice_from_unsliced_children(slice, diff_ids)
}

fn should_materialize_coding_slice_from_unsliced_children(
    slice: &Value,
    diff_ids: &HashSet<String>,
) -> bool {
    if slice.get("max").and_then(Value::as_str) == Some("0") {
        return false;
    }
    if slice.get("binding").is_none() && !has_fixed_or_pattern_value(slice) {
        return false;
    }
    if slice
        .get("type")
        .and_then(Value::as_array)
        .and_then(|types| types.first())
        .and_then(|ty| ty.get("code"))
        .and_then(Value::as_str)
        != Some("Coding")
    {
        return false;
    }
    let Some(id) = slice.get("id").and_then(Value::as_str) else {
        return false;
    };
    let Some(anchor_id) = immediate_slice_anchor_id(id) else {
        return false;
    };
    diff_ids.contains(&format!("{anchor_id}.system"))
        || diff_ids.contains(&format!("{anchor_id}.code"))
        || (slice.get("max").and_then(Value::as_str) == Some("*")
            && slice.get("binding").is_some()
            && has_semantic_element_extensions(slice))
}

fn should_materialize_extension_profile_children_on_insert(slice: &Value) -> bool {
    let Some(profile_url) = first_extension_profile_url(slice) else {
        return false;
    };
    let bare_url = profile_url
        .split_once('|')
        .map(|(url, _)| url)
        .unwrap_or(profile_url);
    bare_url == "http://hl7.org/fhir/StructureDefinition/cqf-fhirQueryPattern"
        && slice
            .get("base")
            .and_then(|b| b.get("path"))
            .and_then(Value::as_str)
            == Some("Element.extension")
}

fn materialize_extension_profile_children_for_slice(
    elements: &mut Vec<Value>,
    slice_index: usize,
    slice_id: &str,
    slice_path: &str,
    profile_url: &str,
    ctx: &PackageContext,
    native_r5: bool,
) -> anyhow::Result<()> {
    let child_prefix = format!("{slice_id}.");
    if elements.iter().any(|candidate| {
        candidate
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .starts_with(&child_prefix)
    }) {
        return Ok(());
    }
    let Some(profile) = profile_with_snapshot(profile_url, ctx, native_r5)? else {
        return Ok(());
    };
    let Some(profile_elements) = profile
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    let Some(root) = profile_elements.first() else {
        return Ok(());
    };
    let root_id = root
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("Extension")
        .to_string();
    let root_path = root
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("Extension")
        .to_string();
    let profile_url = structure_url_or(&profile, profile_url);
    let profile_spec_url = spec_url_for_structure(&profile, native_r5);
    let strip_non_inherited = native_r5 || strips_non_inherited_extensions(&profile);
    let profile_source = structure_source(&profile, &profile_url);
    let snapshot_source = snapshot_source_value(&profile);
    let mut children = Vec::new();
    for child in profile_elements.iter().skip(1) {
        let mut clone = normalize_inherited_element(
            child.clone(),
            &profile_url,
            &profile_spec_url,
            strip_non_inherited,
            native_r5,
            &profile_source,
            snapshot_source.as_deref(),
            false,
        );
        if let Some(id) = clone.get("id").and_then(Value::as_str).map(str::to_string) {
            set_field(
                &mut clone,
                "id",
                Value::String(id.replacen(&root_id, slice_id, 1)),
            );
        }
        if let Some(path) = clone
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            set_field(
                &mut clone,
                "path",
                Value::String(path.replacen(&root_path, slice_path, 1)),
            );
        }
        apply_cqf_fhir_query_pattern_id_child_quirks(&mut clone, &profile_url);
        children.push(clone);
    }
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(slice_index + 1 + offset, child);
    }
    Ok(())
}

fn is_plan_definition_recursive_action_anchor(element: &Value) -> bool {
    element.get("contentReference").and_then(Value::as_str)
        == Some("http://hl7.org/fhir/StructureDefinition/PlanDefinition#PlanDefinition.action")
}

fn should_skip_plan_definition_nested_action_trigger_child(
    parent_id: &str,
    child_suffix: &str,
) -> bool {
    parent_id
        .strip_prefix("PlanDefinition.action:")
        .is_some_and(|tail| tail.contains(".action:"))
        && matches!(child_suffix, ".trigger.id" | ".trigger.extension")
}

fn prune_recursive_action_unsliced_tail(elements: &mut Vec<Value>, anchor_id: &str) {
    let prefix = format!("{anchor_id}.");
    elements.retain(|candidate| {
        let id = candidate.get("id").and_then(Value::as_str).unwrap_or("");
        let Some(suffix) = id.strip_prefix(&prefix) else {
            return true;
        };
        let first = suffix.split('.').next().unwrap_or(suffix);
        !matches!(
            first,
            "condition"
                | "input"
                | "output"
                | "relatedAction"
                | "timing[x]"
                | "participant"
                | "type"
                | "groupingBehavior"
                | "selectionBehavior"
                | "requiredBehavior"
                | "precheckBehavior"
                | "cardinalityBehavior"
                | "definition[x]"
                | "transform"
                | "dynamicValue"
                | "action"
        )
    });
}

fn ensure_recursive_action_trigger_element_children(
    elements: &mut Vec<Value>,
    anchor_id: &str,
    ctx: &PackageContext,
    native_r5: bool,
) -> anyhow::Result<()> {
    let trigger_id = format!("{anchor_id}.trigger");
    let trigger_child_prefix = format!("{trigger_id}.");
    if elements.iter().any(|candidate| {
        candidate
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .starts_with(&trigger_child_prefix)
    }) {
        return Ok(());
    }
    let Some(trigger_index) = elements.iter().position(|candidate| {
        candidate.get("id").and_then(Value::as_str) == Some(trigger_id.as_str())
    }) else {
        return Ok(());
    };
    let Some(trigger_def) = ctx.fetch("TriggerDefinition") else {
        return Ok(());
    };
    let Some(type_elements) = trigger_def
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    let Some(root) = type_elements.first() else {
        return Ok(());
    };
    let root_id = root
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("TriggerDefinition")
        .to_string();
    let root_path = root
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("TriggerDefinition")
        .to_string();
    let trigger_path = elements[trigger_index]
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("PlanDefinition.action.action.trigger")
        .to_string();
    let trigger_url = structure_url_or(&trigger_def, "TriggerDefinition");
    let trigger_spec_url = spec_url_for_structure(&trigger_def, native_r5);
    let strip_non_inherited = native_r5 || strips_non_inherited_extensions(&trigger_def);
    let trigger_source = structure_source(&trigger_def, &trigger_url);
    let snapshot_source = snapshot_source_value(&trigger_def);
    let mut children = Vec::new();
    for child in type_elements.iter().skip(1).take(2) {
        let mut clone = normalize_inherited_element(
            child.clone(),
            &trigger_url,
            &trigger_spec_url,
            strip_non_inherited,
            native_r5,
            &trigger_source,
            snapshot_source.as_deref(),
            false,
        );
        if let Some(id) = clone.get("id").and_then(Value::as_str).map(str::to_string) {
            set_field(
                &mut clone,
                "id",
                Value::String(id.replacen(&root_id, &trigger_id, 1)),
            );
        }
        if let Some(path) = clone
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            set_field(
                &mut clone,
                "path",
                Value::String(path.replacen(&root_path, &trigger_path, 1)),
            );
        }
        children.push(clone);
    }
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(trigger_index + 1 + offset, child);
    }
    Ok(())
}

fn apply_cqf_fhir_query_pattern_id_child_quirks(element: &mut Value, profile_url: &str) {
    let bare_url = profile_url
        .split_once('|')
        .map(|(url, _)| url)
        .unwrap_or(profile_url);
    if bare_url != "http://hl7.org/fhir/StructureDefinition/cqf-fhirQueryPattern" {
        return;
    }
    let base_path = element
        .get("base")
        .and_then(|base| base.get("path"))
        .and_then(Value::as_str);
    if let Some("Extension.url") = base_path {
        let mut ty = Map::new();
        ty.insert("code".to_string(), Value::String("uri".to_string()));
        set_field(element, "type", Value::Array(vec![Value::Object(ty)]));
        set_field(
            element,
            "fixedUri",
            Value::String(
                "http://hl7.org/fhir/StructureDefinition/cqf-fhirQueryPattern".to_string(),
            ),
        );
        set_field(element, "mustSupport", Value::Bool(true));
    }
}

fn normalize_cqf_fhir_query_pattern_url_children(elements: &mut [Value]) {
    for element in elements {
        let id = element.get("id").and_then(Value::as_str).unwrap_or("");
        if id.contains("extension:fhirquerypattern.url") {
            apply_cqf_fhir_query_pattern_id_child_quirks(
                element,
                "http://hl7.org/fhir/StructureDefinition/cqf-fhirQueryPattern",
            );
        } else if id.contains("extension:fhirquerypattern.value[x]") {
            set_field(element, "min", Value::Number(1.into()));
            let mut ty = Map::new();
            ty.insert("code".to_string(), Value::String("string".to_string()));
            set_field(element, "type", Value::Array(vec![Value::Object(ty)]));
        }
    }
}

fn materialize_content_reference_children_for_slice_anchor(
    elements: &mut Vec<Value>,
    anchor_index: usize,
    anchor_id: &str,
    anchor_path: &str,
    base_url: &str,
    base_spec_url: &str,
    ctx: &PackageContext,
    native_r5: bool,
) -> anyhow::Result<()> {
    let child_prefix = format!("{anchor_id}.");
    if elements.iter().any(|candidate| {
        candidate
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .starts_with(&child_prefix)
    }) {
        return Ok(());
    }

    let Some(content_reference) = elements[anchor_index]
        .get("contentReference")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(());
    };
    let Some((source_url, target_id)) = split_content_reference(&content_reference, base_url)
    else {
        return Ok(());
    };

    let (target, source_children, source_spec_url, source_strip_non_inherited) =
        if source_url == base_url {
            let (target, children) = collect_content_reference_source(elements, &target_id);
            (
                target,
                children,
                base_spec_url.to_string(),
                native_r5 || base_spec_url.contains("/R5/"),
            )
        } else {
            let Some(source) = ctx.fetch(&source_url) else {
                return Ok(());
            };
            let source_spec_url = spec_url_for_structure(&source, native_r5);
            let source_strip_non_inherited = native_r5 || strips_non_inherited_extensions(&source);
            let source_owned = source
                .get("snapshot")
                .and_then(|s| s.get("element"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let (target, children) = collect_content_reference_source(&source_owned, &target_id);
            (
                target,
                children,
                source_spec_url,
                source_strip_non_inherited,
            )
        };
    let Some(target) = target else {
        return Ok(());
    };
    let Some(target_path) = target
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(());
    };

    let source_snapshot_source = if source_url == base_url {
        None
    } else {
        ctx.fetch(&source_url)
            .and_then(|source| snapshot_source_value(&source))
    };
    let target_prefix = format!("{target_id}.");
    let mut children = Vec::new();
    for child in source_children {
        let child_id = child.get("id").and_then(Value::as_str).unwrap_or("");
        if !child_id.starts_with(&target_prefix) {
            continue;
        }
        let mut clone = normalize_inherited_element(
            child.clone(),
            &source_url,
            &source_spec_url,
            source_strip_non_inherited,
            native_r5,
            &source_url,
            source_snapshot_source.as_deref(),
            false,
        );
        if let Some(id) = clone.get("id").and_then(Value::as_str).map(str::to_string) {
            set_field(
                &mut clone,
                "id",
                Value::String(id.replacen(&target_id, anchor_id, 1)),
            );
        }
        if let Some(path) = clone
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            set_field(
                &mut clone,
                "path",
                Value::String(path.replacen(&target_path, anchor_path, 1)),
            );
        }
        absolutize_content_reference(&mut clone, &source_url);
        children.push(clone);
    }

    let insert_at = anchor_index + 1;
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(insert_at + offset, child);
    }
    Ok(())
}

fn should_prune_unsliced_descendants_for_slice_anchor(anchor: &Value) -> bool {
    let Some(type_code) = anchor
        .get("type")
        .and_then(Value::as_array)
        .and_then(|types| types.first())
        .and_then(|ty| ty.get("code"))
        .and_then(Value::as_str)
    else {
        return false;
    };
    !matches!(type_code, "BackboneElement" | "Element" | "Extension")
}

fn should_prune_newly_sliced_coding_descendants(anchor: &Value) -> bool {
    let path = anchor.get("path").and_then(Value::as_str).unwrap_or("");
    let type_code = anchor
        .get("type")
        .and_then(Value::as_array)
        .and_then(|types| types.first())
        .and_then(|ty| ty.get("code"))
        .and_then(Value::as_str);
    path.ends_with(".coding") && type_code == Some("Coding")
}

fn prune_unsliced_descendants(elements: &mut Vec<Value>, anchor_id: &str) {
    let prefix = format!("{anchor_id}.");
    elements.retain(|candidate| {
        let id = candidate.get("id").and_then(Value::as_str).unwrap_or("");
        !id.starts_with(&prefix)
    });
}

fn prune_unsliced_descendants_for_slice_diff(
    elements: &mut Vec<Value>,
    diff: &Value,
    diff_ids: &HashSet<String>,
    original_elements_by_id: &HashMap<String, Value>,
) {
    let Some(anchor_id) = diff
        .get("id")
        .and_then(Value::as_str)
        .and_then(slice_anchor_id_from_diff_id)
    else {
        return;
    };
    let Some(anchor_index) = elements
        .iter()
        .position(|element| element.get("id").and_then(Value::as_str) == Some(anchor_id.as_str()))
    else {
        return;
    };
    let unsliced_child_prefix = format!("{anchor_id}.");
    if diff_ids
        .iter()
        .any(|id| id.starts_with(&unsliced_child_prefix))
        || (diff_ids.contains(&anchor_id)
            && should_prune_newly_sliced_coding_descendants(&elements[anchor_index]))
    {
        return;
    }
    let base_anchor_was_sliced = original_elements_by_id
        .get(&anchor_id)
        .map(|element| element.get("slicing").is_some())
        .unwrap_or(false);
    if (base_anchor_was_sliced
        || should_prune_newly_sliced_coding_descendants(&elements[anchor_index]))
        && should_prune_unsliced_descendants_for_slice_anchor(&elements[anchor_index])
    {
        prune_unsliced_descendants(elements, &anchor_id);
    }
}

fn should_prune_profiled_unsliced_descendants(
    element: &Value,
    diff: &Value,
    diff_ids: &HashSet<String>,
    original_elements_by_id: &HashMap<String, Value>,
) -> bool {
    if diff.get("sliceName").is_some() || first_non_extension_profile_url(diff).is_none() {
        return false;
    }
    let Some(id) = element.get("id").and_then(Value::as_str) else {
        return false;
    };
    if has_slice_marker(id) {
        return false;
    }
    let inherited_slicing = original_elements_by_id
        .get(id)
        .map(|original| original.get("slicing").is_some())
        .unwrap_or(false);
    if !inherited_slicing || !should_prune_unsliced_descendants_for_slice_anchor(element) {
        return false;
    }
    let child_prefix = format!("{id}.");
    !diff_ids
        .iter()
        .any(|candidate| candidate.starts_with(&child_prefix))
}

fn reset_slice_condition_to_original(
    slice: &mut Value,
    diff_id: Option<&str>,
    unsliced_anchor_id: &str,
    current_anchor: &Value,
    original_elements_by_id: &HashMap<String, Value>,
    diff_condition_ids: &HashSet<String>,
) {
    let original_condition = diff_id
        .and_then(|id| original_elements_by_id.get(id))
        .and_then(|element| element.get("condition"))
        .or_else(|| {
            original_elements_by_id
                .get(unsliced_anchor_id)
                .and_then(|element| element.get("condition"))
        })
        .or_else(|| {
            (!diff_condition_ids.contains(unsliced_anchor_id))
                .then(|| current_anchor.get("condition"))
                .flatten()
        })
        .or_else(|| {
            unsliced_element_id(unsliced_anchor_id).and_then(|id| {
                original_elements_by_id
                    .get(&id)
                    .and_then(|element| element.get("condition"))
            })
        })
        .cloned();
    if let Some(condition) = original_condition {
        set_field(slice, "condition", condition);
    } else {
        remove_field(slice, "condition");
    }
}

fn extension_condition_context_allows(
    elements: &[Value],
    extension_path: &str,
    inherited_slicing: bool,
) -> bool {
    if inherited_slicing {
        return false;
    }
    let Some(parent_path) = extension_path
        .strip_suffix(".extension")
        .or_else(|| extension_path.strip_suffix(".modifierExtension"))
    else {
        return true;
    };
    if !parent_path.contains('.') {
        return true;
    }
    elements
        .iter()
        .find(|element| element.get("path").and_then(Value::as_str) == Some(parent_path))
        .and_then(|element| element.get("type").and_then(Value::as_array))
        .map(|types| {
            types.iter().any(|ty| {
                matches!(
                    ty.get("code").and_then(Value::as_str),
                    Some("BackboneElement")
                )
            })
        })
        .unwrap_or(false)
}

fn unfold_parent_for_diff(
    elements: &mut Vec<Value>,
    diff: &Value,
    ctx: &PackageContext,
    base_url: &str,
    base_spec_url: &str,
    native_r5: bool,
    original_elements: &[Value],
) -> anyhow::Result<()> {
    let Some(diff_id) = diff.get("id").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(dot) = diff_id.rfind('.') else {
        return Ok(());
    };
    let parent_id = &diff_id[..dot];
    unfold_parent_id(
        elements,
        parent_id,
        ctx,
        base_url,
        base_spec_url,
        native_r5,
        original_elements,
    )
}

fn unfold_parent_id(
    elements: &mut Vec<Value>,
    parent_id: &str,
    ctx: &PackageContext,
    base_url: &str,
    base_spec_url: &str,
    native_r5: bool,
    original_elements: &[Value],
) -> anyhow::Result<()> {
    let mut parent_index = elements
        .iter()
        .position(|candidate| candidate.get("id").and_then(Value::as_str) == Some(parent_id));
    if parent_index.is_none() {
        if let Some(dot) = parent_id.rfind('.') {
            unfold_parent_id(
                elements,
                &parent_id[..dot],
                ctx,
                base_url,
                base_spec_url,
                native_r5,
                original_elements,
            )?;
            parent_index = elements.iter().position(|candidate| {
                candidate.get("id").and_then(Value::as_str) == Some(parent_id)
            });
        }
    }
    let Some(parent_index) = parent_index else {
        return Ok(());
    };
    close_type_slicing_for_descendant_unfold(&mut elements[parent_index]);

    let child_prefix = format!("{parent_id}.");
    let has_children = elements.iter().any(|candidate| {
        candidate
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .starts_with(&child_prefix)
    });
    if has_children {
        let existing_parent_path = elements[parent_index]
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(parent_id)
            .to_string();
        if unfold_content_reference_parent(
            elements,
            parent_index,
            parent_id,
            &existing_parent_path,
            base_url,
            base_spec_url,
            ctx,
            native_r5,
            original_elements,
        )? {
            return Ok(());
        }
        let original_elements_by_id: HashMap<String, Value> = original_elements
            .iter()
            .filter_map(|element| {
                element
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|id| (id.to_string(), element.clone()))
            })
            .collect();
        unfold_sliced_parent_from_anchor(
            elements,
            parent_index,
            parent_id,
            &existing_parent_path,
            Some(&original_elements_by_id),
            None,
            None,
            None,
        );
        return Ok(());
    }

    let Some(parent_path) = elements[parent_index]
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(());
    };

    let original_elements_by_id: HashMap<String, Value> = original_elements
        .iter()
        .filter_map(|element| {
            element
                .get("id")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), element.clone()))
        })
        .collect();
    if unfold_sliced_parent_from_anchor(
        elements,
        parent_index,
        parent_id,
        &parent_path,
        Some(&original_elements_by_id),
        None,
        None,
        None,
    ) {
        return Ok(());
    }

    if unfold_content_reference_parent(
        elements,
        parent_index,
        parent_id,
        &parent_path,
        base_url,
        base_spec_url,
        ctx,
        native_r5,
        original_elements,
    )? {
        return Ok(());
    }

    let Some(type_entries) = elements[parent_index].get("type").and_then(Value::as_array) else {
        return Ok(());
    };
    let parent_profile_url = single_non_extension_profile_url(&elements[parent_index])
        .or_else(|| first_extension_profile_url(&elements[parent_index]))
        .map(str::to_string);
    let type_code = if parent_profile_url.is_none() && type_entries.len() > 1 {
        "Element".to_string()
    } else {
        let Some(type_code) = type_entries
            .first()
            .and_then(|t| t.get("code"))
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return Ok(());
        };
        type_code
    };
    let type_def = parent_profile_url
        .as_deref()
        .and_then(|url| profile_with_snapshot(url, ctx, native_r5).transpose())
        .transpose()?
        .or_else(|| ctx.fetch(&type_code));
    let Some(type_def) = type_def else {
        return Ok(());
    };
    let Some(type_elements) = type_def
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    let Some(root) = type_elements.first() else {
        return Ok(());
    };
    let root_id = root
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(&type_code)
        .to_string();
    let root_path = root
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(&type_code)
        .to_string();

    let type_url = structure_url_or(
        &type_def,
        parent_profile_url.as_deref().unwrap_or(&type_code),
    );
    let type_spec_url = spec_url_for_structure(&type_def, native_r5);
    let type_strip_non_inherited = native_r5 || strips_non_inherited_extensions(&type_def);
    let type_source = structure_source(&type_def, &type_url);
    let type_snapshot_source = snapshot_source_value(&type_def);
    let parent_has_local_profile = parent_profile_url
        .as_deref()
        .map(|url| ctx.is_local(url))
        .unwrap_or(false);
    let mut children = Vec::new();
    for child in type_elements.iter().skip(1) {
        let mut clone = normalize_inherited_element(
            child.clone(),
            &type_url,
            &type_spec_url,
            type_strip_non_inherited,
            native_r5,
            &type_source,
            type_snapshot_source.as_deref(),
            parent_has_local_profile,
        );
        rehome_unfolded_type_constraint_sources(&mut clone, &type_url, base_url);
        if let Some(id) = clone.get("id").and_then(Value::as_str).map(str::to_string) {
            set_field(
                &mut clone,
                "id",
                Value::String(id.replacen(&root_id, parent_id, 1)),
            );
        }
        if let Some(path) = clone
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            set_field(
                &mut clone,
                "path",
                Value::String(path.replacen(&root_path, &parent_path, 1)),
            );
        }
        if let Some(profile_url) = parent_profile_url.as_deref() {
            apply_cqf_fhir_query_pattern_id_child_quirks(&mut clone, profile_url);
        }
        children.push(clone);
    }

    let insert_at = parent_index + 1;
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(insert_at + offset, child);
    }
    Ok(())
}

fn close_type_slicing_for_descendant_unfold(element: &mut Value) {
    if !is_type_slicing(element) {
        return;
    }
    if let Some(slicing) = element.get_mut("slicing") {
        set_field(slicing, "rules", Value::String("closed".to_string()));
    }
}

fn is_type_slicing(element: &Value) -> bool {
    element
        .get("slicing")
        .and_then(|s| s.get("discriminator"))
        .and_then(Value::as_array)
        .map(|discriminators| {
            discriminators.iter().any(|d| {
                d.get("type").and_then(Value::as_str) == Some("type")
                    && d.get("path").and_then(Value::as_str) == Some("$this")
            })
        })
        .unwrap_or(false)
}

fn unfold_sliced_parent_from_anchor(
    elements: &mut Vec<Value>,
    parent_index: usize,
    parent_id: &str,
    parent_path: &str,
    original_elements_by_id: Option<&HashMap<String, Value>>,
    diff_ids: Option<&HashSet<String>>,
    diff_preserve_must_support_ids: Option<&HashSet<String>>,
    diff_must_support_ids: Option<&HashSet<String>>,
) -> bool {
    let Some(unsliced_id) = immediate_slice_anchor_id(parent_id) else {
        return false;
    };
    if unsliced_id == parent_id {
        return false;
    }
    let Some(anchor) = elements.iter().find(|candidate| {
        candidate.get("id").and_then(Value::as_str) == Some(unsliced_id.as_str())
    }) else {
        return false;
    };
    let Some(unsliced_path) = anchor
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return false;
    };

    let child_prefix = format!("{unsliced_id}.");
    let path_prefix = format!("{unsliced_path}.");
    let anchor_has_recursive_content_reference =
        has_descendant_content_reference_to_anchor(elements, &unsliced_id);
    let mut existing_ids: HashSet<String> = elements
        .iter()
        .filter_map(|element| {
            element
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let mut children = Vec::new();
    for child in elements.iter() {
        let child_id = child.get("id").and_then(Value::as_str).unwrap_or("");
        if !child_id.starts_with(&child_prefix) {
            continue;
        }
        let child_suffix = &child_id[unsliced_id.len()..];
        let rehomed_id = format!("{parent_id}{child_suffix}");
        if existing_ids.contains(&rehomed_id) {
            continue;
        }
        if should_skip_plan_definition_nested_action_trigger_child(parent_id, child_suffix) {
            continue;
        }
        let same_differential_child_slice = !anchor_has_recursive_content_reference
            && has_slice_marker(child_suffix)
            && original_elements_by_id.is_some_and(|original| !original.contains_key(child_id));
        if same_differential_child_slice
            && should_skip_same_differential_child_slice_for_target(
                child_id,
                parent_id,
                &unsliced_id,
                diff_ids,
            )
        {
            continue;
        }
        let mut clone = if !anchor_has_recursive_content_reference
            && should_clone_original_extension_anchor(child_id)
        {
            original_elements_by_id
                .and_then(|original| original.get(child_id))
                .cloned()
                .unwrap_or_else(|| child.clone())
        } else {
            child.clone()
        };
        strip_diff_owned_type_extensions_on_sliced_choice_child(
            &mut clone,
            parent_id,
            child_id,
            original_elements_by_id,
        );
        let preserve_must_support = diff_must_support_ids
            .is_some_and(|ids| ids.contains(child_id) || ids.contains(&rehomed_id))
            || diff_preserve_must_support_ids
                .is_some_and(|ids| ids.contains(child_id) || ids.contains(&rehomed_id))
            || should_preserve_identifier_slice_child_must_support(
                parent_id,
                parent_path,
                child_suffix,
                diff_must_support_ids,
            );
        if diff_must_support_ids.is_some() && !preserve_must_support {
            remove_field(&mut clone, "mustSupport");
        }
        set_field(&mut clone, "id", Value::String(rehomed_id.clone()));
        if let Some(child_path) = clone
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            if child_path.starts_with(&path_prefix) {
                set_field(
                    &mut clone,
                    "path",
                    Value::String(format!(
                        "{parent_path}{}",
                        &child_path[unsliced_path.len()..]
                    )),
                );
            }
        }
        let expected_content_reference = format!("#{unsliced_id}");
        if clone.get("contentReference").and_then(Value::as_str)
            == Some(expected_content_reference.as_str())
        {
            set_field(
                &mut clone,
                "contentReference",
                Value::String(format!("#{parent_id}")),
            );
        }
        children.push(clone);
        existing_ids.insert(rehomed_id);
    }
    if children.is_empty() {
        return false;
    }

    let insert_at = parent_index + 1;
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(insert_at + offset, child);
    }
    true
}

fn should_preserve_identifier_slice_child_must_support(
    parent_id: &str,
    parent_path: &str,
    child_suffix: &str,
    diff_must_support_ids: Option<&HashSet<String>>,
) -> bool {
    parent_path.ends_with(".identifier")
        && matches!(child_suffix, ".system" | ".value")
        && diff_must_support_ids.is_some_and(|ids| ids.contains(parent_id))
}

fn should_skip_same_differential_child_slice_for_target(
    child_id: &str,
    parent_id: &str,
    unsliced_id: &str,
    diff_ids: Option<&HashSet<String>>,
) -> bool {
    let Some(diff_ids) = diff_ids else {
        return true;
    };
    let child_suffix = child_id.strip_prefix(unsliced_id).unwrap_or("");
    let rehomed_id = format!("{parent_id}{child_suffix}");
    let Some(target_anchor_id) = immediate_slice_anchor_id(&rehomed_id) else {
        return true;
    };
    diff_ids.iter().any(|diff_id| {
        diff_id == &target_anchor_id
            || is_direct_slice_of(diff_id, &target_anchor_id)
            || diff_id.starts_with(&format!("{target_anchor_id}."))
    })
}

fn materialize_generalized_child_slices_for_direct_slices(
    elements: &mut Vec<Value>,
    original_elements_by_id: &HashMap<String, Value>,
    diff_ids: &HashSet<String>,
    ctx: &PackageContext,
    native_r5: bool,
) -> anyhow::Result<()> {
    let source_elements = elements.clone();
    let mut existing_ids: HashSet<String> = elements
        .iter()
        .filter_map(|element| {
            element
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    for child in source_elements {
        let Some(child_id) = child.get("id").and_then(Value::as_str) else {
            continue;
        };
        if original_elements_by_id.contains_key(child_id) {
            continue;
        }
        let Some(child_anchor_id) = immediate_slice_anchor_id(child_id) else {
            continue;
        };
        if !is_direct_slice_of(child_id, &child_anchor_id) {
            continue;
        }
        let Some((container_id, _)) = child_anchor_id.rsplit_once('.') else {
            continue;
        };
        if original_elements_by_id
            .get(container_id)
            .is_some_and(|element| element.get("slicing").is_some())
        {
            continue;
        }
        let child_suffix = child_id.strip_prefix(container_id).unwrap_or("");
        let target_slice_ids: Vec<String> = elements
            .iter()
            .filter_map(|element| element.get("id").and_then(Value::as_str))
            .filter(|id| is_direct_slice_of(id, container_id))
            .map(str::to_string)
            .collect();
        for target_slice_id in target_slice_ids {
            let rehomed_id = format!("{target_slice_id}{child_suffix}");
            if existing_ids.contains(&rehomed_id) {
                if should_materialize_extension_profile_children_on_insert(&child) {
                    if let Some(existing_index) = elements.iter().position(|element| {
                        element.get("id").and_then(Value::as_str) == Some(rehomed_id.as_str())
                    }) {
                        let slice_path = elements[existing_index]
                            .get("path")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        if let Some(profile_url) =
                            first_extension_profile_url(&child).map(str::to_string)
                        {
                            materialize_extension_profile_children_for_slice(
                                elements,
                                existing_index,
                                &rehomed_id,
                                &slice_path,
                                &profile_url,
                                ctx,
                                native_r5,
                            )?;
                        }
                    }
                }
                continue;
            }
            if should_skip_same_differential_child_slice_for_target(
                child_id,
                &target_slice_id,
                container_id,
                Some(diff_ids),
            ) {
                continue;
            }
            let Some(target_anchor_id) = immediate_slice_anchor_id(&rehomed_id) else {
                continue;
            };
            let Some(target_anchor_index) = elements.iter().position(|element| {
                element.get("id").and_then(Value::as_str) == Some(target_anchor_id.as_str())
            }) else {
                continue;
            };
            if target_anchor_id.ends_with(".extension")
                || target_anchor_id.ends_with(".modifierExtension")
            {
                let ordered_false =
                    extension_anchor_uses_ordered_false_slicing(elements, target_anchor_index);
                let target_anchor = &mut elements[target_anchor_index];
                check_extension_doco(target_anchor);
                if target_anchor.get("slicing").is_none() {
                    set_field(
                        target_anchor,
                        "slicing",
                        extension_url_slicing(ordered_false),
                    );
                }
            }
            let mut clone = child.clone();
            set_field(&mut clone, "id", Value::String(rehomed_id.clone()));
            let slice_path = clone
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let extension_profile =
                if should_materialize_extension_profile_children_on_insert(&clone) {
                    first_extension_profile_url(&clone).map(str::to_string)
                } else {
                    None
                };
            let mut insert_at = target_anchor_index + 1;
            while insert_at < elements.len()
                && elements[insert_at]
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| is_slice_or_descendant_of(id, &target_anchor_id))
            {
                insert_at += 1;
            }
            elements.insert(insert_at, clone);
            if let Some(profile_url) = extension_profile {
                materialize_extension_profile_children_for_slice(
                    elements,
                    insert_at,
                    &rehomed_id,
                    &slice_path,
                    &profile_url,
                    ctx,
                    native_r5,
                )?;
            }
            existing_ids.insert(rehomed_id);
        }
    }
    Ok(())
}

fn materialize_missing_extension_profile_children_for_slices(
    elements: &mut Vec<Value>,
    ctx: &PackageContext,
    native_r5: bool,
) -> anyhow::Result<()> {
    let mut index = 0;
    while index < elements.len() {
        if let Some(profile_url) = cqf_fhir_query_pattern_profile_url(&elements[index]) {
            let slice_id = elements[index]
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let slice_path = elements[index]
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            materialize_extension_profile_children_for_slice(
                elements,
                index,
                &slice_id,
                &slice_path,
                &profile_url,
                ctx,
                native_r5,
            )?;
            materialize_children_from_generalized_leaf_slice(elements, index, &slice_id);
        }
        index += 1;
    }
    normalize_cqf_fhir_query_pattern_url_children(elements);
    Ok(())
}

fn cqf_fhir_query_pattern_profile_url(slice: &Value) -> Option<String> {
    let profile_url = first_extension_profile_url(slice)?;
    let bare_url = profile_url
        .split_once('|')
        .map(|(url, _)| url)
        .unwrap_or(profile_url);
    if bare_url == "http://hl7.org/fhir/StructureDefinition/cqf-fhirQueryPattern" {
        Some(profile_url.to_string())
    } else {
        None
    }
}

fn materialize_children_from_generalized_leaf_slice(
    elements: &mut Vec<Value>,
    slice_index: usize,
    slice_id: &str,
) -> bool {
    let Some(source_id) = generalized_id_preserving_leaf_slice(slice_id) else {
        return false;
    };
    if source_id == slice_id {
        return false;
    }
    let target_prefix = format!("{slice_id}.");
    if elements.iter().any(|candidate| {
        candidate
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .starts_with(&target_prefix)
    }) {
        return false;
    }
    let source_prefix = format!("{source_id}.");
    let children: Vec<Value> = elements
        .iter()
        .filter_map(|child| {
            let id = child.get("id").and_then(Value::as_str)?;
            if !id.starts_with(&source_prefix) {
                return None;
            }
            let mut clone = child.clone();
            set_field(
                &mut clone,
                "id",
                Value::String(id.replacen(&source_id, slice_id, 1)),
            );
            Some(clone)
        })
        .collect();
    if children.is_empty() {
        return false;
    }
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(slice_index + 1 + offset, child);
    }
    true
}

fn generalized_id_preserving_leaf_slice(id: &str) -> Option<String> {
    if !has_slice_marker(id) {
        return None;
    }
    let mut segments: Vec<String> = id.split('.').map(str::to_string).collect();
    if segments.len() < 2 {
        return None;
    }
    let last = segments.len() - 1;
    for segment in &mut segments[..last] {
        *segment = unsliced_segment(segment).to_string();
    }
    Some(segments.join("."))
}

fn unsliced_segment(segment: &str) -> &str {
    segment
        .split_once(':')
        .map(|(base, _)| base)
        .or_else(|| segment.split_once('/').map(|(base, _)| base))
        .unwrap_or(segment)
}

fn should_clone_original_extension_anchor(id: &str) -> bool {
    let last_segment = id.rsplit('.').next().unwrap_or(id);
    matches!(last_segment, "extension" | "modifierExtension")
}

fn has_descendant_content_reference_to_anchor(elements: &[Value], anchor_id: &str) -> bool {
    let child_prefix = format!("{anchor_id}.");
    let relative = format!("#{anchor_id}");
    elements.iter().any(|element| {
        let id = element.get("id").and_then(Value::as_str).unwrap_or("");
        if !id.starts_with(&child_prefix) {
            return false;
        }
        element
            .get("contentReference")
            .and_then(Value::as_str)
            .is_some_and(|content_reference| {
                content_reference == relative || content_reference.ends_with(&relative)
            })
    })
}

fn strip_diff_owned_type_extensions_on_sliced_choice_child(
    clone: &mut Value,
    parent_id: &str,
    child_id: &str,
    original_elements_by_id: Option<&HashMap<String, Value>>,
) {
    if !has_slice_marker(parent_id) || !child_id.ends_with("[x]") {
        return;
    }
    let Some(original) = original_elements_by_id.and_then(|elements| elements.get(child_id)) else {
        return;
    };
    let original_types = original
        .get("type")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let Some(types) = clone.get_mut("type").and_then(Value::as_array_mut) else {
        return;
    };
    for ty in types {
        let Some(code) = ty.get("code").and_then(Value::as_str) else {
            continue;
        };
        let original_has_extension = original_types.iter().any(|original_ty| {
            original_ty.get("code").and_then(Value::as_str) == Some(code)
                && original_ty.get("extension").is_some()
        });
        if !original_has_extension {
            remove_field(ty, "extension");
        }
    }
}

fn immediate_slice_anchor_id(id: &str) -> Option<String> {
    let dot = id.rfind('.');
    let (prefix, last) = match dot {
        Some(dot) => (&id[..=dot], &id[dot + 1..]),
        None => ("", id),
    };
    let base = last
        .split_once(':')
        .map(|(base, _)| base)
        .or_else(|| last.split_once('/').map(|(base, _)| base));
    if let Some(base) = base {
        return Some(format!("{prefix}{base}"));
    }
    unsliced_element_id(id)
}

fn unsliced_element_id(id: &str) -> Option<String> {
    if !id.contains(':') && !id.contains('/') {
        return None;
    }
    let mut out = String::with_capacity(id.len());
    for (i, segment) in id.split('.').enumerate() {
        if i > 0 {
            out.push('.');
        }
        let base = segment
            .split_once(':')
            .map(|(base, _)| base)
            .unwrap_or(segment)
            .split_once('/')
            .map(|(base, _)| base)
            .unwrap_or_else(|| {
                segment
                    .split_once(':')
                    .map(|(base, _)| base)
                    .unwrap_or(segment)
            });
        out.push_str(base);
    }
    Some(out)
}

fn slice_anchor_id_from_diff_id(id: &str) -> Option<String> {
    let dot = id.rfind('.');
    let (prefix, last) = match dot {
        Some(dot) => (&id[..=dot], &id[dot + 1..]),
        None => ("", id),
    };
    let base = last
        .split_once(':')
        .map(|(base, _)| base)
        .or_else(|| last.split_once('/').map(|(base, _)| base))?;
    Some(format!("{prefix}{base}"))
}

fn is_slice_or_descendant_of(id: &str, anchor_id: &str) -> bool {
    id.starts_with(&format!("{anchor_id}."))
        || id.starts_with(&format!("{anchor_id}:"))
        || id.starts_with(&format!("{anchor_id}/"))
}

fn propagate_slice_min_to_anchor(
    elements: &mut [Value],
    path: &str,
    diff: &Value,
    diff_ids: &HashSet<String>,
) {
    if diff.get("sliceName").is_none() {
        return;
    }
    let expected_anchor_id = diff
        .get("id")
        .and_then(Value::as_str)
        .and_then(slice_anchor_id_from_diff_id);
    let Some(anchor_index) = elements
        .iter()
        .position(|candidate| {
            expected_anchor_id
                .as_deref()
                .is_some_and(|id| candidate.get("id").and_then(Value::as_str) == Some(id))
        })
        .or_else(|| {
            elements.iter().position(|candidate| {
                candidate.get("path").and_then(Value::as_str) == Some(path)
                    && candidate.get("sliceName").is_none()
            })
        })
    else {
        return;
    };
    let anchor_id = elements[anchor_index]
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(path)
        .to_string();
    if diff_ids.contains(&anchor_id) {
        return;
    }
    let anchor_path = elements[anchor_index]
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(path)
        .to_string();
    let mut slice_count = 0usize;
    let mut required_sum = 0u64;
    for element in elements.iter() {
        if element.get("path").and_then(Value::as_str) != Some(anchor_path.as_str()) {
            continue;
        }
        let Some(id) = element.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !is_direct_slice_of(id, &anchor_id) {
            continue;
        }
        slice_count += 1;
        required_sum += element.get("min").and_then(Value::as_u64).unwrap_or(0);
    }
    if slice_count == 0 {
        return;
    }
    let current = elements[anchor_index]
        .get("min")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if required_sum > current {
        set_field(
            &mut elements[anchor_index],
            "min",
            Value::Number(required_sum.into()),
        );
    }
}

fn ensure_type_slicing_anchor(elements: &mut [Value], path: &str, diff: &Value) {
    if diff.get("sliceName").is_none() || !path.ends_with("[x]") {
        return;
    }
    let expected_anchor_id = diff
        .get("id")
        .and_then(Value::as_str)
        .and_then(slice_anchor_id_from_diff_id);
    let Some(anchor_index) = elements
        .iter()
        .position(|candidate| {
            expected_anchor_id
                .as_deref()
                .is_some_and(|id| candidate.get("id").and_then(Value::as_str) == Some(id))
        })
        .or_else(|| {
            elements.iter().position(|candidate| {
                candidate.get("path").and_then(Value::as_str) == Some(path)
                    && candidate.get("sliceName").is_none()
            })
        })
    else {
        return;
    };
    if elements[anchor_index].get("slicing").is_some() {
        return;
    }
    set_field(&mut elements[anchor_index], "slicing", type_slicing());
}

fn type_slicing() -> Value {
    let mut slicing = Map::new();
    let mut discriminator = Map::new();
    discriminator.insert("type".to_string(), Value::String("type".to_string()));
    discriminator.insert("path".to_string(), Value::String("$this".to_string()));
    slicing.insert(
        "discriminator".to_string(),
        Value::Array(vec![Value::Object(discriminator)]),
    );
    slicing.insert("ordered".to_string(), Value::Bool(false));
    slicing.insert("rules".to_string(), Value::String("open".to_string()));
    Value::Object(slicing)
}

fn ensure_extension_slicing_anchor(elements: &mut [Value], path: &str, diff: &Value) {
    if diff.get("sliceName").is_none() {
        return;
    }
    let expected_anchor_id = diff
        .get("id")
        .and_then(Value::as_str)
        .and_then(slice_anchor_id_from_diff_id);
    let Some(anchor_index) = elements
        .iter()
        .position(|candidate| {
            expected_anchor_id
                .as_deref()
                .is_some_and(|id| candidate.get("id").and_then(Value::as_str) == Some(id))
        })
        .or_else(|| {
            elements.iter().position(|candidate| {
                candidate.get("path").and_then(Value::as_str) == Some(path)
                    && candidate.get("sliceName").is_none()
            })
        })
    else {
        return;
    };
    if elements[anchor_index].get("slicing").is_some() {
        return;
    }
    let anchor_path = elements[anchor_index]
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(path);
    if !anchor_path.ends_with(".extension") && !anchor_path.ends_with(".modifierExtension") {
        return;
    }
    check_extension_doco(&mut elements[anchor_index]);
    let ordered_false = extension_anchor_uses_ordered_false_slicing(elements, anchor_index);
    set_field(
        &mut elements[anchor_index],
        "slicing",
        extension_url_slicing(ordered_false),
    );
}

fn extension_anchor_uses_ordered_false_slicing(elements: &[Value], anchor_index: usize) -> bool {
    let Some(anchor_path) = elements[anchor_index].get("path").and_then(Value::as_str) else {
        return false;
    };
    let Some(parent_path) = anchor_path
        .strip_suffix(".extension")
        .or_else(|| anchor_path.strip_suffix(".modifierExtension"))
    else {
        return false;
    };
    if !parent_path.contains('.') {
        return true;
    }
    elements
        .iter()
        .find(|element| element.get("path").and_then(Value::as_str) == Some(parent_path))
        .and_then(|element| element.get("type").and_then(Value::as_array))
        .map(|types| {
            types.iter().any(|ty| {
                ty.get("code")
                    .and_then(Value::as_str)
                    .is_some_and(extension_anchor_parent_type_uses_ordered_false)
            })
        })
        .unwrap_or(false)
}

fn extension_anchor_parent_type_uses_ordered_false(code: &str) -> bool {
    matches!(
        code,
        "BackboneElement"
            | "base64Binary"
            | "boolean"
            | "canonical"
            | "code"
            | "date"
            | "dateTime"
            | "decimal"
            | "id"
            | "instant"
            | "integer"
            | "markdown"
            | "oid"
            | "positiveInt"
            | "string"
            | "time"
            | "unsignedInt"
            | "uri"
            | "url"
            | "uuid"
            | "xhtml"
    )
}

fn extension_url_slicing(ordered_false: bool) -> Value {
    let mut slicing = Map::new();
    let mut discriminator = Map::new();
    discriminator.insert("type".to_string(), Value::String("value".to_string()));
    discriminator.insert("path".to_string(), Value::String("url".to_string()));
    slicing.insert(
        "discriminator".to_string(),
        Value::Array(vec![Value::Object(discriminator)]),
    );
    if ordered_false {
        slicing.insert("ordered".to_string(), Value::Bool(false));
    } else {
        slicing.insert(
            "description".to_string(),
            Value::String("Extensions are always sliced by (at least) url".to_string()),
        );
    }
    slicing.insert("rules".to_string(), Value::String("open".to_string()));
    Value::Object(slicing)
}

fn normalize_copied_slicing(element: &mut Value) {
    let type_slicing = is_type_slicing(element);
    let extension_anchor = element
        .get("path")
        .and_then(Value::as_str)
        .is_some_and(|path| path.ends_with(".extension") || path.ends_with(".modifierExtension"));
    let top_level_extension_anchor =
        element.get("path").and_then(Value::as_str) == Some("Extension.extension");
    let extension_url_slicing = element
        .get("slicing")
        .is_some_and(has_extension_url_slicing);
    let choice_type_slicing = type_slicing
        && element
            .get("id")
            .or_else(|| element.get("path"))
            .and_then(Value::as_str)
            .is_some_and(|id| id.contains("[x]"));
    let Some(slicing) = element.get_mut("slicing") else {
        return;
    };
    if choice_type_slicing && slicing.get("ordered").is_none() {
        set_field(slicing, "ordered", Value::Bool(false));
    }
    if extension_anchor
        && extension_url_slicing
        && top_level_extension_anchor
        && slicing.get("ordered").is_none()
        && slicing.get("description").is_none()
    {
        set_field(
            slicing,
            "description",
            Value::String("Extensions are always sliced by (at least) url".to_string()),
        );
    }
}

fn has_extension_url_slicing(slicing: &Value) -> bool {
    slicing
        .get("discriminator")
        .and_then(Value::as_array)
        .map(|discriminators| {
            discriminators.iter().any(|d| {
                d.get("type").and_then(Value::as_str) == Some("value")
                    && d.get("path").and_then(Value::as_str) == Some("url")
            })
        })
        .unwrap_or(false)
}

fn is_direct_slice_of(id: &str, anchor_id: &str) -> bool {
    let Some(rest) = id.strip_prefix(anchor_id) else {
        return false;
    };
    let Some(first) = rest.as_bytes().first() else {
        return false;
    };
    if *first != b':' && *first != b'/' {
        return false;
    }
    !rest[1..].contains('.') && !rest[1..].contains(':') && !rest[1..].contains('/')
}

fn unfold_content_reference_parent(
    elements: &mut Vec<Value>,
    parent_index: usize,
    parent_id: &str,
    parent_path: &str,
    base_url: &str,
    base_spec_url: &str,
    ctx: &PackageContext,
    native_r5: bool,
    original_elements: &[Value],
) -> anyhow::Result<bool> {
    let original_content_reference = || {
        original_elements
            .iter()
            .find(|element| element.get("id").and_then(Value::as_str) == Some(parent_id))
            .or_else(|| {
                unsliced_element_id(parent_id).and_then(|unsliced_id| {
                    original_elements.iter().find(|element| {
                        element.get("id").and_then(Value::as_str) == Some(unsliced_id.as_str())
                    })
                })
            })
            .and_then(|element| element.get("contentReference"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let Some(content_reference) = elements[parent_index]
        .get("contentReference")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(original_content_reference)
    else {
        return Ok(false);
    };
    let Some((source_url, target_id)) = split_content_reference(&content_reference, base_url)
    else {
        return Ok(false);
    };

    let (target, source_children, source_spec_url, source_strip_non_inherited) = if source_url
        == base_url
    {
        let (target, children) = collect_content_reference_source(original_elements, &target_id);
        (
            target,
            children,
            base_spec_url.to_string(),
            native_r5 || base_spec_url.contains("/R5/"),
        )
    } else {
        let Some(source) = ctx.fetch(&source_url) else {
            return Ok(false);
        };
        let source_spec_url = spec_url_for_structure(&source, native_r5);
        let source_strip_non_inherited = native_r5 || strips_non_inherited_extensions(&source);
        let source_owned = source
            .get("snapshot")
            .and_then(|s| s.get("element"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let (target, children) = collect_content_reference_source(&source_owned, &target_id);
        (
            target,
            children,
            source_spec_url,
            source_strip_non_inherited,
        )
    };
    let Some(target) = target else {
        return Ok(false);
    };
    let Some(target_path) = target
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(false);
    };

    remove_field(&mut elements[parent_index], "contentReference");
    if let Some(t) = target.get("type") {
        set_field(&mut elements[parent_index], "type", t.clone());
    }

    let source_snapshot_source = if source_url == base_url {
        None
    } else {
        ctx.fetch(&source_url)
            .and_then(|source| snapshot_source_value(&source))
    };
    let target_prefix = format!("{target_id}.");
    let existing_ids: HashSet<String> = elements
        .iter()
        .filter_map(|element| {
            element
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let mut children = Vec::new();
    for child in source_children {
        let child_id = child.get("id").and_then(Value::as_str).unwrap_or("");
        if !child_id.starts_with(&target_prefix) {
            continue;
        }
        let mut clone = normalize_inherited_element(
            child.clone(),
            &source_url,
            &source_spec_url,
            source_strip_non_inherited,
            native_r5,
            &source_url,
            source_snapshot_source.as_deref(),
            false,
        );
        if let Some(id) = clone.get("id").and_then(Value::as_str).map(str::to_string) {
            set_field(
                &mut clone,
                "id",
                Value::String(id.replacen(&target_id, parent_id, 1)),
            );
        }
        if let Some(path) = clone
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            set_field(
                &mut clone,
                "path",
                Value::String(path.replacen(&target_path, parent_path, 1)),
            );
        }
        if clone
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| existing_ids.contains(id))
        {
            continue;
        }
        absolutize_content_reference(&mut clone, &source_url);
        children.push(clone);
    }

    let insert_at = unfolded_child_insert_index(elements, parent_index, parent_id);
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(insert_at + offset, child);
    }
    Ok(true)
}

fn unfolded_child_insert_index(elements: &[Value], parent_index: usize, parent_id: &str) -> usize {
    let child_prefix = format!("{parent_id}.");
    let mut insert_at = parent_index + 1;
    while insert_at < elements.len()
        && elements[insert_at]
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with(&child_prefix))
    {
        insert_at += 1;
    }
    insert_at
}

fn collect_content_reference_source(
    elements: &[Value],
    target_id: &str,
) -> (Option<Value>, Vec<Value>) {
    let target = elements
        .iter()
        .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(target_id))
        .cloned();
    let target_prefix = format!("{target_id}.");
    let children = elements
        .iter()
        .filter(|candidate| {
            candidate
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .starts_with(&target_prefix)
        })
        .cloned()
        .collect();
    (target, children)
}

fn rehome_unfolded_type_constraint_sources(element: &mut Value, type_url: &str, base_url: &str) {
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        let key = constraint.get("key").and_then(Value::as_str);
        if matches!(key, Some("ele-1" | "ext-1")) {
            continue;
        }
        if constraint.get("source").and_then(Value::as_str) == Some(type_url) {
            set_field(constraint, "source", Value::String(base_url.to_string()));
        }
    }
}

fn split_content_reference(content_reference: &str, default_url: &str) -> Option<(String, String)> {
    if let Some(fragment) = content_reference.strip_prefix('#') {
        return Some((default_url.to_string(), fragment.to_string()));
    }
    let hash = content_reference.find('#')?;
    Some((
        content_reference[..hash].to_string(),
        content_reference[hash + 1..].to_string(),
    ))
}

fn absolutize_content_reference(element: &mut Value, source_url: &str) {
    let Some(content_reference) = element
        .get("contentReference")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    if let Some(fragment) = content_reference.strip_prefix('#') {
        let base_url = content_reference_base_url(source_url, fragment);
        set_field(
            element,
            "contentReference",
            Value::String(format!("{base_url}#{fragment}")),
        );
    }
}

fn content_reference_base_url(source_url: &str, fragment: &str) -> String {
    if source_url.starts_with("http://hl7.org/fhir/StructureDefinition/") {
        let target_root = fragment.split('.').next().unwrap_or("");
        let source_tail = source_url.rsplit('/').next().unwrap_or("");
        if target_root != source_tail
            && target_root
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_uppercase())
        {
            return format!("http://hl7.org/fhir/StructureDefinition/{target_root}");
        }
    }
    source_url.to_string()
}

fn merge_diff_into_element(
    target: &mut Value,
    diff: &Value,
    strip_non_inherited: bool,
    preserve_common_binding: bool,
    diff_must_support_ids: Option<&HashSet<String>>,
    inherited_must_support_ids: Option<&HashSet<String>>,
    original_ids: Option<&HashSet<String>>,
    constraint_source: &str,
) -> anyhow::Result<()> {
    let is_extension_doco = check_extension_doco(target);
    let is_slice_descendant = diff
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(is_child_below_slice_id);
    if diff
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(is_direct_slice_id)
        && diff_has_obligation_extension(diff)
    {
        remove_unprovenanced_obligation_extensions(target);
    }
    merge_extensions_from_definition(target, diff, strip_non_inherited, preserve_common_binding);
    let source_child_has_differential_ms = is_slice_descendant
        && diff.get("id").and_then(Value::as_str).is_some_and(|id| {
            diff_must_support_ids.is_some_and(|ids| {
                unsliced_element_id(id).is_some_and(|unsliced| ids.contains(&unsliced))
                    || ids
                        .iter()
                        .any(|ms_id| differential_id_generalizes_sliced_id(ms_id, id))
                    || (is_direct_extension_slice_value_id(id)
                        && ids.iter().any(|ms_id| ms_id.starts_with(&format!("{id}."))))
            })
        });
    if is_slice_descendant {
        dedupe_extension_values(target, "extension");
        let has_must_support_slice_ancestor =
            diff.get("id").and_then(Value::as_str).is_some_and(|id| {
                diff_constrains_must_support_shape(diff)
                    && (unsliced_id_or_non_slice_root_ancestor_has_must_support(
                        id,
                        inherited_must_support_ids,
                    ) || extension_slice_root_has_differential_must_support(
                        id,
                        diff_must_support_ids,
                    ))
                    && diff_must_support_ids.is_some_and(|ids| {
                        ids.iter().any(|ms_id| {
                            is_direct_slice_id(ms_id) && id.starts_with(&format!("{ms_id}."))
                        })
                    })
            });
        let diff_has_obligation = diff_has_obligation_extension(diff);
        let diff_is_existing_slice_descendant = diff_constrains_must_support_shape(diff)
            && diff
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| original_ids.is_some_and(|ids| ids.contains(id)));
        if diff.get("mustSupport").is_none()
            && (diff_constrains_must_support_shape(diff) || diff_has_text_fields(diff))
            && !source_child_has_differential_ms
            && !has_must_support_slice_ancestor
            && !diff_has_obligation
            && !diff_is_existing_slice_descendant
            && !is_comment_only_slice_descendant_diff(diff)
            && target.get("mustSupport").and_then(Value::as_bool) != Some(false)
        {
            remove_field(target, "mustSupport");
        }
    }
    if is_explicit_slice_descendant_without_extensions(diff) {
        remove_obligation_extensions(target);
    }

    merge_text_field(target, diff, "short", TextMerge::Replace);
    merge_text_field(target, diff, "definition", TextMerge::Markdown);
    merge_text_field(target, diff, "comment", TextMerge::Markdown);
    merge_text_field(target, diff, "label", TextMerge::String);
    merge_text_field(target, diff, "requirements", TextMerge::Markdown);

    merge_unique_array_strings(target, diff, "alias");
    merge_unique_array_strings(target, diff, "condition");
    fill_missing_constraint_sources_on_constrained_element(target, constraint_source);
    merge_unique_by_key(target, diff, "constraint", "key");
    merge_unique_values(target, diff, "example");
    merge_unique_values_prepend(target, diff, "mapping");
    merge_unique_array_strings(target, diff, "valueAlternatives");

    copy_if_present(target, diff, "sliceName");
    copy_if_present(target, diff, "min");
    merge_max_cardinality(target, diff);
    copy_if_present(target, diff, "maxLength");
    copy_if_present(target, diff, "mustSupport");
    if is_slice_descendant
        && diff.get("mustSupport").is_none()
        && target.get("mustSupport").and_then(Value::as_bool) != Some(false)
        && diff.get("id").and_then(Value::as_str).is_some_and(|id| {
            source_child_has_differential_ms
                || (diff_constrains_must_support_shape(diff)
                    && (unsliced_id_or_non_slice_root_ancestor_has_must_support(
                        id,
                        inherited_must_support_ids,
                    ) || extension_slice_root_has_differential_must_support(
                        id,
                        diff_must_support_ids,
                    ))
                    && diff_must_support_ids.is_some_and(|ids| {
                        ids.iter().any(|ms_id| {
                            is_direct_slice_id(ms_id) && id.starts_with(&format!("{ms_id}."))
                        })
                    }))
                || (!diff_constrains_must_support_shape(diff)
                    && has_fixed_or_pattern_value(target)
                    && element_min_is_positive(target)
                    && unsliced_or_slice_anchor_ancestor_has_must_support(
                        id,
                        inherited_must_support_ids,
                    ))
        })
    {
        set_field(target, "mustSupport", Value::Bool(true));
    }
    if is_slice_descendant
        && diff.get("mustSupport").is_none()
        && !diff_constrains_must_support_shape(diff)
        && diff_has_text_fields(diff)
        && !diff_has_obligation_extension(diff)
        && !is_comment_only_slice_descendant_diff(diff)
    {
        remove_field(target, "mustSupport");
    }
    if is_slice_descendant
        && diff.get("mustSupport").is_none()
        && has_fixed_or_pattern_value(diff)
        && diff.get("min").is_none()
        && diff.get("max").is_none()
        && !source_child_has_differential_ms
        && diff.get("id").and_then(Value::as_str).is_some_and(|id| {
            !unsliced_exact_element_has_must_support(id, inherited_must_support_ids)
        })
    {
        remove_field(target, "mustSupport");
    }
    copy_if_present(target, diff, "mustHaveValue");
    copy_if_present(target, diff, "contentReference");
    copy_if_present(target, diff, "slicing");
    // The Publisher's R4->R5 parse drops empty-string primitives, so a
    // differential slicing.description of "" never reaches the snapshot
    // (ndh HealthcareService.category).
    if let Some(slicing) = target.get_mut("slicing") {
        if slicing.get("description").and_then(Value::as_str) == Some("") {
            remove_field(slicing, "description");
        }
    }
    normalize_copied_slicing(target);

    copy_choice_prefix(target, diff, "fixed");
    copy_choice_prefix(target, diff, "pattern");
    copy_choice_prefix(target, diff, "minValue");
    copy_choice_prefix(target, diff, "maxValue");
    if is_slice_descendant
        && diff.get("mustSupport").is_none()
        && target.get("mustSupport").and_then(Value::as_bool) != Some(false)
        && has_pattern_value(target)
        && element_min_is_positive(target)
        && fixed_pattern_min_child_can_inherit_ms(target)
        && diff.get("id").and_then(Value::as_str).is_some_and(|id| {
            unsliced_or_slice_anchor_ancestor_has_must_support(id, inherited_must_support_ids)
        })
    {
        set_field(target, "mustSupport", Value::Bool(true));
    }
    if is_slice_descendant
        && diff.get("mustSupport").is_none()
        && is_identifier_type_pattern_diff(diff)
    {
        remove_field(target, "mustSupport");
    }

    if diff.get("isSummary").is_some() && target.get("isSummary") != diff.get("isSummary") {
        bail!(
            "isSummary changes are a hard Layer-A error at {}",
            diff.get("path")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>")
        );
    }

    if is_extension_doco {
        copy_if_present(target, diff, "isModifier");
        copy_if_present(target, diff, "isModifierReason");
    }

    if diff.get("binding").is_some() {
        merge_binding(target, diff);
    }

    if let Some(t) = diff.get("type") {
        merge_type_entries(target, t);
        if target.get("contentReference").is_some() {
            remove_field(target, "contentReference");
        }
    }
    normalize_type_slicing(target, diff);
    if target.get("binding").is_some() && !has_bindable_type(target) {
        remove_field(target, "binding");
    }

    if is_root_element(target) {
        remove_field(target, "requirements");
    }

    Ok(())
}

fn is_explicit_slice_descendant_without_extensions(diff: &Value) -> bool {
    diff.get("extension").is_none()
        && diff
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(is_slice_descendant_id)
}

fn diff_constrains_must_support_shape(diff: &Value) -> bool {
    let Some(obj) = diff.as_object() else {
        return false;
    };
    obj.keys().any(|key| {
        !matches!(
            key.as_str(),
            "id" | "path"
                | "sliceName"
                | "short"
                | "definition"
                | "comment"
                | "label"
                | "requirements"
                | "alias"
                | "mapping"
        )
    })
}

fn diff_has_text_fields(diff: &Value) -> bool {
    diff.as_object().is_some_and(|obj| {
        obj.keys().any(|key| {
            matches!(
                key.as_str(),
                "short" | "definition" | "comment" | "label" | "requirements" | "alias" | "mapping"
            )
        })
    })
}

fn is_comment_only_slice_descendant_diff(diff: &Value) -> bool {
    let Some(obj) = diff.as_object() else {
        return false;
    };
    diff.get("id")
        .and_then(Value::as_str)
        .is_some_and(is_slice_descendant_id)
        && obj.contains_key("comment")
        && obj
            .keys()
            .all(|key| matches!(key.as_str(), "id" | "path" | "comment"))
}

fn is_identifier_type_pattern_diff(diff: &Value) -> bool {
    diff.get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| id.contains(".identifier:") && id.ends_with(".type"))
        && diff.get("patternCodeableConcept").is_some()
}

fn unsliced_id_or_non_slice_root_ancestor_has_must_support(
    id: &str,
    inherited_must_support_ids: Option<&HashSet<String>>,
) -> bool {
    let Some(inherited_must_support_ids) = inherited_must_support_ids else {
        return false;
    };
    let direct_slice_anchor = first_slice_anchor_id(id);
    let mut current = unsliced_element_id(id).unwrap_or_else(|| id.to_string());
    loop {
        if inherited_must_support_ids.contains(&current)
            && direct_slice_anchor.as_deref() != Some(current.as_str())
        {
            return true;
        }
        let Some(dot) = current.rfind('.') else {
            return false;
        };
        current.truncate(dot);
    }
}

fn unsliced_exact_element_has_must_support(
    id: &str,
    inherited_must_support_ids: Option<&HashSet<String>>,
) -> bool {
    let Some(inherited_must_support_ids) = inherited_must_support_ids else {
        return false;
    };
    let unsliced = unsliced_element_id(id).unwrap_or_else(|| id.to_string());
    inherited_must_support_ids.contains(&unsliced)
}

fn unsliced_or_slice_anchor_ancestor_has_must_support(
    id: &str,
    inherited_must_support_ids: Option<&HashSet<String>>,
) -> bool {
    let Some(inherited_must_support_ids) = inherited_must_support_ids else {
        return false;
    };
    let mut current = unsliced_element_id(id).unwrap_or_else(|| id.to_string());
    loop {
        if inherited_must_support_ids.contains(&current) {
            return true;
        }
        let Some(dot) = current.rfind('.') else {
            return false;
        };
        current.truncate(dot);
    }
}

fn first_slice_anchor_id(id: &str) -> Option<String> {
    let mut out = String::new();
    for (index, segment) in id.split('.').enumerate() {
        if index > 0 {
            out.push('.');
        }
        let (base, has_slice) = segment_base_and_slice_marker(segment);
        out.push_str(base);
        if has_slice {
            return Some(out);
        }
    }
    None
}

fn extension_slice_root_has_differential_must_support(
    _id: &str,
    _diff_must_support_ids: Option<&HashSet<String>>,
) -> bool {
    false
}

fn is_direct_extension_slice_value_id(id: &str) -> bool {
    let mut segments = id.rsplit('.');
    if segments.next() != Some("value[x]") {
        return false;
    }
    segments.next().is_some_and(|segment| {
        let (base, has_slice) = segment_base_and_slice_marker(segment);
        has_slice && matches!(base, "extension" | "modifierExtension")
    })
}

fn is_slice_descendant_id(id: &str) -> bool {
    id.find([':', '/'])
        .is_some_and(|index| id[index + 1..].contains('.'))
}

fn is_child_below_slice_id(id: &str) -> bool {
    if !has_slice_marker(id) {
        return false;
    }
    let last_segment = id.rsplit('.').next().unwrap_or(id);
    !has_slice_marker(last_segment)
}

fn is_direct_slice_id(id: &str) -> bool {
    if !has_slice_marker(id) {
        return false;
    }
    let last_segment = id.rsplit('.').next().unwrap_or(id);
    has_slice_marker(last_segment)
}

fn diff_has_obligation_extension(diff: &Value) -> bool {
    diff.get("extension")
        .and_then(Value::as_array)
        .is_some_and(|exts| exts.iter().any(is_obligation_extension))
}

fn remove_obligation_extensions(target: &mut Value) {
    let Some(obj) = target.as_object_mut() else {
        return;
    };
    let Some(Value::Array(exts)) = obj.get_mut("extension") else {
        return;
    };
    exts.retain(|ext| !is_obligation_extension(ext));
    if exts.is_empty() {
        obj.remove("extension");
    }
}

fn remove_unprovenanced_obligation_extensions(target: &mut Value) {
    let Some(obj) = target.as_object_mut() else {
        return;
    };
    let Some(Value::Array(exts)) = obj.get_mut("extension") else {
        return;
    };
    exts.retain(|ext| !is_obligation_extension(ext) || obligation_has_snapshot_source(ext));
    if exts.is_empty() {
        obj.remove("extension");
    }
}

fn obligation_has_snapshot_source(ext: &Value) -> bool {
    ext.get("extension")
        .and_then(Value::as_array)
        .is_some_and(|children| {
            children.iter().any(|child| {
                child.get("url").and_then(Value::as_str)
                    == Some("http://hl7.org/fhir/tools/StructureDefinition/snapshot-source")
            })
        })
}

fn is_obligation_extension(ext: &Value) -> bool {
    ext.get("url").and_then(Value::as_str)
        == Some("http://hl7.org/fhir/StructureDefinition/obligation")
}

fn is_structuredefinition_hierarchy_extension(ext: &Value) -> bool {
    ext.get("url").and_then(Value::as_str) == Some(STRUCTUREDEFINITION_HIERARCHY_URL)
}

fn normalize_type_slicing(element: &mut Value, diff: &Value) {
    if diff
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(is_child_below_slice_id)
    {
        return;
    }
    let Some(types) = diff.get("type").and_then(Value::as_array) else {
        return;
    };
    let has_reference = types
        .iter()
        .any(|ty| ty.get("code").and_then(Value::as_str) == Some("Reference"));
    let has_codeable_concept = types
        .iter()
        .any(|ty| ty.get("code").and_then(Value::as_str) == Some("CodeableConcept"));
    if !has_reference || !has_codeable_concept {
        return;
    }
    let is_type_slicing = diff
        .get("slicing")
        .and_then(|s| s.get("discriminator"))
        .and_then(Value::as_array)
        .map(|discriminators| {
            discriminators.iter().any(|d| {
                d.get("type").and_then(Value::as_str) == Some("type")
                    && d.get("path").and_then(Value::as_str) == Some("$this")
            })
        })
        .unwrap_or(false);
    if !is_type_slicing {
        return;
    }
    if let Some(slicing) = element.get_mut("slicing") {
        set_field(slicing, "rules", Value::String("closed".to_string()));
    }
}

fn check_extension_doco(element: &mut Value) -> bool {
    let path = element
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let base_path = element
        .get("base")
        .and_then(|b| b.get("path"))
        .and_then(Value::as_str);
    let is_extension = (path == "Extension"
        || path.ends_with(".extension")
        || path.ends_with(".modifierExtension"))
        && base_path != Some("II.extension")
        && !has_profiled_extension_type(element);
    if is_extension {
        set_field(
            element,
            "definition",
            Value::String("An Extension".to_string()),
        );
        set_field(element, "short", Value::String("Extension".to_string()));
        remove_field(element, "comment");
        remove_field(element, "requirements");
        remove_field(element, "alias");
        remove_field(element, "mapping");
    }
    is_extension
}

fn first_extension_profile_url(element: &Value) -> Option<&str> {
    let ty = element
        .get("type")
        .and_then(Value::as_array)?
        .iter()
        .find(|t| t.get("code").and_then(Value::as_str) == Some("Extension"))?;
    ty.get("profile")
        .and_then(Value::as_array)?
        .first()?
        .as_str()
}

fn has_profiled_extension_type(element: &Value) -> bool {
    first_extension_profile_url(element).is_some()
}

fn apply_extension_profile_root(
    slice: &mut Value,
    diff: &Value,
    ctx: &PackageContext,
    native_r5: bool,
    host_extension_source: Option<&str>,
    copy_profile_root_condition: bool,
    allow_local_root_constraints: bool,
    allow_local_root_condition: bool,
) -> anyhow::Result<()> {
    let diff_supplies_extension_profile = first_extension_profile_url(diff).is_some();
    let Some(profile_url_owned) = first_extension_profile_url(diff)
        .or_else(|| first_extension_profile_url(slice))
        .map(str::to_string)
    else {
        return Ok(());
    };
    let profile_url = profile_url_owned.as_str();
    if uses_generic_extension_doco_profile(profile_url) {
        apply_generic_extension_doco(slice);
        return Ok(());
    }
    let is_local_profile = ctx.is_local(profile_url);
    let Some(profile) = profile_with_snapshot(profile_url, ctx, native_r5)? else {
        if apply_native_r5_known_extension_root(slice, profile_url, native_r5) {
            return Ok(());
        }
        apply_missing_profile_extension_slice_doco(slice);
        return Ok(());
    };
    let Some(root) = profile
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    else {
        return Ok(());
    };
    let mut root = root.clone();
    let root_explicit_is_summary = root.get("isSummary").is_some();
    // Extension-root doco keeps Publisher's known-relative links (e.g.
    // workflow-extensions.html#instantiation) as-is; only freshly applied
    // extension slices reach here. Inherited copies of the same slice go through
    // normalize_inherited_element, which rewrites them to the spec URL.
    rewrite_markdown_links(
        &mut root,
        &spec_url_for_structure(&profile, native_r5),
        true,
    );
    let project_local_root_constraints =
        allow_local_root_constraints && projects_local_extension_root_constraints(profile_url);
    let local_profile_had_loaded_snapshot =
        is_local_profile && ctx.resource_has_loaded_snapshot(profile_url);
    if native_r5 && is_local_profile {
        if project_local_root_constraints {
            if local_profile_had_loaded_snapshot {
                if let Some(r4_root) =
                    profile_with_snapshot(profile_url, ctx, false)?.and_then(|profile| {
                        profile
                            .get("snapshot")
                            .and_then(|s| s.get("element"))
                            .and_then(Value::as_array)
                            .and_then(|a| a.first())
                            .cloned()
                    })
                {
                    add_constraint_xpath_extensions_from_source(&mut root, &r4_root);
                } else {
                    convert_own_constraint_xpaths_to_extensions(&mut root);
                }
            } else {
                strip_constraint_extensions(&mut root);
            }
            strip_constraint_xpaths(&mut root);
        }
        if root.get("isSummary").is_none() {
            set_field(&mut root, "isSummary", Value::Bool(false));
        }
    }
    if diff_supplies_extension_profile
        && is_core_extension_profile(profile_url)
        && root.get("isSummary").is_none()
    {
        set_field(&mut root, "isSummary", Value::Bool(false));
    }
    adjust_extension_root_constraint_sources(
        &mut root,
        slice,
        is_local_profile && project_local_root_constraints,
        profile_url,
        host_extension_source,
    );
    if !copy_profile_root_condition {
        remove_field(&mut root, "condition");
    }
    if (!is_local_profile && !allow_local_root_constraints)
        || (is_local_profile
            && !allow_local_root_condition
            && !keeps_extension_root_condition(profile_url))
        || omits_extension_root_condition(profile_url)
    {
        remove_field(&mut root, "condition");
    }
    if !allow_local_root_constraints {
        remove_field(&mut root, "constraint");
    } else if is_local_profile && !project_local_root_constraints {
        retain_base_extension_constraints(&mut root);
    }
    let is_modifier_extension_slice = slice
        .get("path")
        .or_else(|| diff.get("path"))
        .and_then(Value::as_str)
        .is_some_and(|path| path.ends_with(".modifierExtension"));
    if is_local_profile
        && root.get("comment").is_some()
        && !has_semantic_element_extensions(slice)
        && !has_semantic_element_extensions(diff)
        && !is_modifier_extension_slice
    {
        strip_constraint_extensions(&mut root);
    }
    fill_missing_constraint_sources_on_constrained_element(
        &mut root,
        host_extension_source.unwrap_or(profile_url),
    );
    apply_native_r5_variable_extension_comment(&mut root, profile_url, native_r5);

    let mut overlay_keys = vec![
        "short",
        "definition",
        "comment",
        "requirements",
        "alias",
        "isModifier",
        "isModifierReason",
        "mapping",
    ];
    if copy_profile_root_condition {
        overlay_keys.push("condition");
    }
    for key in overlay_keys {
        if let Some(value) = root.get(key) {
            set_field(slice, key, extension_root_overlay_value(key, value));
        } else {
            remove_field(slice, key);
        }
    }
    merge_min_cardinality(slice, &root);
    merge_max_cardinality(slice, &root);
    // isSummary is never stripped by Java's root overlay: the slice keeps whatever
    // it inherits (a stored slice like us-core birthsex carries none; a fresh slice
    // cloned from the unsliced extension element carries false). Synthetic native
    // R5 defaults added above do not count as explicit root values.
    if root_explicit_is_summary {
        let Some(value) = root.get("isSummary") else {
            return Ok(());
        };
        set_field(slice, "isSummary", value.clone());
    }

    if let Some(root_constraints) = root.get("constraint").and_then(Value::as_array) {
        let existing = slice
            .get("constraint")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        remove_field(slice, "constraint");
        let target_constraints = ensure_array_field(slice, "constraint");
        for constraint in root_constraints {
            target_constraints.push(constraint.clone());
        }
        for constraint in existing {
            let key = constraint.get("key").and_then(Value::as_str);
            if key.is_some_and(|key| {
                target_constraints
                    .iter()
                    .any(|existing| existing.get("key").and_then(Value::as_str) == Some(key))
            }) {
                continue;
            }
            target_constraints.push(constraint);
        }
    }
    Ok(())
}

fn extension_root_overlay_value(key: &str, value: &Value) -> Value {
    if matches!(key, "short" | "definition" | "comment" | "requirements") {
        if let Some(text) = value.as_str() {
            return Value::String(text.trim_end().to_string());
        }
    }
    value.clone()
}

fn apply_native_r5_variable_extension_comment(
    root: &mut Value,
    profile_url: &str,
    native_r5: bool,
) {
    if !native_r5 || profile_url != "http://hl7.org/fhir/StructureDefinition/variable" {
        return;
    }
    const R4_CORE_VARIABLE_COMMENT: &str = "Ordering of variable extension declarations is significant as variables declared in one repetition of this extension might be used in subsequent extension repetitions.";
    const NATIVE_R5_VARIABLE_COMMENT: &str = "Ordering of variable extension declarations is significant as variables declared in one repetition of this extension might be used in subsequent extension repetitions\n\nFor questionnaires, see additional guidance and examples in the [SDC implementation guide](http://hl7.org/fhir/uv/sdc/2025Jan/behavior.html#variable).";
    if root.get("comment").and_then(Value::as_str) == Some(R4_CORE_VARIABLE_COMMENT) {
        set_field(
            root,
            "comment",
            Value::String(NATIVE_R5_VARIABLE_COMMENT.to_string()),
        );
    }
}

fn apply_native_r5_known_extension_root(
    slice: &mut Value,
    profile_url: &str,
    native_r5: bool,
) -> bool {
    if !native_r5 || profile_url != "http://hl7.org/fhir/StructureDefinition/cqf-fhirQueryPattern" {
        return false;
    }
    const CQF_FHIR_QUERY_PATTERN_DEFINITION: &str = "A FHIR Query URL pattern that corresponds to the data specified by the data requirement. If multiple FHIR Query URLs are present, they each contribute to the data specified by the data requirement (i.e. the union of the results of the FHIR Queries represents the complete data for the data requirement). This is not a resolveable URL, in that it will contain 1) No base canonical (i.e. it's a relative query), and 2) Parameters using tokens that are delimited using double-braces and the context parameters are dependent solely on the subjectType, according to the following: Patient: context.patientId, Practitioner: context.practitionerId, Organization: context.organizationId, Location: context.locationId, Device: context.deviceId. For example, for a Library with a subjectType of Patient, the context parameter `{{context.patientId}}` will be used as a token to be replaced with the `id` of the Patient in context. This extension is used primarily to address the use case for satisfying a data requirement for a single subject. However, the query pattern could also be used to satisfy population level requests by removing the subject-level filter from the query.";
    const CQF_FHIR_QUERY_PATTERN_COMMENT: &str = "Supports communicating a FHIR query (or set of queries) for the given data requirement. The query is server-specific, and will need to be created as informed by a CapabilityStatement. The $data-requirements operation should be expected to be able to provide an Endpoint or CapabilityStatement to provide this information.; If no endpoint or capability statement is provided, the capability statement of the server performing the operation is used.";
    set_field(
        slice,
        "short",
        Value::String("What FHIR query?".to_string()),
    );
    set_field(
        slice,
        "definition",
        Value::String(CQF_FHIR_QUERY_PATTERN_DEFINITION.to_string()),
    );
    set_field(
        slice,
        "comment",
        Value::String(CQF_FHIR_QUERY_PATTERN_COMMENT.to_string()),
    );
    remove_field(slice, "requirements");
    remove_field(slice, "alias");
    remove_field(slice, "mapping");
    true
}

fn retain_base_extension_constraints(element: &mut Value) {
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    constraints.retain(|constraint| {
        matches!(
            constraint.get("key").and_then(Value::as_str),
            Some("ele-1" | "ext-1")
        )
    });
}

fn strip_constraint_extensions(element: &mut Value) {
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        remove_field(constraint, "extension");
    }
}

fn apply_generic_extension_doco(element: &mut Value) {
    set_field(element, "short", Value::String("Extension".to_string()));
    set_field(
        element,
        "definition",
        Value::String("An Extension".to_string()),
    );
    remove_field(element, "comment");
    remove_field(element, "requirements");
    remove_field(element, "alias");
    remove_field(element, "condition");
    remove_field(element, "mapping");
}

fn apply_missing_profile_extension_slice_doco(element: &mut Value) {
    let is_slice = element.get("sliceName").is_some()
        || element
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(has_slice_marker);
    if !is_slice {
        return;
    }
    let path = element.get("path").and_then(Value::as_str).unwrap_or("");
    if !path.ends_with(".extension") && !path.ends_with(".modifierExtension") {
        return;
    }
    set_field(
        element,
        "definition",
        Value::String("An Extension".to_string()),
    );
    remove_field(element, "comment");
    remove_field(element, "requirements");
    remove_field(element, "alias");
    remove_field(element, "mapping");
}

fn convert_own_constraint_xpaths_to_extensions(element: &mut Value) {
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        let Some(xpath) = constraint
            .get("xpath")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        if has_constraint_xpath_extension(constraint) {
            continue;
        }
        add_constraint_extension_first(constraint, constraint_xpath_extension(&xpath));
    }
}

fn add_constraint_xpath_extensions_from_source(target: &mut Value, source: &Value) {
    let mut xpaths = HashMap::new();
    if let Some(source_constraints) = source.get("constraint").and_then(Value::as_array) {
        for constraint in source_constraints {
            let Some(key) = constraint.get("key").and_then(Value::as_str) else {
                continue;
            };
            let Some(xpath) = constraint.get("xpath").and_then(Value::as_str) else {
                continue;
            };
            xpaths.insert(key.to_string(), xpath.to_string());
        }
    }

    let Some(target_constraints) = target.get_mut("constraint").and_then(Value::as_array_mut)
    else {
        return;
    };
    for constraint in target_constraints {
        let Some(key) = constraint.get("key").and_then(Value::as_str) else {
            continue;
        };
        let Some(xpath) = xpaths.get(key) else {
            continue;
        };
        if has_constraint_xpath_extension(constraint) {
            continue;
        }
        add_constraint_extension_first(constraint, constraint_xpath_extension(xpath));
    }
}

fn adjust_extension_root_constraint_sources(
    root: &mut Value,
    slice: &Value,
    local_profile: bool,
    profile_url: &str,
    host_extension_source: Option<&str>,
) {
    let Some(source) = extension_slice_ext_constraint_source(
        slice,
        local_profile,
        profile_url,
        host_extension_source,
    ) else {
        return;
    };
    let Some(constraints) = root.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        if constraint.get("key").and_then(Value::as_str) == Some("ext-1")
            && constraint.get("source").is_none()
        {
            set_field(constraint, "source", Value::String(source.clone()));
        }
    }
}

fn extension_slice_ext_constraint_source(
    slice: &Value,
    local_profile: bool,
    profile_url: &str,
    host_extension_source: Option<&str>,
) -> Option<String> {
    let path = slice.get("path").and_then(Value::as_str)?;
    if !path.ends_with(".extension") && !path.ends_with(".modifierExtension") {
        return None;
    }
    if local_profile || !is_core_extension_profile(profile_url) {
        Some(
            host_extension_source
                .unwrap_or_else(|| {
                    path.split_once('.')
                        .map(|(root, _)| root)
                        .unwrap_or("Extension")
                })
                .to_string(),
        )
    } else {
        Some("http://hl7.org/fhir/StructureDefinition/Extension".to_string())
    }
}

fn projects_local_extension_root_constraints(profile_url: &str) -> bool {
    !profile_url.ends_with("/mcode-histology-morphology-behavior")
}

fn omits_extension_root_condition(profile_url: &str) -> bool {
    profile_url.ends_with("/mcode-histology-morphology-behavior")
        || profile_url == "http://hl7.org/fhir/StructureDefinition/condition-related"
        || profile_url == "http://hl7.org/fhir/StructureDefinition/alternate-reference"
        || profile_url
            == "http://hl7.org/fhir/us/ph-library/StructureDefinition/us-ph-named-eventtype-extension"
}

fn keeps_extension_root_condition(profile_url: &str) -> bool {
    profile_url == "http://hl7.org/fhir/us/ndh/StructureDefinition/base-ext-org-alias-type"
        || profile_url == "http://hl7.org/fhir/us/ndh/StructureDefinition/base-ext-org-alias-period"
}

fn is_core_extension_profile(profile_url: &str) -> bool {
    profile_url.starts_with("http://hl7.org/fhir/StructureDefinition/")
        || (profile_url.starts_with("http://hl7.org/fhir/5.0/StructureDefinition/extension-"))
}

fn uses_generic_extension_doco_profile(profile_url: &str) -> bool {
    let bare_url = profile_url
        .split_once('|')
        .map(|(url, _)| url)
        .unwrap_or(profile_url);
    bare_url == "http://hl7.org/fhir/StructureDefinition/codeOptions"
        || profile_url == "http://hl7.org/fhir/StructureDefinition/artifact-versionAlgorithm|5.2.0"
}

fn apply_type_profile_root(
    target: &mut Value,
    diff: &Value,
    ctx: &PackageContext,
    native_r5: bool,
) -> anyhow::Result<()> {
    let Some(profile_url) = first_non_extension_profile_url(diff) else {
        return Ok(());
    };
    let Some(profile) = ctx.fetch(profile_url) else {
        return Ok(());
    };
    let root_candidate = if let Some((root, source, snapshot_source)) =
        local_differential_slice_resource_type_root(target, diff, &profile, profile_url, ctx)?
    {
        Some((root, source, snapshot_source))
    } else {
        if !uses_profile_root_overlay(&profile) {
            return Ok(());
        }
        profile_root_element(&profile, ctx)?.map(|root| {
            (
                root,
                profile_url.to_string(),
                snapshot_source_value(&profile),
            )
        })
    };
    let Some((mut root, root_source, root_snapshot_source)) = root_candidate else {
        return Ok(());
    };
    let root_must_support = root
        .get("mustSupport")
        .cloned()
        .or_else(|| profile_root_must_support(&profile));
    if native_r5 {
        let constraint_xpaths = HashMap::new();
        let preserve_common_binding =
            native_r5 && is_r4_spec_url(&spec_url_for_structure(&profile, native_r5));
        project_element_to_native_r5(
            &mut root,
            &root_source,
            root_snapshot_source.as_deref(),
            &constraint_xpaths,
            None,
            false,
            true,
            None,
            preserve_common_binding,
        );
    }
    fill_missing_constraint_sources_on_constrained_element(&mut root, &root_source);

    // The short/definition/comment/requirements/alias/mapping overlay always
    // applies (ProfileUtilities.updateFromDefinition PU:2657-2671). The
    // isModifier/isModifierReason/isSummary/condition root values only carry over
    // when the element narrows to a single profiled type (the type-redirect path);
    // a multi-typed element (e.g. DTR Parameters.parameter:order.resource with 9
    // candidate profiles) keeps its inherited isSummary/condition.
    let single_type = diff
        .get("type")
        .and_then(Value::as_array)
        .map(|types| types.len() == 1)
        .unwrap_or(false);
    let mut keys: Vec<&str> = vec![
        "short",
        "definition",
        "comment",
        "requirements",
        "alias",
        "mapping",
    ];
    if single_type {
        keys.extend(["condition", "isModifier", "isModifierReason", "isSummary"]);
    }
    for key in keys {
        if diff.get(key).is_some() {
            continue;
        }
        if let Some(value) = root.get(key) {
            set_field(target, key, value.clone());
        } else if key != "comment" {
            remove_field(target, key);
        }
    }
    if single_type
        && target
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with(".resource"))
    {
        if let Some(value) = root.get("mustSupport").or(root_must_support.as_ref()) {
            set_field(target, "mustSupport", value.clone());
        }
    }
    Ok(())
}

fn profile_root_must_support(profile: &Value) -> Option<Value> {
    profile
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|root| root.get("mustSupport"))
        .cloned()
        .or_else(|| {
            profile
                .get("differential")
                .and_then(|s| s.get("element"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|root| root.get("mustSupport"))
                .cloned()
        })
}

fn local_differential_slice_resource_type_root(
    target: &Value,
    diff: &Value,
    profile: &Value,
    profile_url: &str,
    ctx: &PackageContext,
) -> anyhow::Result<Option<(Value, String, Option<String>)>> {
    let root_diff = profile
        .get("differential")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .and_then(|elements| elements.first());
    if !(ctx.is_local(profile_url)
        && profile.get("snapshot").is_none()
        && root_diff.is_some_and(|root| is_profile_root_diff(profile, root))
        && root_diff.is_none_or(|root| !root_diff_has_profile_text_overlay(root))
        && target
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with(".resource"))
        && diff
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(is_slice_descendant_id))
    {
        return Ok(None);
    }
    let Some(type_code) = first_non_extension_type_code(diff) else {
        return Ok(None);
    };
    let Some(type_def) = ctx.fetch(type_code) else {
        return Ok(None);
    };
    let source = structure_source(&type_def, type_code);
    let snapshot_source = snapshot_source_value(&type_def);
    let mut root = profile_root_element(&type_def, ctx)?;
    if let Some(root) = root.as_mut() {
        if let Some(profile_root) = profile_root_element(profile, ctx)? {
            prepend_profile_only_mappings(root, &profile_root);
        }
    }
    Ok(root.map(|root| (root, source, snapshot_source)))
}

fn root_diff_has_profile_text_overlay(root: &Value) -> bool {
    ["short", "definition", "comment", "requirements", "alias"]
        .into_iter()
        .any(|key| root.get(key).is_some())
}

fn prepend_profile_only_mappings(root: &mut Value, profile_root: &Value) {
    let Some(profile_mappings) = profile_root.get("mapping").and_then(Value::as_array) else {
        return;
    };
    let existing = root
        .get("mapping")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut additions = Vec::new();
    for mapping in profile_mappings {
        if !existing.iter().any(|candidate| candidate == mapping)
            && !additions.iter().any(|candidate| candidate == mapping)
        {
            additions.push(mapping.clone());
        }
    }
    if additions.is_empty() {
        return;
    }
    additions.extend(existing);
    set_field(root, "mapping", Value::Array(additions));
}

fn first_non_extension_type_code(element: &Value) -> Option<&str> {
    element
        .get("type")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|t| t.get("code").and_then(Value::as_str))
        .find(|code| *code != "Extension")
}

fn uses_profile_root_overlay(profile: &Value) -> bool {
    matches!(
        profile.get("kind").and_then(Value::as_str),
        Some("resource" | "logical")
    )
}

fn profile_root_element(profile: &Value, ctx: &PackageContext) -> anyhow::Result<Option<Value>> {
    if let Some(root) = profile
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    {
        return Ok(Some(root.clone()));
    }

    let profile_url = structure_url_or(profile, "");
    let diff_root = profile
        .get("differential")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .and_then(|a| a.first());
    let has_root_diff = diff_root.is_some_and(|diff| is_profile_root_diff(profile, diff));
    if ctx.is_local(&profile_url) && !has_root_diff {
        let generated = generate_snapshot(
            profile.clone(),
            ctx,
            SnapshotOptions {
                sort_differential: true,
                native_r5: false,
                apply_extension_root_doco: false,
            },
        )?;
        return Ok(generated
            .get("snapshot")
            .and_then(|s| s.get("element"))
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .cloned());
    }
    let base_is_differential_only = profile
        .get("baseDefinition")
        .and_then(Value::as_str)
        .and_then(|base_url| ctx.fetch(base_url))
        .is_some_and(|base| base.get("snapshot").is_none());
    if ctx.is_local(&profile_url) && base_is_differential_only {
        let generated = generate_snapshot(
            profile.clone(),
            ctx,
            SnapshotOptions {
                sort_differential: true,
                native_r5: false,
                apply_extension_root_doco: false,
            },
        )?;
        return Ok(generated
            .get("snapshot")
            .and_then(|s| s.get("element"))
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .cloned());
    }

    let base_root = profile
        .get("baseDefinition")
        .and_then(Value::as_str)
        .and_then(|base_url| ctx.fetch(base_url))
        .and_then(|base| {
            base.get("snapshot")
                .and_then(|s| s.get("element"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .cloned()
                .map(|root| (root, strips_non_inherited_extensions(&base)))
        });

    match (base_root, diff_root) {
        (Some((mut root, strip_non_inherited)), Some(diff_root))
            if is_profile_root_diff(profile, diff_root) =>
        {
            let profile_constraint_source = structure_url_or(profile, "");
            merge_diff_into_element(
                &mut root,
                diff_root,
                strip_non_inherited,
                false,
                None,
                None,
                None,
                &profile_constraint_source,
            )?;
            Ok(Some(root))
        }
        (Some((root, _)), _) => Ok(Some(root)),
        (None, Some(diff_root)) if is_profile_root_diff(profile, diff_root) => {
            Ok(Some(diff_root.clone()))
        }
        _ => {
            // No usable root via the shallow path (e.g. a local profile whose
            // differential has no root element and whose base also lacks a stored
            // snapshot). Generate the profile's full R4 snapshot recursively and
            // take its root, matching the Java oracle which resolves the fully
            // generated profile before overlaying its root.
            let generated = generate_snapshot(
                profile.clone(),
                ctx,
                SnapshotOptions {
                    sort_differential: true,
                    native_r5: false,
                    apply_extension_root_doco: false,
                },
            )?;
            Ok(generated
                .get("snapshot")
                .and_then(|s| s.get("element"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .cloned())
        }
    }
}

fn is_profile_root_diff(profile: &Value, diff: &Value) -> bool {
    let Some(profile_type) = profile.get("type").and_then(Value::as_str) else {
        return false;
    };
    diff.get("id")
        .or_else(|| diff.get("path"))
        .and_then(Value::as_str)
        == Some(profile_type)
}

fn first_non_extension_profile_url(element: &Value) -> Option<&str> {
    let ty = element
        .get("type")
        .and_then(Value::as_array)?
        .iter()
        .find(|t| {
            t.get("code")
                .and_then(Value::as_str)
                .map(|code| code != "Extension")
                .unwrap_or(false)
                && t.get("profile")
                    .and_then(Value::as_array)
                    .map(|p| !p.is_empty())
                    .unwrap_or(false)
        })?;
    ty.get("profile")
        .and_then(Value::as_array)?
        .first()?
        .as_str()
}

fn single_non_extension_profile_url(element: &Value) -> Option<&str> {
    let types = element.get("type").and_then(Value::as_array)?;
    if types.len() != 1 {
        return None;
    }
    let ty = types.first()?;
    if ty.get("code").and_then(Value::as_str) == Some("Extension") {
        return None;
    }
    let profiles = ty.get("profile").and_then(Value::as_array)?;
    if profiles.len() != 1 {
        return None;
    }
    profiles.first()?.as_str()
}

#[derive(Clone, Copy)]
enum TextMerge {
    Replace,
    Markdown,
    String,
}

fn merge_text_field(target: &mut Value, diff: &Value, key: &str, mode: TextMerge) {
    let Some(derived) = diff.get(key).and_then(Value::as_str) else {
        return;
    };
    let base = target.get(key).and_then(Value::as_str);
    let value = match mode {
        TextMerge::Replace => derived.to_string(),
        TextMerge::Markdown => merge_markdown(base, derived),
        TextMerge::String => merge_string(base, derived),
    };
    set_field(target, key, Value::String(value));
}

fn merge_markdown(base: Option<&str>, derived: &str) -> String {
    if derived.starts_with("...") {
        append_derived_text_to_base(base, derived)
    } else if derived.is_empty() {
        base.unwrap_or("").to_string()
    } else {
        derived.to_string()
    }
}

fn merge_string(base: Option<&str>, derived: &str) -> String {
    if derived.starts_with("...") {
        // R5 mergeStrings passes appendDerivedTextToBase arguments in the opposite
        // order from mergeMarkdown. Preserve that quirk.
        let suffix_source = base.unwrap_or("");
        if suffix_source.starts_with("...") {
            format!("{derived}\r\n{}", &suffix_source[3..])
        } else {
            format!("{derived}\r\n{suffix_source}")
        }
    } else if derived.is_empty() {
        base.unwrap_or("").to_string()
    } else {
        derived.to_string()
    }
}

fn append_derived_text_to_base(base: Option<&str>, derived: &str) -> String {
    let derived_tail = derived.strip_prefix("...").unwrap_or(derived);
    match base {
        Some(base) if !base.is_empty() => format!("{base}\r\n{derived_tail}"),
        _ => derived.to_string(),
    }
}

fn merge_unique_array_strings(target: &mut Value, diff: &Value, key: &str) {
    let Some(derived) = diff.get(key).and_then(Value::as_array) else {
        return;
    };
    let target_arr = ensure_array_field(target, key);
    for item in derived {
        if !target_arr.contains(item) {
            target_arr.push(item.clone());
        }
    }
}

fn merge_unique_values(target: &mut Value, diff: &Value, key: &str) {
    let Some(derived) = diff.get(key).and_then(Value::as_array) else {
        return;
    };
    let target_arr = ensure_array_field(target, key);
    for item in derived {
        if !target_arr.contains(item) {
            target_arr.push(item.clone());
        }
    }
}

fn merge_unique_values_prepend(target: &mut Value, diff: &Value, key: &str) {
    let Some(derived) = diff.get(key).and_then(Value::as_array) else {
        return;
    };
    let existing = target
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut merged = Vec::new();
    for item in derived {
        if !merged_contains_by_semantic_key(&merged, item, key) {
            merged.push(item.clone());
        }
    }
    for item in existing {
        if !merged_contains_by_semantic_key(&merged, &item, key) {
            merged.push(item);
        }
    }
    set_field(target, key, Value::Array(merged));
}

fn merged_contains_by_semantic_key(merged: &[Value], item: &Value, key: &str) -> bool {
    if key == "mapping" {
        let item_identity = item.get("identity").and_then(Value::as_str);
        let item_map = item.get("map").and_then(Value::as_str);
        if item_identity.is_some() && item_map.is_some() {
            return merged.iter().any(|existing| {
                existing.get("identity").and_then(Value::as_str) == item_identity
                    && existing.get("map").and_then(Value::as_str) == item_map
            });
        }
    }
    merged.contains(item)
}

fn merge_unique_by_key(target: &mut Value, diff: &Value, key: &str, id_key: &str) {
    let Some(derived) = diff.get(key).and_then(Value::as_array) else {
        return;
    };
    let target_arr = ensure_array_field(target, key);
    for item in derived {
        let item_key = item.get(id_key).and_then(Value::as_str);
        let exists = item_key
            .map(|k| {
                target_arr
                    .iter()
                    .any(|existing| existing.get(id_key).and_then(Value::as_str) == Some(k))
            })
            .unwrap_or_else(|| target_arr.contains(item));
        if !exists {
            target_arr.push(item.clone());
        }
    }
}

fn merge_binding(target: &mut Value, diff: &Value) {
    let Some(derived) = diff.get("binding").and_then(Value::as_object) else {
        return;
    };
    let mut nb = target
        .get("binding")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    if let Some(obj) = nb.as_object_mut() {
        obj.remove("extension");
        obj.remove("description");
        if let Some(ext) = derived.get("extension") {
            obj.insert("extension".to_string(), ext.clone());
        }
        for key in ["strength", "description", "valueSet"] {
            if let Some(v) = derived.get(key) {
                obj.insert(key.to_string(), v.clone());
            }
        }
        if let Some(additional) = derived.get("additional").and_then(Value::as_array) {
            let entry = obj
                .entry("additional".to_string())
                .or_insert_with(|| Value::Array(vec![]));
            let Some(target_additional) = entry.as_array_mut() else {
                return;
            };
            for item in additional {
                merge_additional_binding(target_additional, item);
            }
        }
        if matches!(obj.get("extension"), Some(Value::Array(a)) if a.is_empty()) {
            obj.remove("extension");
        }
    }
    set_field(target, "binding", nb);
}

fn merge_additional_binding(target: &mut Vec<Value>, source: &Value) {
    let source_vs = source.get("valueSet");
    let source_purpose = source.get("purpose");
    let source_has_usage = source
        .get("usage")
        .and_then(Value::as_array)
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if !source_has_usage {
        if let Some(existing) = target
            .iter_mut()
            .find(|item| item.get("valueSet") == source_vs && item.get("purpose") == source_purpose)
        {
            if let (Some(existing_obj), Some(source_obj)) =
                (existing.as_object_mut(), source.as_object())
            {
                for key in ["shortDoco", "documentation", "any"] {
                    if let Some(v) = source_obj.get(key) {
                        existing_obj.insert(key.to_string(), v.clone());
                    }
                }
                if let Some(source_usage) = source_obj.get("usage").and_then(Value::as_array) {
                    let usage = existing_obj
                        .entry("usage".to_string())
                        .or_insert_with(|| Value::Array(vec![]));
                    if let Some(usage) = usage.as_array_mut() {
                        for u in source_usage {
                            if !usage.contains(u) {
                                usage.push(u.clone());
                            }
                        }
                    }
                }
            }
            return;
        }
    }
    target.push(source.clone());
}

fn merge_extensions_from_definition(
    target: &mut Value,
    diff: &Value,
    strip_non_inherited: bool,
    preserve_common_binding: bool,
) {
    if strip_non_inherited {
        remove_non_inherited_extensions_with_binding_policy(
            target,
            preserve_common_binding && has_semantic_element_extensions(target),
        );
    }
    dedupe_extension_values(target, "extension");
    let Some(source_exts) = diff.get("extension").and_then(Value::as_array) else {
        return;
    };
    let target_exts = ensure_array_field(target, "extension");
    for ext in source_exts {
        target_exts.push(ext.clone());
    }
    dedupe_extension_values_except(target, "extension", allows_duplicate_extension_url);
}

fn dedupe_extension_values(parent: &mut Value, key: &str) {
    dedupe_extension_values_except(parent, key, |_| false);
}

fn dedupe_extension_values_except(
    parent: &mut Value,
    key: &str,
    allow_duplicate_url: impl Fn(&str) -> bool,
) {
    let Some(obj) = parent.as_object_mut() else {
        return;
    };
    let Some(Value::Array(exts)) = obj.get_mut(key) else {
        return;
    };
    let mut seen: Vec<Value> = Vec::new();
    exts.retain(|ext| {
        if ext
            .get("url")
            .and_then(Value::as_str)
            .is_some_and(&allow_duplicate_url)
        {
            return true;
        }
        if seen.contains(ext) {
            false
        } else {
            seen.push(ext.clone());
            true
        }
    });
    if exts.is_empty() {
        obj.remove(key);
    }
}

fn allows_duplicate_extension_url(url: &str) -> bool {
    url == USCDI_REQUIREMENT_EXTENSION_URL
}

fn has_bindable_type(element: &Value) -> bool {
    let Some(types) = element.get("type").and_then(Value::as_array) else {
        return false;
    };
    types.iter().any(|t| {
        matches!(
            t.get("code").and_then(Value::as_str),
            Some(
                "Coding"
                    | "CodeableConcept"
                    | "Quantity"
                    | "uri"
                    | "string"
                    | "code"
                    | "CodeableReference"
            )
        )
    })
}

fn merge_type_entries(target: &mut Value, derived: &Value) {
    let Some(derived_types) = derived.as_array() else {
        set_field(target, "type", derived.clone());
        return;
    };
    let inherited_types = target
        .get("type")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut merged = Vec::new();
    for derived_type in derived_types {
        let mut next = derived_type.clone();
        if let Some(code) = derived_type.get("code").and_then(Value::as_str) {
            if let Some(inherited_type) = inherited_types
                .iter()
                .find(|candidate| candidate.get("code").and_then(Value::as_str) == Some(code))
            {
                merge_type_extensions(&mut next, inherited_type);
            }
        }
        merged.push(next);
    }
    set_field(target, "type", Value::Array(merged));
}

fn merge_type_extensions(derived_type: &mut Value, inherited_type: &Value) {
    let Some(inherited_exts) = inherited_type.get("extension").and_then(Value::as_array) else {
        return;
    };
    let Some(derived_obj) = derived_type.as_object_mut() else {
        return;
    };
    let mut merged = derived_obj
        .get("extension")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for ext in inherited_exts.iter().filter(|ext| {
        !is_obligation_extension(ext) && !is_structuredefinition_hierarchy_extension(ext)
    }) {
        if !merged.contains(ext) {
            merged.push(ext.clone());
        }
    }
    for ext in inherited_exts.iter().filter(|ext| {
        is_obligation_extension(ext) && !is_structuredefinition_hierarchy_extension(ext)
    }) {
        if !merged.contains(ext) {
            merged.push(ext.clone());
        }
    }
    if !merged.is_empty() {
        derived_obj.insert("extension".to_string(), Value::Array(merged));
    }
}

fn is_root_element(element: &Value) -> bool {
    !element
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .contains('.')
}

fn copy_if_present(target: &mut Value, diff: &Value, key: &str) {
    if let Some(value) = diff.get(key) {
        set_field(target, key, value.clone());
    }
}

fn merge_min_cardinality(target: &mut Value, diff: &Value) {
    let Some(diff_min) = diff.get("min").and_then(Value::as_u64) else {
        return;
    };
    let merged = target
        .get("min")
        .and_then(Value::as_u64)
        .map(|current| current.max(diff_min))
        .unwrap_or(diff_min);
    set_field(target, "min", Value::Number(merged.into()));
}

fn merge_max_cardinality(target: &mut Value, diff: &Value) {
    let Some(diff_max) = diff.get("max").and_then(Value::as_str) else {
        return;
    };
    let target_max = target.get("max").and_then(Value::as_str);
    let merged = match (target_max, diff_max) {
        (Some(current), "*") => current.to_string(),
        (Some("*"), next) => next.to_string(),
        (Some(current), next) => {
            let current_num = current.parse::<u32>().ok();
            let next_num = next.parse::<u32>().ok();
            match (current_num, next_num) {
                (Some(current), Some(next)) => current.min(next).to_string(),
                _ => next.to_string(),
            }
        }
        (None, next) => next.to_string(),
    };
    set_field(target, "max", Value::String(merged));
}

fn copy_choice_prefix(target: &mut Value, diff: &Value, prefix: &str) {
    let Some(obj) = diff.as_object() else {
        return;
    };
    for (key, value) in obj {
        if key.starts_with(prefix) {
            remove_choice_prefix(target, prefix);
            set_field(target, key, value.clone());
        }
    }
}

fn remove_choice_prefix(target: &mut Value, prefix: &str) {
    let Some(obj) = target.as_object_mut() else {
        return;
    };
    let keys: Vec<String> = obj
        .keys()
        .filter(|key| key.starts_with(prefix))
        .cloned()
        .collect();
    for key in keys {
        obj.remove(&key);
    }
}

fn set_field(target: &mut Value, key: &str, value: Value) {
    let Some(target) = target.as_object_mut() else {
        return;
    };
    target.insert(key.to_string(), value);
}

fn remove_field(target: &mut Value, key: &str) {
    if let Some(target) = target.as_object_mut() {
        target.remove(key);
    }
}

fn ensure_array_field<'a>(target: &'a mut Value, key: &str) -> &'a mut Vec<Value> {
    let Some(target) = target.as_object_mut() else {
        panic!("element is not an object");
    };
    let entry = target
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(vec![]));
    if !entry.is_array() {
        *entry = Value::Array(vec![]);
    }
    entry.as_array_mut().expect("array just inserted")
}

fn normalize_inherited_element(
    mut element: Value,
    source_url: &str,
    spec_url: &str,
    strip_non_inherited: bool,
    native_r5: bool,
    constraint_source: &str,
    snapshot_source: Option<&str>,
    convert_own_xpaths: bool,
) -> Value {
    if strip_non_inherited {
        remove_non_inherited_extensions_with_binding_policy(
            &mut element,
            native_r5 && is_r4_spec_url(spec_url),
        );
    }
    trim_inherited_text_fields(&mut element);
    if element.get("comment").and_then(Value::as_str) == Some("-") {
        remove_field(&mut element, "comment");
    }
    rewrite_markdown_links(&mut element, spec_url, false);
    if native_r5 || spec_url.contains("/R5/") {
        absolutize_content_reference(&mut element, source_url);
    }
    if native_r5 {
        if convert_own_xpaths {
            convert_own_constraint_xpaths_to_extensions(&mut element);
        }
        let constraint_xpaths = HashMap::new();
        project_element_to_native_r5(
            &mut element,
            constraint_source,
            snapshot_source,
            &constraint_xpaths,
            None,
            false,
            true,
            None,
            native_r5 && is_r4_spec_url(spec_url),
        );
    }
    element
}

fn trim_inherited_text_fields(element: &mut Value) {
    for key in ["short", "definition", "comment", "requirements", "label"] {
        let Some(text) = element.get(key).and_then(Value::as_str) else {
            continue;
        };
        let trimmed = text.trim_end();
        if trimmed.len() != text.len() {
            set_field(element, key, Value::String(trimmed.to_string()));
        }
    }
}

// Mirrors org.hl7.fhir.r5.conformance.profile.ProfileUtilities updateFromDefinition
// (~line 3085): when a derived element is merged with its base, every base
// constraint lacking a `source` is stamped with the source StructureDefinition's
// URL (srcSD.getUrl()). This fires only for elements actually touched by the
// differential, so inherited-but-untouched constraints (e.g. CRD's us-core-16..19
// on slices it never merges) keep their missing source, while a profile that does
// constrain the slice (e.g. CARIN BB's Organization.identifier:NPI) stamps them.
fn fill_missing_constraint_sources_on_constrained_element(element: &mut Value, source: &str) {
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        let Some(obj) = constraint.as_object_mut() else {
            continue;
        };
        if !obj.contains_key("source") {
            obj.insert("source".to_string(), Value::String(source.to_string()));
        }
    }
}

fn structure_url_or(structure: &Value, fallback: &str) -> String {
    structure
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

fn spec_url_for_structure(structure: &Value, native_r5: bool) -> String {
    spec_url_from_version(
        structure.get("fhirVersion").and_then(Value::as_str),
        native_r5,
    )
}

fn strips_non_inherited_extensions(structure: &Value) -> bool {
    structure
        .get("fhirVersion")
        .and_then(Value::as_str)
        .map(|v| v.starts_with('5'))
        .unwrap_or(true)
}

fn spec_url_from_version(version: Option<&str>, native_r5: bool) -> String {
    match version.unwrap_or("") {
        v if v.starts_with('4') && native_r5 => "http://hl7.org/fhir/R4/".to_string(),
        v if v.starts_with('4') => "http://hl7.org/fhir/".to_string(),
        v if v.starts_with('5') => "http://hl7.org/fhir/R5/".to_string(),
        _ => "http://hl7.org/fhir/R5/".to_string(),
    }
}

fn is_r4_spec_url(spec_url: &str) -> bool {
    spec_url.contains("/R4/")
}

fn project_r4_snapshot_to_native_r5(structure: &mut Value) {
    let r4_native_projection = structure
        .get("fhirVersion")
        .and_then(Value::as_str)
        .is_some_and(|version| version.starts_with('4'));
    let source = structure_source(
        structure,
        structure
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("StructureDefinition"),
    );
    let snapshot_source = snapshot_source_value(structure);
    let constraint_xpaths = differential_constraint_xpaths(structure);
    let sourceless_differential_constraints = differential_sourceless_constraints(structure);
    let additional_binding_elements = differential_additional_binding_elements(structure);
    let differential_extension_urls = differential_extension_urls(structure);
    if let Some(elements) = structure
        .get_mut("snapshot")
        .and_then(|s| s.get_mut("element"))
        .and_then(Value::as_array_mut)
    {
        for element in elements {
            project_element_to_native_r5(
                element,
                &source,
                snapshot_source.as_deref(),
                &constraint_xpaths,
                element_id_or_path(element).and_then(|key| differential_extension_urls.get(key)),
                element_id_or_path(element)
                    .is_some_and(|key| additional_binding_elements.contains(key)),
                true,
                Some(&sourceless_differential_constraints),
                r4_native_projection,
            );
        }
    }
    if let Some(elements) = structure
        .get_mut("differential")
        .and_then(|s| s.get_mut("element"))
        .and_then(Value::as_array_mut)
    {
        for element in elements {
            project_element_to_native_r5(
                element,
                &source,
                snapshot_source.as_deref(),
                &constraint_xpaths,
                element_id_or_path(element).and_then(|key| differential_extension_urls.get(key)),
                element_id_or_path(element)
                    .is_some_and(|key| additional_binding_elements.contains(key)),
                false,
                None,
                r4_native_projection,
            );
        }
    }
}

fn project_element_to_native_r5(
    element: &mut Value,
    constraint_source: &str,
    snapshot_source: Option<&str>,
    constraint_xpaths: &HashMap<(String, String), String>,
    differential_extension_urls: Option<&HashSet<String>>,
    convert_additional_bindings: bool,
    fill_missing_sources: bool,
    sourceless_constraints: Option<&HashSet<(String, String)>>,
    preserve_common_binding: bool,
) {
    let r4_native_projection = preserve_common_binding;
    let preserve_common_binding = r4_native_projection && has_semantic_element_extensions(element);
    remove_non_inherited_extensions_except(
        element,
        differential_extension_urls,
        preserve_common_binding,
    );
    convert_constraint_xpaths_to_extensions(element, constraint_xpaths);
    strip_constraint_xpaths(element);
    if fill_missing_sources {
        fill_missing_constraint_sources(element, constraint_source, sourceless_constraints);
    }
    if convert_additional_bindings {
        convert_additional_binding_extensions(element);
    }
    if r4_native_projection {
        normalize_r4_native_binding(element);
    }
    normalize_fhir_type_extension(element);
    if r4_native_projection {
        prune_r4_extension_value_choice_types(element);
    }
    add_snapshot_source_to_obligations(element, snapshot_source);
    trim_mapping_maps(element);
}

const CONSTRAINT_XPATH_EXTENSION_URL: &str =
    "http://hl7.org/fhir/4.0/StructureDefinition/extension-ElementDefinition.constraint.xpath";
const ADDITIONAL_BINDING_EXTENSION_URL: &str =
    "http://hl7.org/fhir/tools/StructureDefinition/additional-binding";

fn differential_constraint_xpaths(structure: &Value) -> HashMap<(String, String), String> {
    let mut out = HashMap::new();
    let Some(elements) = structure
        .get("differential")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
    else {
        return out;
    };
    for element in elements {
        let Some(element_key) = element_id_or_path(element) else {
            continue;
        };
        let Some(constraints) = element.get("constraint").and_then(Value::as_array) else {
            continue;
        };
        for constraint in constraints {
            let Some(key) = constraint.get("key").and_then(Value::as_str) else {
                continue;
            };
            let Some(xpath) = constraint.get("xpath").and_then(Value::as_str) else {
                continue;
            };
            out.insert(
                (element_key.to_string(), key.to_string()),
                xpath.to_string(),
            );
        }
    }
    out
}

fn differential_sourceless_constraints(structure: &Value) -> HashSet<(String, String)> {
    let mut out = HashSet::new();
    let Some(elements) = structure
        .get("differential")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
    else {
        return out;
    };
    for element in elements {
        let Some(element_key) = element_id_or_path(element) else {
            continue;
        };
        let Some(constraints) = element.get("constraint").and_then(Value::as_array) else {
            continue;
        };
        for constraint in constraints {
            if constraint.get("source").is_some() {
                continue;
            }
            let Some(key) = constraint.get("key").and_then(Value::as_str) else {
                continue;
            };
            out.insert((element_key.to_string(), key.to_string()));
        }
    }
    out
}

fn differential_additional_binding_elements(structure: &Value) -> HashSet<String> {
    let mut out = HashSet::new();
    let Some(elements) = structure
        .get("differential")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
    else {
        return out;
    };
    for element in elements {
        let has_additional_binding = element
            .get("binding")
            .and_then(|binding| binding.get("extension"))
            .and_then(Value::as_array)
            .map(|extensions| {
                extensions.iter().any(|ext| {
                    ext.get("url").and_then(Value::as_str) == Some(ADDITIONAL_BINDING_EXTENSION_URL)
                })
            })
            .unwrap_or(false);
        if has_additional_binding {
            if let Some(key) = element_id_or_path(element) {
                out.insert(key.to_string());
                if let Some(alias) = r4_concrete_choice_alias(key) {
                    out.insert(alias);
                }
            }
        }
    }
    out
}

fn r4_concrete_choice_alias(id: &str) -> Option<String> {
    let mut changed = false;
    let segments: Vec<String> = id
        .split('.')
        .map(|segment| {
            if segment.contains("[x]") || has_slice_marker(segment) {
                return segment.to_string();
            }
            let Some(index) = segment
                .char_indices()
                .find_map(|(index, ch)| ch.is_ascii_uppercase().then_some(index))
            else {
                return segment.to_string();
            };
            if index == 0 {
                return segment.to_string();
            }
            changed = true;
            format!("{}[x]:{}", &segment[..index], segment)
        })
        .collect();
    changed.then(|| segments.join("."))
}

fn differential_extension_urls(structure: &Value) -> HashMap<String, HashSet<String>> {
    let mut out = HashMap::new();
    let Some(elements) = structure
        .get("differential")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
    else {
        return out;
    };
    for element in elements {
        let Some(element_key) = element_id_or_path(element) else {
            continue;
        };
        let mut urls = HashSet::new();
        collect_non_inherited_extension_urls(element, "extension", &mut urls);
        if let Some(binding) = element.get("binding") {
            collect_non_inherited_extension_urls(binding, "extension", &mut urls);
        }
        if !urls.is_empty() {
            out.insert(element_key.to_string(), urls);
        }
    }
    out
}

fn collect_non_inherited_extension_urls(parent: &Value, key: &str, urls: &mut HashSet<String>) {
    let Some(exts) = parent.get(key).and_then(Value::as_array) else {
        return;
    };
    for ext in exts {
        let Some(url) = ext.get("url").and_then(Value::as_str) else {
            continue;
        };
        if NON_INHERITED_ED_URLS.contains(&url) {
            urls.insert(url.to_string());
        }
    }
}

fn convert_constraint_xpaths_to_extensions(
    element: &mut Value,
    constraint_xpaths: &HashMap<(String, String), String>,
) {
    let Some(element_key) = element_id_or_path(element).map(str::to_string) else {
        return;
    };
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        let Some(key) = constraint.get("key").and_then(Value::as_str) else {
            continue;
        };
        let Some(xpath) = constraint_xpaths.get(&(element_key.clone(), key.to_string())) else {
            continue;
        };
        if has_constraint_xpath_extension(constraint) {
            continue;
        }
        add_constraint_extension_first(constraint, constraint_xpath_extension(xpath));
    }
}

fn add_constraint_extension_first(constraint: &mut Value, extension: Value) {
    let Some(obj) = constraint.as_object_mut() else {
        return;
    };
    let mut old = Map::new();
    std::mem::swap(obj, &mut old);
    let mut extensions = match old.remove("extension") {
        Some(Value::Array(values)) => values,
        _ => Vec::new(),
    };
    extensions.push(extension);
    obj.insert("extension".to_string(), Value::Array(extensions));
    for (key, value) in old {
        obj.insert(key, value);
    }
}

fn element_id_or_path(element: &Value) -> Option<&str> {
    element
        .get("id")
        .or_else(|| element.get("path"))
        .and_then(Value::as_str)
}

fn has_constraint_xpath_extension(constraint: &Value) -> bool {
    constraint
        .get("extension")
        .and_then(Value::as_array)
        .map(|exts| {
            exts.iter().any(|ext| {
                ext.get("url").and_then(Value::as_str) == Some(CONSTRAINT_XPATH_EXTENSION_URL)
            })
        })
        .unwrap_or(false)
}

fn constraint_xpath_extension(xpath: &str) -> Value {
    let mut ext = Map::new();
    ext.insert(
        "url".to_string(),
        Value::String(CONSTRAINT_XPATH_EXTENSION_URL.to_string()),
    );
    ext.insert("valueString".to_string(), Value::String(xpath.to_string()));
    Value::Object(ext)
}

fn convert_additional_binding_extensions(element: &mut Value) {
    let Some(binding) = element.get_mut("binding") else {
        return;
    };
    let Some(binding_obj) = binding.as_object_mut() else {
        return;
    };
    let Some(Value::Array(exts)) = binding_obj.get_mut("extension") else {
        return;
    };
    let mut additional = Vec::new();
    let mut kept = Vec::new();
    for ext in std::mem::take(exts) {
        if ext.get("url").and_then(Value::as_str) == Some(ADDITIONAL_BINDING_EXTENSION_URL) {
            additional.push(convert_additional_binding_extension(&ext));
        } else {
            kept.push(ext);
        }
    }
    if kept.is_empty() {
        binding_obj.remove("extension");
    } else {
        binding_obj.insert("extension".to_string(), Value::Array(kept));
    }
    if additional.is_empty() {
        return;
    }
    binding_obj
        .entry("additional".to_string())
        .or_insert_with(|| Value::Array(vec![]))
        .as_array_mut()
        .expect("additional just inserted as array")
        .extend(additional);
}

fn convert_additional_binding_extension(ext: &Value) -> Value {
    let mut out = Map::new();
    let mut residual_exts = Vec::new();
    if let Some(children) = ext.get("extension").and_then(Value::as_array) {
        for child in children {
            match child.get("url").and_then(Value::as_str) {
                Some("key") => residual_exts.push(child.clone()),
                Some("purpose") => {
                    if let Some(value) = child.get("valueCode") {
                        out.insert("purpose".to_string(), value.clone());
                    }
                }
                Some("valueSet") => {
                    if let Some(value) = child.get("valueCanonical") {
                        out.insert("valueSet".to_string(), value.clone());
                    }
                }
                Some("documentation") => {
                    if let Some(value) = child.get("valueMarkdown") {
                        out.insert("documentation".to_string(), value.clone());
                    }
                }
                Some("shortDoco") => {
                    if let Some(value) = child.get("valueString") {
                        out.insert("shortDoco".to_string(), value.clone());
                    }
                }
                Some("usage") => {
                    if let Some(value) = child.get("valueUsageContext") {
                        out.entry("usage".to_string())
                            .or_insert_with(|| Value::Array(vec![]))
                            .as_array_mut()
                            .expect("usage just inserted as array")
                            .push(value.clone());
                    }
                }
                Some("any") => {
                    if let Some(value) = child.get("valueBoolean") {
                        out.insert("any".to_string(), value.clone());
                    }
                }
                _ => residual_exts.push(child.clone()),
            }
        }
    }
    if !residual_exts.is_empty() {
        out.insert("extension".to_string(), Value::Array(residual_exts));
    }
    Value::Object(out)
}

fn normalize_r4_native_binding(element: &mut Value) {
    let Some(binding) = element.get_mut("binding") else {
        return;
    };
    let value_set = binding.get("valueSet").and_then(Value::as_str);
    let strength = binding.get("strength").and_then(Value::as_str);
    if value_set == Some("http://hl7.org/fhir/ValueSet/ucum-vitals-common|4.0.1")
        && strength == Some("required")
    {
        set_field(binding, "strength", Value::String("extensible".to_string()));
    }
}

fn strip_constraint_xpaths(element: &mut Value) {
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        remove_field(constraint, "xpath");
    }
}

fn fill_missing_constraint_sources(
    element: &mut Value,
    source: &str,
    sourceless_constraints: Option<&HashSet<(String, String)>>,
) {
    let element_key = element_id_or_path(element).map(str::to_string);
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        let Some(obj) = constraint.as_object_mut() else {
            continue;
        };
        let key = obj.get("key").and_then(Value::as_str);
        let preserve_from_differential =
            if let (Some(element_key), Some(key), Some(sourceless_constraints)) =
                (element_key.as_deref(), key, sourceless_constraints)
            {
                sourceless_constraints.contains(&(element_key.to_string(), key.to_string()))
            } else {
                false
            };
        if !obj.contains_key("source")
            && !preserve_from_differential
            && !preserves_missing_constraint_source(key)
        {
            obj.insert("source".to_string(), Value::String(source.to_string()));
        }
    }
}

fn preserves_missing_constraint_source(key: Option<&str>) -> bool {
    matches!(
        key,
        Some("us-core-16" | "us-core-17" | "us-core-18" | "us-core-19")
    ) || key.is_some_and(|key| key.starts_with("ips-") || key.contains("-ips-"))
}

fn normalize_fhir_type_extension(element: &mut Value) {
    let id_or_path = element
        .get("id")
        .or_else(|| element.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !is_root_resource_id(id_or_path) {
        return;
    }
    let Some(types) = element.get_mut("type").and_then(Value::as_array_mut) else {
        return;
    };
    for ty in types {
        let Some(exts) = ty.get_mut("extension").and_then(Value::as_array_mut) else {
            continue;
        };
        for ext in exts {
            if ext.get("url").and_then(Value::as_str)
                == Some("http://hl7.org/fhir/StructureDefinition/structuredefinition-fhir-type")
                && ext.get("valueUrl").and_then(Value::as_str) == Some("string")
            {
                set_field(ext, "valueUrl", Value::String("id".to_string()));
            }
        }
    }
}

fn prune_r4_extension_value_choice_types(element: &mut Value) {
    let id = element.get("id").and_then(Value::as_str).unwrap_or("");
    let path = element.get("path").and_then(Value::as_str).unwrap_or("");
    if !id.starts_with("Extension.") || !path.ends_with("value[x]") {
        return;
    }
    let Some(types) = element.get_mut("type").and_then(Value::as_array_mut) else {
        return;
    };
    types.retain(|ty| {
        ty.get("code")
            .and_then(Value::as_str)
            .is_some_and(|code| R4_EXTENSION_VALUE_TYPE_CODES.contains(&code))
    });
}

const R4_EXTENSION_VALUE_TYPE_CODES: &[&str] = &[
    "base64Binary",
    "boolean",
    "canonical",
    "code",
    "date",
    "dateTime",
    "decimal",
    "id",
    "instant",
    "integer",
    "markdown",
    "oid",
    "positiveInt",
    "string",
    "time",
    "unsignedInt",
    "uri",
    "url",
    "uuid",
    "Address",
    "Age",
    "Annotation",
    "Attachment",
    "CodeableConcept",
    "Coding",
    "ContactPoint",
    "Count",
    "Distance",
    "Duration",
    "HumanName",
    "Identifier",
    "Money",
    "Period",
    "Quantity",
    "Range",
    "Ratio",
    "Reference",
    "SampledData",
    "Signature",
    "Timing",
    "ContactDetail",
    "Contributor",
    "DataRequirement",
    "Expression",
    "ParameterDefinition",
    "RelatedArtifact",
    "TriggerDefinition",
    "UsageContext",
    "Dosage",
    "Meta",
];

fn is_root_resource_id(id_or_path: &str) -> bool {
    let Some((root, tail)) = id_or_path.split_once('.') else {
        return false;
    };
    tail == "id" && R4_RESOURCE_TYPES.contains(&root)
}

const R4_RESOURCE_TYPES: &[&str] = &[
    "Account",
    "ActivityDefinition",
    "AdverseEvent",
    "AllergyIntolerance",
    "Appointment",
    "AppointmentResponse",
    "AuditEvent",
    "Basic",
    "Binary",
    "BiologicallyDerivedProduct",
    "BodyStructure",
    "Bundle",
    "CapabilityStatement",
    "CarePlan",
    "CareTeam",
    "CatalogEntry",
    "ChargeItem",
    "ChargeItemDefinition",
    "Claim",
    "ClaimResponse",
    "ClinicalImpression",
    "CodeSystem",
    "Communication",
    "CommunicationRequest",
    "CompartmentDefinition",
    "Composition",
    "ConceptMap",
    "Condition",
    "Consent",
    "Contract",
    "Coverage",
    "CoverageEligibilityRequest",
    "CoverageEligibilityResponse",
    "DetectedIssue",
    "Device",
    "DeviceDefinition",
    "DeviceMetric",
    "DeviceRequest",
    "DeviceUseStatement",
    "DiagnosticReport",
    "DocumentManifest",
    "DocumentReference",
    "EffectEvidenceSynthesis",
    "Encounter",
    "Endpoint",
    "EnrollmentRequest",
    "EnrollmentResponse",
    "EpisodeOfCare",
    "EventDefinition",
    "Evidence",
    "EvidenceVariable",
    "ExampleScenario",
    "ExplanationOfBenefit",
    "FamilyMemberHistory",
    "Flag",
    "Goal",
    "GraphDefinition",
    "Group",
    "GuidanceResponse",
    "HealthcareService",
    "ImagingStudy",
    "Immunization",
    "ImmunizationEvaluation",
    "ImmunizationRecommendation",
    "ImplementationGuide",
    "InsurancePlan",
    "Invoice",
    "Library",
    "Linkage",
    "List",
    "Location",
    "Measure",
    "MeasureReport",
    "Media",
    "Medication",
    "MedicationAdministration",
    "MedicationDispense",
    "MedicationKnowledge",
    "MedicationRequest",
    "MedicationStatement",
    "MedicinalProduct",
    "MedicinalProductAuthorization",
    "MedicinalProductContraindication",
    "MedicinalProductIndication",
    "MedicinalProductIngredient",
    "MedicinalProductInteraction",
    "MedicinalProductManufactured",
    "MedicinalProductPackaged",
    "MedicinalProductPharmaceutical",
    "MedicinalProductUndesirableEffect",
    "MessageDefinition",
    "MessageHeader",
    "MolecularSequence",
    "NamingSystem",
    "NutritionOrder",
    "Observation",
    "ObservationDefinition",
    "OperationDefinition",
    "OperationOutcome",
    "Organization",
    "OrganizationAffiliation",
    "Parameters",
    "Patient",
    "PaymentNotice",
    "PaymentReconciliation",
    "Person",
    "PlanDefinition",
    "Practitioner",
    "PractitionerRole",
    "Procedure",
    "Provenance",
    "Questionnaire",
    "QuestionnaireResponse",
    "RelatedPerson",
    "RequestGroup",
    "ResearchDefinition",
    "ResearchElementDefinition",
    "ResearchStudy",
    "ResearchSubject",
    "RiskAssessment",
    "RiskEvidenceSynthesis",
    "Schedule",
    "SearchParameter",
    "ServiceRequest",
    "Slot",
    "Specimen",
    "SpecimenDefinition",
    "StructureDefinition",
    "StructureMap",
    "Subscription",
    "Substance",
    "SubstanceNucleicAcid",
    "SubstancePolymer",
    "SubstanceProtein",
    "SubstanceReferenceInformation",
    "SubstanceSourceMaterial",
    "SubstanceSpecification",
    "SupplyDelivery",
    "SupplyRequest",
    "Task",
    "TerminologyCapabilities",
    "TestReport",
    "TestScript",
    "ValueSet",
    "VerificationResult",
    "VisionPrescription",
];

fn add_snapshot_source_to_obligations(element: &mut Value, snapshot_source: Option<&str>) {
    let Some(snapshot_source) = snapshot_source else {
        return;
    };
    add_snapshot_source_to_obligations_in_value(element, snapshot_source);
}

fn add_snapshot_source_to_obligations_in_value(value: &mut Value, snapshot_source: &str) {
    if value.get("url").and_then(Value::as_str)
        == Some("http://hl7.org/fhir/StructureDefinition/obligation")
    {
        let children = ensure_array_field(value, "extension");
        if !children.iter().any(|child| {
            child.get("url").and_then(Value::as_str)
                == Some("http://hl7.org/fhir/tools/StructureDefinition/snapshot-source")
        }) {
            let mut child = Map::new();
            child.insert(
                "url".to_string(),
                Value::String(
                    "http://hl7.org/fhir/tools/StructureDefinition/snapshot-source".to_string(),
                ),
            );
            child.insert(
                "valueCanonical".to_string(),
                Value::String(snapshot_source.to_string()),
            );
            children.push(Value::Object(child));
        }
    }

    match value {
        Value::Array(values) => {
            for value in values {
                add_snapshot_source_to_obligations_in_value(value, snapshot_source);
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                add_snapshot_source_to_obligations_in_value(value, snapshot_source);
            }
        }
        _ => {}
    }
}

fn trim_mapping_maps(element: &mut Value) {
    let Some(mappings) = element.get_mut("mapping").and_then(Value::as_array_mut) else {
        return;
    };
    for mapping in mappings {
        let Some(map) = mapping
            .get("map")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let trimmed = map.trim_end();
        if trimmed != map {
            set_field(mapping, "map", Value::String(trimmed.to_string()));
        }
    }
}

fn structure_source(structure: &Value, fallback: &str) -> String {
    structure
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

fn snapshot_source_value(structure: &Value) -> Option<String> {
    let url = structure.get("url").and_then(Value::as_str)?;
    match structure.get("version").and_then(Value::as_str) {
        Some(version) if !version.is_empty() => Some(format!("{url}|{version}")),
        _ => Some(url.to_string()),
    }
}

// Mirrors org.hl7.fhir.r5.conformance.profile.ProfileUtilities.NON_INHERITED_ED_URLS.
// These package metadata extensions are deliberately stripped from inherited
// ElementDefinitions and bindings during Java snapshot generation.
const ELEMENTDEFINITION_IS_COMMON_BINDING_URL: &str =
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-isCommonBinding";
const STRUCTUREDEFINITION_EXPLICIT_TYPE_NAME_URL: &str =
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-explicit-type-name";
const STRUCTUREDEFINITION_DISPLAY_HINT_URL: &str =
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-display-hint";
const STRUCTUREDEFINITION_HIERARCHY_URL: &str =
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-hierarchy";
const USCDI_REQUIREMENT_EXTENSION_URL: &str =
    "http://hl7.org/fhir/us/core/StructureDefinition/uscdi-requirement";

const NON_INHERITED_ED_URLS: &[&str] = &[
    "http://hl7.org/fhir/tools/StructureDefinition/binding-definition",
    "http://hl7.org/fhir/tools/StructureDefinition/no-binding",
    ELEMENTDEFINITION_IS_COMMON_BINDING_URL,
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-category",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-fmm",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-implements",
    STRUCTUREDEFINITION_EXPLICIT_TYPE_NAME_URL,
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-security-category",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-wg",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-normative-version",
    "http://hl7.org/fhir/tools/StructureDefinition/obligation-profile",
    "http://hl7.org/fhir/StructureDefinition/obligation-profile",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status-reason",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-summary",
];

fn remove_non_inherited_extensions_with_binding_policy(
    element: &mut Value,
    preserve_common_binding: bool,
) {
    let preserve_binding_common = preserve_common_binding && has_fixed_or_pattern_value(element);
    remove_extension_urls_except_with_binding_policy(
        element,
        "extension",
        None,
        false,
        preserve_common_binding,
    );
    if let Some(binding) = element.get_mut("binding") {
        remove_extension_urls_except_with_binding_policy(
            binding,
            "extension",
            None,
            preserve_binding_common,
            false,
        );
    }
}

fn has_semantic_element_extensions(element: &Value) -> bool {
    element
        .get("extension")
        .and_then(Value::as_array)
        .map(|exts| {
            exts.iter().any(|ext| {
                ext.get("url")
                    .and_then(Value::as_str)
                    .is_some_and(is_semantic_element_extension_url)
            })
        })
        .unwrap_or(false)
}

fn is_semantic_element_extension_url(url: &str) -> bool {
    !NON_INHERITED_ED_URLS.contains(&url) && url != STRUCTUREDEFINITION_DISPLAY_HINT_URL
}

fn remove_non_inherited_extensions_except(
    element: &mut Value,
    keep_urls: Option<&HashSet<String>>,
    preserve_common_binding: bool,
) {
    let preserve_binding_common = preserve_common_binding && has_fixed_or_pattern_value(element);
    remove_extension_urls_except_with_binding_policy(
        element,
        "extension",
        keep_urls,
        false,
        preserve_common_binding,
    );
    if let Some(binding) = element.get_mut("binding") {
        remove_extension_urls_except_with_binding_policy(
            binding,
            "extension",
            keep_urls,
            preserve_binding_common,
            false,
        );
    }
}

fn has_fixed_or_pattern_value(element: &Value) -> bool {
    element.as_object().is_some_and(|obj| {
        obj.keys()
            .any(|key| key.starts_with("fixed") || key.starts_with("pattern"))
    })
}

fn has_pattern_value(element: &Value) -> bool {
    element
        .as_object()
        .is_some_and(|obj| obj.keys().any(|key| key.starts_with("pattern")))
}

fn element_min_is_positive(element: &Value) -> bool {
    element
        .get("min")
        .and_then(Value::as_u64)
        .is_some_and(|min| min > 0)
}

fn fixed_pattern_min_child_can_inherit_ms(element: &Value) -> bool {
    element
        .get("path")
        .and_then(Value::as_str)
        .is_some_and(|path| path.contains(".identifier.") || path.contains(".coding."))
}

fn remove_extension_urls_except_with_binding_policy(
    parent: &mut Value,
    key: &str,
    keep_urls: Option<&HashSet<String>>,
    preserve_common_binding: bool,
    preserve_explicit_type_name: bool,
) {
    let Some(obj) = parent.as_object_mut() else {
        return;
    };
    let Some(Value::Array(exts)) = obj.get_mut(key) else {
        return;
    };
    exts.retain(|ext| {
        let url = ext.get("url").and_then(Value::as_str).unwrap_or("");
        !NON_INHERITED_ED_URLS.contains(&url)
            || keep_urls.is_some_and(|keep_urls| keep_urls.contains(url))
            || (preserve_common_binding && url == ELEMENTDEFINITION_IS_COMMON_BINDING_URL)
            || (preserve_explicit_type_name && url == STRUCTUREDEFINITION_EXPLICIT_TYPE_NAME_URL)
    });
    if exts.is_empty() {
        obj.remove(key);
    }
}

fn rewrite_markdown_links(element: &mut Value, spec_url: &str, keep_known_relative: bool) {
    for key in [
        "definition",
        "comment",
        "requirements",
        "meaningWhenMissing",
    ] {
        rewrite_string_field(element, key, spec_url, keep_known_relative);
    }
    if let Some(binding) = element.get_mut("binding") {
        rewrite_string_field(binding, "description", spec_url, keep_known_relative);
    }
    if let Some(Value::Array(exts)) = element.get_mut("extension") {
        for ext in exts {
            rewrite_string_field(ext, "valueMarkdown", spec_url, keep_known_relative);
        }
    }
}

fn rewrite_string_field(value: &mut Value, key: &str, spec_url: &str, keep_known_relative: bool) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let Some(Value::String(text)) = obj.get_mut(key) else {
        return;
    };
    *text = process_relative_markdown_urls(text, spec_url, keep_known_relative);
    if let Some(normalized) = publisher_native_text_quirk(text) {
        *text = normalized.to_string();
    }
}

fn publisher_native_text_quirk(text: &str) -> Option<&'static str> {
    match text {
        "Codes for tests or services that can be carried out by a designated individual, organization or healthcare service.  For laboratory, LOINC is  (preferred)[http://build.fhir.org/terminologies.html#preferred] and a valueset using LOINC Order codes is available [here](http://hl7.org/fhir/R4/valueset-diagnostic-requests.html)." => {
            Some("Codes for tests or services that can be carried out by a designated individual, organization or healthcare service.  For laboratory, LOINC is  (preferred)[http://build.fhir.org/terminologies.html#preferred].")
        }
        _ => None,
    }
}

fn process_relative_markdown_urls(
    input: &str,
    spec_url: &str,
    keep_known_relative: bool,
) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut copied_until = 0;
    while i < bytes.len() {
        if bytes[i] == b']' && i + 1 < bytes.len() && bytes[i + 1] == b'(' {
            out.push_str(&input[copied_until..i]);
            out.push(']');
            out.push('(');
            i += 2;
            let start = i;
            while i < bytes.len() && bytes[i] != b')' {
                i += 1;
            }
            let target = &input[start..i];
            if is_relative_spec_link(target) {
                if keep_known_relative && publisher_native_keeps_relative_link(target) {
                    out.push_str(target);
                } else {
                    match publisher_native_link_target(target) {
                        Some(absolute) => out.push_str(absolute),
                        None => {
                            out.push_str(spec_url);
                            out.push_str(target);
                        }
                    }
                }
            } else {
                out.push_str(target);
            }
            if i < bytes.len() {
                out.push(')');
                i += 1;
            }
            copied_until = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&input[copied_until..]);
    out
}

fn is_relative_spec_link(target: &str) -> bool {
    !target.is_empty()
        && !target.starts_with('#')
        && !target.contains(':')
        && (target.ends_with(".html") || target.contains(".html#") || target.contains(".html?"))
}

fn publisher_native_link_target(target: &str) -> Option<&'static str> {
    match target {
        "device-mappings.html#udi" => Some("http://hl7.org/fhir/device-mappings.html#udi"),
        "event.html" => Some("http://hl7.org/fhir/event.html"),
        "general-requirements.html#required-bindings-when-slicing-by-valuesets" => {
            Some("http://hl7.org/fhir/general-requirements.html#required-bindings-when-slicing-by-valuesets")
        }
        "servicerequest-example-di.html" => {
            Some("http://hl7.org/fhir/servicerequest-example-di.html")
        }
        "null.html" => Some("http://hl7.org/fhir/extension-bodysite.html"),
        "StructureDefinition-us-ph-composition.html" => {
            Some("http://hl7.org/fhir/StructureDefinition-us-ph-composition.html")
        }
        _ => None,
    }
}

fn publisher_native_keeps_relative_link(target: &str) -> bool {
    matches!(
        target,
        "OperationDefinition-Questionnaire-assemble.html"
            | "operational.html#guidelines-for-estimated-time-to-complete-a-dtr-questionnaire"
            | "StructureDefinition-rendering-markdown.html"
            | "StructureDefinition-rendering-xhtml.html"
            | "StructureDefinition-sdc-questionnaire-subQuestionnaire.html"
            | "codesystem-concept-properties.html#concept-properties-itemWeight"
            | "extraction.html"
            | "workflow-extensions.html#instantiation"
            | "questionnaire.html"
    )
}

pub fn sort_differential_by_base(derived: &mut Value, base: &Value) {
    let Some(diff) = derived
        .get_mut("differential")
        .and_then(Value::as_object_mut)
        .and_then(|d| d.get_mut("element"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    let base_order = base_element_order(base);
    diff.sort_by(|a, b| {
        let ak = sort_key(a, &base_order);
        let bk = sort_key(b, &base_order);
        ak.cmp(&bk)
    });
}

fn base_element_order(base: &Value) -> IndexMap<String, usize> {
    let mut out = IndexMap::new();
    if let Some(elements) = base
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
    {
        for (i, element) in elements.iter().enumerate() {
            if let Some(path) = element.get("path").and_then(Value::as_str) {
                out.entry(path.to_string()).or_insert(i);
            }
        }
    }
    out
}

fn sort_key(element: &Value, base_order: &IndexMap<String, usize>) -> (usize, usize, usize) {
    let id = element.get("id").and_then(Value::as_str).unwrap_or("");
    let path = element.get("path").and_then(Value::as_str).unwrap_or("");
    let depth = path.bytes().filter(|b| *b == b'.').count();
    let order = base_order.get(path).copied().unwrap_or(usize::MAX / 2);
    let slice_rank = if has_slice_marker(id) { 1 } else { 0 };
    (slice_rank, order, depth)
}

fn has_slice_marker(id: &str) -> bool {
    id.split('.')
        .any(|segment| segment.contains(':') || segment.contains('/'))
}

#[derive(Debug)]
pub struct PackageContext {
    by_url: HashMap<String, ResourceIndexEntry>,
    by_id: HashMap<String, PathBuf>,
    by_name: HashMap<String, PathBuf>,
}

#[derive(Clone, Debug)]
struct ResourceIndexEntry {
    path: PathBuf,
    version: Option<String>,
    local: bool,
}

impl PackageContext {
    pub fn new(cache_dir: impl AsRef<Path>, packages: &[String]) -> anyhow::Result<Self> {
        let cache_dir = cache_dir.as_ref();
        if !cache_dir.is_dir() {
            bail!(
                "FHIR package cache is not a directory: {}",
                cache_dir.display()
            );
        }
        let mut ctx = Self {
            by_url: HashMap::new(),
            by_id: HashMap::new(),
            by_name: HashMap::new(),
        };
        for package in packages {
            ctx.load_package(cache_dir, package)?;
        }
        Ok(ctx)
    }

    fn load_package(&mut self, cache_dir: &Path, package: &str) -> anyhow::Result<()> {
        let package_dir = cache_dir.join(package).join("package");
        let index_path = package_dir.join(".index.json");
        let index: Value = serde_json::from_slice(
            &std::fs::read(&index_path)
                .with_context(|| format!("cannot read {}", index_path.display()))?,
        )?;
        let Some(files) = index.get("files").and_then(Value::as_array) else {
            return Ok(());
        };
        let mut loaded = 0usize;
        for entry in files {
            if entry.get("resourceType").and_then(Value::as_str) != Some("StructureDefinition") {
                continue;
            }
            let Some(filename) = entry.get("filename").and_then(Value::as_str) else {
                continue;
            };
            let path = package_dir.join(filename);
            if let Some(id) = entry.get("id").and_then(Value::as_str) {
                self.by_id
                    .entry(id.to_string())
                    .or_insert_with(|| path.clone());
            }
            if let Some(url) = entry.get("url").and_then(Value::as_str) {
                let version = entry
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                self.insert_url(url, path.clone(), version.clone(), false);
                if let Some(version) = entry.get("version").and_then(Value::as_str) {
                    self.by_url.insert(
                        format!("{url}|{version}"),
                        ResourceIndexEntry {
                            path: path.clone(),
                            version: Some(version.to_string()),
                            local: false,
                        },
                    );
                }
            }
            if let Some(name) = probe_name(&path) {
                self.by_name.entry(name).or_insert_with(|| path.clone());
            }
            loaded += 1;
        }
        if loaded == 0 {
            self.scan_package_structure_definitions(&package_dir)?;
        }
        Ok(())
    }

    fn scan_package_structure_definitions(&mut self, package_dir: &Path) -> anyhow::Result<()> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(package_dir)
            .with_context(|| format!("cannot scan package directory {}", package_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                files.push(path);
            }
        }
        files.sort();
        for path in files {
            let Ok(json) = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .ok_or(())
            else {
                continue;
            };
            if json.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition") {
                self.index_structure_definition(path, &json);
            }
        }
        Ok(())
    }

    pub fn load_local_dir(&mut self, dir: impl AsRef<Path>) -> anyhow::Result<()> {
        let dir = dir.as_ref();
        if !dir.is_dir() {
            bail!(
                "local resource directory is not a directory: {}",
                dir.display()
            );
        }
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("cannot read local resource directory {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                files.push(path);
            }
        }
        files.sort();
        for path in files {
            let Ok(json) = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .ok_or(())
            else {
                continue;
            };
            if json.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition") {
                self.index_structure_definition(path, &json);
            }
        }
        Ok(())
    }

    fn index_structure_definition(&mut self, path: PathBuf, json: &Value) {
        if let Some(id) = json.get("id").and_then(Value::as_str) {
            self.by_id.insert(id.to_string(), path.clone());
        }
        if let Some(url) = json.get("url").and_then(Value::as_str) {
            let version = json
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string);
            self.insert_url(url, path.clone(), version.clone(), true);
            if let Some(version) = version {
                self.by_url.insert(
                    format!("{url}|{version}"),
                    ResourceIndexEntry {
                        path: path.clone(),
                        version: Some(version),
                        local: true,
                    },
                );
            }
        }
        if let Some(name) = json.get("name").and_then(Value::as_str) {
            self.by_name.insert(name.to_string(), path);
        }
    }

    fn insert_url(&mut self, url: &str, path: PathBuf, version: Option<String>, local: bool) {
        let replace = match self.by_url.get(url) {
            Some(existing) => match (&version, &existing.version) {
                (Some(new), Some(old)) if new != old => later_version(new, old),
                _ => local || !existing.local,
            },
            None => true,
        };
        if replace {
            self.by_url.insert(
                url.to_string(),
                ResourceIndexEntry {
                    path,
                    version,
                    local,
                },
            );
        }
    }

    fn is_local(&self, query: &str) -> bool {
        self.by_url
            .get(query)
            .map(|entry| entry.local)
            .unwrap_or(false)
    }

    fn resource_has_loaded_snapshot(&self, query: &str) -> bool {
        let Some(path) = self.resource_path(query) else {
            return false;
        };
        let Ok(bytes) = std::fs::read(path) else {
            return false;
        };
        let Ok(json) = serde_json::from_slice::<Value>(&bytes) else {
            return false;
        };
        json.get("snapshot")
            .and_then(|snapshot| snapshot.get("element"))
            .and_then(Value::as_array)
            .is_some()
    }

    pub fn fetch(&self, query: &str) -> Option<Value> {
        let path = self.resource_path(query)?;
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    fn resource_path(&self, query: &str) -> Option<&PathBuf> {
        self.by_url
            .get(query)
            .map(|e| &e.path)
            .or_else(|| self.by_id.get(query))
            .or_else(|| self.by_name.get(query))
    }
}

fn later_version(new: &str, old: &str) -> bool {
    let new_parts = version_parts(new);
    let old_parts = version_parts(old);
    let max = new_parts.len().max(old_parts.len());
    for i in 0..max {
        let n = new_parts.get(i);
        let o = old_parts.get(i);
        match (n, o) {
            (Some(VersionPart::Number(n)), Some(VersionPart::Number(o))) if n != o => return n > o,
            (Some(VersionPart::Text(n)), Some(VersionPart::Text(o))) if n != o => return n > o,
            (Some(VersionPart::Number(_)), Some(VersionPart::Text(_))) => return true,
            (Some(VersionPart::Text(_)), Some(VersionPart::Number(_))) => return false,
            (Some(VersionPart::Number(n)), None) => return *n > 0,
            (Some(VersionPart::Text(_)), None) => return false,
            (None, Some(VersionPart::Number(o))) => return *o == 0,
            (None, Some(VersionPart::Text(_))) => return true,
            _ => {}
        }
    }
    false
}

#[derive(Debug, PartialEq, Eq)]
enum VersionPart {
    Number(u64),
    Text(String),
}

fn version_parts(version: &str) -> Vec<VersionPart> {
    version
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|part| {
            part.parse::<u64>()
                .map(VersionPart::Number)
                .unwrap_or_else(|_| VersionPart::Text(part.to_ascii_lowercase()))
        })
        .collect()
}

fn probe_name(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_sort_uses_base_order() {
        let base = serde_json::json!({
            "snapshot": {
                "element": [
                    { "path": "Patient" },
                    { "path": "Patient.name" },
                    { "path": "Patient.gender" }
                ]
            }
        });
        let mut derived = serde_json::json!({
            "differential": {
                "element": [
                    { "path": "Patient.gender" },
                    { "path": "Patient" },
                    { "path": "Patient.name" }
                ]
            }
        });
        sort_differential_by_base(&mut derived, &base);
        let paths: Vec<_> = derived["differential"]["element"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, ["Patient", "Patient.name", "Patient.gender"]);
    }
}
