//! CLI: read markdown from stdin, write kramdown-parity HTML to stdout.
//! Used by the differential gate (scripts/md-diff.sh) against the ruby oracle.

use std::io::{Read, Write};

fn main() {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .expect("read stdin");
    // `--rouge` turns on Jekyll's rouge markdownify wrappers (F5 page pass);
    // default off keeps the F1b differential gate comparing bare kramdown.
    let rouge = std::env::args().any(|a| a == "--rouge");
    let html = if rouge {
        render_md::render_with(
            &input,
            &render_md::Options { rouge_wrappers: true, ..Default::default() },
        )
    } else {
        render_md::render(&input)
    };
    std::io::stdout()
        .write_all(html.as_bytes())
        .expect("write stdout");
}
