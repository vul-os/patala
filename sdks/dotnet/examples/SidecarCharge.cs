using System;
using System.Collections.Generic;
using System.Text;
using System.Threading.Tasks;
using Patala;

namespace Patala.Examples
{
    /// <summary>
    /// patala as a child process on <c>127.0.0.1</c> — the same
    /// charge -> verify round trip as <see cref="DirectCharge"/>, over HTTP.
    ///
    /// <para><b>MockRail, deliberately</b>, and here it is not even a choice:
    /// patala-sidecar's default registry contains exactly one rail and it is
    /// the offline mock. Nothing here opens a socket to anywhere but loopback.
    /// </para>
    ///
    /// <code>sdks/dotnet/run-examples.sh sidecar</code>
    /// </summary>
    internal static class SidecarCharge
    {
        private const string Alice = "mock:wallet:alice";

        internal static async Task<int> RunAsync()
        {
            // No token passed: 32 bytes from the OS CSPRNG are minted and
            // handed to the child. The server refuses to start without one.
            using var patala = Sidecar.Start();

            Console.WriteLine($"sidecar:  {patala.BaseUrl} (loopback, hardcoded in the server)");
            Console.WriteLine($"healthz:  {await patala.HealthzAsync()}   <- the one unauthenticated route");
            Console.WriteLine($"caps:     {await patala.CapabilitiesAsync()}");

            Console.WriteLine();
            Console.WriteLine("-- destination pre-flight (POST /v1/rails/mock/validate-destination) --");
            foreach (string candidate in new[] { Alice, "eth:wallet:alice", string.Empty })
            {
                string verdict = await patala.ValidateDestinationAsync(candidate);
                string shown = candidate.Length == 0 ? "\"\" (empty)" : $"\"{candidate}\"";
                Console.WriteLine(
                    $"  {shown} -> {Json.Field(verdict, "status")}, IsRefusal={Sidecar.IsRefusal(verdict)}");
            }
            Console.WriteLine("  all five verdicts are HTTP 200. A 200 means the rail ANSWERED,");
            Console.WriteLine("  not that the address is good — read the body, not the status.");

            string request = Json.PayRequest(1250, "USDC", Alice, "order-4711");

            Console.WriteLine();
            Console.WriteLine("-- quote -> charge -> verify --");
            Console.WriteLine($"  quote:   {await patala.QuoteAsync(request)}");

            string receipt = await patala.ChargeAsync(request);
            Console.WriteLine($"  receipt: {receipt}");

            Require(await patala.IsValidAsync(receipt), "a fresh receipt must verify");
            Console.WriteLine($"  IsValid(receipt):  {await patala.IsValidAsync(receipt)}"
                + "   <- 200; this, not the charge, is entitlement");

            string tampered = receipt.Replace("\"amount_minor\":1250", "\"amount_minor\":125000");
            Require(tampered != receipt, "the tamper did not apply");
            Require(!await patala.IsValidAsync(tampered), "a tampered receipt must NOT verify");
            Console.WriteLine($"  IsValid(tampered): {await patala.IsValidAsync(tampered)}"
                + "   <- also 200. false is data, not an HTTP error.");

            Console.WriteLine();
            Console.WriteLine("-- what this server refuses to pretend --");

            var headers = new Dictionary<string, string> { ["Stripe-Signature"] = "t=1,v1=deadbeef" };
            Console.WriteLine("  webhook (mock has no push delivery): "
                + await FailureAsync(() => patala.WebhookAsync(Encoding.UTF8.GetBytes("{}"), headers)));

            Console.WriteLine("  an unregistered rail: "
                + await FailureAsync(() => patala.CapabilitiesAsync("stellar")));
            Console.WriteLine("    ^ 404 because this process has never heard of it. The default");
            Console.WriteLine("      registry is mock-only; per-rail registration is unwritten.");

            // The token gate, exercised rather than described.
            using (var wrong = Sidecar.Attach(patala.BaseUrl, new string('0', 64)))
            {
                Console.WriteLine("  a wrong bearer token: "
                    + await FailureAsync(() => wrong.CapabilitiesAsync()));
            }

            Console.WriteLine();
            Console.WriteLine("SidecarCharge: OK — child process stopped.");
            return 0;
        }

        private static async Task<string> FailureAsync(Func<Task<string>> block)
        {
            try
            {
                return "UNEXPECTED SUCCESS: " + await block();
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
                throw new InvalidOperationException("SidecarCharge: FAILED — " + what);
            }
        }
    }
}
