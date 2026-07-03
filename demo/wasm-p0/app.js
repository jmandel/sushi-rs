// WASM P0 demo driver.
//
// Runs the two Rust WASI binaries (rust_sushi + snapshot_gen) entirely in the
// browser against an in-memory virtual filesystem, mirroring the native CLI:
//
//   rust_sushi   build /work/cycle -o /work/out --cache /work/packages
//   snapshot_gen --cache /work/packages --package hl7.fhir.r5.core#5.0.0 \
//                --local-dir <resources> <profile.json>
//
// Both binaries are plain WASI programs; the @bjorn3/browser_wasi_shim provides
// argv/env, an in-memory FS, and captured stdout/stderr. No engine code changes.

import {
  WASI, File, Directory, PreopenDirectory, OpenFile, ConsoleStdout, wasi, strace,
} from "./vendor/browser_wasi_shim/index.js";

const STRACE = new URLSearchParams(location.search).get("strace") === "1";
if (new URLSearchParams(location.search).get("debug") === "1") {
  const { debug } = await import("./vendor/browser_wasi_shim/debug.js");
  debug.enable(true);
}

const $ = (id) => document.getElementById(id);
const log = (msg) => { $("log").textContent += msg + "\n"; };
const sha256Hex = async (bytes) => {
  const d = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(d)].map((b) => b.toString(16).padStart(2, "0")).join("");
};
const enc = new TextEncoder();
const dec = new TextDecoder();

// ---- build an in-memory Directory tree from a flat {path: Uint8Array} map ----
function buildTree(fileMap) {
  const root = new Map();
  const dirCache = new Map([["", root]]);
  const ensureDir = (dirPath) => {
    if (dirCache.has(dirPath)) return dirCache.get(dirPath);
    const parts = dirPath.split("/");
    const name = parts.pop();
    const parent = ensureDir(parts.join("/"));
    let d = parent.get(name);
    if (!d) { d = new Directory([]); parent.set(name, d); }
    dirCache.set(dirPath, d.contents);
    return d.contents;
  };
  for (const [path, bytes] of Object.entries(fileMap)) {
    const parts = path.split("/");
    const name = parts.pop();
    const dir = ensureDir(parts.join("/"));
    dir.set(name, new File(bytes));
  }
  return root;
}

// Recursively collect every regular file under a Directory, as {path: bytes}.
function collectFiles(dir, prefix, out) {
  for (const [name, inode] of dir.contents.entries()) {
    const p = prefix ? `${prefix}/${name}` : name;
    if (inode instanceof Directory) collectFiles(inode, p, out);
    else if (inode instanceof File) out[p] = inode.data;
  }
  return out;
}

// ---- run one WASI program to completion, return {stdout, stderr, code, ms} ----
// `root` is the shared /work Directory (Map of inodes); mutations persist so a
// later invocation sees files an earlier one wrote (exactly like the real FS).
async function runWasi(module, args, rootMap) {
  const stdoutChunks = [];
  const stderrChunks = [];
  // ConsoleStdout(write) invokes `write(bytes)` on every fd_write; capture raw
  // bytes so snapshot JSON is byte-exact (no line-buffering / re-encoding).
  const outFd = new ConsoleStdout((b) => stdoutChunks.push(b.slice()));
  const errFd = new ConsoleStdout((b) => stderrChunks.push(b.slice()));

  const fds = [
    new OpenFile(new File([])),                 // fd 0: stdin (empty)
    outFd,                                       // fd 1: stdout
    errFd,                                       // fd 2: stderr
    new PreopenDirectory("/work", rootMap),      // fd 3: preopened /work
  ];
  const w = new WASI(args, [], fds, { debug: false });
  const importObj = STRACE
    ? strace(w.wasiImport, ["fd_write", "fd_read", "environ_get", "environ_sizes_get"])
    : w.wasiImport;
  const inst = await WebAssembly.instantiate(module, {
    wasi_snapshot_preview1: importObj,
  });
  const t0 = performance.now();
  let code = 0;
  try {
    w.start(inst);
  } catch (e) {
    if (e && e.constructor && e.constructor.name === "WASIProcExit") code = e.code;
    else throw e;
  }
  const ms = performance.now() - t0;
  const join = (chunks) => {
    const total = chunks.reduce((n, c) => n + c.length, 0);
    const buf = new Uint8Array(total);
    let o = 0; for (const c of chunks) { buf.set(c, o); o += c.length; }
    return buf;
  };
  return { stdout: join(stdoutChunks), stderr: dec.decode(join(stderrChunks)), code, ms };
}

async function main() {
  const runBtn = $("run");
  runBtn.disabled = true;
  $("log").textContent = "";
  const results = { build: {}, snapshots: {}, hashes: {} };

  // 1. Load the vfs manifest + all files.
  log("fetching vfs manifest + files ...");
  const manifest = await (await fetch("data/vfs.json")).json();
  const t0 = performance.now();
  const fileMap = {};
  await Promise.all(manifest.files.map(async (rel) => {
    // Package dir names contain '#' (e.g. hl7.fhir.r4.core#4.0.1); a raw '#' in a
    // URL is a fragment delimiter, so fetch() would drop everything after it and
    // 404. Encode each path segment so '#' (and any other special char) survives.
    const url = `data/${manifest.root}/${rel.split("/").map(encodeURIComponent).join("/")}`;
    const resp = await fetch(url);
    if (!resp.ok) throw new Error(`fetch ${url} -> ${resp.status}`);
    fileMap[rel] = new Uint8Array(await resp.arrayBuffer());
  }));
  const fetchMs = performance.now() - t0;
  const totalBytes = Object.values(fileMap).reduce((n, b) => n + b.length, 0);
  log(`  ${manifest.files.length} files, ${(totalBytes / 1e6).toFixed(1)} MB, ${fetchMs.toFixed(0)} ms`);

  // 2. Compile the two wasm modules (once).
  log("compiling wasm modules ...");
  const tc = performance.now();
  const [sushiBytes, snapBytes] = await Promise.all([
    fetch("data/rust_sushi.wasm").then((r) => r.arrayBuffer()),
    fetch("data/snapshot_gen.wasm").then((r) => r.arrayBuffer()),
  ]);
  const [sushiMod, snapMod] = await Promise.all([
    WebAssembly.compile(sushiBytes),
    WebAssembly.compile(snapBytes),
  ]);
  log(`  compiled in ${(performance.now() - tc).toFixed(0)} ms`);

  // 3. Assemble /work: cycle + packages, plus an empty out/ we can write to.
  const rootMap = buildTree(fileMap);
  rootMap.set("out", new Directory([]));

  // 4. rust_sushi build.
  log("\n=== rust_sushi build (cycle IG) ===");
  const build = await runWasi(sushiMod,
    ["rust_sushi", "build", "/work/cycle", "-o", "/work/out", "--cache", "/work/packages"],
    rootMap);
  log(`  exit=${build.code}  ${build.ms.toFixed(0)} ms`);
  if (build.stderr.trim()) log("  stderr: " + build.stderr.trim());
  results.build.ms = build.ms;
  results.build.code = build.code;

  // 5. Gather produced resources.
  const outFiles = {};
  const outDir = rootMap.get("out");
  collectFiles(outDir, "out", outFiles);
  window.__BUILD_DEBUG__ = { stderr: build.stderr, outFiles: Object.keys(outFiles), code: build.code };
  const resourcePaths = Object.keys(outFiles)
    .filter((p) => p.startsWith("out/fsh-generated/resources/") && p.endsWith(".json"))
    .sort();
  log(`  produced ${resourcePaths.length} resource files`);

  // Hash every build output (for the native byte-match gate).
  for (const p of resourcePaths) {
    results.hashes[p.replace("out/fsh-generated/resources/", "build/")] =
      await sha256Hex(outFiles[p]);
  }

  const resourcesDir = "/work/out/fsh-generated/resources";
  // resourcePaths are keyed as "out/fsh-generated/resources/<file>"; the guest
  // sees them under /work, so the guest path is "/work/<resourcePath>".
  const profilePaths = resourcePaths.filter((p) => p.includes("/StructureDefinition-"));

  // 6. snapshot_gen for every profile.
  log("\n=== snapshot_gen (per profile) ===");
  let snapTotal = 0;
  const table = [];
  for (const rel of profilePaths) {
    const guestPath = `/work/${rel}`;
    const snap = await runWasi(snapMod,
      ["snapshot_gen", "--cache", "/work/packages", "--package", "hl7.fhir.r5.core#5.0.0",
       "--local-dir", resourcesDir, guestPath],
      rootMap);
    snapTotal += snap.ms;
    const name = rel.split("/").pop();
    let elems = "?";
    try { elems = JSON.parse(dec.decode(snap.stdout)).snapshot.element.length; } catch (_) {}
    if (snap.code !== 0 && snap.stderr.trim()) log(`  ${name}: exit=${snap.code} ${snap.stderr.trim()}`);
    table.push({ name, ms: snap.ms, elems, code: snap.code });
    results.snapshots[name] = { ms: snap.ms, elems, code: snap.code };
    results.hashes[`snapshot/${name}`] = await sha256Hex(snap.stdout);
  }
  for (const r of table) log(`  ${r.name.padEnd(44)} ${String(r.elems).padStart(4)} elems  ${r.ms.toFixed(0)} ms`);

  // 7. Summary + gate.
  const totalCompute = build.ms + snapTotal;
  log("\n=== summary ===");
  log(`  build:            ${build.ms.toFixed(0)} ms`);
  log(`  snapshots (${table.length}):    ${snapTotal.toFixed(0)} ms  (avg ${(snapTotal / table.length).toFixed(0)} ms/profile)`);
  log(`  compute total:    ${totalCompute.toFixed(0)} ms`);
  const gatePass = totalCompute < 3000 && build.code === 0;
  log(`  P0 gate (<3s, build ok): ${gatePass ? "PASS" : "FAIL"}`);

  // Render the summary card.
  $("summary").innerHTML = `
    <div class="stat"><span>Build</span><b>${build.ms.toFixed(0)} ms</b></div>
    <div class="stat"><span>Snapshots (${table.length})</span><b>${snapTotal.toFixed(0)} ms</b></div>
    <div class="stat"><span>Avg / profile</span><b>${(snapTotal / table.length).toFixed(0)} ms</b></div>
    <div class="stat"><span>Sample: ${table[0]?.name.replace(/^StructureDefinition-|\.json$/g, "")}</span><b>${table[0]?.elems} elems</b></div>
    <div class="stat gate ${gatePass ? "ok" : "bad"}"><span>P0 gate (compute &lt; 3s)</span><b>${gatePass ? "PASS" : "FAIL"} (${totalCompute.toFixed(0)} ms)</b></div>`;
  $("summary").hidden = false;

  // 8. Export the hash manifest for the byte-match check against native.
  const hashJson = JSON.stringify(results.hashes, Object.keys(results.hashes).sort(), 2);
  const blob = new Blob([hashJson], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = $("download");
  a.href = url; a.download = "wasm-hashes.json"; a.hidden = false;
  a.textContent = "download wasm-hashes.json";
  console.log("WASM_HASHES_JSON_BEGIN");
  console.log(hashJson);
  console.log("WASM_HASHES_JSON_END");
  window.__WASM_HASHES__ = results.hashes;  // for headless extraction

  // Headless marker: embed the hash manifest in a hidden element so a
  // `chromium --headless --dump-dom` run can extract it without CDP.
  const marker = document.createElement("div");
  marker.id = "done-marker";
  marker.dataset.gate = gatePass ? "PASS" : "FAIL";
  marker.textContent = hashJson;
  marker.style.display = "none";
  document.body.appendChild(marker);

  runBtn.disabled = false;
}

$("run").addEventListener("click", () => { main().catch((e) => { log("ERROR: " + (e.stack || e)); }); });

// Auto-run for headless verification: open with ?auto=1
if (new URLSearchParams(location.search).get("auto") === "1") {
  main().catch((e) => { log("ERROR: " + (e.stack || e)); });
}
