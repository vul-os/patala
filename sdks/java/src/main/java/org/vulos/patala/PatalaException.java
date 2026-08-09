package org.vulos.patala;

/**
 * Thrown when patala cannot be located, started, or made to answer.
 *
 * <p>Both paths use this: the sidecar raises it when the binary is missing,
 * the server never became healthy, or a request came back with a status that
 * is not {@code 200}; the direct path raises it carrying {@code libpatala_ffi}'s
 * own error string, which is plain UTF-8 text and <b>not JSON</b> — the header
 * says so, so do not parse it.
 *
 * <h2>This is not the "payment failed" type</h2>
 *
 * Two answers from patala look like failures and are <b>not</b> exceptions:
 *
 * <ul>
 *   <li>{@code verify} returning {@code {"valid": false}} is the rail's
 *       honest, fail-closed verdict that a receipt does not hold. It arrives
 *       as an ordinary result. Gate entitlement on {@code true} and nothing
 *       else, and never retry a {@code false} as though it were transient.</li>
 *   <li>{@code validate-destination} returning
 *       {@code {"status":"Unknown"}} is a verdict, not an error, for exactly
 *       the same reason: a caller must handle "I cannot check this" as
 *       carefully as a refusal, and an exception is too easy to swallow.</li>
 * </ul>
 *
 * So catching this type does not mean "the money did not move". It means
 * patala could not answer at all.
 */
public class PatalaException extends RuntimeException {

    private static final long serialVersionUID = 1L;

    public PatalaException(String message) {
        super(message);
    }

    public PatalaException(String message, Throwable cause) {
        super(message, cause);
    }
}
