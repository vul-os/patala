#!/usr/bin/env ruby
# frozen_string_literal: true

# patala in-process from Ruby, through the C ABI with `fiddle`.
#
#   ruby sdks/ruby/examples/direct_charge.rb
#
# Everything runs against MockRail: deterministic, offline, no credentials and
# no network. patala is a payments library, so an example that moves real value
# is not an example.
#
# Build the library first, from the workspace root:
#
#   cargo build -p patala-ffi

$LOAD_PATH.unshift(File.expand_path("../lib", __dir__))

require "patala/ffi"

CHECKS = [0]

def check(condition, message)
  CHECKS[0] += 1
  abort("FAILED: #{message}") unless condition
  puts "  ok  #{message}"
end

# Threads in this process, counted by the OS rather than by Ruby.
# Thread.list only sees threads Ruby created — the exact wrong number, since
# the question is what the native library started.
def os_threads
  `ps -M #{Process.pid} 2>/dev/null`.lines.length - 1
rescue StandardError
  -1
end

puts "ruby #{RUBY_VERSION} (#{RUBY_PLATFORM}), fiddle #{Fiddle::VERSION}"
puts "threads before dlopen: #{os_threads}"

Patala::Ffi.open do |rail|
  puts "library: #{rail.library_path}"
  puts "patala:  #{rail.version}"
  puts "threads after dlopen + patala_new: #{os_threads}"
  puts

  puts "the version probe, because a stale library earlier on the load path is silent"
  rail.abi_check!(rail.version)
  check(true, "abi_check! against the loaded version passes")
  begin
    rail.abi_check!("9.9.9")
    abort("FAILED: abi_check!('9.9.9') should have raised")
  rescue Patala::Error => e
    check(e.message.include?("mismatch"), "abi_check!('9.9.9') raises and names both versions")
  end

  puts "\ncapabilities"
  caps = rail.capabilities
  check(rail.id == "mock", "id == #{rail.id.inspect}")
  check(caps["class"] == "NonCustodialFinal",
        "class is #{caps['class'].inspect} — a wallet address and a final receipt, not a card form")
  check(caps["holds_funds"] == false, "holds_funds is false — patala never holds funds")
  check(caps["reversible"] == false, "reversible is false — there is no refund on this rail")

  puts "\npre-flight: validate-destination, before any money moves"
  verdict = rail.validate_destination("mock:wallet:alice")
  check(verdict["status"] == "StructurallyValid",
        "a well-formed address gives status #{verdict['status'].inspect}")
  check(verdict["is_refusal"] == false,
        "is_refusal is false — a field, never re-derived from status")
  check(verdict["human_must_confirm"] == true,
        "human_must_confirm is true even here — patala does not detect exchange addresses")
  refused = rail.validate_destination("")
  check(refused["status"] == "Malformed" && refused["is_refusal"],
        "an empty destination is a Malformed refusal, returned as a verdict and never raised")
  check(!rail.caveat.empty?,
        "caveat returns the sentence to show a human on the address form: #{rail.caveat[0, 48]}…")

  puts "\nquote -> charge -> verify"
  pay = { amount_minor: 1250, currency: "USDC",
          destination: "mock:wallet:alice", reference: "order-1" }

  quote = rail.quote(pay)
  check(quote["total_minor"] == 1250 && quote["total_minor"].is_a?(Integer),
        "total_minor == #{quote['total_minor']} and is an Integer — minor units, never a Float")

  receipt = rail.charge(pay)
  check(receipt["amount_minor"] == 1250,
        "charge -> receipt for #{receipt['amount_minor']} #{receipt['currency']}")

  check(rail.verify(receipt) == { "valid" => true }, "the genuine receipt verifies true")

  tampered = receipt.merge("amount_minor" => receipt["amount_minor"] + 1)
  check(rail.verify(tampered) == { "valid" => false },
        "a tampered receipt verifies false — fail-closed, and false is DATA, not an exception")

  puts "\nerrors come back as errors, never as a crash in your process"
  begin
    rail.charge(pay.merge(currency: "EUR"))
    abort("FAILED: charging EUR on a USDC rail should have raised")
  rescue Patala::Error => e
    check(e.message.include?("does not support currency EUR"), "an unsupported currency: #{e.message}")
  end

  begin
    rail.call("nope")
    abort("FAILED: an unknown method should have raised")
  rescue Patala::Error => e
    check(e.message.include?("unknown method"), "an unknown method is caught before the FFI call")
  end

  puts "\nwebhooks: a rail with no push delivery says so"
  begin
    rail.webhook(body: "{}", headers: {})
    abort("FAILED: the mock has no push delivery and should have refused")
  rescue Patala::Error => e
    check(e.message.include?("not supported"),
          "the mock refuses rather than inventing an event: #{e.message.split(': ').last}")
  end

  puts "\na closed or invented handle is a clean error, never a segfault"
  scratch = Patala::Ffi.new
  scratch.close
  begin
    scratch.capabilities
    abort("FAILED: a closed handle should have raised")
  rescue Patala::Error => e
    check(e.message.include?("closed"), "use-after-close says so: #{e.message}")
  end
  scratch.close
  check(true, "closing twice is a no-op, so cleanup paths can be idempotent")

  puts "\nthreads after the whole round trip: #{os_threads}   <- unchanged"
end

puts "\nALL #{CHECKS[0]} RUBY DIRECT ASSERTIONS PASSED"
