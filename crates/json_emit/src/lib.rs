//! Byte-stable JSON emission with SUSHI-compatible property ordering. Phase 1/4.
//!
//! SUSHI serializes each resource with `JSON.stringify(obj, null, 2) + '\n'`
//! after running it through `orderedCloneDeep` (`common.ts:1571`), which moves
//! each `_x` "primitive sibling" key to sit immediately after its base key `x`
//! (orphan `_x` keys go last). Property order is otherwise the JS object
//! insertion order. We build resources as `serde_json::Value::Object` maps
//! (backed by `indexmap` via the `preserve_order` feature) in assignment order,
//! then emit with a 2-space pretty printer and a single trailing newline.

use serde_json::ser::{Formatter, PrettyFormatter};
use serde_json::{Map, Value};
use std::io;

/// Port of ECMAScript `Number::toString` (base 10) — exactly the algorithm V8
/// uses for `JSON.stringify(number)`. SUSHI parses every FSH numeric lexeme into
/// a JS `Number` and re-serializes it, so to be byte-identical we must reproduce
/// JS number formatting (e.g. `155e-8` → `0.00000155`, `2.3E11` → `230000000000`,
/// `6.453E+25` → `6.453e+25`).
///
/// Rust's `{:e}` formatting yields the same *shortest round-tripping* decimal
/// digit string that V8 (and serde/ryu) produce; the only thing that differs
/// between Rust's default `Display`/ryu and JS is the fixed-vs-exponential
/// *layout* decision and the exponent sign. This function re-decomposes `{:e}`
/// into (digits `s`, count `k`, ECMAScript position `n`) and applies the spec's
/// layout rules (ECMA-262 Number::toString steps 5-10).
pub fn js_number_string(value: f64) -> String {
    // Step 1-3: zero (and negative zero) render as "0".
    if value == 0.0 {
        return "0".to_string();
    }
    // Negative: emit "-" and recurse on the magnitude.
    if value < 0.0 {
        return format!("-{}", js_number_string(-value));
    }
    // Rust `{:e}` => "<d0>[.<rest>]e<exp>" with shortest round-tripping digits
    // and a single digit before the point. `exp` is the power of ten of d0.
    let sci = format!("{:e}", value);
    let (mantissa, exp_str) = sci.split_once('e').expect("`{:e}` always has 'e'");
    let exp: i64 = exp_str.parse().expect("integer exponent");
    // digits `s` = mantissa with the '.' removed; k = digit count.
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i64;
    // ECMAScript `n`: value = s × 10^(n-k), with 10^(k-1) ≤ s < 10^k.
    // From `{:e}`: value = s × 10^(exp-(k-1)), hence n = exp + 1.
    let n = exp + 1;

    if k <= n && n <= 21 {
        // Step 5: integer digits followed by (n-k) zeros.
        let mut out = digits;
        out.push_str(&"0".repeat((n - k) as usize));
        out
    } else if 0 < n && n <= 21 {
        // Step 6: first n digits, '.', remaining (k-n) digits.
        let (head, tail) = digits.split_at(n as usize);
        format!("{head}.{tail}")
    } else if -6 < n && n <= 0 {
        // Step 7: "0.", (-n) leading zeros, then all k digits.
        format!("0.{}{}", "0".repeat((-n) as usize), digits)
    } else {
        // Steps 8-10: exponential. Mantissa is d0[.d1..], exponent is (n-1).
        let mantissa_out = if k == 1 {
            digits.clone()
        } else {
            let (head, tail) = digits.split_at(1);
            format!("{head}.{tail}")
        };
        let e = n - 1;
        let sign = if e >= 0 { "+" } else { "-" };
        format!("{mantissa_out}e{sign}{}", e.abs())
    }
}

/// A pretty-printer that delegates structure (indentation, separators) to
/// serde_json's [`PrettyFormatter`] but renders floating-point numbers exactly
/// as JavaScript would (see [`js_number_string`]). All other primitives use the
/// default trait behavior, which is identical to `PrettyFormatter`.
struct JsPretty<'a> {
    inner: PrettyFormatter<'a>,
}

impl Formatter for JsPretty<'_> {
    fn write_f32<W>(&mut self, writer: &mut W, value: f32) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        writer.write_all(js_number_string(value as f64).as_bytes())
    }

    fn write_f64<W>(&mut self, writer: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        writer.write_all(js_number_string(value).as_bytes())
    }

    // --- delegate all structural methods to the inner PrettyFormatter ---
    fn begin_array<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.begin_array(w)
    }
    fn end_array<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.end_array(w)
    }
    fn begin_array_value<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.inner.begin_array_value(w, first)
    }
    fn end_array_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.end_array_value(w)
    }
    fn begin_object<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.begin_object(w)
    }
    fn end_object<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.end_object(w)
    }
    fn begin_object_key<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.inner.begin_object_key(w, first)
    }
    fn begin_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.begin_object_value(w)
    }
    fn end_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.end_object_value(w)
    }
}

/// Port of `orderedCloneDeep` (`sushi-ts/src/fhirtypes/common.ts:1571`).
/// Recursively reorders object keys so each `_key` is glued directly after its
/// base `key`; orphan underscore keys keep their order and land at the end.
/// Arrays are never reordered. Non-objects are returned as-is.
pub fn ordered_clone_deep(input: &Value) -> Value {
    match input {
        Value::Array(items) => Value::Array(items.iter().map(ordered_clone_deep).collect()),
        Value::Object(map) => {
            // Partition keys into non-underscore (base) keys and underscore keys.
            let mut underscore: Vec<&String> = map.keys().filter(|k| k.starts_with('_')).collect();
            let base: Vec<&String> = map.keys().filter(|k| !k.starts_with('_')).collect();

            let mut out = Map::new();
            for k in base {
                out.insert(k.clone(), ordered_clone_deep(&map[k]));
                let under = format!("_{k}");
                if let Some(pos) = underscore.iter().position(|u| **u == under) {
                    out.insert(under.clone(), ordered_clone_deep(&map[&under]));
                    underscore.remove(pos);
                }
            }
            // Leftover orphan underscore keys, in original order.
            for u in underscore {
                out.insert(u.clone(), ordered_clone_deep(&map[u]));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Serialize to the exact textual form SUSHI writes to disk: 2-space indented
/// JSON (matching `JSON.stringify(obj, null, 2)`) terminated by one `'\n'`.
/// Runs `ordered_clone_deep` first to apply underscore-sibling gluing.
pub fn to_fhir_json_string(value: &Value) -> String {
    let ordered = ordered_clone_deep(value);
    let mut buf = Vec::with_capacity(128);
    let formatter = JsPretty {
        inner: PrettyFormatter::with_indent(b"  "),
    };
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    serde::Serialize::serialize(&ordered, &mut ser).expect("serialize json");
    let mut s = String::from_utf8(buf).expect("serde_json emits valid utf-8");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn js_number_matches_v8() {
        // (f64, expected JS `Number.prototype.toString` output)
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-3.14, "-3.14"),
            (155e-8, "0.00000155"), // misc-025 origin.value
            (1.5e-3, "0.0015"),
            (0.88e6, "880000"),
            (2.3e11, "230000000000"),
            (6.453e25, "6.453e+25"),
            (4.50e3, "4500"),
            (300.0e-1, "30"),
            (48000e-5, "0.48"),
            (155.0, "155"),
            (1e21, "1e+21"),
            (1e-7, "1e-7"),
            (1.5, "1.5"),
            (12.34, "12.34"),
            (100.0, "100"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
        ];
        for (f, want) in cases {
            assert_eq!(&js_number_string(*f), want, "js_number_string({f})");
        }
    }

    #[test]
    fn fhir_json_uses_js_numbers() {
        let v = json!({ "value": 155e-8, "big": 6.453e25 });
        let s = to_fhir_json_string(&v);
        assert!(s.contains("\"value\": 0.00000155"), "{s}");
        assert!(s.contains("\"big\": 6.453e+25"), "{s}");
    }
}
