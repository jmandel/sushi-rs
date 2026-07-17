use std::collections::BTreeMap;

use crate::{database::SqlStatementBudget, SqlRuntime};

const MAX_SQL_STATEMENTS_PER_SOURCE: usize = 64;
const MAX_SQL_DIRECTIVES_PER_SITE: usize = 4_096;
const MAX_SQL_RESULT_BYTES_PER_SOURCE: usize = 16 * 1024 * 1024;
const MAX_SQL_GENERATED_BYTES_PER_SITE: usize = 64 * 1024 * 1024;

/// Publisher SQL's complete handoff to the ordinary page renderer.
///
/// Rewritten sources contain ordinary Liquid only. Direct queries become
/// generated raw-wrapped includes; `sqlToData` becomes global `_data` plus an
/// ordinary assignment. The database and its lifecycle end at this boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublisherSqlExpansion {
    pub rewritten_sources: BTreeMap<String, Vec<u8>>,
    pub generated_includes: BTreeMap<String, Vec<u8>>,
    pub generated_data: BTreeMap<String, Vec<u8>>,
}

/// Expand Publisher SQL directives in deterministic source-path order.
///
/// Callers pass only Publisher-eligible HTML, Markdown, or XML sources. A
/// malformed directive leaves its complete source unchanged. Query errors are
/// visible inline and do not abort unrelated pages.
pub fn expand_publisher_sql(
    runtime: &SqlRuntime,
    sources: impl IntoIterator<Item = (String, Vec<u8>)>,
) -> PublisherSqlExpansion {
    let mut parsed = sources
        .into_iter()
        .filter_map(|(name, bytes)| {
            let source = String::from_utf8(bytes).ok()?;
            if !source.contains("{% sql") && !source.contains("{%! sql") {
                return None;
            }
            let directives = parse_directives(&source)?;
            Some((name, source, directives))
        })
        .collect::<Vec<_>>();
    parsed.sort_by(|left, right| left.0.cmp(&right.0));

    let directive_count = parsed
        .iter()
        .map(|(_, _, directives)| directives.iter().filter(|directive| !directive.raw).count())
        .sum::<usize>();
    let site_over_budget = directive_count > MAX_SQL_DIRECTIVES_PER_SITE;

    let mut expansion = PublisherSqlExpansion::default();
    let mut directive_index = 0usize;
    let mut generated_bytes = 0usize;
    let mut site_bytes_exhausted = false;

    for (source_name, source, directives) in parsed {
        let budget = SqlStatementBudget::new(MAX_SQL_STATEMENTS_PER_SOURCE);
        let mut source_result_bytes = 0usize;
        let mut output = String::with_capacity(source.len());
        let mut cursor = 0usize;

        for directive in directives {
            output.push_str(&source[cursor..directive.start]);
            cursor = directive.end;
            if directive.raw {
                output.push_str("{% raw %}{% sql ");
                output.push_str(&directive.arguments);
                output.push_str(" %}{% endraw %}");
                continue;
            }

            let current_index = directive_index;
            directive_index = directive_index.saturating_add(1);
            if site_over_budget {
                output.push_str(&sql_error(format!(
                    "site contains more than {MAX_SQL_DIRECTIVES_PER_SITE} SQL directives"
                )));
                continue;
            }
            if site_bytes_exhausted {
                output.push_str(&sql_error(
                    "Publisher SQL generated-site byte limit exceeded".to_string(),
                ));
                continue;
            }

            if let Some(data) = directive.arguments.strip_prefix("ToData ") {
                let Some((name, query)) = data.trim().split_once(char::is_whitespace) else {
                    output.push_str(&sql_error(
                        "sqlToData requires a data name and query".to_string(),
                    ));
                    continue;
                };
                if !safe_data_name(name) {
                    output.push_str(&sql_error(format!("invalid sqlToData name {name}")));
                    continue;
                }
                match runtime.to_data_with_budget(query.trim(), &budget) {
                    Ok(value) => {
                        let bytes = publisher_json_bytes(&value);
                        if !admit_bytes(bytes.len(), &mut source_result_bytes, &mut generated_bytes)
                        {
                            site_bytes_exhausted = generated_bytes.saturating_add(bytes.len())
                                > MAX_SQL_GENERATED_BYTES_PER_SITE;
                            output.push_str(&sql_error(
                                "Publisher SQL generated-data limit exceeded".to_string(),
                            ));
                            continue;
                        }
                        // Publisher data is global. Later successful sources
                        // win; bytewise source order makes that deterministic.
                        expansion.generated_data.insert(name.to_string(), bytes);
                        output.push_str(&format!("{{% assign {name} = site.data.{name} %}}"));
                    }
                    Err(error) => {
                        let message = error.to_string();
                        output.push_str(&sql_error(format!("Error processing SQL: {message}")))
                    }
                }
            } else {
                match runtime.render_with_budget(&directive.arguments, &budget) {
                    Ok(value) => {
                        // SQL values are data, never a second Liquid program.
                        let fragment = format!("{{% raw %}}{value}{{% endraw %}}");
                        if !admit_bytes(
                            fragment.len(),
                            &mut source_result_bytes,
                            &mut generated_bytes,
                        ) {
                            site_bytes_exhausted = generated_bytes.saturating_add(fragment.len())
                                > MAX_SQL_GENERATED_BYTES_PER_SITE;
                            output.push_str(&sql_error(
                                "Publisher SQL generated-fragment limit exceeded".to_string(),
                            ));
                            continue;
                        }
                        let include = format!("sql-{current_index}-fragment.xhtml");
                        expansion
                            .generated_includes
                            .insert(include.clone(), fragment.into_bytes());
                        output.push_str(&format!("{{% include {include} %}}"));
                    }
                    Err(error) => {
                        let message = error.to_string();
                        output.push_str(&sql_error(format!("Error processing SQL: {message}")))
                    }
                }
            }
        }
        output.push_str(&source[cursor..]);
        expansion
            .rewritten_sources
            .insert(source_name, output.into_bytes());
    }

    expansion
}

fn admit_bytes(bytes: usize, source_total: &mut usize, site_total: &mut usize) -> bool {
    let next_source = source_total.saturating_add(bytes);
    let next_site = site_total.saturating_add(bytes);
    if next_source > MAX_SQL_RESULT_BYTES_PER_SOURCE || next_site > MAX_SQL_GENERATED_BYTES_PER_SITE
    {
        return false;
    }
    *source_total = next_source;
    *site_total = next_site;
    true
}

/// Use one stable spaced representation for generated JSON data.
fn publisher_json_bytes(value: &serde_json::Value) -> Vec<u8> {
    fn write(value: &serde_json::Value, output: &mut String) {
        match value {
            serde_json::Value::Array(values) => {
                output.push_str("[ ");
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        output.push_str(", ");
                    }
                    write(value, output);
                }
                output.push_str(" ]");
            }
            serde_json::Value::Object(values) => {
                output.push_str("{ ");
                for (index, (name, value)) in values.iter().enumerate() {
                    if index > 0 {
                        output.push_str(", ");
                    }
                    output.push_str(
                        &serde_json::to_string(name).expect("Publisher SQL object key is JSON"),
                    );
                    output.push_str(" : ");
                    write(value, output);
                }
                output.push_str(" }");
            }
            _ => output
                .push_str(&serde_json::to_string(value).expect("Publisher SQL scalar is JSON")),
        }
    }

    let mut output = String::new();
    write(value, &mut output);
    output.into_bytes()
}

struct Directive {
    start: usize,
    end: usize,
    arguments: String,
    raw: bool,
}

fn parse_directives(source: &str) -> Option<Vec<Directive>> {
    let mut directives = Vec::new();
    let mut cursor = 0usize;
    while cursor < source.len() {
        let normal = source[cursor..].find("{% sql").map(|index| cursor + index);
        let raw = source[cursor..].find("{%! sql").map(|index| cursor + index);
        let (start, raw) = match (normal, raw) {
            (None, None) => break,
            (Some(start), None) => (start, false),
            (None, Some(start)) => (start, true),
            (Some(normal), Some(raw)) if normal < raw => (normal, false),
            (Some(_), Some(raw)) => (raw, true),
        };
        let arguments_start = start + if raw { "{%! ".len() } else { "{% ".len() };
        let relative_end = source[arguments_start..].find("%}")?;
        let close = arguments_start + relative_end;
        let arguments = source[arguments_start..close]
            .trim()
            .strip_prefix("sql")?
            .trim()
            .to_string();
        directives.push(Directive {
            start,
            end: close + 2,
            arguments,
            raw,
        });
        cursor = close + 2;
    }
    Some(directives)
}

fn safe_data_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn sql_error(message: String) -> String {
    format!(
        "<span style=\"color: maroon\">{}</span>",
        escape_xml(&message)
    )
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::SqlRuntime;

    fn runtime() -> SqlRuntime {
        let resource = json!({
            "resourceType": "CodeSystem",
            "id": "editor-stages",
            "url": "https://example.org/CodeSystem/editor-stages",
            "concept": [
                {"code": "author", "display": "Author"},
                {"code": "preview", "display": "Preview"}
            ]
        });
        SqlRuntime::from_resources([&resource])
    }

    #[test]
    fn sql_to_data_is_global_and_direct_sql_is_raw_isolated() {
        let expansion = expand_publisher_sql(
            &runtime(),
            [
                (
                    "z-consumer.md".into(),
                    b"{{ site.data.stages[0].display }}".to_vec(),
                ),
                (
                    "a-producer.md".into(),
                    b"{% sqlToData stages SELECT Code AS code, Display AS display FROM Concepts ORDER BY Code %}{% sql SELECT '{{ injected }}' %}".to_vec(),
                ),
            ],
        );
        assert_eq!(
            expansion.generated_data["stages"],
            br#"[ { "code" : "author", "display" : "Author" }, { "code" : "preview", "display" : "Preview" } ]"#
        );
        assert_eq!(
            expansion.generated_includes["sql-1-fragment.xhtml"],
            b"{% raw %}{{ injected }}{% endraw %}"
        );
        let producer =
            String::from_utf8(expansion.rewritten_sources["a-producer.md"].clone()).unwrap();
        assert!(producer.contains("{% assign stages = site.data.stages %}"));
        assert!(producer.contains("{% include sql-1-fragment.xhtml %}"));
        assert!(!expansion.rewritten_sources.contains_key("z-consumer.md"));
    }

    #[test]
    fn later_global_data_definition_wins_in_bytewise_path_order() {
        let expansion = expand_publisher_sql(
            &runtime(),
            [
                (
                    "b.md".into(),
                    b"{% sqlToData shared SELECT Display FROM Concepts ORDER BY Code %}".to_vec(),
                ),
                (
                    "a.md".into(),
                    b"{% sqlToData shared SELECT Code FROM Concepts ORDER BY Code %}".to_vec(),
                ),
            ],
        );
        assert_eq!(
            expansion.generated_data["shared"],
            br#"[ { "Display" : "Author" }, { "Display" : "Preview" } ]"#
        );
    }

    #[test]
    fn malformed_source_is_unchanged_and_raw_sql_is_literal() {
        let expansion = expand_publisher_sql(
            &runtime(),
            [
                ("bad.md".into(), b"before {% sql SELECT 1".to_vec()),
                (
                    "escaped.md".into(),
                    b"before {%! sql SELECT 1 %} after".to_vec(),
                ),
            ],
        );
        assert!(!expansion.rewritten_sources.contains_key("bad.md"));
        assert_eq!(
            expansion.rewritten_sources["escaped.md"],
            b"before {% raw %}{% sql SELECT 1 %}{% endraw %} after"
        );
    }

    #[test]
    fn publisher_json_spacing_and_scalar_shapes_are_stable() {
        let value = json!([{"n": null, "i": 1, "l": 2147483648_i64, "r": "1.5"}]);
        assert_eq!(
            publisher_json_bytes(&value),
            br#"[ { "n" : null, "i" : 1, "l" : 2147483648, "r" : "1.5" } ]"#
        );
        assert_eq!(publisher_json_bytes(&json!([])), b"[  ]");
        assert_eq!(publisher_json_bytes(&json!({})), b"{  }");
    }
}
