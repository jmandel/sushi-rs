//! The renderer: walk the AST against a scope stack + DataProvider, producing
//! the output string. Matches Jekyll/Liquid-4.0.4 evaluation semantics.

use crate::ast::*;
use crate::filters;
use crate::provider::DataProvider;
use crate::value::{OrderedMap, Value};
use std::rc::Rc;

/// Render options.
#[derive(Clone, Copy, Default)]
pub struct Options {
    /// Reproduce the IG Publisher quirk of evaluating tags inside `{% raw %}`
    /// (survey nasty #4). Default = correct Liquid raw (verbatim body).
    pub publisher_raw_quirk: bool,
}

/// A single stack frame of local variables (assign/capture/for-var/include
/// params). Ordered so shadowing is deterministic.
struct Scope {
    vars: OrderedMap,
}

pub struct Renderer<'p> {
    provider: &'p dyn DataProvider,
    opts: Options,
    scopes: Vec<Scope>,
    counters: OrderedMap, // increment/decrement counters (separate namespace)
    include_depth: usize,
}

/// Control-flow signal bubbled up through `for` bodies.
enum Flow {
    Normal,
    Break,
    Continue,
}

impl<'p> Renderer<'p> {
    pub fn new(provider: &'p dyn DataProvider, opts: Options) -> Self {
        Renderer {
            provider,
            opts,
            scopes: vec![Scope { vars: OrderedMap::new() }],
            counters: OrderedMap::new(),
            include_depth: 0,
        }
    }

    /// Seed the top-level scope with page/include/etc. context values.
    pub fn set_global(&mut self, name: &str, val: Value) {
        self.scopes[0].vars.insert(name, val);
    }

    pub fn render(&mut self, tpl: &Template) -> String {
        let mut out = String::new();
        self.render_block(tpl, &mut out);
        out
    }

    fn render_block(&mut self, tpl: &Template, out: &mut String) -> Flow {
        for node in tpl {
            match self.render_node(node, out) {
                Flow::Normal => {}
                other => return other,
            }
        }
        Flow::Normal
    }

    fn render_node(&mut self, node: &Node, out: &mut String) -> Flow {
        match node {
            Node::Raw(s) => out.push_str(s),
            Node::Raw2(s) => {
                if self.opts.publisher_raw_quirk {
                    // Publisher quirk: re-parse & evaluate the raw body.
                    if let Ok(sub) = crate::parser::parse(s) {
                        self.render_block(&sub, out);
                    } else {
                        out.push_str(s);
                    }
                } else {
                    out.push_str(s);
                }
            }
            Node::Comment => {}
            Node::Output(expr) => {
                let v = self.eval_expr(expr);
                out.push_str(&v.to_output_string());
            }
            Node::Assign { name, expr } => {
                let v = self.eval_expr(expr);
                self.assign(name, v);
            }
            Node::Capture { name, body } => {
                let mut buf = String::new();
                self.render_block(body, &mut buf);
                self.assign(name, Value::str(buf));
            }
            Node::Increment(name) => {
                let cur = self.counters.get(name).map(|v| v.to_integer()).unwrap_or(0);
                out.push_str(&cur.to_string());
                self.counters.insert(name.clone(), Value::Int(cur + 1));
            }
            Node::Decrement(name) => {
                let cur = self.counters.get(name).map(|v| v.to_integer()).unwrap_or(0) - 1;
                out.push_str(&cur.to_string());
                self.counters.insert(name.clone(), Value::Int(cur));
            }
            Node::If { branches, else_body } => {
                for (cond, body) in branches {
                    if self.eval_condition(cond) {
                        return self.render_block(body, out);
                    }
                }
                if let Some(eb) = else_body {
                    return self.render_block(eb, out);
                }
            }
            Node::For { .. } => return self.render_for(node, out),
            Node::Break => return Flow::Break,
            Node::Continue => return Flow::Continue,
            Node::Include { name, params } => self.render_include(name, params, out),
            Node::UnknownTag { .. } => { /* passthrough: emit nothing */ }
        }
        Flow::Normal
    }

    fn render_for(&mut self, node: &Node, out: &mut String) -> Flow {
        let Node::For {
            var,
            iterable,
            reversed,
            offset,
            limit,
            body,
            else_body,
        } = node
        else {
            return Flow::Normal;
        };

        let mut items = self.eval_iterable(iterable);
        if *reversed {
            items.reverse();
        }
        // offset / limit (Liquid applies offset then limit)
        if let Some(off) = offset {
            let n = self.eval_expr(off).to_integer().max(0) as usize;
            if n < items.len() {
                items = items.split_off(n);
            } else {
                items.clear();
            }
        }
        if let Some(lim) = limit {
            let n = self.eval_expr(lim).to_integer().max(0) as usize;
            items.truncate(n);
        }

        if items.is_empty() {
            if let Some(eb) = else_body {
                return self.render_block(eb, out);
            }
            return Flow::Normal;
        }

        let len = items.len();
        self.push_scope();
        for (i, item) in items.into_iter().enumerate() {
            self.set_local(var, item);
            self.set_local("forloop", forloop_value(i, len));
            match self.render_block(body, out) {
                Flow::Break => break,
                Flow::Continue | Flow::Normal => {}
            }
        }
        self.pop_scope();
        Flow::Normal
    }

    fn render_include(&mut self, name: &IncludeName, params: &[(String, Expr)], out: &mut String) {
        // Resolve the include name (dynamic names are rendered first).
        let resolved_name = match name {
            IncludeName::Literal(s) => s.clone(),
            IncludeName::Dynamic(src) => {
                // src like `{{ path }}.md` — render as a mini-template.
                match crate::parser::parse(src) {
                    Ok(tpl) => {
                        let mut buf = String::new();
                        self.render_block(&tpl, &mut buf);
                        buf
                    }
                    Err(_) => return,
                }
            }
        };

        // Evaluate params -> the `include` hash.
        let mut inc = OrderedMap::new();
        for (k, expr) in params {
            inc.insert(k.clone(), self.eval_expr(expr));
        }

        let Some(src) = self.provider.include_source(&resolved_name) else {
            // include-not-found: emit nothing (host decides policy). Jekyll
            // would raise; the plan wires a real provider so a miss here means
            // an unmodeled artifact — silent by default, host can override.
            return;
        };

        if self.include_depth > 64 {
            return; // guard against runaway recursion
        }
        let Ok(tpl) = crate::parser::parse(&src) else {
            return;
        };
        // Includes get a fresh scope with `include.*`; parent variables are
        // still visible (Jekyll includes inherit the outer scope).
        self.push_scope();
        self.set_local("include", Value::Hash(Rc::new(inc)));
        self.include_depth += 1;
        self.render_block(&tpl, out);
        self.include_depth -= 1;
        self.pop_scope();
    }

    // ------------------------------------------------------------- scoping

    fn push_scope(&mut self) {
        self.scopes.push(Scope { vars: OrderedMap::new() });
    }
    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// `set_local` writes into the CURRENT (top) scope.
    fn set_local(&mut self, name: &str, val: Value) {
        self.scopes.last_mut().unwrap().vars.insert(name, val);
    }

    /// `assign` writes to the OUTERMOST scope in Liquid (assigns escape
    /// for/if blocks and persist). Liquid's Context assigns to the root
    /// environment. We mirror that: assign mutates scope[0].
    fn assign(&mut self, name: &str, val: Value) {
        self.scopes[0].vars.insert(name, val);
    }

    fn lookup(&self, name: &str) -> Option<Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.vars.get(name) {
                return Some(v.clone());
            }
        }
        None
    }

    // ------------------------------------------------------------- eval

    fn eval_iterable(&mut self, expr: &Expr) -> Vec<Value> {
        // A range term produces integers a..=b.
        if let Term::Range(a, b) = &expr.base {
            let lo = self.eval_term(a).to_integer();
            let hi = self.eval_term(b).to_integer();
            let mut v: Vec<Value> = if lo <= hi {
                (lo..=hi).map(Value::Int).collect()
            } else {
                vec![]
            };
            // filters can still apply after a range (rare)
            let val = self.apply_filters(Value::array(std::mem::take(&mut v)), &expr.filters);
            return as_iter(&val);
        }
        let val = self.eval_expr(expr);
        as_iter(&val)
    }

    fn eval_expr(&mut self, expr: &Expr) -> Value {
        let base = self.eval_term(&expr.base);
        self.apply_filters(base, &expr.filters)
    }

    fn apply_filters(&mut self, mut val: Value, fs: &[FilterCall]) -> Value {
        for f in fs {
            // Expression-based Jekyll filters need the condition evaluator +
            // scope, so they are handled here rather than in the pure filter
            // table. (jekyll filters.rb: where_exp/find_exp bind `variable` to
            // each item and evaluate a Liquid condition.)
            match f.name.as_str() {
                "where_exp" => {
                    val = self.where_exp(val, &f.args);
                    continue;
                }
                "find_exp" => {
                    val = self.find_exp(val, &f.args);
                    continue;
                }
                _ => {}
            }
            let args: Vec<Value> = f.args.iter().map(|t| self.eval_term(t)).collect();
            let named: Vec<(String, Value)> =
                f.named.iter().map(|(k, t)| (k.clone(), self.eval_term(t))).collect();
            val = filters::apply(&f.name, val, &args, &named);
        }
        val
    }

    /// Jekyll `where_exp: "var", "expr"` — keep items for which the Liquid
    /// condition `expr` (with `var` bound to the item) is truthy.
    fn where_exp(&mut self, input: Value, args: &[Term]) -> Value {
        let (Some(var), Some(expr_src)) = (args.first(), args.get(1)) else {
            return input;
        };
        let var = self.eval_term(var).to_str();
        let expr_src = self.eval_term(expr_src).to_str();
        let Ok(cond) = crate::parser::parse_condition(&expr_src) else {
            return input;
        };
        let items = as_iter(&input);
        self.push_scope();
        let mut out = Vec::new();
        for item in items {
            self.set_local(&var, item.clone());
            if self.eval_condition(&cond) {
                out.push(item);
            }
        }
        self.pop_scope();
        Value::array(out)
    }

    /// Jekyll `find_exp: "var", "expr"` — first item matching the condition.
    fn find_exp(&mut self, input: Value, args: &[Term]) -> Value {
        let (Some(var), Some(expr_src)) = (args.first(), args.get(1)) else {
            return Value::Nil;
        };
        let var = self.eval_term(var).to_str();
        let expr_src = self.eval_term(expr_src).to_str();
        let Ok(cond) = crate::parser::parse_condition(&expr_src) else {
            return Value::Nil;
        };
        let items = as_iter(&input);
        self.push_scope();
        let mut found = Value::Nil;
        for item in items {
            self.set_local(&var, item.clone());
            if self.eval_condition(&cond) {
                found = item;
                break;
            }
        }
        self.pop_scope();
        found
    }

    fn eval_term(&mut self, term: &Term) -> Value {
        match term {
            Term::Literal(v) => v.clone(),
            Term::Range(a, b) => {
                let lo = self.eval_term(a).to_integer();
                let hi = self.eval_term(b).to_integer();
                if lo <= hi {
                    Value::array((lo..=hi).map(Value::Int).collect())
                } else {
                    Value::array(vec![])
                }
            }
            Term::Var(path) => self.eval_var(path),
        }
    }

    fn eval_var(&mut self, path: &VarPath) -> Value {
        // Special roots. NOTE: `empty`/`blank` are NOT special here — they are
        // only sentinels inside comparisons (handled via is_empty_sentinel on
        // the Expr). As a plain variable/iterable, `empty` is a normal lookup,
        // matching Jekyll (`{% for x in empty %}` iterates a var named empty).
        match path.root.as_str() {
            "site" => return self.eval_site(&path.segments),
            "forloop" => {
                if let Some(v) = self.lookup("forloop") {
                    return self.walk_segments(v, &path.segments);
                }
                return Value::Nil;
            }
            _ => {}
        }

        // counters (increment/decrement share the variable namespace in Liquid)
        let root_val = if let Some(v) = self.lookup(&path.root) {
            v
        } else if let Some(c) = self.counters.get(&path.root) {
            c.clone()
        } else {
            Value::Nil
        };
        self.walk_segments(root_val, &path.segments)
    }

    fn eval_site(&mut self, segments: &[Segment]) -> Value {
        // site.data.<...> is served by the provider; other site.* too.
        if let Some(Segment::Field(first)) = segments.first() {
            if first == "data" {
                // collect the remaining path as strings (dynamic indexes resolved)
                let rest = self.resolve_path_strings(&segments[1..]);
                let refs: Vec<&str> = rest.iter().map(|s| s.as_str()).collect();
                return self.provider.site_data(&refs).unwrap_or(Value::Nil);
            }
        }
        let rest = self.resolve_path_strings(segments);
        let refs: Vec<&str> = rest.iter().map(|s| s.as_str()).collect();
        self.provider.site(&refs).unwrap_or(Value::Nil)
    }

    /// Resolve a segment path into a vec of string keys, evaluating dynamic
    /// `[expr]` indexes.
    fn resolve_path_strings(&mut self, segments: &[Segment]) -> Vec<String> {
        let mut out = Vec::new();
        for seg in segments {
            match seg {
                Segment::Field(f) => out.push(f.clone()),
                Segment::Index(e) => out.push(self.eval_expr(e).to_str()),
            }
        }
        out
    }

    fn walk_segments(&mut self, mut val: Value, segments: &[Segment]) -> Value {
        for seg in segments {
            let key = match seg {
                Segment::Field(f) => Value::str(f.clone()),
                Segment::Index(e) => self.eval_expr(e),
            };
            val = val.index(&key);
        }
        val
    }

    // ------------------------------------------------------------- conditions

    fn eval_condition(&mut self, cond: &Condition) -> bool {
        match cond {
            Condition::Truthy(e) => self.eval_expr(e).is_truthy(),
            Condition::NotTruthy(c) => !self.eval_condition(c),
            Condition::And(a, b) => self.eval_condition(a) && self.eval_condition(b),
            Condition::Or(a, b) => self.eval_condition(a) || self.eval_condition(b),
            Condition::Comparison { left, op, right } => {
                let l = self.eval_expr(left);
                let r = self.eval_expr(right);
                self.compare(&l, *op, &r, left, right)
            }
        }
    }

    fn compare(&self, l: &Value, op: CompareOp, r: &Value, le: &Expr, re: &Expr) -> bool {
        use std::cmp::Ordering;
        // `empty`/`blank` sentinels (Liquid::Condition.equal_variables):
        //  * `x == empty` -> x.respond_to?(:empty?) && x.empty?  (String/Array/
        //    Hash respond; nil/false/numbers don't -> false).
        //  * `x == blank` -> x.respond_to?(:blank?) && x.blank?. In Jekyll's
        //    plain Liquid NOTHING defines blank? (no ActiveSupport), so it is
        //    ALWAYS false. Verified via oracle. We honor that exactly.
        let l_sent = sentinel_kind(le);
        let r_sent = sentinel_kind(re);
        match op {
            CompareOp::Eq => {
                if let Some(s) = r_sent {
                    return sentinel_eq(l, s);
                }
                if let Some(s) = l_sent {
                    return sentinel_eq(r, s);
                }
                l.liquid_eq(r)
            }
            CompareOp::Ne => {
                if let Some(s) = r_sent {
                    return !sentinel_eq(l, s);
                }
                if let Some(s) = l_sent {
                    return !sentinel_eq(r, s);
                }
                !l.liquid_eq(r)
            }
            CompareOp::Contains => l.liquid_contains(r),
            CompareOp::Lt => matches!(l.liquid_cmp(r), Some(Ordering::Less)),
            CompareOp::Gt => matches!(l.liquid_cmp(r), Some(Ordering::Greater)),
            CompareOp::Le => matches!(l.liquid_cmp(r), Some(Ordering::Less | Ordering::Equal)),
            CompareOp::Ge => matches!(l.liquid_cmp(r), Some(Ordering::Greater | Ordering::Equal)),
        }
    }
}

#[derive(Clone, Copy)]
enum Sentinel {
    Empty,
    Blank,
}

/// Detect a bare `empty` / `blank` literal on one side of a comparison.
fn sentinel_kind(e: &Expr) -> Option<Sentinel> {
    if !e.filters.is_empty() {
        return None;
    }
    if let Term::Var(v) = &e.base {
        if v.segments.is_empty() {
            return match v.root.as_str() {
                "empty" => Some(Sentinel::Empty),
                "blank" => Some(Sentinel::Blank),
                _ => None,
            };
        }
    }
    None
}

fn sentinel_eq(v: &Value, s: Sentinel) -> bool {
    match s {
        Sentinel::Empty => value_is_empty(v),
        // Jekyll's plain Liquid: nothing responds to blank? -> always false.
        Sentinel::Blank => false,
    }
}

/// Liquid `== empty` / `== blank` semantics (Liquid::Utils / MethodLiteral):
/// a value equals `empty` iff it is an empty string, array or hash. **nil does
/// NOT equal empty** (verified via oracle: `nil == empty` is false). `blank`
/// additionally treats whitespace-only strings and false as blank, but the
/// corpus only ever compares strings/arrays, where empty==blank.
fn value_is_empty(v: &Value) -> bool {
    match v {
        Value::Str(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Hash(h) => h.is_empty(),
        _ => false,
    }
}

/// Build the `forloop` drop for iteration i of length len.
/// (liquid-4.0.4/lib/liquid/tags/for.rb — forloop attributes.)
fn forloop_value(i: usize, len: usize) -> Value {
    let mut m = OrderedMap::new();
    m.insert("index", Value::Int((i + 1) as i64));
    m.insert("index0", Value::Int(i as i64));
    m.insert("rindex", Value::Int((len - i) as i64));
    m.insert("rindex0", Value::Int((len - i - 1) as i64));
    m.insert("first", Value::Bool(i == 0));
    m.insert("last", Value::Bool(i + 1 == len));
    m.insert("length", Value::Int(len as i64));
    Value::Hash(Rc::new(m))
}

/// Coerce a value to an iterable for `{% for %}`.
/// Arrays iterate elements; hashes iterate [k,v] pairs (Ruby Hash#each);
/// strings iterate as a single-element list (Liquid does NOT iterate string
/// chars in `for`); nil -> empty.
fn as_iter(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.as_ref().clone(),
        Value::Hash(h) => h
            .iter()
            .map(|(k, val)| Value::array(vec![Value::str(k.clone()), val.clone()]))
            .collect(),
        Value::Nil => vec![],
        other => vec![other.clone()],
    }
}
