use std::path::Path;
use std::time::Instant;
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let build = &args[1];
    let root = fig::engine::RenderRoot::detect(Path::new(build))?;
    let opts = fig::engine::RenderOptions::default(); // engine on, includes from disk (the F0 tree)
    let t0 = Instant::now();
    let mut st = fig::watch::WatchState::initial(root.clone(), opts)?;
    eprintln!("initial full render: {} ms", t0.elapsed().as_millis());
    // A realistic warm edit: the author edits a pagecontent page source. The
    // watch loop re-renders exactly that page.
    let mut inputs: Vec<_> = std::fs::read_dir(&root.input_dir)?.flatten()
        .map(|e| e.path())
        .filter(|f| f.extension().and_then(|x| x.to_str()) == Some("html"))
        .collect();
    inputs.sort();
    let page = inputs.iter().find(|p| p.file_name().unwrap().to_string_lossy() == "index.html")
        .cloned().unwrap_or_else(|| inputs[0].clone());
    let t1 = Instant::now();
    let n = st.on_change(&[page.clone()])?;
    eprintln!("warm page edit ({}) -> {n} page(s) in {} ms  [gate <1000ms]",
        page.file_name().unwrap().to_string_lossy(), t1.elapsed().as_millis());
    // Also time re-rendering the heaviest profile page (a table-bearing page).
    if let Some(heavy) = inputs.iter().find(|p| p.file_name().unwrap().to_string_lossy().contains("patient")) {
        let t2 = Instant::now();
        let n2 = st.on_change(&[heavy.clone()])?;
        eprintln!("warm page edit ({}) -> {n2} page(s) in {} ms",
            heavy.file_name().unwrap().to_string_lossy(), t2.elapsed().as_millis());
    }
    Ok(())
}
