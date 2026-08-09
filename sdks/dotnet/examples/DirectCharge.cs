using System;
using System.IO;
using System.Threading.Tasks;
using Patala;

namespace Patala.Examples
{
    /// <summary>
    /// patala in this process, through the C ABI — a full charge -> verify
    /// round trip against the offline MockRail.
    ///
    /// <para><b>MockRail, deliberately.</b> patala is a payments library and
    /// an example that moves real value is not an example. Nothing here opens
    /// a socket.</para>
    ///
    /// <code>sdks/dotnet/run-examples.sh direct</code>
    /// </summary>
    internal static class DirectCharge
    {
        private const string Alice = "mock:wallet:alice";

        internal static Task<int> RunAsync()
        {
            string library = Direct.FindLibrary();
            Console.WriteLine($"library: {library}");
            Console.WriteLine($"         {new FileInfo(library).Length} bytes");

            Direct.AbiCheck();
            Console.WriteLine($"abi version: {Direct.AbiVersion()} (compared by the library, not by us)");

            // Creating a rail talks to nothing: no socket, no thread, no
            // environment variable.
            using (var rail = Direct.Mock(feeMinor: 25))
            {
                Console.WriteLine($"id:           {rail.Id()}");
                Console.WriteLine($"capabilities: {rail.Capabilities()}");

                Console.WriteLine();
                Console.WriteLine("-- destination pre-flight --");
                foreach (string candidate in new[] { Alice, "eth:wallet:alice", string.Empty })
                {
                    string verdict = rail.ValidateDestination(candidate);
                    string shown = candidate.Length == 0 ? "\"\" (empty)" : $"\"{candidate}\"";
                    Console.WriteLine(
                        $"  {shown} -> {Json.Field(verdict, "status")}"
                        + $", IsRefusal={rail.IsRefusal(verdict)}"
                        + $", human_must_confirm={Json.Field(verdict, "human_must_confirm")}");
                }
                Console.WriteLine("  human_must_confirm is true on EVERY verdict, StructurallyValid included.");
                Console.WriteLine("  patala does not detect exchange-owned addresses and will not guess.");

                // A rail configured without destination checks: the offline
                // stand-in for a fiat rail, whose destination is an opaque
                // processor-side token.
                using (var opaque = Direct.Mock(destinationChecks: false))
                {
                    string verdict = opaque.ValidateDestination(Alice);
                    Console.WriteLine(
                        $"  the same address on a rail that cannot check: {Json.Field(verdict, "status")}"
                        + $", IsRefusal={opaque.IsRefusal(verdict)}");
                    Console.WriteLine("  Unknown is NOT a refusal and is NOT an approval. It needs a human.");
                }

                // ------------------------------------------------- the money
                string request = Json.PayRequest(
                    amountMinor: 1250,   // integer minor units. Never a float.
                    currency: "USDC",
                    destination: Alice,
                    reference: "order-4711");

                Console.WriteLine();
                Console.WriteLine("-- quote -> charge -> verify --");
                Console.WriteLine($"  request: {request}");
                Console.WriteLine($"  quote:   {rail.Quote(request)}");

                string receipt = rail.Charge(request);
                Console.WriteLine($"  receipt: {receipt}");

                // THIS is the entitlement check. Not "Charge returned".
                Require(rail.IsValid(receipt), "a fresh receipt must verify");
                Console.WriteLine($"  IsValid(receipt):  {rail.IsValid(receipt)}");

                string tampered = receipt.Replace("\"amount_minor\":1250", "\"amount_minor\":125000");
                Require(tampered != receipt, "the tamper did not apply");
                Require(!rail.IsValid(tampered), "a tampered receipt must NOT verify");
                Console.WriteLine($"  IsValid(tampered): {rail.IsValid(tampered)}   <- an ordinary result, not an exception");

                Console.WriteLine();
                Console.WriteLine("-- what this rail refuses to pretend --");
                Console.WriteLine("  webhook:  " + Failure(() => rail.Call("webhook",
                    "{\"body\":\"{}\",\"headers\":{},\"now_unix\":1700000000}")));
                Console.WriteLine("  unknown:  " + Failure(() => rail.Call("settle-later")));
            }

            using (var broken = Direct.Mock(failing: true))
            {
                Console.WriteLine("  a failing rail: "
                    + Failure(() => broken.Charge(Json.PayRequest(1, "USDC", Alice, "x"))));
            }

            // Handles are retired, not recycled: a stale handle can never land
            // on somebody else's rail.
            var closed = Direct.Mock();
            closed.Dispose();
            closed.Dispose();   // idempotent
            Console.WriteLine();
            Console.WriteLine("after dispose: " + Failure(() => closed.Id()));

            Console.WriteLine();
            Console.WriteLine("DirectCharge: OK — offline, no socket opened, no thread started.");
            return Task.FromResult(0);
        }

        private static string Failure(Func<string> block)
        {
            try
            {
                return "UNEXPECTED SUCCESS: " + block();
            }
            catch (PatalaException e)
            {
                return e.Message;
            }
        }

        private static void Require(bool condition, string what)
        {
            if (!condition)
            {
                throw new InvalidOperationException("DirectCharge: FAILED — " + what);
            }
        }
    }
}
