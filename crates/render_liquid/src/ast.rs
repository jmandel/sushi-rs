//! AST for the T1+T2 Liquid subset.

use crate::value::Value;

/// A rendered template is a list of nodes.
pub type Template = Vec<Node>;

#[derive(Clone, Debug)]
pub enum Node {
    Raw(String),
    /// `{{ expr }}`
    Output(Expr),
    Assign {
        name: String,
        expr: Expr,
    },
    Capture {
        name: String,
        body: Template,
    },
    Increment(String),
    Decrement(String),
    /// `{% comment %}...{% endcomment %}` — body discarded.
    Comment,
    /// `{% raw %}...{% endraw %}` — body emitted verbatim.
    Raw2(String),
    If {
        branches: Vec<(Condition, Template)>,
        else_body: Option<Template>,
    },
    /// `{% unless cond %}` — sugar for `if !cond`.
    For {
        var: String,
        iterable: Expr,
        reversed: bool,
        offset: Option<Expr>,
        limit: Option<Expr>,
        body: Template,
        else_body: Option<Template>,
    },
    /// `{% case expr %}{% when a,b %}...{% else %}...{% endcase %}`
    Case {
        subject: Expr,
        /// each `when` arm: a list of candidate values (comma/or separated) +
        /// its body.
        whens: Vec<(Vec<Term>, Template)>,
        else_body: Option<Template>,
    },
    Break,
    Continue,
    /// `{% include name.md k=v %}` (name may be a variable expr).
    Include {
        name: IncludeName,
        params: Vec<(String, Expr)>,
    },
    /// A tag we recognize by name but treat as a passthrough/no-op with a
    /// registry note (e.g. lang-fragment, fragment, sql). Emits nothing by
    /// default; `name`/`markup` are retained so a host can later register a
    /// handler keyed on them (F4/F5 fragment + lang-fragment wiring).
    UnknownTag {
        #[allow(dead_code)]
        name: String,
        #[allow(dead_code)]
        markup: String,
    },
}

#[derive(Clone, Debug)]
pub enum IncludeName {
    /// literal `foo.md`
    Literal(String),
    /// dynamic `{{ path }}.md` — a template fragment that resolves to a name
    Dynamic(String),
}

/// A pipeline expression: a base term followed by filters.
#[derive(Clone, Debug)]
pub struct Expr {
    pub base: Term,
    pub filters: Vec<FilterCall>,
}

#[derive(Clone, Debug)]
pub struct FilterCall {
    pub name: String,
    pub args: Vec<Term>,
    /// keyword args (Liquid `date: "%Y", tz: "x"` style) — rare in corpus.
    pub named: Vec<(String, Term)>,
}

#[derive(Clone, Debug)]
pub enum Term {
    Literal(Value),
    /// variable path: root plus member/index accesses
    Var(VarPath),
    /// `(a..b)` range literal used by `for`
    Range(Box<Term>, Box<Term>),
}

#[derive(Clone, Debug)]
pub struct VarPath {
    pub root: String,
    pub segments: Vec<Segment>,
}

#[derive(Clone, Debug)]
pub enum Segment {
    /// `.name` or `["name"]`
    Field(String),
    /// `[expr]` dynamic index/key. The bracket may contain a FULL expression
    /// with filters, e.g. `item["title" | trim]` (Liquid allows filters inside
    /// index brackets), so it holds an Expr, not a bare Term.
    Index(Expr),
}

/// Boolean condition tree for `if`/`unless`/`elsif`.
#[derive(Clone, Debug)]
pub enum Condition {
    Comparison {
        left: Expr,
        op: CompareOp,
        right: Expr,
    },
    /// bare truthiness of an expression
    Truthy(Expr),
    /// negation of a condition (used to desugar `unless` over non-comparison
    /// conditions).
    NotTruthy(Box<Condition>),
    And(Box<Condition>, Box<Condition>),
    Or(Box<Condition>, Box<Condition>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Contains,
}
