# patala from Elixir over the sidecar — a separate OS process, no NIF at all.
#
#     cd sdks/elixir && mix run examples/sidecar_charge.exs
#
# This spawns patala-sidecar on a free loopback port with a freshly generated
# token, drives a full quote -> charge -> verify against MockRail over
# :gen_tcp, and terminates it. Nothing is left running, and nothing outside the
# standard library is used — not even :inets.
#
# THIS IS THE RECOMMENDED DEFAULT FOR ELIXIR, and the last section shows why in
# the only terms that matter on the BEAM: a call you can abandon, a failure you
# can supervise, and a fault that cannot take the VM.
#
# Build the server first, from the workspace root:
#
#     cargo build -p patala-sidecar

defmodule SidecarExample do
  alias Patala.Sidecar

  @pay %{
    amount_minor: 1250,
    currency: "USDC",
    destination: "mock:wallet:alice",
    reference: "order-1"
  }

  def run do
    Process.put(:checks, 0)

    IO.puts("elixir #{System.version()} / OTP #{:erlang.system_info(:otp_release)}")

    {:ok, sc} = Sidecar.spawn()

    try do
      IO.puts("binary:  #{sc.binary}")
      IO.puts("listening on #{sc.base_url} (loopback only — the bind address is not configurable)")
      IO.puts("os pid:  #{sc.os_pid}   <- a real OS process, which is the entire point\n")

      IO.puts("capabilities")
      {:ok, caps} = Sidecar.capabilities(sc, "mock")

      check(
        caps["class"] == "NonCustodialFinal",
        "class is #{inspect(caps["class"])} — decide the whole UX off this, not off a provider name"
      )

      check(caps["holds_funds"] == false, "holds_funds is false")

      IO.puts("\npre-flight: validate-destination, before any money moves")
      {:ok, verdict} = Sidecar.validate_destination(sc, "mock", "mock:wallet:alice")

      check(
        verdict["status"] == "StructurallyValid",
        "a well-formed address -> 200 #{inspect(verdict["status"])}"
      )

      check(
        verdict["is_refusal"] == false,
        "is_refusal is false — read the body, not just the code"
      )

      check(
        verdict["human_must_confirm"] == true,
        "human_must_confirm is true even on StructurallyValid"
      )

      {:ok, 200, refused} =
        Sidecar.try(sc, "POST", "/v1/rails/mock/validate-destination", %{destination: ""})

      check(
        refused["status"] == "Malformed" and refused["is_refusal"],
        "an empty destination is a well-formed REQUEST -> 200 with a Malformed refusal"
      )

      IO.puts("\nquote -> charge -> verify")
      {:ok, quote} = Sidecar.quote(sc, "mock", @pay)

      check(
        quote["total_minor"] == 1250 and is_integer(quote["total_minor"]),
        "total_minor == #{quote["total_minor"]} and decodes as an integer — never a float"
      )

      {:ok, receipt} = Sidecar.charge(sc, "mock", @pay)

      check(
        receipt["amount_minor"] == 1250,
        "charge -> receipt for #{receipt["amount_minor"]} #{receipt["currency"]}"
      )

      check(
        Sidecar.verify(sc, "mock", receipt) == {:ok, %{"valid" => true}},
        "the genuine receipt verifies {\"valid\": true}"
      )

      tampered = Map.update!(receipt, "amount_minor", &(&1 + 1))
      {:ok, 200, body} = Sidecar.try(sc, "POST", "/v1/rails/mock/verify", tampered)

      check(
        body == %{"valid" => false},
        "a tampered receipt is 200 {\"valid\": false} — fail-closed, and NOT an HTTP error"
      )

      IO.puts("\nthe error surface, so you can tell these four apart")

      {:ok, 400, body} = Sidecar.try(sc, "POST", "/v1/rails/mock/charge", %{@pay | currency: "EUR"})
      check(true, "an unsupported currency -> 400 #{inspect(body["kind"])}")

      {:ok, 404, body} = Sidecar.try(sc, "GET", "/v1/rails/nope")
      check(true, "an unknown rail_id -> 404 #{inspect(body["kind"])}")

      {:error, {501, kind, _}} = Sidecar.webhook(sc, "mock", "{}")

      check(
        kind == "unsupported",
        "the mock has no push delivery -> 501 #{inspect(kind)}, never an invented event"
      )

      {:ok, 401, _} = Sidecar.try(sc, "GET", "/v1/rails/mock", nil, authed: false)
      check(true, "no Authorization header -> 401 on a READ-ONLY route too")

      beam_properties(sc)
    after
      Sidecar.stop(sc)
    end

    IO.puts("\nsidecar terminated; nothing left running")
    IO.puts("\nALL #{Process.get(:checks)} ELIXIR SIDECAR ASSERTIONS PASSED")
  end

  # The part a NIF cannot match, demonstrated rather than argued.
  defp beam_properties(sc) do
    IO.puts("\nwhat the process boundary buys, measured")

    # 1. An in-flight call can be abandoned. Task.shutdown actually stops the
    #    work here, because the work is a socket read in a BEAM process — a NIF
    #    on a dirty scheduler would keep going after the timeout returned.
    task = Task.async(fn -> Sidecar.charge(sc, "mock", @pay) end)

    case Task.yield(task, 5_000) || Task.shutdown(task, :brutal_kill) do
      {:ok, {:ok, _receipt}} ->
        check(true, "a charge inside a Task returns normally")

      other ->
        check(false, "unexpected task result: #{inspect(other)}")
    end

    slow = Task.async(fn -> Sidecar.charge(sc, "mock", @pay) end)
    Task.shutdown(slow, :brutal_kill)

    check(
      Process.alive?(slow.pid) == false,
      "Task.shutdown/:brutal_kill really stops it — there is a PROCESS to kill, which is the whole difference"
    )

    # 2. A supervisor can restart the caller, because the caller is a process.
    {:ok, supervisor} = Task.Supervisor.start_link()

    task =
      Task.Supervisor.async_nolink(supervisor, fn ->
        Sidecar.capabilities(sc, "definitely-not-a-rail")
      end)

    check(
      match?({:ok, {:error, {404, "unknown_rail", _}}}, Task.yield(task, 5_000)),
      "a failure comes back as data to a supervised process, not as a VM-wide event"
    )

    Supervisor.stop(supervisor)

    # 3. The sidecar is a separate OS process; a fault in it cannot take this VM.
    check(
      is_integer(sc.os_pid) and sc.os_pid != System.pid() |> String.to_integer(),
      "the rail runs in OS pid #{sc.os_pid}, this VM is #{System.pid()} — a segfault there is not a segfault here"
    )
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

SidecarExample.run()
