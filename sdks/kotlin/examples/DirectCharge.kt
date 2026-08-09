import java.nio.file.Files
import org.vulos.patala.kotlin.Patala
import org.vulos.patala.kotlin.describe
import org.vulos.patala.kotlin.payRequest
import uniffi.patala.DestinationStatus
import uniffi.patala.PatalaException
import uniffi.patala.RailClass
import uniffi.patala.WebhookDelivery

/**
 * patala in this JVM through the generated UniFFI bindings — a full
 * charge -> verify round trip against the offline `MockRail`, in real types.
 *
 * **MockRail, deliberately.** patala is a payments library and an example that
 * moves real value is not an example. Nothing here opens a socket.
 *
 * ```
 * sdks/kotlin/run-examples.sh direct
 * ```
 */
private const val ALICE = "mock:wallet:alice"

fun main() {
    val library = Patala.findLibrary()
    Patala.useLibrary(library)
    println("library: $library")
    println("         ${Files.size(library)} bytes")
    println("loading it also checks the UniFFI contract version and every")
    println("function checksum against these bindings — a stale cdylib throws here.")

    // Creating a rail talks to nothing: no socket, no thread, no environment
    // variable. `use {}` releases it on every path out.
    Patala.mock(feeMinor = 25).use { rail ->
        println("id:           ${rail.id()}")

        val caps = rail.capabilities()
        println("capabilities: ${caps.railClass} settlement=${caps.settlement.describe()}")
        println("              reversible=${caps.reversible} holds_funds=${caps.holdsFunds}")
        println("              currencies=${caps.currencies}")

        // The class is an enum, so this `when` is exhaustive without an else.
        // A third rail class added upstream stops this build; a String compare
        // would have shipped and silently taken the wrong branch.
        when (caps.railClass) {
            RailClass.NON_CUSTODIAL_FINAL ->
                println("              -> wallet address, signed final receipt, no reversal")
            RailClass.CUSTODIAL_REVERSIBLE ->
                println("              -> card form, refundable pending state")
        }

        // -------------------------------------------- destination pre-flight
        println()
        println("-- destination pre-flight --")
        for (candidate in listOf(ALICE, "eth:wallet:alice", "")) {
            val verdict = rail.validateDestination(candidate)
            val shown = if (candidate.isEmpty()) "\"\" (empty)" else "\"$candidate\""
            println(
                "  $shown -> ${verdict.status}" +
                    ", isRefusal=${verdict.isRefusal}" +
                    ", human_must_confirm=${verdict.humanMustConfirm}",
            )
        }
        println("  human_must_confirm is true on EVERY verdict, STRUCTURALLY_VALID included.")
        println("  patala does not detect exchange-owned addresses and will not guess.")

        // A rail configured without destination checks — the offline stand-in
        // for a fiat rail, whose destination is an opaque processor-side
        // token. It exists so the "a human must decide" branch of a payout UI
        // is reachable in the default build.
        Patala.mock(destinationChecks = false).use { opaque ->
            val verdict = opaque.validateDestination(ALICE)
            check(verdict.status == DestinationStatus.UNKNOWN)
            println(
                "  the same address on a rail that cannot check: " +
                    "${verdict.status}, isRefusal=${verdict.isRefusal}",
            )
            println("  UNKNOWN is NOT a refusal and is NOT an approval. It needs a human.")
        }

        // ------------------------------------------------------- the money
        val request = payRequest(
            amountMinor = 1250, // integer minor units. Never a float, on either side.
            currency = "USDC",
            destination = ALICE,
            reference = "order-4711",
        )

        println()
        println("-- quote -> charge -> verify --")
        val quote = rail.quote(request)
        println(
            "  quote:   ${quote.amountMinor} + ${quote.feeMinor} fee = ${quote.totalMinor} " +
                "minor units of ${quote.currency}, ${quote.settlement.describe()}",
        )

        val receipt = rail.charge(request)
        println(
            "  receipt: ${receipt.amountMinor} ${receipt.currency} ref=${receipt.reference} " +
                "proof=${receipt.proof.size}B issued by ${receipt.railId}",
        )

        // THIS is the entitlement check. Not "charge returned without
        // throwing" — that only says the rail accepted the instruction.
        check(rail.verify(receipt)) { "a fresh receipt must verify" }
        println("  verify(receipt):  ${rail.verify(receipt)}")

        // `Receipt` is a data class, so tampering is one `copy` — and the
        // amount is a ULong, so this is the mistake a real bug would make
        // rather than a mangled string.
        val tampered = receipt.copy(amountMinor = 125_000uL)
        check(!rail.verify(tampered)) { "a tampered receipt must NOT verify" }
        println("  verify(tampered): ${rail.verify(tampered)}   <- returned, not thrown")

        // ------------------------------------------------ honest refusals
        println()
        println("-- what this rail refuses to pretend --")
        println("  wrong currency: ${refusal { rail.charge(payRequest(1, "EUR", ALICE, "x")) }}")
        println(
            "  webhook:        " +
                refusal {
                    rail.verifyWebhook(
                        WebhookDelivery(
                            rawBody = ByteArray(0),
                            headers = emptyMap(),
                            query = null,
                            nowUnix = 1_700_000_000uL,
                        ),
                    )
                },
        )
    }

    // A rail configured to fail every operation, for exercising your error
    // path without a real processor to break.
    Patala.mock(failing = true).use { broken ->
        println("  a failing rail: ${refusal { broken.charge(payRequest(1, "USDC", ALICE, "x")) }}")
    }

    // The generated object is AutoCloseable and use-after-close is a clean
    // IllegalStateException from the generated call counter, not a crash.
    val closed = Patala.mock()
    closed.close()
    closed.close() // idempotent
    println()
    println(
        "after close: " +
            try {
                "UNEXPECTED SUCCESS: ${closed.id()}"
            } catch (e: IllegalStateException) {
                e.message ?: "(no message)"
            },
    )

    println()
    println("DirectCharge: OK — offline, no socket opened, MockRail only.")
}

/**
 * The refusal, rendered by matching on the sealed error rather than on text.
 *
 * `PatalaException` is a sealed class, so this `when` names the cases and the
 * compiler checks it — the whole difference between a typed binding and a
 * string one, on the code path where you are deciding what a failure means.
 */
private fun refusal(block: () -> Any): String =
    try {
        "UNEXPECTED SUCCESS: ${block()}"
    } catch (e: PatalaException) {
        when (e) {
            is PatalaException.Unsupported -> "Unsupported(${e.operation}) — this rail has no such thing"
            is PatalaException.InvalidRequest -> "InvalidRequest: ${e.detail}"
            is PatalaException.Rail -> "Rail: ${e.detail}"
            is PatalaException.CrossClassFailover -> "CrossClassFailover(${e.from} -> ${e.to})"
            is PatalaException.AllRailsFailed -> "AllRailsFailed"
        }
    }
