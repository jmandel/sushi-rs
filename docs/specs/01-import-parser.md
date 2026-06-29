# 01 — Import / Parser Subsystem Porting Spec

Scope: how FSH text becomes the import AST. Covers `sushi-ts/src/import/` (lexing,
parsing, the ANTLR visitor `FSHImporter`, error listener, alias handling,
source-span capture, parameterized RuleSet expansion) and the grammars
`sushi-ts/antlr/src/main/antlr/{FSH.g4,FSHLexer.g4,MiniFSH.g4,MiniFSHLexer.g4}`.
**Excludes** rule application/export (FSHTank, importConfiguration, exporters).

All line citations are relative to repo root. Upstream is `sushi-ts@v3.20.0`, READ ONLY.

---

## 1. Purpose

This subsystem turns raw FSH source text into an in-memory "import AST": a set of
`FSHDocument`s, one per input file, each holding maps of named FSH entities
(profiles, extensions, instances, value sets, code systems, invariants, rule sets,
mappings, aliases) and their rules. It runs ANTLR-generated lexer/parser over each
file, then walks the parse tree with a hand-written visitor (`FSHImporter`) that
constructs typed `fshtypes` objects, attaches 1-based source spans, resolves
aliases, applies indentation-based path context (soft-indexing), and expands
parameterized RuleSets. It also normalizes/validates lexemes (strings, codes,
numbers, references, quantities) and emits the bulk of SUSHI's syntax and
early-semantic diagnostics. The output (`FSHDocument[]`) is consumed downstream by
the FSHTank and exporters; this subsystem does no FHIR resolution.

---

## 2. TS entry points

- `importText(rawFSHes)` — public entry; constructs one `FSHImporter` and calls
  `.import()`. `sushi-ts/src/import/importText.ts:10`.
- `class FSHImporter extends FSHVisitor` — the whole machine.
  `sushi-ts/src/import/FSHImporter.ts:148`.
  - `import(rawFSHes): FSHDocument[]` — two-pass driver. `FSHImporter.ts:163`.
  - `parseDoc(input, file)` — builds lexer→`CommonTokenStream`→parser, wires the
    error listener, returns `doc()` parse tree. `FSHImporter.ts:2552`.
  - `visitDoc` / `visitEntity` — dispatch per entity, wrapped in try/catch.
    `FSHImporter.ts:254`, `FSHImporter.ts:268`.
  - `extractStartStop(ctx, suppressError?)` — span capture (the load-bearing
    geometry function). `FSHImporter.ts:2511`.
  - `getStarContextStartColumn(ctx)` — derives indent from STAR token text.
    `FSHImporter.ts:2547`.
  - `prependPathContext` / `isValidContext` — indentation/soft-index context engine.
    `FSHImporter.ts:2284`, `FSHImporter.ts:2364`.
  - `aliasAwareValue` / `validateAliasResolves` — alias substitution.
    `FSHImporter.ts:2268`, `FSHImporter.ts:2258`.
  - `applyRuleSetParams` / `parseGeneratedRuleSet` / `parseInsertRuleParams` —
    parameterized RuleSet expansion. `FSHImporter.ts:1926`, `:1992`, `:1881`.
  - lexeme helpers: `extractString`/`unescapeQuotedString` `:2397`/`:2402`,
    `extractMultilineString` `:2427`, `extractNumberValue` `:2474`,
    `parseCodeLexeme` (member) `:1056`.
- `parseCodeLexeme(conceptText)` — free function, system/code split.
  `sushi-ts/src/import/parseCodeLexeme.ts:11`.
- `class FSHDocument` — per-file output container. `sushi-ts/src/import/FSHDocument.ts:14`.
- `class RawFSH { content, path }` — input wrapper. `sushi-ts/src/import/RawFSH.ts:1`.
- `class FSHErrorListener extends ErrorListener<Token>` — converts ANTLR syntax
  errors into improved messages + spans, logs via `logger.error`.
  `sushi-ts/src/import/FSHErrorListener.ts:5`.
- `applyRuleSetSubstitutions(ruleSet, values)` + `MiniFSHImporter` — textual
  parameter substitution producing FSH text. `sushi-ts/src/import/MiniFSHImporter.ts:10`.
- `parserContexts.ts` — TypeScript interface declarations describing the generated
  ANTLR context accessor shape; also exports the predicates `isStarContext`,
  `containsPathContext`, `containsCodePathContext`, `hasPathRule`.
  `sushi-ts/src/import/parserContexts.ts:1`, predicates `:563`–`:591`.
- Grammars: `FSH.g4` (parser rules), `FSHLexer.g4` (tokens + lexer modes),
  `MiniFSH.g4`/`MiniFSHLexer.g4` (param-substitution mini-language).

---

## 3. Key data structures

- `FSHDocument` (`FSHDocument.ts:14`): one per file; fields are `Map<string, T>`
  keyed by entity name: `aliases` (string→string), `profiles`, `extensions`,
  `resources`, `logicals`, `instances`, `valueSets`, `codeSystems`, `invariants`,
  `ruleSets` (params-free), `appliedRuleSets` (expanded, keyed by JSON identifier,
  not name), `mappings`. `file` is the path. Insertion order of Maps is the file
  order; downstream relies on it.
- `FSHImporter` mutable state (`FSHImporter.ts:149`–`155`):
  - `docs: FSHDocument[]` — all docs (used for cross-file duplicate checks).
  - `currentFile`, `currentDoc` — swapped per file during each pass.
  - `allAliases: Map<string,string>` — **global across all files**, built in pass 1.
  - `paramRuleSets: Map<string, ParamRuleSet>` — **global**, built in pass 1.
  - `topLevelParse: boolean` — guards secret-logger nesting during RuleSet expansion.
  - `pathContext: string[][]` — per-entity indentation stack of path segment arrays;
    reset to `[]` at the start of every entity (`visitEntity`, `:269`).
- `TextLocation { startLine, startColumn, endLine, endColumn }` — 1-based, inclusive
  columns. `sushi-ts/src/fshtypes/FshEntity.ts:53`.
- `SourceInfo { file?, location?, appliedFile?, appliedLocation? }`;
  `FshEntity.withLocation/.withFile/.withAppliedLocation/.withAppliedFile`.
  `FshEntity.ts:46`, `:7`, `:21`, `:26`. Every entity/rule/value object carries a
  `sourceInfo`.
- Grammar→type mapping is direct: each parser rule has a `visitX` method that emits
  a `fshtypes`/`fshtypes/rules` object (e.g. `cardRule`→`CardRule`,
  `caretValueRule`→`CaretValueRule`, `vsComponent`→`ValueSet*ComponentRule`). See
  §4. The generated context accessor contract is `parserContexts.ts`.
- Metadata key enums (`SdMetadataKey`, `InstanceMetadataKey`, `VsMetadataKey`,
  `CsMetadataKey`, `InvariantMetadataKey`, `MappingMetadataKey`, plus `Flag`) drive
  "already declared" dedup. `FSHImporter.ts:75`–`130`. `FLAGS=['MS','SU','?!','TU','N','D']`
  `:132`; `INDENT_WIDTH=2` `:133`; `DEFAULT_START_COLUMN=1` `:134`;
  `aliasRegex=/^\$?[a-zA-z0-9_\-\.]+$/` `:135` (note the literal `a-z` AND `A-z`
  range typo — reproduce verbatim for parity).

---

## 4. Algorithms & control flow

### 4.1 Lexer (FSHLexer.g4) — order-sensitive token rules

ANTLR lexing is longest-match, first-rule-wins on ties; rule order matters.

- Keyword tokens are `'Word' WS* ':'` (e.g. `KW_PROFILE: 'Profile' WS* ':'`),
  so the colon (and surrounding whitespace) is *part of the token*. `FSHLexer.g4:10`–`33`.
- **`STAR` swallows the preceding line break + leading indentation**:
  `STAR: ([\r\n] | LINE_COMMENT) WS* '*' [  ];` `FSHLexer.g4:65`. So a rule's
  `*` token text is e.g. `"\n    * "`. This is how indentation is later recovered
  (§4.5). A space OR non-breaking space (` `) must follow the `*`.
- `STRING` allows escapes `\u \r \n \t \" \\`; `MULTILINE_STRING: '"""' .*? '"""'`;
  `CONCEPT_STRING` is a quoted-code form. `FSHLexer.g4:73`,`:76`,`:88`.
- `CODE: SEQUENCE? '#' (SEQUENCE | CONCEPT_STRING)` — URLs with fragments
  (`http://x#y`) lex as a single `CODE`; this is why `alias` accepts `CODE` as a
  value (`FSH.g4:15`) and aliases read `SEQUENCE() ?? CODE()`. `FSHLexer.g4:85`.
- `NUMBER`, `UNIT` (`'…'`), `DATETIME`, `TIME`, `CARD` (`a..b`), `REFERENCE`,
  `CODEABLE_REFERENCE`, `CANONICAL`, `CARET_SEQUENCE` (`^` + non-ws), `REGEX`,
  then `BLOCK_COMMENT` (skipped), then catch-all `SEQUENCE: NONWS+`. Order:
  `BLOCK_COMMENT` *precedes* `SEQUENCE` deliberately so `/*…*/` is not a SEQUENCE.
  `FSHLexer.g4:79`–`117`.
- `WHITESPACE -> channel(HIDDEN)` (kept, used by error listener token scan);
  `LINE_COMMENT: '//' …[\r\n] -> skip` (note: requires a trailing newline — this
  drives the EOF-newline workaround §4.2). `FSHLexer.g4:127`–`128`.
- **Lexer modes (stack-based)**: `KW_RULESET`/`KW_INSERT` push `RULESET_OR_INSERT`
  (emit `RULESET_REFERENCE` and popMode, or `PARAM_RULESET_REFERENCE` ending in `(`
  pushing `PARAM_RULESET_OR_INSERT`); `KW_CONTEXT` pushes `LIST_OF_CONTEXTS`;
  `KW_CHARACTERISTICS` pushes `LIST_OF_CODES`. List modes emit comma-terminated
  items (`..._CONTEXT`, `CODE_ITEM`) and a final non-comma item that pops.
  `FSHLexer.g4:130`–`151`. A Rust lexer must replicate this mode stack exactly;
  the comma is baked into the token text and stripped later (`.slice(0,-1)`).

### 4.2 Driver `import()` — two passes (`FSHImporter.ts:163`)

Order is significant:

1. `allAliases = new Map()`.
2. **Pass 1 (preprocess), per file in input order** (`:168`):
   a. `new FSHDocument(rawFSH.path)`, push to `docs`, set `currentDoc/currentFile`.
   b. **Append `\n` if content doesn't end in one** (`:181`) — required so a trailing
      `LINE_COMMENT` tokenizes (it needs a following newline). Reproduce exactly,
      since it shifts EOF position/spans.
   c. `ctx = parseDoc(content, path)` — full lex+parse (`:182`); store context.
   d. Walk `ctx.entity()`: for each `alias()`, collect name + value
      (`SEQUENCE() ?? CODE()`); for each `paramRuleSet()`, call `visitParamRuleSet`
      now (so param RuleSets are available before main pass). `:186`–`223`.
3. Pass-1 alias rules (`:195`–`218`), in order:
   - name containing `|` → **error**, skip. `:195`.
   - name failing `aliasRegex` → **warn** (still defined). `:201`.
   - name already in `allAliases` with a *different* value → **error**, keep
     original (first wins). `:207`. Same value → silently ignored (no-op set).
   - else set in both `allAliases` and `doc.aliases`.
4. `logger.info("Preprocessed N documents with M aliases.")` `:227`.
5. **Pass 2 (main), per stored context in order** (`:230`): set `currentDoc/File`,
   `visitDoc(context)`.
6. `logger.info("Imported X definitions and Y instances.")` `:238`–`249`.
7. return `docs`.

`visitDoc` iterates entities; each `visitEntity` is wrapped in try/catch that logs
`Error in parsing: <msg>` with the entity span (`:256`–`264`). `visitEntity` first
**resets `pathContext=[]`** (`:269`) then dispatches by present sub-context, in this
fixed order: profile, extension, resource, logical, instance, valueSet, codeSystem,
invariant, ruleSet, mapping (`:272`–`292`). Note: `alias` and `paramRuleSet` are
*not* dispatched in pass 2 (already handled in pass 1).

### 4.3 Entity visiting & metadata dedup

Each `visitX` (profile `:295`, extension `:310`, resource `:369`, logical `:384`,
instance `:446`, valueSet `:525`, codeSystem `:594`, invariant `:645`, ruleSet
`:700`, mapping `:765`):
1. Construct the typed object from `ctx.name().getText()` with location+file.
2. **Cross-file duplicate check** `this.docs.some(doc => doc.<map>.has(name))` →
   if found, `logger.error("Skipping …: a … named N already exists.")` and **do not
   add** (first definition across the whole tank wins). `:299`, `:314`, etc.
3. Else `parseX(...)` to fill metadata+rules, then `currentDoc.<map>.set(name, obj)`.

Metadata parsing (`parseProfileOrExtension` `:335`, `parseResourceOrLogical` `:412`,
`parseInstance` `:469`, `parseValueSet` `:540`, `parseCodeSystem` `:609`,
`parseInvariant` `:660`, `parseMapping` `:780`): map each metadata ctx to
`{key, value, context}` via `visitXMetadata`, then iterate; a repeated key →
`logger.error("Metadata field 'K' already declared with value 'V'.")` and skip
(first wins). `:344`. Unknown keys are produced with `ctx.getText()` but only Known
keys are assigned. Rules are visited after metadata and pushed in source order.

Entity-specific extras:
- Extension `Context` and Logical `Characteristics` are parsed *after* the shared
  metadata loop; a second `Context`/`Characteristics` declaration errors
  ("Metadata field 'X' already declared."). `:321`–`330`, `:398`–`407`.
- Instance: missing `InstanceOf` throws `RequiredMetadataError` (caught at `:460`,
  logged). If `instanceOf` ∈ `CONFORMANCE_AND_TERMINOLOGY_RESOURCES` and no `Usage`
  given → warn about default `#example`. `:502`–`516`.

Metadata-value extraction: `Id`/`Parent`/`InstanceOf`/`Source` → `name().getText()`
(Parent/InstanceOf/Source are **alias-aware** `:930`,`:948`,`:991`; Id is not
`:926`). `Title`/`XPath`/`Target` → `extractString`. `Description`/`Expression` →
string or multiline. `Usage` → `visitUsage` (`:951`): parse code, warn if a system
was specified, `upperFirst`, validate against `isInstanceUsage`, else error +
default `Example`. `Severity` → `FshCode` (`:983`).

### 4.4 Rule dispatch (grammar→type)

Per-entity rule wrappers select by present sub-context and return arrays that are
flattened into the entity's `rules`:
- `visitSdRule` `:1163` → cardRule|flagRule|valueSetRule|fixedValueRule|onlyRule|
  containsRule|caretValueRule|obeysRule|insertRule|pathRule; pathRule returns `[]`
  (side-effect only: sets context). Unknown → `logger.warn("Unsupported rule: …")`.
- `visitLrRule` `:1064` → addElementRule|addCRElementRule|sdRule.
- `visitInstanceRule` `:1195` → fixedValueRule|insertRule|pathRule(isInstanceRule=true).
- `visitVsRule` `:1205`, `visitCsRule` `:1250`, `visitInvariantRule` `:1260`,
  `visitMappingEntityRule` `:1271` (special: a `pathRule` whose text contains `->`
  re-reports the missing-space mapping error `:1278`).

Notable per-rule construction:
- `cardRule` → `[CardRule, FlagRule?]` (flags on a card produce a *second*
  `FlagRule` with the same path). `visitCardRule:1320`. `parseCard` splits on `..`;
  `min=parseInt(parts[0])`, `max=parts[1]` (string). Empty both sides → error `:1341`.
- `flagRule` may have multiple paths (`STAR path (KW_AND path)* flag+`); emits one
  `FlagRule` per path. `:1355`. Flags map: MS→mustSupport, SU→summary, `?!`→modifier,
  TU→trialUse, N→normative, D→draft. `parseFlags:1365`, `visitFlag:1387`.
- `valueSetRule` → `BindingRule`; `valueSet = aliasAwareValue(name)`,
  `strength = visitStrength ?? 'required'`. `:1404`.
- `fixedValueRule` → `AssignmentRule`; sets `value` via `visitValue`, keeps
  `rawValue` for NUMBER/bool, `exactly`, and `isInstance = name present AND not an
  alias`. `:1424`.
- `onlyRule`/addElement target types via `parseTargetType:1680` (Reference/
  CodeableReference split on ` or `; canonical keeps `|version`; all alias-aware).
- `containsRule` → `[ContainsRule, CardRule, FlagRule?]*` — each item generates a
  synthetic CardRule (and FlagRule if flagged) with path `parent[itemName]`.
  `named` form sets `{type, name}`, else `{name}`. `:1718`.
- `caretValueRule`/`codeCaretValueRule` → `CaretValueRule` with `pathArray`,
  `caretPath` (leading `^` sliced off), value, rawValue, isInstance. CodeCaret keeps
  system only when `keepSystem` (ValueSet context). `:1758`, `:1785`.
- `concept` → `ConceptRule` with `code`, `hierarchy` (preceding codes, `#` stripped),
  `display`/`definition`; if any listed code has a system → error unless it's a
  RuleSetRule that could later be a VS concept component (then keep `system`).
  `:1521`.
- `mappingRule` → `MappingRule` (`map`=STRING[0], optional `comment`, optional CODE).
- `obeysRule` → one `ObeysRule` per invariant name. `:1817`.

### 4.5 Source spans — `extractStartStop` (`:2511`) — order-critical geometry

Three cases:
1. **StarContext** (rule has a `STAR()` terminal; detected by `isStarContext`,
   `parserContexts.ts:563`):
   - `startLine = STAR.symbol.line + 1` (because the STAR token *starts on the prior
     line's newline*, §4.1). `:2514`.
   - `startColumn = getStarContextStartColumn` = `STAR.text.length -
     STAR.text.lastIndexOf('\n') - 2` — i.e. count of chars after the last newline
     up to and including `*`, converted to the 1-based column of the `*`. `:2547`.
   - `endLine = stop.line`; `endColumn = stop.stop - stop.start + stop.column + 1`.
   - **Side-effect**: if not suppressed and the rule has no path/code-path
     (`!containsPathContext && !containsCodePathContext`) yet `startColumn>1`
     (indented), logs *"A rule that does not use a path cannot be indented…"*. `:2519`.
2. **TerminalNode**: span from the single token (`symbol.line`, `column+1` … ).
   `:2530`.
3. **Otherwise** (normal ctx): `start.line`/`start.column+1` … `stop` end col. `:2537`.

Columns are 1-based (`+1`); end column is inclusive of last char. A Rust port must
reproduce the `+1` on startLine for star rules and the `length - lastIndexOf('\n') -
2` indent formula bit-for-bit, since these spans appear in diagnostics and in
`appliedLocation` math.

### 4.6 Path context / soft indexing (`prependPathContext` `:2284`)

State: `pathContext: string[][]`, reset per entity. For each rule carrying a path
(`getPathWithContext`/`getArrayPathWithContext` `:1292`/`:1302`):
1. `currentIndent = startColumn - 1`; `contextIndex = currentIndent / 2`.
2. `isValidContext` (`:2364`): first rule must be left-aligned (indent>0 with empty
   context → error, return path as-is); indent must be a non-negative multiple of 2
   (else error); cannot indent deeper than `existingContext.length+1` (else error).
   Any failure → use the raw path.
3. `contextIndex===0` → reset `pathContext=[path]`. If the previous top context still
   contained an unconsumed `[+]`, warn that soft indices won't increment. `:2301`.
4. `['.']` while indented → error ("special '.' path only at top level"). `:2323`.
5. else prepend `pathContext[contextIndex-1]` to `path`; if that parent context is
   empty (parent rule had no path) → error. Splice off out-of-scope deeper contexts;
   push the new full path. `:2334`–`2351`.
6. **`finally` (always runs)**: if `!isPathRule` OR (pathRule AND on an Instance),
   replace every `[+]`→`[=]` across all stored contexts so soft-add fires only once.
   `:2352`–`2361`. This mutation-in-finally is the crux of soft-indexing semantics.

ValueSet concept/filter components also drive context: a single-concept component
sets context to `system#code` (with `suppressError=true`); multi-concept or filter
resets to `[]`. `visitVsComponent:2086`–`2105`.

### 4.7 Alias resolution (`aliasAwareValue` `:2268`)

Split value on first `|` into `value|version`. `validateAliasResolves` (`:2258`):
if value starts with `$` and is not a known alias → error. If `valueWithoutVersion`
∈ `allAliases`, return `resolved + (version? '|'+version : '')`; else return original.
Codes resolve their *system* alias-aware via member `parseCodeLexeme` (`:1056`).

### 4.8 Parameterized RuleSet expansion

- Pass 1 stores `ParamRuleSet { name, parameters, contents }`. `contents` is the raw
  text between the first `*` and end, captured via
  `ctx.start.getInputStream().getText(start, stop)` (`visitParamRuleSetContent:761`).
  Unused parameters → warn `:748`.
- On an `insert`/`codeInsert` referencing a parameterized RuleSet
  (`applyRuleSetParams:1926`): parse name+params (`parseRuleSetReference:1870`,
  `parseInsertRuleParams:1881` — bracketed `[[…]]` vs plain params, with specific
  unescaping of `]],`/`]])`/`\,`/`\)`/`\\`). Key the expansion by
  `JSON.stringify([name, ...params])` into `currentDoc.appliedRuleSets`. Param-count
  mismatch → error; unknown RuleSet → error. `:1941`–`1987`.
- Expansion text is produced by `MiniFSHImporter.transformRuleSet` (`MiniFSHImporter.ts:32`):
  re-lex with MiniFSHLexer, substitute `{param}` / `[[{param}]]` occurrences with
  values (bracket-aware escaping inside `[[…]]` zones), re-emit `RuleSet: name\n…`.
  Uses `EOL` from `os` (platform line ending) — **portability hazard**, see §5.
- `parseGeneratedRuleSet:1992` re-enters `parseDoc`+`visitDoc` on the generated text
  with `currentDoc` swapped to a temp doc and `pathContext` saved/restored. The
  *first* level of recursion flips `topLevelParse=false` and switches the logger into
  a "secret" collecting mode (`switchToSecretLogger`), then re-emits collected
  errors/warnings prefixed `Error(s)/Warning(s) parsing insert rule with
  parameterized RuleSet <name>` against the insert rule's sourceInfo
  (`restoreMainLogger`). `:1999`–`2044`. Applied-rule spans are rebased onto the
  original RuleSet's start line (`:1956`–`1962`).

### 4.9 Lexeme normalization

- `unescapeQuotedString` (`:2402`): strip quotes, split on `\\`, within each segment
  unescape `\uXXXX`,`\"`,`\n`,`\r`,`\t`, rejoin with literal `\`. The `\uXXXX` is
  decoded via `JSON.parse('"\\uXXXX"')` (`unescapeUnicode:137`). Regex char class is
  `[A-F,a-f,0-9]` — includes a literal comma (harmless but reproduce). `:2410`.
- `extractMultilineString` (`:2427`): strip `"""`, split lines, unescape per-line
  (note: **no `\"` or `\\` handling** here, unlike single-line), drop first/last
  whitespace-only lines, blank out whitespace-only interior lines, compute min
  leading spaces over non-empty lines and trim that prefix, join with `\n`. `:2436`–`2471`.
- `extractNumberValue` (`:2474`): integers → `BigInt` (FHIR integer64), with careful
  trailing-zero/exponent logic deciding bigint vs `parseFloat`. Quantity/ratio
  numbers instead use plain `parseFloat` (`:1572`,`:1612`). Port must keep both code
  paths distinct.
- `parseCodeLexeme` (free, `parseCodeLexeme.ts:11`): split system#code at first
  unescaped `#` via `/(^|[^\\])(\\\\)*#/`; unescape `\\`→`\` and `\#`→`#` in system;
  if code is quoted, strip quotes and unescape `\\`,`\"`.

### 4.10 Error listener (`FSHErrorListener.ts`)

`syntaxError` → `buildErrorMessage` (`:27`) then `logger.error`. Default location is
`{startLine:line, startColumn:column+1, endLine:line, endColumn:column +
(offendingSymbol.text.length ?? 1)}` (`:35`) — **note end col uses
`column + len` (no `+1`), differing from `extractStartStop`**. It then pattern-matches
the raw ANTLR message + neighboring tokens (via `getPreviousNonWsToken` scanning the
hidden-channel token stream backwards, `:262`) to substitute friendlier messages for:
missing spaces around `=`/`->`, missing space after `*`, deprecated `Mixins:`/`units`/
`|`-reference/`,`-list syntax. Each branch may relocate the span to the previous
token (`getTokenLocation:276`). Order of the `if/else` chain is significant —
first match wins. `:58`–`252`.

---

## 5. Edge cases & gotchas

- **Aliases & param RuleSets are global, entities are namespaced but checked
  globally too.** Aliases/`paramRuleSets` live on the importer across all files
  (`:164`,`:159`); entity duplicate checks use `this.docs.some(...)` so a name
  collision *anywhere* in the tank skips the later one. First-wins everywhere.
  (`:299` etc., alias `:207`.)
- **STAR token eats the preceding newline + indent.** Therefore `startLine` for any
  `*` rule is `STAR.symbol.line + 1` and the indent comes from the STAR text, not
  the parser column. A naive lexer that doesn't fold the newline into `*` will get
  every rule span and every indentation wrong. `FSHLexer.g4:65`, `:2514`, `:2547`.
- **Trailing-newline append.** Files not ending in `\n` get one appended before
  parsing (`:181`) so trailing `//` comments tokenize. Affects EOF span.
- **Two different span formulas.** `extractStartStop` end column is
  `stop.stop - stop.start + stop.column + 1`; the error listener's default end col is
  `column + text.length` (no `+1`). Don't unify them. `:2542` vs `FSHErrorListener.ts:39`.
- **Soft-index mutation happens in a `finally`.** `[+]`→`[=]` rewrite runs even when
  the function returns early through an error branch, and is gated on
  `!isPathRule || isInstanceRule`. Path rules on non-instances deliberately *don't*
  consume `[+]`. `:2352`.
- **`pathContext` reset per entity**, not per file. Cross-rule context only lives
  within one entity. `:269`.
- **Integer vs decimal numbers.** Assignment/caret values run through
  `extractNumberValue` → may be `bigint`; quantities/ratios use `parseFloat`. Also
  `rawValue` is preserved for NUMBER/bool to handle Instance ids that look numeric.
  `:1431`, `:2474`.
- **`isInstance` heuristic.** An assignment/caret whose value is a bare `name` that is
  *not* a defined alias is flagged `isInstance=true` — alias state therefore changes
  parse output, so pass-1 alias collection must precede this. `:1439`, `:1778`.
- **CodeCaret/codeInsert `keepSystem` differs by host.** On ValueSets the system is
  retained in the path array; elsewhere only `#code`. `:1785`, `:1841`.
- **Concept-with-system ambiguity in RuleSets.** A concept listed with a system inside
  a `ruleSetRule` is tentatively kept (may be a VS component later); the same concept
  in a CodeSystem errors. `:1548`–`1564`.
- **VS filter operator spelling fix.** `descendant`→`descendent` and lowercasing
  applied to the operator text before matching `VsOperator`. `:2196`.
- **`Reference(a or b)` / `Canonical(u|v or …)` are single lexer tokens**, then split
  textually on `\s+or\s+` (and `|`). The grammar explicitly rejects `|`-separated
  references with a special error. `FSHLexer.g4:100`,`:106`; split `:1641`,`:1648`.
- **List tokens carry their comma.** Context/characteristics/param items lex with a
  trailing `,`/`)` that is sliced off (`.slice(0,-1)`), and whitespace trimmed.
  `:1004`,`:1048`,`:1874`.
- **`EOL`/platform line endings.** `MiniFSHImporter` and the secret-logger
  re-emission join with Node's `os.EOL`. On Windows this changes generated FSH text
  and thus downstream spans. For deterministic parity the Rust port should pin `\n`
  (and a parity test should confirm stock SUSHI's behavior on the target platform).
  `MiniFSHImporter.ts:41`, `FSHImporter.ts:2029`.
- **`aliasRegex` has a typo range `a-zA-z`** (`A-z` spans extra ASCII like `[`,`\`,`_`).
  Reproduce verbatim to match which alias names trigger the warning. `:135`.
- **Multiline vs single-line unescaping differ.** Multiline does not process `\"` or
  `\\`. `:2436` vs `:2402`.
- **Error recovery is lenient.** Many visitors null-check `ctx` and return `undefined`
  rules (filtered out by the pushers) because ANTLR error recovery can yield partial
  trees; e.g. `visitValue` returns early on null `ctx` (`:1446`). The diagnostics from
  `FSHErrorListener` are what surface the syntax error.

---

## 6. Recommended Rust mapping

Primary crate: **`fsh_lexer_parser`** (lexer + parser + AST visitor). It produces
**`fsh_model`** types (the entity/rule structs, `TextLocation`, `SourceInfo`,
`FSHDocument`). Diagnostics route through **`diagnostics`** (the logger + error
listener message catalog). The driver/orchestration that owns the two-pass alias/
RuleSet state can live in `fsh_lexer_parser` and be invoked by **`compiler`**.

- **Lexer**: hand-write or use a generator that supports a **mode stack** (ANTLR
  `pushMode`/`popMode`). Reproduce: STAR-folds-newline, comma-bearing list tokens,
  `RULESET_OR_INSERT`/`PARAM_RULESET_OR_INSERT`/`LIST_OF_CONTEXTS`/`LIST_OF_CODES`
  modes, hidden-channel whitespace (the error listener needs it). Tokens should carry
  byte offset, 0-based line, 0-based column, and text (UTF-16 length semantics matter
  for column math — see Open Questions).
- **Parser**: an LL recursive-descent matching `FSH.g4` rule order; or port the
  generated tables. Build a CST with the same context-accessor shape that
  `parserContexts.ts` documents (children-by-rule). Keep terminal tokens addressable
  for span math.
- **AST/visitor**: one `visit_*` per parser rule returning the corresponding
  `fsh_model` struct(s). Use `Vec` for ordered rule lists; use `IndexMap<String, T>`
  for `FSHDocument` maps to preserve insertion order (Maps are order-sensitive
  downstream). Represent number values as an enum `Number { Int(i128/BigInt), Float(f64) }`
  to mirror bigint/float; keep `raw_value: Option<String>`.
- **State**: importer struct holding `all_aliases: IndexMap<String,String>`,
  `param_rule_sets: IndexMap<String,ParamRuleSet>`, `path_context: Vec<Vec<String>>`,
  `top_level_parse: bool`, plus `docs: Vec<FSHDocument>`. Mirror the two-pass driver.
- **Spans**: `TextLocation { start_line, start_column, end_line, end_column }` all
  1-based; centralize the three-case `extract_start_stop` and the
  `getStarContextStartColumn` formula. Keep a separate span builder for the error
  listener (different end-column convention).
- **Diagnostics**: a message catalog mirroring `FSHErrorListener` substitutions and
  the importer's `logger.error/warn/info` strings verbatim (downstream golden tests
  compare message text). Implement the "secret logger" as a collecting sink toggled
  during nested RuleSet parsing.
- **MiniFSH**: a small second lexer/parser + regex-based substitution matching
  `MiniFSHImporter`. Pin newline to `\n` (document the deviation from `os.EOL`).
- Neighbors: emits `FSHDocument[]` consumed by `FSHTank`/exporters (out of scope);
  consumes `RawFSH { content, path }` from the file loader; alias/`isInstance`
  decisions couple lexing to global alias state (pass ordering is a hard contract).

---

## 7. Parity test ideas

Compare against stock SUSHI by feeding identical `RawFSH[]` and diffing (a) the
`FSHDocument` JSON (entities, rules, `sourceInfo`) and (b) the ordered logger
output (level + message + file + location).

1. **Span geometry**: a profile with rules at indents 0/2/4 spaces, a rule preceded
   by a blank line and by a `// comment` line, and a rule whose `*` is at column >1
   with no path → assert `startLine = STAR.line+1`, exact `startColumn`, and the
   "rule that does not use a path cannot be indented" error.
2. **Trailing newline / EOF comment**: a file ending in `// comment` with no final
   newline vs with one → identical parse, no spurious syntax error.
3. **Alias semantics**: redefine an alias to a different value (error, first wins);
   redefine to same value (silent); `|`-containing name (error); name with illegal
   char (warn); a `$X` reference that doesn't resolve (error). Confirm a bare-name
   assignment becomes `isInstance` only when not an alias.
4. **Soft indexing**: nested instance rules using `[+]`/`[=]` across path rules vs
   assignment rules, plus an unconsumed `[+]` reset by a left-aligned rule (warn).
   Diff the resulting `pathArray`/`path` strings.
5. **Parameterized RuleSet**: a RuleSet with bracketed `[[ ]]` params containing
   commas/parens and escapes, inserted twice with same and different args → check
   `appliedRuleSets` keys (`JSON.stringify([name,...params])`), expanded text, rebased
   line numbers, and the aggregated "Error(s)/Warning(s) parsing insert rule…" message
   when the body is malformed. Param-count mismatch and unknown-RuleSet errors.
6. **Number fidelity**: `* x = 1e3`, `0.1000`, `9999999999999999999`, negative
   exponents, vs quantity `5 'mg'` → assert bigint vs float typing and `rawValue`.
7. **Strings**: escapes `é \" \\ \n`, multiline with mixed indentation and
   blank lines, and the multiline-vs-singleline unescape difference.
8. **Codes/quantities/references/canonicals**: `SYSTEM#code "disp"`, `#"quoted code"`,
   `http://x#frag` alias value, `Reference(A or B)` (first used + error),
   `Canonical(url|1.0 or url2)`.
9. **VS components**: concept with/without system, `codes from system X where p = v`,
   filter operator `descendant-of` normalization, missing-system error.
10. **Error-listener catalog**: each substituted-message branch (`MyAlias =x`,
    `* a=true`, `* a -> "b"`, `*active`, `Mixins:`, `units`, `Reference(A|B)`,
    comma-lists) → byte-identical messages and relocated spans.
11. **Cross-file duplicate skip**: same profile name in two files → one kept, one
    "Skipping" error, in input order.

---

## 8. Open questions

- **Column units (UTF-16 vs bytes vs scalar values).** ANTLR/JS columns count UTF-16
  code units, and `offendingSymbol.text.length` is UTF-16 length. Rust strings are
  UTF-8. To match spans on non-ASCII input the lexer/parser must track UTF-16 code
  unit offsets for columns and token lengths. Decide whether to carry a parallel
  UTF-16 column or to constrain parity claims to ASCII. (`extractStartStop:2542`,
  `FSHErrorListener.ts:39`.)
- **BigInt range.** TS uses arbitrary-precision `BigInt` for integers; Rust needs
  `i128` or a bigint crate. Confirm whether any fixtures exceed `i128`. (`:2474`.)
- **`os.EOL` dependence.** Stock SUSHI's generated-RuleSet text and aggregated error
  text use platform EOL. Should the Rust port pin `\n` and accept a deviation on
  Windows, or replicate platform EOL for byte parity? (`MiniFSHImporter.ts:41`,
  `FSHImporter.ts:2029`.)
- **Generated parser vs hand-written.** Do we port ANTLR's exact error-recovery /
  ambiguity resolution (needed for identical syntax-error messages and partial trees)
  or accept message-level parity only? The `FSHErrorListener` substitutions depend on
  ANTLR's raw message wording and token-index neighbors.
- **`JSON.parse('"\\uXXXX"')` decode** vs Rust char decoding — confirm behavior for
  surrogate pairs / invalid escapes matches. (`unescapeUnicode:137`.)
- **Map ordering guarantees.** Downstream relies on `FSHDocument` Map insertion order
  and `docs` order; confirm the FSHTank/exporters' exact ordering requirements before
  choosing `IndexMap` vs `BTreeMap` (out-of-scope subsystem dependency).
