using System;
using System.Text;

namespace Patala
{
    /// <summary>
    /// The only JSON this SDK writes, and a reader used strictly for printing.
    ///
    /// <para>Neither path parses JSON. Every document patala returns — a
    /// Quote, a Receipt, a DestinationVerdict — is handed to you as a
    /// <c>string</c> for <c>System.Text.Json</c> or whatever you already use,
    /// so this SDK has no serializer dependency and cannot disagree with yours
    /// about how a <c>u64</c> should be decoded. It must not become a
    /// <c>double</c>: amounts are integer minor units on both sides of the
    /// boundary.</para>
    ///
    /// <para>But the SDK does build one request object for you —
    /// <c>{"destination": …}</c> — and a destination arrives from a user.
    /// Concatenating that into JSON without escaping is how injection bugs get
    /// written, so the escaping lives here, in one place.</para>
    /// </summary>
    public static class Json
    {
        /// <summary>Quote and escape a string as a JSON scalar, quotes included.</summary>
        public static string Quote(string s)
        {
            var sb = new StringBuilder(s.Length + 2);
            sb.Append('"');
            foreach (char c in s)
            {
                switch (c)
                {
                    case '"': sb.Append("\\\""); break;
                    case '\\': sb.Append("\\\\"); break;
                    case '\n': sb.Append("\\n"); break;
                    case '\r': sb.Append("\\r"); break;
                    case '\t': sb.Append("\\t"); break;
                    default:
                        if (c < ' ')
                        {
                            sb.Append("\\u").Append(((int)c).ToString("x4"));
                        }
                        else
                        {
                            sb.Append(c);
                        }
                        break;
                }
            }
            sb.Append('"');
            return sb.ToString();
        }

        /// <summary>
        /// Build a PayRequest document.
        ///
        /// <para><paramref name="amountMinor"/> is an <b>integer number of
        /// minor units</b> — 1250 is USDC 0.01250, or ZAR 12.50 — and it is a
        /// <c>ulong</c> rather than a <c>decimal</c> or a <c>double</c> on
        /// purpose. patala never puts a float on either side of the boundary,
        /// and a convenience overload that took one would be where the
        /// rounding bug got in.</para>
        /// </summary>
        public static string PayRequest(
            ulong amountMinor, string currency, string destination, string reference) =>
            "{\"amount_minor\":" + amountMinor
            + ",\"currency\":" + Quote(currency)
            + ",\"destination\":" + Quote(destination)
            + ",\"reference\":" + Quote(reference) + "}";

        /// <summary>
        /// The value of a top-level scalar field, <b>for printing and
        /// assertions only</b>.
        ///
        /// <para>Not a parser and not a substitute for one: it does not
        /// understand nesting, so a key that also appears inside a nested
        /// object may be found there first. The one place the SDK itself uses
        /// it is <c>is_refusal</c>, a top-level boolean on a flat document.
        /// </para>
        /// </summary>
        /// <returns>the field's text, or null when the key is absent</returns>
        public static string? Field(string json, string key)
        {
            int at = json.IndexOf("\"" + key + "\":", StringComparison.Ordinal);
            if (at < 0)
            {
                return null;
            }
            int from = at + key.Length + 3;
            if (from < json.Length && json[from] == '"')
            {
                int end = from + 1;
                while (end < json.Length && json[end] != '"')
                {
                    end += json[end] == '\\' ? 2 : 1;
                }
                return json.Substring(from + 1, Math.Min(end, json.Length) - from - 1);
            }
            int to = from;
            while (to < json.Length && json[to] != ',' && json[to] != '}')
            {
                to++;
            }
            return json.Substring(from, to - from);
        }
    }

    /// <summary>
    /// Thrown when patala cannot be located, started, or made to answer.
    ///
    /// <para><b>This is not the "payment failed" type.</b> Two answers from
    /// patala look like failures and are not exceptions: <c>verify</c>
    /// returning <c>{"valid":false}</c>, which is the rail's fail-closed
    /// verdict, and <c>validate-destination</c> returning
    /// <c>{"status":"Unknown"}</c>, which means "I cannot check this". Both
    /// arrive as ordinary results, because a caller must handle them
    /// deliberately and an exception is too easy to swallow.</para>
    /// </summary>
    public class PatalaException : Exception
    {
        public PatalaException(string message)
            : base(message)
        {
        }

        public PatalaException(string message, Exception inner)
            : base(message, inner)
        {
        }
    }
}
