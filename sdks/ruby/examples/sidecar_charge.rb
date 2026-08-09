#!/usr/bin/env ruby
# frozen_string_literal: true

# patala from Ruby over the sidecar — a separate process, no FFI at all.
#
#   ruby sdks/ruby/examples/sidecar_charge.rb
#
# This spawns patala-sidecar on a free loopback port with a freshly generated
# token, drives a full quote -> charge -> verify against MockRail with nothing
# but the standard library, and terminates it. Nothing is left running.
#
# Build the server first, from the workspace root:
#
#   cargo build -p patala-sidecar

$LOAD_PATH.unshift(File.expand_path("../lib", __dir__))

require "patala"

CHECKS = [0]

def check(condition, message)
  CHECKS[0] += 1
  abort("FAILED: #{message}") unless condition
  puts "  ok  #{message}"
end

puts "ruby #{RUBY_VERSION} (#{RUBY_PLATFORM})"

Patala::Sidecar.start do |sc|
  puts "binary:  #{sc.binary}"
  puts "listening on #{sc.base_url} (loopback only — the bind address is not configurable)"
  puts

  puts "capabilities"
  caps = sc.capabilities("mock")
  check(caps["class"] == "NonCustodialFinal",
        "class is #{caps['class'].inspect} — decide the whole UX off this, not off a provider name")
  check(caps["holds_funds"] == false, "holds_funds is false")

  puts "\npre-flight: validate-destination, before any money moves"
  verdict = sc.validate_destination("mock", "mock:wallet:alice")
  check(verdict["status"] == "StructurallyValid",
        "a well-formed address -> 200 #{verdict['status'].inspect}")
  check(verdict["is_refusal"] == false, "is_refusal is false — read the body, not just the code")
  check(verdict["human_must_confirm"] == true,
        "human_must_confirm is true even on StructurallyValid")

  status, refused = sc.try("POST", "/v1/rails/mock/validate-destination",
                           body: { destination: "" })
  check(status == 200 && refused["status"] == "Malformed" && refused["is_refusal"],
        "an empty destination is a well-formed REQUEST -> 200 with a Malformed refusal")

  puts "\nquote -> charge -> verify"
  pay = { amount_minor: 1250, currency: "USDC",
          destination: "mock:wallet:alice", reference: "order-1" }

  quote = sc.quote("mock", pay)
  check(quote["total_minor"] == 1250 && quote["total_minor"].is_a?(Integer),
        "total_minor == #{quote['total_minor']} and parses as an Integer — never a Float")

  receipt = sc.charge("mock", pay)
  check(receipt["amount_minor"] == 1250,
        "charge -> receipt for #{receipt['amount_minor']} #{receipt['currency']}")

  check(sc.verify("mock", receipt) == { "valid" => true },
        "the genuine receipt verifies {\"valid\": true}")

  tampered = receipt.merge("amount_minor" => receipt["amount_minor"] + 1)
  status, body = sc.try("POST", "/v1/rails/mock/verify", body: tampered)
  check(status == 200 && body == { "valid" => false },
        "a tampered receipt is 200 {\"valid\": false} — fail-closed, and NOT an HTTP error")

  puts "\nthe error surface, so you can tell these four apart"
  status, body = sc.try("POST", "/v1/rails/mock/charge", body: pay.merge(currency: "EUR"))
  check(status == 400, "an unsupported currency -> #{status} #{body['kind'].inspect}")

  status, body = sc.try("GET", "/v1/rails/nope")
  check(status == 404, "an unknown rail_id -> #{status} #{body['kind'].inspect}")

  status, body = sc.try("POST", "/v1/rails/mock/webhook", raw_body: "{}")
  check(status == 501,
        "the mock has no push delivery -> #{status} #{body['kind'].inspect}, never an invented event")

  status, = sc.try("GET", "/v1/rails/mock", authed: false)
  check(status == 401, "no Authorization header -> #{status} on a READ-ONLY route too")

  puts "\nthe raised form, for the 95% of call sites that just want the answer"
  begin
    sc.capabilities("nope")
    abort("FAILED: an unknown rail should have raised")
  rescue Patala::HTTPError => e
    check(e.status == 404 && e.body["kind"] == "unknown_rail",
          "Patala::HTTPError keeps the status and the parsed body: #{e.message}")
  end
end

puts "\nsidecar terminated; nothing left running"
puts "\nALL #{CHECKS[0]} RUBY SIDECAR ASSERTIONS PASSED"
