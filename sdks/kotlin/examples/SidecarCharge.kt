import org.vulos.patala.Json
import org.vulos.patala.PatalaException
import org.vulos.patala.kotlin.PatalaSidecar
import org.vulos.patala.kotlin.payRequest
import org.vulos.patala.kotlin.toJson

/**
 * patala as a child process on `127.0.0.1` — the same charge -> verify round
 * trip as `DirectCharge.kt`, over HTTP.
 *
 * **MockRail, deliberately**, and here it is not even a choice:
 * `patala-sidecar`'s default registry contains exactly one rail and it is the
 * offline mock. Nothing here opens a socket to anywhere but loopback.
 *
 * ```
 * sdks/kotlin/run-examples.sh sidecar
 * ```
 */
private const val ALICE = "mock:wallet:alice"

fun main() {
    // No token passed: 32 bytes from SecureRandom are minted and handed to the
    // child. The server refuses to start without one — there is no
    // unauthenticated mode to fall into.
    PatalaSidecar().use { patala ->
        println("sidecar:  ${patala.baseUrl} (loopback, hardcoded in the server)")
        println("healthz:  ${patala.healthz()}   <- the one unauthenticated route")
        println("caps:     ${patala.capabilities()}")

        println()
        println("-- destination pre-flight (POST /v1/rails/mock/validate-destination) --")
        for (candidate in listOf(ALICE, "eth:wallet:alice", "")) {
            val verdict = patala.validateDestination(candidate)
            val shown = if (candidate.isEmpty()) "\"\" (empty)" else "\"$candidate\""
            println("  $shown -> ${Json.field(verdict, "status")}, isRefusal=${patala.isRefusal(verdict)}")
        }
        println("  all five verdicts are HTTP 200. A 200 means the rail ANSWERED,")
        println("  not that the address is good — read the body, not the status.")

        // The same typed PayRequest the direct path takes, serialised at the
        // wire boundary and nowhere earlier.
        val request = payRequest(1250, "USDC", ALICE, "order-4711").toJson()

        println()
        println("-- quote -> charge -> verify --")
        println("  quote:   ${patala.quote(request)}")

        val receipt = patala.charge(request)
        println("  receipt: $receipt")

        check(patala.isValid(receipt)) { "a fresh receipt must verify" }
        println("  isValid(receipt):  ${patala.isValid(receipt)}   <- 200; this, not the charge, is entitlement")

        val tampered = receipt.replace("\"amount_minor\":1250", "\"amount_minor\":125000")
        check(tampered != receipt) { "the tamper did not apply" }
        check(!patala.isValid(tampered)) { "a tampered receipt must NOT verify" }
        println("  isValid(tampered): ${patala.isValid(tampered)}   <- also 200. false is data, not an HTTP error.")

        println()
        println("-- what this server refuses to pretend --")
        println(
            "  webhook (mock has no push delivery): " +
                failure {
                    patala.webhook(
                        "{}".toByteArray(),
                        mapOf("Stripe-Signature" to "t=1,v1=deadbeef"),
                    )
                },
        )
        println("  an unregistered rail: ${failure { patala.capabilities("stellar") }}")
        println("    ^ 404 because this process has never heard of it. The default")
        println("      registry is mock-only; per-rail registration is unwritten.")

        // The token gate, exercised rather than described: same server, same
        // route, a token that is not the one it was started with.
        PatalaSidecar.attach(patala.baseUrl, "0".repeat(64)).use { wrong ->
            println("  a wrong bearer token: ${failure { wrong.capabilities() }}")
        }
    }

    println()
    println("SidecarCharge: OK — child process stopped.")
}

private fun failure(block: () -> String): String =
    try {
        "UNEXPECTED SUCCESS: ${block()}"
    } catch (e: PatalaException) {
        e.message ?: "(no message)"
    }
