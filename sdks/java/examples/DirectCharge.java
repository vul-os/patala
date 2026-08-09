import java.nio.file.Files;
import java.nio.file.Path;
import org.vulos.patala.Json;
import org.vulos.patala.PatalaDirect;
import org.vulos.patala.PatalaException;

/**
 * patala in this JVM, through the C ABI — a full charge -> verify round trip
 * against the offline {@code MockRail}.
 *
 * <p><b>MockRail, deliberately.</b> patala is a payments library and an example
 * that moves real value is not an example. Everything below is deterministic,
 * offline, and needs no credential: no socket is opened by any line of it.
 *
 * <pre>
 *   sdks/java/run-examples.sh direct
 * </pre>
 *
 * <p>Requires Java 22+ and {@code --enable-native-access=ALL-UNNAMED}.
 */
public final class DirectCharge {

    /** A well-formed mock address. The format is {@code <network>:<kind>:<label>}. */
    private static final String ALICE = "mock:wallet:alice";

    public static void main(String[] args) throws Exception {
        Path library = PatalaDirect.findLibrary();
        System.out.println("library: " + library);
        System.out.println("         " + Files.size(library) + " bytes");

        // ---------------------------------------------------------- the rail
        //
        // Creating a rail talks to nothing: no socket, no thread, no
        // environment variable. The whole example runs offline because there
        // is no code path by which a MockRail could reach a network.
        try (PatalaDirect rail = PatalaDirect.open("{\"rail\":\"mock\",\"fee_minor\":25}")) {

            System.out.println("abi version: " + rail.abiVersion());
            // Not a string comparison here: the library compares, so the check
            // is not reimplemented — and forgotten — in each binding.
            rail.abiCheck();
            System.out.println("abi check against " + PatalaDirect.VERSION + ": ok");

            System.out.println("id:           " + rail.id());
            System.out.println("capabilities: " + rail.capabilities());

            // ------------------------------------------- destination pre-flight
            //
            // Offline, and it runs BEFORE any money moves. It never fails:
            // "I cannot check this" is the verdict "Unknown", not an error,
            // because a caller must handle it as carefully as a refusal.
            System.out.println();
            System.out.println("-- destination pre-flight --");
            for (String candidate : new String[] {ALICE, "eth:wallet:alice", ""}) {
                String verdict = rail.validateDestination(candidate);
                System.out.println("  " + quoted(candidate) + " -> "
                        + Json.field(verdict, "status") + ", is_refusal=" + Json.field(verdict, "is_refusal")
                        + ", human_must_confirm=" + Json.field(verdict, "human_must_confirm"));
            }
            System.out.println("  every verdict — including StructurallyValid — carries");
            System.out.println("  human_must_confirm=true. patala does not detect exchange-owned");
            System.out.println("  addresses and will not guess.");

            // ------------------------------------------------------ the money
            String payRequest = "{\"amount_minor\":1250,"
                    + "\"currency\":\"USDC\","
                    + "\"destination\":" + Json.quote(ALICE) + ","
                    + "\"reference\":\"order-4711\"}";

            System.out.println();
            System.out.println("-- quote -> charge -> verify --");
            System.out.println("  request: " + payRequest);
            System.out.println("  quote:   " + rail.quote(payRequest));

            String receipt = rail.charge(payRequest);
            System.out.println("  receipt: " + receipt);

            // THIS is the entitlement check. Not "charge returned without
            // throwing" — that only says the rail accepted the instruction.
            String verdict = rail.verify(receipt);
            System.out.println("  verify:  " + verdict);
            require("{\"valid\":true}".equals(verdict), "a fresh receipt must verify");

            // ------------------------------------------------- fails closed
            //
            // One digit changed in the amount. verify answering false is the
            // rail's honest verdict and arrives as an ordinary result, never
            // as an exception — do not retry it as though it were transient.
            String tampered = receipt.replace("\"amount_minor\":1250", "\"amount_minor\":125000");
            require(!tampered.equals(receipt), "the tamper did not apply");
            String tamperedVerdict = rail.verify(tampered);
            System.out.println("  verify(tampered amount): " + tamperedVerdict);
            require("{\"valid\":false}".equals(tamperedVerdict), "a tampered receipt must NOT verify");

            // ------------------------------------------------- honest refusals
            System.out.println();
            System.out.println("-- what this rail refuses to pretend --");
            System.out.println("  webhook:  " + failureOf(rail, "webhook",
                    "{\"body\":\"{}\",\"headers\":{},\"query\":{},\"now_unix\":1700000000}"));
            System.out.println("  providers: " + failureOf(rail, "providers", null));
            System.out.println("  unknown method: " + failureOf(rail, "settle-later", null));
        }

        // ------------------------------------------------------ use after close
        //
        // Handles are integers in a registry inside the library, and they are
        // retired rather than recycled — so a stale handle can never land on
        // somebody else's rail.
        PatalaDirect closed = PatalaDirect.open();
        closed.close();
        closed.close();  // idempotent
        try {
            closed.id();
            throw new IllegalStateException("a closed rail answered");
        } catch (PatalaException e) {
            System.out.println();
            System.out.println("after close: " + e.getMessage());
        }

        System.out.println();
        System.out.println("DirectCharge: OK — offline, no socket opened, no thread started.");
    }

    /** The library's own message for a call it refuses. */
    private static String failureOf(PatalaDirect rail, String method, String request) {
        try {
            return "UNEXPECTED SUCCESS: " + rail.call(method, request);
        } catch (PatalaException e) {
            return e.getMessage();
        }
    }

    private static void require(boolean cond, String what) {
        if (!cond) {
            throw new IllegalStateException("DirectCharge: FAILED — " + what);
        }
    }

    private static String quoted(String s) {
        return s.isEmpty() ? "\"\" (empty)" : "\"" + s + "\"";
    }

}
