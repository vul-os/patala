@file:JvmName("PatalaDirectKt")

package org.vulos.patala.kotlin

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import uniffi.patala.PatalaRail
import uniffi.patala.PayRequest
import uniffi.patala.RailCapabilities
import uniffi.patala.RailClass
import uniffi.patala.Settlement
import uniffi.patala.exchangeDepositCaveat

/**
 * Kotlin over the **generated UniFFI bindings** in `uniffi.patala` — patala
 * running in this JVM, with real types.
 *
 * `uniffi.patala.PatalaRail` is the rail itself: generated from
 * `patala-uniffi`'s one `#[uniffi::export]` surface, the same surface
 * `patala-py` and `patala-go` are generated from. Its methods take and return
 * [PayRequest], `Quote`, `Receipt`, `DestinationVerdict`, [RailCapabilities]
 * and `WebhookEvent` — Kotlin data classes and enums, not JSON strings — and
 * it throws `PatalaException`, a sealed class you can `when` over.
 *
 * ```kotlin
 * Patala.mock(feeMinor = 25).use { rail ->
 *     val verdict = rail.validateDestination("mock:wallet:alice")
 *     if (verdict.isRefusal) return                       // a Boolean field, not a parsed one
 *
 *     val receipt = rail.charge(payRequest(1250, "USDC", "mock:wallet:alice", "order-4711"))
 *     check(rail.verify(receipt))                          // the entitlement check
 * }
 * ```
 *
 * **What this file adds, and deliberately no more.** Everything money-shaped
 * lives in the generated code, so this layer is defaults and spelling:
 *
 * - [Patala.mock] — the five-argument generated constructor with Kotlin
 *   default arguments, and a `destinationChecks` flag that picks between the
 *   two generated constructors.
 * - [payRequest] — a `Long` amount checked to be non-negative before it
 *   becomes the `ULong` the record actually holds.
 * - [railClass] and [describe] — two spelling conveniences over generated
 *   names Kotlin cannot say comfortably (`caps.\`class\``) or print usefully
 *   (a sealed `Settlement`).
 * - [Patala.useLibrary] / [Patala.findLibrary] — where the cdylib is loaded
 *   from, which is a build concern rather than an API one.
 *
 * There is **no `isValid(json)` and no `isRefusal(json)` here any more**.
 * `rail.verify(receipt)` already returns a `Boolean` decided inside Rust, and
 * `verdict.isRefusal` is a field of the record. Both used to be Kotlin
 * functions that parsed a JSON document to recover something the Rust type
 * already knew; deleting them is most of the reason this SDK is generated now.
 *
 * ### No coroutines, and no `Flow`
 *
 * patala has no streaming operation — a quote, a charge, a verification and a
 * destination check are each one question with one answer — so every call
 * here returns one value. This SDK depends on `kotlin-stdlib` and JNA (which
 * the generated bindings require) and nothing else. If you are calling from a
 * coroutine, wrap the call in `Dispatchers.IO` at the call site: one line, no
 * dependency, and it keeps the blocking visible where it happens.
 */
public object Patala {

    /** The JNA/UniFFI component name — `uniffi::setup_scaffolding!("patala")`. */
    public const val COMPONENT: String = "patala"

    /** The cdylib the generated bindings load: `libpatala_uniffi.{dylib,so}`. */
    public const val LIBRARY_STEM: String = "patala_uniffi"

    /**
     * Point the generated bindings at a specific cdylib file.
     *
     * The generated code calls `Native.load("patala_uniffi")`, which searches
     * `jna.library.path`, `java.library.path` and the OS loader path. That is
     * the right default for a packaged application and the wrong one in a
     * checkout, where the library is under `target/`. This sets UniFFI's own
     * `uniffi.component.patala.libraryOverride` system property to an absolute
     * path.
     *
     * **Call it before the first rail is created.** The library is loaded once,
     * lazily, on first use; setting the override afterwards changes nothing and
     * says nothing, which is why this throws rather than returning a status:
     * an ignored override is exactly how a process ends up talking to a stale
     * library it did not mean to load.
     */
    @JvmStatic
    public fun useLibrary(library: Path) {
        require(Files.isRegularFile(library)) { "no patala cdylib at $library" }
        System.setProperty(
            "uniffi.component.$COMPONENT.libraryOverride",
            library.toAbsolutePath().toString(),
        )
    }

    /**
     * Locate `libpatala_uniffi` in a checkout without loading it.
     *
     * Resolution order, matching the Swift package's: `$PATALA_LIBRARY`, then
     * `target/release/` and `target/debug/` walking up from the working
     * directory. Throws when there is nothing to find — a payments process
     * should fail at startup rather than at the first charge.
     */
    @JvmStatic
    public fun findLibrary(): Path {
        System.getenv("PATALA_LIBRARY")?.let { explicit ->
            val path = Paths.get(explicit)
            require(Files.isRegularFile(path)) { "PATALA_LIBRARY=$explicit does not exist" }
            return path
        }
        val ext = if (System.getProperty("os.name").startsWith("Mac")) "dylib" else "so"
        val file = "lib$LIBRARY_STEM.$ext"
        val tried = ArrayList<Path>()
        var dir: Path? = Paths.get("").toAbsolutePath()
        while (dir != null) {
            for (profile in listOf("release", "debug")) {
                val candidate: Path = dir.resolve("target").resolve(profile).resolve(file)
                tried.add(candidate)
                if (Files.isRegularFile(candidate)) return candidate
            }
            dir = dir.parent
        }
        throw IllegalStateException(
            "could not find $file. Tried:\n" + tried.joinToString("\n") { "  $it" } +
                "\nBuild it with: cargo build -p patala-uniffi --release",
        )
    }

    /**
     * The sentence to show a human who is being asked for a payout address.
     *
     * It is also on every [uniffi.patala.DestinationVerdict], including a
     * `STRUCTURALLY_VALID` one, because patala does not detect exchange-owned
     * addresses and will not guess.
     */
    @JvmStatic
    public fun caveat(): String = exchangeDepositCaveat()

    /**
     * The offline `MockRail` — deterministic, no credentials, no network.
     *
     * Creating one talks to nothing: no socket, no thread, no environment
     * variable. `use {}` releases it on every path out (the generated class is
     * `AutoCloseable`; it is also registered with a `Cleaner`, so a dropped
     * rail is freed eventually either way).
     *
     * @param destinationChecks pass `false` for a rail that answers
     *   `DestinationStatus.UNKNOWN` to every destination. That is the offline
     *   stand-in for a fiat rail, whose destination is an opaque
     *   processor-side token, and it exists so the branch of a payout UI that
     *   matters most — "a human must decide" — is reachable in the default
     *   build.
     */
    @JvmStatic
    @JvmOverloads
    public fun mock(
        id: String = "mock",
        railClass: RailClass = RailClass.NON_CUSTODIAL_FINAL,
        currencies: List<String> = listOf("USDC"),
        feeMinor: Long = 0,
        failing: Boolean = false,
        destinationChecks: Boolean = true,
    ): PatalaRail {
        require(feeMinor >= 0) { "fee_minor is unsigned in patala; got $feeMinor" }
        return if (destinationChecks) {
            PatalaRail.newMock(id, railClass, currencies, feeMinor.toULong(), failing)
        } else {
            PatalaRail.newMockWithoutDestinationChecks(
                id,
                railClass,
                currencies,
                feeMinor.toULong(),
                failing,
            )
        }
    }
}

/**
 * Build a [PayRequest].
 *
 * [amountMinor] is an **integer number of minor units** — 1250 is USDC
 * 0.01250, or ZAR 12.50. The generated record holds a `ULong`; this takes a
 * `Long` and refuses a negative one, because `(-1L).toULong()` is
 * 18446744073709551615 and a wrapped amount is a money bug that type-checks.
 *
 * There is no `Double` overload and there will not be one. patala never puts a
 * float on either side of the boundary.
 */
public fun payRequest(
    amountMinor: Long,
    currency: String,
    destination: String,
    reference: String,
): PayRequest {
    require(amountMinor >= 0) { "amount_minor is unsigned in patala; got $amountMinor" }
    return PayRequest(
        amountMinor = amountMinor.toULong(),
        currency = currency,
        destination = destination,
        reference = reference,
    )
}

/**
 * `capabilities().class` under a name Kotlin can say without backticks.
 *
 * It is an enum with exactly two members and no default branch is possible in
 * an exhaustive `when`, which is the point: a `CUSTODIAL_REVERSIBLE` rail
 * means a card form and a refundable pending state, a `NON_CUSTODIAL_FINAL`
 * rail means a wallet address and a signed final receipt, and adding a third
 * class upstream should stop your build rather than fall through.
 */
public val RailCapabilities.railClass: RailClass get() = this.`class`

/**
 * A one-line rendering of the sealed [Settlement] enum.
 *
 * Its default `toString` is a synthetic class name for the `Instant` case,
 * because that arm is an `object` rather than a `data class`. This exists so a
 * log line says `instant` instead of `Settlement$Instant@16c0663d`.
 */
public fun Settlement.describe(): String = when (this) {
    is Settlement.Instant -> "instant"
    is Settlement.Seconds -> "${this.secs}s"
    is Settlement.Days -> "${this.days}d"
}
