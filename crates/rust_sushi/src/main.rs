//! rust_sushi CLI. Phase-by-phase the `compile` subcommand grows; for now it
//! exposes `lex` for token-stream parity checking against the ANTLR oracle.

use fsh_lexer_parser::{import_to_json, lex_document, Channel};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--version") | Some("version") => {
            println!("rust_sushi {}", env!("CARGO_PKG_VERSION"));
        }
        Some("lex") => {
            // rust_sushi lex <file.fsh>  -> token JSON matching harness/lex-oracle.cjs
            let file = args
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("usage: rust_sushi lex <file.fsh>"))?;
            let content = std::fs::read_to_string(file)?;
            let toks = lex_document(&content);
            let arr: Vec<serde_json::Value> = toks
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": t.kind.name(),
                        "channel": match t.channel {
                            Channel::Hidden => serde_json::json!("HIDDEN"),
                            Channel::Default => serde_json::json!(0),
                        },
                        "text": t.text,
                        "line": t.line,
                        "col": t.col,
                        "start": t.start,
                        "stop": t.stop,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&arr)?);
        }
        Some("ast") => {
            // rust_sushi ast <file.fsh>  -> import AST JSON matching harness/parse-oracle.cjs
            let file = args
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("usage: rust_sushi ast <file.fsh>"))?;
            let content = std::fs::read_to_string(file)?;
            let v = import_to_json(&[(file.as_str(), &content)]);
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Some("expand") => {
            // rust_sushi expand <file.fsh ...>  -> post-expansion AST matching harness/expand-oracle.cjs
            let files: Vec<String> = args[2..].to_vec();
            if files.is_empty() {
                return Err(anyhow::anyhow!("usage: rust_sushi expand <file.fsh ...>"));
            }
            let loaded: Vec<(String, String)> = files
                .iter()
                .map(|f| Ok((f.clone(), std::fs::read_to_string(f)?)))
                .collect::<anyhow::Result<_>>()?;
            let refs: Vec<(&str, &str)> =
                loaded.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
            let v = compiler::expand_to_json(&refs);
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Some("build") => {
            // rust_sushi build <ig-dir> -o <out-dir>
            let ig = args.get(2).ok_or_else(|| anyhow::anyhow!("usage: rust_sushi build <ig-dir> -o <out>"))?;
            let mut out = "fsh-generated".to_string();
            if let Some(i) = args.iter().position(|a| a == "-o" || a == "--out") {
                out = args.get(i + 1).cloned().ok_or_else(|| anyhow::anyhow!("-o needs a value"))?;
            }
            compiler::build_project(ig, &out)?;
        }
        Some("pkg-fish") => {
            // rust_sushi pkg-fish <ig-dir> <cache-dir> <query...>  -> package-oracle JSON shape
            let ig = args.get(2).ok_or_else(|| anyhow::anyhow!("usage: pkg-fish <ig> <cache> <query...>"))?;
            let cache = args.get(3).ok_or_else(|| anyhow::anyhow!("need <cache-dir>"))?;
            let queries = &args[4.min(args.len())..];
            let store = package_store::PackageStore::for_project(ig, cache)?;
            let mut qout = Vec::new();
            for q in queries {
                let fhir = store.fish_for_fhir(q, package_store::ALL_FISH_TYPES);
                let meta = store.fish_for_metadata(q, package_store::ALL_FISH_TYPES);
                let fhir_summary = fhir.as_ref().map(|v| serde_json::json!({
                    "resourceType": v.get("resourceType"), "id": v.get("id"),
                    "url": v.get("url"), "version": v.get("version"),
                }));
                qout.push(serde_json::json!({"query": q, "fhir": fhir_summary, "meta": meta}));
            }
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({"queries": qout}))?);
        }
        _ => {
            eprintln!("rust_sushi {}: compile pipeline under construction", env!("CARGO_PKG_VERSION"));
            eprintln!("usage: rust_sushi <lex <file.fsh> | ast <file.fsh> | --version>");
        }
    }
    Ok(())
}
