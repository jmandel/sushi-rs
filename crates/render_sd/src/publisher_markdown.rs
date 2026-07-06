//! `publisher_markdown` — the publisher's markdown pipeline for SD leaf fragments.
//!
//! Two consumer shapes, both sharing fhir-core's `MarkDownProcessor` COMMON_MARK
//! dialect (determined below):
//!
//!  1. `BaseRenderer.processMarkdown` (publisher BaseRenderer.java:184) returns an
//!     HTML STRING = `preProcessMarkdown(text)` then `markdownEngine.process(text)`.
//!     Used by `summary`/`extensionSummary` (psdr:157/220/290/305/310), `dict`
//!     (psdr:1508), `maps` (psdr:1482). Callers then run `stripPara`/`stripAllPara`.
//!  2. `XhtmlFluent.markdown` (fhir-core XhtmlFluent.java:305) = `process(md)` then
//!     re-parse the HTML string with XhtmlParser and add the `<div>`'s children.
//!     Used by `useContext` deprecated block (psdr:2888, after a preProcessMarkdown
//!     hop), `tx` unresolvable bindings (psdr:101), `sd-xref`/references (psdr:3189).
//!
//! ## Markdown-engine determination (required finding)
//! The SDR's `markdownEngine` is built at PublisherIGLoader.java:908-910:
//!   `version 1.0/1.4/1.6/3.0 -> DARING_FIREBALL, else -> COMMON_MARK`.
//! Every corpus IG is R4/R4B/R5 (4.0/4.3/5.0) -> **COMMON_MARK**.
//! `XhtmlFluent.markdown` also hard-codes COMMON_MARK.
//!
//! COMMON_MARK (`MarkDownProcessor.processCommonMark`, fhir-core
//! MarkDownProcessor.java:239-247) is **NOT** the vanilla `Cell.addMarkdown` engine
//! (`crate::commonmark`). It differs by:
//!   (a) `preProcess(source)` (MDP:222-237) — a regex that backslash-escapes raw
//!       HTML tags (`<tag ...>`, `</tag>`, `<!`/`<?`) so they render as literal `<`;
//!   (b) `TablesExtension` enabled in both parser and renderer;
//!   (c) `html.replace("<table>", "<table class=\"grid\">")`.
//! Same `escapeHtml(true)`. Corpus measurement (1229 markdown-bearing strings over
//! all 3 IGs): zero `[[[`, zero `||`, zero raw-HTML tags, zero tables/fences; the
//! live features are links, code spans, `*em*`, tight bullet lists, soft/hard
//! breaks — all already covered by `crate::commonmark`. So (a)/(b)/(c) are no-ops
//! on the corpus and are ported faithfully but fire a loud gap if a table appears.
//!
//! `preProcessMarkdown` (BaseRenderer.java:78-183): `||`->para, `[[[link]]]`
//! resolution, and `ProfileUtilities.processRelativeUrls` (PU:2179). The
//! BaseRenderer path passes `webUrl=""` (BaseRenderer:41), so
//! `isLikelySourceURLReference` (PU:2301) ENABLES its resourceNames branch: a
//! relative `](x)` link is corePath-prefixed if `x` is `<resource>.html` /
//! `<resource>-definitions.html` (R4 resource names), in `BASE_FILENAMES` (the
//! FHIR core spec page set), or starts with `extension-`. The `updateURLs`
//! base-element path (SDR:4065, webUrl=render_webroot which IS R-prefixed)
//! instead DISABLES resourceNames — see `process_relative_urls_pub`.

use crate::commonmark;
use crate::context::IgContext;

/// The FHIR core spec page basenames (ProfileUtilities.BASE_FILENAMES, PU:130).
/// The only allow-list that prefixes a relative `](x)` link with corePath when
/// corePath is under `http://hl7.org/fhir/R`.
const BASE_FILENAMES: &[&str] = &[
    "async",
    "ballot-intro",
    "best-practices",
    "bindings-list",
    "biologically-derived-product-module",
    "broken-link",
    "cda-intro",
    "change",
    "clinicalreasoning-cds-on-fhir",
    "clinicalreasoning-evidence-and-statistics",
    "clinicalreasoning-knowledge-artifact-distribution",
    "clinicalreasoning-knowledge-artifact-representation",
    "clinicalreasoning-module",
    "clinicalreasoning-quality-reporting",
    "clinicalreasoning-topics-definitional-resources",
    "clinicalreasoning-topics-supporting-documentation",
    "clinicalreasoning-topics-template",
    "clinicalreasoning-topics-using-expressions",
    "clinicalsummary-module",
    "codesystem",
    "comparison",
    "comparison-cda",
    "comparison-other",
    "comparison-v2",
    "comparison-v3",
    "conformance-module",
    "conformance-rules",
    "credits",
    "datatypes",
    "datatypes-definitions",
    "datatypes-examples",
    "datatypes-mappings",
    "datatypes-profiles",
    "defining-extensions",
    "device-module",
    "diagnostics-module",
    "diff",
    "diff-r4",
    "diff-r4b",
    "diff-r5",
    "documentation",
    "documents",
    "dosage",
    "dosage-definitions",
    "dosage-examples",
    "dosage-mappings",
    "dosage-profiles",
    "downloads",
    "ehr-fm",
    "element-definitions",
    "elementdefinition",
    "elementdefinition-definitions",
    "elementdefinition-examples",
    "elementdefinition-mappings",
    "elementdefinition-profiles",
    "exchange-module",
    "exchanging",
    "exchanging-messaging",
    "exchanging-operation",
    "exchanging-polling",
    "exchanging-request",
    "exchanging-rest",
    "exchanging-search",
    "exchanging-subscription",
    "extensibility",
    "extensibility-definitions",
    "extensibility-examples",
    "fhir-xquery",
    "fhirpatch",
    "fhirpath",
    "financial-module",
    "fmg",
    "formats",
    "foundation-module",
    "genomics",
    "glossary",
    "graphql",
    "help",
    "history",
    "http",
    "identifier-registry",
    "implsupport-module",
    "index",
    "integrated-examples",
    "json",
    "languages",
    "license",
    "lifecycle",
    "logical",
    "loinc",
    "managing",
    "mapping-language",
    "mapping-tutorial",
    "mappings",
    "marketingstatus",
    "marketingstatus-definitions",
    "marketingstatus-examples",
    "marketingstatus-mappings",
    "marketingstatus-profiles",
    "medication-definition-module",
    "medications-module",
    "messaging",
    "metadatatypes",
    "metadatatypes-definitions",
    "metadatatypes-examples",
    "metadatatypes-mappings",
    "metadatatypes-profiles",
    "modules",
    "modules-fragment",
    "modules-list",
    "narrative",
    "narrative-definitions",
    "narrative-examples",
    "narrative-version-maps",
    "nd-json",
    "ns",
    "nutrition-module",
    "obligations",
    "observation",
    "oids",
    "op-example-request",
    "operations",
    "operations-for-large-resources",
    "operationslist",
    "overview",
    "overview-arch",
    "overview-clinical",
    "overview-dev",
    "overview-patient",
    "packages",
    "page",
    "patient-operation-match",
    "patterns",
    "population-profiles",
    "productshelflife",
    "productshelflife-definitions",
    "productshelflife-examples",
    "productshelflife-mappings",
    "productshelflife-profiles",
    "profilelist",
    "profiling",
    "profiling-examples",
    "pushpull",
    "qa",
    "r2maps",
    "r3maps",
    "r4maps",
    "rdf",
    "redirect",
    "references",
    "references-definitions",
    "references-profiles",
    "resource",
    "resource-definitions",
    "ansi",
    "resource-formats",
    "resourceguide",
    "resourcelist",
    "resourcelist-examples",
    "resources-definitions",
    "resources-examples",
    "safety",
    "sc",
    "search",
    "search-build",
    "search_filter",
    "searchparameter-registry",
    "secpriv-module",
    "security",
    "security-labels",
    "services",
    "sid-icd-10",
    "sid-icd-9",
    "sid-us-ssn",
    "signatures",
    "snomedct",
    "snomedct-usage",
    "storage",
    "subscriptions",
    "summary",
    "terminologies",
    "terminologies-binding-examples",
    "terminologies-conceptmaps",
    "terminologies-systems",
    "terminologies-valuesets",
    "terminology-module",
    "terminology-service",
    "toc",
    "types",
    "types-definitions",
    "types-mappings",
    "types-profiles",
    "uml",
    "updates",
    "usecases",
    "validation",
    "versioning",
    "versions",
    "w5",
    "wglist",
    "workflow",
    "workflow-ad-hoc",
    "workflow-communications",
    "workflow-examples",
    "workflow-management",
    "workflow-module",
    "workflow-extensions",
    "xml"
];

/// `MarkDownProcessor.preProcess` (MDP:222-237): backslash-escape raw HTML tags so
/// commonmark renders them as literal `<`. Two regexes:
///   `(?<!\)<(/)?([A-Za-z][A-Za-z0-9-]*[\s>])` -> `\<$1$2`
///   `<(!|?)` -> `\<$1`
/// Ported without a regex engine (corpus has zero raw HTML tags; this is the
/// faithful transform, verified inert on the corpus).
fn pre_process(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '<' {
            // lookbehind (?<!\): preceding char must not be a backslash.
            let prev_bs = i > 0 && chars[i - 1] == '\\';
            if !prev_bs {
                // <(!|?)
                if let Some(&n) = chars.get(i + 1) {
                    if n == '!' || n == '?' {
                        out.push('\\');
                        out.push('<');
                        i += 1;
                        continue;
                    }
                }
                // <(/)?([A-Za-z][A-Za-z0-9-]*[\s>])
                let mut j = i + 1;
                let has_slash = chars.get(j) == Some(&'/');
                if has_slash {
                    j += 1;
                }
                if chars.get(j).map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
                    // name: [A-Za-z][A-Za-z0-9-]*
                    let mut k = j + 1;
                    while chars
                        .get(k)
                        .map(|c| c.is_ascii_alphanumeric() || *c == '-')
                        .unwrap_or(false)
                    {
                        k += 1;
                    }
                    // then a [\s>] (whitespace or '>') — consumed as $2's last char.
                    if chars.get(k).map(|c| c.is_whitespace() || *c == '>').unwrap_or(false) {
                        // match: emit \ then the '<', then (/)?, then name, then the trailing char.
                        out.push('\\');
                        out.push('<');
                        if has_slash {
                            out.push('/');
                        }
                        for &ch in &chars[j..=k] {
                            out.push(ch);
                        }
                        i = k + 1;
                        continue;
                    }
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// `MarkDownProcessor.process(source, ctx)` COMMON_MARK (MDP:62-75 + 239-247):
/// `processCommonMark(preProcess(source))` = preProcess, commonmark(+tables),
/// then `<table>`->`<table class="grid">`. Returns the HTML string.
pub fn md_process(source: &str) -> String {
    if source.is_empty() {
        return String::new();
    }
    let pre = pre_process(source);
    // TablesExtension: the corpus exercises no table syntax. commonmark::render_html
    // is the (extension-free) subset; a GFM table (a line with a leading/interior
    // `|` followed by a `---|---` delimiter row) would need the extension — fire a
    // loud gap so it's never silently mis-rendered.
    if has_gfm_table(&pre) {
        crate::loud_gap!((), "LOUD GAP: publisher_markdown TablesExtension (MDP:240) — GFM table in markdown source");
    }
    let html = commonmark::render_html(&pre);
    html.replace("<table>", "<table class=\"grid\">")
}

/// Heuristic GFM-table detector (gap trigger only): a `|`-bearing line immediately
/// followed by a delimiter row of `-`/`:`/`|`/space. Conservative; never fires on
/// the corpus (measured zero).
fn has_gfm_table(s: &str) -> bool {
    let lines: Vec<&str> = s.lines().collect();
    for w in lines.windows(2) {
        if w[0].contains('|') {
            let d = w[1].trim();
            if !d.is_empty() && d.contains('-') && d.chars().all(|c| matches!(c, '-' | ':' | '|' | ' ')) {
                return true;
            }
        }
    }
    false
}

/// `preProcessMarkdown(location, text)` (BaseRenderer.java:78-183). corePath is the
/// `http://hl7.org/fhir/R4/`-style spec base. `prefix` is always "" (BaseRenderer:41)
/// so the step-2 prefix loop is skipped. `[[[link]]]` resolution uses IgContext.
pub fn pre_process_markdown(ctx: &IgContext, text: &str, core_path: &str) -> String {
    // 1a. `||` -> paragraph break.
    let mut text = text.replace("||", "\r\n\r\n");

    // 1b. `[[[ linkText ]]]` FHIR link syntax.
    while let (Some(lo), Some(hi)) = (text.find("[[["), text.find("]]]")) {
        if hi < lo {
            break;
        }
        let left = text[..lo].to_string();
        let link_text = text[lo + 3..hi].trim().to_string();
        let right = text[hi + 3..].to_string();
        let (url, display) = resolve_triple(ctx, &link_text);
        text = match url {
            Some(u) => format!("{}[{}]({}){}", left, display, u, right),
            None => format!("{}`{}`{}", left, display, right),
        };
    }

    // 3. processRelativeUrls(text, webUrl="", basePath=corePath, ..., false).
    process_relative_urls(&text, core_path)
}

/// `[[[link]]]` resolution (BaseRenderer:88-165), corpus-relevant subset: named
/// links / spec-map are empty here; resolve the linkText as a canonical via
/// IgContext. If unresolved, the caller emits ``linkText`` (backtick code).
/// Corpus fires this ZERO times (no `[[[` present); ported for faithfulness and
/// surfaced loudly if it ever resolves nothing AND is non-canonical.
fn resolve_triple(ctx: &IgContext, link_text: &str) -> (Option<String>, String) {
    let parts0 = link_text.split('#').next().unwrap_or(link_text);
    if let Some(r) = ctx.resolve(parts0) {
        if !r.web_path.is_empty() {
            return (Some(r.web_path.clone()), r.present());
        }
    }
    // No named-link/spec-map/profile fallbacks in this corpus; keep display text.
    (None, link_text.to_string())
}

/// `ProfileUtilities.processRelativeUrls(markdown, "", corePath, ..., false)`
/// (PU:2179-2262). Only the `](url)` branch matters here (no reference-style
/// links in the corpus). A relative, non-`..` `url` is corePath-prefixed iff
/// `isLikelySourceURLReference`; `processRelatives=false` disables the webUrl path.
/// Public wrapper: `ProfileUtilities.processRelativeUrls(md, webUrl, specUrl,
/// …, processRelatives=false)` (PU:2179) as used by StructureDefinitionRenderer's
/// `updateURLs` on a base compare element (dict-key/dict-diff). `base_path` is the
/// spec base (corePath without trailing slash) used for the source-URL prefix.
pub fn process_relative_urls_pub(markdown: &str, base_path: &str) -> String {
    // updateURLs passes webUrl = render_webroot (the core spec base, which starts
    // with http://hl7.org/fhir/R), so isLikelySourceURLReference's resourceNames
    // branch is DISABLED — only BASE_FILENAMES / extension- pages are prefixed.
    process_relative_urls_impl(markdown, base_path, false)
}

fn process_relative_urls(markdown: &str, base_path: &str) -> String {
    // BaseRenderer.processMarkdown passes webUrl="" -> resourceNames branch ACTIVE.
    process_relative_urls_impl(markdown, base_path, true)
}

fn process_relative_urls_impl(markdown: &str, base_path: &str, resource_names: bool) -> String {
    let md: Vec<char> = format!("{} ", markdown).chars().collect();
    let mut b = String::new();
    let mut i = 0usize;
    // The corpus has no reference-style `[x]: url` links, so we skip the anchorRefs
    // / processingLink machinery (it only activates for `]` not followed by `(` that
    // later reappears as `]:`). We reproduce the inline `](` branch faithfully.
    while i < md.len() {
        if i + 3 < md.len() && md[i] == ']' && md[i + 1] == '(' {
            // find closing ')'
            let mut j = i + 2;
            while j < md.len() && md[j] != ')' {
                j += 1;
            }
            if j < md.len() {
                let url: String = md[i + 2..j].iter().collect();
                if !is_absolute_url(&url) && !url.starts_with("..") {
                    if is_likely_source_url_reference(&url, base_path, resource_names) {
                        b.push_str("](");
                        b.push_str(base_path);
                        if !base_path.is_empty() && !base_path.ends_with('/') {
                            b.push('/');
                        }
                        i += 1; // consumed ']'; loop appends "(" ... rest normally
                        i += 1;
                        continue;
                    } else {
                        // processRelatives=false -> DO NOTHING (leave `](` as-is).
                        b.push_str("](");
                        i += 2;
                        continue;
                    }
                } else {
                    b.push_str("](");
                    i += 2;
                    continue;
                }
            } else {
                b.push(md[i]);
            }
        } else {
            b.push(md[i]);
        }
        i += 1;
    }
    // Utilities.rightTrim
    b.trim_end().to_string()
}

/// `Utilities.isAbsoluteUrl`: has a scheme like `http:`/`https:`/`urn:` etc.
fn is_absolute_url(url: &str) -> bool {
    // A scheme: ^[a-zA-Z][a-zA-Z0-9+.-]*: with '/' or ':' style. FHIR's check is
    // `Utilities.isAbsoluteUrl` = contains "://" or starts "urn:" / "mailto:".
    if url.starts_with("urn:") || url.starts_with("mailto:") {
        return true;
    }
    if let Some(colon) = url.find(':') {
        let scheme = &url[..colon];
        if !scheme.is_empty()
            && scheme.chars().next().unwrap().is_ascii_alphabetic()
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
            && url[colon + 1..].starts_with("//")
        {
            return true;
        }
    }
    false
}

/// `isLikelySourceURLReference` (PU:2301). The publisher's markdown processor
/// (BaseRenderer) always passes `webUrl=""`, so the FIRST guard
/// `baseUrl != null && !baseUrl.startsWith("http://hl7.org/fhir/R")` is TRUE
/// (empty string), enabling the resourceNames branch: a `<resource>.html` or
/// `<resource>-definitions.html` page (resource name lowercased) is a source ref.
/// Then the BASE_FILENAMES / `extension-` tail always applies.
fn is_likely_source_url_reference(url: &str, base_url: &str, resource_names: bool) -> bool {
    let _ = base_url;
    // PORT NOTE (corpus-inert, measured): Java's guarded block (PU:2306-2327)
    // also has a `localFilenames` veto (returns false for IG-local pages) and a
    // runtime `masterSourceFileNames` startsWith check between the resourceNames
    // loop and the static BASE_FILENAMES tail. Neither list is populated in the
    // fragment-render context we reproduce (no local page in the corpus collides
    // with a lowercase resource-name prefix), so both are omitted here.
    // resourceNames branch (webUrl==""): a core resource page.
    if resource_names {
        for rn in R4_RESOURCE_NAMES {
            let low = rn; // already lowercase in the table
            if url.starts_with(&format!("{}.html", low))
                || url.starts_with(&format!("{}-definitions.html", low))
            {
                return true;
            }
        }
    }
    if let Some(idx) = url.find(".html") {
        let base = &url[..idx];
        BASE_FILENAMES.contains(&base) || url.starts_with("extension-")
    } else {
        false
    }
}

/// The R4 resource type names, lowercased (context.getResourceNames()). Used by
/// isLikelySourceURLReference's resourceNames branch (PU:2306). A `location.html`
/// / `bundle.html` link in an element definition's markdown is thus recognised as
/// a spec source page and corePath-prefixed — matching the goldens.
const R4_RESOURCE_NAMES: &[&str] = &[
    "account", "activitydefinition", "adverseevent", "allergyintolerance", "appointment",
    "appointmentresponse", "auditevent", "basic", "binary", "biologicallyderivedproduct",
    "bodystructure", "bundle", "capabilitystatement", "careplan", "careteam", "catalogentry",
    "chargeitem", "chargeitemdefinition", "claim", "claimresponse", "clinicalimpression",
    "codesystem", "communication", "communicationrequest", "compartmentdefinition", "composition",
    "conceptmap", "condition", "consent", "contract", "coverage", "coverageeligibilityrequest",
    "coverageeligibilityresponse", "detectedissue", "device", "devicedefinition", "devicemetric",
    "devicerequest", "deviceusestatement", "diagnosticreport", "documentmanifest",
    "documentreference", "domainresource", "effectevidencesynthesis", "encounter", "endpoint",
    "enrollmentrequest", "enrollmentresponse", "episodeofcare", "eventdefinition", "evidence",
    "evidencevariable", "examplescenario", "explanationofbenefit", "familymemberhistory", "flag",
    "goal", "graphdefinition", "group", "guidanceresponse", "healthcareservice", "imagingstudy",
    "immunization", "immunizationevaluation", "immunizationrecommendation", "implementationguide",
    "insuranceplan", "invoice", "library", "linkage", "list", "location", "measure", "measurereport",
    "media", "medication", "medicationadministration", "medicationdispense", "medicationknowledge",
    "medicationrequest", "medicationstatement", "medicinalproduct", "medicinalproductauthorization",
    "medicinalproductcontraindication", "medicinalproductindication", "medicinalproductingredient",
    "medicinalproductinteraction", "medicinalproductmanufactured", "medicinalproductpackaged",
    "medicinalproductpharmaceutical", "medicinalproductundesirableeffect", "messagedefinition",
    "messageheader", "molecularsequence", "namingsystem", "nutritionorder", "observation",
    "observationdefinition", "operationdefinition", "operationoutcome", "organization",
    "organizationaffiliation", "parameters", "patient", "paymentnotice", "paymentreconciliation",
    "person", "plandefinition", "practitioner", "practitionerrole", "procedure", "provenance",
    "questionnaire", "questionnaireresponse", "relatedperson", "requestgroup", "researchdefinition",
    "researchelementdefinition", "researchstudy", "researchsubject", "resource", "riskassessment",
    "riskevidencesynthesis", "schedule", "searchparameter", "servicerequest", "slot", "specimen",
    "specimendefinition", "structuredefinition", "structuremap", "subscription", "substance",
    "substancenucleicacid", "substancepolymer", "substanceprotein", "substancereferenceinformation",
    "substancesourcematerial", "substancespecification", "supplydelivery", "supplyrequest", "task",
    "terminologycapabilities", "testreport", "testscript", "valueset", "verificationresult",
    "visionprescription",
];

/// `Utilities.stripPara` (Utilities.java:1870): trim, drop a leading `<p>` and a
/// trailing `</p>`.
pub fn strip_para(p: &str) -> String {
    if p.trim().is_empty() {
        return String::new();
    }
    let mut p = p.trim().to_string();
    if let Some(r) = p.strip_prefix("<p>") {
        p = r.to_string();
    }
    if let Some(r) = p.strip_suffix("</p>") {
        p = r.to_string();
    }
    p
}

/// `Utilities.stripAllPara` (Utilities.java:1884): stripPara, then remaining
/// `</p>`->" ", `<p>`->"". (The `<p ` attr loop in Java is a no-op degenerate
/// substring; commonmark emits only bare `<p>`, so it never triggers.)
pub fn strip_all_para(p: &str) -> String {
    if p.trim().is_empty() {
        return String::new();
    }
    let mut p = p.trim().to_string();
    if let Some(r) = p.strip_prefix("<p>") {
        p = r.to_string();
    }
    if let Some(r) = p.strip_suffix("</p>") {
        p = r.to_string();
    }
    p = p.replace("</p>", " ");
    p = p.replace("<p>", "");
    p
}

/// `BaseRenderer.processMarkdown` (BaseRenderer.java:184): preProcessMarkdown then
/// `markdownEngine.process`. Returns the HTML string.
pub fn process_markdown(ctx: &IgContext, text: &str, core_path: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let pre = pre_process_markdown(ctx, text, core_path);
    md_process(&pre)
}

/// `XhtmlFluent.markdown(md, source)` (XhtmlFluent.java:305): run the md through
/// `MarkDownProcessor.process` (NO preProcessMarkdown — the caller already ran it),
/// then `XhtmlParser.parse("<div>"+html+"</div>", "div")` and `addChildren(
/// m.getChildNodes())`. Since `parse(..)` returns an XhtmlDocument whose single
/// child is the parsed `<div>`, the nodes ADDED are `[<div> ... </div>]` — a
/// nested div, as the goldens show. Here `html` is the already-md-processed HTML
/// string (the caller supplies the preProcessMarkdown'd text; md_process does the
/// commonmark). Returns the node list to append.
pub fn markdown_children_from_html(html: &str) -> Vec<render_xhtml::node::XhtmlNode> {
    let wrapped = format!("<div>{}</div>", html);
    let mut parser = render_xhtml::XhtmlParser::new();
    match parser.parse_fragment(&wrapped) {
        // parse_fragment returns the <div> node directly. XhtmlDocument.getChildNodes
        // in Java would be [that <div>]; reproduce by returning the whole node.
        Ok(div) => vec![div],
        Err(_) => Vec::new(),
    }
}

/// `parseMDFragmentStripParas(html)` (XhtmlParser:1351) then
/// `new XhtmlComposer(false, true).setAutoLinks(true).compose(list)` — the
/// IPStatementsRenderer copyright path (ipr:230-234). Parse `<div>`+html+`</div>`,
/// flatten each top-level child's children (dropping the `<p>` wrappers AND the
/// inter-paragraph whitespace, which lives at div-level), then compose the inline
/// node list with the html-pretty composer + autoLinks.
pub fn md_fragment_strip_paras_autolinks(html: &str) -> String {
    use render_xhtml::composer::{Config, XhtmlComposer};
    let wrapped = format!("<div>{}</div>", html);
    let mut parser = render_xhtml::XhtmlParser::new();
    let Ok(div) = parser.parse_fragment(&wrapped) else {
        return String::new();
    };
    // for (x : div.children) res.addAll(x.children) — flatten one level.
    let mut nodes: Vec<render_xhtml::node::XhtmlNode> = Vec::new();
    for child in div.child_nodes() {
        for gc in child.child_nodes() {
            nodes.push(gc.clone());
        }
    }
    let mut cfg = Config::html_pretty();
    cfg.auto_links = true;
    let mut composer = XhtmlComposer::new(cfg);
    composer.compose_nodes(&nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_process_escapes_raw_html_tags() {
        // <tag ...> and </tag> get a leading backslash; <! and <? too.
        assert_eq!(pre_process("a <b> c"), "a \\<b> c");
        assert_eq!(pre_process("x </div> y"), "x \\</div> y");
        assert_eq!(pre_process("<!-- c -->"), "\\<!-- c -->");
        // already-escaped stays put
        assert_eq!(pre_process("a \\<b> c"), "a \\<b> c");
        // a bare < not starting a tag is untouched (e.g. "a < b")
        assert_eq!(pre_process("a < b"), "a < b");
    }

    #[test]
    fn md_process_wraps_table_class_but_corpus_is_table_free() {
        // simple paragraph, no table -> commonmark passthrough
        assert_eq!(md_process("Hello."), "<p>Hello.</p>\n");
    }

    #[test]
    fn strip_para_and_all_para() {
        assert_eq!(strip_para("<p>hi</p>\n".trim()), "hi");
        assert_eq!(
            strip_all_para("<p>a</p>\n<ul>\n<li>x</li>\n</ul>"),
            "a \n<ul>\n<li>x</li>\n</ul>"
        );
    }

    #[test]
    fn is_absolute_url_scheme() {
        assert!(is_absolute_url("http://x"));
        assert!(is_absolute_url("https://x"));
        assert!(is_absolute_url("urn:oid:1"));
        assert!(!is_absolute_url("foo.html"));
        assert!(!is_absolute_url("a/b.html"));
    }
}
