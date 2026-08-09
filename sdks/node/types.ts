// The JSON patala speaks, as TypeScript.
//
// These are the `patala-core` types, verbatim: the SAME shapes the
// `patala-sidecar` HTTP API serves and the same ones `patala_call` returns, so
// both modes in this package share one set and neither transport gets to drift
// from the other. Nothing here is a hand-rolled second DTO set.
//
// MONEY IS INTEGER MINOR UNITS. `amount_minor` is a whole number of the
// currency's smallest unit — cents, USDC base units — on both sides of the
// boundary, never a float. `number` is the type here only because JSON has one
// numeric type; treat every `*_minor` as an integer and do not do arithmetic
// that could produce a fraction.

/**
 * Custodial-and-reversible, or non-custodial-and-final. **Not a bool**, on
 * purpose: these are not two shades of one thing, and the difference decides
 * your whole UX. A `CustodialReversible` rail wants a card form and a
 * refundable pending state; a `NonCustodialFinal` rail wants a wallet address
 * and a signed, irreversible receipt.
 */
export type RailClass = "CustodialReversible" | "NonCustodialFinal";

/** Final at broadcast, after N seconds, or after N days (card-network T+2). */
export type Settlement = "Instant" | { Seconds: number } | { Days: number };

/** What a rail can and cannot do. Decide your UX from `class`. */
export interface RailCapabilities {
  class: RailClass;
  reversible: boolean;
  requires_kyc: boolean;
  /** patala itself never holds funds; this says whether the *rail* does. */
  holds_funds: boolean;
  currencies: string[];
  settlement: Settlement;
  atomic_multi_party: boolean;
}

/** A request to move money. `reference` is your idempotency/correlation key. */
export interface PayRequest {
  /** Integer minor units. Never a float. */
  amount_minor: number;
  /** `"USDC"`, `"USD"`, … — what a rail accepts is in its capabilities. */
  currency: string;
  /** A wallet address on a crypto rail; an opaque token on a fiat one. */
  destination: string;
  /** Echoed back on the receipt. Rails make `charge` idempotent on it. */
  reference: string;
}

/** Fees, fx and expiry. No money moves. */
export interface Quote {
  rail_id: string;
  amount_minor: number;
  currency: string;
  fee_minor: number;
  /** `amount_minor + fee_minor`, saturating. */
  total_minor: number;
  settlement: Settlement;
  expires_at_unix: number;
}

/**
 * Proof that a charge executed — **this is the entitlement**, and it is the
 * thing to store. Gate on `verify` returning `{valid: true}` later, never on
 * `charge` having returned without throwing.
 */
export interface Receipt {
  rail_id: string;
  amount_minor: number;
  currency: string;
  reference: string;
  /** Rail-specific binding blob, as JSON's only byte encoding: a number array. */
  proof: number[];
  settled_at_unix: number;
}

/**
 * `{valid: false}` is an ANSWER, not a failure. It is the rail's fail-closed
 * verdict that a receipt does not hold. Do not retry it as if it were a
 * transient error — that is how an unpaid order becomes an entitlement.
 */
export interface VerifyResult {
  valid: boolean;
}

/** `{rail_id}` — what `id` returns. */
export interface IdResult {
  rail_id: string;
}

/** **No status here means "safe to send to".** Read the docs on each. */
export type DestinationStatus =
  /** Positively defective — wrong alphabet, bad checksum, empty. Do not send. */
  | "Malformed"
  /** Well-formed, wrong network. Common and expensive. Do not send. */
  | "WrongNetwork"
  /** A real address nobody holds a key for — a contract, a PDA, a mint. Do not send. */
  | "NotAWallet"
  /** Every offline check passed. That is the absence of a defect, not safety. */
  | "StructurallyValid"
  /** This rail cannot check it and refuses to guess. A human must decide. */
  | "Unknown";

/**
 * What one rail could decide about one address, offline, plus what it could
 * not. This call NEVER fails — "I cannot check this" arrives as
 * `status: "Unknown"`, because an error is too easy to swallow.
 */
export interface DestinationVerdict {
  rail_id: string;
  status: DestinationStatus;
  reason: string;
  /**
   * `true` on EVERY verdict, including `StructurallyValid`. patala does not
   * detect exchange-owned addresses and will not guess.
   */
  human_must_confirm: boolean;
  /** The sentence to show that human. Also available on its own via `caveat`. */
  exchange_deposit_caveat: string;
  /**
   * Do not send. Derived from `status` by patala rather than by you: a `switch`
   * that has not heard of a status added later falls through to "not a
   * refusal", which fails OPEN on the one question that decides where a
   * customer's money goes.
   */
  is_refusal: boolean;
}

/** `{exchange_deposit_caveat}` — the same sentence, with no verdict attached. */
export interface CaveatResult {
  exchange_deposit_caveat: string;
}

/** `{providers}` — fiat builds only; a default build refuses by feature name. */
export interface ProvidersResult {
  providers: string[];
}

/**
 * One inbound webhook delivery, forwarded VERBATIM. Every scheme signs the
 * bytes the processor actually sent, so a body that has been through a JSON
 * round-trip on your side is no longer what was signed. Give exactly one of
 * `body` / `body_hex`.
 */
export interface WebhookDelivery {
  /** The exact request body, as text. */
  body?: string;
  /** The exact request body, hex-encoded, when it is genuinely binary. */
  body_hex?: string;
  /** Matched case-insensitively. */
  headers?: Record<string, string>;
  /** Only schemes that put their secret in the URL (LNbits) read this. */
  query?: Record<string, string>;
  /** An explicit clock for replay windows, so a delivery can be reproduced. */
  now_unix: number;
}

/**
 * An AUTHENTICATED delivery. Reaching this type means the signature checked
 * out; a forged delivery is an error, never an event with a negative status.
 */
export interface WebhookEvent {
  rail_id: string;
  event_id: string;
  reference: string;
  object_id: string;
  /**
   * Gate entitlement on `"Settled"` and nothing else. `"Unconfirmed"` means
   * genuine-but-says-nothing-about-money: take `object_id`, find your own
   * record, and call `verify`.
   */
  status: "Settled" | "NotSettled" | "Unconfirmed";
  /** Processor-reported. Reconcile against your own order before trusting it. */
  amount_minor: number;
  currency: string;
}

/**
 * The `{"rail": …}` document `patala_new` takes. Unknown fields are REFUSED,
 * not ignored: a misspelled `"currencys"` is an error rather than a rail
 * quietly built with a currency list you did not choose.
 */
export type RailConfig =
  | {
      rail: "mock";
      id?: string;
      class?: "non-custodial-final" | "custodial-reversible";
      currencies?: string[];
      fee_minor?: number;
      /** Make every operation fail, for exercising your error path. */
      failing?: boolean;
      /** `false` answers "Unknown" to every destination — a fiat rail's shape. */
      destination_checks?: boolean;
    }
  | { rail: "fiat"; provider: string; config?: Record<string, string> }
  | { rail: "solana"; rpc_url: string; cluster: "devnet" | "mainnet"; keypair_seed_hex?: string }
  | {
      rail: "stellar";
      horizon_url: string;
      network: "testnet" | "public";
      usdc_issuer?: string;
      keypair_seed_hex?: string;
    }
  | {
      rail: "hyperswitch";
      base_url: string;
      api_key: string;
      connector?: string;
      webhook_secret?: string;
      requires_kyc?: boolean;
      currencies?: string[];
      settlement_days?: number;
      timeout_secs?: number;
    };

/** Method name -> the shape that method answers with. */
export interface ResultOf {
  id: IdResult;
  capabilities: RailCapabilities;
  quote: Quote;
  charge: Receipt;
  verify: VerifyResult;
  "validate-destination": DestinationVerdict;
  webhook: WebhookEvent;
  caveat: CaveatResult;
  providers: ProvidersResult;
}

/** Every method `patala_call` accepts. A closed set — see `patala.h`. */
export const METHODS = [
  "id",
  "capabilities",
  "quote",
  "charge",
  "verify",
  "validate-destination",
  "webhook",
  "caveat",
  "providers",
] as const satisfies readonly (keyof ResultOf)[];
