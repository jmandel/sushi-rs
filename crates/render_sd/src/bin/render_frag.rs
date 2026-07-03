//! render-frag: render one SD fragment and print it (wrapped in {% raw %}).
//!
//! Usage: render-frag <kind> <sd.json> [def_file] [core_path]
//!   kind: grid | snapshot | diff | ...
//! def_file defaults to `StructureDefinition-<id>-definitions.html`,
//! core_path defaults to "" (the fragment path).

use std::process::exit;

use render_sd::context::IgContext;
use render_sd::grid::render_grid;
use render_sd::table::{render_table, TableConfig};
use render_sd::{wrap_raw, Sd};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: render-frag <kind> <sd.json> [def_file] [core_path]");
        exit(2);
    }
    let kind = &args[1];
    let json = std::fs::read_to_string(&args[2]).expect("read sd json");
    let sd = Sd::from_json(&json).expect("parse sd json");
    let def_file = if args.len() > 3 && !args[3].is_empty() {
        args[3].clone()
    } else {
        format!("StructureDefinition-{}-definitions.html", sd.id())
    };
    let core_path = if args.len() > 4 { args[4].clone() } else { String::new() };

    // us-core context hardwired for the debug bins (grid + snapshot).
    let f0 = "/home/jmandel/hobby/sushi-rs-snapshot-f0-builds";
    let dbg_ctx = || {
        IgContext::load_with_txcache(
            std::path::Path::new(&format!("{}/us-core/output", f0)),
            std::path::Path::new(&format!("{}/us-core/.home/.fhir/packages", f0)),
            Some(std::path::Path::new(&format!("{}/us-core/input-cache/txcache", f0))),
        )
    };
    let body = match kind.as_str() {
        "grid" => {
            let ctx = dbg_ctx();
            render_grid(&sd, &ctx, &def_file, &core_path)
        }
        "snapshot" => {
            let ctx = dbg_ctx();
            let (b, gaps) = render_table(&sd, &ctx, &def_file, &TableConfig::snapshot(""));
            for g in gaps { eprintln!("gap: {}", g); }
            b
        }
        other => {
            eprintln!("unsupported kind: {}", other);
            exit(2);
        }
    };
    print!("{}", wrap_raw(&body));
}
