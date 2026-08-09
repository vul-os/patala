# frozen_string_literal: true

# patala from Ruby over the sidecar: `patala-sidecar` as a separate process,
# JSON over a loopback socket, no FFI in your address space at all.
#
#   require "patala"
#
#   Patala::Sidecar.start do |sc|            # spawns it, waits for /healthz
#     receipt = sc.charge("mock", amount_minor: 1250, currency: "USDC",
#                                 destination: "mock:wallet:alice", reference: "order-1")
#     sc.verify("mock", receipt).fetch("valid")   # => true
#   end                                      # terminated on the way out
#
#   # or point at one somebody else runs:
#   sc = Patala::Sidecar.new(base_url: "http://127.0.0.1:8420", token: ENV["PATALA_SIDECAR_TOKEN"])
#
# The direct alternative is `require "patala/ffi"`, and for patala it is a
# genuinely cheap option — see ../README.md. The reason to run a sidecar anyway
# is not hazard avoidance, it is KEY ISOLATION: a non-custodial rail's signing
# key lives in whichever process calls charge, so one sidecar means one process
# holds it instead of every Unicorn worker on the box.
#
# No gem dependencies: net/http, json and securerandom are all standard library.

require "json"
require "net/http"
require "securerandom"
require "socket"
require "uri"

module Patala
  class Error < StandardError; end unless defined?(Error)

  # An HTTP error from the sidecar, carrying the parsed body. A non-2xx on this
  # API is an ANSWER with a body worth reading — `kind` is one of
  # "invalid_request", "unknown_rail", "unsupported", … — not a transport
  # failure, which is why the body is kept rather than flattened to a message.
  class HTTPError < Error
    attr_reader :status, :body

    def initialize(status, body)
      @status = status
      @body = body
      detail = body.is_a?(Hash) ? "#{body['kind']}: #{body['error']}" : body.to_s
      super("patala-sidecar returned #{status} — #{detail}")
    end
  end

  # A client for patala-sidecar, optionally owning the process.
  class Sidecar
    DEFAULT_PORT = 8420

    attr_reader :base_url, :token, :binary, :pid

    # Spawn a sidecar, yield a client for it, and terminate it however the block
    # exits. A fresh 32-byte token and a free loopback port are generated per
    # call, so nothing is shared with anything else on the machine.
    def self.start(binary: nil, port: nil, token: nil)
      sc = spawn(binary: binary, port: port, token: token)
      begin
        yield sc
      ensure
        sc.stop
      end
    end

    # The same, without the block form. The caller must call #stop.
    def self.spawn(binary: nil, port: nil, token: nil)
      binary ||= resolve_binary
      token ||= SecureRandom.hex(32)
      port ||= free_port

      pid = Process.spawn(
        { "PATALA_SIDECAR_TOKEN" => token, "PATALA_SIDECAR_PORT" => port.to_s },
        binary,
        out: File::NULL, err: File::NULL
      )
      client = new(base_url: "http://127.0.0.1:#{port}", token: token, pid: pid, binary: binary)
      client.wait_until_healthy!
      client
    rescue Errno::ENOENT
      raise Error, "could not run #{binary.inspect}. Build it first: cargo build -p patala-sidecar"
    end

    def initialize(base_url:, token:, pid: nil, binary: nil)
      @base_url = base_url.chomp("/")
      @token = token
      @pid = pid
      @binary = binary
      @uri = URI.parse(@base_url)
    end

    # /healthz is the one unauthenticated route and reveals nothing about the
    # configured rails — just liveness.
    def healthy?
      get("/healthz", authed: false) == "ok"
    rescue StandardError
      false
    end

    def wait_until_healthy!(timeout: 10)
      deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + timeout
      until Process.clock_gettime(Process::CLOCK_MONOTONIC) > deadline
        return self if healthy?

        if @pid && Process.waitpid(@pid, Process::WNOHANG)
          @pid = nil
          raise Error, "patala-sidecar exited before answering /healthz"
        end
        sleep 0.05
      end
      stop
      raise Error, "patala-sidecar never became healthy at #{@base_url}"
    end

    def stop
      return unless @pid

      Process.kill("TERM", @pid)
      Process.waitpid(@pid)
    rescue Errno::ESRCH, Errno::ECHILD
      # already gone
    ensure
      @pid = nil
    end

    # ------------------------------------------------------------- the API

    def capabilities(rail_id) = get("/v1/rails/#{rail_id}")
    def quote(rail_id, request = nil, **kw) = post("/v1/rails/#{rail_id}/quote", request || kw)
    def charge(rail_id, request = nil, **kw) = post("/v1/rails/#{rail_id}/charge", request || kw)

    # Returns the whole {"valid" => bool} document. `false` arrives with HTTP
    # 200 on purpose: it is the rail's fail-closed answer, not a broken sidecar,
    # and the two must not be confusable.
    def verify(rail_id, receipt) = post("/v1/rails/#{rail_id}/verify", receipt)

    # 200 for all five verdicts, including the refusals. Branch on "status" and
    # "is_refusal"; a 400 means the REQUEST was malformed and carries no verdict
    # fields at all, so a rejected request can never look like a checked address.
    def validate_destination(rail_id, destination)
      post("/v1/rails/#{rail_id}/validate-destination", { destination: destination })
    end

    # Forward the processor's request VERBATIM. `raw_body` must be the exact
    # bytes off the wire — this is the one endpoint whose body is not re-encoded
    # JSON, because every webhook scheme signs what was actually sent.
    def webhook(rail_id, raw_body, headers = {})
      request("POST", "/v1/rails/#{rail_id}/webhook", raw_body: raw_body, headers: headers)
    end

    # ---------------------------------------------------------------- internals

    def get(path, authed: true) = request("GET", path, authed: authed)
    def post(path, body) = request("POST", path, body: body)

    # One HTTP call. Raises HTTPError on a non-2xx; use #try for the cases where
    # the status IS the thing you want to look at.
    def request(method, path, body: nil, raw_body: nil, headers: {}, authed: true)
      status, parsed = try(method, path, body: body, raw_body: raw_body,
                                         headers: headers, authed: authed)
      raise HTTPError.new(status, parsed) unless status.between?(200, 299)

      parsed
    end

    # The same call, returning [status, body] instead of raising.
    def try(method, path, body: nil, raw_body: nil, headers: {}, authed: true)
      klass = method == "GET" ? Net::HTTP::Get : Net::HTTP::Post
      req = klass.new(path)
      req["Authorization"] = "Bearer #{@token}" if authed
      headers.each { |k, v| req[k] = v }
      if raw_body
        req.body = raw_body
      elsif body
        req["Content-Type"] = "application/json"
        req.body = JSON.generate(body)
      end

      res = Net::HTTP.start(@uri.host, @uri.port, read_timeout: 30) { |http| http.request(req) }
      [res.code.to_i, parse(res.body)]
    end

    def self.resolve_binary
      env = ENV["PATALA_SIDECAR_BIN"]
      return env if env && !env.empty?

      repo = File.expand_path("../../..", __dir__) # …/sdks/ruby/lib -> repo root
      %w[debug release].each do |profile|
        candidate = File.join(repo, "target", profile, "patala-sidecar")
        return candidate if File.file?(candidate)
      end
      "patala-sidecar"
    end

    def self.free_port
      server = TCPServer.new("127.0.0.1", 0)
      port = server.addr[1]
      server.close
      port
    end

    private

    def parse(raw)
      return nil if raw.nil? || raw.empty?

      JSON.parse(raw)
    rescue JSON::ParserError
      raw
    end
  end
end
