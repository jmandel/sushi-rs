# Examples

The supported site example is the same closed lifecycle used by WASM and native
hosts. The editor's [`ARCHITECTURE.md`](../../../ARCHITECTURE.md) is normative;
examples do not define a second host API.

## Publisher from a fresh process

```sh
SOURCE_DATE_EPOCH=1783555200 fig prepare <ig-dir> \
  --target publisher-site/v1 \
  --template hl7.fhir.template#1.0.0 \
  --cache <package-cache> \
  --out <closed-bundle>

fig outputs <closed-bundle>
fig render <closed-bundle> en/index.html -o index.html
fig finalize <closed-bundle> -o <new-site-directory>
```

`prepare` writes only the canonical `site-build.json` and its addressed
`objects/sha256` closure. Each following command may run in a fresh process:
it authenticates that closure, calls `SiteEngine::restore`, and then uses the
same handle-scoped `outputs`, `render`, or `finalize` operation. No staged page
tree or prior renderer process is an input.

## External Cycle renderer

```sh
SOURCE_DATE_EPOCH=1783555200 fig prepare <ig-dir> \
  --target cycle-site/v2 \
  --cache <package-cache> \
  --out <closed-bundle>

# Cycle LiquidJS opens the closed build, renders directly into ContentStore,
# and uses the same no-argument Build.finalize lifecycle.
SITE_BUILD_DIR=<closed-bundle> SITE_GEN_REPLACE_OUTPUT=1 \
  bun /path/to/cycle/site-gen/build.tsx
```

Cycle intentionally uses LiquidJS while Publisher templates use Rust Liquid.
They share `PreparedGuide -> SiteBuild -> SiteOutput`, exact `ContentRef` bytes,
and Rust-owned final validation; they do not share renderer implementation.

## Transport examples

| Example | What it demonstrates |
| --- | --- |
| `envelope/` | Validate the shared `apiVersion` success/error envelope. |
| `shell-to-fig/` | Invoke Fig from a non-JavaScript process and parse envelopes. |

The old F0 build-root, fragment-materialization, and staged-render examples and
their skippable harness branches are deleted rather than retained as aliases.
