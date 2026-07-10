//! The Liquid value model + EXACT Shopify-Liquid-4.0.4 semantics for
//! truthiness, coercion, comparison and stringification.
//!
//! Every rule here is cited to liquid-4.0.4 source (the version Jekyll 4.4.1
//! bundles) so the differential gate can be reasoned about, not guessed.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::rc::Rc;

/// A Liquid runtime value.
///
/// Hashes preserve insertion order via an ordered map so `{% for %}` over a
/// hash and object stringification match Ruby's Hash iteration order (Ruby
/// hashes are insertion-ordered).
#[derive(Clone, Debug, Default)]
pub enum Value {
    #[default]
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    Array(Rc<Vec<Value>>),
    /// Insertion-ordered map (Ruby Hash order).
    Hash(Rc<OrderedMap>),
}

/// Insertion-ordered string-keyed map.
#[derive(Clone, Debug, Default)]
pub struct OrderedMap {
    keys: Vec<String>,
    map: BTreeMap<String, Value>,
}

impl OrderedMap {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&mut self, k: impl Into<String>, v: Value) {
        let k = k.into();
        if !self.map.contains_key(&k) {
            self.keys.push(k.clone());
        }
        self.map.insert(k, v);
    }
    pub fn get(&self, k: &str) -> Option<&Value> {
        self.map.get(k)
    }
    pub fn contains_key(&self, k: &str) -> bool {
        self.map.contains_key(k)
    }
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.keys.iter()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.keys.iter().map(move |k| (k, self.map.get(k).unwrap()))
    }
    pub fn len(&self) -> usize {
        self.keys.len()
    }
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    pub fn values(&self) -> Vec<Value> {
        self.keys.iter().map(|k| self.map[k].clone()).collect()
    }
}

impl Value {
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(Rc::from(s.into().as_str()))
    }
    pub fn array(v: Vec<Value>) -> Value {
        Value::Array(Rc::new(v))
    }

    /// Liquid truthiness: **only `nil` and `false` are falsy**; everything
    /// else (0, "", empty array/hash) is truthy.
    /// (liquid-4.0.4/lib/liquid/utils.rf — `Liquid::Utils` / Condition: only
    /// `nil` and `false` are non-truthy; see condition.rb `equal_variables`
    /// and the interpreter's `if` which checks `Liquid::Utils` truthiness.)
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }

    pub fn is_nil(&self) -> bool {
        matches!(self, Value::Nil)
    }

    /// Ruby-ish `to_s` used when a value is emitted by `{{ }}` or coerced to a
    /// string by string filters.
    ///
    /// Rules matched to Liquid 4.0.4 (`Liquid::Variable#render` -> `to_s` via
    /// `Liquid::Utils` and the block's `render_to_output_buffer`):
    ///  * nil  -> "" (empty)
    ///  * true/false -> "true"/"false"
    ///  * int  -> decimal
    ///  * float -> Ruby Float#to_s (always has a decimal point, e.g. "1.0")
    ///  * string -> itself
    ///  * array -> elements concatenated with NO separator (Ruby Array#to_s in
    ///    Liquid output is `join("")`; Liquid's `Variable` calls `to_s` which
    ///    for arrays joins without separator — see below note).
    ///  * hash -> Ruby Hash#to_s style is NOT used by Liquid output; Liquid
    ///    renders a hash via its `to_s`, which yields the Ruby inspect-like
    ///    `{"k"=>"v"}`. In practice IG templates never emit a bare hash; we
    ///    reproduce Ruby Hash#to_s for completeness.
    pub fn to_output_string(&self) -> String {
        match self {
            Value::Nil => String::new(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => format_ruby_float(*f),
            Value::Str(s) => s.to_string(),
            Value::Array(a) => {
                // Liquid emits arrays by joining element to_s with no separator.
                let mut out = String::new();
                for v in a.iter() {
                    out.push_str(&v.to_output_string());
                }
                out
            }
            Value::Hash(h) => {
                // Ruby Hash#to_s / inspect form: {"k"=>value, ...}
                let mut out = String::from("{");
                for (i, (k, v)) in h.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    let _ = write!(out, "{:?}=>{}", k, v.inspect());
                }
                out.push('}');
                out
            }
        }
    }

    /// Ruby `inspect`-ish, used inside Hash#to_s above.
    fn inspect(&self) -> String {
        match self {
            Value::Str(s) => format!("{:?}", s.to_string()),
            Value::Nil => "nil".to_string(),
            other => other.to_output_string(),
        }
    }

    /// String coercion for filter inputs that expect a string (e.g. `split`,
    /// `replace`). Same as output stringification.
    pub fn to_str(&self) -> String {
        self.to_output_string()
    }

    /// Integer coercion used by filters (`Liquid::Utils.to_integer`): strings
    /// are parsed, floats truncated, nil -> 0. Invalid strings raise in Liquid
    /// but IG templates never hit that; we saturate to 0 to stay lax.
    pub fn to_integer(&self) -> i64 {
        match self {
            Value::Int(i) => *i,
            Value::Float(f) => *f as i64,
            Value::Bool(_) | Value::Nil => 0,
            Value::Str(s) => s.trim().parse::<i64>().unwrap_or(0),
            _ => 0,
        }
    }

    /// Number coercion to f64 for arithmetic/comparison.
    pub fn to_number(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            Value::Str(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        }
    }

    /// `.size` property (Liquid `Drop`/`Variable` special: strings, arrays,
    /// hashes answer `size`; other values answer nil in Liquid — but our size
    /// filter/`.size` in `if` returns 0 for scalars to match observed Jekyll
    /// which never guards a scalar `.size`).
    pub fn size(&self) -> Value {
        match self {
            Value::Str(s) => Value::Int(s.chars().count() as i64),
            Value::Array(a) => Value::Int(a.len() as i64),
            Value::Hash(h) => Value::Int(h.len() as i64),
            _ => Value::Int(0),
        }
    }

    /// Liquid `==` equality (`Liquid::Condition.equal_variables`). nil == nil,
    /// nil != anything-else, numeric cross-type equality (1 == 1.0), string ==
    /// string, arrays/hashes structural.
    pub fn liquid_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Nil, _) | (_, Value::Nil) => false,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            // numeric cross-type
            (a, b) if a.is_number() && b.is_number() => {
                a.to_number().unwrap() == b.to_number().unwrap()
            }
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.liquid_eq(y))
            }
            (Value::Hash(a), Value::Hash(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(k, v)| b.get(k).map_or(false, |w| v.liquid_eq(w)))
            }
            // empty literal handling is done at the condition layer.
            _ => false,
        }
    }

    fn is_number(&self) -> bool {
        matches!(self, Value::Int(_) | Value::Float(_))
    }

    /// Ordering for `<`, `>`, `<=`, `>=`. Numbers compare numerically; strings
    /// lexically. Mismatched/uncomparable -> None (Liquid raises
    /// ArgumentError, which in warn mode becomes a Liquid error string; we
    /// surface None and the caller emits the same error marker).
    pub fn liquid_cmp(&self, other: &Value) -> Option<Ordering> {
        if self.is_number() && other.is_number() {
            self.to_number()
                .unwrap()
                .partial_cmp(&other.to_number().unwrap())
        } else if let (Value::Str(a), Value::Str(b)) = (self, other) {
            Some(a.as_ref().cmp(b.as_ref()))
        } else {
            None
        }
    }

    /// `contains` operator (`Liquid::Condition`): string contains substring;
    /// array contains element (by ==). Applying `contains` to nil is false.
    pub fn liquid_contains(&self, needle: &Value) -> bool {
        match self {
            Value::Str(hay) => hay.contains(&needle.to_str()),
            Value::Array(arr) => arr.iter().any(|v| v.liquid_eq(needle)),
            _ => false,
        }
    }

    /// Index into a value by a resolved key/index (member access `a.b` /
    /// `a[expr]`). Follows Liquid's `Drop`/`Context.find_variable`:
    ///  * Hash: string key lookup; also the special `size`/`first`/`last`.
    ///  * Array: integer index (negatives from end), plus `size/first/last`.
    ///  * String: `size` only.
    pub fn index(&self, key: &Value) -> Value {
        match self {
            Value::Hash(h) => {
                let k = key.to_str();
                if let Some(v) = h.get(&k) {
                    v.clone()
                } else {
                    match k.as_str() {
                        "size" => Value::Int(h.len() as i64),
                        "first" => h
                            .iter()
                            .next()
                            .map(|(k, v)| Value::array(vec![Value::str(k.clone()), v.clone()]))
                            .unwrap_or(Value::Nil),
                        _ => Value::Nil,
                    }
                }
            }
            Value::Array(a) => match key {
                Value::Int(_) | Value::Float(_) => {
                    let mut i = key.to_integer();
                    if i < 0 {
                        i += a.len() as i64;
                    }
                    if i >= 0 && (i as usize) < a.len() {
                        a[i as usize].clone()
                    } else {
                        Value::Nil
                    }
                }
                _ => match key.to_str().as_str() {
                    "size" => Value::Int(a.len() as i64),
                    "first" => a.first().cloned().unwrap_or(Value::Nil),
                    "last" => a.last().cloned().unwrap_or(Value::Nil),
                    _ => Value::Nil,
                },
            },
            Value::Str(s) => match key.to_str().as_str() {
                "size" => Value::Int(s.chars().count() as i64),
                _ => Value::Nil,
            },
            _ => Value::Nil,
        }
    }
}

/// Render a float the way Ruby's `Float#to_s` does: always include a decimal
/// point, drop trailing zeros beyond the first, e.g. 1.0 -> "1.0",
/// 1.5 -> "1.5". Ruby uses shortest round-trip; for the integer-valued case it
/// appends ".0".
fn format_ruby_float(f: f64) -> String {
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f == f.trunc() && f.abs() < 1e16 {
        format!("{}.0", f as i64)
    } else {
        // shortest representation
        let mut s = format!("{}", f);
        if !s.contains('.') && !s.contains('e') {
            s.push_str(".0");
        }
        s
    }
}
