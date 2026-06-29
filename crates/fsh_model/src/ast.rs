//! Import AST types mirroring `sushi-ts/src/fshtypes/**`, shaped to serialize to
//! the `parse-oracle.cjs` JSON (see `docs/specs/ast-shape.md`). The dumper lives
//! in `fsh_lexer_parser::dump` so this crate stays light.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Location {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceInfo {
    pub file: Option<String>,
    pub location: Option<Location>,
    pub applied_file: Option<String>,
    pub applied_location: Option<Location>,
}

impl SourceInfo {
    pub fn new(file: &str, loc: Location) -> Self {
        SourceInfo {
            file: Some(file.to_string()),
            location: Some(loc),
            applied_file: None,
            applied_location: None,
        }
    }
}

// ---------- value types ----------

#[derive(Clone, Debug, PartialEq)]
pub struct FshCode {
    pub source_info: SourceInfo,
    pub code: String,
    pub system: Option<String>,
    pub display: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FshQuantity {
    pub source_info: SourceInfo,
    pub value: Option<f64>,
    pub unit: Option<FshCode>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FshRatio {
    pub source_info: SourceInfo,
    pub numerator: FshQuantity,
    pub denominator: FshQuantity,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FshReference {
    pub source_info: SourceInfo,
    pub reference: String,
    pub display: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FshCanonical {
    pub source_info: SourceInfo,
    pub entity_name: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    /// Arbitrary-precision integer, stored as a decimal string -> {"__bigint"}.
    BigInt(String),
    Float(f64),
    Str(String),
    Code(FshCode),
    Quantity(FshQuantity),
    Ratio(Box<FshRatio>),
    Reference(FshReference),
    Canonical(FshCanonical),
}

// ---------- rule subtypes ----------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OnlyRuleType {
    pub type_: String,
    pub is_reference: bool,
    pub is_canonical: bool,
    pub is_codeable_reference: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContainsRuleItem {
    pub name: String,
    pub type_: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Flags {
    pub must_support: bool,
    pub summary: bool,
    pub modifier: bool,
    pub trial_use: bool,
    pub normative: bool,
    pub draft: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValueSetComponentFrom {
    pub system: Option<String>,
    pub value_sets: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValueSetFilter {
    pub property: String,
    pub operator: String,
    pub value: FilterValue,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FilterValue {
    Code(FshCode),
    Str(String),
    Bool(bool),
    Regex(String),
}

// ---------- rules ----------

#[derive(Clone, Debug, PartialEq)]
pub enum Rule {
    Card {
        source_info: SourceInfo,
        path: String,
        min: Option<i64>,
        max: String,
    },
    Flag {
        source_info: SourceInfo,
        path: String,
        flags: Flags,
    },
    Binding {
        source_info: SourceInfo,
        path: String,
        value_set: String,
        strength: String,
    },
    Assignment {
        source_info: SourceInfo,
        path: String,
        value: Option<Value>,
        raw_value: Option<String>,
        exactly: bool,
        is_instance: bool,
    },
    Only {
        source_info: SourceInfo,
        path: String,
        types: Vec<OnlyRuleType>,
    },
    Contains {
        source_info: SourceInfo,
        path: String,
        items: Vec<ContainsRuleItem>,
    },
    CaretValue {
        source_info: SourceInfo,
        path: String,
        caret_path: Option<String>,
        value: Option<Value>,
        raw_value: Option<String>,
        is_instance: bool,
        is_code_caret_rule: bool,
        path_array: Vec<String>,
    },
    Obeys {
        source_info: SourceInfo,
        path: String,
        invariant: String,
    },
    Insert {
        source_info: SourceInfo,
        path: String,
        path_array: Vec<String>,
        params: Vec<String>,
        rule_set: String,
    },
    Path {
        source_info: SourceInfo,
        path: String,
    },
    Concept {
        source_info: SourceInfo,
        path: String,
        code: String,
        display: Option<String>,
        definition: Option<String>,
        system: Option<String>,
        hierarchy: Vec<String>,
    },
    Mapping {
        source_info: SourceInfo,
        path: String,
        map: String,
        comment: Option<String>,
        language: Option<FshCode>,
    },
    AddElement {
        source_info: SourceInfo,
        path: String,
        min: Option<i64>,
        max: String,
        flags: Flags,
        types: Vec<OnlyRuleType>,
        content_reference: Option<String>,
        short: Option<String>,
        definition: Option<String>,
    },
    VsConcept {
        source_info: SourceInfo,
        path: String,
        inclusion: bool,
        from: ValueSetComponentFrom,
        concepts: Vec<FshCode>,
    },
    VsFilter {
        source_info: SourceInfo,
        path: String,
        inclusion: bool,
        from: ValueSetComponentFrom,
        filters: Vec<ValueSetFilter>,
    },
}

impl Rule {
    /// Mutable access to the rule's `sourceInfo` (every variant has one).
    pub fn source_info_mut(&mut self) -> &mut SourceInfo {
        match self {
            Rule::Card { source_info, .. }
            | Rule::Flag { source_info, .. }
            | Rule::Binding { source_info, .. }
            | Rule::Assignment { source_info, .. }
            | Rule::Only { source_info, .. }
            | Rule::Contains { source_info, .. }
            | Rule::CaretValue { source_info, .. }
            | Rule::Obeys { source_info, .. }
            | Rule::Insert { source_info, .. }
            | Rule::Path { source_info, .. }
            | Rule::Concept { source_info, .. }
            | Rule::Mapping { source_info, .. }
            | Rule::AddElement { source_info, .. }
            | Rule::VsConcept { source_info, .. }
            | Rule::VsFilter { source_info, .. } => source_info,
        }
    }

    /// The rule's dotted `path` (every variant has one).
    pub fn path(&self) -> &str {
        match self {
            Rule::Card { path, .. }
            | Rule::Flag { path, .. }
            | Rule::Binding { path, .. }
            | Rule::Assignment { path, .. }
            | Rule::Only { path, .. }
            | Rule::Contains { path, .. }
            | Rule::CaretValue { path, .. }
            | Rule::Obeys { path, .. }
            | Rule::Insert { path, .. }
            | Rule::Path { path, .. }
            | Rule::Concept { path, .. }
            | Rule::Mapping { path, .. }
            | Rule::AddElement { path, .. }
            | Rule::VsConcept { path, .. }
            | Rule::VsFilter { path, .. } => path,
        }
    }

    pub fn set_path(&mut self, p: String) {
        match self {
            Rule::Card { path, .. }
            | Rule::Flag { path, .. }
            | Rule::Binding { path, .. }
            | Rule::Assignment { path, .. }
            | Rule::Only { path, .. }
            | Rule::Contains { path, .. }
            | Rule::CaretValue { path, .. }
            | Rule::Obeys { path, .. }
            | Rule::Insert { path, .. }
            | Rule::Path { path, .. }
            | Rule::Concept { path, .. }
            | Rule::Mapping { path, .. }
            | Rule::AddElement { path, .. }
            | Rule::VsConcept { path, .. }
            | Rule::VsFilter { path, .. } => *path = p,
        }
    }

    pub fn is_insert(&self) -> bool {
        matches!(self, Rule::Insert { .. })
    }

    /// Mirrors TS `rule.constructorName`, used in diagnostic messages.
    pub fn constructor_name(&self) -> &'static str {
        match self {
            Rule::Card { .. } => "CardRule",
            Rule::Flag { .. } => "FlagRule",
            Rule::Binding { .. } => "BindingRule",
            Rule::Assignment { .. } => "AssignmentRule",
            Rule::Only { .. } => "OnlyRule",
            Rule::Contains { .. } => "ContainsRule",
            Rule::CaretValue { .. } => "CaretValueRule",
            Rule::Obeys { .. } => "ObeysRule",
            Rule::Insert { .. } => "InsertRule",
            Rule::Path { .. } => "PathRule",
            Rule::Concept { .. } => "ConceptRule",
            Rule::Mapping { .. } => "MappingRule",
            Rule::AddElement { .. } => "AddElementRule",
            Rule::VsConcept { .. } => "ValueSetConceptComponentRule",
            Rule::VsFilter { .. } => "ValueSetFilterComponentRule",
        }
    }
}

// ---------- entities ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructureKind {
    Profile,
    Extension,
    Logical,
    Resource,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExtensionContext {
    pub value: String,
    pub is_quoted: bool,
    pub source_info: SourceInfo,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StructureDef {
    pub kind: StructureKind,
    pub source_info: SourceInfo,
    pub name: String,
    pub id: String,
    pub parent: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub rules: Vec<Rule>,
    pub contexts: Vec<ExtensionContext>,
    pub characteristics: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    pub source_info: SourceInfo,
    pub name: String,
    pub id: String,
    pub instance_of: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub usage: String,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FshValueSet {
    pub source_info: SourceInfo,
    pub name: String,
    pub id: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FshCodeSystem {
    pub source_info: SourceInfo,
    pub name: String,
    pub id: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Invariant {
    pub source_info: SourceInfo,
    pub name: String,
    pub description: Option<String>,
    pub expression: Option<String>,
    pub xpath: Option<String>,
    pub severity: Option<FshCode>,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuleSet {
    pub source_info: SourceInfo,
    pub name: String,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamRuleSet {
    pub source_info: SourceInfo,
    pub name: String,
    pub parameters: Vec<String>,
    pub contents: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mapping {
    pub source_info: SourceInfo,
    pub name: String,
    pub id: String,
    pub source: Option<String>,
    pub target: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FshDocument {
    pub file: String,
    pub aliases: Vec<(String, String)>,
    pub profiles: Vec<(String, StructureDef)>,
    pub extensions: Vec<(String, StructureDef)>,
    pub resources: Vec<(String, StructureDef)>,
    pub logicals: Vec<(String, StructureDef)>,
    pub instances: Vec<(String, Instance)>,
    pub value_sets: Vec<(String, FshValueSet)>,
    pub code_systems: Vec<(String, FshCodeSystem)>,
    pub invariants: Vec<(String, Invariant)>,
    pub rule_sets: Vec<(String, RuleSet)>,
    pub applied_rule_sets: Vec<(String, RuleSet)>,
    pub mappings: Vec<(String, Mapping)>,
}

impl FshDocument {
    pub fn new(file: &str) -> Self {
        FshDocument {
            file: file.to_string(),
            ..Default::default()
        }
    }
}
