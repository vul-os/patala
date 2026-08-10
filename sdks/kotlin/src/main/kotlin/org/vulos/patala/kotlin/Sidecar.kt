@file:JvmName("PatalaSidecarKt")

package org.vulos.patala.kotlin

import java.time.Duration
import org.vulos.patala.Json
import org.vulos.patala.Patala as JavaPatala
import uniffi.patala.PayRequest

/**
 * A [PayRequest] as the JSON document `patala-sidecar` expects on the wire.
 *
 * The sidecar speaks JSON — that is what an HTTP boundary is — so this path
 * keeps taking and returning strings. What it does not do any more is make you
 * *build* one: [payRequest] produces the same typed record the direct path
 * takes, and this turns it into bytes at the last possible moment.
 *
 * `amountMinor` is written as the `ULong` it is. It is spelled out here rather
 * than handed to a JSON library on purpose: a `Receipt` whose `amount_minor`
 * went through a `Double` is a payments bug that type-checks, and the default
 * number handling of whichever JSON library is already in your build is not
 * this SDK's business.
 */
public fun PayRequest.toJson(): String =
    "{\"amount_minor\":${this.amountMinor}" +
        ",\"currency\":${Json.quote(this.currency)}" +
        ",\"destination\":${Json.quote(this.destination)}" +
        ",\"reference\":${Json.quote(this.reference)}}"

/**
 * Kotlin over [org.vulos.patala.Patala] — patala as a child process on
 * `127.0.0.1`, spoken to over HTTP.
 *
 * ```kotlin
 * PatalaSidecar().use { patala ->
 *     val receipt = patala.charge(payRequest(1250, "USDC", "mock:wallet:alice", "order-4711"))
 *     check(patala.isValid(receipt))
 * }
 * ```
 *
 * `use {}` stops the child process on every path out. Runs on **Java 11+**
 * with no native library, no `--enable-native-access` and no platform matrix
 * — including on Windows, where `libpatala_ffi` has never been built.
 *
 * ### Why you would choose this over the direct path
 *
 * **Key isolation**, and on patala that is the whole argument — not the JVM.
 * A non-custodial rail's signing key lives inside whichever process calls
 * `charge`; route five services through one sidecar and the key lives in one
 * narrow process instead of five. `patala-sidecar/README.md` has the threat
 * model, including what it does not defend against.
 *
 * The *other* products' reason — "loading the shared library replaces the
 * JVM's signal handlers" — is true of llmux and openrate and **false here**.
 * See `README.md`, which carries the measurement rather than the claim.
 *
 * ### There is no `isRefusal(json)` here, and that is the fix
 *
 * This class used to carry `Json.field(verdictJson, "is_refusal") == "true"`,
 * and [org.vulos.patala.Json.field] is a substring scan, not a parser: it did
 * not skip whitespace after the colon. A verdict reformatted anywhere between
 * patala and here read as `" true"`, so `isRefusal` returned **false for a
 * `Malformed` verdict** — failing open, on the one question in this API where
 * failing open loses money.
 *
 * The direct path deleted the identical function rather than repairing it
 * (see [Patala]'s docs — `verdict.isRefusal` is a `Boolean` field decided
 * inside Rust), and [org.vulos.patala.Patala], the Java sidecar client this
 * class wraps under the same no-JSON-dependency constraint, never had one.
 * The sidecar path was simply never migrated. It is now.
 *
 * A better scan is not the fix. patala computes `is_refusal` once, in
 * `DestinationVerdict::is_refusal()`, from an exhaustive match, and puts it on
 * the wire precisely so no binding has to re-derive it. Recovering a boolean
 * from a JSON document is your parser's job — the same parser every other
 * method here hands you a document for — and it must default to refusing:
 *
 * ```kotlin
 * val verdict = mapper.readTree(patala.validateDestination(dest))
 * if (verdict.path("is_refusal").asBoolean(true)) return   // default: refuse
 * ```
 *
 * With no parser on the classpath, `Json.field(verdict, "is_refusal")` will
 * print the field. It is documented for printing, and only that.
 *
 * ### The token is mandatory and this class mints one
 *
 * `patala-sidecar` refuses to start without `PATALA_SIDECAR_TOKEN`. Leaving
 * [token] null mints 32 bytes from `SecureRandom` and sends them as
 * `Authorization: Bearer` on every `/v1` request. `/healthz` is the one
 * unauthenticated route.
 *
 * Pass [token] and [port], or use [PatalaSidecar.attach], to talk to a sidecar
 * somebody else runs — which is the shape key isolation actually takes in
 * production, where one long-lived sidecar serves several services and none of
 * them spawns it.
 */
public class PatalaSidecar private constructor(
    private val delegate: JavaPatala,
    /**
     * The rail id every un-suffixed method below talks to.
     *
     * `patala-sidecar`'s default registry contains exactly one rail, `"mock"`.
     * Any other id is a 404 — not a failure of that rail, but a process that
     * has never heard of it.
     */
    public val railId: String,
) : AutoCloseable {

    public constructor(
        railId: String = JavaPatala.DEFAULT_RAIL,
        port: Int? = null,
        token: String? = null,
        extraEnv: Map<String, String>? = null,
        healthTimeout: Duration = Duration.ofSeconds(20),
        requestTimeout: Duration = Duration.ofSeconds(30),
    ) : this(
        JavaPatala.start(
            JavaPatala.Options().apply {
                this.port = port
                this.token = token
                this.env = extraEnv
                this.timeout = healthTimeout
                this.requestTimeout = requestTimeout
            },
        ),
        railId,
    )

    public companion object {
        /**
         * Talk to a `patala-sidecar` somebody else started. [close] on the
         * result stops nothing.
         */
        public fun attach(
            baseUrl: String,
            token: String,
            railId: String = JavaPatala.DEFAULT_RAIL,
        ): PatalaSidecar = PatalaSidecar(JavaPatala.attach(baseUrl, token), railId)
    }

    /** `http://127.0.0.1:<port>`. */
    public val baseUrl: String get() = delegate.baseUrl()

    /** True when this instance spawned the server it talks to. */
    public val ownsProcess: Boolean get() = delegate.ownsProcess()

    /** `GET /healthz` — the one unauthenticated route. `"ok"`. */
    public fun healthz(): String = delegate.healthz()

    /** `GET /v1/rails/{rail}` — `RailCapabilities`. */
    public fun capabilities(rail: String = railId): String = delegate.capabilities(rail)

    /** `POST /v1/rails/{rail}/quote`. */
    public fun quote(payRequestJson: String, rail: String = railId): String =
        delegate.quote(rail, payRequestJson)

    /** `POST /v1/rails/{rail}/charge` — a `Receipt`. Store it. */
    public fun charge(payRequestJson: String, rail: String = railId): String =
        delegate.charge(rail, payRequestJson)

    /**
     * `POST /v1/rails/{rail}/verify` — `{"valid":true|false}`, **both HTTP
     * 200**. `false` is data, never an HTTP error, precisely so a caller
     * cannot mistake "verified false" for "the sidecar broke".
     */
    public fun verify(receiptJson: String, rail: String = railId): String =
        delegate.verify(rail, receiptJson)

    /** [verify] as a `Boolean`, fail-closed on anything unrecognised. */
    public fun isValid(receiptJson: String, rail: String = railId): Boolean =
        verify(receiptJson, rail).trim() == "{\"valid\":true}"

    /**
     * `POST /v1/rails/{rail}/validate-destination` — the offline pre-flight
     * check. All five verdicts are `200`: a `200` means the rail **answered**,
     * not that the address is good.
     */
    public fun validateDestination(destination: String, rail: String = railId): String =
        delegate.validateDestination(rail, destination)

    /**
     * `POST /v1/rails/{rail}/webhook` — forward the processor's delivery
     * **verbatim**: the exact bytes it sent, and its headers.
     *
     * Every webhook scheme signs the bytes that arrived, so a body that has
     * been through a JSON round-trip on your side is no longer what was
     * signed, and every genuine delivery will fail to authenticate. A rail
     * with no push delivery — the mock — answers 501, which arrives as an
     * exception rather than as an invented event.
     */
    public fun webhook(
        rawBody: ByteArray,
        headers: Map<String, String> = emptyMap(),
        rail: String = railId,
    ): String = delegate.webhook(rail, rawBody, headers)

    /** One raw authenticated GET, for anything this wrapper does not name. */
    public fun get(path: String): String = delegate.get(path)

    /** One raw authenticated POST of a JSON body. */
    public fun post(path: String, body: String): String = delegate.post(path, body)

    /** Stop the child process. Idempotent; `use {}` calls it on every path out. */
    override fun close(): Unit = delegate.close()
}
