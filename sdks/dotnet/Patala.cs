using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Net.Sockets;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace Patala
{
    /// <summary>
    /// patala as a <b>child process</b> on <c>127.0.0.1</c>, spoken to over
    /// HTTP.
    ///
    /// <code>
    /// using var patala = Sidecar.Start();
    /// string receipt = await patala.ChargeAsync(Json.PayRequest(1250, "USDC", "mock:wallet:alice", "order-4711"));
    /// bool paid = await patala.IsValidAsync(receipt);
    /// </code>
    ///
    /// <para><b>This is the recommended default on .NET</b>, and the deciding
    /// reason is platform coverage: patala's shared library has been built and
    /// executed on darwin/arm64 only, and there is no Windows DLL. The sidecar
    /// is an ordinary Rust binary and builds wherever Rust runs.</para>
    ///
    /// <para>The second reason is <b>key isolation</b>. A non-custodial rail's
    /// signing key lives inside whichever process calls <c>charge</c>; link
    /// the direct path into five services and it is smeared across five
    /// processes' memory. Route them through one sidecar and it lives in one
    /// narrow, purpose-built process. See <c>patala-sidecar/README.md</c> for
    /// the threat model, including what it does not defend against: a
    /// co-resident, same-privilege attacker can read the token out of the
    /// environment.</para>
    ///
    /// <para>Like openrate's and unlike llmux's .NET SDK, this is <b>not a
    /// process-wide singleton</b>: you get an instance you own and dispose.
    /// Two sidecars with different rail sets is a normal thing to want, and a
    /// static singleton would forbid it.</para>
    /// </summary>
    public sealed class Sidecar : IDisposable
    {
        /// <summary>The patala version this SDK was written against.</summary>
        public const string Version = "0.1.0";

        /// <summary>The one rail patala-sidecar's default registry knows.</summary>
        public const string DefaultRail = "mock";

        /// <summary>Options for <see cref="Start"/>.</summary>
        public sealed class Options
        {
            /// <summary>
            /// Fixed port; defaults to an ephemeral free port.
            ///
            /// <para>The server's own default is 8420. This class does not use
            /// it: two test runs on one machine would collide.</para>
            /// </summary>
            public int? Port { get; set; }

            /// <summary>
            /// Bearer token for <c>/v1</c>. Defaults to 32 fresh random bytes.
            /// </summary>
            public string? Token { get; set; }

            /// <summary>Extra environment for the child process.</summary>
            public IDictionary<string, string>? Env { get; set; }

            /// <summary>How long to wait for <c>/healthz</c> (default 20s).</summary>
            public TimeSpan HealthTimeout { get; set; } = TimeSpan.FromSeconds(20);

            /// <summary>Per-request timeout (default 30s).</summary>
            public TimeSpan RequestTimeout { get; set; } = TimeSpan.FromSeconds(30);

            /// <summary>The rail id the un-suffixed methods talk to.</summary>
            public string RailId { get; set; } = DefaultRail;
        }

        private readonly Process? _proc;   // null when attached to somebody else's server
        private readonly HttpClient _http;
        private readonly string _token;
        private int _disposed;

        /// <summary><c>http://127.0.0.1:&lt;port&gt;</c>.</summary>
        public string BaseUrl { get; }

        /// <summary>The rail id the un-suffixed methods talk to.</summary>
        public string RailId { get; }

        /// <summary>True when this instance spawned the server it talks to.</summary>
        public bool OwnsProcess => _proc != null;

        private Sidecar(Process? proc, string baseUrl, string token, string railId, TimeSpan requestTimeout)
        {
            _proc = proc;
            BaseUrl = baseUrl;
            RailId = railId;
            _token = token;
            _http = new HttpClient { Timeout = requestTimeout };
        }

        /// <summary>
        /// Spawn <c>patala-sidecar</c> and wait for <c>/healthz</c>.
        ///
        /// <para>The wait is the whole wait. openrate's sidecar needs a second
        /// readiness probe because it answers <c>/healthz</c> while its first
        /// rate fetch is in flight; patala's has nothing to warm up — its
        /// default registry is an offline MockRail and it can answer the moment
        /// it binds. There is no <c>/readyz</c> here and none is needed.</para>
        /// </summary>
        public static Sidecar Start(Options? options = null)
        {
            var opts = options ?? new Options();
            int port = opts.Port ?? FreePort();
            string token = string.IsNullOrEmpty(opts.Token) ? FreshToken() : opts.Token!;

            var psi = new ProcessStartInfo(BinaryPath())
            {
                UseShellExecute = false,
            };
            psi.Environment["PATALA_SIDECAR_PORT"] = port.ToString();
            psi.Environment["PATALA_SIDECAR_TOKEN"] = token;
            if (opts.Env != null)
            {
                foreach (var kv in opts.Env)
                {
                    psi.Environment[kv.Key] = kv.Value;
                }
            }

            Process proc;
            try
            {
                proc = Process.Start(psi) ?? throw new PatalaException("Process.Start returned null");
            }
            catch (Exception e) when (e is not PatalaException)
            {
                throw new PatalaException("failed to spawn the patala-sidecar binary", e);
            }

            var instance = new Sidecar(proc, $"http://127.0.0.1:{port}", token, opts.RailId, opts.RequestTimeout);
            try
            {
                instance.WaitHealthy(opts.HealthTimeout);
            }
            catch
            {
                // Do not leave a child behind because startup failed.
                instance.Dispose();
                throw;
            }
            return instance;
        }

        /// <summary>
        /// Talk to a <c>patala-sidecar</c> somebody else is running — the
        /// shape key isolation actually takes in production, where one
        /// long-lived sidecar serves several services and none of them spawns
        /// it. <see cref="Dispose"/> on the result stops nothing.
        /// </summary>
        public static Sidecar Attach(string baseUrl, string token, string railId = DefaultRail) =>
            new Sidecar(null, baseUrl, token, railId, TimeSpan.FromSeconds(30));

        // ------------------------------------------------------------ requests

        /// <summary><c>GET /healthz</c> — the one unauthenticated route.</summary>
        public Task<string> HealthzAsync() => SendAsync(HttpMethod.Get, "/healthz", null, null);

        /// <summary><c>GET /v1/rails/{rail}</c> — RailCapabilities.</summary>
        public Task<string> CapabilitiesAsync(string? rail = null) =>
            SendAsync(HttpMethod.Get, $"/v1/rails/{rail ?? RailId}", null, null);

        /// <summary><c>POST /v1/rails/{rail}/quote</c>.</summary>
        public Task<string> QuoteAsync(string payRequestJson, string? rail = null) =>
            PostJsonAsync($"/v1/rails/{rail ?? RailId}/quote", payRequestJson);

        /// <summary>
        /// <c>POST /v1/rails/{rail}/charge</c> — a Receipt. <b>Store it.</b>
        /// Handing it back to <see cref="VerifyAsync"/> and getting
        /// <c>{"valid":true}</c> is the entitlement check; a 200 here is not.
        /// </summary>
        public Task<string> ChargeAsync(string payRequestJson, string? rail = null) =>
            PostJsonAsync($"/v1/rails/{rail ?? RailId}/charge", payRequestJson);

        /// <summary>
        /// <c>POST /v1/rails/{rail}/verify</c> — <c>{"valid":true|false}</c>,
        /// <b>both HTTP 200</b>. false is data, never an HTTP error, precisely
        /// so a caller cannot mistake "verified false" for "the sidecar broke".
        /// </summary>
        public Task<string> VerifyAsync(string receiptJson, string? rail = null) =>
            PostJsonAsync($"/v1/rails/{rail ?? RailId}/verify", receiptJson);

        /// <summary>
        /// <see cref="VerifyAsync"/> as a bool, fail-closed on anything
        /// unrecognised.
        /// </summary>
        public async Task<bool> IsValidAsync(string receiptJson, string? rail = null) =>
            (await VerifyAsync(receiptJson, rail).ConfigureAwait(false)).Trim() == "{\"valid\":true}";

        /// <summary>
        /// <c>POST /v1/rails/{rail}/validate-destination</c> — the offline
        /// pre-flight check.
        ///
        /// <para>All five verdicts come back 200. <b>Read the body, not the
        /// status code:</b> a 200 means the rail answered, not that the
        /// address is good. Branch on <c>status</c> and <c>is_refusal</c>, and
        /// respect <c>human_must_confirm</c>, which is true even for
        /// <c>StructurallyValid</c>.</para>
        /// </summary>
        public Task<string> ValidateDestinationAsync(string destination, string? rail = null) =>
            PostJsonAsync($"/v1/rails/{rail ?? RailId}/validate-destination",
                "{\"destination\":" + Json.Quote(destination) + "}");

        /// <summary>
        /// True when the verdict says <b>do not send</b>.
        ///
        /// <para>Read from the document's own <c>is_refusal</c> field — which
        /// Rust computes once, in <c>DestinationVerdict::is_refusal()</c>, from
        /// an exhaustive match — and never re-derived from <c>status</c> here,
        /// where a status added later would fall through to a default of "not
        /// a refusal".</para>
        ///
        /// <para><b>Fails closed.</b> A verdict this SDK cannot parse, one with
        /// no <c>is_refusal</c>, or one whose <c>is_refusal</c> is not a JSON
        /// boolean, all answer <c>true</c>. This was
        /// <c>Json.Field(verdictJson, "is_refusal") == "true"</c> over a
        /// substring scan that did not skip whitespace after the colon, so a
        /// verdict reformatted anywhere between patala and here read as
        /// <c>" true"</c> and this returned <b>false for a Malformed
        /// verdict</b>.</para>
        /// </summary>
        public static bool IsRefusal(string verdictJson) => Json.Flag(verdictJson, "is_refusal", true);

        /// <summary>
        /// <c>POST /v1/rails/{rail}/webhook</c> — forward a processor's
        /// delivery <b>verbatim</b>: the exact bytes it sent, and its headers.
        ///
        /// <para>Every webhook scheme signs the bytes that arrived, so a body
        /// that has been through a JSON round-trip on your side is no longer
        /// what was signed and every genuine delivery will fail to
        /// authenticate. A rail with no push delivery — the mock — answers 501,
        /// which arrives as a <see cref="PatalaException"/> rather than as an
        /// invented event.</para>
        /// </summary>
        public Task<string> WebhookAsync(
            byte[] rawBody, IDictionary<string, string>? headers = null, string? rail = null) =>
            SendAsync(HttpMethod.Post, $"/v1/rails/{rail ?? RailId}/webhook",
                new ByteArrayContent(rawBody), headers);

        private Task<string> PostJsonAsync(string path, string body)
        {
            var content = new StringContent(body, Encoding.UTF8, "application/json");
            return SendAsync(HttpMethod.Post, path, content, null);
        }

        /// <summary>
        /// The one place a status code is turned into a verdict.
        ///
        /// <para>Anything that is not 200 raises, carrying the server's own
        /// error envelope. That is deliberate for a payments client: returning
        /// the body regardless of status — which openrate's SDK does, because
        /// a rates lookup that failed is merely unhelpful — would let a 404 or
        /// a 502 be deserialised into a Receipt whose fields simply happened to
        /// be absent.</para>
        /// </summary>
        private async Task<string> SendAsync(
            HttpMethod method, string path, HttpContent? content, IDictionary<string, string>? headers)
        {
            using var req = new HttpRequestMessage(method, BaseUrl + path);
            if (content != null)
            {
                req.Content = content;
            }
            if (path != "/healthz")
            {
                req.Headers.TryAddWithoutValidation("Authorization", "Bearer " + _token);
            }
            if (headers != null)
            {
                foreach (var kv in headers)
                {
                    if (!req.Headers.TryAddWithoutValidation(kv.Key, kv.Value))
                    {
                        req.Content?.Headers.TryAddWithoutValidation(kv.Key, kv.Value);
                    }
                }
            }

            HttpResponseMessage res;
            try
            {
                res = await _http.SendAsync(req).ConfigureAwait(false);
            }
            catch (Exception e)
            {
                throw new PatalaException($"{method} {path} failed", e);
            }
            using (res)
            {
                string body = await res.Content.ReadAsStringAsync().ConfigureAwait(false);
                if (res.StatusCode != HttpStatusCode.OK)
                {
                    throw new PatalaException(
                        $"{method} {path}: HTTP {(int)res.StatusCode} {OneLine(body)}");
                }
                return body;
            }
        }

        // ------------------------------------------------------------- process

        /// <summary>Stop the child process. Idempotent — use <c>using</c>.</summary>
        public void Dispose()
        {
            if (Interlocked.Exchange(ref _disposed, 1) == 1)
            {
                return;
            }
            _http.Dispose();
            if (_proc == null)
            {
                return;
            }
            try
            {
                if (!_proc.HasExited)
                {
                    // main.rs has a graceful-shutdown path; Kill is what .NET
                    // gives us portably, and the server holds no state to lose.
                    _proc.Kill(entireProcessTree: true);
                    _proc.WaitForExit(5000);
                }
            }
            catch (InvalidOperationException)
            {
                // Already gone.
            }
            finally
            {
                _proc.Dispose();
            }
        }

        private void WaitHealthy(TimeSpan timeout)
        {
            var deadline = DateTime.UtcNow + timeout;
            string last = "connection refused";
            while (DateTime.UtcNow < deadline)
            {
                if (_proc != null && _proc.HasExited)
                {
                    // The commonest cause by far, and the server prints it: no
                    // token in the environment. This class always sets one.
                    throw new PatalaException(
                        $"patala-sidecar exited before becoming healthy (status {_proc.ExitCode})."
                        + " Its own message is above, on stderr.");
                }
                try
                {
                    using var probe = new HttpClient { Timeout = TimeSpan.FromSeconds(2) };
                    var res = probe.GetAsync(BaseUrl + "/healthz").GetAwaiter().GetResult();
                    if (res.StatusCode == HttpStatusCode.OK)
                    {
                        return;
                    }
                    last = "status " + (int)res.StatusCode;
                }
                catch (Exception e)
                {
                    last = e.InnerException?.Message ?? e.Message;
                }
                Thread.Sleep(50);
            }
            throw new PatalaException(
                $"patala-sidecar did not answer /healthz within {timeout.TotalSeconds:0}s: {last}");
        }

        // ------------------------------------------------------------- helpers

        /// <summary>
        /// 32 bytes from the OS CSPRNG, hex-encoded — the same shape the
        /// server's own startup message suggests (<c>openssl rand -hex 32</c>).
        /// </summary>
        public static string FreshToken() => Convert.ToHexString(RandomNumberGenerator.GetBytes(32)).ToLowerInvariant();

        private static string OneLine(string s) =>
            string.IsNullOrEmpty(s) ? string.Empty : System.Text.RegularExpressions.Regex.Replace(s, @"\s+", " ").Trim();

        /// <summary>
        /// Resolve the binary: <c>$PATALA_SIDECAR_BINARY</c>, then
        /// <c>bin/patala-sidecar</c> next to the assembly, then
        /// <c>$PATALA_HOME/target/{release,debug}/</c>, then <c>PATH</c>.
        /// </summary>
        public static string BinaryPath()
        {
            string? env = Environment.GetEnvironmentVariable("PATALA_SIDECAR_BINARY");
            if (!string.IsNullOrEmpty(env))
            {
                return env!;
            }
            bool windows = RuntimeInformation.IsOSPlatform(OSPlatform.Windows);
            string name = windows ? "patala-sidecar.exe" : "patala-sidecar";

            string? dir = Path.GetDirectoryName(Assembly.GetExecutingAssembly().Location);
            if (!string.IsNullOrEmpty(dir))
            {
                string bundled = Path.Combine(dir!, "bin", name);
                if (File.Exists(bundled))
                {
                    return bundled;
                }
            }

            string? home = Environment.GetEnvironmentVariable("PATALA_HOME");
            if (!string.IsNullOrEmpty(home))
            {
                foreach (string profile in new[] { "release", "debug" })
                {
                    string p = Path.Combine(home!, "target", profile, name);
                    if (File.Exists(p))
                    {
                        return p;
                    }
                }
            }

            string? path = Environment.GetEnvironmentVariable("PATH");
            if (path != null)
            {
                foreach (string d in path.Split(Path.PathSeparator))
                {
                    string candidate = Path.Combine(d, name);
                    if (File.Exists(candidate))
                    {
                        return candidate;
                    }
                }
            }

            throw new PatalaException(
                "patala-sidecar binary not found. Set PATALA_SIDECAR_BINARY, or build it:"
                + " `cargo build -p patala-sidecar --release`");
        }

        private static int FreePort()
        {
            var listener = new TcpListener(IPAddress.Loopback, 0);
            listener.Start();
            int port = ((IPEndPoint)listener.LocalEndpoint).Port;
            listener.Stop();
            return port;
        }
    }
}
