import org.vulos.patala.Json
import org.vulos.patala.kotlin.Patala
import org.vulos.patala.kotlin.PatalaSidecar
import org.vulos.patala.kotlin.payRequest
import uniffi.patala.DestinationStatus
import uniffi.patala.PatalaException
import uniffi.patala.RailClass
import uniffi.patala.Receipt
import uniffi.patala.Settlement
import uniffi.patala.WebhookDelivery
import uniffi.patala.WebhookStatus

/**
 * The Kotlin SDK's assertions, as an ordinary program.
 *
 * **There is no `kotlin.test` here and no Gradle to run it with.** Nothing in
 * this repo runs Gradle, so a test source set would be a file nobody had ever
 * seen pass. This is the same shape as the Swift package's
 * `Sources/patala-checks` and `patala-ffi/ctest/smoke.c`: a main() that counts
 * its own checks and **asserts how many ran**, so a suite that silently stops
 * executing half of them is a failure rather than a pass.
 *
 * What it is checking is the thing this SDK changed: the surface is generated
 * UniFFI, so the money-shaped values are typed. Every check below is one that
 * could not be written at all against a JSON-string binding, or that a JSON
 * binding could only answer by re-parsing a document.
 *
 * ```
 * sdks/kotlin/run-examples.sh checks     # or: make -C sdks/kotlin checks
 * ```
 */
private const val ALICE = "mock:wallet:alice"
private const val EXPECTED_CHECKS = 42

private var ran = 0
private var failed = 0

private fun check(name: String, condition: Boolean) {
    ran += 1
    if (condition) {
        println("  ok   $name")
    } else {
        failed += 1
        println("  FAIL $name")
    }
}

fun main() {
    val library = Patala.findLibrary()
    Patala.useLibrary(library)
    println("patala Kotlin checks (generated UniFFI bindings)")
    println("  library: $library")

    // ---- the type system itself -------------------------------------------
    // Enum arity is pinned here rather than described in a README: a variant
    // added or removed upstream changes what an exhaustive `when` in a
    // consumer must handle, and that should break loudly at this boundary.
    check("RailClass has exactly 2 variants", RailClass.entries.size == 2)
    check("DestinationStatus has exactly 5 variants", DestinationStatus.entries.size == 5)
    check("WebhookStatus has exactly 3 variants", WebhookStatus.entries.size == 3)
    check(
        "UNCONFIRMED is a distinct WebhookStatus, never SETTLED",
        WebhookStatus.UNCONFIRMED != WebhookStatus.SETTLED,
    )

    Patala.mock(feeMinor = 25).use { rail ->
        check("id() is the configured rail id", rail.id() == "mock")

        val caps = rail.capabilities()
        check("capabilities().railClass is a RailClass", caps.railClass == RailClass.NON_CUSTODIAL_FINAL)
        check("a non-custodial rail is not reversible", !caps.reversible)
        check("a non-custodial rail holds no funds", !caps.holdsFunds)
        check("settlement is the sealed Settlement type", caps.settlement is Settlement.Instant)
        check("currencies is a List<String>", caps.currencies == listOf("USDC"))

        // ---- destination pre-flight ---------------------------------------
        val good = rail.validateDestination(ALICE)
        check("a well-formed address is STRUCTURALLY_VALID", good.status == DestinationStatus.STRUCTURALLY_VALID)
        check("STRUCTURALLY_VALID is not a refusal", !good.isRefusal)
        check("STRUCTURALLY_VALID still requires a human", good.humanMustConfirm)
        check("every verdict carries the exchange caveat", good.exchangeDepositCaveat == Patala.caveat())

        val wrongNetwork = rail.validateDestination("eth:wallet:alice")
        check("another network's address is WRONG_NETWORK", wrongNetwork.status == DestinationStatus.WRONG_NETWORK)
        check("WRONG_NETWORK is a refusal", wrongNetwork.isRefusal)

        val empty = rail.validateDestination("")
        check("an empty destination is MALFORMED", empty.status == DestinationStatus.MALFORMED)
        check("MALFORMED is a refusal", empty.isRefusal)

        // ---- quote --------------------------------------------------------
        val request = payRequest(1250, "USDC", ALICE, "checks-1")
        val quote = rail.quote(request)
        check("quote totals in integer minor units", quote.totalMinor == quote.amountMinor + quote.feeMinor)
        check("the fee is the configured one", quote.feeMinor == 25uL)

        // ---- charge -> verify ---------------------------------------------
        val receipt = rail.charge(request)
        check("a fresh receipt verifies", rail.verify(receipt))
        check("the receipt carries the amount that was charged", receipt.amountMinor == 1250uL)
        check("the receipt is signed", receipt.proof.size == 32)

        // Fail-closed, field by field. Each of these is a `copy` on a data
        // class: the mutation a real bug would make, not a mangled string.
        val tampers: List<Pair<String, Receipt>> = listOf(
            "amount" to receipt.copy(amountMinor = 125_000uL),
            "currency" to receipt.copy(currency = "USD"),
            "reference" to receipt.copy(reference = "someone-elses-order"),
            "proof" to receipt.copy(proof = ByteArray(32)),
        )
        for ((field, tampered) in tampers) {
            check("verify() is false for a tampered $field", !rail.verify(tampered))
        }

        // ---- the refusals -------------------------------------------------
        val unsupported = try {
            rail.verifyWebhook(WebhookDelivery(ByteArray(0), emptyMap(), null, 1_700_000_000uL))
            null
        } catch (e: PatalaException) {
            e
        }
        check(
            "the mock rail reports webhook verification Unsupported",
            unsupported is PatalaException.Unsupported && unsupported.operation == "verify_webhook",
        )

        val badCurrency = try {
            rail.charge(payRequest(1, "EUR", ALICE, "checks-2"))
            null
        } catch (e: PatalaException) {
            e
        }
        check(
            "an unsupported currency is InvalidRequest, with a detail",
            badCurrency is PatalaException.InvalidRequest && badCurrency.detail.isNotEmpty(),
        )
    }

    // ---- the rail that cannot check a destination -------------------------
    Patala.mock(destinationChecks = false).use { opaque ->
        val verdict = opaque.validateDestination(ALICE)
        check("a rail that cannot check answers UNKNOWN", verdict.status == DestinationStatus.UNKNOWN)
        check("UNKNOWN is not a refusal — it needs a human", !verdict.isRefusal)
        check("UNKNOWN still requires a human", verdict.humanMustConfirm)
    }

    // ---- the sidecar client makes no money decision of its own ------------
    //
    // PatalaSidecar used to carry
    // `isRefusal(json) = Json.field(json, "is_refusal") == "true"`, over a
    // substring scan that did not skip the whitespace after the colon — so a
    // verdict reformatted anywhere between patala and here read as `" true"`
    // and a MALFORMED verdict came back "not a refusal". Failing OPEN, on the
    // one question in this API where failing open loses money.
    //
    // The direct path had already deleted the identical function as a defect;
    // the sidecar path was never migrated. Re-add it and the first check here
    // reports: `FAIL PatalaSidecar exposes no isRefusal(String) -- a JSON
    // scan must not decide whether to send money`.
    val sidecarDecisions =
        PatalaSidecar::class.java.methods.filter { it.name.lowercase().contains("refusal") }
    check(
        "PatalaSidecar exposes no isRefusal(String) — a JSON scan must not " +
            "decide whether to send money (found: ${sidecarDecisions.map { it.name }})",
        sidecarDecisions.isEmpty(),
    )

    // ---- Json.field: a printer, and it must print the truth ---------------
    //
    // Still a scan, still documented for printing only — but a printer that
    // prints the wrong thing is still wrong, and this is the scan the deleted
    // helper was built on. Every form below is the SAME JSON document.
    val compact = """{"status":"Malformed","is_refusal":true,"human_must_confirm":true}"""
    val spaced = """{"status": "Malformed", "is_refusal": true, "human_must_confirm": true}"""
    val indented = "{\n  \"status\": \"Malformed\",\n  \"is_refusal\": true\n}"
    val beforeColon = """{"status" : "Malformed", "is_refusal" : true}"""

    check("compact: status", Json.field(compact, "status") == "Malformed")
    check("compact: is_refusal", Json.field(compact, "is_refusal") == "true")
    check("a space after the colon: status", Json.field(spaced, "status") == "Malformed")
    check("a space after the colon: is_refusal", Json.field(spaced, "is_refusal") == "true")
    check("pretty-printed over newlines: is_refusal", Json.field(indented, "is_refusal") == "true")
    check("a space BEFORE the colon too: is_refusal", Json.field(beforeColon, "is_refusal") == "true")
    check("an absent key is null", Json.field(compact, "nope") == null)

    // ---- the guards this SDK adds on top of the generated code ------------
    val negative = try {
        payRequest(-1, "USDC", ALICE, "checks-3")
        false
    } catch (e: IllegalArgumentException) {
        true
    }
    check("payRequest refuses a negative amount rather than wrapping it", negative)

    val closed = Patala.mock()
    closed.close()
    closed.close()
    val afterClose = try {
        closed.id()
        false
    } catch (e: IllegalStateException) {
        true
    }
    check("use-after-close is an error, not a crash", afterClose)

    println()
    println("$ran checks ran, $failed failed (expected $EXPECTED_CHECKS)")
    if (ran != EXPECTED_CHECKS) {
        println("FAIL: $ran checks ran, expected $EXPECTED_CHECKS")
        kotlin.system.exitProcess(1)
    }
    if (failed != 0) {
        println("FAIL: $failed check(s) failed")
        kotlin.system.exitProcess(1)
    }
    println("PASS")
}
