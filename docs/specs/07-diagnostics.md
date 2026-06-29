# 07 — Diagnostics Subsystem Porting Spec

## 1. Purpose

The diagnostics subsystem is SUSHI's centralized logging and error-reporting machinery. A single global Winston logger (`utils/FSHLogger.ts`) receives every `info`/`warn`/`error`/`debug` message, appends source-location footers (`File:`/`Line:`), counts messages by severity, optionally suppresses configured "ignored" messages, and optionally captures messages into an in-memory `ErrorsAndWarnings` collector for the library API. The `errors/` directory holds ~53 typed `Error` subclasses whose constructors build the exact human-readable message string; these are thrown deep in the exporter, caught at call sites, and re-emitted via `logger.error(e.message, sourceInfo)`. Because QA compares stdout byte-for-byte, the message text, the `\n  File:`/`\n  Line:` footer format, the severity coloring, the per-message ordering, and the final SUSHI-results banner all matter for parity.

## 2. TS entry points

- `sushi-ts/src/utils/FSHLogger.ts:117` — `logger` (the global Winston logger; the heart of the subsystem).
- `sushi-ts/src/utils/FSHLogger.ts:16` — `withLocation` format: appends `File:`/`Line:`/`Applied in File:`/`Applied on Line:` to the message.
- `sushi-ts/src/utils/FSHLogger.ts:42` — `ignoreMessages` format: drops messages matching configured ignore lists.
- `sushi-ts/src/utils/FSHLogger.ts:54` — `incrementCounts` format: bumps `stats.numInfo/Warn/Error/Debug`.
- `sushi-ts/src/utils/FSHLogger.ts:75` — `trackErrorsAndWarnings` format: pushes into `errorsAndWarnings`.
- `sushi-ts/src/utils/FSHLogger.ts:95` — `printer` format: renders `${coloredLevel} ${message}`.
- `sushi-ts/src/utils/FSHLogger.ts:118` — `combine(...)` pipeline order (load-bearing).
- `sushi-ts/src/utils/FSHLogger.ts:143` / `:148` — `setIgnoredWarnings` / `setIgnoredErrors`.
- `sushi-ts/src/utils/FSHLogger.ts:128` — `parseIgnoredLogsConfiguration` (regex-vs-literal parsing of ignore files).
- `sushi-ts/src/utils/FSHLogger.ts:152` — `LoggerStats` class; `:170` `stats` singleton.
- `sushi-ts/src/utils/FSHLogger.ts:172` — `ErrorsAndWarnings` class; `:184` `errorsAndWarnings` singleton.
- `sushi-ts/src/utils/FSHLogger.ts:196` / `:210` — `switchToSecretLogger` / `restoreMainLogger` (suppress-and-capture for parameterized RuleSets).
- `sushi-ts/src/utils/FSHLogger.ts:192` — `logMessage(level, message)` thin wrapper over `logger.log`.
- `sushi-ts/src/import/FSHErrorListener.ts:5` — ANTLR `FSHErrorListener` (lexer/parser syntax errors → `logger.error(message, {file, location})`); message-rewriting heuristics at `:42–252`.
- `sushi-ts/src/errors/index.ts:1` — barrel export of all error classes.
- `sushi-ts/src/errors/Annotated.ts:1` — `Annotated` interface (`specReferences`).
- `sushi-ts/src/errors/WithSource.ts:3` — `WithSource` interface (`sourceInfo`).
- `sushi-ts/src/errors/ValidationError.ts:1` — the one error carrying an explicit `severity`.
- `sushi-ts/src/fshtypes/FshEntity.ts:46` — `SourceInfo`; `:53` `TextLocation`; `:7`/`:26` `withLocation`/`withAppliedLocation` accepting `[startLine,startCol,endLine,endCol]` tuples.
- `sushi-ts/src/app.ts:385` — `printResults` (the SUSHI RESULTS banner using `stats`); `:345` `process.exit(stats.numError)`.
- `sushi-ts/src/run/FshToFhir.ts:21` — library entry that uses `errorsAndWarnings` to return `{errors, warnings}` instead of printing.
- `sushi-ts/src/utils/puns.ts:103` — `getRandomPun(numErrors, numWarnings)` (random banner text — non-deterministic).

## 3. Key data structures

- `TextLocation` (`fshtypes/FshEntity.ts:53`): `{ startLine, startColumn, endLine, endColumn }`, all `number`. 1-based lines; columns are 1-based after the listener's `+1` adjustment (`FSHErrorListener.ts:37`).
- `SourceInfo` (`fshtypes/FshEntity.ts:46`): `{ file?, location?: TextLocation, appliedFile?, appliedLocation?: TextLocation }`. Every `FshEntity` owns one (`FshEntity.ts:5`), populated via `withFile`/`withLocation`/`withAppliedFile`/`withAppliedLocation`.
- `LoggerInfo` (`FSHLogger.ts:9`): Winston `TransformableInfo` extended with `file?`, `location?`, `appliedFile?`, `appliedLocation?`. The metadata object passed as the 2nd arg to `logger.error/warn` is spread into `info`, so passing a `SourceInfo` makes those four keys available to `withLocation`.
- `LoggerStats` (`FSHLogger.ts:152`): six counters `numInfo, numWarn, numError, numDebug, numIgnoredWarn, numIgnoredError`; `reset()` zeroes all. Singleton `stats` (`:170`).
- `ErrorsAndWarnings` (`FSHLogger.ts:172`): `errors[]` and `warnings[]`, each element `{ message: string, location?: TextLocation, input?: string }`; flag `shouldTrack` (default `false`); `reset()`. Singleton `errorsAndWarnings` (`:184`). Note `input` is populated from `info.file` (`:83`,`:89`), i.e. the file name only — not location.
- `LoggerData` (`FSHLogger.ts:186`): `{ level, errorsAndWarnings, stats }` snapshot used by secret-logger save/restore.
- Error classes: all extend JS `Error` (message set via `super(...)`). Two marker interfaces:
  - `Annotated` (`errors/Annotated.ts`): adds `specReferences: string[]` — **purely informational; never read outside `errors/`** (see Gotchas).
  - `WithSource` (`errors/WithSource.ts`): adds `sourceInfo: SourceInfo` — carried so the catch site can re-emit with location.
  - `ValidationError` (`errors/ValidationError.ts:1`) is the sole class with a `severity: 'error'|'warn'|'info'` field and message `` `${fshPath}: ${issue}` ``.

## 4. Algorithms & control flow

### 4.1 Winston format pipeline (the critical ordering)
`logger` runs every record through `combine(...)` in this exact order (`FSHLogger.ts:118`):
1. `ignoreMessages()` (`:42`) — picks `ignoredWarnings` for `warn`, `ignoredErrors` for `error`, else `null`. Tests each entry: literal `string` → `===` equality; `RegExp` → `.test()`. **Matching is against `info.message` as it stands BEFORE any `File:`/`Line:` footer is appended** (because `ignoreMessages` precedes `withLocation`). If matched: increment `numIgnoredWarn`/`numIgnoredError` and `return false` (Winston drops the record entirely — no count, no track, no print).
2. `incrementCounts()` (`:54`) — switch on level, bump the matching `stats.num*`. Runs only for non-ignored records.
3. `trackErrorsAndWarnings()` (`:75`) — if `errorsAndWarnings.shouldTrack`, push `{message: info.message, location: info.location, input: info.file}` for `error`/`warn`. **Captured `message` is still the bare message (no footer), and `location`/`input` are the structured fields** (`:79–91`).
4. `withLocation()` (`:16`) — mutates `info.message` in place, appending in fixed order: `\n  File: ${file}`, then `\n  Line: ${startLine}` (and `` ` - ${endLine}` `` only if `endLine !== startLine`), then `\n  Applied in File: ${appliedFile}`, then `\n  Applied on Line: ${appliedLine}` (with same end-line suffix rule). Each consumed key is `delete`d. Two spaces of indent before each label.
5. `printer` (`:95`) — returns `` `${level} ${info.message}` `` where `level` is the chalk-colored label. **No timestamp.** Color/label by level:
   - `info` → `chalk.whiteBright.bgGreen('info ')` (note trailing space inside the colored span).
   - `warn` → `chalk.whiteBright.bgRgb(179,98,0)('warn ')` (trailing space; "dark dark orange").
   - `error` → `chalk.whiteBright.bgRed('error')` (no trailing space inside span).
   - `debug` → `chalk.whiteBright.bgBlue('debug')`.
   Then a single literal space separates label from message.

### 4.2 Throw → catch → emit pattern
Error classes never log themselves. Exporter code throws e.g. `new ValueAlreadyAssignedError(...)`; a surrounding `try/catch` does `logger.error(e.message, rule.sourceInfo)` (e.g. `import/FSHImporter.ts:127`, `utils/Processing.ts:235`). The `SourceInfo` becomes the metadata object → its `file`/`location` flow into the pipeline. Thus **diagnostic ordering is entirely determined by the order the exporter visits entities/rules**, not by the diagnostics subsystem itself.

### 4.3 Ignore-list parsing (`parseIgnoredLogsConfiguration`, `:128`)
Split on `/\r?\n/`; `trim()` each line; drop empties and lines starting with `#`. A line wrapped in `/.../` (starts and ends with `/`) becomes `new RegExp(line.slice(1,-1))` (no flags); everything else is a literal string for exact-equality matching. `setIgnoredWarnings`/`setIgnoredErrors` store the parsed arrays in module-level `ignoredWarnings`/`ignoredErrors` (initially `undefined`; `?.some` guards the undefined case at `:45`). Wired from `app.ts:214`/`:219` reading `input/ignoreWarnings.txt` / `ignoreErrors.txt` (resolved via `getIgnoredMessages`, `app.ts:356`).

### 4.4 Secret logger (parameterized RuleSet parsing)
When `FSHImporter` parses a generated/parameterized RuleSet at top level (`FSHImporter.ts:2002`): `switchToSecretLogger()` (`FSHLogger.ts:196`) sets `logger.level='emerg'` (suppresses console output while still running all format functions), deep-clones and `reset()`s both `errorsAndWarnings` and `stats`, and sets `shouldTrack=true`. After the sub-parse, `restoreMainLogger(savedData)` (`:210`) restores level, swaps `errorsAndWarnings`/`stats` back to the originals, and returns the *captured* messages. The importer then re-emits a single aggregated `logger.error`/`logger.warn` (`FSHImporter.ts:2022–2043`) of the form:
```
Error(s) parsing insert rule with parameterized RuleSet <name>
- <msg1>
- <msg2>
```
joined with `os.EOL` (`import { EOL } from 'os'`, `FSHImporter.ts:72`), tagged with `insertRule.sourceInfo`. Singular/plural toggles on count (`Error` vs `Errors`).

### 4.5 Library mode (`fshToFhir`, `run/FshToFhir.ts:21`)
`errorsAndWarnings.reset(); shouldTrack=true` (`:30`). `logLevel==='silent'` → `logger.transports[0].silent=true`; otherwise validate against the level whitelist and set `logger.level`. Returns `{fhir, errors: errorsAndWarnings.errors, warnings: errorsAndWarnings.warnings}` (`:97`). Explicitly **not** reentrant (global singletons).

### 4.6 Run summary (`app.ts:385`)
After all processing, `printResults` reads `stats.num*` to build the boxed banner: counts table, `${numError} Error[s]` / `${numWarn} Warning[s]` (with pluralization at `:394`/`:395`), an optional `… ignored` line if `numIgnoredError||numIgnoredWarn` (`:402`), a random pun (`:397`), and a color (`:399`): red if errors, orange `rgb(179,98,0)` if warnings, else green. Process exits with `stats.numError` (`:345`).

### 4.7 FSHErrorListener message rewriting (`import/FSHErrorListener.ts:27`)
ANTLR raw messages are run through a long `if/else if` ladder (`:58–250`) matching the raw `msg`, the offending token, and up to two previous non-whitespace tokens to produce friendlier messages (e.g. "Assignment rules must include at least one space both before and after the '=' sign", deprecated `Mixins:`/`units`/`,`/`|` guidance). Default `location` is `{startLine:line, startColumn:column+1, endLine:line, endColumn: column + (offendingSymbol?.text.length ?? 1)}` (`:35`); several branches relocate to the previous token via `getTokenLocation` (`:276`). Each branch is order-sensitive (first match wins).

## 5. Edge cases & gotchas

- **`specReferences` is dead output.** A repo-wide grep shows `specReferences` is only ever assigned inside `errors/*.ts` and declared in `Annotated.ts` — never read or printed anywhere. A naive port that appends spec URLs to messages would break parity. Port it as inert metadata or omit it. (`errors/index.ts`, grep over `src/`.)
- **Ignore-matching happens pre-footer.** Ignore entries are tested against the bare message *before* `File:`/`Line:` is appended (pipeline order `:118`). A regex anchored to match the footer will never fire. Conversely, literal-string ignores must equal the entire bare message exactly (`===`, `:46`).
- **Ignored messages don't count as errors/warnings.** `ignoreMessages` returns `false`, dropping the record before `incrementCounts`/`trackErrorsAndWarnings`, so `numError`/`numWarn` and `errorsAndWarnings` exclude them; only `numIgnoredError`/`numIgnoredWarn` move (`:48–51`).
- **Footer end-line suppression.** `Line:`/`Applied on Line:` print a range only when `endLine !== startLine`; single-line locations show one number (`:23`, `:34`). Columns are never printed.
- **Two-space indent, leading `\n`.** Footers are `"\n  File: …"` with a hard `\n` (not `os.EOL`) and exactly two spaces (`:18`,`:22`,`:29`,`:33`).
- **Mixed newline conventions.** Footers use literal `\n`, but the aggregated parameterized-RuleSet message uses `os.EOL` (`FSHImporter.ts:2029`). On Windows these differ — replicate per-call to keep byte parity.
- **Colored level labels embed/omit trailing space.** `info `/`warn ` include a trailing space *inside* the chalk span; `error`/`debug` do not (`:99–110`). The printer then adds one more space. Chalk emits ANSI escapes only when stdout is a TTY / `FORCE_COLOR` is set — parity tests must control `chalk.level`/`FORCE_COLOR`.
- **No timestamps, no JSON.** Output is purely `${level} ${message}`; a default Winston JSON formatter would diverge.
- **Default level is `info`.** `createLogger` is called without `level`, so Winston defaults to `info` (`debug`/`silly` suppressed unless `--log-level` raises it). `silent` is handled only in library mode via `transports[0].silent`.
- **Globals are not reentrant.** `logger`, `stats`, `errorsAndWarnings`, `ignoredWarnings`, `ignoredErrors` are module singletons; `fshToFhir` documents this hazard (`FshToFhir.ts:14`). Secret-logger relies on deep-clone save/restore (`cloneDeep`, `:202`/`:205`).
- **`switchToSecretLogger` suppresses by raising level to `emerg`, not by silencing the transport** — all format functions still execute (so counting/tracking still happen against the swapped singletons) (`:197`).
- **`ValidationError` is the only severity-bearing error**, but the catch site still chooses the log level explicitly via `logger.error`/`logger.warn`; the `severity` field is informational unless the caller branches on it (`ValidationError.ts:5`).
- **Random pun ⇒ non-deterministic banner.** `getRandomPun` uses lodash `sample` (`puns.ts:105`); the SUSHI-results pun line cannot be byte-compared. Exclude it (or seed) in parity harness.
- **`errorsAndWarnings.input` holds only the file**, not the location object (`:83`,`:89`); `location` is a separate field. Don't conflate.

## 6. Recommended Rust mapping

Belongs in the **`diagnostics`** crate.

- Core type `Diagnostic { level: Level, message: String, source: Option<SourceInfo> }` where `Level` is `enum { Info, Warn, Error, Debug }`. Mirror `SourceInfo { file: Option<String>, location: Option<TextLocation>, applied_file: Option<String>, applied_location: Option<TextLocation> }` and `TextLocation { start_line, start_column, end_line, end_column: u32 }` exactly (these are shared with `fsh_model`/`fsh_lexer_parser`, so define `SourceInfo`/`TextLocation` in `fsh_model` and re-export, since `FshEntity` carries them).
- A `Logger` struct (not a global) holding `stats: LoggerStats`, `errors_and_warnings: ErrorsAndWarnings`, `ignored_warnings: Vec<Matcher>`, `ignored_errors: Vec<Matcher>`, `level: Level`, `track: bool`, plus a sink. `Matcher = Literal(String) | Regex(regex::Regex)`. Reproduce the pipeline as an ordered method body: ignore-check → count → track → render-footer → emit, matching §4.1 order precisely.
- Render the footer with a dedicated function producing literal `\n  File: …\n  Line: …` to guarantee byte parity; keep the end-line suppression rule and two-space indent. Keep the `os.EOL` vs `\n` distinction (aggregated RuleSet messages use platform EOL).
- For coloring, gate ANSI behind a `color: bool` (driven by TTY detection / `FORCE_COLOR`) and hardcode the exact SGR sequences chalk produces for `bgGreen`/`bgRgb(179,98,0)`/`bgRed`/`bgBlue` + `whiteBright`, including the in-span trailing spaces for `info `/`warn `.
- Error catalog: one Rust `enum SushiError` (or per-variant structs) whose `Display` reproduces each `super(...)` template byte-for-byte; carry `spec_references` as a `&'static [&'static str]` if desired but never print it. Helper formatters (`allowedTypesToString` `InvalidTypeError.ts:28`, `getExpectedTypes` `ValueSetFilterValueTypeError.ts:19`) must be ported verbatim.
- Connections: the **`compiler`** crate (exporter/processing) is the producer — it owns visitation order and calls `logger.error(e.to_string(), source)`. **`fsh_lexer_parser`** produces the `FSHErrorListener` equivalent (port the rewrite ladder). **`package_store`/`json_emit`** are downstream consumers of `stats`/exit code only. The library-API surface (`fsh_to_fhir`) returns `ErrorsAndWarnings` from the `Logger` instead of printing.
- Prefer an explicit `Logger` threaded through (or a scoped thread-local) over a true global, but the secret-logger save/restore (§4.4) maps cleanly to swapping two fields and restoring them — implement as a guard/RAII that snapshots `stats`+`errors_and_warnings`+`level`+`track` and returns captured messages on drop.

## 7. Parity test ideas

- **Footer formatting:** single-line vs multi-line location → assert `\n  Line: 5` vs `\n  Line: 5 - 9`; assert two-space indent and no column output (`withLocation` `:16`).
- **Applied location:** an `insert` rule emitting an error so all four footer lines appear in order `File / Line / Applied in File / Applied on Line` (`:16–38`).
- **Ignore lists:** an `ignoreWarnings.txt` with one literal line and one `/regex/` line; confirm the matched warning is suppressed, `numIgnoredWarn` increments, `numWarn` does not, and the banner shows the `… ignored` line (`:42`, `app.ts:402`).
- **Ignore-vs-footer:** an ignore entry that matches the bare message but would fail if tested against the footer-augmented string — proves pre-footer matching (`:118`).
- **Pluralization & exit code:** fixtures with exactly 1 vs 2 errors → `1 Error ` vs `2 Errors `, and `process.exit` value equals `numError` (`app.ts:394`,`:345`).
- **Parameterized RuleSet aggregation:** a parameterized RuleSet with two parse errors → single `Errors parsing insert rule with parameterized RuleSet X` block with `- ` bullets joined by EOL, tagged at the insert-rule location (`FSHImporter.ts:2022`).
- **Syntax-error rewrites:** golden fixtures for each `FSHErrorListener` branch (`MyAlias =url`, `* active=true`, `Mixins:`, `units`, `Reference(A | B)`, comma lists) asserting the rewritten message and relocated line/column (`FSHErrorListener.ts:58–250`).
- **Library mode:** call the `fsh_to_fhir` equivalent and assert returned `errors`/`warnings` arrays contain bare messages plus structured `location`/`input`, with no footer text (`FshToFhir.ts:97`).
- **Color control:** run with color on and off; byte-compare ANSI sequences against stock SUSHI under `FORCE_COLOR=1` and `FORCE_COLOR=0`.
- **Message-template catalog:** unit-test each error class's exact string for representative args (e.g. `ValueAlreadyAssignedError`, `InvalidTypeError` with Reference/CodeableReference/profile/code variants, `MismatchedTypeError`, `ValueSetFilterValueTypeError`).

## 8. Open questions

- **Color parity scope:** Do we need byte-exact ANSI (chalk's specific SGR sequences) for QA, or is plain/no-color output the comparison target? This dictates whether we hardcode chalk's escapes. (chalk usage `FSHLogger.ts:99–110`.)
- **Pun line:** The SUSHI-results pun is random (`puns.ts:105`). Confirm the parity harness strips or pins it; do we need to port the pun arrays at all for non-CLI use?
- **`os.EOL` handling:** Target parity OS is Linux (`\n`), but the aggregated RuleSet message uses `os.EOL` (`FSHImporter.ts:2029`). Do we hardcode `\n` or replicate platform-dependence?
- **Winston log-level whitelist:** library mode accepts `silly/verbose/http` (`FshToFhir.ts:119`) that SUSHI never emits. Port the whitelist for error-message parity on invalid `logLevel`, or simplify?
- **Globals vs threaded logger:** Is any caller relying on the singleton semantics (e.g. tests poking `stats`/`errorsAndWarnings` directly)? Affects whether the Rust `Logger` must be a global/thread-local.
- **`ValidationError.severity`:** Are there call sites that branch on `severity` to choose log level, or is it always emitted via a fixed `logger.error`/`logger.warn`? Determines whether the field is behaviorally load-bearing.

---

### Cross-subsystem dependencies noticed
- `SourceInfo`/`TextLocation` originate in `fshtypes/FshEntity.ts` and are shared by the parser, AST, and diagnostics — they belong in `fsh_model` and are consumed here.
- Diagnostic *ordering* is owned entirely by the exporter/processing pipeline (`compiler`), not diagnostics; this spec only fixes per-message format and the secret-logger reordering.
- The `compiler` crate is the sole producer of typed errors (throw/catch/emit); `fsh_lexer_parser` produces syntax diagnostics via the ported `FSHErrorListener`.
- Ignore-list wiring reads files from `input/ignoreWarnings.txt` / `ignoreErrors.txt` (`app.ts`), coupling diagnostics to the CLI/`package_store` file layout.
