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
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            _ if arg.starts_with('-') => bail!("unknown option: {arg}"),
            _ => input = Some(arg),
        }
    }

    let input = input.context("missing input StructureDefinition JSON")?;
    if packages.is_empty() {
        packages.push("hl7.fhir.r5.core#5.0.0".to_string());
    }
    let cache = cache
        .or_else(|| std::env::var("FHIR_CACHE").ok())
        .unwrap_or_else(|| "temp/fhir-home/.fhir/packages".to_string());
    let source = std::fs::read_to_string(&input)?;
    let derived: Value = serde_json::from_str(&source)?;
    let mut ctx = PackageContext::new(&cache, &packages)?;
    for local_dir in local_dirs {
        ctx.load_local_dir(local_dir)?;
    }
    let out = generate_snapshot(
        derived,
        &ctx,
        SnapshotOptions {
            sort_differential,
            native_r5,
            apply_extension_root_doco: true,
        },
    )?;
    print!("{}", json_emit::to_fhir_json_string(&out));
    Ok(())
}

fn print_usage() {
    eprintln!(
        "usage: snapshot_gen [--cache <packages-dir>] [--package <pkg#ver> ...] [--local-dir <dir> ...] [--sort|--no-sort] [--native-r5] <StructureDefinition.json>"
    );
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

    if options.sort_differential {
        sort_differential_by_base(&mut derived, &base);
    }

    let base_elements = base
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .context("base StructureDefinition has no snapshot.element")?;
    let diff_elements = derived
        .get("differential")
        .and_then(|d| d.get("element"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
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
                false,
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
    for diff in diff_elements {
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
                options.native_r5,
                &base_url,
                &base_spec_url,
                &explicit_slicing_paths,
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
                continue;
            }
            apply_extension_profile_root(
                &mut snapshot_elements[index],
                &diff,
                ctx,
                options.native_r5,
                Some(&base_url),
                true,
                true,
            )?;
            apply_type_profile_root(&mut snapshot_elements[index], &diff, ctx, options.native_r5)?;
            merge_diff_into_element(
                &mut snapshot_elements[index],
                &diff,
                base_strip_non_inherited,
                &base_constraint_source,
            )?;
        }
    }

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

fn find_matching_snapshot_index(elements: &[Value], path: &str, diff: &Value) -> Option<usize> {
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
        candidate.get("path").and_then(Value::as_str) == Some(path)
            && candidate.get("sliceName").and_then(Value::as_str) == diff_slice
    })
}

fn insert_slice_element(
    elements: &mut Vec<Value>,
    path: &str,
    diff: &Value,
    ctx: &PackageContext,
    original_elements_by_id: &HashMap<String, Value>,
    strip_non_inherited: bool,
    native_r5: bool,
    host_extension_source: &str,
    base_spec_url: &str,
    explicit_slicing_paths: &HashSet<String>,
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
    let unsliced_anchor_id = if anchor_id.contains(':') || anchor_id.contains('/') {
        unsliced_element_id(&anchor_id).unwrap_or_else(|| anchor_id.clone())
    } else {
        anchor_id.clone()
    };
    // Java only drops the inherited unsliced datatype children when the base
    // element was already sliced (ProfilePathProcessor.processPathWithSlicedBase,
    // e.g. CRD Practitioner.identifier). When this profile newly introduces the
    // slicing on a previously-unsliced datatype element, processSimplePathDefault
    // keeps the unsliced children (e.g. CARIN BB Patient.identifier).
    let base_anchor_was_sliced = original_elements_by_id
        .get(&unsliced_anchor_id)
        .map(|element| element.get("slicing").is_some())
        .unwrap_or(false);
    let mut slice = if anchor_id.contains(':') || anchor_id.contains('/') {
        unsliced_element_id(&anchor_id)
            .and_then(|id| original_elements_by_id.get(&id).cloned())
            .unwrap_or_else(|| elements[anchor].clone())
    } else {
        original_elements_by_id
            .get(&anchor_id)
            .cloned()
            .unwrap_or_else(|| elements[anchor].clone())
    };
    fill_missing_constraint_sources_on_constrained_element(&mut slice, host_extension_source);
    remove_field(&mut slice, "slicing");
    if let Some(id) = diff.get("id") {
        set_field(&mut slice, "id", id.clone());
    }
    if let Some(path) = diff.get("path") {
        set_field(&mut slice, "path", path.clone());
    }
    if first_extension_profile_url(diff).is_some() {
        if let Some(t) = diff.get("type") {
            set_field(&mut slice, "type", t.clone());
        }
        let inherited_slicing =
            elements[anchor].get("slicing").is_some() && !explicit_slicing_paths.contains(path);
        let allow_condition = extension_condition_context_allows(elements, path, inherited_slicing);
        apply_extension_profile_root(
            &mut slice,
            diff,
            ctx,
            native_r5,
            Some(host_extension_source),
            !inherited_slicing,
            allow_condition,
        )?;
    }
    merge_diff_into_element(&mut slice, diff, strip_non_inherited, host_extension_source)?;

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
    if is_plan_definition_recursive_action_anchor(&elements[anchor]) {
        prune_recursive_action_unsliced_tail(elements, &anchor_id);
    }

    if base_anchor_was_sliced
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
        should_materialize_extension_profile_children_on_insert(&slice, &format!("{anchor_id}."));
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

fn should_materialize_extension_profile_children_on_insert(
    slice: &Value,
    anchor_prefix: &str,
) -> bool {
    if first_extension_profile_url(slice).is_none() {
        return false;
    }
    slice
        .get("base")
        .and_then(|b| b.get("path"))
        .and_then(Value::as_str)
        == Some("Element.extension")
        && slice
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.starts_with(anchor_prefix))
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

fn prune_unsliced_descendants(elements: &mut Vec<Value>, anchor_id: &str) {
    let prefix = format!("{anchor_id}.");
    elements.retain(|candidate| {
        let id = candidate.get("id").and_then(Value::as_str).unwrap_or("");
        !id.starts_with(&prefix) || has_slice_marker(id)
    });
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
) -> anyhow::Result<()> {
    let Some(diff_id) = diff.get("id").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(dot) = diff_id.rfind('.') else {
        return Ok(());
    };
    let parent_id = &diff_id[..dot];
    unfold_parent_id(elements, parent_id, ctx, base_url, base_spec_url, native_r5)
}

fn unfold_parent_id(
    elements: &mut Vec<Value>,
    parent_id: &str,
    ctx: &PackageContext,
    base_url: &str,
    base_spec_url: &str,
    native_r5: bool,
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
    if elements.iter().any(|candidate| {
        candidate
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .starts_with(&child_prefix)
    }) {
        return Ok(());
    }

    let Some(parent_path) = elements[parent_index]
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(());
    };

    if unfold_sliced_parent_from_anchor(elements, parent_index, parent_id, &parent_path) {
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
    )? {
        return Ok(());
    }

    let Some(type_entries) = elements[parent_index].get("type").and_then(Value::as_array) else {
        return Ok(());
    };
    let parent_profile_url = first_non_extension_profile_url(&elements[parent_index])
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
        children.push(clone);
    }

    let insert_at = parent_index + 1;
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(insert_at + offset, child);
    }
    Ok(())
}

fn close_type_slicing_for_descendant_unfold(element: &mut Value) {
    let is_type_slicing = element
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

fn unfold_sliced_parent_from_anchor(
    elements: &mut Vec<Value>,
    parent_index: usize,
    parent_id: &str,
    parent_path: &str,
) -> bool {
    let Some(unsliced_id) = unsliced_element_id(parent_id) else {
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
    let mut children = Vec::new();
    for child in elements.iter() {
        let child_id = child.get("id").and_then(Value::as_str).unwrap_or("");
        if !child_id.starts_with(&child_prefix) {
            continue;
        }
        let mut clone = child.clone();
        set_field(
            &mut clone,
            "id",
            Value::String(format!("{parent_id}{}", &child_id[unsliced_id.len()..])),
        );
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

fn unfold_content_reference_parent(
    elements: &mut Vec<Value>,
    parent_index: usize,
    parent_id: &str,
    parent_path: &str,
    base_url: &str,
    base_spec_url: &str,
    ctx: &PackageContext,
    native_r5: bool,
) -> anyhow::Result<bool> {
    let Some(content_reference) = elements[parent_index]
        .get("contentReference")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(false);
    };
    let Some((source_url, target_id)) = split_content_reference(&content_reference, base_url)
    else {
        return Ok(false);
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
        absolutize_content_reference(&mut clone, &source_url);
        children.push(clone);
    }

    let insert_at = parent_index + 1;
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(insert_at + offset, child);
    }
    Ok(true)
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
    if is_core_structure_url(base_url) {
        return;
    }
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

fn is_core_structure_url(url: &str) -> bool {
    url.starts_with("http://hl7.org/fhir/StructureDefinition/")
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
    if content_reference.starts_with('#') {
        set_field(
            element,
            "contentReference",
            Value::String(format!("{source_url}{content_reference}")),
        );
    }
}

fn merge_diff_into_element(
    target: &mut Value,
    diff: &Value,
    strip_non_inherited: bool,
    constraint_source: &str,
) -> anyhow::Result<()> {
    let is_extension_doco = check_extension_doco(target);
    merge_extensions_from_definition(target, diff, strip_non_inherited);
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
    copy_if_present(target, diff, "mustHaveValue");
    copy_if_present(target, diff, "contentReference");
    copy_if_present(target, diff, "slicing");

    copy_choice_prefix(target, diff, "fixed");
    copy_choice_prefix(target, diff, "pattern");
    copy_choice_prefix(target, diff, "minValue");
    copy_choice_prefix(target, diff, "maxValue");

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
        set_field(target, "type", t.clone());
        if target.get("binding").is_some() && !has_bindable_type(target) {
            remove_field(target, "binding");
        }
    }
    normalize_type_slicing(target, diff);

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
            .is_some_and(has_slice_marker)
}

fn remove_obligation_extensions(target: &mut Value) {
    let Some(obj) = target.as_object_mut() else {
        return;
    };
    let Some(Value::Array(exts)) = obj.get_mut("extension") else {
        return;
    };
    exts.retain(|ext| {
        ext.get("url").and_then(Value::as_str)
            != Some("http://hl7.org/fhir/StructureDefinition/obligation")
    });
    if exts.is_empty() {
        obj.remove("extension");
    }
}

fn normalize_type_slicing(element: &mut Value, diff: &Value) {
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
    allow_local_root_constraints: bool,
    allow_local_root_condition: bool,
) -> anyhow::Result<()> {
    let Some(profile_url) =
        first_extension_profile_url(diff).or_else(|| first_extension_profile_url(slice))
    else {
        return Ok(());
    };
    if uses_generic_extension_doco_profile(profile_url) {
        apply_generic_extension_doco(slice);
        return Ok(());
    }
    let Some(profile) = profile_with_snapshot(profile_url, ctx, native_r5)? else {
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
    // Extension-root doco keeps Publisher's known-relative links (e.g.
    // workflow-extensions.html#instantiation) as-is; only freshly applied
    // extension slices reach here. Inherited copies of the same slice go through
    // normalize_inherited_element, which rewrites them to the spec URL.
    rewrite_markdown_links(
        &mut root,
        &spec_url_for_structure(&profile, native_r5),
        true,
    );
    let is_local_profile = ctx.is_local(profile_url);
    let project_local_root_constraints =
        allow_local_root_constraints && projects_local_extension_root_constraints(profile_url);
    if native_r5 && is_local_profile {
        if project_local_root_constraints {
            convert_own_constraint_xpaths_to_extensions(&mut root);
            strip_constraint_xpaths(&mut root);
        }
        if root.get("isSummary").is_none() {
            set_field(&mut root, "isSummary", Value::Bool(false));
        }
    }
    if is_core_extension_profile(profile_url) && root.get("isSummary").is_none() {
        set_field(&mut root, "isSummary", Value::Bool(false));
    }
    adjust_extension_root_constraint_sources(
        &mut root,
        slice,
        is_local_profile && project_local_root_constraints,
        host_extension_source,
    );
    if (!is_local_profile && !allow_local_root_constraints)
        || (is_local_profile && !allow_local_root_condition)
        || diff.get("max").and_then(Value::as_str) == Some("0")
        || omits_extension_root_condition(profile_url)
    {
        remove_field(&mut root, "condition");
    }
    if !allow_local_root_constraints {
        remove_field(&mut root, "constraint");
    } else if is_local_profile && !project_local_root_constraints {
        retain_base_extension_constraints(&mut root);
    }
    fill_missing_constraint_sources_on_constrained_element(
        &mut root,
        host_extension_source.unwrap_or(profile_url),
    );

    for key in [
        "short",
        "definition",
        "comment",
        "requirements",
        "alias",
        "condition",
        "min",
        "max",
        "isModifier",
        "isModifierReason",
        "mapping",
    ] {
        if let Some(value) = root.get(key) {
            set_field(slice, key, value.clone());
        } else {
            remove_field(slice, key);
        }
    }
    // isSummary is never stripped by Java's root overlay: the slice keeps whatever
    // it inherits (a stored slice like us-core birthsex carries none; a fresh slice
    // cloned from the unsliced extension element carries false). Only overwrite it
    // when the (core/local) root explicitly carries an isSummary value.
    if let Some(value) = root.get("isSummary") {
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

fn adjust_extension_root_constraint_sources(
    root: &mut Value,
    slice: &Value,
    local_profile: bool,
    host_extension_source: Option<&str>,
) {
    let Some(source) =
        extension_slice_ext_constraint_source(slice, local_profile, host_extension_source)
    else {
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
    host_extension_source: Option<&str>,
) -> Option<String> {
    let path = slice.get("path").and_then(Value::as_str)?;
    if !path.ends_with(".extension") && !path.ends_with(".modifierExtension") {
        return None;
    }
    if local_profile {
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
        || profile_url == "http://hl7.org/fhir/StructureDefinition/workflow-supportingInfo"
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
    if !uses_profile_root_overlay(&profile) {
        return Ok(());
    }
    let Some(mut root) = profile_root_element(&profile, ctx)? else {
        return Ok(());
    };
    if native_r5 {
        let constraint_xpaths = HashMap::new();
        project_element_to_native_r5(
            &mut root,
            &structure_source(&profile, profile_url),
            snapshot_source_value(&profile).as_deref(),
            &constraint_xpaths,
        );
    }
    fill_missing_constraint_sources_on_constrained_element(&mut root, profile_url);

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
        if let Some(value) = root.get(key) {
            set_field(target, key, value.clone());
        } else if key != "comment" {
            remove_field(target, key);
        }
    }
    Ok(())
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

    let diff_root = profile
        .get("differential")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .and_then(|a| a.first());

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
        if !merged.contains(item) {
            merged.push(item.clone());
        }
    }
    for item in existing {
        if !merged.contains(&item) {
            merged.push(item);
        }
    }
    set_field(target, key, Value::Array(merged));
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

fn merge_extensions_from_definition(target: &mut Value, diff: &Value, strip_non_inherited: bool) {
    if strip_non_inherited {
        remove_non_inherited_extensions(target);
    }
    dedupe_extension_values(target, "extension");
    let Some(source_exts) = diff.get("extension").and_then(Value::as_array) else {
        return;
    };
    let target_exts = ensure_array_field(target, "extension");
    for ext in source_exts {
        if !target_exts.contains(ext) {
            target_exts.push(ext.clone());
        }
    }
}

fn dedupe_extension_values(parent: &mut Value, key: &str) {
    let Some(obj) = parent.as_object_mut() else {
        return;
    };
    let Some(Value::Array(exts)) = obj.get_mut(key) else {
        return;
    };
    let mut seen: Vec<Value> = Vec::new();
    exts.retain(|ext| {
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
        remove_non_inherited_extensions(&mut element);
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
        );
    }
    element
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

fn project_r4_snapshot_to_native_r5(structure: &mut Value) {
    let source = structure_source(
        structure,
        structure
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("StructureDefinition"),
    );
    let snapshot_source = snapshot_source_value(structure);
    let constraint_xpaths = differential_constraint_xpaths(structure);
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
            );
        }
    }
}

fn project_element_to_native_r5(
    element: &mut Value,
    constraint_source: &str,
    snapshot_source: Option<&str>,
    constraint_xpaths: &HashMap<(String, String), String>,
) {
    remove_non_inherited_extensions(element);
    convert_constraint_xpaths_to_extensions(element, constraint_xpaths);
    strip_constraint_xpaths(element);
    fill_missing_constraint_sources(element, constraint_source);
    convert_additional_binding_extensions(element);
    normalize_fhir_type_extension(element);
    add_snapshot_source_to_obligations(element, snapshot_source);
    trim_mapping_maps(element);
}

const CONSTRAINT_XPATH_EXTENSION_URL: &str =
    "http://hl7.org/fhir/4.0/StructureDefinition/extension-ElementDefinition.constraint.xpath";

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
        if ext.get("url").and_then(Value::as_str)
            == Some("http://hl7.org/fhir/tools/StructureDefinition/additional-binding")
        {
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
                _ => residual_exts.push(child.clone()),
            }
        }
    }
    if !residual_exts.is_empty() {
        out.insert("extension".to_string(), Value::Array(residual_exts));
    }
    Value::Object(out)
}

fn strip_constraint_xpaths(element: &mut Value) {
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        remove_field(constraint, "xpath");
    }
}

fn fill_missing_constraint_sources(element: &mut Value, source: &str) {
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        let Some(obj) = constraint.as_object_mut() else {
            continue;
        };
        let key = obj.get("key").and_then(Value::as_str);
        if !obj.contains_key("source") && !preserves_missing_constraint_source(key) {
            obj.insert("source".to_string(), Value::String(source.to_string()));
        }
    }
}

fn preserves_missing_constraint_source(key: Option<&str>) -> bool {
    matches!(
        key,
        Some("us-core-16" | "us-core-17" | "us-core-18" | "us-core-19")
    )
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
    let Some(exts) = element.get_mut("extension").and_then(Value::as_array_mut) else {
        return;
    };
    for ext in exts {
        if ext.get("url").and_then(Value::as_str)
            != Some("http://hl7.org/fhir/StructureDefinition/obligation")
        {
            continue;
        }
        let children = ensure_array_field(ext, "extension");
        if children.iter().any(|child| {
            child.get("url").and_then(Value::as_str)
                == Some("http://hl7.org/fhir/tools/StructureDefinition/snapshot-source")
        }) {
            continue;
        }
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
const NON_INHERITED_ED_URLS: &[&str] = &[
    "http://hl7.org/fhir/tools/StructureDefinition/binding-definition",
    "http://hl7.org/fhir/tools/StructureDefinition/no-binding",
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-isCommonBinding",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-category",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-fmm",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-implements",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-explicit-type-name",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-security-category",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-wg",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-normative-version",
    "http://hl7.org/fhir/tools/StructureDefinition/obligation-profile",
    "http://hl7.org/fhir/StructureDefinition/obligation-profile",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status-reason",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-summary",
];

fn remove_non_inherited_extensions(element: &mut Value) {
    remove_extension_urls(element, "extension");
    if let Some(binding) = element.get_mut("binding") {
        remove_extension_urls(binding, "extension");
    }
}

fn remove_extension_urls(parent: &mut Value, key: &str) {
    let Some(obj) = parent.as_object_mut() else {
        return;
    };
    let Some(Value::Array(exts)) = obj.get_mut(key) else {
        return;
    };
    exts.retain(|ext| {
        let url = ext.get("url").and_then(Value::as_str).unwrap_or("");
        !NON_INHERITED_ED_URLS.contains(&url)
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
            | "StructureDefinition-us-ph-composition.html"
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

    pub fn fetch(&self, query: &str) -> Option<Value> {
        let path = self
            .by_url
            .get(query)
            .map(|e| &e.path)
            .or_else(|| self.by_id.get(query))
            .or_else(|| self.by_name.get(query))?;
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
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
