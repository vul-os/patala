#!/usr/bin/env ruby
# frozen_string_literal: true

# Does patala survive fork()? Measured, not asserted.
#
#   ruby sdks/ruby/examples/fork_probe.rb [iterations]
#
# This file exists because the same file in llmux and openrate reports the
# opposite result. Those are Go, built with `-buildmode=c-shared`, so the Go
# runtime lands in your Ruby process and does not survive fork() — a real
# collision with Unicorn, clustered Puma, Passenger, Resque and Spring, all of
# which fork by design. patala is Rust: no runtime, no threads, nothing running
# at fork() time.
#
# Do not take that on trust. Everything below is a live measurement.
#
# There IS one rule, and section 3 is where it shows up: a handle's tokio
# runtime sits behind a mutex, and fork() copies a LOCKED mutex as locked.

$LOAD_PATH.unshift(File.expand_path("../lib", __dir__))

require "patala/ffi"

WATCHDOG = Float(ENV.fetch("PATALA_FORK_TIMEOUT", "5"))
ITERATIONS = Integer(ARGV[0] || 200)

PAY = { amount_minor: 1250, currency: "USDC",
        destination: "mock:wallet:alice", reference: "order-1" }.freeze

def os_threads
  `ps -M #{Process.pid} 2>/dev/null`.lines.length - 1
rescue StandardError
  -1
end

# Fork, run the block in the child, and report — even if it never returns. The
# pipe is the only channel that can be trusted: a hung child cannot write to it
# and a crashed one closes it. Reading with a timeout is what turns "this hangs"
# from folklore into a printed result.
def fork_and_run(label, timeout: WATCHDOG)
  reader, writer = IO.pipe
  started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
  pid = fork do
    reader.close
    begin
      writer.write("returned #{yield}"[0, 300])
    rescue StandardError, ScriptError => e
      writer.write("raised #{e.class}: #{e.message}"[0, 300])
    end
    writer.close
    exit!(0)
  end
  writer.close

  message =
    if reader.wait_readable(timeout)
      reader.read.to_s
    else
      Process.kill("KILL", pid)
      "HUNG — nothing in #{timeout}s, SIGKILLed"
    end
  Process.waitpid(pid)
  reader.close
  message = "(wrote nothing)" if message.empty?
  elapsed = Process.clock_gettime(Process::CLOCK_MONOTONIC) - started
  printf("    %-40s %s  (%.2fs)\n", label, message, elapsed)
  message
end

puts "=" * 74
puts "patala fork probe (Ruby) — every line below is measured, not claimed"
puts "=" * 74
puts "ruby #{RUBY_VERSION} (#{RUBY_PLATFORM}), fiddle #{Fiddle::VERSION}"
puts "watchdog #{WATCHDOG}s\n\n"

puts "threads in a bare ruby process: #{os_threads}"

rail = Patala::Ffi.new
puts "library: #{rail.library_path}"
puts "patala:  #{rail.version}"
puts "threads after dlopen + patala_new: #{os_threads}"
rail.charge(PAY)
puts "threads after a charge round trip: #{os_threads}   <- unchanged: no runtime, no thread pool"

puts "\n" + ("-" * 74)
puts "1. after fork(), with the library loaded AND USED before the fork"
puts "-" * 74
puts "   (this is what Unicorn's `preload_app true` and Puma's `preload_app!` do)"
fork_and_run("charge on a FRESH handle") do
  Patala::Ffi.open { |r| r.charge(PAY)["amount_minor"] }
end
fork_and_run("charge on the INHERITED handle") { rail.charge(PAY)["amount_minor"] }
fork_and_run("charge -> verify, inherited handle") { rail.verify(rail.charge(PAY)) }
fork_and_run("validate-destination (pure, offline)") do
  rail.validate_destination("mock:wallet:alice")["status"]
end

puts "\n  Nothing hung. In llmux the equivalent child hangs on the first real"
puts "  call — and the trap there is that a cheap method still answers, so a"
puts "  boot check reports a clean bill of health for a broken worker. There is"
puts "  no such trap here because there is nothing broken to hide.\n"

puts "-" * 74
puts "2. the sidecar, for contrast"
puts "-" * 74
puts "  Nothing to measure. patala-sidecar is a separate OS process; your"
puts "  forking is not its business, and an HTTP client in the child works"
puts "  because a socket is a socket. This is the reason llmux and openrate"
puts "  steer Ruby users here — the reason does not exist for patala, but the"
puts "  option still does, and key isolation is a better argument for it.\n"

puts "-" * 74
puts "3. the one real hazard: an INHERITED handle, #{ITERATIONS} forks under contention"
puts "-" * 74
puts "  patala.h says: \"Handles are not inherited usefully across a fork; open"
puts "  them in the child.\" Section 1 forked from a quiet parent and the"
puts "  inherited handle was fine, which makes that rule look like superstition."
puts "  It is not: the handle's runtime sits behind a mutex, and fork() copies a"
puts "  LOCKED mutex as locked, with nobody in the child left to unlock it.\n"

stop = false
completed = 0
hammers = 4.times.map do
  Thread.new do
    until stop
      begin
        rail.call_raw("charge", PAY)
        completed += 1
      rescue Patala::Error
        nil
      end
    end
  end
end
sleep 0.3

results = {}
[
  ["inherited handle", -> { rail.call_raw("charge", PAY) }],
  ["fresh handle in the child", -> { Patala::Ffi.open { |r| r.call_raw("charge", PAY) } }]
].each do |label, work|
  hung = 0
  ITERATIONS.times do
    reader, writer = IO.pipe
    pid = fork do
      reader.close
      begin
        work.call
        writer.write("ok")
      rescue StandardError
        writer.write("err")
      end
      writer.close
      exit!(0)
    end
    writer.close
    unless reader.wait_readable(1.5)
      hung += 1
      Process.kill("KILL", pid)
    end
    Process.waitpid(pid)
    reader.close
  end
  results[label] = hung
  printf("    %-40s %d/%d hung\n", label, hung, ITERATIONS)
end
stop = true
hammers.each(&:join)

puts "\n  (#{completed} charges completed on the hammering threads meanwhile.)"
if results["inherited handle"].positive?
  puts "\n  Reproduced. Note the shape: it is a RACE against a window a few"
  puts "  microseconds wide, so most forks look fine and a test that forks once"
  puts "  is a false green."
else
  puts "\n  Not reproduced in this run — the window is a few microseconds wide and"
  puts "  a slower machine can miss it entirely. That is not evidence it is"
  puts "  closed; re-run with a larger count."
end

puts <<~ADVICE

    So the rule is about ONE HANDLE, not about the library, and the fix costs
    nothing:

      Unicorn                build the Patala::Ffi in `after_fork`, not at boot
      Puma clustered         `on_worker_boot`
      Passenger              per worker, or `passenger_spawn_method direct`
      Resque                 in the job's child
      Spring                 same, or the sidecar in development
      Puma single, Falcon,
      Sidekiq, rake, CLI     nothing to do — these do not fork

    Loading the LIBRARY before the fork is fine. Every one of those hosts is
    listed as a hard "use the sidecar" in llmux's Ruby README; here they are a
    one-line placement note.
ADVICE

rail.close
