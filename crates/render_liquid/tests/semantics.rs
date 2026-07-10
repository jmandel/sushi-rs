//! Unit tests for the load-bearing Liquid semantics that the differential gate
//! pinned against Jekyll 4.4.1 / Liquid 4.0.4. Each asserts a behavior that was
//! verified via `scripts/liquid-oracle.rb` (cited in-code). These run in plain
//! `cargo test` (no ruby needed); the FULL byte-parity gate lives in
//! `scripts/liquid-diff.sh` (fixtures + corpus).

use render_liquid::{render_with, DataProvider, JsonProvider, Options, OrderedMap, Value};
use std::rc::Rc;

fn hash(pairs: &[(&str, Value)]) -> Value {
    let mut m = OrderedMap::new();
    for (k, v) in pairs {
        m.insert(*k, v.clone());
    }
    Value::Hash(Rc::new(m))
}

fn arr(v: &[Value]) -> Value {
    Value::array(v.to_vec())
}

/// Render with a bare provider (no site.data), given globals.
fn r(src: &str, globals: &[(&str, Value)]) -> String {
    let p = JsonProvider::new();
    render_with(src, &p, globals, Options::default())
}

#[test]
fn string_filters() {
    assert_eq!(r("{{ 'Hi There' | upcase }}", &[]), "HI THERE");
    assert_eq!(r("{{ 'Hi There' | downcase }}", &[]), "hi there");
    assert_eq!(r("{{ 'hi there' | capitalize }}", &[]), "Hi there");
    assert_eq!(r("{{ 'a.b.c' | replace: '.', '-' }}", &[]), "a-b-c");
    assert_eq!(r("{{ 'a.b.c' | replace_first: '.', '-' }}", &[]), "a-b.c");
    assert_eq!(r("{{ 'x' | append: 'y' | prepend: 'z' }}", &[]), "zxy");
    assert_eq!(r("{{ 'a,b,c' | split: ',' | join: '-' }}", &[]), "a-b-c");
}

#[test]
fn trim_is_not_a_filter() {
    // Jekyll/Liquid 4.0.4 has NO `trim` filter; strict_filters=false ->
    // passthrough (verified via oracle).
    assert_eq!(r("{{ '  hi  ' | trim }}", &[]), "  hi  ");
    // but strip IS a filter
    assert_eq!(r("{{ '  hi  ' | strip }}", &[]), "hi");
}

#[test]
fn liquid_truthiness() {
    // Only nil and false are falsy; 0 and "" are truthy.
    assert_eq!(
        r("{% if z %}T{% else %}F{% endif %}", &[("z", Value::Int(0))]),
        "T"
    );
    assert_eq!(
        r(
            "{% if z %}T{% else %}F{% endif %}",
            &[("z", Value::str(""))]
        ),
        "T"
    );
    assert_eq!(
        r("{% if z %}T{% else %}F{% endif %}", &[("z", Value::Nil)]),
        "F"
    );
    assert_eq!(
        r(
            "{% if z %}T{% else %}F{% endif %}",
            &[("z", Value::Bool(false))]
        ),
        "F"
    );
}

#[test]
fn empty_vs_blank_sentinels() {
    // `"" == empty` is true; `"" == blank` is FALSE (Jekyll's plain Liquid has
    // no blank? on strings). `nil == empty` is false. (Verified via oracle.)
    assert_eq!(
        r(
            "{% if s == empty %}Y{% else %}N{% endif %}",
            &[("s", Value::str(""))]
        ),
        "Y"
    );
    assert_eq!(
        r(
            "{% if s == blank %}Y{% else %}N{% endif %}",
            &[("s", Value::str(""))]
        ),
        "N"
    );
    assert_eq!(
        r(
            "{% if m == empty %}Y{% else %}N{% endif %}",
            &[("m", Value::Nil)]
        ),
        "N"
    );
    assert_eq!(
        r(
            "{% if a == empty %}Y{% else %}N{% endif %}",
            &[("a", arr(&[]))]
        ),
        "Y"
    );
}

#[test]
fn blank_block_emits_nothing() {
    // A block whose whole body is blank (whitespace + assign) emits NOTHING
    // (Liquid block_body.rb skip_output). Verified via oracle.
    let out = r(
        "A\n{% if t %}\n  {% assign x = 1 %}\n{% endif -%}\nB",
        &[("t", Value::Bool(true))],
    );
    assert_eq!(out, "A\nB");
}

#[test]
fn for_loop_and_forloop() {
    let list = arr(&[Value::str("a"), Value::str("b"), Value::str("c")]);
    assert_eq!(
        r("{% for x in l %}{{ forloop.index }}:{{ x }}{% unless forloop.last %},{% endunless %}{% endfor %}",
          &[("l", list.clone())]),
        "1:a,2:b,3:c"
    );
    // offset + limit
    let nums = arr(&(1..=5).map(Value::Int).collect::<Vec<_>>());
    assert_eq!(
        r(
            "{% for x in l offset:1 limit:2 %}{{x}}{% endfor %}",
            &[("l", nums.clone())]
        ),
        "23"
    );
    // reversed
    assert_eq!(
        r("{% for x in l reversed %}{{x}}{% endfor %}", &[("l", nums)]),
        "54321"
    );
    // range
    assert_eq!(r("{% for i in (1..3) %}{{i}}{% endfor %}", &[]), "123");
}

#[test]
fn break_and_continue() {
    let l = arr(&(1..=4).map(Value::Int).collect::<Vec<_>>());
    assert_eq!(
        r(
            "{% for x in l %}{% if x == 3 %}{% break %}{% endif %}{{x}}{% endfor %}",
            &[("l", l.clone())]
        ),
        "12"
    );
    assert_eq!(
        r(
            "{% for x in l %}{% if x == 2 %}{% continue %}{% endif %}{{x}}{% endfor %}",
            &[("l", l)]
        ),
        "134"
    );
}

#[test]
fn if_operators_and_boolean() {
    assert_eq!(r("{% if 2 > 1 and 1 < 2 %}Y{% endif %}", &[]), "Y");
    assert_eq!(r("{% if false or true %}Y{% endif %}", &[]), "Y");
    assert_eq!(r("{% if 'hello' contains 'ell' %}Y{% endif %}", &[]), "Y");
    let l = arr(&[Value::Int(1), Value::Int(2)]);
    assert_eq!(r("{% if l contains 2 %}Y{% endif %}", &[("l", l)]), "Y");
    // .size operand + elsif
    let a = arr(&[Value::str("x"), Value::str("y"), Value::str("z")]);
    assert_eq!(
        r(
            "{% if a.size > 2 %}big{% elsif a.size == 2 %}two{% else %}small{% endif %}",
            &[("a", a)]
        ),
        "big"
    );
}

#[test]
fn unless_with_contains_is_negated_correctly() {
    // Regression: `unless X contains "!"` must NOT be mis-inverted into
    // `if X contains "!"` (US Core screening-and-assessments).
    assert_eq!(
        r(
            "{% unless s contains '!' %}Y{% else %}N{% endunless %}",
            &[("s", Value::str("clean"))]
        ),
        "Y"
    );
    assert_eq!(
        r(
            "{% unless s contains '!' %}Y{% else %}N{% endunless %}",
            &[("s", Value::str("bad!"))]
        ),
        "N"
    );
}

#[test]
fn case_when() {
    assert_eq!(
        r(
            "{% case x %}{% when 'a' %}A{% when 'b' %}B{% else %}O{% endcase %}",
            &[("x", Value::str("b"))]
        ),
        "B"
    );
    // comma / or separated
    assert_eq!(
        r(
            "{% case x %}{% when 'a', 'b' %}AB{% else %}O{% endcase %}",
            &[("x", Value::str("b"))]
        ),
        "AB"
    );
}

#[test]
fn where_map_sort_uniq() {
    let rows = arr(&[
        hash(&[("a", Value::str("1")), ("b", Value::str("x"))]),
        hash(&[("a", Value::str("2")), ("b", Value::str("y"))]),
        hash(&[("a", Value::str("3")), ("b", Value::str("x"))]),
    ]);
    assert_eq!(
        r(
            "{% assign m = rows | where: 'b', 'x' | first %}{{ m.a }}",
            &[("rows", rows.clone())]
        ),
        "1"
    );
    assert_eq!(
        r(
            "{{ rows | where: 'b','x' | size }}",
            &[("rows", rows.clone())]
        ),
        "2"
    );
    assert_eq!(
        r("{{ rows | map: 'a' | join: ',' }}", &[("rows", rows)]),
        "1,2,3"
    );
    let dups = arr(&[
        Value::str("b"),
        Value::str("a"),
        Value::str("b"),
        Value::str("c"),
    ]);
    assert_eq!(
        r("{{ d | uniq | join: ',' }}", &[("d", dups.clone())]),
        "b,a,c"
    );
    assert_eq!(r("{{ d | sort | join: ',' }}", &[("d", dups)]), "a,b,b,c");
}

#[test]
fn sort_by_property_nils_first() {
    // Jekyll sort default = nils first (verified via oracle: [nil][A][B]).
    let rows = arr(&[
        hash(&[("t", Value::str("B"))]),
        hash(&[("t", Value::Nil)]),
        hash(&[("t", Value::str("A"))]),
    ]);
    assert_eq!(
        r("{% assign s = rows | sort: 't' %}{% for x in s %}[{{x.t | default: 'NIL'}}]{% endfor %}", &[("rows", rows)]),
        "[NIL][A][B]"
    );
}

#[test]
fn group_by_shape() {
    let rows = arr(&[
        hash(&[("c", Value::str("x")), ("n", Value::str("1"))]),
        hash(&[("c", Value::str("y")), ("n", Value::str("2"))]),
        hash(&[("c", Value::str("x")), ("n", Value::str("3"))]),
    ]);
    // group_by preserves first-seen key order; each group is {name, items, size}
    assert_eq!(
        r("{% assign g = rows | group_by: 'c' %}{% for grp in g %}{{ grp.name }}={{ grp.size }} {% endfor %}",
          &[("rows", rows)]),
        "x=2 y=1 "
    );
}

#[test]
fn raw_verbatim_by_default() {
    assert_eq!(
        r("X{% raw %}{% include foo %}{{ bar }}{% endraw %}Y", &[]),
        "X{% include foo %}{{ bar }}Y"
    );
}

#[test]
fn raw_preserves_exact_spacing() {
    // Regression (smart/app-launch): raw must preserve the ORIGINAL inner bytes,
    // e.g. `{{access_token}}` (no spaces) must NOT become `{{ access_token }}`.
    assert_eq!(
        r(
            "X{% raw %}{{access_token}} {%if y%}z{%endif%}{% endraw %}Y",
            &[]
        ),
        "X{{access_token}} {%if y%}z{%endif%}Y"
    );
}

#[test]
fn raw_publisher_quirk_evaluates_inner() {
    // With publisher_raw_quirk, the raw body is re-parsed & evaluated (the
    // Java Publisher wart, survey nasty #4).
    let p = JsonProvider::new();
    let out = render_with(
        "X{% raw %}{{ bar }}{% endraw %}Y",
        &p,
        &[("bar", Value::str("Z"))],
        Options {
            publisher_raw_quirk: true,
            ..Options::default()
        },
    );
    assert_eq!(out, "XZY");
}

#[test]
fn assign_capture_comment() {
    assert_eq!(r("{% assign x = 'v' %}{{ x }}", &[]), "v");
    assert_eq!(
        r(
            "{% capture c %}a{{ n }}b{% endcapture %}{{ c }}",
            &[("n", Value::Int(1))]
        ),
        "a1b"
    );
    assert_eq!(r("A{% comment %}hidden{% endcomment %}B", &[]), "AB");
}

#[test]
fn whitespace_control() {
    assert_eq!(r("a  {%- assign z = 1 -%}  b", &[]), "ab");
    assert_eq!(r("a{{- x -}}b", &[("x", Value::str("X"))]), "aXb");
}

#[test]
fn site_data_via_provider() {
    // The engine asks the provider for the FIRST key after `site.data` and
    // walks the remaining path itself, so a provider returns whole subtrees.
    struct P;
    impl DataProvider for P {
        fn site_data(&self, path: &[&str]) -> Option<Value> {
            match path {
                ["fhir"] => {
                    let mut m = OrderedMap::new();
                    m.insert("path", Value::str("http://hl7.org/fhir/R4/"));
                    Some(Value::Hash(Rc::new(m)))
                }
                _ => None,
            }
        }
    }
    let out = render_with("{{ site.data.fhir.path }}", &P, &[], Options::default());
    assert_eq!(out, "http://hl7.org/fhir/R4/");
}

#[test]
fn site_data_deep_mixed_access() {
    // Regression (cdex): `site.data.d.parameter[7].part.[4].x` — array indexes
    // and `.`-before-`[` deep inside a site.data path must resolve with proper
    // typing (int index, not string key). Verified via oracle.
    struct P;
    impl DataProvider for P {
        fn site_data(&self, path: &[&str]) -> Option<Value> {
            if path == ["d"] {
                // {parameter: [ {part:[{x:A}]}, {part:[{x:B}]} ]}
                let mk = |x: &str| {
                    let mut inner = OrderedMap::new();
                    inner.insert("x", Value::str(x));
                    let part = Value::array(vec![Value::Hash(Rc::new(inner))]);
                    let mut m = OrderedMap::new();
                    m.insert("part", part);
                    Value::Hash(Rc::new(m))
                };
                let mut m = OrderedMap::new();
                m.insert("parameter", Value::array(vec![mk("A"), mk("B")]));
                return Some(Value::Hash(Rc::new(m)));
            }
            None
        }
    }
    let out = render_with(
        "{{ site.data.d.parameter[1].part.[0].x }}",
        &P,
        &[],
        Options::default(),
    );
    assert_eq!(out, "B");
}

#[test]
fn parameterized_include_with_include_dot() {
    let mut p = JsonProvider::new();
    p.includes
        .insert("greet.md".into(), "Hello, {{ include.who }}!".into());
    let out = render_with(
        r#"{% include greet.md who="World" %}"#,
        &p,
        &[],
        Options::default(),
    );
    assert_eq!(out, "Hello, World!");
}

#[test]
fn paramless_nested_include_inherits_parent_include_hash() {
    // Jekyll include.rb:120 only reassigns `include` when the tag HAS params;
    // a param-less nested include inherits the parent's include.* (US Core
    // observation_guidance_1 -> obs_cat_guidance reads include.category).
    let mut p = JsonProvider::new();
    p.includes
        .insert("child.md".into(), "[{{ include.category }}]".into());
    p.includes.insert(
        "parent.md".into(),
        "{{ include.category }}{% include child.md %}".into(),
    );
    let out = render_with(
        r#"{% include parent.md category="LAB" %}"#,
        &p,
        &[],
        Options::default(),
    );
    assert_eq!(out, "LAB[LAB]");
}

#[test]
fn dynamic_include_name() {
    let mut p = JsonProvider::new();
    p.includes.insert("v9.md".into(), "NINE".into());
    let out = render_with(
        "{% include {{ target }}.md %}",
        &p,
        &[("target", Value::str("v9"))],
        Options::default(),
    );
    assert_eq!(out, "NINE");
}

#[test]
fn index_with_filter_and_dynamic_key() {
    // `item["title" | trim]` -> item.title (trim no-op). Verified via oracle.
    let item = hash(&[("title", Value::str("T"))]);
    assert_eq!(
        r("{{ item['title' | trim ] }}", &[("item", item.clone())]),
        "T"
    );
    // dynamic key from a variable
    assert_eq!(
        r("{% assign k = 'title' %}{{ item[k] }}", &[("item", item)]),
        "T"
    );
}

#[test]
fn nested_braces_in_tag() {
    // `{% assign x = {{v}} | append: 'y' %}` interpolates {{v}} (Jekyll quirk).
    assert_eq!(
        r(
            "{% assign x = {{ v }} | append: 'y' %}{{ x }}",
            &[("v", Value::str("http://a/"))]
        ),
        "http://a/y"
    );
}

#[test]
fn parenthesized_boolean_grouping() {
    // Ruby Liquid 4.x groups parenthesized boolean expressions:
    // `a and (b or c)` == a && (b || c), NOT (a && b) || c. Verified via the
    // liquid oracle. This drives the US-Core search-requirement handler
    // (`multipleAnd_conf and (shall_comparator or should_comparator)`), where an
    // empty (nil) comparator must not collapse the whole conjunction.
    let t = "{% if a and (b or c) %}Y{% else %}N{% endif %}";
    let tv = Value::str("x");
    // a truthy, b truthy, c nil -> (b or c) true -> Y
    assert_eq!(
        r(
            t,
            &[("a", tv.clone()), ("b", tv.clone()), ("c", Value::Nil)]
        ),
        "Y"
    );
    // a truthy, b nil, c nil -> (b or c) false -> N
    assert_eq!(
        r(
            t,
            &[("a", tv.clone()), ("b", Value::Nil), ("c", Value::Nil)]
        ),
        "N"
    );
    // a nil -> false regardless -> N
    assert_eq!(
        r(t, &[("a", Value::Nil), ("b", tv.clone()), ("c", tv)]),
        "N"
    );
}
