package org.vulos.patala;

import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.Linker;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.SymbolLookup;
import java.lang.foreign.ValueLayout;
import java.lang.invoke.MethodHandle;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;

/**
 * patala <b>in this JVM</b>, through the C ABI of {@code libpatala_ffi}.
 *
 * <p>{@link Patala} is the other path: it spawns {@code patala-sidecar} as a
 * child process and talks HTTP over loopback. Both are supported; which one to
 * default to is argued in {@code README.md}, and for patala on the JVM the
 * answer is <b>not</b> the one llmux and openrate give.
 *
 * <h2>Loading this library does not disturb the JVM</h2>
 *
 * patala's core is Rust and carries no language runtime, so
 * {@code libpatala_ffi} installs <b>no signal handlers</b>, starts
 * <b>no threads</b>, and leaves HotSpot's {@code SIGSEGV}, {@code SIGBUS},
 * {@code SIGFPE}, {@code SIGPIPE}, {@code SIGURG} and {@code SIGUSR2} exactly
 * as it found them. That is measured rather than asserted —
 * {@code sdks/java/signal-probe.sh} is the program that measures it, and
 * {@code README.md} carries its output next to llmux's, where five handlers
 * are replaced.
 *
 * <p>Each handle owns a <b>current-thread</b> Tokio runtime, so the async work
 * inside patala is driven on whichever thread called in. Calls on one handle
 * serialise; calls on different handles run concurrently.
 *
 * <h2>JSON in, JSON out</h2>
 *
 * This class deliberately parses nothing. Every document it returns is the
 * same JSON {@code patala-sidecar} serves, built from the same
 * {@code patala-core} types, and it is handed to you as a {@link String} for
 * the parser you already have. Amounts are <b>integer minor units</b> on both
 * sides of the boundary — never a float. Parse {@code amount_minor} as a
 * {@code long}, not as a {@code double}.
 *
 * <pre>{@code
 * try (PatalaDirect rail = PatalaDirect.open("{\"rail\":\"mock\"}")) {
 *     String req     = "{\"amount_minor\":1250,\"currency\":\"USDC\","
 *                    + "\"destination\":\"mock:wallet:alice\",\"reference\":\"order-4711\"}";
 *     String receipt = rail.call("charge", req);
 *     String verdict = rail.call("verify", receipt);   // {"valid":true}
 * }
 * }</pre>
 *
 * <h2>No streaming</h2>
 *
 * There is no {@code patala_stream} and this class has no streaming method.
 * patala has no incremental operation: a quote, a charge, a verification and a
 * destination check are each one question with one answer. llmux, which shares
 * this ABI's shape, does define {@code llmux_stream}. The omission here is
 * deliberate, not missing work.
 *
 * <h2>Requirements</h2>
 * <ul>
 *   <li><b>Java 22+</b> — {@code java.lang.foreign} became permanent in 22.
 *       Tested on OpenJDK 26.0.2, darwin/arm64.</li>
 *   <li>{@code --enable-native-access=ALL-UNNAMED} on the java command line.</li>
 *   <li>A {@code libpatala_ffi} for your platform. See
 *       {@link #findLibrary()} and the README's platform table — the list is
 *       short and there is no Windows DLL.</li>
 * </ul>
 */
public final class PatalaDirect implements AutoCloseable {

    /** The patala version this SDK was written against; see {@link #abiCheck()}. */
    public static final String VERSION = "0.1.0";

    /**
     * Every method {@code patala_call} accepts, in the order the header lists
     * them. Exposed so a caller can fail early on a typo rather than at the
     * boundary, and so a test can assert this list has not drifted from
     * {@code patala-ffi/include/patala.h}.
     */
    public static final List<String> METHODS = List.of(
            "id", "capabilities", "quote", "charge", "verify",
            "validate-destination", "webhook", "caveat", "providers");

    // ---------------------------------------------------------------- binding

    /**
     * The six exported symbols, bound once per library file.
     *
     * <p>Six, and no more — {@code patala.h} is a small surface on purpose:
     * one method dispatcher rather than one C function per operation, so the
     * header stays stable as patala grows methods.
     */
    private static final class Native {
        final MethodHandle abiVersion;  // const char* (void)
        final MethodHandle abiCheck;    // int (const char*, char**)
        final MethodHandle newRail;     // uint64_t (const char*, char**)
        final MethodHandle closeRail;   // void (uint64_t)
        final MethodHandle call;        // char* (uint64_t, const char*, const char*, char**)
        final MethodHandle free;        // void (char*)
        final String version;

        Native(Path library) {
            Linker linker = Linker.nativeLinker();
            SymbolLookup lookup;
            try {
                // A global arena, so the library is never unloaded. Nothing in
                // patala requires that — there is no runtime to leave behind —
                // but a downcall handle bound to a closed arena is a crash, and
                // there is no reason to hand anyone that footgun.
                lookup = SymbolLookup.libraryLookup(library, Arena.global());
            } catch (IllegalArgumentException e) {
                throw new PatalaException("could not load " + library + ": " + e.getMessage(), e);
            }
            abiVersion = down(linker, lookup, "patala_abi_version",
                    FunctionDescriptor.of(ValueLayout.ADDRESS));
            abiCheck = down(linker, lookup, "patala_abi_check",
                    FunctionDescriptor.of(ValueLayout.JAVA_INT, ValueLayout.ADDRESS, ValueLayout.ADDRESS));
            newRail = down(linker, lookup, "patala_new",
                    FunctionDescriptor.of(ValueLayout.JAVA_LONG, ValueLayout.ADDRESS, ValueLayout.ADDRESS));
            closeRail = down(linker, lookup, "patala_close",
                    FunctionDescriptor.ofVoid(ValueLayout.JAVA_LONG));
            call = down(linker, lookup, "patala_call",
                    FunctionDescriptor.of(ValueLayout.ADDRESS, ValueLayout.JAVA_LONG,
                            ValueLayout.ADDRESS, ValueLayout.ADDRESS, ValueLayout.ADDRESS));
            free = down(linker, lookup, "patala_free",
                    FunctionDescriptor.ofVoid(ValueLayout.ADDRESS));
            try {
                MemorySegment v = (MemorySegment) abiVersion.invokeExact();
                // Static, owned by the library. The header says: do NOT free it.
                version = v.reinterpret(Long.MAX_VALUE).getString(0);
            } catch (Throwable t) {
                throw new PatalaException("patala_abi_version failed", t);
            }
        }

        private static MethodHandle down(Linker linker, SymbolLookup lookup, String name,
                                         FunctionDescriptor desc) {
            MemorySegment sym = lookup.find(name).orElseThrow(() -> new PatalaException(
                    "libpatala_ffi does not export " + name
                            + " — the library on the load path is not a libpatala_ffi, or is too old"));
            return linker.downcallHandle(sym, desc);
        }
    }

    private static final Map<Path, Native> LOADED = new ConcurrentHashMap<>();

    private static Native nativeFor(Path library) {
        return LOADED.computeIfAbsent(library.toAbsolutePath().normalize(), Native::new);
    }

    // ------------------------------------------------------------------ state

    private final Native lib;
    private final Path libraryPath;
    private volatile long handle;

    private PatalaDirect(Native lib, Path libraryPath, long handle) {
        this.lib = lib;
        this.libraryPath = libraryPath;
        this.handle = handle;
    }

    // ---------------------------------------------------------------- opening

    /**
     * Build a rail and take a handle on it.
     *
     * <p><b>Creating a rail talks to nothing</b>: no socket is opened, no
     * thread is started, nothing is read from the environment. Only
     * {@link #call} reaches a network, and only for a rail that has one.
     *
     * @param configJson a JSON object tagged by {@code "rail"}, or null for
     *                   the offline default — a deterministic {@code MockRail}
     *                   on USDC, which needs no credentials and no network.
     *                   Unknown fields are <b>refused</b>, so a misspelled
     *                   {@code "currencys"} is an error rather than a rail
     *                   quietly built with a currency list you did not choose.
     */
    public static PatalaDirect open(String configJson) {
        return open(findLibrary(), configJson);
    }

    /** {@code open(null)} — the offline mock rail. */
    public static PatalaDirect open() {
        return open((String) null);
    }

    /** Open a rail from an explicit library path. */
    public static PatalaDirect open(Path library, String configJson) {
        Native lib = nativeFor(library);
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment cfg = configJson == null ? MemorySegment.NULL : arena.allocateFrom(configJson);
            MemorySegment errBox = arena.allocate(ValueLayout.ADDRESS);
            errBox.set(ValueLayout.ADDRESS, 0, MemorySegment.NULL);
            long h;
            try {
                h = (long) lib.newRail.invokeExact(cfg, errBox);
            } catch (Throwable t) {
                throw new PatalaException("patala_new failed", t);
            }
            // 0 is FAILURE here, not success: patala_new's success value is a
            // handle, and handles start at 1.
            if (h == 0) {
                throw new PatalaException("patala_new: " + takeError(lib, errBox));
            }
            drainError(lib, errBox);
            return new PatalaDirect(lib, library, h);
        }
    }

    // ---------------------------------------------------------------- calling

    /**
     * Run one method against this rail. See {@link #METHODS} for the set.
     *
     * <p>{@code requestJson} may be null for the methods that ignore it
     * ({@code id}, {@code capabilities}, {@code caveat}, {@code providers}).
     *
     * @return the response JSON — the same JSON the sidecar's matching
     *         endpoint returns
     * @throws PatalaException carrying the library's own message. Note that
     *         {@code verify} answering {@code {"valid":false}} and
     *         {@code validate-destination} answering {@code "Unknown"} are
     *         <b>results</b>, not exceptions — see {@link PatalaException}.
     */
    public String call(String method, String requestJson) {
        long h = requireOpen();
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment m = arena.allocateFrom(method);
            MemorySegment req = requestJson == null ? MemorySegment.NULL : arena.allocateFrom(requestJson);
            MemorySegment errBox = arena.allocate(ValueLayout.ADDRESS);
            errBox.set(ValueLayout.ADDRESS, 0, MemorySegment.NULL);

            MemorySegment out;
            try {
                out = (MemorySegment) lib.call.invokeExact(h, m, req, errBox);
            } catch (Throwable t) {
                throw new PatalaException("patala_call failed", t);
            }
            if (out.equals(MemorySegment.NULL)) {
                throw new PatalaException("patala_call(" + method + "): " + takeError(lib, errBox));
            }
            try {
                return out.reinterpret(Long.MAX_VALUE).getString(0);
            } finally {
                // Copied into a java.lang.String; the Rust allocation goes back
                // to the only allocator that can take it. NOT free(3).
                freeNative(lib, out);
                drainError(lib, errBox);
            }
        }
    }

    /** {@code call(method, null)}. */
    public String call(String method) {
        return call(method, null);
    }

    /** {@code call("id")} — {@code {"rail_id":"mock"}}. */
    public String id() {
        return call("id");
    }

    /**
     * {@code call("capabilities")} — how to decide your whole UX without
     * knowing which provider answered. A {@code "CustodialReversible"} rail
     * means a card form and a refundable pending state; a
     * {@code "NonCustodialFinal"} rail means a wallet address and a signed
     * final receipt. It is not a bool because those are not two shades of one
     * thing.
     */
    public String capabilities() {
        return call("capabilities");
    }

    /** {@code call("quote", payRequestJson)}. */
    public String quote(String payRequestJson) {
        return call("quote", payRequestJson);
    }

    /**
     * {@code call("charge", payRequestJson)} — returns a Receipt.
     *
     * <p>Store it. Handing it back to {@link #verify} later, and getting
     * {@code {"valid":true}}, is the entitlement check — <b>not</b> this call
     * having returned without throwing.
     */
    public String charge(String payRequestJson) {
        return call("charge", payRequestJson);
    }

    /** {@code call("verify", receiptJson)} — {@code {"valid":true|false}}. */
    public String verify(String receiptJson) {
        return call("verify", receiptJson);
    }

    /**
     * {@code call("validate-destination", {"destination":…})} — the offline
     * pre-flight check to run <b>before</b> any money moves.
     *
     * <p>It never fails: "I cannot check this" comes back as the verdict
     * {@code {"status":"Unknown"}}. Read {@code is_refusal} (do not send) and
     * {@code human_must_confirm}, which is {@code true} on <b>every</b>
     * verdict including {@code "StructurallyValid"} — patala does not detect
     * exchange-owned addresses and will not guess.
     */
    public String validateDestination(String destination) {
        return call("validate-destination", "{\"destination\":" + Json.quote(destination) + "}");
    }

    /**
     * {@code call("caveat")} — the sentence to show the human who is being
     * asked for a payout address, before there is a verdict to render.
     */
    public String caveat() {
        return call("caveat");
    }

    // ---------------------------------------------------------------- closing

    /**
     * Release this rail. Idempotent, so error paths can close blindly.
     *
     * <p>Handle numbers are <b>retired, not recycled</b>, so a call after
     * close is a clean error naming the dead handle rather than a live rail
     * belonging to someone else.
     */
    @Override
    public void close() {
        long h;
        synchronized (this) {
            h = handle;
            handle = 0;
        }
        if (h == 0) {
            return;
        }
        try {
            lib.closeRail.invokeExact(h);
        } catch (Throwable t) {
            throw new PatalaException("patala_close failed", t);
        }
    }

    // ------------------------------------------------------------ diagnostics

    /** The patala version the loaded shared library was built from. */
    public String abiVersion() {
        return lib.version;
    }

    /** The library this handle lives in. */
    public Path libraryPath() {
        return libraryPath;
    }

    /** True until {@link #close()}. */
    public boolean isOpen() {
        return handle != 0;
    }

    /**
     * Ask the library to compare its own version against {@link #VERSION},
     * through {@code patala_abi_check} rather than by comparing strings here.
     *
     * <p>The header's reasoning, and it is worth repeating: a shared library
     * is resolved off a load path you may not control, so without this probe a
     * stale {@code libpatala_ffi} earlier on that path is called silently and
     * misbehaves in ways that look like patala bugs.
     *
     * @throws PatalaException naming both versions when they differ
     */
    public void abiCheck() {
        abiCheck(VERSION);
    }

    /** {@link #abiCheck()} against an explicit expected version. */
    public void abiCheck(String expected) {
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment want = arena.allocateFrom(expected);
            MemorySegment errBox = arena.allocate(ValueLayout.ADDRESS);
            errBox.set(ValueLayout.ADDRESS, 0, MemorySegment.NULL);
            int rc;
            try {
                rc = (int) lib.abiCheck.invokeExact(want, errBox);
            } catch (Throwable t) {
                throw new PatalaException("patala_abi_check failed", t);
            }
            if (rc != 0) {
                throw new PatalaException("patala_abi_check: " + takeError(lib, errBox));
            }
            drainError(lib, errBox);
        }
    }

    // ---------------------------------------------------------------- helpers

    private long requireOpen() {
        long h = handle;
        if (h == 0) {
            throw new PatalaException("this rail is closed");
        }
        return h;
    }

    private static String takeError(Native lib, MemorySegment errBox) {
        MemorySegment p = errBox.get(ValueLayout.ADDRESS, 0);
        if (p.equals(MemorySegment.NULL)) {
            return "the library reported a failure but set no message";
        }
        String msg = p.reinterpret(Long.MAX_VALUE).getString(0);
        errBox.set(ValueLayout.ADDRESS, 0, MemorySegment.NULL);
        freeNative(lib, p);
        return msg;
    }

    /** Drain the error out-parameter on the SUCCESS path too, so it cannot leak. */
    private static void drainError(Native lib, MemorySegment errBox) {
        MemorySegment p = errBox.get(ValueLayout.ADDRESS, 0);
        if (!p.equals(MemorySegment.NULL)) {
            errBox.set(ValueLayout.ADDRESS, 0, MemorySegment.NULL);
            freeNative(lib, p);
        }
    }

    private static void freeNative(Native lib, MemorySegment p) {
        try {
            lib.free.invokeExact(p);
        } catch (Throwable t) {
            throw new PatalaException("patala_free failed", t);
        }
    }

    // ----------------------------------------------------------------- lookup

    /**
     * Locate {@code libpatala_ffi}, in order:
     * <ol>
     *   <li>{@code $PATALA_LIBRARY} — an explicit path</li>
     *   <li>{@code $PATALA_HOME/target/release/}, then {@code target/debug/}</li>
     *   <li>{@code target/release/} then {@code target/debug/}, walking up
     *       from the working directory — the layout {@code cargo build} writes</li>
     * </ol>
     *
     * <p>Unlike llmux's and openrate's Go libraries the file name carries no
     * target triple: cargo writes {@code libpatala_ffi.dylib} /
     * {@code .so} / {@code patala_ffi.dll}, one name per platform.
     *
     * @throws PatalaException naming every path tried
     */
    public static Path findLibrary() {
        String explicit = System.getenv("PATALA_LIBRARY");
        if (explicit != null && !explicit.isEmpty()) {
            Path p = Paths.get(explicit);
            if (!Files.isRegularFile(p)) {
                throw new PatalaException("PATALA_LIBRARY is set to " + p + ", which is not a file");
            }
            return p;
        }

        String file = libraryFileName();
        List<Path> tried = new ArrayList<>();

        String home = System.getenv("PATALA_HOME");
        if (home != null && !home.isEmpty()) {
            for (String profile : new String[] {"release", "debug"}) {
                Path p = Paths.get(home, "target", profile, file);
                if (Files.isRegularFile(p)) {
                    return p;
                }
                tried.add(p);
            }
        }

        for (Path at = Paths.get("").toAbsolutePath(); at != null; at = at.getParent()) {
            for (String profile : new String[] {"release", "debug"}) {
                Path p = at.resolve("target").resolve(profile).resolve(file);
                if (Files.isRegularFile(p)) {
                    return p;
                }
                tried.add(p);
            }
        }

        StringBuilder msg = new StringBuilder("no " + file + " found. Tried:");
        for (Path p : tried) {
            msg.append("\n  ").append(p);
        }
        msg.append("\nBuild one with `cargo build -p patala-ffi --release` in the patala")
           .append(" checkout, or set PATALA_LIBRARY to an existing library.")
           .append("\nThe only library built and executed so far is darwin/arm64.")
           .append(" There is no Windows DLL.");
        throw new PatalaException(msg.toString());
    }

    /** {@code libpatala_ffi.dylib} / {@code libpatala_ffi.so} / {@code patala_ffi.dll}. */
    public static String libraryFileName() {
        String os = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
        if (os.contains("mac") || os.contains("darwin")) {
            return "libpatala_ffi.dylib";
        }
        if (os.contains("win")) {
            return "patala_ffi.dll";
        }
        return "libpatala_ffi.so";
    }
}
