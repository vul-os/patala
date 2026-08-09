defmodule Patala.Sidecar do
  @moduledoc """
  patala from Elixir over the sidecar — a separate OS process, JSON over
  loopback, and **the recommended default for Elixir**.

      {:ok, sc} = Patala.Sidecar.spawn()
      {:ok, receipt} = Patala.Sidecar.charge(sc, "mock", %{
        amount_minor: 1250, currency: "USDC",
        destination: "mock:wallet:alice", reference: "order-1"
      })
      {:ok, %{"valid" => true}} = Patala.Sidecar.verify(sc, "mock", receipt)
      :ok = Patala.Sidecar.stop(sc)

      # or point at one somebody else runs — the usual production shape:
      sc = Patala.Sidecar.connect("http://127.0.0.1:8420", System.fetch_env!("PATALA_SIDECAR_TOKEN"))

  ## Why this is the default here and not merely an option

  patala's in-process cost is unusually low — it is Rust, so no runtime, no GC,
  no signal handlers, no threads (see `README.md`, where that is measured). The
  reasons to prefer a separate process on the BEAM are not about patala:

    * a NIF **cannot be killed**, so `Task.await/2`'s timeout returns to your
      caller while the dirty scheduler stays occupied;
    * a NIF **cannot be supervised** — there is no process to restart;
    * a fault in native code takes **the whole VM**, not one process.

  A port or an OS process restores all three, and the sidecar is an OS process
  you already have. It also buys **key isolation**: a non-custodial rail's
  signing key lives in whichever process calls charge.

  ## Spawning

  `spawn/1` starts the binary with a fresh 32-byte token on a free loopback
  port and waits for `/healthz`.

  **Always call `stop/1`.** `Port.close/1` — and therefore an owner that simply
  dies — shuts the pipes without signalling the child, and `patala-sidecar`
  keeps serving with its stdin at EOF. `stop/1` signals the OS pid; see its own
  docs for the failure this avoids, which is measured rather than theoretical.
  """

  alias Patala.HTTP

  @enforce_keys [:base_url, :token]
  defstruct [:base_url, :token, :port, :binary, :os_pid]

  @type t :: %__MODULE__{
          base_url: String.t(),
          token: String.t(),
          port: port() | nil,
          binary: String.t() | nil,
          os_pid: non_neg_integer() | nil
        }

  @doc "A client for a sidecar somebody else is running."
  @spec connect(String.t(), String.t()) :: t()
  def connect(base_url, token) do
    %__MODULE__{base_url: String.trim_trailing(base_url, "/"), token: token}
  end

  @doc """
  Start a sidecar and wait for it to answer `/healthz`.

  Options: `:binary`, `:port`, `:token`, `:timeout` (ms, default 10_000).
  """
  @spec spawn(keyword()) :: {:ok, t()} | {:error, term()}
  def spawn(opts \\ []) do
    binary = Keyword.get_lazy(opts, :binary, &resolve_binary/0)

    token =
      Keyword.get_lazy(opts, :token, fn ->
        Base.encode16(:crypto.strong_rand_bytes(32), case: :lower)
      end)

    tcp_port = Keyword.get_lazy(opts, :port, &free_port/0)

    case System.find_executable(binary) do
      nil ->
        {:error, "could not find #{inspect(binary)}. Build it first: cargo build -p patala-sidecar"}

      executable ->
        port =
          Port.open({:spawn_executable, executable}, [
            :binary,
            :exit_status,
            :hide,
            args: [],
            env: [
              {~c"PATALA_SIDECAR_TOKEN", String.to_charlist(token)},
              {~c"PATALA_SIDECAR_PORT", String.to_charlist(Integer.to_string(tcp_port))}
            ]
          ])

        sidecar = %__MODULE__{
          base_url: "http://127.0.0.1:#{tcp_port}",
          token: token,
          port: port,
          binary: executable,
          os_pid: os_pid(port)
        }

        case await_healthy(sidecar, Keyword.get(opts, :timeout, 10_000)) do
          :ok ->
            {:ok, sidecar}

          {:error, reason} ->
            stop(sidecar)
            {:error, reason}
        end
    end
  end

  @doc "Start a sidecar, run `fun`, and stop it however `fun` exits."
  @spec with_sidecar(keyword(), (t() -> result)) :: result | {:error, term()} when result: term()
  def with_sidecar(opts \\ [], fun) when is_function(fun, 1) do
    case __MODULE__.spawn(opts) do
      {:ok, sidecar} ->
        try do
          fun.(sidecar)
        after
          stop(sidecar)
        end

      {:error, _} = error ->
        error
    end
  end

  @doc """
  Terminate a sidecar this VM spawned. A no-op for a `connect/2` client.

  **`Port.close/1` alone is not enough, and this is worth knowing before you
  copy the spawn code.** Closing a port shuts the pipes; it does not signal the
  OS process. `patala-sidecar` keeps serving with its stdin at EOF — quite
  correctly, it is a network server, not a filter — so the child outlives the
  VM that started it, still holding the port it was given. The symptom is a
  script that prints its last line and never exits, because an orphan is
  holding the inherited stdout open.

  So: SIGTERM the pid, wait for the exit status the port will deliver, and only
  then close the port.
  """
  @spec stop(t()) :: :ok
  def stop(%__MODULE__{port: nil}), do: :ok

  def stop(%__MODULE__{port: port, os_pid: os_pid}) do
    if is_integer(os_pid) do
      System.cmd("kill", ["-TERM", Integer.to_string(os_pid)], stderr_to_stdout: true)
    end

    if Port.info(port) != nil do
      receive do
        {^port, {:exit_status, _}} -> :ok
      after
        2_000 ->
          if is_integer(os_pid) do
            System.cmd("kill", ["-KILL", Integer.to_string(os_pid)], stderr_to_stdout: true)
          end
      end

      if Port.info(port) != nil, do: Port.close(port)
    end

    :ok
  catch
    :error, :badarg -> :ok
  end

  @doc "`/healthz` — the one unauthenticated route, and it reveals only liveness."
  @spec healthy?(t()) :: boolean()
  def healthy?(sidecar) do
    match?({:ok, 200, "ok"}, HTTP.request(sidecar.base_url, "GET", "/healthz", nil, [], 1_000))
  end

  # ---------------------------------------------------------------- the API

  @spec capabilities(t(), String.t()) :: {:ok, map()} | {:error, term()}
  def capabilities(sidecar, rail_id), do: get(sidecar, "/v1/rails/#{rail_id}")

  @spec quote(t(), String.t(), map()) :: {:ok, map()} | {:error, term()}
  def quote(sidecar, rail_id, request), do: post(sidecar, "/v1/rails/#{rail_id}/quote", request)

  @doc """
  Move money. Store the returned Receipt and hand it back to `verify/3` later —
  that, not `charge/3` returning `:ok`, is the entitlement check.
  """
  @spec charge(t(), String.t(), map()) :: {:ok, map()} | {:error, term()}
  def charge(sidecar, rail_id, request), do: post(sidecar, "/v1/rails/#{rail_id}/charge", request)

  @doc """
  Verify a Receipt. `%{"valid" => false}` arrives with HTTP **200** on purpose:
  a rail's fail-closed refusal is data, and must not be confusable with a
  broken sidecar. Gate entitlement on `true` and never retry a `false`.
  """
  @spec verify(t(), String.t(), map()) :: {:ok, map()} | {:error, term()}
  def verify(sidecar, rail_id, receipt), do: post(sidecar, "/v1/rails/#{rail_id}/verify", receipt)

  @doc """
  The offline pre-flight check. All five verdicts are `200`, refusals included —
  branch on `"status"` and `"is_refusal"`. A `400` means the REQUEST was
  malformed and carries no verdict fields at all, so a rejected request can
  never be mistaken for a checked address.
  """
  @spec validate_destination(t(), String.t(), String.t()) :: {:ok, map()} | {:error, term()}
  def validate_destination(sidecar, rail_id, destination) do
    post(sidecar, "/v1/rails/#{rail_id}/validate-destination", %{destination: destination})
  end

  @doc """
  Forward a processor's webhook VERBATIM. `raw_body` must be the exact bytes off
  the wire — this endpoint's body is deliberately not re-encoded JSON, because
  every webhook scheme signs what was actually sent.
  """
  @spec webhook(t(), String.t(), binary(), [{String.t(), String.t()}]) ::
          {:ok, map()} | {:error, term()}
  def webhook(sidecar, rail_id, raw_body, headers \\ []) do
    with {:ok, status, body} <-
           req(sidecar, "POST", "/v1/rails/#{rail_id}/webhook", raw_body, headers),
         :ok <- ok_status(status, body) do
      {:ok, decode(body)}
    end
  end

  # ------------------------------------------------------------- transport

  @doc """
  One call, returning `{:status, body}` rather than raising or wrapping. Use it
  where the status IS what you want to look at — 400 / 404 / 501 / 401 mean
  four genuinely different things here.
  """
  @spec try(t(), String.t(), String.t(), map() | nil, keyword()) ::
          {:ok, non_neg_integer(), term()} | {:error, term()}
  def try(sidecar, method, path, body \\ nil, opts \\ []) do
    encoded = if body, do: JSON.encode!(body), else: nil
    headers = if Keyword.get(opts, :authed, true), do: auth(sidecar), else: []
    headers = if encoded, do: [{"Content-Type", "application/json"} | headers], else: headers

    with {:ok, status, raw} <- HTTP.request(sidecar.base_url, method, path, encoded, headers) do
      {:ok, status, decode(raw)}
    end
  end

  defp get(sidecar, path) do
    with {:ok, status, raw} <- req(sidecar, "GET", path, nil, []),
         :ok <- ok_status(status, raw) do
      {:ok, decode(raw)}
    end
  end

  defp post(sidecar, path, body) do
    encoded = JSON.encode!(body)

    with {:ok, status, raw} <-
           req(sidecar, "POST", path, encoded, [{"Content-Type", "application/json"}]),
         :ok <- ok_status(status, raw) do
      {:ok, decode(raw)}
    end
  end

  defp req(sidecar, method, path, body, headers) do
    HTTP.request(sidecar.base_url, method, path, body, headers ++ auth(sidecar))
  end

  defp auth(sidecar), do: [{"Authorization", "Bearer " <> sidecar.token}]

  defp ok_status(status, _body) when status in 200..299, do: :ok

  defp ok_status(status, body) do
    case decode(body) do
      %{"kind" => kind, "error" => message} -> {:error, {status, kind, message}}
      other -> {:error, {status, other}}
    end
  end

  defp decode(""), do: nil

  defp decode(raw) do
    case JSON.decode(raw) do
      {:ok, decoded} -> decoded
      {:error, _} -> raw
    end
  end

  # ------------------------------------------------------------- internals

  defp await_healthy(sidecar, timeout) do
    deadline = System.monotonic_time(:millisecond) + timeout
    do_await(sidecar, deadline)
  end

  defp do_await(sidecar, deadline) do
    cond do
      healthy?(sidecar) ->
        :ok

      System.monotonic_time(:millisecond) > deadline ->
        {:error, "patala-sidecar never became healthy at #{sidecar.base_url}"}

      true ->
        receive do
          {port, {:exit_status, code}} when port == sidecar.port ->
            {:error, "patala-sidecar exited with #{code} before answering /healthz"}
        after
          50 -> do_await(sidecar, deadline)
        end
    end
  end

  defp os_pid(port) do
    case Port.info(port, :os_pid) do
      {:os_pid, pid} -> pid
      _ -> nil
    end
  end

  defp resolve_binary do
    case System.get_env("PATALA_SIDECAR_BIN") do
      value when is_binary(value) and value != "" ->
        value

      _ ->
        root = Path.expand(Path.join([__DIR__, "..", "..", "..", ".."]))

        ["debug", "release"]
        |> Enum.map(&Path.join([root, "target", &1, "patala-sidecar"]))
        |> Enum.find("patala-sidecar", &File.regular?/1)
    end
  end

  defp free_port do
    {:ok, socket} = :gen_tcp.listen(0, [:binary, ip: {127, 0, 0, 1}])
    {:ok, port} = :inet.port(socket)
    :gen_tcp.close(socket)
    port
  end
end
