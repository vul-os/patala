@file:JvmName("PatalaDirectKt")

package org.vulos.patala.kotlin

import java.nio.file.Path
import org.vulos.patala.Json
import org.vulos.patala.PatalaDirect

/**
 * Kotlin over [org.vulos.patala.PatalaDirect] — patala running **in this JVM**
 * through `libpatala_ffi`'s C ABI.
 *
 * A thin, idiomatic layer rather than a second binding: the FFM calls, the
 * memory rules and the handle lifecycle stay in the Java class, because two
 * bindings to one C ABI is two places for a use-after-free.
 *
 * **On the JVM this is the recommended default** — the opposite of what
 * llmux's and openrate's Kotlin SDKs say, and for a reason that was measured
 * rather than inherited. See `README.md`.
 *
 * ```kotlin
 * Patala.mock(feeMinor = 25).use { rail ->
 *     val req = payRequest(1250, "USDC", "mock:wallet:alice", "order-4711")
 *     val receipt = rail.charge(req)
 *     check(rail.isValid(receipt))     // the entitlement check
 * }
 * ```
 *
 * ### No coroutines, and no `Flow`
 *
 * llmux's Kotlin SDK depends on `kotlinx-coroutines-core` because chat
 * streaming genuinely needs `Flow`. **patala has no streaming** — there is no
 * `patala_stream`, because a quote, a charge, a verification and a destination
 * check are each one question with one answer — so every operation here is one
 * call that returns one document. Putting `suspend` in front of them would buy
 * a jar in everybody's build in exchange for a keyword.
 *
 * This SDK depends on nothing but `kotlin-stdlib`. If you are calling from a
 * coroutine, wrap the call in `Dispatchers.IO` at the call site: one line, no
 * dependency. That matters more here than in openrate, because a handle owns a
 * *current-thread* Tokio runtime — the work genuinely happens on your calling
 * thread, so a call from a dispatcher that must not block should be moved
 * deliberately rather than by a `suspend` that hides where the blocking went.
 */
public class PatalaRail internal constructor(
    private val delegate: PatalaDirect,
) : AutoCloseable {

    /** The patala version the loaded shared library was built from. */
    public val abiVersion: String get() = delegate.abiVersion()

    /** The library this rail is running out of. */
    public val libraryPath: Path get() = delegate.libraryPath()

    /** False once [close] has run. */
    public val isOpen: Boolean get() = delegate.isOpen

    /**
     * Ask the library to compare its version against the one this SDK was
     * written for, through `patala_abi_check` rather than by comparing strings
     * here — so the comparison is not reimplemented, and forgotten, per
     * binding.
     */
    public fun abiCheck(expected: String = PatalaDirect.VERSION): Unit = delegate.abiCheck(expected)

    /** Run any method: see [PatalaDirect.METHODS]. */
    public fun call(method: String, requestJson: String? = null): String =
        delegate.call(method, requestJson)

    /** `{"rail_id":"mock"}`. */
    public fun id(): String = delegate.id()

    /**
     * `RailCapabilities` — how to decide your whole UX without knowing which
     * provider answered. A `CustodialReversible` rail means a card form and a
     * refundable pending state; a `NonCustodialFinal` rail means a wallet
     * address and a signed final receipt. It is not a bool, because those are
     * not two shades of one thing.
     */
    public fun capabilities(): String = delegate.capabilities()

    /** A `Quote` for a [payRequest]. Opens no socket on a mock rail. */
    public fun quote(payRequestJson: String): String = delegate.quote(payRequestJson)

    /**
     * A `Receipt`. **Store it.** Handing it back to [verify] later and getting
     * `true` is the entitlement check; this call returning without throwing is
     * not.
     */
    public fun charge(payRequestJson: String): String = delegate.charge(payRequestJson)

    /** `{"valid":true|false}` for a receipt. See [isValid]. */
    public fun verify(receiptJson: String): String = delegate.verify(receiptJson)

    /**
     * [verify] as a `Boolean`, decided by an exact match on the library's own
     * two possible answers.
     *
     * **Fails closed twice over.** `false` is patala's honest verdict that a
     * receipt does not hold, not a transient failure to retry — and anything
     * this function does not recognise is also `false`, so a future third
     * answer cannot be read as "valid" by a caller who has not been updated.
     */
    public fun isValid(receiptJson: String): Boolean = verify(receiptJson).trim() == "{\"valid\":true}"

    /**
     * The offline pre-flight check to run **before** any money moves.
     *
     * It never fails: "I cannot check this" is the verdict `"Unknown"`, not an
     * error, because a caller must handle it as carefully as a refusal and an
     * error is too easy to swallow. See [isRefusal] and the caveat below.
     */
    public fun validateDestination(destination: String): String =
        delegate.validateDestination(destination)

    /**
     * `true` when the verdict says **do not send**.
     *
     * Read from the document's own `is_refusal` field rather than re-derived
     * from `status`: a re-derivation falls through to its default for any
     * status added later, and that default is "not a refusal" — failing open,
     * on the one question in this API where failing open means losing money.
     */
    public fun isRefusal(verdictJson: String): Boolean =
        Json.field(verdictJson, "is_refusal") == "true"

    /**
     * The sentence to show the human who is being asked for a payout address,
     * before there is a verdict to render.
     *
     * Every verdict — **including `StructurallyValid`** — carries
     * `human_must_confirm: true` and this same text, because patala does not
     * detect exchange-owned addresses and will not guess.
     */
    public fun caveat(): String = delegate.caveat()

    /** Release the rail. Idempotent; `use {}` calls it on every path out. */
    override fun close(): Unit = delegate.close()
}

/** Entry points for the direct path. */
public object Patala {

    /**
     * Open a rail from a configuration document.
     *
     * Creating one talks to nothing: no socket, no thread, no environment
     * variable. Unknown fields are **refused** — a misspelled `"currencys"` is
     * an error, not a rail quietly built with a currency list you did not
     * choose.
     */
    public fun rail(configJson: String? = null, library: Path? = null): PatalaRail =
        PatalaRail(
            if (library == null) PatalaDirect.open(configJson)
            else PatalaDirect.open(library, configJson),
        )

    /**
     * The offline `MockRail` — deterministic, no credentials, no network.
     *
     * @param destinationChecks pass `false` for a rail that answers `Unknown`
     *   to every destination. That is the offline stand-in for a fiat rail,
     *   whose destination is an opaque processor-side token, and it exists so
     *   the branch of your payout UI that matters most — "a human must
     *   decide" — is reachable without compiling in a real rail.
     */
    public fun mock(
        id: String = "mock",
        railClass: String = "non-custodial-final",
        currencies: List<String> = listOf("USDC"),
        feeMinor: Long = 0,
        failing: Boolean = false,
        destinationChecks: Boolean = true,
        library: Path? = null,
    ): PatalaRail {
        val config = buildString {
            append("{\"rail\":\"mock\"")
            append(",\"id\":").append(Json.quote(id))
            append(",\"class\":").append(Json.quote(railClass))
            append(",\"currencies\":[")
            currencies.forEachIndexed { i, c ->
                if (i > 0) append(',')
                append(Json.quote(c))
            }
            append("]")
            append(",\"fee_minor\":").append(feeMinor)
            append(",\"failing\":").append(failing)
            append(",\"destination_checks\":").append(destinationChecks)
            append('}')
        }
        return rail(config, library)
    }

    /** Where the direct path would load its library from, without loading it. */
    public fun findLibrary(): Path = PatalaDirect.findLibrary()
}

/**
 * Build a `PayRequest` document.
 *
 * [amountMinor] is an **integer number of minor units** — 1250 is
 * USDC 0.01250, or ZAR 12.50 — and it is a `Long` rather than a `Double` on
 * purpose. patala never puts a float on either side of the boundary, and a
 * Kotlin helper that took a `Double` would be the place the rounding bug got
 * in.
 *
 * The same reasoning is why this returns a `String` rather than taking a
 * data class through a JSON library: your JSON library's default number
 * handling is not this SDK's business, and a `Receipt` decoded with
 * `amount_minor` as a `Double` is a payments bug that type-checks.
 */
public fun payRequest(
    amountMinor: Long,
    currency: String,
    destination: String,
    reference: String,
): String {
    require(amountMinor >= 0) { "amount_minor is unsigned in patala; got $amountMinor" }
    return "{\"amount_minor\":$amountMinor" +
        ",\"currency\":${Json.quote(currency)}" +
        ",\"destination\":${Json.quote(destination)}" +
        ",\"reference\":${Json.quote(reference)}}"
}
