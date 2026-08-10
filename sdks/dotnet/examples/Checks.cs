using System;
using System.Text.Json;
using Patala;

namespace Patala.Examples
{
    /// <summary>
    /// Counted assertions over the parts of this SDK that make a decision.
    ///
    /// <para>Everything money-shaped in patala is decided in Rust, and the two
    /// examples exercise that end to end. This file covers the small remainder
    /// that is decided <b>here</b>, in C#: reading one field out of a document
    /// patala returned. That is a short list, and it is the list that was
    /// wrong.</para>
    ///
    /// <para><see cref="Patala.Sidecar.IsRefusal"/> and
    /// <see cref="Patala.PatalaRail.IsRefusal"/> were
    /// <c>Json.Field(json, "is_refusal") == "true"</c> over a substring scan
    /// that did not skip whitespace after the colon. A verdict re-serialised
    /// with <c>WriteIndented</c>, or passed through any proxy that reformats
    /// JSON, yields <c>" true"</c> — so both returned <b>false for a Malformed
    /// verdict</b>, and the payout gate this SDK's README recommends sends the
    /// money. Nothing in this SDK tested it, because nothing in this SDK
    /// tested anything.</para>
    ///
    /// <para>The count is asserted, following <c>sdks/kotlin/checks</c> and
    /// <c>sdks/swift/Sources/patala-checks</c>: a suite that silently stops
    /// running half its assertions is worse than no suite.</para>
    ///
    /// <code>sdks/dotnet/run-examples.sh checks</code>
    /// </summary>
    internal static class Checks
    {
        /// <summary>Every assertion below is counted against this.</summary>
        private const int Expected = 27;

        private static int _ran;
        private static int _failed;

        private static void Check(string what, bool ok)
        {
            _ran++;
            if (ok)
            {
                Console.WriteLine($"  ok   {what}");
            }
            else
            {
                _failed++;
                Console.WriteLine($"  FAIL {what}");
            }
        }

        /// <summary>A Malformed verdict, exactly as patala serialises one.</summary>
        private const string Refusal =
            "{\"rail_id\":\"mock\",\"status\":\"Malformed\",\"reason\":\"empty\","
            + "\"is_refusal\":true,\"human_must_confirm\":true,\"caveat\":\"...\"}";

        /// <summary>A StructurallyValid verdict — not a refusal, not an approval.</summary>
        private const string NotRefusal =
            "{\"rail_id\":\"mock\",\"status\":\"StructurallyValid\",\"reason\":\"ok\","
            + "\"is_refusal\":false,\"human_must_confirm\":true,\"caveat\":\"...\"}";

        internal static int Run()
        {
            Console.WriteLine("-- Json.Flag: the one decision this SDK makes in C# --");

            Check("a refusal is a refusal", Json.Flag(Refusal, "is_refusal", true));
            Check("a non-refusal is not", !Json.Flag(NotRefusal, "is_refusal", true));

            // THE regression. System.Text.Json's own WriteIndented output is
            // the shortest way to produce it, and it is what a caller who logs
            // a verdict through their own serializer hands back.
            string indented;
            using (JsonDocument doc = JsonDocument.Parse(Refusal))
            {
                indented = JsonSerializer.Serialize(
                    doc.RootElement, new JsonSerializerOptions { WriteIndented = true });
            }
            Check("the reformatted refusal really does have a space after the colon",
                indented.Contains("\"is_refusal\": true", StringComparison.Ordinal));
            Check("a refusal re-serialised with WriteIndented is STILL a refusal",
                Json.Flag(indented, "is_refusal", true));
            Check("...and the same document is still not a refusal when it says so",
                !Json.Flag(
                    "{\n  \"status\": \"Unknown\",\n  \"is_refusal\": false\n}", "is_refusal", true));

            // Fail-closed on every kind of doubt. Each of these is a document
            // this SDK cannot read, and the answer to "may I send money to
            // this address?" when you cannot read the verdict is no.
            Check("an unparseable document is a refusal",
                Json.Flag("{not json", "is_refusal", true));
            Check("an empty string is a refusal", Json.Flag("", "is_refusal", true));
            Check("a JSON array is a refusal", Json.Flag("[true]", "is_refusal", true));
            Check("a document with no is_refusal is a refusal",
                Json.Flag("{\"status\":\"Malformed\"}", "is_refusal", true));
            Check("a STRING \"true\" is not a boolean, so it is a refusal",
                Json.Flag("{\"is_refusal\":\"true\"}", "is_refusal", true));
            Check("a STRING \"false\" is not a boolean either, so it is also a refusal",
                Json.Flag("{\"is_refusal\":\"false\"}", "is_refusal", true));
            Check("a number is not a boolean, so it is a refusal",
                Json.Flag("{\"is_refusal\":1}", "is_refusal", true));
            Check("null is not a boolean, so it is a refusal",
                Json.Flag("{\"is_refusal\":null}", "is_refusal", true));
            Check("is_refusal nested inside another object does not count",
                Json.Flag("{\"inner\":{\"is_refusal\":false}}", "is_refusal", true));
            Check("the fallback is the caller's, not a hardcoded true",
                !Json.Flag("{}", "is_refusal", false));

            Console.WriteLine();
            Console.WriteLine("-- Sidecar.IsRefusal: the same helper, fail-closed --");

            Check("Sidecar.IsRefusal on a Malformed verdict", Sidecar.IsRefusal(Refusal));
            Check("Sidecar.IsRefusal on a reformatted Malformed verdict",
                Sidecar.IsRefusal(indented));
            Check("Sidecar.IsRefusal is false only when the verdict says so",
                !Sidecar.IsRefusal(NotRefusal));
            Check("Sidecar.IsRefusal on an unreadable verdict is a refusal",
                Sidecar.IsRefusal("<html>502 Bad Gateway</html>"));

            Console.WriteLine();
            Console.WriteLine("-- Json.Field: for printing, and top-level only --");

            Check("a string field comes back unquoted",
                Json.Field(Refusal, "status") == "Malformed");
            Check("a string field survives reformatting",
                Json.Field(indented, "status") == "Malformed");
            Check("a u64 keeps every digit and never becomes a double",
                Json.Field("{\"amount_minor\":18446744073709551615}", "amount_minor")
                    == "18446744073709551615");
            Check("an absent key is null", Json.Field("{}", "status") == null);
            Check("an unparseable document is null", Json.Field("{not json", "status") == null);
            Check("a nested key is not found at the top level",
                Json.Field("{\"inner\":{\"status\":\"Malformed\"}}", "status") == null);

            Console.WriteLine();
            Console.WriteLine("-- Json.Quote: the one thing this SDK writes --");

            Check("a quote in a destination is escaped, not concatenated",
                Json.Quote("a\"b") == "\"a\\\"b\"");
            Check("a control character is escaped as \\u00XX",
                Json.Quote("a\u0001b") == "\"a\\u0001b\"");

            Console.WriteLine();
            if (_ran != Expected)
            {
                Console.Error.WriteLine(
                    $"checks: ran {_ran} assertions, expected {Expected}. A suite that "
                    + "quietly stops running half of itself is worse than no suite — "
                    + "update Expected deliberately.");
                return 1;
            }
            if (_failed != 0)
            {
                Console.Error.WriteLine($"checks: {_failed} of {_ran} FAILED");
                return 1;
            }
            Console.WriteLine($"checks: {_ran}/{Expected} OK");
            return 0;
        }
    }
}
