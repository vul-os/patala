//! patala from Rust, **sidecar** — `patala-sidecar` as a child process on
//! loopback, driven over HTTP.
//!
//! Rust does not need this to *reach* patala; `examples/direct.rs` shows the
//! whole substrate as ordinary types. It needs it for one property direct mode
//! cannot give: a non-custodial rail's signing key lives in exactly one narrow
//! process instead of in every process that links the crate. If your Rust
//! service is one of five things that take payments, that is the reason to
//! pay the JSON tax.
//!
//! Run it:
//!
//! ```text
//! cd sdks/rust && cargo run --example sidecar
//! ```
//!
//! It builds nothing: point `PATALA_SIDECAR_BIN` at a binary, or let it find
//! `target/{release,debug}/patala-sidecar` in this checkout.
//!
//! `MockRail` throughout — the sidecar's registry is mock-only anyway (see
//! `patala-sidecar/src/registry.rs`), and this is a payments library.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use patala_core::{DestinationStatus, RailCapabilities, Receipt};

fn main() {
    let bin = sidecar_binary();
    let port = free_port();
    // A real deployment generates this once and keeps it out of the
    // environment of everything that is not the sidecar or its client. The
    // server refuses to start without it — there is no unauthenticated mode.
    let token = random_hex_32();

    println!("patala sidecar — child process on 127.0.0.1:{port}");
    println!("binary:    {}", bin.display());

    let child = Command::new(&bin)
        .env("PATALA_SIDECAR_PORT", port.to_string())
        .env("PATALA_SIDECAR_TOKEN", &token)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| die(&format!("spawning {}: {e}", bin.display())));
    // Kill it on every path out of main, including a panic unwind. In Rust
    // the child guard is a `Drop` impl; the C example needs an explicit
    // cleanup label and a signal handler to get the same thing.
    let _guard = Reaper(child);

    let sc = Sidecar { port, token };
    sc.wait_healthy();

    // ------------------------------------------------- the token is a gate
    // Every /v1 route is behind it, including the read-only one. A missing
    // header, a malformed header and a wrong token are the same 401.
    let anon = Sidecar {
        port,
        token: String::new(),
    };
    let (status, _) = anon.get("/v1/rails/mock");
    println!("no token:  HTTP {status}");
    assert_eq!(status, 401);

    // ------------------------------------------------------- capabilities
    let (status, body) = sc.get("/v1/rails/mock");
    assert_eq!(status, 200, "{body}");
    // The sidecar serializes `patala_core`'s own types, so they deserialize
    // straight back into them. There is no second DTO set to drift.
    let caps: RailCapabilities = serde_json::from_str(&body).expect("RailCapabilities");
    println!(
        "caps:      {:?} settlement={:?} holds_funds={} currencies={:?}",
        caps.class, caps.settlement, caps.holds_funds, caps.currencies
    );

    // ------------------------------------------------- charge -> verify
    let pay = r#"{"amount_minor":1250,"currency":"USDC","destination":"mock:wallet:alice","reference":"order-1"}"#;

    let (status, body) = sc.post("/v1/rails/mock/quote", pay);
    assert_eq!(status, 200, "{body}");
    let total = json_u64(&body, "total_minor").expect("total_minor");
    println!("quote:     total_minor={total} (an integer on the wire, never a float)");

    let t0 = Instant::now();
    let (status, body) = sc.post("/v1/rails/mock/charge", pay);
    let charged_in = t0.elapsed();
    assert_eq!(status, 200, "{body}");
    let receipt: Receipt = serde_json::from_str(&body).expect("Receipt");
    println!(
        "charge:    {} {} ref={} rail={}  [{charged_in:?} incl. loopback]",
        receipt.amount_minor, receipt.currency, receipt.reference, receipt.rail_id
    );

    let (status, body) = sc.post(
        "/v1/rails/mock/verify",
        &serde_json::to_string(&receipt).unwrap(),
    );
    assert_eq!(status, 200, "{body}");
    println!("verify:    HTTP 200 {body}");
    assert_eq!(body.trim(), r#"{"valid":true}"#);

    // A tampered receipt is HTTP **200** with `{"valid":false}`. Read the
    // body, not the status code: a rail's honest refusal is data, and
    // mapping it onto an HTTP error is how an unpaid order becomes an
    // entitlement the day someone adds a retry on 4xx.
    let mut tampered = receipt.clone();
    tampered.amount_minor = 999_999;
    let (status, body) = sc.post(
        "/v1/rails/mock/verify",
        &serde_json::to_string(&tampered).unwrap(),
    );
    println!("tampered:  HTTP {status} {body}  <- 200, and false");
    assert_eq!((status, body.trim()), (200, r#"{"valid":false}"#));

    // ------------------------------------------------- destination check
    let (status, body) = sc.post(
        "/v1/rails/mock/validate-destination",
        r#"{"destination":"stellar:wallet:alice"}"#,
    );
    assert_eq!(status, 200, "{body}");
    let verdict: serde_json::Value = serde_json::from_str(&body).unwrap();
    println!(
        "dest:      HTTP 200 status={} is_refusal={} human_must_confirm={}",
        verdict["status"], verdict["is_refusal"], verdict["human_must_confirm"]
    );
    // `is_refusal` is on the wire because it is a *method* on the Rust type
    // and a method does not survive JSON. Re-deriving it from `status` fails
    // open for any status added later.
    assert_eq!(verdict["status"], "WrongNetwork");
    assert_eq!(verdict["is_refusal"], true);
    assert_eq!(
        format!("{:?}", DestinationStatus::WrongNetwork),
        verdict["status"].as_str().unwrap()
    );

    // A malformed REQUEST is a 400 and carries no verdict at all, so a
    // rejected request can never be mistaken for a checked address.
    let (status, _) = sc.post(
        "/v1/rails/mock/validate-destination",
        r#"{"destinaton":"typo"}"#,
    );
    println!("typo:      HTTP {status} — a bad request is not a verdict");
    assert_eq!(status, 400);

    // ------------------------------------------------------- the edges
    let (status, _) = sc.get("/v1/rails/solana");
    println!("no rail:   HTTP {status} — the registry is mock-only");
    assert_eq!(status, 404);

    let (status, _) = sc.post("/v1/rails/mock/webhook", r#"{"hello":"there"}"#);
    println!("webhook:   HTTP {status} — the mock has no processor, so it invents no event");
    assert_eq!(status, 501);

    println!("\nOK — offline, MockRail only, no value moved. Child reaped on exit.");
}

// ---------------------------------------------------------------------------
// A sidecar client, in about eighty lines.
//
// THIS IS NOT AN HTTP CLIENT. It does one request to 127.0.0.1 with
// `Connection: close`, and it does not do TLS, redirects, chunked encoding,
// keep-alive, IPv6, retries or timeouts on the socket itself. It is here so
// this example has no dependency you have to trust to read it. A real program
// uses `reqwest` or `ureq` and writes the same six lines.

struct Sidecar {
    port: u16,
    token: String,
}

impl Sidecar {
    fn get(&self, path: &str) -> (u16, String) {
        self.request("GET", path, None)
    }

    fn post(&self, path: &str, body: &str) -> (u16, String) {
        self.request("POST", path, Some(body))
    }

    fn request(&self, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
        let mut sock = TcpStream::connect(("127.0.0.1", self.port))
            .unwrap_or_else(|e| die(&format!("connect: {e}")));
        let body = body.unwrap_or("");
        let auth = if self.token.is_empty() {
            String::new()
        } else {
            format!("Authorization: Bearer {}\r\n", self.token)
        };
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{auth}\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.port,
            body.len()
        );
        sock.write_all(req.as_bytes())
            .unwrap_or_else(|e| die(&format!("send: {e}")));
        let mut raw = String::new();
        sock.read_to_string(&mut raw)
            .unwrap_or_else(|e| die(&format!("recv: {e}")));

        let (head, payload) = raw
            .split_once("\r\n\r\n")
            .unwrap_or_else(|| die("no headers"));
        let status: u16 = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| die("no status line"));
        (status, payload.to_string())
    }

    /// Poll `/healthz` — the one unauthenticated route — until the child is
    /// listening. Something like this is mandatory: `spawn` returns as soon as
    /// the process exists, which is well before it has bound a socket.
    fn wait_healthy(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", self.port)) {
                let req = format!(
                    "GET /healthz HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
                    self.port
                );
                let mut raw = String::new();
                let spoke =
                    s.write_all(req.as_bytes()).is_ok() && s.read_to_string(&mut raw).is_ok();
                if spoke && raw.starts_with("HTTP/1.1 200") {
                    println!("health:    ok");
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        die("the sidecar never became healthy");
    }
}

/// Kills the child on the way out — happy path, `?`, or panic unwind.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn sidecar_binary() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("PATALA_SIDECAR_BIN") {
        return p.into();
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("sdks/rust is two levels below the repo root")
        .to_path_buf();
    for profile in ["release", "debug"] {
        let candidate = root.join("target").join(profile).join("patala-sidecar");
        if candidate.is_file() {
            return candidate;
        }
    }
    die(
        "no patala-sidecar binary. Build one:\n    cargo build -p patala-sidecar --release\n\
         or point PATALA_SIDECAR_BIN at it.",
    )
}

/// Ask the kernel for an unused loopback port and hand it straight back.
/// Racy by construction — another process can take it before the child binds
/// — which is why `wait_healthy` treats a silent child as a startup failure
/// rather than hanging.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or_else(|e| die(&format!("no free port: {e}")))
}

fn random_hex_32() -> String {
    let mut bytes = [0u8; 32];
    let mut f =
        std::fs::File::open("/dev/urandom").unwrap_or_else(|e| die(&format!("urandom: {e}")));
    f.read_exact(&mut bytes)
        .unwrap_or_else(|e| die(&format!("urandom: {e}")));
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The one field this example reads by hand rather than through serde, to make
/// the point: amounts are parsed as `u64`. A JSON reader that hands you an
/// `f64` has already lost every integer above 2^53, and this is money.
fn json_u64(doc: &str, key: &str) -> Option<u64> {
    let at = doc.find(&format!("\"{key}\":"))? + key.len() + 3;
    let digits: String = doc[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn die(msg: &str) -> ! {
    eprintln!("patala sidecar example: {msg}");
    std::process::exit(1);
}
