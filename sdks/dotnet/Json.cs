using System;
using System.Text;
using System.Text.Json;

namespace Patala
{
    /// <summary>
    /// The only JSON this SDK writes, and the two readers it uses.
    ///
    /// <para>The SDK does not <b>deserialise</b> patala's documents. Every one
    /// it returns — a Quote, a Receipt, a DestinationVerdict — is handed to you
    /// as a <c>string</c> for <c>System.Text.Json</c> or whatever you already
    /// use, so this SDK cannot disagree with yours about how a <c>u64</c>
    /// should be decoded. It must not become a <c>double</c>: amounts are
    /// integer minor units on both sides of the boundary.</para>
    ///
    /// <para>It does build one request object for you —
    /// <c>{"destination": …}</c> — and a destination arrives from a user.
    /// Concatenating that into JSON without escaping is how injection bugs get
    /// written, so the escaping lives here, in one place.</para>
    ///
    /// <para><b>The two readers use a real parser, and used not to.</b>
    /// <see cref="Field"/> was a substring scan that did not skip whitespace
    /// after the colon, and <see cref="Patala.Sidecar.IsRefusal"/> was
    /// <c>Field(json, "is_refusal") == "true"</c> over it. A verdict
    /// re-serialised by <c>JsonSerializer</c> with <c>WriteIndented</c>, or by
    /// any proxy on the way, yields <c>" true"</c> — so <c>IsRefusal</c>
    /// returned <b>false for a Malformed verdict</b>, and the payout gate in
    /// this SDK's own README sent the money. Two lines away, <c>IsValid</c>
    /// used the same shape and failed CLOSED; the polarity was simply
    /// inverted on the one question in this API where failing open costs.
    /// <c>System.Text.Json</c> is in the <c>net8.0</c> shared framework, so
    /// using it here adds no package reference and no version to reconcile —
    /// it was never the reason not to parse.</para>
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
        /// The text of a <b>top-level</b> field, for printing.
        ///
        /// <para>A string comes back unquoted and unescaped; anything else
        /// comes back as its exact source text, so a <c>u64</c> keeps every
        /// digit it was sent with and never passes through a
        /// <c>double</c>.</para>
        ///
        /// <para>Top-level only, and null on any doubt: an unparseable
        /// document, a document that is not an object, or an absent key. The
        /// substring scan this replaced could match a key inside a nested
        /// object first, and did not skip whitespace after the colon.</para>
        /// </summary>
        /// <returns>the field's text, or null</returns>
        public static string? Field(string json, string key)
        {
            if (!TryGet(json, key, out JsonElement value))
            {
                return null;
            }
            return value.ValueKind == JsonValueKind.String
                ? value.GetString()
                : value.GetRawText();
        }

        /// <summary>
        /// A <b>top-level</b> JSON boolean, or <paramref name="fallback"/> on
        /// any doubt at all.
        ///
        /// <para>"Any doubt" is deliberately everything: a document that will
        /// not parse, one that is not an object, an absent key, and a value
        /// that is present but is not <c>true</c> or <c>false</c> — a string
        /// <c>"true"</c> included. The caller chooses which way that falls, and
        /// for <c>is_refusal</c> it must be <c>true</c>: a verdict this SDK
        /// cannot read is a verdict it has not been told to send against.</para>
        /// </summary>
        public static bool Flag(string json, string key, bool fallback)
        {
            if (!TryGet(json, key, out JsonElement value))
            {
                return fallback;
            }
            return value.ValueKind switch
            {
                JsonValueKind.True => true,
                JsonValueKind.False => false,
                _ => fallback,
            };
        }

        private static bool TryGet(string json, string key, out JsonElement value)
        {
            value = default;
            if (json == null)
            {
                return false;
            }
            try
            {
                using var doc = JsonDocument.Parse(json);
                if (doc.RootElement.ValueKind != JsonValueKind.Object
                    || !doc.RootElement.TryGetProperty(key, out JsonElement found))
                {
                    return false;
                }
                // Clone: the element borrows the document, which is disposed here.
                value = found.Clone();
                return true;
            }
            catch (JsonException)
            {
                return false;
            }
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
