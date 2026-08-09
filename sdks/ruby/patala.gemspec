# frozen_string_literal: true

Gem::Specification.new do |spec|
  spec.name = "patala"
  spec.version = File.read(File.expand_path("../../VERSION", __dir__)).strip
  spec.authors = ["imranparuk"]
  spec.summary = "patala from Ruby: in-process over the C ABI, or over the local sidecar"
  spec.description = <<~TEXT
    Two ways to reach patala — a sovereign, centerless payment-rail substrate —
    from Ruby. Direct mode loads libpatala_ffi with `fiddle` from the standard
    library and runs patala in your own process; sidecar mode talks JSON to
    `patala-sidecar` over loopback. Both speak the same JSON, so moving a call
    site between them is a transport change, not a rewrite. No runtime
    dependencies.
  TEXT
  spec.homepage = "https://vulos.org/projects/patala"
  spec.license = "MIT OR Apache-2.0"
  spec.required_ruby_version = ">= 3.0"

  spec.files = Dir["lib/**/*.rb", "examples/*.rb", "README.md"]
  spec.require_paths = ["lib"]

  # None, deliberately. `fiddle`, `json`, `net/http`, `securerandom` and
  # `socket` are all standard library. Direct mode additionally needs a
  # libpatala_ffi for your platform (`cargo build -p patala-ffi`), and sidecar
  # mode needs the `patala-sidecar` binary — neither is a gem dependency.
  spec.metadata = {
    "source_code_uri" => "https://github.com/vul-os/patala",
    "rubygems_mfa_required" => "true"
  }
end
