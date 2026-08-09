import java.nio.file.Files
import org.vulos.patala.Json
import org.vulos.patala.PatalaException
import org.vulos.patala.kotlin.Patala
import org.vulos.patala.kotlin.payRequest

/**
 * patala in this JVM, through the C ABI — a full charge -> verify round trip
 * against the offline `MockRail`.
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
    println("library: $library")
    println("         ${Files.size(library)} bytes")

    // Creating a rail talks to nothing: no socket, no thread, no environment
    // variable. `use {}` closes it on every path out.
    Patala.mock(feeMinor = 25).use { rail ->
        rail.abiCheck()
        println("abi version: ${rail.abiVersion} (checked by the library, not by us)")
        println("id:           ${rail.id()}")
        println("capabilities: ${rail.capabilities()}")

        // -------------------------------------------- destination pre-flight
        println()
        println("-- destination pre-flight --")
        for (candidate in listOf(ALICE, "eth:wallet:alice", "")) {
            val verdict = rail.validateDestination(candidate)
            val shown = if (candidate.isEmpty()) "\"\" (empty)" else "\"$candidate\""
            println(
                "  $shown -> ${Json.field(verdict, "status")}" +
                    ", isRefusal=${rail.isRefusal(verdict)}" +
                    ", human_must_confirm=${Json.field(verdict, "human_must_confirm")}",
            )
        }
        println("  human_must_confirm is true on EVERY verdict, StructurallyValid included.")
        println("  patala does not detect exchange-owned addresses and will not guess.")

        // A rail configured without destination checks — the offline stand-in
        // for a fiat rail, whose destination is an opaque processor-side
        // token. It exists so the "a human must decide" branch of a payout UI
        // is reachable in the default build.
        Patala.mock(destinationChecks = false).use { opaque ->
            val verdict = opaque.validateDestination(ALICE)
            println(
                "  the same address on a rail that cannot check: " +
                    "${Json.field(verdict, "status")}, isRefusal=${opaque.isRefusal(verdict)}",
            )
            println("  Unknown is NOT a refusal and is NOT an approval. It needs a human.")
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
        println("  request: $request")
        println("  quote:   ${rail.quote(request)}")

        val receipt = rail.charge(request)
        println("  receipt: $receipt")

        // THIS is the entitlement check. Not "charge returned without
        // throwing" — that only says the rail accepted the instruction.
        check(rail.isValid(receipt)) { "a fresh receipt must verify" }
        println("  isValid(receipt):  ${rail.isValid(receipt)}")

        val tampered = receipt.replace("\"amount_minor\":1250", "\"amount_minor\":125000")
        check(tampered != receipt) { "the tamper did not apply" }
        check(!rail.isValid(tampered)) { "a tampered receipt must NOT verify" }
        println("  isValid(tampered): ${rail.isValid(tampered)}   <- an ordinary result, not an exception")

        // ------------------------------------------------ honest refusals
        println()
        println("-- what this rail refuses to pretend --")
        println("  webhook:  ${failure { rail.call("webhook", "{\"body\":\"{}\",\"headers\":{},\"now_unix\":1700000000}") }}")
        println("  unknown:  ${failure { rail.call("settle-later") }}")
    }

    // A rail configured to fail every operation, for exercising your error
    // path without a real processor to break.
    Patala.mock(failing = true).use { broken ->
        println("  a failing rail: ${failure { broken.charge(payRequest(1, "USDC", ALICE, "x")) }}")
    }

    // Handles are integers in a registry inside the library, retired rather
    // than recycled — a stale handle can never land on somebody else's rail.
    val closed = Patala.mock()
    closed.close()
    closed.close() // idempotent
    println()
    println("after close: ${failure { closed.id() }}")

    println()
    println("DirectCharge: OK — offline, no socket opened, no thread started.")
}

/** The library's own message for a call it refuses. */
private fun failure(block: () -> String): String =
    try {
        "UNEXPECTED SUCCESS: ${block()}"
    } catch (e: PatalaException) {
        e.message ?: "(no message)"
    }
