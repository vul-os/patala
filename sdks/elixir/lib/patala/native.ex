defmodule Patala.Native do
  @moduledoc """
  The raw NIF over patala's C ABI. You almost certainly want `Patala.Direct`.

  Five functions, mirroring `patala-ffi/include/patala.h` one for one. Every
  request and response is the JSON `patala-sidecar` serves, so a body that works
  against one mode works against the other unchanged.

  ## Loading

  The NIF is `priv/patala_nif.so`, built by `make` (see `mix.exs`). It does not
  link `libpatala_ffi` — it `dlopen`s it at load time from a path resolved here,
  in Elixir, where you can read and override it:

    1. `PATALA_LIBRARY`
    2. `priv/` in this package
    3. `target/{debug,release}/` in a checkout of the patala workspace
    4. the bare soname, letting the dynamic loader search its own paths

  `load/0` is idempotent and returns `{:error, reason}` rather than raising, so
  a host that would rather fall back to `Patala.Sidecar` can decide for itself.

  ## Read this before using it in an OTP application

  A NIF is not a process. It cannot be killed, `Task.await`-timed-out, or
  supervised, and a segfault in it takes the whole VM with it. `README.md`
  argues that case properly and still recommends the sidecar as the default for
  Elixir — for reasons that are about the BEAM, not about patala.
  """

  @on_load :load

  @doc false
  def load do
    so = Path.join(:code.priv_dir(:patala), "patala_nif")

    case :erlang.load_nif(String.to_charlist(so), library_path()) do
      :ok -> :ok
      {:error, {:reload, _}} -> :ok
      other -> other
    end
  end

  @doc """
  The path this module will hand the NIF to `dlopen`. Exposed because "which
  library did it actually load" is the first question when something is wrong.
  """
  @spec library_path() :: binary()
  def library_path do
    case System.get_env("PATALA_LIBRARY") do
      value when is_binary(value) and value != "" ->
        value

      _ ->
        name = library_name()

        candidates =
          [Path.join(:code.priv_dir(:patala), name)] ++
            Enum.map(["debug", "release"], &Path.join([repo_root(), "target", &1, name]))

        Enum.find(candidates, name, &File.regular?/1)
    end
  end

  defp library_name do
    case :os.type() do
      {:unix, :darwin} -> "libpatala_ffi.dylib"
      {:win32, _} -> "patala_ffi.dll"
      _ -> "libpatala_ffi.so"
    end
  end

  # .../sdks/elixir/_build/<env>/lib/patala/priv -> the workspace root. Walking
  # up from the priv dir rather than __DIR__ keeps this correct for both a
  # source checkout and a compiled build.
  defp repo_root do
    Path.expand(Path.join([__DIR__, "..", "..", "..", ".."]))
  end

  @doc "The patala version the LOADED library was built from. Never freed — it is static."
  @spec abi_version() :: binary()
  def abi_version, do: :erlang.nif_error(:nif_not_loaded)

  @doc """
  `:ok` when the loaded library is exactly `expected`, `{:error, message}`
  otherwise. The comparison lives in the library so it is not reimplemented —
  and forgotten — in each binding.
  """
  @spec abi_check(binary()) :: :ok | {:error, binary()}
  def abi_check(_expected), do: :erlang.nif_error(:nif_not_loaded)

  @doc """
  Build a rail. `nil` means the offline default: a deterministic MockRail on
  USDC, no credentials and no network. Unknown fields in the config document
  are REFUSED, never ignored.

  Returns a resource. Its destructor calls `patala_close`, so a rail that goes
  out of scope is released at GC rather than leaking a handle — but call
  `close/1` when you are done, because GC is not a schedule.
  """
  @spec new(binary() | nil) :: {:ok, reference()} | {:error, binary()}
  def new(_config_json), do: :erlang.nif_error(:nif_not_loaded)

  @doc """
  One unary call. `method` is one of `id`, `capabilities`, `quote`, `charge`,
  `verify`, `validate-destination`, `webhook`, `caveat`, `providers`.

  Runs on a dirty IO scheduler: MockRail answers in microseconds, but a real
  rail talks to a network, and a NIF that occupies a normal scheduler for that
  long degrades every process sharing it.
  """
  @spec call(reference(), binary(), binary() | nil) :: {:ok, binary()} | {:error, binary()}
  def call(_rail, _method, _request_json), do: :erlang.nif_error(:nif_not_loaded)

  @doc "Release a rail. Idempotent, so cleanup paths can call it blindly."
  @spec close(reference()) :: :ok
  def close(_rail), do: :erlang.nif_error(:nif_not_loaded)
end
