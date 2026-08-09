defmodule Patala do
  @moduledoc """
  patala from Elixir, two ways.

  | mode | module | recommended |
  | --- | --- | --- |
  | sidecar — a separate OS process, JSON over loopback | `Patala.Sidecar` | **yes** |
  | direct — a dirty-IO NIF over the C ABI | `Patala.Direct` | works, with eyes open |

  Both speak the same JSON and return the same maps, so moving a call site
  between them is a transport change, not a rewrite.

  ## Why the sidecar is the default here, when it is not elsewhere

  llmux and openrate ship no in-process Elixir binding at all, and give as one
  of their reasons that a Go runtime inside a BEAM scheduler is a poor fit.
  That reason does not apply to patala: patala is Rust and brings no runtime,
  no GC, no signal handlers and no threads into your process. The direct path
  here therefore exists, works, and is measured.

  The reasons that remain are about the BEAM rather than about patala, and they
  are enough:

    * **A NIF cannot be killed.** `Task.await/2`'s timeout returns to your
      caller; the dirty scheduler thread keeps going. `Process.exit(pid, :kill)`
      does not reach into native code.
    * **A NIF cannot be supervised.** There is no process to restart, so the
      one recovery mechanism the platform is built around does not apply.
    * **A fault in native code takes the VM**, not one process. patala catches
      Rust panics at its own boundary and turns them into error strings — which
      removes patala's own bugs from that list, but not the binding's, nor a
      mismatched shared library's.
    * **Dirty schedulers are a fixed pool.** `+SDio` defaults to 10; ten
      concurrent in-flight rail calls saturate it, and the eleventh queues
      behind them, invisible to any BEAM-level backpressure you have.

  A sidecar answers all four with one OS process boundary, and adds key
  isolation: a non-custodial rail's signing key lives in whichever process
  calls charge.

  Reach for `Patala.Direct` when you are writing a CLI, a migration, a test
  helper, or a service where you would rather not supervise a second process
  and can accept the above. It is a real option, not a trap — just not the
  default.
  """

  @doc "The patala version this SDK's examples were written against."
  @spec version() :: binary()
  def version do
    Application.spec(:patala, :vsn) |> to_string()
  end
end
