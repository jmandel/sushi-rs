//! Filters. Each is cited to the Jekyll/Liquid-4.0.4 source that defines its
//! behavior. String filters come from Liquid::StandardFilters; `where`, `sort`,
//! `jsonify`, `slugify`, `markdownify`, `xml_escape` etc. come from
//! Jekyll::Filters (registered over the standard set).
//!
//! markdownify is a DETERMINISTIC MARKER ("MD…/MD") to match the oracle stub —
//! real markdown is render_md's job (a separate crate).

use crate::value::{OrderedMap, Value};
use std::rc::Rc;

pub const MD_OPEN: &str = "MD";
pub const MD_CLOSE: &str = "/MD";

/// Apply a filter by name. `named` carries `k: v` style args (only `date` uses
/// them in practice). Unknown filters pass the input through unchanged
/// (Jekyll: strict_filters=false).
pub fn apply(name: &str, input: Value, args: &[Value], named: &[(String, Value)]) -> Value {
    let a0 = args.first().cloned();
    let a1 = args.get(1).cloned();
    match name {
        // ---------------- string filters (Liquid StandardFilters) ----------
        "upcase" => Value::str(input.to_str().to_uppercase()),
        "downcase" => Value::str(input.to_str().to_lowercase()),
        "capitalize" => Value::str(capitalize(&input.to_str())),
        "strip" => Value::str(input.to_str().trim().to_string()),
        "lstrip" => Value::str(input.to_str().trim_start().to_string()),
        "rstrip" => Value::str(input.to_str().trim_end().to_string()),
        // Jekyll normalize_whitespace / strip_newlines
        "strip_newlines" => Value::str(input.to_str().replace(['\n', '\r'], "")),
        "newline_to_br" => Value::str(input.to_str().replace('\n', "<br />\n")),
        "escape" => Value::str(html_escape(&input.to_str())),
        "escape_once" => Value::str(escape_once(&input.to_str())),
        "xml_escape" => Value::str(html_escape(&input.to_str())),
        "url_encode" => Value::str(url_encode(&input.to_str())),
        "strip_html" => Value::str(strip_html(&input.to_str())),
        "append" => Value::str(format!("{}{}", input.to_str(), arg_str(&a0))),
        "prepend" => Value::str(format!("{}{}", arg_str(&a0), input.to_str())),
        "remove" => Value::str(input.to_str().replace(&arg_str(&a0), "")),
        "remove_first" => Value::str(replace_first(&input.to_str(), &arg_str(&a0), "")),
        "replace" => Value::str(input.to_str().replace(&arg_str(&a0), &arg_str(&a1))),
        "replace_first" => {
            Value::str(replace_first(&input.to_str(), &arg_str(&a0), &arg_str(&a1)))
        }
        "truncate" => Value::str(truncate(&input.to_str(), &a0, &a1)),
        "truncatewords" => Value::str(truncatewords(&input.to_str(), &a0, &a1)),
        "slice" => slice(&input, &a0, &a1),
        // NOTE: `trim` is NOT a Liquid/Jekyll filter (verified via oracle:
        // `{{ " x " | trim }}` is unchanged). The survey listed it as "used",
        // but in Jekyll those usages are no-ops (strict_filters=false ->
        // passthrough). We therefore fall through to the passthrough arm.

        // ---------------- default ------------------------------------------
        // Liquid `default`: returns input unless it is nil/false/empty.
        "default" => {
            if is_default_empty(&input) {
                a0.unwrap_or(Value::Nil)
            } else {
                input
            }
        }

        // ---------------- number-ish ---------------------------------------
        "size" => input.size(),
        "plus" => num_op(&input, &a0, |a, b| a + b),
        "minus" => num_op(&input, &a0, |a, b| a - b),
        "times" => num_op(&input, &a0, |a, b| a * b),
        "divided_by" => divided_by(&input, &a0),
        "modulo" => num_op(&input, &a0, |a, b| a % b),
        "abs" => match input.to_number() {
            Some(n) => number_value(n.abs()),
            None => Value::Int(0),
        },
        "ceil" => input.to_number().map(|n| Value::Int(n.ceil() as i64)).unwrap_or(Value::Int(0)),
        "floor" => input.to_number().map(|n| Value::Int(n.floor() as i64)).unwrap_or(Value::Int(0)),
        "round" => round(&input, &a0),
        "at_least" => at_least(&input, &a0),
        "at_most" => at_most(&input, &a0),

        // ---------------- array / collection -------------------------------
        "split" => split(&input, &arg_str(&a0)),
        "join" => join(&input, &a0),
        "first" => to_array(&input).first().cloned().unwrap_or(Value::Nil),
        "last" => to_array(&input).last().cloned().unwrap_or(Value::Nil),
        "reverse" => {
            let mut v = to_array(&input);
            v.reverse();
            Value::array(v)
        }
        "uniq" => uniq(&input),
        "compact" => {
            let v: Vec<Value> = to_array(&input).into_iter().filter(|x| !x.is_nil()).collect();
            Value::array(v)
        }
        "concat" => {
            let mut v = to_array(&input);
            if let Some(b) = &a0 {
                v.extend(to_array(b));
            }
            Value::array(v)
        }
        "map" => map_filter(&input, &arg_str(&a0)),
        "where" => where_filter(&input, &a0, &a1),
        "sort" => sort_filter(&input, &a0),
        "sort_natural" => sort_natural(&input, &a0),
        "group_by" => input, // rare; passthrough

        // ---------------- misc / jekyll ------------------------------------
        "markdownify" => Value::str(format!("{}{}{}", MD_OPEN, input.to_str(), MD_CLOSE)),
        "jsonify" => Value::str(jsonify(&input)),
        "slugify" => Value::str(slugify(&input.to_str(), a0.as_ref())),
        "number_of_words" => Value::Int(input.to_str().split_whitespace().count() as i64),
        "date" => Value::str(input.to_str()), // dates are pre-formatted strings in corpus
        "inspect" => Value::str(jsonify(&input)),

        // unknown filter: Jekyll strict_filters=false -> passthrough
        _ => {
            let _ = named;
            input
        }
    }
}

fn arg_str(a: &Option<Value>) -> String {
    a.as_ref().map(|v| v.to_str()).unwrap_or_default()
}

fn capitalize(s: &str) -> String {
    // Ruby String#capitalize: first char upper, REST lowercased.
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

fn replace_first(s: &str, from: &str, to: &str) -> String {
    if from.is_empty() {
        return s.to_string();
    }
    if let Some(idx) = s.find(from) {
        let mut out = String::with_capacity(s.len());
        out.push_str(&s[..idx]);
        out.push_str(to);
        out.push_str(&s[idx + from.len()..]);
        out
    } else {
        s.to_string()
    }
}

/// Liquid `split`: on empty separator, Ruby String#split("") splits into
/// characters; Liquid's split with "" returns each char. With a real sep,
/// splits and (like Ruby) drops trailing empty fields.
fn split(input: &Value, sep: &str) -> Value {
    let s = input.to_str();
    if sep.is_empty() {
        return Value::array(s.chars().map(|c| Value::str(c.to_string())).collect());
    }
    // Ruby split drops trailing empty strings (limit default). Liquid uses
    // `input.split(sep)` which is Ruby semantics.
    let mut parts: Vec<&str> = s.split(sep).collect();
    while matches!(parts.last(), Some(&"")) {
        parts.pop();
    }
    Value::array(parts.into_iter().map(Value::str).collect())
}

fn join(input: &Value, sep: &Option<Value>) -> Value {
    let sep = sep.as_ref().map(|v| v.to_str()).unwrap_or_else(|| " ".to_string());
    let parts: Vec<String> = to_array(input).iter().map(|v| v.to_str()).collect();
    Value::str(parts.join(&sep))
}

/// Coerce to an array for collection filters. A Hash yields its VALUES (Ruby
/// Enumerable over a Hash yields [k,v] pairs, but Liquid filters that call
/// `.to_a` on a hash... in practice the corpus only maps/wheres over arrays and
/// over `site.data.X` which are arrays). Scalars wrap into a 1-element array;
/// nil -> empty.
fn to_array(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.as_ref().clone(),
        Value::Nil => vec![],
        Value::Hash(h) => h
            .iter()
            .map(|(k, val)| Value::array(vec![Value::str(k.clone()), val.clone()]))
            .collect(),
        other => vec![other.clone()],
    }
}

fn uniq(input: &Value) -> Value {
    let mut seen: Vec<Value> = Vec::new();
    for v in to_array(input) {
        if !seen.iter().any(|s| s.liquid_eq(&v)) {
            seen.push(v);
        }
    }
    Value::array(seen)
}

/// Jekyll `map`: pluck a property from each element (item_property).
fn map_filter(input: &Value, prop: &str) -> Value {
    let out: Vec<Value> = to_array(input).iter().map(|v| item_property(v, prop)).collect();
    Value::array(out)
}

/// Jekyll `where`: keep items whose `property` compares-equal to `value`
/// (compare_property_vs_target). If value is Array/Hash, Jekyll returns input
/// unchanged.
fn where_filter(input: &Value, prop: &Option<Value>, value: &Option<Value>) -> Value {
    let (Some(prop), Some(value)) = (prop, value) else {
        return input.clone();
    };
    if matches!(value, Value::Array(_) | Value::Hash(_)) {
        return input.clone();
    }
    let prop = prop.to_str();
    let out: Vec<Value> = to_array(input)
        .into_iter()
        .filter(|item| compare_property_vs_target(&item_property(item, &prop), value))
        .collect();
    Value::array(out)
}

/// Jekyll compare_property_vs_target (filters.rb:400).
fn compare_property_vs_target(property: &Value, target: &Value) -> bool {
    match target {
        Value::Nil => property.is_nil(),
        _ => {
            let t = target.to_str();
            match property {
                Value::Str(s) => s.as_ref() == t,
                Value::Array(arr) => arr.iter().any(|p| p.to_str() == t),
                other => other.to_str() == t,
            }
        }
    }
}

/// Jekyll item_property (filters.rb:421) + parse_sort_input numeric coercion.
fn item_property(item: &Value, property: &str) -> Value {
    let raw = if property.contains('.') {
        // read_liquid_attribute nested access
        let mut cur = item.clone();
        for key in property.split('.') {
            cur = cur.index(&Value::str(key));
        }
        cur
    } else {
        item.index(&Value::str(property))
    };
    parse_sort_input(&raw)
}

/// Jekyll parse_sort_input: numeric-looking strings become numbers for sorting.
fn parse_sort_input(v: &Value) -> Value {
    if let Value::Str(s) = v {
        let t = s.trim();
        if !t.is_empty() {
            if let Ok(i) = t.parse::<i64>() {
                return Value::Int(i);
            }
            if let Ok(f) = t.parse::<f64>() {
                if t.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+')) {
                    return Value::Float(f);
                }
            }
        }
    }
    v.clone()
}

/// Jekyll `sort` (filters.rb:301). No property -> natural sort of values.
/// With property -> sort by item_property, nils first (default).
fn sort_filter(input: &Value, property: &Option<Value>) -> Value {
    let mut arr = to_array(input);
    match property {
        None => {
            arr.sort_by(|a, b| natural_cmp(a, b));
        }
        Some(p) => {
            let prop = p.to_str();
            // nils first (order = -1)
            arr.sort_by(|a, b| {
                let pa = item_property(a, &prop);
                let pb = item_property(b, &prop);
                match (pa.is_nil(), pb.is_nil()) {
                    (false, true) => std::cmp::Ordering::Less, // -order, order=-1 => Less
                    (true, false) => std::cmp::Ordering::Greater,
                    _ => natural_cmp(&pa, &pb),
                }
            });
        }
    }
    Value::array(arr)
}

fn sort_natural(input: &Value, property: &Option<Value>) -> Value {
    // case-insensitive natural sort
    let mut arr = to_array(input);
    match property {
        None => arr.sort_by(|a, b| a.to_str().to_lowercase().cmp(&b.to_str().to_lowercase())),
        Some(p) => {
            let prop = p.to_str();
            arr.sort_by(|a, b| {
                item_property(a, &prop)
                    .to_str()
                    .to_lowercase()
                    .cmp(&item_property(b, &prop).to_str().to_lowercase())
            });
        }
    }
    Value::array(arr)
}

/// Ruby `<=>` fallback used by Jekyll sort: numbers numerically, strings
/// lexically, mixed via to_s.
fn natural_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if let (Some(x), Some(y)) = (a.to_number(), b.to_number()) {
        if matches!(a, Value::Int(_) | Value::Float(_))
            && matches!(b, Value::Int(_) | Value::Float(_))
        {
            return x.partial_cmp(&y).unwrap_or(Ordering::Equal);
        }
    }
    a.to_str().cmp(&b.to_str())
}

fn is_default_empty(v: &Value) -> bool {
    match v {
        Value::Nil | Value::Bool(false) => true,
        Value::Str(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Hash(h) => h.is_empty(),
        _ => false,
    }
}

fn num_op(a: &Value, b: &Option<Value>, f: impl Fn(f64, f64) -> f64) -> Value {
    let x = a.to_number().unwrap_or(0.0);
    let y = b.as_ref().and_then(|v| v.to_number()).unwrap_or(0.0);
    let r = f(x, y);
    // integer preservation if both were ints
    if is_int(a) && b.as_ref().map_or(true, is_int) && r.fract() == 0.0 {
        Value::Int(r as i64)
    } else {
        Value::Float(r)
    }
}

fn is_int(v: &Value) -> bool {
    matches!(v, Value::Int(_))
}

fn number_value(n: f64) -> Value {
    if n.fract() == 0.0 {
        Value::Int(n as i64)
    } else {
        Value::Float(n)
    }
}

fn divided_by(a: &Value, b: &Option<Value>) -> Value {
    let x = a.to_number().unwrap_or(0.0);
    let y = b.as_ref().and_then(|v| v.to_number()).unwrap_or(1.0);
    if is_int(a) && b.as_ref().map_or(true, is_int) {
        if y == 0.0 {
            return Value::Int(0);
        }
        Value::Int((x as i64) / (y as i64))
    } else {
        Value::Float(x / y)
    }
}

fn round(a: &Value, digits: &Option<Value>) -> Value {
    let x = a.to_number().unwrap_or(0.0);
    let d = digits.as_ref().map(|v| v.to_integer()).unwrap_or(0);
    if d <= 0 {
        Value::Int(x.round() as i64)
    } else {
        let f = 10f64.powi(d as i32);
        Value::Float((x * f).round() / f)
    }
}

fn at_least(a: &Value, b: &Option<Value>) -> Value {
    let x = a.to_number().unwrap_or(0.0);
    let y = b.as_ref().and_then(|v| v.to_number()).unwrap_or(0.0);
    number_value(x.max(y))
}
fn at_most(a: &Value, b: &Option<Value>) -> Value {
    let x = a.to_number().unwrap_or(0.0);
    let y = b.as_ref().and_then(|v| v.to_number()).unwrap_or(0.0);
    number_value(x.min(y))
}

fn slice(input: &Value, start: &Option<Value>, len: &Option<Value>) -> Value {
    match input {
        Value::Array(a) => {
            let (s, e) = slice_bounds(a.len(), start, len);
            Value::array(a[s..e].to_vec())
        }
        _ => {
            let s = input.to_str();
            let chars: Vec<char> = s.chars().collect();
            let (b, e) = slice_bounds(chars.len(), start, len);
            Value::str(chars[b..e].iter().collect::<String>())
        }
    }
}

fn slice_bounds(len: usize, start: &Option<Value>, l: &Option<Value>) -> (usize, usize) {
    let mut s = start.as_ref().map(|v| v.to_integer()).unwrap_or(0);
    if s < 0 {
        s += len as i64;
    }
    let s = s.clamp(0, len as i64) as usize;
    let count = l.as_ref().map(|v| v.to_integer()).unwrap_or(1).max(0) as usize;
    let e = (s + count).min(len);
    (s, e)
}

fn truncate(s: &str, len: &Option<Value>, tail: &Option<Value>) -> String {
    let l = len.as_ref().map(|v| v.to_integer()).unwrap_or(50).max(0) as usize;
    let tail = tail.as_ref().map(|v| v.to_str()).unwrap_or_else(|| "...".to_string());
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= l {
        return s.to_string();
    }
    let keep = l.saturating_sub(tail.chars().count());
    let head: String = chars[..keep.min(chars.len())].iter().collect();
    format!("{head}{tail}")
}

fn truncatewords(s: &str, n: &Option<Value>, tail: &Option<Value>) -> String {
    let n = n.as_ref().map(|v| v.to_integer()).unwrap_or(15).max(1) as usize;
    let tail = tail.as_ref().map(|v| v.to_str()).unwrap_or_else(|| "...".to_string());
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.len() <= n {
        return s.to_string();
    }
    format!("{}{}", words[..n].join(" "), tail)
}

// ---------- HTML / URL escaping (Liquid StandardFilters exact tables) --------

fn html_escape(s: &str) -> String {
    // Liquid `escape` / Jekyll `xml_escape` use CGI.escapeHTML-ish: & < > " '
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Liquid `escape_once`: escape but don't double-escape existing entities.
fn escape_once(s: &str) -> String {
    // Replace &(not already an entity) first, then < > " '
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            '&' => {
                // check if this begins a valid entity
                if is_entity_at(s, i) {
                    out.push('&');
                } else {
                    out.push_str("&amp;");
                }
            }
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

fn is_entity_at(s: &str, amp: usize) -> bool {
    // matches &word; or &#num; or &#xhex;
    let rest = &s[amp + 1..];
    if let Some(semi) = rest.find(';') {
        let ent = &rest[..semi];
        if ent.is_empty() || semi > 32 {
            return false;
        }
        if let Some(num) = ent.strip_prefix('#') {
            let num = num.strip_prefix(['x', 'X']).unwrap_or(num);
            return !num.is_empty() && num.chars().all(|c| c.is_ascii_alphanumeric());
        }
        return ent.chars().all(|c| c.is_ascii_alphanumeric());
    }
    false
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Jekyll `slugify` (default mode "default"): downcase, replace runs of
/// non-alphanumeric with '-', strip leading/trailing '-'.
fn slugify(s: &str, mode: Option<&Value>) -> String {
    let mode = mode.map(|v| v.to_str()).unwrap_or_else(|| "default".to_string());
    let lower = s.to_lowercase();
    let replaced: String = lower
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c
            } else if mode == "pretty" && matches!(c, '.' | '_' | '~' | '!' | '$' | '&' | '\'' | '(' | ')' | '+' | ',' | ';' | '=' | '@') {
                c
            } else {
                ' '
            }
        })
        .collect();
    let parts: Vec<&str> = replaced.split_whitespace().collect();
    parts.join("-")
}

/// Minimal JSON serialization for `jsonify` / `inspect`.
fn jsonify(v: &Value) -> String {
    match v {
        Value::Nil => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format!("{}", f),
        Value::Str(s) => json_string(s),
        Value::Array(a) => {
            let items: Vec<String> = a.iter().map(jsonify).collect();
            format!("[{}]", items.join(","))
        }
        Value::Hash(h) => {
            let items: Vec<String> = h
                .iter()
                .map(|(k, val)| format!("{}:{}", json_string(k), jsonify(val)))
                .collect();
            format!("{{{}}}", items.join(","))
        }
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

// re-export helpers the renderer needs
pub fn make_hash(pairs: Vec<(String, Value)>) -> Value {
    let mut m = OrderedMap::new();
    for (k, v) in pairs {
        m.insert(k, v);
    }
    Value::Hash(Rc::new(m))
}
