//! Shared `genTypes` / `genTargetLink` (SDR:2317 / 2534) — the element-type
//! cell builder, used by BOTH the SUMMARY element table (`table::TCtx`) and the
//! grid (`grid::GridCtx`). Previously duplicated branch-for-branch; unified here
//! as a trait with default methods.
//!
//! The two callers differ only in three inputs, all expressed as trait methods:
//!   - `must_support_mode()`  — grid: always false (non-MS view).
//!   - `pointer(e)`           — grid: always None (non-diff view).
//!   - the `dim` argument     — grid: always false (non-diff, so nothing dims).
//! Under `dim=false`, `dim_piece(p,false) == p`; under `ms_mode=false`, every
//! `if ms_mode …` filter is skipped and the `!ms_mode …` S-flag paths reduce to
//! the grid's `type_is_must_support && mustSupport` checks. So the unified body
//! reproduces the grid's prior output byte-for-byte (gate-verified).

use render_tables::model::{Cell, Piece};
use serde_json::Value;

use crate::context::IgContext;
use crate::sdmodel::{Ed, TypeRef};
use crate::table::{
    all_canonicals_must_support, all_types_must_support, canonical_is_must_support, dim_piece,
    is_profiled_type, tail, type_is_must_support, type_is_must_support_full, type_name_of,
    RED_BACKGROUND_COLOR,
};

/// The host context `genTypes` needs. `'a` is the borrowed-snapshot lifetime.
pub trait TypesHost<'a> {
    fn ctx(&self) -> &IgContext;
    fn core_path(&self) -> &str;
    /// The rendered profile's root object (for `url` / `baseDefinition`).
    fn sd_root(&self) -> &Value;
    fn gap(&mut self, what: &str);
    /// diff-mode SNAPSHOT_DERIVATION_POINTER; None outside diff/grid.
    fn pointer(&self, e: Ed<'_>) -> Option<Ed<'a>>;
    /// `context.getStructureMode()` mustSupport filtering (by-mustsupport view).
    fn must_support_mode(&self) -> bool;

    fn sd_url(&self) -> &str {
        self.sd_root().get("url").and_then(|x| x.as_str()).unwrap_or("")
    }

    /// `genTypes` (SDR:2317). `dim` dims every emitted piece (diff EQUALS).
    ///
    /// Generic over the element lifetime `'e` (not tied to the host's `'a`): the
    /// extension value[x] path (SDR:1402) calls this with a value definition
    /// whose JSON outlives only the call, NOT the borrowed snapshot. The `pointer`
    /// branch (which yields `Ed<'a>` types) only fires for empty-types elements —
    /// value defns always have types — so `'e` and `'a` never need to unify there.
    fn gen_types<'e>(&mut self, e: Ed<'e>, types: &[TypeRef<'e>], root: bool, dim: bool) -> Cell {
        let mut c = Cell::new();
        if let Some(cr) = e.content_reference() {
            // (SDR:2320-2334 + getElementByName): the snapshot generator writes
            // absolute contentReferences ("http://...#Path"); a bare "#Path"
            // resolves in this profile.
            let (url, frag) = match cr.split_once('#') {
                Some((u, f)) => (u, f),
                None => ("", cr),
            };
            if url.is_empty() || url == self.sd_url() {
                c.pieces.push(Piece::ref_text(None, Some("See ".into()), None));
                c.pieces.push(Piece::ref_text(
                    Some(format!("#{}", frag)),
                    Some(tail(frag).to_string()),
                    Some(frag.to_string()),
                ));
            } else if let Some(src) = self.ctx().resolve(url) {
                let type_name = src.name.clone().unwrap_or_else(|| tail(url).to_string());
                c.pieces.push(Piece::ref_text(None, Some("See ".into()), None));
                c.pieces.push(Piece::ref_text(
                    Some(format!("{}#{}", src.web_path, frag)),
                    Some(format!("{} ({})", tail(frag), type_name)),
                    Some(frag.to_string()),
                ));
            } else {
                self.gap("unresolved contentReference");
            }
            return c;
        }
        if types.is_empty() {
            if root {
                // base branch (SDR:2337-2350)
                let base_url = self
                    .sd_root()
                    .get("baseDefinition")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(bsd) = self.ctx().resolve(&base_url) {
                    // SDR:2340-2343: "(version)" when multiple loaded versions.
                    let v = if base_url.contains('|') || self.ctx().version_count(&base_url) > 1 {
                        format!("({})", bsd.version)
                    } else {
                        String::new()
                    };
                    let name = format!("{}{}", bsd.name.clone().unwrap_or_default(), v);
                    c.pieces
                        .push(Piece::ref_text(Some(bsd.web_path.clone()), Some(name), None));
                }
                return c;
            }
            // diff mode, non-root, no restated types: take the pointer's types,
            // each marked SNAPSHOT_DERIVATION_EQUALS (SDR:2357-2364) so the
            // checkForNoChange-wrapped pieces render dimmed.
            if let Some(p) = self.pointer(e) {
                let pt = p.types();
                if !pt.is_empty() {
                    return self.gen_types(e, &pt, root, true);
                }
            }
            return c;
        }
        let ms_mode = self.must_support_mode();
        let all_types_ms = all_types_must_support(types);
        let mut first = true;
        for t in types {
            // mustSupportMode type filter (SDR:2375).
            if ms_mode && !all_types_ms && !type_is_must_support_full(t) {
                continue;
            }
            if first {
                first = false;
            } else {
                c.pieces
                    .push(dim_piece(Piece::ref_text(None, Some(", ".into()), None), dim));
            }
            if t.has_target() {
                // Reference/canonical (SDR:2379-2427)
                if !t.profiles().is_empty() {
                    let ref_ = t.profiles()[0];
                    if let Some(tsd) = self.ctx().resolve(ref_) {
                        // SDR:2385-2389: "(version)" when multiple versions.
                        let name = if ref_.contains('|') || self.ctx().version_count(ref_) > 1 {
                            tsd.name.clone().map(|n| format!("{}({})", n, tsd.version))
                        } else {
                            tsd.name.clone()
                        };
                        c.pieces
                            .push(Piece::ref_text(Some(tsd.web_path.clone()), name, Some(tsd.present())));
                    } else {
                        c.pieces.push(Piece::ref_text(
                            Some(format!("{}references.html", self.core_path())),
                            Some(t.working_code().to_string()),
                            None,
                        ));
                    }
                } else {
                    c.pieces.push(Piece::ref_text(
                        Some(format!("{}references.html", self.core_path())),
                        Some(t.working_code().to_string()),
                        None,
                    ));
                }
                // " S" flag when isMustSupportDirect(t) && e.mustSupport
                if !ms_mode && type_is_must_support(t) && e.must_support() {
                    c.pieces.push(Piece::ref_text(None, Some(" ".into()), None));
                    c.add_styled_text(
                        Some("This type must be supported".into()),
                        Some("S".into()),
                        Some("white"),
                        Some(RED_BACKGROUND_COLOR),
                        None,
                        false,
                    );
                }
                c.pieces.push(Piece::ref_text(None, Some("(".into()), None));
                let tp_all_ms = all_canonicals_must_support(t, &t.target_profiles());
                let mut tfirst = true;
                for u in t.target_profiles() {
                    // targetProfile MS filter (SDR:2406).
                    if ms_mode && !tp_all_ms && !canonical_is_must_support(t, u) {
                        continue;
                    }
                    if tfirst {
                        tfirst = false;
                    } else {
                        c.pieces.push(Piece::ref_text(None, Some(" | ".into()), None));
                    }
                    self.gen_target_link(&mut c, t, u, dim);
                    if !ms_mode && canonical_is_must_support(t, u) && e.must_support() {
                        c.pieces.push(Piece::ref_text(None, Some(" ".into()), None));
                        // SDR:2414: targetProfile S also uses STRUC_DEF_TYPE_SUPP.
                        c.add_styled_text(
                            Some("This type must be supported".into()),
                            Some("S".into()),
                            Some("white"),
                            Some(RED_BACKGROUND_COLOR),
                            None,
                            false,
                        );
                    }
                }
                c.pieces.push(Piece::ref_text(None, Some(")".into()), None));
                // aggregation modes (SDR:2416-2427) — rare; gap if present
                if t.v.get("aggregation").is_some() {
                    self.gap("aggregation modes");
                }
            } else if !t.profiles().is_empty()
                && (t.working_code() != "Extension" || is_profiled_type(&t.profiles()))
            {
                // profiled type (SDR:2428-2461)
                let pf_all_ms = all_canonicals_must_support(t, &t.profiles());
                let mut pfirst = true;
                for p in t.profiles() {
                    // profile MS filter (SDR:2435).
                    if ms_mode && !pf_all_ms && !canonical_is_must_support(t, p) {
                        continue;
                    }
                    if pfirst {
                        pfirst = false;
                    } else {
                        c.pieces
                            .push(dim_piece(Piece::ref_text(None, Some(", ".into()), None), dim));
                    }
                    // getLinkForProfile -> webPath|name, name gains "(version)"
                    // when multiple versions of the canonical are loaded
                    // (IGKP:719-723).
                    if let Some(psd) = self.ctx().resolve(p) {
                        let name = if p.contains('|') || self.ctx().version_count(p) > 1 {
                            psd.name.clone().map(|n| format!("{}({})", n, psd.version))
                        } else {
                            psd.name.clone()
                        };
                        c.pieces.push(dim_piece(
                            Piece::ref_text(
                                Some(psd.web_path.clone()),
                                name,
                                Some(t.working_code().to_string()),
                            ),
                            dim,
                        ));
                    } else {
                        c.pieces.push(dim_piece(
                            Piece::ref_text(None, Some(t.working_code().to_string()), None),
                            dim,
                        ));
                    }
                    if !ms_mode && canonical_is_must_support(t, p) && e.must_support() {
                        c.pieces.push(Piece::ref_text(None, Some(" ".into()), None));
                        c.add_styled_text(
                            Some("This profile must be supported".into()),
                            Some("S".into()),
                            Some("white"),
                            Some(RED_BACKGROUND_COLOR),
                            None,
                            false,
                        );
                    }
                }
            } else {
                // plain type (SDR:2462-2501)
                let tc = t.working_code();
                if tc.starts_with("http://") || tc.starts_with("https://") {
                    if let Some(sd) = self.ctx().resolve_type(tc) {
                        // getLinkFor(corePath, tc) -> webPath; text = typeName
                        let tn = type_name_of(&sd, tc);
                        c.pieces.push(dim_piece(
                            Piece::ref_text(Some(sd.web_path.clone()), Some(tn), None),
                            dim,
                        ));
                    } else {
                        c.pieces.push(dim_piece(
                            Piece::ref_text(None, Some(tc.to_string()), None),
                            dim,
                        ));
                    }
                } else if self.ctx().has_link_for(tc) {
                    // pkp.hasLinkFor gate (IGKP:568): derivation must be
                    // specialization — base abstract types (Resource, Element)
                    // render as plain text.
                    let sd = self.ctx().resolve_type(tc).unwrap();
                    c.pieces.push(dim_piece(
                        Piece::ref_text(Some(sd.web_path.clone()), Some(tc.to_string()), None),
                        dim,
                    ));
                } else {
                    c.pieces
                        .push(dim_piece(Piece::ref_text(None, Some(tc.to_string()), None), dim));
                }
                if !ms_mode && type_is_must_support(t) && e.must_support() {
                    c.pieces.push(Piece::ref_text(None, Some(" ".into()), None));
                    c.add_styled_text(
                        Some("This type must be supported".into()),
                        Some("S".into()),
                        Some("white"),
                        Some(RED_BACKGROUND_COLOR),
                        None,
                        false,
                    );
                }
            }
        }
        c
    }

    /// `genTargetLink` (SDR:2534-2565). Every emitted piece is dimmed when `dim`
    /// (the checkForNoChange wrapping for pointer-derived EQUALS types). `_t` is
    /// unused (the Java signature carries it) — generic over its lifetime so the
    /// generic `gen_types` can pass its `TypeRef<'e>`.
    fn gen_target_link<'e>(&mut self, c: &mut Cell, _t: &TypeRef<'e>, u: &str, dim: bool) {
        if u.starts_with("http://hl7.org/fhir/StructureDefinition/") {
            if let Some(sd) = self.ctx().resolve(u) {
                let disp = sd.title.clone().or(sd.name.clone()).unwrap_or_default();
                c.pieces.push(dim_piece(
                    Piece::ref_text(Some(sd.web_path.clone()), Some(disp), None),
                    dim,
                ));
            } else {
                let rn = &u[40..];
                let link = self.ctx().resolve_type(rn).map(|r| r.web_path);
                c.pieces
                    .push(dim_piece(Piece::ref_text(link, Some(rn.to_string()), None), dim));
            }
        } else if u.starts_with("http://") || u.starts_with("https://") {
            if let Some(sd) = self.ctx().resolve(u) {
                let disp = sd.present();
                // href = getLinkForProfile == webPath (| stripped)
                let mut href = sd.web_path.clone();
                if let Some(i) = href.find('|') {
                    href.truncate(i);
                }
                c.pieces
                    .push(dim_piece(Piece::ref_text(Some(href), Some(disp), None), dim));
            } else {
                c.pieces
                    .push(dim_piece(Piece::ref_text(None, Some(u.to_string()), None), dim));
            }
        } else if u.starts_with('#') {
            self.gap("contained target profile link");
        }
    }
}
