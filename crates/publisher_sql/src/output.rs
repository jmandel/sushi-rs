use render_xhtml::{Config, NodeType, XhtmlComposer, XhtmlNode, XhtmlParser};
use serde::Deserialize;

use crate::database::{normalize_query, QueryResult, SqlError, SqlStatementBudget, SqlValue};
use crate::SqlRuntime;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SqlControl {
    query: String,
    #[serde(default = "default_class", rename = "class")]
    class_name: String,
    #[serde(default = "default_titles")]
    titles: bool,
    #[serde(default)]
    columns: Vec<SqlColumn>,
    #[serde(default)]
    code_systems: Vec<String>,
}

fn default_class() -> String {
    "grid".to_string()
}

fn default_titles() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SqlColumn {
    #[serde(default, rename = "type")]
    column_type: ColumnType,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    display: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ColumnType {
    #[default]
    Auto,
    Link,
    Text,
    Markdown,
    Url,
    Coding,
    Canonical,
    Resource,
}

#[derive(Clone, Debug)]
struct ResolvedColumn {
    column_type: ColumnType,
    title: String,
    source: usize,
    target: Option<usize>,
    system_literal: Option<String>,
    system: Option<usize>,
    display: Option<usize>,
    version: Option<usize>,
}

impl SqlRuntime {
    #[cfg(test)]
    pub(crate) fn render(&self, arguments: &str) -> Result<String, SqlError> {
        self.render_with_budget(arguments, &SqlStatementBudget::new(usize::MAX))
    }

    pub(crate) fn render_with_budget(
        &self,
        arguments: &str,
        budget: &SqlStatementBudget,
    ) -> Result<String, SqlError> {
        let normalized_directive = arguments.replace("\r\n", "\n").replace('\r', "\n");
        let normalized_directive = normalized_directive.trim();
        if normalized_directive.is_empty() {
            return Err(SqlError::EmptyQuery);
        }

        let (query, class_name, titles, requested_columns) = if normalized_directive
            .starts_with('{')
        {
            let control: SqlControl = serde_json::from_str(normalized_directive)
                .map_err(|error| SqlError::Sqlite(format!("invalid SQL control JSON: {error}")))?;
            if !control.code_systems.is_empty() {
                // Java mutates package.db by fishing these systems from its
                // ambient worker context. This runtime has no ambient
                // authority: a requested system must already be present in
                // the immutable query snapshot.
                for system in &control.code_systems {
                    let escaped = system.replace('\'', "''");
                    let check = self.query_with_budget(&format!(
                            "SELECT COUNT(*) FROM Resources WHERE Type = 'CodeSystem' AND (Url = '{escaped}' OR Url || '|' || Version = '{escaped}')"
                        ), budget)?;
                    if check.rows.first().and_then(|row| row.first()) != Some(&SqlValue::Integer(1))
                    {
                        return Err(SqlError::Sqlite(format!(
                            "code system {system} is not present in the closed query snapshot"
                        )));
                    }
                }
            }
            (
                normalize_query(&control.query)?,
                control.class_name,
                control.titles,
                control.columns,
            )
        } else {
            (
                normalize_query(normalized_directive)?,
                default_class(),
                true,
                Vec::new(),
            )
        };

        let result = self.query_normalized_with_budget(&query, budget)?;
        let columns = resolve_columns(&result, requested_columns)?;
        render_result(&result, &columns, &class_name, titles)
    }
}

fn resolve_columns(
    result: &QueryResult,
    requested: Vec<SqlColumn>,
) -> Result<Vec<ResolvedColumn>, SqlError> {
    if requested.is_empty() {
        return Ok(result
            .columns
            .iter()
            .enumerate()
            .map(|(source, name)| ResolvedColumn {
                column_type: ColumnType::Auto,
                title: name.clone(),
                source,
                target: None,
                system_literal: None,
                system: None,
                display: None,
                version: None,
            })
            .collect());
    }

    requested
        .into_iter()
        .map(|column| {
            let source_name = column
                .source
                .as_deref()
                .or(column.title.as_deref())
                .ok_or_else(|| SqlError::Sqlite("a source column is required".into()))?;
            let source = column_index(result, source_name)?;
            let title = column
                .title
                .clone()
                .or(column.source.clone())
                .unwrap_or_else(|| source_name.to_string());
            let lookup = |name: Option<&str>| -> Result<Option<usize>, SqlError> {
                name.map(|name| column_index(result, name)).transpose()
            };
            let system_column = column
                .system
                .as_deref()
                .filter(|name| result.columns.iter().any(|candidate| candidate == name));
            Ok(ResolvedColumn {
                column_type: column.column_type,
                title,
                source,
                target: lookup(column.target.as_deref())?,
                system_literal: column.system.clone().filter(|_| system_column.is_none()),
                system: lookup(system_column)?,
                display: lookup(column.display.as_deref())?,
                version: lookup(column.version.as_deref())?,
            })
        })
        .collect()
}

fn column_index(result: &QueryResult, name: &str) -> Result<usize, SqlError> {
    result
        .columns
        .iter()
        .position(|column| column == name)
        .ok_or_else(|| SqlError::Sqlite(format!("unable to find column {name} in SQL result")))
}

fn render_result(
    result: &QueryResult,
    columns: &[ResolvedColumn],
    class_name: &str,
    titles: bool,
) -> Result<String, SqlError> {
    let cell_count = result.rows.len().saturating_mul(columns.len());
    if cell_count == 1 {
        if let Some(text) = result.rows[0][columns[0].source].display_text() {
            if text.len() > super::database::MAX_RENDERED_BYTES {
                return Err(SqlError::ResultLimit);
            }
            return Ok(text);
        }
    }

    let mut table = XhtmlNode::new(NodeType::Element);
    table.set_name("table").set_attribute("class", class_name);
    if titles {
        let row = table.add_tag("tr");
        for column in columns {
            row.add_tag("td")
                .set_attribute("style", "background-color: #eeeeee")
                .add_text(&column.title);
        }
    }
    for values in &result.rows {
        let row = table.add_tag("tr");
        for column in columns {
            let cell = row.add_tag("td");
            render_cell(cell, values, column);
        }
    }
    let mut composer = XhtmlComposer::new(Config::xml_compact());
    let value = composer.compose_node(&table);
    if value.len() > super::database::MAX_RENDERED_BYTES {
        return Err(SqlError::ResultLimit);
    }
    Ok(value)
}

fn render_cell(cell: &mut XhtmlNode, row: &[SqlValue], column: &ResolvedColumn) {
    let Some(text) = row[column.source].display_text() else {
        return;
    };
    match column.column_type {
        ColumnType::Text => {
            cell.add_text(text);
        }
        ColumnType::Link => {
            let target = column
                .target
                .and_then(|index| row[index].display_text())
                .unwrap_or_else(|| text.clone());
            add_link(cell, &target, &text);
        }
        ColumnType::Url | ColumnType::Canonical | ColumnType::Resource => {
            add_link(cell, &text, &text);
        }
        ColumnType::Markdown => add_markdown(cell, &text),
        ColumnType::Coding => {
            let display = column
                .display
                .and_then(|index| row[index].display_text())
                .unwrap_or_else(|| text.clone());
            let system = column
                .system
                .and_then(|index| row[index].display_text())
                .or_else(|| column.system_literal.clone());
            let version = column.version.and_then(|index| row[index].display_text());
            cell.add_text(display);
            if let Some(system) = system {
                let suffix = match version {
                    Some(version) => format!(" ({system}|{version}#{text})"),
                    None => format!(" ({system}#{text})"),
                };
                cell.add_text(suffix);
            }
        }
        ColumnType::Auto => {
            if is_linkable_url(&text) {
                add_link(cell, &text, &text);
            } else if looks_like_markdown(&text) {
                add_markdown(cell, &text);
            } else {
                cell.add_text(text);
            }
        }
    }
}

fn add_link(parent: &mut XhtmlNode, target: &str, text: &str) {
    parent
        .add_tag("a")
        .set_attribute("href", target)
        .add_text(text);
}

fn add_markdown(parent: &mut XhtmlNode, source: &str) {
    let rendered = render_md::render(source);
    let mut parser = XhtmlParser::new();
    match parser.parse_fragment_children(&rendered) {
        Ok(nodes) => parent.child_nodes_mut().extend(nodes),
        Err(_) => {
            parent.add_text(source);
        }
    }
}

fn is_linkable_url(value: &str) -> bool {
    value.starts_with("http://")
        || value.starts_with("https://")
        || value.starts_with("mailto:")
        || value.starts_with("ftp://")
}

fn looks_like_markdown(value: &str) -> bool {
    value.contains("**") || value.contains("__") || (value.contains('[') && value.contains("]("))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn runtime() -> SqlRuntime {
        let resource = json!({
            "resourceType": "CodeSystem",
            "id": "stages",
            "url": "https://example.org/CodeSystem/stages",
            "concept": [
                {"code": "author", "display": "Author"},
                {"code": "preview", "display": "Preview"}
            ]
        });
        SqlRuntime::from_resources([&resource])
    }

    #[test]
    fn one_cell_is_raw_and_multi_cell_is_publisher_table() {
        let runtime = runtime();
        assert_eq!(runtime.render("SELECT 'raw'").unwrap(), "raw");
        let table = runtime
            .render("SELECT Code, Display FROM Concepts ORDER BY Code")
            .unwrap();
        assert!(table.starts_with("<table class=\"grid\"><tr>"));
        assert!(table.contains("background-color: #eeeeee"));
        assert!(table.contains("<td>author</td><td>Author</td>"));
    }

    #[test]
    fn json_control_selects_and_renames_columns() {
        let runtime = runtime();
        let table = runtime
            .render(
                r#"{"query":"SELECT Code, Display FROM Concepts ORDER BY Code","class":"codes","titles":false,"columns":[{"source":"Display","title":"Label","type":"text"}]}"#,
            )
            .unwrap();
        assert!(table.starts_with("<table class=\"codes\"><tr><td>Author"));
        assert!(!table.contains("Label"));
    }

    #[test]
    fn json_code_system_checks_share_the_page_statement_budget() {
        let runtime = runtime();
        let control = serde_json::json!({
            "query": "SELECT 1",
            "codeSystems": std::iter::repeat_n("https://example.org/CodeSystem/stages", 64)
                .collect::<Vec<_>>()
        })
        .to_string();
        let error = runtime
            .render_with_budget(&control, &SqlStatementBudget::new(64))
            .unwrap_err();
        assert!(matches!(error, SqlError::StatementBudget(64)));
    }
}
