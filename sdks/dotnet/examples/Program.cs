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
            if (which is not ("both" or "direct" or "sidecar"))
            {
                Console.Error.WriteLine($"unknown example: {which} (want: direct, sidecar, both)");
                return 2;
            }

            int status = 0;

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
