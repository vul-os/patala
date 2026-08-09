import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.Linker;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.SymbolLookup;
import java.lang.foreign.ValueLayout;
import java.lang.invoke.MethodHandle;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.LinkedHashMap;
import java.util.Map;

/**
 * What loading {@code libpatala_ffi} does to a JVM — measured, not asserted.
 *
 * <p>patala's README claims something llmux's and openrate's cannot: that
 * loading the shared library leaves the host process alone. No signal
 * handlers, no threads, no runtime. This is the program that establishes it,
 * and it is a near-copy of llmux's {@code SignalHandlerProbe} on purpose — the
 * same measurement, run against a Rust library instead of a Go one, so the two
 * numbers are comparable rather than merely both quoted.
 *
 * <p>It differs from llmux's in two ways, both because patala can afford them:
 *
 * <ul>
 *   <li>it counts the <b>process's OS threads</b> across the load and across a
 *       full charge -> verify round trip, which is the same evidence
 *       {@code patala-ffi/ctest/smoke.c} produces from C;</li>
 *   <li>it re-reads the handlers <b>after real work</b>, not only after
 *       {@code dlopen}. patala's handles own a lazily-created current-thread
 *       Tokio runtime, so "nothing at load time" would be a weaker claim than
 *       "nothing, ever" if the probe stopped at the load.</li>
 * </ul>
 *
 * <pre>
 *   sdks/java/signal-probe.sh                 # this repo's library
 *   sdks/java/signal-probe.sh --checkjni      # HotSpot's own audit
 *   sdks/java/signal-probe.sh --jsig          # again, with libjsig preloaded
 * </pre>
 *
 * <p>A probe that prints "no change" is as useful as one that prints a diff:
 * the point is that the README's claim is this program's output rather than a
 * recollection of how Rust behaves.
 *
 * <p>Requires Java 22+ ({@code java.lang.foreign}) and
 * {@code --enable-native-access=ALL-UNNAMED}.
 */
public final class SignalHandlerProbe {

    /**
     * Signal numbers. These are the BSD/darwin numbers; Linux renumbers
     * several of them, so both tables are here and the host picks one.
     */
    private static final Map<String, Integer> DARWIN = new LinkedHashMap<>();
    private static final Map<String, Integer> LINUX = new LinkedHashMap<>();
    static {
        DARWIN.put("SIGILL", 4);   LINUX.put("SIGILL", 4);
        DARWIN.put("SIGTRAP", 5);  LINUX.put("SIGTRAP", 5);
        DARWIN.put("SIGABRT", 6);  LINUX.put("SIGABRT", 6);
        DARWIN.put("SIGFPE", 8);   LINUX.put("SIGFPE", 8);
        DARWIN.put("SIGBUS", 10);  LINUX.put("SIGBUS", 7);
        DARWIN.put("SIGSEGV", 11); LINUX.put("SIGSEGV", 11);
        DARWIN.put("SIGPIPE", 13); LINUX.put("SIGPIPE", 13);
        DARWIN.put("SIGURG", 16);  LINUX.put("SIGURG", 23);
        DARWIN.put("SIGXCPU", 24); LINUX.put("SIGXCPU", 24);
        DARWIN.put("SIGXFSZ", 25); LINUX.put("SIGXFSZ", 25);
        DARWIN.put("SIGPROF", 27); LINUX.put("SIGPROF", 27);
        DARWIN.put("SIGUSR1", 30); LINUX.put("SIGUSR1", 10);
        DARWIN.put("SIGUSR2", 31); LINUX.put("SIGUSR2", 12);
    }

    private static final int SA_ONSTACK = 0x0001;

    /**
     * struct sigaction layout. Both platforms put the handler pointer first;
     * only the offset and width of sa_flags differ.
     *   darwin: handler(8) sa_mask(4) sa_flags(4)                 = 16 bytes
     *   linux:  handler(8) sa_flags(8) restorer(8) sa_mask(128)   = 152 bytes
     */
    private static final boolean DARWIN_HOST =
            System.getProperty("os.name", "").toLowerCase().contains("mac");
    private static final long SA_SIZE = DARWIN_HOST ? 16 : 152;
    private static final long FLAGS_OFFSET = DARWIN_HOST ? 12 : 8;
    private static final long FLAGS_WIDTH = DARWIN_HOST ? 4 : 8;

    private static MethodHandle sigaction;

    public static void main(String[] args) throws Throwable {
        if (args.length < 1) {
            System.err.println("usage: SignalHandlerProbe <library>");
            System.exit(2);
        }
        Path library = Path.of(args[0]);

        Map<String, Integer> signals = DARWIN_HOST ? DARWIN : LINUX;
        System.out.println("host: " + System.getProperty("os.name")
                + " " + System.getProperty("os.arch")
                + " | jvm: " + System.getProperty("java.vm.name")
                + " " + System.getProperty("java.version"));
        System.out.println("library: " + library + " (" + Files.size(library) + " bytes)");
        boolean jsig = jsigLoaded();
        System.out.println("libjsig preloaded: " + jsig);
        if (jsig) {
            System.out.println();
            System.out.println("CAVEAT: libjsig interposes sigaction(), including THIS PROGRAM'S calls");
            System.out.println("  to it, so the address columns below are what libjsig reports rather");
            System.out.println("  than necessarily what is installed. Under --jsig the authority is");
            System.out.println("  HotSpot's own audit: re-run with --checkjni.");
        }
        System.out.println();

        Linker linker = Linker.nativeLinker();
        sigaction = linker.downcallHandle(
                linker.defaultLookup().find("sigaction").orElseThrow(),
                FunctionDescriptor.of(ValueLayout.JAVA_INT,
                        ValueLayout.JAVA_INT, ValueLayout.ADDRESS, ValueLayout.ADDRESS));

        Map<String, long[]> before = snapshot(signals);
        int threadsBefore = threadCount(linker);

        // ------------------------------------------------------------- load
        SymbolLookup lookup = SymbolLookup.libraryLookup(library, Arena.global());
        MethodHandle abiVersion = linker.downcallHandle(
                lookup.find("patala_abi_version").orElseThrow(
                        () -> new IllegalStateException("no symbol patala_abi_version")),
                FunctionDescriptor.of(ValueLayout.ADDRESS));
        MemorySegment v = (MemorySegment) abiVersion.invokeExact();
        System.out.println("loaded; abi version = " + v.reinterpret(Long.MAX_VALUE).getString(0));

        Map<String, long[]> afterLoad = snapshot(signals);
        int threadsAfterLoad = threadCount(linker);

        // -------------------------------------------------------- real work
        //
        // A whole charge -> verify round trip on the offline MockRail, so the
        // verdict below covers patala actually running rather than only
        // patala being present. This is where the lazily-created current-
        // thread Tokio runtime comes into existence.
        String receiptSummary = roundTrip(linker, lookup);
        System.out.println("round trip: " + receiptSummary);
        System.out.println();

        Map<String, long[]> afterWork = snapshot(signals);
        int threadsAfterWork = threadCount(linker);

        // ---------------------------------------------------------- verdict
        System.out.println("signal    before                after load            after round trip      verdict");
        System.out.println("---------------------------------------------------------------------------------------------------");
        int replaced = 0;
        int flagsOnly = 0;
        for (String name : signals.keySet()) {
            long[] b = before.get(name);
            long[] l = afterLoad.get(name);
            long[] w = afterWork.get(name);
            String verdict;
            if (b[0] != l[0] || b[0] != w[0]) {
                replaced++;
                verdict = "HANDLER REPLACED";
            } else if (b[1] != l[1] || b[1] != w[1]) {
                flagsOnly++;
                verdict = "flags changed"
                        + (((b[1] & SA_ONSTACK) == 0 && (w[1] & SA_ONSTACK) != 0)
                            ? " (SA_ONSTACK added)" : "");
            } else {
                verdict = "unchanged";
            }
            System.out.printf("%-9s %-21s %-21s %-21s %s%n", name, show(b), show(l), show(w), verdict);
        }

        System.out.println();
        System.out.println(replaced + " handler(s) replaced, " + flagsOnly
                + " left in place with altered flags");
        System.out.println();

        System.out.println("threads in this process (the same measurement patala-ffi/ctest/smoke.c makes):");
        System.out.println("  before dlopen:        " + threadsBefore);
        System.out.println("  after dlopen:         " + threadsAfterLoad);
        System.out.println("  after a round trip:   " + threadsAfterWork);
        if (threadsBefore < 0) {
            System.out.println("  (this platform has no thread count implementation here)");
        }
        System.out.println();

        System.out.println("does the JVM still work through the handlers that could have changed?");
        System.out.println("  implicit null checks (SIGSEGV): " + implicitNullChecks() + " recovered");
        System.out.println("  stack banging (SIGSEGV/SIGBUS): " + stackOverflow());
        System.out.println("  arithmetic traps:               " + divideByZero());
        System.out.println();
        System.out.println("probe completed without terminating the VM.");
        System.out.println("Run the JVM with -Xcheck:jni to see HotSpot's own opinion of the above.");
    }

    /** A full MockRail charge -> verify through the C ABI. */
    private static String roundTrip(Linker linker, SymbolLookup lookup) throws Throwable {
        MethodHandle newRail = linker.downcallHandle(lookup.find("patala_new").orElseThrow(),
                FunctionDescriptor.of(ValueLayout.JAVA_LONG, ValueLayout.ADDRESS, ValueLayout.ADDRESS));
        MethodHandle call = linker.downcallHandle(lookup.find("patala_call").orElseThrow(),
                FunctionDescriptor.of(ValueLayout.ADDRESS, ValueLayout.JAVA_LONG,
                        ValueLayout.ADDRESS, ValueLayout.ADDRESS, ValueLayout.ADDRESS));
        MethodHandle closeRail = linker.downcallHandle(lookup.find("patala_close").orElseThrow(),
                FunctionDescriptor.ofVoid(ValueLayout.JAVA_LONG));
        MethodHandle free = linker.downcallHandle(lookup.find("patala_free").orElseThrow(),
                FunctionDescriptor.ofVoid(ValueLayout.ADDRESS));

        try (Arena arena = Arena.ofConfined()) {
            MemorySegment errBox = arena.allocate(ValueLayout.ADDRESS);
            errBox.set(ValueLayout.ADDRESS, 0, MemorySegment.NULL);
            long h = (long) newRail.invokeExact(arena.allocateFrom("{\"rail\":\"mock\"}"), errBox);
            if (h == 0) {
                return "patala_new FAILED";
            }
            String request = "{\"amount_minor\":1250,\"currency\":\"USDC\","
                    + "\"destination\":\"mock:wallet:alice\",\"reference\":\"signal-probe\"}";
            String receipt = str(call, free, arena, h, "charge", request, errBox);
            String verdict = str(call, free, arena, h, "verify", receipt, errBox);
            closeRail.invokeExact(h);
            return "charge -> verify " + verdict;
        }
    }

    private static String str(MethodHandle call, MethodHandle free, Arena arena, long h,
                              String method, String request, MemorySegment errBox) throws Throwable {
        MemorySegment out = (MemorySegment) call.invokeExact(h,
                arena.allocateFrom(method), arena.allocateFrom(request), errBox);
        if (out.equals(MemorySegment.NULL)) {
            MemorySegment e = errBox.get(ValueLayout.ADDRESS, 0);
            String msg = e.equals(MemorySegment.NULL)
                    ? "(no message)" : e.reinterpret(Long.MAX_VALUE).getString(0);
            throw new IllegalStateException("patala_call(" + method + ") failed: " + msg);
        }
        String s = out.reinterpret(Long.MAX_VALUE).getString(0);
        free.invokeExact(out);
        return s;
    }

    /**
     * OS threads in this process.
     *
     * <p>Not {@code ThreadMXBean.getThreadCount()}: that counts threads the
     * JVM knows about, and a native library's threads are exactly the ones it
     * would miss — which would make it useless for this question.
     *
     * <p>darwin: {@code task_threads} on {@code mach_task_self_}. linux:
     * {@code /proc/self/status}'s {@code Threads:} line. Anywhere else: -1,
     * reported as "no implementation" rather than as zero.
     */
    private static int threadCount(Linker linker) {
        try {
            if (DARWIN_HOST) {
                SymbolLookup libc = linker.defaultLookup();
                MemorySegment selfPort = libc.find("mach_task_self_").orElseThrow().reinterpret(4);
                int task = selfPort.get(ValueLayout.JAVA_INT, 0);
                MethodHandle taskThreads = linker.downcallHandle(
                        libc.find("task_threads").orElseThrow(),
                        FunctionDescriptor.of(ValueLayout.JAVA_INT, ValueLayout.JAVA_INT,
                                ValueLayout.ADDRESS, ValueLayout.ADDRESS));
                MethodHandle portDealloc = linker.downcallHandle(
                        libc.find("mach_port_deallocate").orElseThrow(),
                        FunctionDescriptor.of(ValueLayout.JAVA_INT, ValueLayout.JAVA_INT,
                                ValueLayout.JAVA_INT));
                MethodHandle vmDealloc = linker.downcallHandle(
                        libc.find("vm_deallocate").orElseThrow(),
                        FunctionDescriptor.of(ValueLayout.JAVA_INT, ValueLayout.JAVA_INT,
                                ValueLayout.JAVA_LONG, ValueLayout.JAVA_LONG));
                try (Arena arena = Arena.ofConfined()) {
                    MemorySegment listBox = arena.allocate(ValueLayout.ADDRESS);
                    MemorySegment countBox = arena.allocate(ValueLayout.JAVA_INT);
                    int rc = (int) taskThreads.invokeExact(task, listBox, countBox);
                    if (rc != 0) {
                        return -1;
                    }
                    int n = countBox.get(ValueLayout.JAVA_INT, 0);
                    // Every port name task_threads hands back is a reference
                    // this process now owns. Leaking them would make the probe
                    // itself the thing that changed the process.
                    MemorySegment list = listBox.get(ValueLayout.ADDRESS, 0)
                            .reinterpret((long) n * 4);
                    for (int i = 0; i < n; i++) {
                        int unusedRc = (int) portDealloc.invokeExact(
                                task, list.get(ValueLayout.JAVA_INT, (long) i * 4));
                    }
                    int unusedVm = (int) vmDealloc.invokeExact(
                            task, list.address(), (long) n * 4);
                    return n;
                }
            }
            for (String line : Files.readAllLines(Path.of("/proc/self/status"))) {
                if (line.startsWith("Threads:")) {
                    return Integer.parseInt(line.substring(8).trim());
                }
            }
            return -1;
        } catch (Throwable t) {
            return -1;
        }
    }

    private static boolean jsigLoaded() {
        String ins = System.getenv("DYLD_INSERT_LIBRARIES");
        if (ins == null) {
            ins = System.getenv("LD_PRELOAD");
        }
        return ins != null && ins.contains("jsig");
    }

    private static Map<String, long[]> snapshot(Map<String, Integer> signals) throws Throwable {
        Map<String, long[]> out = new LinkedHashMap<>();
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment old = arena.allocate(SA_SIZE);
            for (Map.Entry<String, Integer> e : signals.entrySet()) {
                old.fill((byte) 0);
                int signo = e.getValue();
                int rc = (int) sigaction.invokeExact(signo, MemorySegment.NULL, old);
                if (rc != 0) {
                    throw new IllegalStateException("sigaction(" + e.getKey() + ") failed");
                }
                long flags = FLAGS_WIDTH == 4
                        ? old.get(ValueLayout.JAVA_INT, FLAGS_OFFSET)
                        : old.get(ValueLayout.JAVA_LONG, FLAGS_OFFSET);
                out.put(e.getKey(), new long[] {old.get(ValueLayout.JAVA_LONG, 0), flags});
            }
        }
        return out;
    }

    private static String show(long[] h) {
        String addr = h[0] == 0 ? "SIG_DFL" : h[0] == 1 ? "SIG_IGN" : String.format("0x%x", h[0]);
        return addr + " f=0x" + Long.toHexString(h[1]);
    }

    /** HotSpot elides null checks and recovers them from SIGSEGV. */
    private static long implicitNullChecks() {
        String s = null;
        long n = 0;
        for (int i = 0; i < 2_000_000; i++) {
            try {
                n += s.length();
            } catch (NullPointerException e) {
                n++;
            }
            if (i == 1_000_000) {
                s = "x";   // force deopt/reopt churn
            }
            if (i == 1_500_000) {
                s = null;
            }
        }
        return n;
    }

    /** Guard-page faults are how HotSpot produces StackOverflowError. */
    private static String stackOverflow() {
        try {
            return "no StackOverflowError at depth " + recurse(0);
        } catch (StackOverflowError e) {
            return "StackOverflowError raised and caught";
        }
    }

    private static int recurse(int d) {
        return recurse(d + 1) + 1;
    }

    private static String divideByZero() {
        int zero = Integer.parseInt("0");
        try {
            return "no exception: " + (1 / zero);
        } catch (ArithmeticException e) {
            return "ArithmeticException raised and caught";
        }
    }
}
