package org.vulos.patala;

import java.io.File;
import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.security.SecureRandom;
import java.time.Duration;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.concurrent.TimeUnit;

/**
 * patala as a <b>child process</b> on {@code 127.0.0.1}, spoken to over HTTP.
 *
 * <pre>{@code
 * try (Patala patala = Patala.start(new Patala.Options())) {
 *     String receipt = patala.charge("mock", payRequestJson);
 *     String verdict = patala.verify("mock", receipt);   // {"valid":true}
 * }
 * }</pre>
 *
 * <p>Requires <b>Java 11</b>. No native library, no {@code --enable-native-access},
 * no platform matrix — it runs on Windows, where {@code libpatala_ffi} has never
 * been built.
 *
 * <h2>What the sidecar buys that the direct path cannot</h2>
 *
 * <b>Key isolation.</b> A non-custodial rail's signing key lives inside
 * whichever process calls {@code charge}. Link the direct path into five
 * services and that key is smeared across five processes' memory, so a bug or
 * a dependency-confusion attack in any one of them is a path to it. Route them
 * all through one sidecar and the key lives in exactly one narrow,
 * purpose-built process. See {@code patala-sidecar/README.md} for the full
 * threat model, including what it does <b>not</b> defend against: a
 * co-resident, same-privilege attacker can read the token out of this
 * process's environment.
 *
 * <h2>The token is mandatory and this class generates one</h2>
 *
 * {@code patala-sidecar} refuses to start without {@code PATALA_SIDECAR_TOKEN}
 * — there is no auto-generated fallback inside the server and no
 * "runs unauthenticated if you forget" path. {@link #start} therefore mints 32
 * random bytes from {@link SecureRandom}, passes them to the child, and sends
 * them as {@code Authorization: Bearer} on every {@code /v1} request. Set
 * {@link Options#token} to share one long-running sidecar between processes
 * instead.
 *
 * <h2>Startup waits for {@code /healthz} and that is the whole wait</h2>
 *
 * Unlike openrate's sidecar, which answers {@code /healthz} while its first
 * rate fetch is still in flight and therefore needs a second readiness probe,
 * patala's has nothing to warm up: {@code default_registry()} builds an
 * offline {@code MockRail} and the process is able to answer the moment it
 * binds. There is no {@code /readyz} here and none is needed.
 *
 * <h2>The registry this server starts with is mock-only</h2>
 *
 * {@code patala-sidecar}'s {@code default_registry()} registers exactly one
 * rail, {@code "mock"}. Any other {@code railId} is a {@code 404} — not
 * because the rail failed, but because the process has never heard of it.
 * Per-rail registration is unwritten; the HTTP surface around it is real and
 * tested.
 */
public final class Patala implements AutoCloseable {

    /** The patala version this SDK was written against. */
    public static final String VERSION = "0.1.0";

    /** The one rail {@code patala-sidecar}'s default registry knows. */
    public static final String DEFAULT_RAIL = "mock";

    /** Options for {@link #start(Options)}. */
    public static final class Options {
        /**
         * Fixed port; defaults to an ephemeral free port.
         *
         * <p>The server's own default is 8420. This class does not use it:
         * two test runs on one machine would collide, and the port is not the
         * interesting part of the address when the host is hardcoded to
         * loopback anyway.
         */
        public Integer port;
        /**
         * Bearer token for {@code /v1}. Defaults to 32 fresh random bytes.
         *
         * <p>Set it to attach to a sidecar somebody else started — pair it
         * with {@link #port} and {@link #attach}.
         */
        public String token;
        /** Extra environment for the child process. */
        public Map<String, String> env;
        /** How long to wait for {@code /healthz} (default 20s). */
        public Duration timeout;
        /** Per-request timeout (default 30s). */
        public Duration requestTimeout;
    }

    private final Process proc;          // null when attached to somebody else's server
    private final String baseUrl;
    private final String token;
    private final HttpClient http;
    private final Duration requestTimeout;
    private final Thread shutdownHook;

    private Patala(Process proc, String baseUrl, String token, Duration requestTimeout) {
        this.proc = proc;
        this.baseUrl = baseUrl;
        this.token = token;
        this.requestTimeout = requestTimeout;
        this.http = HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(5)).build();
        if (proc != null) {
            this.shutdownHook = new Thread(this::terminate);
            Runtime.getRuntime().addShutdownHook(shutdownHook);
        } else {
            this.shutdownHook = null;
        }
    }

    /**
     * Spawn {@code patala-sidecar} and wait for {@code /healthz}.
     *
     * <p>You own the returned instance and close it; this is not a
     * process-wide singleton. Several sidecars with different rail sets is a
     * normal thing to want, and a static singleton would forbid it.
     */
    public static Patala start(Options opts) {
        if (opts == null) {
            opts = new Options();
        }
        int port = opts.port != null ? opts.port : freePort();
        String token = opts.token != null && !opts.token.isEmpty() ? opts.token : freshToken();

        ProcessBuilder pb = new ProcessBuilder(binaryPath());
        // inheritIO so the server's own fail-closed startup message reaches
        // somebody. It never logs the token.
        pb.inheritIO();
        Map<String, String> environment = pb.environment();
        environment.put("PATALA_SIDECAR_PORT", Integer.toString(port));
        environment.put("PATALA_SIDECAR_TOKEN", token);
        if (opts.env != null) {
            environment.putAll(opts.env);
        }

        Process proc;
        try {
            proc = pb.start();
        } catch (IOException e) {
            throw new PatalaException("failed to spawn the patala-sidecar binary", e);
        }

        Patala instance = new Patala(proc, "http://127.0.0.1:" + port, token,
                opts.requestTimeout != null ? opts.requestTimeout : Duration.ofSeconds(30));
        try {
            instance.waitHealthy(opts.timeout != null ? opts.timeout : Duration.ofSeconds(20));
        } catch (RuntimeException e) {
            // Do not leave a child behind because startup failed.
            instance.close();
            throw e;
        }
        return instance;
    }

    /** {@code start(new Options())}. */
    public static Patala start() {
        return start(new Options());
    }

    /**
     * Talk to a {@code patala-sidecar} somebody else is running — the shape
     * key isolation actually takes in production, where one long-lived sidecar
     * serves several services and none of them spawns it.
     *
     * <p>{@link #close()} on the result stops nothing.
     */
    public static Patala attach(String baseUrl, String token) {
        return new Patala(null, baseUrl, token, Duration.ofSeconds(30));
    }

    /** {@code http://127.0.0.1:<port>}. */
    public String baseUrl() {
        return baseUrl;
    }

    /** True when this instance spawned the server it talks to. */
    public boolean ownsProcess() {
        return proc != null;
    }

    // ------------------------------------------------------------- operations

    /** {@code GET /healthz} — the one unauthenticated route. Returns {@code "ok"}. */
    public String healthz() {
        return get("/healthz");
    }

    /** {@code GET /v1/rails/{railId}} — {@code RailCapabilities}. */
    public String capabilities(String railId) {
        return get("/v1/rails/" + railId);
    }

    /** {@code POST /v1/rails/{railId}/quote} with a PayRequest. */
    public String quote(String railId, String payRequestJson) {
        return post("/v1/rails/" + railId + "/quote", payRequestJson);
    }

    /**
     * {@code POST /v1/rails/{railId}/charge} with a PayRequest — returns a
     * Receipt.
     *
     * <p>Store it. Handing it back to {@link #verify} and getting
     * {@code {"valid":true}} is the entitlement check; this call returning
     * {@code 200} is not.
     */
    public String charge(String railId, String payRequestJson) {
        return post("/v1/rails/" + railId + "/charge", payRequestJson);
    }

    /**
     * {@code POST /v1/rails/{railId}/verify} with a Receipt —
     * {@code {"valid":true|false}}, both as HTTP {@code 200}.
     *
     * <p>{@code false} is the fail-closed answer, and it is data rather than
     * an error precisely so a caller cannot mistake "verified false" for "the
     * sidecar broke".
     */
    public String verify(String railId, String receiptJson) {
        return post("/v1/rails/" + railId + "/verify", receiptJson);
    }

    /**
     * {@code POST /v1/rails/{railId}/validate-destination} — the offline
     * pre-flight check.
     *
     * <p>All five verdicts come back {@code 200}. <b>Read the body, not the
     * status code:</b> a {@code 200} means the rail answered, not that the
     * address is good. Branch on {@code status} and {@code is_refusal}, and
     * respect {@code human_must_confirm}, which is {@code true} even for
     * {@code "StructurallyValid"}.
     */
    public String validateDestination(String railId, String destination) {
        return post("/v1/rails/" + railId + "/validate-destination",
                "{\"destination\":" + Json.quote(destination) + "}");
    }

    /**
     * {@code POST /v1/rails/{railId}/webhook} — forward a processor's delivery
     * <b>verbatim</b>.
     *
     * <p>Pass the exact bytes the processor sent and its headers. Every
     * webhook scheme signs the bytes that arrived, so a body that has been
     * through a JSON round-trip on your side is no longer what was signed and
     * every genuine delivery will fail to authenticate.
     *
     * <p>A rail with no push delivery — the mock — answers {@code 501}, which
     * arrives here as a {@link PatalaException} rather than as an invented
     * event.
     */
    public String webhook(String railId, byte[] rawBody, Map<String, String> headers) {
        return send("POST", "/v1/rails/" + railId + "/webhook",
                HttpRequest.BodyPublishers.ofByteArray(rawBody), headers);
    }

    // ------------------------------------------------------------------- HTTP

    /** One authenticated GET; the body on {@code 200}. */
    public String get(String path) {
        return send("GET", path, HttpRequest.BodyPublishers.noBody(), null);
    }

    /** One authenticated POST of a JSON body; the body on {@code 200}. */
    public String post(String path, String body) {
        Map<String, String> h = new LinkedHashMap<>();
        h.put("Content-Type", "application/json");
        return send("POST", path,
                HttpRequest.BodyPublishers.ofString(body == null ? "" : body, StandardCharsets.UTF_8), h);
    }

    /**
     * The one place a status code is turned into a verdict.
     *
     * <p>Anything that is not {@code 200} raises, carrying the server's own
     * error envelope. That is deliberate for a payments client: returning the
     * body regardless of status — which openrate's SDK does, because a rates
     * lookup that failed is merely unhelpful — would let a {@code 404} or a
     * {@code 502} be parsed as a Receipt whose fields simply happened to be
     * absent.
     */
    private String send(String method, String path, HttpRequest.BodyPublisher body,
                        Map<String, String> headers) {
        HttpRequest.Builder b = HttpRequest.newBuilder(URI.create(baseUrl + path))
                .timeout(requestTimeout)
                .method(method, body);
        if (!"/healthz".equals(path)) {
            b.header("Authorization", "Bearer " + token);
        }
        if (headers != null) {
            for (Map.Entry<String, String> e : headers.entrySet()) {
                b.header(e.getKey(), e.getValue());
            }
        }
        HttpResponse<String> res;
        try {
            res = http.send(b.build(), HttpResponse.BodyHandlers.ofString());
        } catch (IOException e) {
            throw new PatalaException(method + " " + path + " failed", e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new PatalaException(method + " " + path + " interrupted", e);
        }
        if (res.statusCode() != 200) {
            throw new PatalaException(
                    method + " " + path + ": HTTP " + res.statusCode() + " " + oneLine(res.body()));
        }
        return res.body();
    }

    // ---------------------------------------------------------------- process

    /** Stop the child process. Idempotent — use try-with-resources. */
    @Override
    public void close() {
        terminate();
        if (shutdownHook != null) {
            try {
                Runtime.getRuntime().removeShutdownHook(shutdownHook);
            } catch (IllegalStateException ignored) {
                // Already shutting down; the hook is running or has run.
            }
        }
    }

    private void terminate() {
        if (proc == null || !proc.isAlive()) {
            return;
        }
        // SIGTERM first: main.rs has a graceful-shutdown path on ctrl_c/TERM.
        proc.destroy();
        try {
            if (!proc.waitFor(5, TimeUnit.SECONDS)) {
                proc.destroyForcibly();
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            proc.destroyForcibly();
        }
    }

    private void waitHealthy(Duration timeout) {
        HttpRequest req = HttpRequest.newBuilder(URI.create(baseUrl + "/healthz"))
                .timeout(Duration.ofSeconds(2))
                .GET()
                .build();
        long deadline = System.nanoTime() + timeout.toNanos();
        String last = "connection refused";
        while (System.nanoTime() < deadline) {
            if (proc != null && !proc.isAlive()) {
                // The commonest cause by far, and the server prints it: no
                // token in the environment. This class always sets one, so if
                // it happens here something removed it.
                throw new PatalaException(
                        "patala-sidecar exited before becoming healthy (status "
                                + proc.exitValue() + "). Its own message is above, on stderr.");
            }
            try {
                HttpResponse<Void> res = http.send(req, HttpResponse.BodyHandlers.discarding());
                if (res.statusCode() == 200) {
                    return;
                }
                last = "status " + res.statusCode();
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                break;
            } catch (Exception e) {
                last = String.valueOf(e.getMessage());
            }
            try {
                Thread.sleep(50);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                break;
            }
        }
        throw new PatalaException("patala-sidecar did not answer /healthz within "
                + human(timeout) + ": " + last);
    }

    // ---------------------------------------------------------------- helpers

    /**
     * 32 bytes from {@link SecureRandom}, hex-encoded — the same shape the
     * server's own startup message suggests
     * ({@code openssl rand -hex 32}).
     */
    public static String freshToken() {
        byte[] raw = new byte[32];
        new SecureRandom().nextBytes(raw);
        StringBuilder sb = new StringBuilder(64);
        for (byte b : raw) {
            sb.append(Character.forDigit((b >> 4) & 0xF, 16));
            sb.append(Character.forDigit(b & 0xF, 16));
        }
        return sb.toString();
    }

    private static String oneLine(String s) {
        return s == null ? "" : s.replaceAll("\\s+", " ").trim();
    }

    private static String human(Duration d) {
        long ms = d.toMillis();
        return ms % 1000 == 0 ? (ms / 1000) + "s" : ms + "ms";
    }

    /**
     * Resolve the binary: {@code $PATALA_SIDECAR_BINARY}, then a sibling
     * {@code bin/patala-sidecar} next to the classes or under
     * {@code $PATALA_HOME}, then {@code $PATALA_HOME/target/{release,debug}/},
     * then {@code patala-sidecar} on {@code PATH}.
     */
    static String binaryPath() {
        String env = System.getenv("PATALA_SIDECAR_BINARY");
        if (env != null && !env.isEmpty()) {
            return env;
        }
        boolean windows = System.getProperty("os.name", "").toLowerCase().contains("win");
        String name = windows ? "patala-sidecar.exe" : "patala-sidecar";

        Path bundled = bundledDir().resolve("bin").resolve(name);
        if (Files.isRegularFile(bundled)) {
            return bundled.toString();
        }
        String home = System.getenv("PATALA_HOME");
        if (home != null && !home.isEmpty()) {
            for (String profile : new String[] {"release", "debug"}) {
                Path p = Paths.get(home, "target", profile, name);
                if (Files.isRegularFile(p)) {
                    return p.toString();
                }
            }
        }
        String found = which(name);
        if (found != null) {
            return found;
        }
        throw new PatalaException(
                "patala-sidecar binary not found. Set PATALA_SIDECAR_BINARY, or build it: "
                        + "`cargo build -p patala-sidecar --release`");
    }

    private static Path bundledDir() {
        String home = System.getenv("PATALA_HOME");
        if (home != null && !home.isEmpty()) {
            return Paths.get(home);
        }
        try {
            Path self = Paths.get(
                    Patala.class.getProtectionDomain().getCodeSource().getLocation().toURI());
            Path dir = Files.isDirectory(self) ? self : self.getParent();
            return dir != null ? dir : Paths.get(".");
        } catch (Exception e) {
            return Paths.get(".");
        }
    }

    private static String which(String cmd) {
        String path = System.getenv("PATH");
        if (path == null) {
            return null;
        }
        for (String dir : path.split(File.pathSeparator)) {
            Path candidate = Paths.get(dir, cmd);
            if (Files.isRegularFile(candidate) && Files.isExecutable(candidate)) {
                return candidate.toString();
            }
        }
        return null;
    }

    private static int freePort() {
        try (ServerSocket s = new ServerSocket()) {
            s.bind(new InetSocketAddress("127.0.0.1", 0));
            return s.getLocalPort();
        } catch (IOException e) {
            throw new PatalaException("could not allocate a free port", e);
        }
    }
}
