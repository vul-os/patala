# patala in-process from Elixir, through the dirty-IO NIF over the C ABI.
#
#     cd sdks/elixir && mix run examples/direct_charge.exs
#
# Everything runs against MockRail: deterministic, offline, no credentials and
# no network. patala is a payments library, so an example that moves real value
# is not an example.
#
# This path WORKS. It is still not the recommended default for Elixir, and the
# last section of this file is the reason — which is about the BEAM, not about
# patala. See README.md.
#
# Build the library first, from the workspace root:
#
#     cargo build -p patala-ffi

defmodule DirectExample do
  alias Patala.{Direct, Native}

  @pay %{
    amount_minor: 1250,
    currency: "USDC",
    destination: "mock:wallet:alice",
    reference: "order-1"
  }

  def run do
    Process.put(:checks, 0)

    IO.puts(
      "elixir #{System.version()} / #{:erlang.system_info(:otp_release) |> to_string()} " <>
        "(erts #{:erlang.system_info(:version) |> to_string()})"
    )

    IO.puts("library: #{Native.library_path()}")
    IO.puts("patala:  #{Direct.version()}")

    IO.puts(
      "schedulers: #{:erlang.system_info(:schedulers_online)} normal, " <>
        "#{:erlang.system_info(:dirty_io_schedulers)} dirty-IO\n"
    )

    IO.puts("the version probe, because a stale library earlier on the load path is silent")
    Direct.abi_check!(Direct.version())
    check(true, "abi_check! against the loaded version passes")

    check(
      match?({:error, _}, Native.abi_check("9.9.9")),
      "abi_check(\"9.9.9\") returns {:error, message} naming both versions"
    )

    Direct.with_rail(fn rail ->
      IO.puts("\ncapabilities")
      {:ok, id} = Direct.id(rail)
      {:ok, caps} = Direct.capabilities(rail)
      check(id == "mock", "id() == #{inspect(id)}")

      check(
        caps["class"] == "NonCustodialFinal",
        "class is #{inspect(caps["class"])} — a wallet address and a final receipt, not a card form"
      )

      check(caps["holds_funds"] == false, "holds_funds is false — patala never holds funds")
      check(caps["reversible"] == false, "reversible is false — there is no refund on this rail")

      IO.puts("\npre-flight: validate_destination, before any money moves")
      {:ok, verdict} = Direct.validate_destination(rail, "mock:wallet:alice")

      check(
        verdict["status"] == "StructurallyValid",
        "a well-formed address gives status #{inspect(verdict["status"])}"
      )

      check(verdict["is_refusal"] == false, "is_refusal is false — a field, never re-derived")

      check(
        verdict["human_must_confirm"] == true,
        "human_must_confirm is true even here — patala does not detect exchange addresses"
      )

      {:ok, refused} = Direct.validate_destination(rail, "")

      check(
        refused["status"] == "Malformed" and refused["is_refusal"],
        "an empty destination is a Malformed refusal — a verdict in {:ok, _}, never {:error, _}"
      )

      {:ok, caveat} = Direct.caveat(rail)

      check(
        caveat != "",
        "caveat/1 is the sentence for the address form: #{String.slice(caveat, 0, 44)}…"
      )

      IO.puts("\nquote -> charge -> verify")
      {:ok, quote} = Direct.quote(rail, @pay)

      check(
        quote["total_minor"] == 1250 and is_integer(quote["total_minor"]),
        "total_minor == #{quote["total_minor"]} and is an integer — minor units, never a float"
      )

      {:ok, receipt} = Direct.charge(rail, @pay)

      check(
        receipt["amount_minor"] == 1250,
        "charge -> receipt for #{receipt["amount_minor"]} #{receipt["currency"]}"
      )

      check(
        Direct.verify(rail, receipt) == {:ok, %{"valid" => true}},
        "the genuine receipt verifies true"
      )

      tampered = Map.update!(receipt, "amount_minor", &(&1 + 1))

      check(
        Direct.verify(rail, tampered) == {:ok, %{"valid" => false}},
        "a tampered receipt is {:ok, %{\"valid\" => false}} — fail-closed, and false is DATA"
      )

      IO.puts("\nerrors are {:error, message}, never a crash in the VM")
      result = Direct.charge(rail, %{@pay | currency: "EUR"})

      check(
        match?({:error, "patala: invalid request:" <> _}, result),
        "an unsupported currency: #{inspect(elem(result, 1))}"
      )

      check(
        match?({:error, "unknown method" <> _}, Direct.call(rail, "nope")),
        "an unknown method is caught before the NIF call"
      )

      check(
        match?({:error, _}, Direct.webhook(rail, "{}")),
        "the mock has no push delivery and refuses rather than inventing an event"
      )

      IO.puts("\nthe boundary holds: bad input is an error, not a segfault")
      {:ok, scratch} = Direct.open()
      :ok = Direct.close(scratch)

      check(
        Direct.capabilities(scratch) == {:error, "this rail is closed"},
        "use-after-close says so — handles are registry keys, never pointers, and never reused"
      )

      check(
        Direct.close(scratch) == :ok,
        "closing twice is a no-op, so cleanup paths can be idempotent"
      )

      check(
        match?({:error, _}, Direct.open(%{rail: "mock", currencys: ["USDC"]})),
        "a misspelled config field is REFUSED, not defaulted to a currency list you did not choose"
      )
    end)

    concurrency()

    IO.puts("\nALL #{Process.get(:checks)} ELIXIR DIRECT ASSERTIONS PASSED")
  end

  # The dirty-IO pool is the concrete version of "a NIF is not a process". It is
  # a fixed number of OS threads, decided at VM start (+SDio, default 10), and
  # nothing at the BEAM level queues behind it visibly.
  defp concurrency do
    IO.puts("\nconcurrency, measured — the dirty-IO pool is a fixed resource")

    {:ok, rail} = Direct.open()
    ticker = spawn_link(fn -> tick(0) end)

    {micros, counts} =
      :timer.tc(fn ->
        1..40
        |> Task.async_stream(fn _ -> Enum.each(1..200, fn _ -> Direct.charge(rail, @pay) end) end,
          max_concurrency: 40,
          timeout: 60_000
        )
        |> Enum.count()
      end)

    send(ticker, {:report, self()})

    ticks =
      receive do
        {:ticks, n} -> n
      after
        1_000 -> 0
      end

    Process.unlink(ticker)
    Process.exit(ticker, :kill)
    Direct.close(rail)

    total = counts * 200

    IO.puts(
      "    #{total} charges from 40 concurrent processes in #{Float.round(micros / 1000, 1)} ms " <>
        "(#{round(total / (micros / 1_000_000))}/s)"
    )

    IO.puts("    a plain BEAM process kept scheduling throughout: #{ticks} iterations")
    check(ticks > 0, "normal schedulers stayed free — that is what ERL_NIF_DIRTY_JOB_IO_BOUND buys")
  end

  defp tick(n) do
    receive do
      {:report, from} -> send(from, {:ticks, n})
    after
      0 -> tick(n + 1)
    end
  end

  defp check(true, message) do
    Process.put(:checks, Process.get(:checks) + 1)
    IO.puts("  ok  #{message}")
  end

  defp check(_false, message) do
    IO.puts(:stderr, "FAILED: #{message}")
    System.halt(1)
  end
end

DirectExample.run()
