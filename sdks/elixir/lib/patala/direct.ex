defmodule Patala.Direct do
  @moduledoc """
  patala in-process, through the C ABI, over the NIF in `Patala.Native`.

      {:ok, rail} = Patala.Direct.open()

      {:ok, receipt} = Patala.Direct.charge(rail, %{
        amount_minor: 1250, currency: "USDC",
        destination: "mock:wallet:alice", reference: "order-1"
      })

      {:ok, %{"valid" => true}} = Patala.Direct.verify(rail, receipt)
      :ok = Patala.Direct.close(rail)

  **This is not the recommended default for Elixir, and the reason has nothing
  to do with patala.** See `README.md`: a NIF cannot be killed, cannot be
  `Task.await`-timed-out, cannot be supervised, and a crash in one takes the
  VM. `Patala.Sidecar` gives up native types and loopback latency and gets an
  OS process boundary back, which on the BEAM is worth more than it sounds.

  Every request and response is the JSON the sidecar serves — `Patala.Sidecar`
  returns the same maps — so moving a call site between the two modes is a
  transport change, not a rewrite.
  """

  alias Patala.Native

  @methods ~w(id capabilities quote charge verify validate-destination webhook caveat providers)

  @typedoc "An open rail. A NIF resource, released by `close/1` or at GC."
  @type rail :: reference()

  @doc """
  Open a rail. `nil` (the default) is a deterministic offline MockRail.

  Pass a map or a JSON string for anything else, e.g.
  `%{rail: "mock", failing: true}` for a rail where every operation fails,
  which is how you exercise your error path with no network in sight.
  """
  @spec open(map() | binary() | nil) :: {:ok, rail()} | {:error, binary()}
  def open(config \\ nil)
  def open(nil), do: Native.new(nil)
  def open(config) when is_binary(config), do: Native.new(config)
  def open(config) when is_map(config), do: Native.new(JSON.encode!(config))

  @doc """
  Open a rail, run `fun`, and close it however `fun` exits. Prefer this: a
  `try/after` around `open/1` is the same thing written longhand.
  """
  @spec with_rail(map() | binary() | nil, (rail() -> result)) :: result | {:error, binary()}
        when result: term()
  def with_rail(config \\ nil, fun) when is_function(fun, 1) do
    case open(config) do
      {:ok, rail} ->
        try do
          fun.(rail)
        after
          close(rail)
        end

      {:error, _} = error ->
        error
    end
  end

  @doc "Release a rail. Idempotent."
  @spec close(rail()) :: :ok
  defdelegate close(rail), to: Native

  @doc "The patala version the loaded library was built from."
  @spec version() :: binary()
  defdelegate version(), to: Native, as: :abi_version

  @doc """
  Raise unless the loaded library is exactly `expected`.

  Worth doing at boot: a shared library is resolved off a load path you may not
  control, and a stale `libpatala_ffi` earlier on it is called silently.
  """
  @spec abi_check!(binary()) :: :ok
  def abi_check!(expected) do
    case Native.abi_check(expected) do
      :ok -> :ok
      {:error, message} -> raise RuntimeError, message
    end
  end

  @doc """
  One unary call, decoded. `request` may be a map, a JSON string, or `nil`.
  """
  @spec call(rail(), binary(), map() | binary() | nil) :: {:ok, map()} | {:error, binary()}
  def call(rail, method, request \\ nil)

  def call(rail, method, request) when method in @methods do
    body =
      case request do
        nil -> nil
        value when is_binary(value) -> value
        value when is_map(value) -> JSON.encode!(value)
      end

    with {:ok, json} <- Native.call(rail, method, body) do
      {:ok, JSON.decode!(json)}
    end
  end

  def call(_rail, method, _request) do
    {:error, "unknown method #{inspect(method)} (want one of: #{Enum.join(@methods, ", ")})"}
  end

  @doc "The rail's id, e.g. `\"mock\"`."
  @spec id(rail()) :: {:ok, binary()} | {:error, binary()}
  def id(rail) do
    with {:ok, %{"rail_id" => rail_id}} <- call(rail, "id"), do: {:ok, rail_id}
  end

  @doc """
  What this rail can do, without naming the provider behind it.

  `"class"` is `"NonCustodialFinal"` (a wallet address and a final receipt) or
  `"CustodialReversible"` (a card form and a refundable pending state). Branch
  your whole UX on it; it is not two shades of one thing.
  """
  @spec capabilities(rail()) :: {:ok, map()} | {:error, binary()}
  def capabilities(rail), do: call(rail, "capabilities")

  @spec quote(rail(), map()) :: {:ok, map()} | {:error, binary()}
  def quote(rail, request), do: call(rail, "quote", request)

  @doc """
  Move money. Store the returned Receipt and hand it back to `verify/2` later —
  that, and not `charge/2` having returned `:ok`, is the entitlement check.
  """
  @spec charge(rail(), map()) :: {:ok, map()} | {:error, binary()}
  def charge(rail, request), do: call(rail, "charge", request)

  @doc """
  Verify a Receipt. Returns `{:ok, %{"valid" => bool}}` — the document, not the
  bool, because `false` is the rail's honest fail-closed answer and NOT an
  error. Gate entitlement on `true` and never retry a `false` as though it were
  transient.
  """
  @spec verify(rail(), map()) :: {:ok, map()} | {:error, binary()}
  def verify(rail, receipt), do: call(rail, "verify", receipt)

  @doc """
  The offline pre-flight check, to run before any money moves.

  It never fails: "I cannot check this address" comes back as the verdict
  `%{"status" => "Unknown"}`, because a caller must handle that as carefully as
  a refusal and an error is too easy to swallow. Read `"is_refusal"` (do not
  send) and `"human_must_confirm"` — true on EVERY verdict, `"StructurallyValid"`
  included, because patala does not detect exchange-owned addresses.
  """
  @spec validate_destination(rail(), binary()) :: {:ok, map()} | {:error, binary()}
  def validate_destination(rail, destination) do
    call(rail, "validate-destination", %{destination: destination})
  end

  @doc """
  Authenticate an inbound processor delivery. Forward it VERBATIM — the exact
  body bytes, the headers, the query string — because every scheme signs what
  the processor actually sent, and a body that has been through a JSON round
  trip on your side is no longer that.
  """
  @spec webhook(rail(), binary(), keyword()) :: {:ok, map()} | {:error, binary()}
  def webhook(rail, body, opts \\ []) do
    payload = %{
      headers: Map.new(Keyword.get(opts, :headers, [])),
      query: Map.new(Keyword.get(opts, :query, [])),
      now_unix: Keyword.get(opts, :now_unix, System.system_time(:second))
    }

    payload =
      if Keyword.get(opts, :binary, false) do
        Map.put(payload, :body_hex, Base.encode16(body, case: :lower))
      else
        Map.put(payload, :body, body)
      end

    call(rail, "webhook", payload)
  end

  @doc """
  The sentence to show a human on the form where a payout address is first
  asked for, before there is a verdict to render.
  """
  @spec caveat(rail()) :: {:ok, binary()} | {:error, binary()}
  def caveat(rail) do
    with {:ok, %{"exchange_deposit_caveat" => text}} <- call(rail, "caveat"), do: {:ok, text}
  end

  @doc "Fiat providers this build can construct. Needs a library built with `--features fiat`."
  @spec providers(rail()) :: {:ok, [binary()]} | {:error, binary()}
  def providers(rail) do
    with {:ok, %{"providers" => list}} <- call(rail, "providers"), do: {:ok, list}
  end
end
