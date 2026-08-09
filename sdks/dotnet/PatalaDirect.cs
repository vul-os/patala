using System;
using System.Collections.Generic;
using System.IO;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Threading;

namespace Patala
{
    /// <summary>
    /// patala <b>in this process</b>, through the C ABI of
    /// <c>libpatala_ffi</c>.
    ///
    /// <para><see cref="Sidecar"/> is the other path: it spawns
    /// <c>patala-sidecar</c> and talks HTTP over loopback. Both are supported.
    /// <b>For .NET the sidecar is the recommended default, and the deciding
    /// reason is platform coverage, not the runtime.</b> patala's shared
    /// library has been built and executed on exactly one target —
    /// darwin/arm64 — and there is no Windows DLL. .NET has a very large
    /// Windows install base; a direct-mode dependency would simply not load
    /// for a large fraction of the people who took it.</para>
    ///
    /// <para>Note what is <b>not</b> a reason here. llmux's and openrate's
    /// .NET SDKs warn about the Go runtime in your process — its GC, its
    /// scheduler, its signal handlers, its fork-unsafety. patala is Rust and
    /// carries none of that: no runtime, no threads, no signal handlers. That
    /// was measured against HotSpot rather than CoreCLR (see
    /// <c>sdks/java/README.md</c>), but the property being measured belongs to
    /// the library, not to the host: a library that calls <c>sigaction</c>
    /// zero times calls it zero times under either runtime.</para>
    ///
    /// <h3>JSON in, JSON out</h3>
    /// This class parses nothing. Every document it returns is the same JSON
    /// the sidecar serves, built from the same <c>patala-core</c> types, handed
    /// to you as a <c>string</c> for the parser you already have. Amounts are
    /// <b>integer minor units</b> on both sides — deserialise
    /// <c>amount_minor</c> as a <c>long</c>/<c>ulong</c>, never as a
    /// <c>double</c>.
    ///
    /// <h3>No streaming, and therefore no <c>IAsyncEnumerable</c></h3>
    /// There is no <c>patala_stream</c>. A quote, a charge, a verification and
    /// a destination check are each one question with one answer. llmux's .NET
    /// SDK, which binds the same ABI shape, does expose
    /// <c>IAsyncEnumerable&lt;string&gt;</c> for <c>llmux_stream</c>. The
    /// absence here is deliberate and stated rather than left to be noticed.
    /// </summary>
    public static partial class Direct
    {
        /// <summary>The patala version this SDK was written against.</summary>
        public const string Version = "0.1.0";

        // ------------------------------------------------------------ P/Invoke
        //
        // LibraryImport (source-generated, .NET 7+) rather than DllImport:
        // compile-time stubs, NativeAOT-compatible, every string across the
        // boundary declared rather than guessed.
        //
        // Every function returning a string returns IntPtr, never string. A
        // `string` return compiles, runs, and leaks: the marshaller copies the
        // C string and has no idea the original must go back to patala_free —
        // and it must go back to patala_free specifically, not to free(3),
        // because that memory came from Rust's allocator.
        //
        // patala has no callback (there is deliberately no patala_stream), so
        // `char**` is expressed as `out IntPtr` and nothing in this file uses
        // a pointer, a function pointer or a fixed buffer. The project still
        // sets AllowUnsafeBlocks, because LibraryImport requires it
        // unconditionally (SYSLIB1062) — see the .csproj.

        private const string Lib = "patala_ffi";

        [LibraryImport(Lib, EntryPoint = "patala_abi_version")]
        private static partial IntPtr AbiVersionNative();

        [LibraryImport(Lib, EntryPoint = "patala_abi_check", StringMarshalling = StringMarshalling.Utf8)]
        private static partial int AbiCheckNative(string expected, out IntPtr err);

        [LibraryImport(Lib, EntryPoint = "patala_new", StringMarshalling = StringMarshalling.Utf8)]
        private static partial ulong NewNative(string? configJson, out IntPtr err);

        [LibraryImport(Lib, EntryPoint = "patala_call", StringMarshalling = StringMarshalling.Utf8)]
        private static partial IntPtr CallNative(ulong handle, string method, string? requestJson, out IntPtr err);

        [LibraryImport(Lib, EntryPoint = "patala_close")]
        internal static partial void CloseNative(ulong handle);

        [LibraryImport(Lib, EntryPoint = "patala_free")]
        private static partial void FreeNative(IntPtr p);

        // ------------------------------------------------------ library lookup

        private static int _resolverInstalled;
        private static string? _explicitPath;

        /// <summary>Point the runtime at a specific libpatala_ffi.</summary>
        public static void UseLibrary(string path)
        {
            if (!File.Exists(path))
            {
                throw new PatalaException($"no libpatala_ffi at {path}");
            }
            _explicitPath = path;
            InstallResolver();
        }

        internal static void InstallResolver()
        {
            if (Interlocked.Exchange(ref _resolverInstalled, 1) == 1)
            {
                return;
            }
            NativeLibrary.SetDllImportResolver(
                Assembly.GetExecutingAssembly(),
                (name, assembly, searchPath) =>
                    name == Lib ? NativeLibrary.Load(_explicitPath ?? FindLibrary()) : IntPtr.Zero);
        }

        /// <summary>
        /// Locate libpatala_ffi: <c>$PATALA_LIBRARY</c>, then
        /// <c>$PATALA_HOME/target/{release,debug}/</c>, then
        /// <c>target/{release,debug}/</c> walking up from the current
        /// directory — the layout <c>cargo build</c> writes.
        ///
        /// <para>Unlike llmux's and openrate's Go libraries the file name
        /// carries no target triple: cargo writes one name per platform.</para>
        /// </summary>
        public static string FindLibrary()
        {
            string? explicitPath = Environment.GetEnvironmentVariable("PATALA_LIBRARY");
            if (!string.IsNullOrEmpty(explicitPath))
            {
                if (!File.Exists(explicitPath))
                {
                    throw new PatalaException(
                        $"PATALA_LIBRARY is set to {explicitPath}, which is not a file");
                }
                return explicitPath!;
            }

            string file = LibraryFileName();
            var tried = new List<string>();

            string? home = Environment.GetEnvironmentVariable("PATALA_HOME");
            if (!string.IsNullOrEmpty(home))
            {
                foreach (string profile in new[] { "release", "debug" })
                {
                    string p = Path.Combine(home!, "target", profile, file);
                    if (File.Exists(p))
                    {
                        return p;
                    }
                    tried.Add(p);
                }
            }

            for (DirectoryInfo? at = new DirectoryInfo(Directory.GetCurrentDirectory());
                 at != null;
                 at = at.Parent)
            {
                foreach (string profile in new[] { "release", "debug" })
                {
                    string p = Path.Combine(at.FullName, "target", profile, file);
                    if (File.Exists(p))
                    {
                        return p;
                    }
                    tried.Add(p);
                }
            }

            throw new PatalaException(
                $"no {file} found. Tried:\n  " + string.Join("\n  ", tried)
                + "\nBuild one with `cargo build -p patala-ffi --release` in the patala checkout,"
                + " or set PATALA_LIBRARY to an existing library."
                + "\nThe only library built and executed so far is darwin/arm64."
                + " There is no Windows DLL — on Windows, use the sidecar.");
        }

        /// <summary>libpatala_ffi.dylib / libpatala_ffi.so / patala_ffi.dll.</summary>
        public static string LibraryFileName() =>
            RuntimeInformation.IsOSPlatform(OSPlatform.OSX) ? "libpatala_ffi.dylib"
            : RuntimeInformation.IsOSPlatform(OSPlatform.Windows) ? "patala_ffi.dll"
            : "libpatala_ffi.so";

        // ------------------------------------------------------------- opening

        /// <summary>
        /// Build a rail and take a handle on it.
        ///
        /// <para><b>Creating a rail talks to nothing</b>: no socket is opened,
        /// no thread is started, nothing is read from the environment. Only a
        /// call reaches a network, and only for a rail that has one.</para>
        ///
        /// <para>Unknown configuration fields are <b>refused</b>, so a
        /// misspelled <c>"currencys"</c> is an error rather than a rail quietly
        /// built with a currency list you did not choose.</para>
        /// </summary>
        /// <param name="configJson">
        /// A JSON object tagged by <c>"rail"</c>, or null for the offline
        /// default: a deterministic MockRail on USDC.
        /// </param>
        /// <param name="libraryPath">An explicit library, or null to search.</param>
        public static PatalaRail Open(string? configJson = null, string? libraryPath = null)
        {
            if (libraryPath != null)
            {
                UseLibrary(libraryPath);
            }
            else
            {
                InstallResolver();
            }

            ulong h = NewNative(configJson, out IntPtr err);
            // 0 is FAILURE here, not success: patala_new's success value is a
            // handle, and handles start at 1.
            if (h == 0)
            {
                throw new PatalaException("patala_new: " + TakeError(ref err));
            }
            DrainError(ref err);
            return new PatalaRail(new PatalaSafeHandle(h));
        }

        /// <summary>
        /// The offline MockRail — deterministic, no credentials, no network.
        /// </summary>
        /// <param name="destinationChecks">
        /// Pass <c>false</c> for a rail that answers <c>Unknown</c> to every
        /// destination: the offline stand-in for a fiat rail, whose
        /// destination is an opaque processor-side token. It exists so the
        /// branch of a payout UI that matters most — "a human must decide" —
        /// is reachable without compiling in a real rail.
        /// </param>
        public static PatalaRail Mock(
            string id = "mock",
            string railClass = "non-custodial-final",
            IEnumerable<string>? currencies = null,
            ulong feeMinor = 0,
            bool failing = false,
            bool destinationChecks = true,
            string? libraryPath = null)
        {
            var list = new List<string>(currencies ?? new[] { "USDC" });
            var sb = new System.Text.StringBuilder("{\"rail\":\"mock\"");
            sb.Append(",\"id\":").Append(Json.Quote(id));
            sb.Append(",\"class\":").Append(Json.Quote(railClass));
            sb.Append(",\"currencies\":[");
            for (int i = 0; i < list.Count; i++)
            {
                if (i > 0)
                {
                    sb.Append(',');
                }
                sb.Append(Json.Quote(list[i]));
            }
            sb.Append(']');
            sb.Append(",\"fee_minor\":").Append(feeMinor);
            sb.Append(",\"failing\":").Append(failing ? "true" : "false");
            sb.Append(",\"destination_checks\":").Append(destinationChecks ? "true" : "false");
            sb.Append('}');
            return Open(sb.ToString(), libraryPath);
        }

        /// <summary>The patala version the loaded shared library was built from.</summary>
        public static string AbiVersion()
        {
            InstallResolver();
            IntPtr p = AbiVersionNative();
            if (p == IntPtr.Zero)
            {
                throw new PatalaException("patala_abi_version returned NULL");
            }
            // Static, owned by the library. The header says: never free it.
            return Marshal.PtrToStringUTF8(p)
                ?? throw new PatalaException("abi version is not UTF-8");
        }

        /// <summary>
        /// Ask the library to compare its own version against
        /// <paramref name="expected"/>, through <c>patala_abi_check</c> rather
        /// than by comparing strings here — so the comparison is not
        /// reimplemented, and forgotten, in each binding.
        ///
        /// <para>A shared library is resolved off a load path you may not
        /// control; without this probe a stale libpatala_ffi earlier on that
        /// path is called silently and misbehaves in ways that look like
        /// patala bugs.</para>
        /// </summary>
        public static void AbiCheck(string? expected = null)
        {
            InstallResolver();
            int rc = AbiCheckNative(expected ?? Version, out IntPtr err);
            if (rc != 0)
            {
                throw new PatalaException("patala_abi_check: " + TakeError(ref err));
            }
            DrainError(ref err);
        }

        // -------------------------------------------------------------- shared

        internal static string Call(ulong handle, string method, string? requestJson)
        {
            IntPtr result = CallNative(handle, method, requestJson, out IntPtr err);
            if (result == IntPtr.Zero)
            {
                throw new PatalaException($"patala_call({method}): " + TakeError(ref err));
            }
            try
            {
                return Marshal.PtrToStringUTF8(result)
                    ?? throw new PatalaException("patala_call returned a string that is not UTF-8");
            }
            finally
            {
                // Copied into a managed string; the Rust allocation goes back
                // to the only allocator that can take it.
                FreeNative(result);
                DrainError(ref err);
            }
        }

        private static string TakeError(ref IntPtr err)
        {
            if (err == IntPtr.Zero)
            {
                return "the library reported a failure but set no message";
            }
            string message = Marshal.PtrToStringUTF8(err) ?? "(error message is not UTF-8)";
            FreeNative(err);
            err = IntPtr.Zero;
            return message;
        }

        /// <summary>
        /// Free the error out-parameter on the SUCCESS path too, so a library
        /// that set a message alongside a success cannot leak it.
        /// </summary>
        private static void DrainError(ref IntPtr err)
        {
            if (err != IntPtr.Zero)
            {
                FreeNative(err);
                err = IntPtr.Zero;
            }
        }
    }

    /// <summary>
    /// A patala rail handle, as a <see cref="SafeHandle"/> so it is released
    /// deterministically by <c>using</c> and, failing that, by the finaliser
    /// the base class provides — rather than never.
    ///
    /// <para>patala handles are uint64 registry keys, not pointers. SafeHandle
    /// is still the right vehicle: it is the type the runtime knows how to
    /// keep alive across a P/Invoke and release exactly once. The key lives in
    /// the IntPtr, 64 bits on every platform patala builds for.</para>
    /// </summary>
    public sealed class PatalaSafeHandle : SafeHandle
    {
        internal PatalaSafeHandle(ulong h)
            : base(IntPtr.Zero, ownsHandle: true)
        {
            SetHandle((IntPtr)h);
        }

        /// <summary>Handles start at 1, so 0 is "no handle".</summary>
        public override bool IsInvalid => handle == IntPtr.Zero;

        internal ulong Value => (ulong)handle;

        protected override bool ReleaseHandle()
        {
            // Closing an unknown or already-closed handle is a documented
            // no-op, so this cannot fail.
            Direct.CloseNative((ulong)handle);
            return true;
        }
    }

    /// <summary>
    /// One rail, running in this process.
    ///
    /// <para>A handle is safe to use from several threads at once. Calls on
    /// <b>one</b> handle serialise, because that handle owns one
    /// current-thread Tokio runtime; calls on different handles run
    /// concurrently. Open one handle per rail, and more than one if you want
    /// parallelism on the same rail.</para>
    /// </summary>
    public sealed class PatalaRail : IDisposable
    {
        private readonly PatalaSafeHandle _handle;

        internal PatalaRail(PatalaSafeHandle handle) => _handle = handle;

        /// <summary>Run one method. See patala.h for the set of nine.</summary>
        public string Call(string method, string? requestJson = null)
        {
            bool taken = false;
            try
            {
                // DangerousAddRef around the call, so a concurrent Dispose
                // cannot close the handle mid-flight.
                _handle.DangerousAddRef(ref taken);
                if (!taken || _handle.IsClosed)
                {
                    throw new PatalaException("this rail is disposed");
                }
                return Direct.Call(_handle.Value, method, requestJson);
            }
            catch (ObjectDisposedException)
            {
                throw new PatalaException("this rail is disposed");
            }
            finally
            {
                if (taken)
                {
                    _handle.DangerousRelease();
                }
            }
        }

        /// <summary><c>{"rail_id":"mock"}</c>.</summary>
        public string Id() => Call("id");

        /// <summary>
        /// RailCapabilities — how to decide your whole UX without knowing
        /// which provider answered. A <c>CustodialReversible</c> rail means a
        /// card form and a refundable pending state; a
        /// <c>NonCustodialFinal</c> rail means a wallet address and a signed
        /// final receipt. It is not a bool, because those are not two shades
        /// of one thing.
        /// </summary>
        public string Capabilities() => Call("capabilities");

        /// <summary>A Quote for a PayRequest.</summary>
        public string Quote(string payRequestJson) => Call("quote", payRequestJson);

        /// <summary>
        /// A Receipt. <b>Store it.</b> Handing it back to <see cref="Verify"/>
        /// later and getting true is the entitlement check; this call
        /// returning without throwing is not.
        /// </summary>
        public string Charge(string payRequestJson) => Call("charge", payRequestJson);

        /// <summary><c>{"valid":true|false}</c>. See <see cref="IsValid"/>.</summary>
        public string Verify(string receiptJson) => Call("verify", receiptJson);

        /// <summary>
        /// <see cref="Verify"/> as a bool, decided by an exact match on the
        /// library's own two possible answers.
        ///
        /// <para><b>Fails closed twice over.</b> <c>false</c> is patala's
        /// honest verdict that a receipt does not hold, not a transient
        /// failure to retry — and anything this method does not recognise is
        /// also false, so a future third answer cannot be read as "valid" by a
        /// caller who has not been updated.</para>
        /// </summary>
        public bool IsValid(string receiptJson) => Verify(receiptJson).Trim() == "{\"valid\":true}";

        /// <summary>
        /// The offline pre-flight check to run <b>before</b> any money moves.
        ///
        /// <para>It never fails: "I cannot check this" is the verdict
        /// <c>Unknown</c>, not an error, because a caller must handle it as
        /// carefully as a refusal and an error is too easy to swallow.</para>
        /// </summary>
        public string ValidateDestination(string destination) =>
            Call("validate-destination", "{\"destination\":" + Json.Quote(destination) + "}");

        /// <summary>
        /// True when the verdict says <b>do not send</b>.
        ///
        /// <para>Read from the document's own <c>is_refusal</c> field rather
        /// than re-derived from <c>status</c>: a re-derivation falls through
        /// to its default for any status added later, and that default is "not
        /// a refusal" — failing open, on the one question in this API where
        /// failing open loses money.</para>
        /// </summary>
        public bool IsRefusal(string verdictJson) => Json.Field(verdictJson, "is_refusal") == "true";

        /// <summary>
        /// The sentence to show the human being asked for a payout address,
        /// before there is a verdict to render. Every verdict — including
        /// <c>StructurallyValid</c> — carries <c>human_must_confirm: true</c>
        /// and this same text, because patala does not detect exchange-owned
        /// addresses and will not guess.
        /// </summary>
        public string Caveat() => Call("caveat");

        /// <summary>
        /// Release the rail. Idempotent, and calling after it is a clean
        /// <see cref="PatalaException"/> rather than a crash: handle numbers
        /// are retired, not recycled, so a stale handle can never land on
        /// somebody else's rail.
        /// </summary>
        public void Dispose() => _handle.Dispose();
    }
}
