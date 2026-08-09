# frozen_string_literal: true

# patala in-process, through the C ABI (`libpatala_ffi`), using `fiddle` from
# the standard library. No child process, no port, no gem dependency.
#
#   require "patala/ffi"
#
#   Patala::Ffi.open do |rail|                     # MockRail by default
#     receipt = rail.charge(amount_minor: 1250, currency: "USDC",
#                           destination: "mock:wallet:alice", reference: "order-1")
#     rail.verify(receipt).fetch("valid")          # => true
#   end
#
# Why `fiddle` and not the `ffi` gem: fiddle ships with Ruby, so direct mode
# adds no dependency at all. Everything needed is there — dlopen and a
# struct-free calling convention. patala has no streaming operation, so there
# is no callback to arrange either; six functions is the whole ABI.
#
# WHAT LOADING THIS COSTS YOU: almost nothing, and that is not the usual answer.
# patala is Rust, so no language runtime enters your process — no GC, no
# scheduler, no signal handlers, and no threads (each handle owns a
# current-thread tokio runtime that runs on whichever thread called in). If you
# have read the equivalent file in llmux or openrate, its Go-runtime warnings
# are true there and false here; see ../../README.md, where it is measured.
#
# The ONE fork rule, which is real: a handle's runtime sits behind a mutex, and
# fork() copies a locked mutex as locked. Open handles in the child. Unicorn's
# `after_fork`, Puma's `on_worker_boot`, Resque's per-job child — build the
# Patala::Ffi there, not at boot. Loading the LIBRARY before the fork is fine;
# it is the handle that matters. examples/fork_probe.rb measures both.

require "fiddle"
require "json"
require "rbconfig"

module Patala
  class Error < StandardError; end unless defined?(Error)

  # The C ABI of libpatala_ffi (patala-ffi/include/patala.h), bound with fiddle.
  class Ffi
    PTR  = Fiddle::TYPE_VOIDP
    U64  = Fiddle::TYPE_LONG_LONG # fiddle has no unsigned 64-bit constant;
    INT  = Fiddle::TYPE_INT       # handles are small counters, so this is exact.
    VOID = Fiddle::TYPE_VOID

    # Every method `patala_call` dispatches, as of this ABI. Listed so a typo
    # is caught here with the real list attached rather than after a round trip
    # into the library.
    METHODS = %w[
      id capabilities quote charge verify validate-destination webhook caveat providers
    ].freeze

    # Open a rail, yield it, and close it however the block exits. Prefer this
    # to a bare .new: `ensure` around one is the same thing written longhand and
    # easier to get wrong.
    #
    #   Patala::Ffi.open { |rail| ... }                                  # MockRail
    #   Patala::Ffi.open(config: { rail: "mock", failing: true }) { ... } # every op fails
    def self.open(config: nil, library: nil)
      rail = new(config: config, library: library)
      begin
        yield rail
      ensure
        rail.close
      end
    end

    # config:  a rail configuration document — a Hash, a JSON String, or nil for
    #          the offline default (a deterministic MockRail on USDC). Unknown
    #          fields are REFUSED, not ignored: a misspelled "currencys" is an
    #          error rather than a rail quietly built with defaults you did not
    #          choose.
    # library: override library resolution entirely.
    def initialize(config: nil, library: nil)
      @library_path = library || self.class.resolve_library
      begin
        @dl = Fiddle.dlopen(@library_path)
      rescue Fiddle::DLError => e
        raise Error, "could not load #{@library_path}: #{e.message}"
      end

      bind!

      json = case config
             when nil, String then config
             else JSON.generate(config)
             end

      slot = err_slot
      handle = @fn_new.call(json, slot)
      # patala_new returns 0 for FAILURE, inverting the usual convention,
      # because its success value is a handle and handles start at 1.
      raise Error, "patala_new: #{take_error(slot)}" if handle.zero?

      @handle = handle
      @mutex = Mutex.new
    end

    # The path this instance actually loaded.
    attr_reader :library_path

    # The patala version the LOADED library was built from.
    #
    # Worth checking at startup: a shared library is resolved off a load path
    # you may not control, and a stale libpatala_ffi earlier on it is called
    # silently otherwise. The string is static inside the library — never freed.
    def version
      @fn_abi_version.call.to_s
    end

    # Raise unless the loaded library is exactly `expected`. The comparison
    # lives in the library (`patala_abi_check`) so it is not reimplemented —
    # and forgotten — in each binding.
    def abi_check!(expected)
      slot = err_slot
      return true if @fn_abi_check.call(expected.to_s, slot).zero?

      raise Error, take_error(slot)
    end

    # One unary call. Returns the parsed JSON response.
    #
    #   rail.call("charge", amount_minor: 1250, currency: "USDC", ...)
    #   rail.call("capabilities")
    def call(method, request = nil, **kwargs)
      request = kwargs unless kwargs.empty?
      JSON.parse(call_raw(method, request))
    end

    # The same call, returning the response JSON verbatim — the same bytes
    # POST /v1/rails/:id/<method> returns from patala-sidecar.
    def call_raw(method, request = nil)
      assert_open!
      method = method.to_s
      unless METHODS.include?(method)
        raise Error, "unknown method #{method.inspect} (want one of: #{METHODS.join(', ')})"
      end

      json = case request
             when nil, String then request
             else JSON.generate(request)
             end

      # A FRESH, zeroed slot per call. patala writes *err on failure only and
      # leaves it untouched otherwise, so a reused slot still holds the previous
      # (already freed) pointer.
      slot = err_slot
      res = @fn_call.call(@handle, method, json, slot)
      raise Error, "patala_call(#{method}): #{take_error(slot)}" if res.null?

      begin
        res.to_s # Fiddle::Pointer#to_s copies up to the NUL into a Ruby String
      ensure
        # Everything the library returns — results AND error messages — is
        # released with patala_free and with nothing else. Not Ruby's free,
        # not yours: this is Rust's allocator.
        @fn_free.call(res)
      end
    end

    # ------------------------------------------------------------- convenience

    def id           = call("id").fetch("rail_id")
    def capabilities = call("capabilities")
    def quote(request = nil, **kw) = call("quote", request, **kw)
    def charge(request = nil, **kw) = call("charge", request, **kw)

    # Hand back the Receipt "charge" returned, unchanged. Returns the whole
    # {"valid" => bool} document rather than the bool, because `false` here is
    # the rail's honest fail-closed answer and not an error — gate entitlement
    # on `true` and never retry a `false` as though it were transient.
    def verify(receipt) = call("verify", receipt)

    # The offline pre-flight check. NEVER raises for an unknown address: "I
    # cannot check this" comes back as the verdict {"status" => "Unknown"},
    # because a caller must handle it as carefully as a refusal and an error is
    # too easy to swallow. Read "is_refusal" (do not send) and
    # "human_must_confirm" (true on EVERY verdict, "StructurallyValid" included).
    def validate_destination(destination)
      call("validate-destination", destination: destination)
    end

    # Authenticate an inbound processor delivery. Forward it VERBATIM — the
    # exact body bytes, the headers, the query string — because every scheme
    # signs what the processor actually sent, and a body that has been through
    # a JSON round trip on your side is no longer that.
    def webhook(body:, headers: {}, query: {}, now_unix: Time.now.to_i, binary: false)
      payload = { headers: headers, query: query, now_unix: now_unix }
      payload[binary ? :body_hex : :body] = binary ? body.unpack1("H*") : body
      call("webhook", payload)
    end

    # The sentence to show a human on the form where a payout address is first
    # asked for — before there is a verdict to render. patala does not detect
    # exchange-owned addresses and this is how it says so.
    def caveat = call("caveat").fetch("exchange_deposit_caveat")

    # Fiat provider names this specific build can construct. Only present when
    # the library was built with --features fiat.
    def providers = call("providers").fetch("providers")

    # Release the rail. Idempotent, so cleanup paths can call it blindly.
    def close
      @mutex&.synchronize do
        return if @handle.nil? || @handle.zero?

        @fn_close.call(@handle)
        @handle = 0
      end
      nil
    end

    def closed? = @handle.nil? || @handle.zero?

    # ---------------------------------------------------------------- internals

    private

    def bind!
      @fn_abi_version = Fiddle::Function.new(@dl["patala_abi_version"], [], PTR,
                                             name: "patala_abi_version")
      @fn_abi_check = Fiddle::Function.new(@dl["patala_abi_check"], [PTR, PTR], INT,
                                           name: "patala_abi_check")
      @fn_new = Fiddle::Function.new(@dl["patala_new"], [PTR, PTR], U64, name: "patala_new")
      @fn_close = Fiddle::Function.new(@dl["patala_close"], [U64], VOID, name: "patala_close")
      @fn_call = Fiddle::Function.new(@dl["patala_call"], [U64, PTR, PTR, PTR], PTR,
                                      name: "patala_call")
      @fn_free = Fiddle::Function.new(@dl["patala_free"], [PTR], VOID, name: "patala_free")
    rescue Fiddle::DLError => e
      raise Error, "#{@library_path} is missing a symbol the ABI promises: #{e.message}"
    end

    def assert_open!
      raise Error, "this Patala::Ffi handle is closed" if closed?
    end

    # A zeroed `char*` slot for the trailing `char** err` argument.
    def err_slot
      slot = Fiddle::Pointer.malloc(Fiddle::SIZEOF_VOIDP, Fiddle::RUBY_FREE)
      slot[0, Fiddle::SIZEOF_VOIDP] = "\x00".b * Fiddle::SIZEOF_VOIDP
      slot
    end

    # Read the message out of an err slot and free it with patala_free — an
    # error must not also be a leak.
    def take_error(slot)
      ptr = slot.ptr
      return "(no message)" if ptr.null?

      message = ptr.to_s
      @fn_free.call(ptr)
      message
    end

    class << self
      # 1. PATALA_LIBRARY
      # 2. lib/ inside this gem (where a platform gem would put it)
      # 3. target/{debug,release}/ in a checkout of the patala workspace
      # 4. the bare soname, letting the dynamic loader search its own paths
      def resolve_library
        env = ENV["PATALA_LIBRARY"]
        return env if env && !env.empty?

        gem_root = File.expand_path("../..", __dir__) # …/sdks/ruby
        repo = File.expand_path("../..", gem_root)    # …/repo root

        candidates = [File.join(gem_root, "lib", library_name)]
        %w[debug release].each do |profile|
          candidates << File.join(repo, "target", profile, library_name)
        end
        candidates.find { |path| File.file?(path) } || library_name
      end

      def library_name
        case RbConfig::CONFIG["host_os"]
        when /mswin|mingw|cygwin/ then "patala_ffi.dll"
        when /darwin/ then "libpatala_ffi.dylib"
        else "libpatala_ffi.so"
        end
      end
    end
  end
end
