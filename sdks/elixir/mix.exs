defmodule Patala.MixProject do
  use Mix.Project

  # Read from the workspace's VERSION file so this package cannot drift from
  # the Rust crates it binds.
  @version Path.join(__DIR__, "../../VERSION") |> File.read!() |> String.trim()

  def project do
    [
      app: :patala,
      version: @version,
      elixir: "~> 1.15",
      start_permanent: Mix.env() == :prod,
      # The NIF is built by `make` before Elixir compilation. There is no
      # elixir_make dependency on purpose: this package has no deps at all, and
      # adding one to shell out to make would be a poor trade for a single
      # compiler invocation. The compiler is tolerant — if `make` or a C
      # compiler is missing, Patala.Sidecar (which needs neither) still works,
      # and Patala.Native says so clearly when it cannot load.
      compilers: [:patala_nif] ++ Mix.compilers(),
      description: "patala from Elixir: the local sidecar, or a dirty-IO NIF over the C ABI",
      package: package(),
      deps: deps(),
      docs: [main: "Patala"]
    ]
  end

  def application do
    # No supervision tree: the direct path is a NIF (no process to supervise —
    # which is one of the reasons the sidecar is the recommended default) and
    # the sidecar path is an OS process the caller owns.
    [extra_applications: [:crypto]]
  end

  defp deps do
    # None. JSON is Elixir's own (1.18+), HTTP is :gen_tcp — see
    # Patala.HTTP for why not even :inets.
    []
  end

  defp package do
    [
      maintainers: ["imranparuk"],
      licenses: ["MIT", "Apache-2.0"],
      links: %{"patala" => "https://vulos.org/projects/patala"},
      files: ~w(lib c_src examples Makefile mix.exs README.md)
    ]
  end
end

defmodule Mix.Tasks.Compile.PatalaNif do
  @moduledoc """
  Builds `priv/patala_nif.so` via the Makefile, then copies it where the
  compiled application will look for it (`_build/<env>/lib/patala/priv`).

  A missing `make` or C compiler is a WARNING, not an error: `Patala.Sidecar`
  needs neither, and failing the whole build for the optional half would be
  the wrong trade.
  """
  use Mix.Task

  @shortdoc "Compiles the patala NIF"

  def run(_args) do
    root = Path.expand(__DIR__)

    case System.cmd("make", ["--no-print-directory"], cd: root, stderr_to_stdout: true) do
      {output, 0} ->
        if String.trim(output) != "", do: Mix.shell().info(output)
        copy_priv(root)
        :ok

      {output, status} ->
        Mix.shell().error("""
        patala: could not build the NIF (make exited #{status}).

        #{output}
        Patala.Sidecar still works — it needs no native code at all. Patala.Direct
        will report that the NIF could not be loaded.
        """)

        :ok
    end
  rescue
    error in ErlangError ->
      Mix.shell().error("patala: could not run `make` (#{inspect(error)}); skipping the NIF.")
      :ok
  end

  # :code.priv_dir/1 resolves inside _build, not the source tree, so the .so has
  # to be reachable from there.
  #
  # Mix usually SYMLINKS _build/<env>/lib/patala/priv at the source priv/, in
  # which case there is nothing to do — and copying anyway is actively harmful:
  # File.cp! of a file onto itself through a symlink truncates it to zero bytes,
  # which then dlopens with an empty error message and looks like a linker
  # problem for as long as it takes to run `ls -la priv/`. Compare the resolved
  # paths and copy only when they genuinely differ.
  defp copy_priv(root) do
    source = Path.join(root, "priv/patala_nif.so")
    target_dir = Path.join(Mix.Project.app_path(), "priv")
    target = Path.join(target_dir, "patala_nif.so")

    if File.regular?(source) and resolve(source) != resolve(target) do
      File.mkdir_p!(target_dir)
      File.cp!(source, target)
    end
  end

  defp resolve(path) do
    case File.read_link(Path.dirname(path)) do
      {:ok, link} ->
        Path.expand(Path.join([Path.dirname(Path.dirname(path)), link, Path.basename(path)]))

      _ ->
        Path.expand(path)
    end
  end
end
