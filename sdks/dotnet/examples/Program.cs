using System;
using System.Threading.Tasks;

namespace Patala.Examples
{
    /// <summary>
    /// Entry point for the two patala examples.
    ///
    /// <code>
    ///   dotnet run --project sdks/dotnet/examples -- direct
    ///   dotnet run --project sdks/dotnet/examples -- sidecar
    ///   dotnet run --project sdks/dotnet/examples -- checks
    /// </code>
    ///
    /// Prefer <c>sdks/dotnet/run-examples.sh</c>, which builds the shared
    /// library and the sidecar binary first.
    /// </summary>
    internal static class Program
    {
        internal static async Task<int> Main(string[] args)
        {
            string which = args.Length > 0 ? args[0] : "both";
            if (which is not ("both" or "direct" or "sidecar" or "checks"))
            {
                Console.Error.WriteLine(
                    $"unknown example: {which} (want: direct, sidecar, checks, both)");
                return 2;
            }

            int status = 0;

            // `checks` needs neither the shared library nor the sidecar
            // binary — it is the pure-C# half of this SDK, which is the half
            // that had a fail-open bug in it. Part of `both`, so
            // run-examples.sh with no argument runs it.
            if (which is "both" or "checks")
            {
                Console.WriteLine("================ Checks (pure C#, no library, no process) ========");
                status |= Checks.Run();
                Console.WriteLine();
            }

            if (which is "both" or "direct")
            {
                Console.WriteLine("================ DirectCharge (in-process, C ABI) ================");
                status |= await DirectCharge.RunAsync();
                Console.WriteLine();
            }

            if (which is "both" or "sidecar")
            {
                Console.WriteLine("================ SidecarCharge (child process, HTTP) =============");
                status |= await SidecarCharge.RunAsync();
                Console.WriteLine();
            }

            return status;
        }
    }
}
