//! Main cursor loop `process_paths` (PPP:191/198) + dispatch + checkAllElementsOK.

use serde_json::Value;

use super::context::WalkContext;
use super::frame::{WalkCursor, WalkFrame};
use super::paths::*;
use super::trace;
use super::{simple, sliced, slicing};

/// Returns the first emitted output index (`res`) for the top of this frame.
pub(crate) fn process_paths(
    ctx: &mut WalkContext,
    cur: &mut WalkCursor,
    frame: &WalkFrame,
    slicer: Option<&Value>,
) -> anyhow::Result<Option<usize>> {
    let mut res: Option<usize> = None;
    let mut type_list: Vec<super::types::TypeSlice> = Vec::new();
    let mut first = true;

    while cur.base_cursor <= frame.base_limit && cur.base_cursor < cur.base.len() {
        let current_base = cur.base[cur.base_cursor].clone();
        let current_base_path = fixed_path_source(
            frame.context_path_source.as_deref(),
            path_of(&current_base),
            &frame.redirector,
        );
        let diff_matches = get_diff_matches(&ctx.diff, &current_base_path, cur.diff_cursor, frame.diff_limit);

        let dc = cur.diff_cursor;

        if trace::active() {
            let diff_ids: Vec<String> = diff_matches
                .iter()
                .filter_map(|&i| trace::id(&ctx.diff[i]))
                .collect();
            let diff_cursor_ed = ctx.diff.get(cur.diff_cursor).and_then(trace::id);
            let x = serde_json::json!({
                "basePath": current_base_path,
                "baseCursor": cur.base_cursor,
                "baseLimit": frame.base_limit,
                "diffCursor": cur.diff_cursor,
                "diffLimit": frame.diff_limit,
                "diffMatches": diff_matches.len(),
                "baseHasSlicing": has_slicing(&current_base),
                "slicingDone": frame.slicing.done,
                "diffMatchIds": diff_ids,
            });
            trace::rec(
                "processPaths",
                "processPaths.iteration",
                trace::id(&current_base).as_deref(),
                diff_cursor_ed.as_deref(),
                Some(x),
            );
        }

        let slicing_path = frame.slicing.path.as_deref();
        if !has_slicing(&current_base) || Some(current_base_path.as_str()) == slicing_path {
            if trace::active() {
                let diff_cursor_ed = ctx.diff.get(cur.diff_cursor).and_then(trace::id);
                trace::rec(
                    "processPaths",
                    "processPaths.dispatch.simplePath",
                    trace::id(&current_base).as_deref(),
                    diff_cursor_ed.as_deref(),
                    None,
                );
            }
            let current_res = simple::process_simple_path(
                ctx,
                cur,
                frame,
                &current_base,
                &current_base_path,
                &diff_matches,
                &mut type_list,
                if first { slicer } else { None },
            )?;
            if res.is_none() {
                res = current_res;
            }
        } else {
            if trace::active() {
                let diff_cursor_ed = ctx.diff.get(cur.diff_cursor).and_then(trace::id);
                trace::rec(
                    "processPaths",
                    "processPaths.dispatch.slicedBase",
                    trace::id(&current_base).as_deref(),
                    diff_cursor_ed.as_deref(),
                    None,
                );
            }
            sliced::process_path_with_sliced_base(
                ctx,
                cur,
                frame,
                &current_base,
                &current_base_path,
                &diff_matches,
                &mut type_list,
            )?;
        }

        // R-WALK-1 blanket advance (PPP:238).
        if !diff_matches.is_empty() && dc == cur.diff_cursor {
            cur.diff_cursor += diff_matches.len();
        }
        first = false;
    }

    check_all_elements_ok(ctx)?;
    Ok(res)
}

fn check_all_elements_ok(ctx: &WalkContext) -> anyhow::Result<()> {
    for e in &ctx.output {
        if let Some(min) = e.get("min") {
            if min.is_null() {
                anyhow::bail!("NULL_MIN: an element has a null min");
            }
        }
    }
    let _ = slicing::noop();
    Ok(())
}
