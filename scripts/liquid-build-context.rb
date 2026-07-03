#!/usr/bin/env ruby
# frozen_string_literal: true
#
# liquid-build-context.rb — materialize a single JSON render context for the
# differential gate, so the Ruby oracle and the Rust engine consume IDENTICAL
# input and only the ENGINE differs.
#
# It loads `input/data/*` with Jekyll's EXACT coercion (CSV -> array of header-
# keyed hashes; YAML/JSON as-is) into `site.data.<name>`, then layers a small
# mock of the always-present surfaces (`site.data.fhir.*`, `page.*`) that the
# Publisher injects. Emits the merged context as JSON on stdout.
#
# Usage: liquid-build-context.rb --data-dir DIR [--base BASE.json] > ctx.json
require "json"
require "optparse"
require "csv"
require "yaml"

opts = { data_dir: nil, base: nil }
OptionParser.new do |o|
  o.on("--data-dir DIR") { |v| opts[:data_dir] = v }
  o.on("--base FILE") { |v| opts[:base] = v }
end.parse!

ctx = opts[:base] ? JSON.parse(File.read(opts[:base])) : {}
ctx["site"] ||= {}
ctx["site"]["data"] ||= {}

if opts[:data_dir] && Dir.exist?(opts[:data_dir])
  Dir.glob(File.join(opts[:data_dir], "*.{yml,yaml,json,csv,tsv}")).sort.each do |path|
    key = File.basename(path, ".*").gsub(/[^\w\s-]+/, "").gsub(/\s+/, "_")
    ext = File.extname(path).downcase
    value =
      case ext
      when ".csv" then CSV.read(path, headers: true).map(&:to_hash)
      when ".tsv" then CSV.read(path, headers: true, col_sep: "\t").map(&:to_hash)
      else
        begin
          JSON.parse(File.read(path))
        rescue JSON::ParserError
          YAML.safe_load(File.read(path)) || {}
        end
      end
    # data-dir values do not clobber base-provided keys (e.g. a mock fhir).
    ctx["site"]["data"][key] ||= value
  end
end

$stdout.write(JSON.pretty_generate(ctx))
