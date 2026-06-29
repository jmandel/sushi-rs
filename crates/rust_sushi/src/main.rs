//! rust_sushi CLI. Phase-by-phase the `compile` subcommand grows; for now it
//! reports version and validates the workspace wires together.
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--version") | Some("version") => {
            println!("rust_sushi {}", env!("CARGO_PKG_VERSION"));
        }
        _ => {
            eprintln!("rust_sushi {}: compile pipeline under construction", env!("CARGO_PKG_VERSION"));
            eprintln!("usage: rust_sushi <command>  (compile coming online phase by phase)");
        }
    }
    Ok(())
}
