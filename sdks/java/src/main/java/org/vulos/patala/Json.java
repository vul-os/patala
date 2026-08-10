package org.vulos.patala;

/**
 * The only JSON this SDK writes.
 *
 * <p>Neither path parses JSON. Every document patala returns — a Quote, a
 * Receipt, a DestinationVerdict — is handed to you as a {@link String} for the
 * parser you already have, so this SDK has no JSON dependency and cannot
 * disagree with yours about how a {@code u64} should be decoded. (It must not
 * become a {@code double}. Amounts are integer minor units on both sides of
 * the boundary.)
 *
 * <p>But it does build one request object for you —
 * {@code {"destination": …}} — and a destination arrives from a user.
 * Concatenating that into JSON without escaping is how injection bugs get
 * written, so the escaping lives here, in one place, targeting Java 11 so that
 * both {@link Patala} (11) and {@link PatalaDirect} (22) can use it.
 */
public final class Json {

    private Json() {
    }

    /** Quote and escape a string as a JSON scalar, surrounding quotes included. */
    public static String quote(String s) {
        StringBuilder sb = new StringBuilder(s.length() + 2);
        sb.append('"');
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            switch (c) {
                case '"': sb.append("\\\""); break;
                case '\\': sb.append("\\\\"); break;
                case '\n': sb.append("\\n"); break;
                case '\r': sb.append("\\r"); break;
                case '\t': sb.append("\\t"); break;
                default:
                    if (c < ' ') {
                        sb.append(String.format("\\u%04x", (int) c));
                    } else {
                        sb.append(c);
                    }
            }
        }
        sb.append('"');
        return sb.toString();
    }

    /**
     * The value of a scalar field, <b>for printing, and only for printing</b>.
     *
     * <p>Not a parser and not a substitute for one: it does not understand
     * nesting, so a key that also appears inside a nested object may be found
     * there first. It exists because the examples print three fields out of a
     * verdict, and pulling Jackson in to do that would make the examples a
     * demonstration of Jackson.
     *
     * <p><b>Never branch on what this returns.</b> The Kotlin sidecar client
     * had `isRefusal(json) = field(json, "is_refusal") == "true"` over it, and
     * because this method did not skip the whitespace after the colon, a
     * verdict reformatted anywhere between patala and the caller yielded
     * {@code " true"} — so a {@code Malformed} verdict read as "not a
     * refusal". That helper is gone; recovering a boolean is the job of the
     * parser you already have, defaulting to refusal. The whitespace is now
     * skipped anyway, because a printer that prints the wrong thing is still
     * wrong.
     *
     * @return the field's text, or {@code null} when the key is absent
     */
    public static String field(String json, String key) {
        int at = json.indexOf("\"" + key + "\"");
        if (at < 0) {
            return null;
        }
        int from = at + key.length() + 2;
        // Whitespace before the colon, the colon, then whitespace after it.
        // JSON permits all three; `"is_refusal" : true` is the same document
        // as `"is_refusal":true` and must print the same.
        while (from < json.length() && Character.isWhitespace(json.charAt(from))) {
            from++;
        }
        if (from >= json.length() || json.charAt(from) != ':') {
            return null;
        }
        from++;
        while (from < json.length() && Character.isWhitespace(json.charAt(from))) {
            from++;
        }
        if (from < json.length() && json.charAt(from) == '"') {
            int to = from + 1;
            while (to < json.length() && json.charAt(to) != '"') {
                to += json.charAt(to) == '\\' ? 2 : 1;
            }
            return json.substring(from + 1, Math.min(to, json.length()));
        }
        int to = from;
        while (to < json.length()
                && ",}]".indexOf(json.charAt(to)) < 0
                && !Character.isWhitespace(json.charAt(to))) {
            to++;
        }
        return json.substring(from, to);
    }
}
