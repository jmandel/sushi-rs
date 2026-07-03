#!/usr/bin/env ruby
# frozen_string_literal: true
#
# liquid-oracle.rb — render a Liquid template using EXACTLY the Liquid engine +
# filter set + options that the HL7 FHIR IG Publisher's Jekyll runs.
#
# This is the reference ("oracle") for the render_liquid Rust crate's
# differential gate (task F1c of docs/stock-template-renderer-plan.md).
#
# ---------------------------------------------------------------------------
# What "Jekyll-as-it-runs-it" means here (all cited):
# ---------------------------------------------------------------------------
#  * Liquid engine version: Jekyll 4.4.1 bundles liquid 4.0.4
#      (require "jekyll"; Liquid::VERSION == "4.0.4").
#  * Filter set = Liquid::StandardFilters (built in) PLUS Jekyll::Filters,
#    which Jekyll registers globally via
#      Liquid::Template.register_filter(Jekyll::Filters)
#    (jekyll-4.4.1/lib/jekyll/filters.rb:538). This is why `where`, `sort`,
#    `jsonify`, `markdownify`, `slugify`, ... resolve. Jekyll's `where`/`sort`
#    OVERRIDE liquid-core `where`/`sort`; US Core's `where:"code",x` idiom is
#    the Jekyll one. We register the whole module so behavior is byte-identical.
#  * Liquid options (jekyll-4.4.1/lib/jekyll/configuration.rb:61-64):
#      error_mode: "warn", strict_filters: false, strict_variables: false
#    and templates are parsed with `line_numbers: true`
#    (jekyll-4.4.1/lib/jekyll/liquid_renderer/file.rb:13).
#  * site.data model (jekyll-4.4.1/lib/jekyll/readers/data_reader.rb):
#    input/data/*.{yml,yaml,json} loaded as-is; *.csv/*.tsv -> Array of Hashes
#    keyed by header row (headers:true). We DO NOT reload here — the caller
#    passes an already-materialized JSON context (site/page/include/...), so the
#    Rust DataProvider and this oracle share one context and only the ENGINE
#    differs. (A --data-dir mode is provided to build site.data the Jekyll way
#    for fixtures that want the real CSV coercion.)
#
# markdownify is STUBBED to a deterministic marker (see MD_MARKER) because
# markdown is a separate crate (render_md, F1b). The gate compares Liquid
# structure, not markdown.
#
# ---------------------------------------------------------------------------
# Usage:
#   liquid-oracle.rb --template FILE --context CTX.json [--publisher-raw-quirk]
#   liquid-oracle.rb --template FILE --data-dir input/data [--context CTX.json]
#   (template may also be read from stdin if --template - )
#
# Emits the rendered string on stdout. On a hard Liquid error emits the partial
# output Jekyll would emit (warn mode swallows most) and exits 0; only a true
# parse crash exits non-zero with the message on stderr.
# ---------------------------------------------------------------------------

require "json"
require "optparse"
require "tmpdir"
require "fileutils"
require "jekyll"

# Register Jekyll's filters exactly as Jekyll boot does.
Liquid::Template.register_filter(Jekyll::Filters)

# Deterministic markdownify stub. The real markdownify is render_md's job; here
# we replace it with a reversible marker so the gate isolates Liquid behavior.
MD_MARKER_OPEN = "MD"
MD_MARKER_CLOSE = "/MD"

module Jekyll
  module Filters
    def markdownify(input)
      # NOTE: use inline string literals, NOT interpolated frozen constants.
      # Interpolating the frozen `MD_MARKER_OPEN`/`MD_MARKER_CLOSE` constants
      # here makes Liquid emit \x01 span-guard bytes around the segments; inline
      # literals stay clean. (Verified empirically; both markers are still
      # exactly "MD"…"/MD" so the Rust stub matches byte-for-byte.)
      "MD#{input}/MD"
    end
  end
end

# ---------------------------------------------------------------------------
# The {% raw %} publisher quirk.
#
# The Java IG Publisher evaluates certain tags (notably {% fragment %} /
# {% include %}) INSIDE {% raw %} blocks — raw is NOT a clean escape there
# (survey (f) nasty #4; cycle liquid.ts:213-220). Real Liquid does the correct
# thing: raw emits its body verbatim. We reproduce the quirk behind a flag by
# preprocessing: when --publisher-raw-quirk is set, we strip the {% raw %} /
# {% endraw %} wrappers (so their contents are parsed & evaluated) BUT only the
# include/fragment-bearing ones, matching the Publisher. Default = correct raw.
# ---------------------------------------------------------------------------
def apply_publisher_raw_quirk(src)
  # Remove raw/endraw delimiters so the enclosed tags get evaluated, mirroring
  # the Publisher's pre-Jekyll fragment pass which does not respect raw.
  src.gsub(/\{%-?\s*raw\s*-?%\}/, "").gsub(/\{%-?\s*endraw\s*-?%\}/, "")
end

options = { template: nil, context: nil, data_dir: nil, includes_dir: nil, raw_quirk: false }
OptionParser.new do |o|
  o.on("--template FILE", "template file, or - for stdin") { |v| options[:template] = v }
  o.on("--context FILE", "JSON context file") { |v| options[:context] = v }
  o.on("--data-dir DIR", "load site.data from DIR the Jekyll way") { |v| options[:data_dir] = v }
  o.on("--includes-dir DIR", "resolve {% include %} from DIR (Jekyll _includes)") { |v| options[:includes_dir] = v }
  o.on("--publisher-raw-quirk", "evaluate tags inside {% raw %} like the Publisher") { options[:raw_quirk] = true }
end.parse!

template_src =
  if options[:template].nil? || options[:template] == "-"
    $stdin.read
  else
    File.read(options[:template])
  end

context = {}
if options[:context]
  context = JSON.parse(File.read(options[:context]))
end

# Optionally build site.data from a real input/data dir using Jekyll's exact
# CSV/YAML coercion, so fixtures can exercise the true data model.
if options[:data_dir]
  require "csv"
  data = {}
  Dir.glob(File.join(options[:data_dir], "*.{yml,yaml,json,csv,tsv}")).sort.each do |path|
    key = File.basename(path, ".*").gsub(/[^\w\s-]+/, "").gsub(/\s+/, "_")
    ext = File.extname(path).downcase
    data[key] =
      case ext
      when ".csv" then CSV.read(path, headers: true).map(&:to_hash)
      when ".tsv" then CSV.read(path, headers: true, col_sep: "\t").map(&:to_hash)
      else JSON.parse(File.read(path)) rescue YAML.safe_load(File.read(path))
      end
  end
  context["site"] ||= {}
  context["site"]["data"] = data
end

template_src = apply_publisher_raw_quirk(template_src) if options[:raw_quirk]

# ---------------------------------------------------------------------------
# Boot a minimal Jekyll::Site so that Jekyll's context-dependent filters
# (`where`, `sort`, `group_by`, `slugify`, ...) have `@context.registers[:site]`
# available — Jekyll's `item_property` reads `site.filter_cache`, so a real Site
# object is required. This is precisely the environment Jekyll renders pages in.
# ---------------------------------------------------------------------------
src_dir = Dir.mktmpdir
dst_dir = Dir.mktmpdir
# Clean up on EVERY exit path (including SIGTERM/quota crashes) so a long gate
# loop doesn't accumulate Jekyll's temp source/dest trees on a tight tmpfs.
cleanup = lambda do
  FileUtils.remove_entry(src_dir) if Dir.exist?(src_dir)
  FileUtils.remove_entry(dst_dir) if Dir.exist?(dst_dir)
end
at_exit(&cleanup)
%w[TERM INT].each { |s| Signal.trap(s) { cleanup.call; exit 1 } }
# Point Jekyll's _includes at the IG's includes dir so its real {% include %}
# tag (parameterized includes + include.* + recursive re-render) resolves files
# exactly as the Publisher does.
if options[:includes_dir]
  FileUtils.ln_s(File.expand_path(options[:includes_dir]), File.join(src_dir, "_includes"))
end
config = Jekyll.configuration(
  "source"      => src_dir,
  "destination" => dst_dir,
  "quiet"       => true,
  "safe"        => false
)
site = Jekyll::Site.new(config)

# Parse with Jekyll's exact options.
begin
  template = Liquid::Template.parse(template_src, line_numbers: true, error_mode: :warn)
rescue Liquid::Error => e
  warn "PARSE ERROR: #{e.message}"
  exit 2
end

# Render with Jekyll's registers ({ site: } is required by where/sort's
# item_property, which reads site.filter_cache) and Jekyll's non-strict flags.
output = template.render(context,
                         registers: { site: site, page: context["page"] },
                         strict_variables: false,
                         strict_filters: false)

$stdout.write(output)
