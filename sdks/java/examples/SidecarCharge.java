import java.nio.charset.StandardCharsets;
import java.util.LinkedHashMap;
import java.util.Map;
import org.vulos.patala.Json;
import org.vulos.patala.Patala;
import org.vulos.patala.PatalaException;

/**
 * patala as a child process on {@code 127.0.0.1} — the same charge -> verify
 * round trip as {@link DirectCharge}, over HTTP.
 *
 * <p><b>MockRail, deliberately</b>, and here it is not even a choice:
 * {@code patala-sidecar}'s default registry contains exactly one rail and it
 * is the offline mock. Nothing below opens a socket to anywhere but loopback.
 *
 * <pre>
 *   sdks/java/run-examples.sh sidecar
 * </pre>
 *
 * <p>Requires Java 11. No native library, no {@code --enable-native-access}.
 */
public final class SidecarCharge {

    private static final String ALICE = "mock:wallet:alice";

    public static void main(String[] args) throws Exception {
        Patala.Options opts = new Patala.Options();
        // Leaving opts.token null mints 32 fresh random bytes. The server
        // refuses to start without one; there is no unauthenticated mode.

        try (Patala patala = Patala.start(opts)) {
            System.out.println("sidecar:  " + patala.baseUrl() + " (loopback, hardcoded in the server)");
            System.out.println("healthz:  " + patala.healthz() + "   <- the one unauthenticated route");
            System.out.println("caps:     " + patala.capabilities(Patala.DEFAULT_RAIL));

            // ------------------------------------------- destination pre-flight
            System.out.println();
            System.out.println("-- destination pre-flight (POST /v1/rails/mock/validate-destination) --");
            for (String candidate : new String[] {ALICE, "eth:wallet:alice", ""}) {
                String verdict = patala.validateDestination(Patala.DEFAULT_RAIL, candidate);
                System.out.println("  " + (candidate.isEmpty() ? "\"\" (empty)" : "\"" + candidate + "\"")
                        + " -> " + Json.field(verdict, "status")
                        + ", is_refusal=" + Json.field(verdict, "is_refusal"));
            }
            System.out.println("  all five verdicts are HTTP 200. A 200 means the rail ANSWERED,");
            System.out.println("  not that the address is good — read the body, not the status.");

            // ------------------------------------------------------ the money
            String payRequest = "{\"amount_minor\":1250,"
                    + "\"currency\":\"USDC\","
                    + "\"destination\":\"" + ALICE + "\","
                    + "\"reference\":\"order-4711\"}";

            System.out.println();
            System.out.println("-- quote -> charge -> verify --");
            System.out.println("  quote:   " + patala.quote(Patala.DEFAULT_RAIL, payRequest));

            String receipt = patala.charge(Patala.DEFAULT_RAIL, payRequest);
            System.out.println("  receipt: " + receipt);

            String verdict = patala.verify(Patala.DEFAULT_RAIL, receipt);
            System.out.println("  verify:  " + verdict + "   <- 200; this, not the charge, is entitlement");
            require("{\"valid\":true}".equals(verdict), "a fresh receipt must verify");

            String tampered = receipt.replace("\"amount_minor\":1250", "\"amount_minor\":125000");
            require(!tampered.equals(receipt), "the tamper did not apply");
            String tamperedVerdict = patala.verify(Patala.DEFAULT_RAIL, tampered);
            System.out.println("  verify(tampered amount): " + tamperedVerdict
                    + "   <- also 200. false is data, not an HTTP error.");
            require("{\"valid\":false}".equals(tamperedVerdict), "a tampered receipt must NOT verify");

            // ---------------------------------------------------- the refusals
            System.out.println();
            System.out.println("-- what this server refuses to pretend --");

            Map<String, String> headers = new LinkedHashMap<>();
            headers.put("Stripe-Signature", "t=1,v1=deadbeef");
            System.out.println("  webhook (mock has no push delivery): "
                    + failure(() -> patala.webhook(Patala.DEFAULT_RAIL,
                            "{}".getBytes(StandardCharsets.UTF_8), headers)));

            System.out.println("  an unregistered rail: "
                    + failure(() -> patala.capabilities("stellar")));
            System.out.println("    ^ 404 because this process has never heard of it. The default");
            System.out.println("      registry is mock-only; per-rail registration is unwritten.");

            // The token gate, exercised rather than described. Same server,
            // same route, a token that is one character different.
            try (Patala wrong = Patala.attach(patala.baseUrl(), "0".repeat(64))) {
                System.out.println("  a wrong bearer token: " + failure(() -> wrong.capabilities("mock")));
            }
        }

        System.out.println();
        System.out.println("SidecarCharge: OK — child process stopped.");
    }

    private interface Call {
        String run();
    }

    private static String failure(Call c) {
        try {
            return "UNEXPECTED SUCCESS: " + c.run();
        } catch (PatalaException e) {
            return e.getMessage();
        }
    }

    private static void require(boolean cond, String what) {
        if (!cond) {
            throw new IllegalStateException("SidecarCharge: FAILED — " + what);
        }
    }

}
