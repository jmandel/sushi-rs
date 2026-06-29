//! rust_sushi CLI. Phase-by-phase the `compile` subcommand grows; for now it
//! exposes `lex` for token-stream parity checking against the ANTLR oracle.

use fsh_lexer_parser::{lex_document, Channel};

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
        _ => {
            eprintln!("rust_sushi {}: compile pipeline under construction", env!("CARGO_PKG_VERSION"));
            eprintln!("usage: rust_sushi <lex <file.fsh> | --version>");
        }
    }
    Ok(())
}
