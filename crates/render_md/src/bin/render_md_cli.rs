//! CLI: read markdown from stdin, write kramdown-parity HTML to stdout.
//! Used by the differential gate (scripts/md-diff.sh) against the ruby oracle.

use std::io::{Read, Write};

fn main() {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .expect("read stdin");
    let html = render_md::render(&input);
    std::io::stdout()
        .write_all(html.as_bytes())
        .expect("write stdout");
}
