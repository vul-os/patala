//! The `extern "C"` shell. Pointer marshalling, string ownership and panic
//! containment — nothing else. Every decision lives in the parent module,
//! where it can be read and tested as ordinary Rust.
//!
//! `include/patala.h` is the hand-written, supported declaration of these
//! symbols and is the file a host should read. This module and that header
//! must agree; `ctest/smoke.c` is what checks they do, by compiling against
//! the header and `dlopen`ing the built library.
//!
//! ## Ownership, in one rule
//!
//! Every non-const `char*` this library returns — results **and** error
//! messages — is freed with [`patala_free`] and with nothing else. It is
//! `CString` memory from Rust's allocator, so passing it to the host's
//! `free()` is undefined behaviour, and one free function for everything is
//! the only rule a binding in twelve languages can be relied on to follow.
//! [`patala_abi_version`] is the single exception: it returns a static string
//! that must not be freed.
//!
//! ## Panics
//!
//! Every entry point runs inside [`std::panic::catch_unwind`]. A panic in Rust
//! code called across an `extern "C"` boundary aborts the process by default —
//! it becomes the host's crash, with a Rust backtrace the host's operator
//! cannot act on. Catching it turns a bug here into an error string there,
//! which is the only form a C caller can do anything with.

use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// A NUL-terminated copy of [`crate::ABI_VERSION`], with the terminator
/// spelled out so it can be handed out as a `const char*` that is never freed.
static ABI_VERSION_C: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");

/// Turn a Rust `String` into memory the caller owns and frees with
/// [`patala_free`].
///
/// An interior NUL cannot survive a C string. It can only come from a rail's
/// own error text or from JSON we produced, so it is not reachable today — but
/// silently truncating would be worse than saying so, and returning NULL
/// without a message would be worse still.
fn into_c(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => CString::new(
            "patala: internal error — a response contained a NUL byte and cannot cross a C string",
        )
        .expect("this literal has no NUL")
        .into_raw(),
    }
}

/// Write a malloc'd copy of `message` through `errp`, if the caller supplied
/// somewhere to put it. `NULL` for `errp` means "I do not want the message",
/// which is a legitimate choice and must not be a crash.
///
/// # Safety
/// `errp` must be null or point to a writable `*mut c_char`.
unsafe fn set_err(errp: *mut *mut c_char, message: String) {
    if !errp.is_null() {
        *errp = into_c(message);
    }
}

/// A possibly-NULL C string as a Rust `&str`, distinguishing "absent" from
/// "present but not text".
///
/// Both used to collapse to `""`. For `patala_call` that was harmless — the
/// dispatcher rejects `""` as invalid JSON. For `patala_new` it was a money
/// bug: `open()` treats an empty document as "the offline default", so a single
/// non-UTF-8 byte anywhere in a configuration asking for Stripe silently
/// produced a `MockRail`. `charge` then succeeded, `verify` returned
/// `{"valid":true}`, and entitlement was granted against a payment that never
/// happened. It is the one place in this library where an error mapped onto
/// valid, which is the direction the whole design exists to prevent.
///
/// A host whose config bytes are latin-1, or that splices a byte-string, hits
/// it without doing anything exotic.
///
/// # Safety
/// `p` must be null or point to a NUL-terminated C string that stays valid for
/// the duration of the call.
unsafe fn as_str_opt<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return Some("");
    }
    CStr::from_ptr(p).to_str().ok()
}

/// `as_str_opt` for the arguments where non-text is indistinguishable from
/// nonsense and the dispatcher will reject it anyway (a method name, a request
/// body). Never use this for anything whose empty value carries meaning.
///
/// # Safety
/// As [`as_str_opt`].
unsafe fn as_str<'a>(p: *const c_char) -> &'a str {
    as_str_opt(p).unwrap_or("")
}

/// Returns the patala version this library was built from, e.g. `"0.1.0"`, as
/// a static string the caller must **not** free.
///
/// A host compares it against the version its bindings were generated for. A
/// shared library is resolved off a load path the host may not control;
/// without this probe a stale `libpatala_ffi` earlier on that path is called
/// silently and misbehaves in ways that look like patala bugs.
#[no_mangle]
pub extern "C" fn patala_abi_version() -> *const c_char {
    ABI_VERSION_C.as_ptr() as *const c_char
}

/// Compares this library's version against `expected` — the version the
/// caller's bindings were generated for.
///
/// Returns `0` when they match, `-1` otherwise with `*err` set to a message
/// naming both. It exists so the stale-library check is one call rather than a
/// `strcmp` reimplemented, and forgotten, in each of a dozen languages.
///
/// # Safety
/// `expected` must be null or a valid NUL-terminated C string; `err` must be
/// null or point to a writable `*mut c_char`.
#[no_mangle]
pub unsafe extern "C" fn patala_abi_check(
    expected: *const c_char,
    err: *mut *mut c_char,
) -> std::ffi::c_int {
    let want = as_str(expected);
    if want == crate::ABI_VERSION {
        0
    } else {
        set_err(
            err,
            format!(
                "patala: ABI version mismatch — this library is {}, the caller expected {:?}. \
                 A stale libpatala_ffi is earlier on the load path than the one you installed.",
                crate::ABI_VERSION,
                want
            ),
        );
        -1
    }
}

/// Builds a rail from a JSON configuration document and returns its handle, or
/// `0` on failure with `*err` set.
///
/// `NULL` or `""` means the offline default — a `MockRail` on `USDC`, which
/// needs no credentials and no network. See `include/patala.h` for the
/// document shapes.
///
/// Creating a rail talks to nothing: no socket is opened, no thread is
/// started, no signal handler is installed. Only `patala_call` reaches a
/// network, and only for a rail configured to have one.
///
/// # Safety
/// `config_json` must be null or a valid NUL-terminated C string; `err` must
/// be null or point to a writable `*mut c_char`.
#[no_mangle]
pub unsafe extern "C" fn patala_new(config_json: *const c_char, err: *mut *mut c_char) -> u64 {
    // Refuse non-UTF-8 rather than letting it fall through as "" — an empty
    // document means "the offline default MockRail", so collapsing the two
    // turned a corrupt Stripe config into a mock that reports every charge as
    // settled. Failing here is the only answer that cannot be mistaken for a
    // payment.
    let config = match as_str_opt(config_json) {
        Some(c) => c.to_string(),
        None => {
            set_err(
                err,
                "patala: configuration is not valid UTF-8. It is refused rather than                  treated as absent, because an absent configuration means the offline                  MockRail — which would report a charge that never happened as settled."
                    .to_string(),
            );
            return 0;
        }
    };
    match catch_unwind(AssertUnwindSafe(move || crate::open(&config))) {
        Ok(Ok(handle)) => handle,
        Ok(Err(message)) => {
            set_err(err, message);
            0
        }
        Err(_) => {
            set_err(err, "patala: panicked while creating a rail".to_string());
            0
        }
    }
}

/// Releases the rail behind `h`. Closing an unknown or already-closed handle
/// is a no-op, so a host's cleanup path can be idempotent.
#[no_mangle]
pub extern "C" fn patala_close(h: u64) {
    let _ = catch_unwind(AssertUnwindSafe(|| crate::close(h)));
}

/// One unary call. Returns malloc'd UTF-8 JSON the caller frees with
/// [`patala_free`], or `NULL` with `*err` set.
///
/// `method` is one of the short strings in `include/patala.h`;
/// `request_json` is the same JSON body `patala-sidecar`'s matching endpoint
/// takes, and the result is what that endpoint returns.
///
/// # Safety
/// `method` and `request_json` must be null or valid NUL-terminated C strings;
/// `err` must be null or point to a writable `*mut c_char`.
#[no_mangle]
pub unsafe extern "C" fn patala_call(
    h: u64,
    method: *const c_char,
    request_json: *const c_char,
    err: *mut *mut c_char,
) -> *mut c_char {
    let method = as_str(method).to_string();
    let request = as_str(request_json).to_string();
    match catch_unwind(AssertUnwindSafe(move || crate::call(h, &method, &request))) {
        Ok(Ok(out)) => into_c(out),
        Ok(Err(message)) => {
            set_err(err, message);
            std::ptr::null_mut()
        }
        Err(_) => {
            set_err(err, "patala: panicked during the call".to_string());
            std::ptr::null_mut()
        }
    }
}

/// Frees anything this library returned: results and error messages alike.
/// `NULL` is accepted and ignored, so bindings can call it on every path.
///
/// Do **not** pass it the return of [`patala_abi_version`], and do not free
/// anything this library returned with the host's own `free()`: this memory
/// comes from Rust's allocator.
///
/// # Safety
/// `p` must be null, or a pointer this library returned from
/// [`patala_new`]'s `err`, [`patala_abi_check`]'s `err`, or [`patala_call`],
/// and must not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn patala_free(p: *mut c_char) {
    if !p.is_null() {
        drop(CString::from_raw(p));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The version string handed to C must be NUL-terminated and must be the
    /// same text the Rust constant carries. A `concat!` that lost its `"\0"`
    /// would hand C a pointer that runs off the end of the literal.
    #[test]
    fn the_c_version_string_is_nul_terminated_and_matches() {
        let bytes = ABI_VERSION_C.as_bytes();
        assert_eq!(*bytes.last().unwrap(), 0, "missing NUL terminator");
        assert_eq!(&bytes[..bytes.len() - 1], crate::ABI_VERSION.as_bytes());
        let c = unsafe { CStr::from_ptr(patala_abi_version()) };
        assert_eq!(c.to_str().unwrap(), crate::ABI_VERSION);
    }

    #[test]
    fn abi_check_accepts_the_right_version_and_names_a_wrong_one() {
        let ok = CString::new(crate::ABI_VERSION).unwrap();
        assert_eq!(
            unsafe { patala_abi_check(ok.as_ptr(), std::ptr::null_mut()) },
            0
        );

        let stale = CString::new("0.0.1-old").unwrap();
        let mut err: *mut c_char = std::ptr::null_mut();
        assert_eq!(unsafe { patala_abi_check(stale.as_ptr(), &mut err) }, -1);
        assert!(!err.is_null(), "a mismatch must set *err");
        let message = unsafe { CStr::from_ptr(err) }.to_str().unwrap().to_string();
        assert!(message.contains("0.0.1-old"), "{message}");
        assert!(message.contains(crate::ABI_VERSION), "{message}");
        unsafe { patala_free(err) };
    }

    #[test]
    fn a_null_config_is_the_offline_default_and_close_is_idempotent() {
        let mut err: *mut c_char = std::ptr::null_mut();
        let h = unsafe { patala_new(std::ptr::null(), &mut err) };
        assert_ne!(h, 0, "a NULL config must mean defaults, not a failure");
        assert!(err.is_null());

        let method = CString::new("capabilities").unwrap();
        let out = unsafe { patala_call(h, method.as_ptr(), std::ptr::null(), &mut err) };
        assert!(!out.is_null());
        let json = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        assert!(json.contains("NonCustodialFinal"), "{json}");
        unsafe { patala_free(out) };

        patala_close(h);
        patala_close(h);

        let out = unsafe { patala_call(h, method.as_ptr(), std::ptr::null(), &mut err) };
        assert!(out.is_null(), "a closed handle must not answer");
        assert!(!err.is_null(), "use-after-close must set *err");
        unsafe { patala_free(err) };
    }

    #[test]
    fn a_null_err_pointer_is_accepted_rather_than_dereferenced() {
        // A host that does not want the message passes NULL. Writing through
        // it would be a crash in someone else's process.
        let bad = CString::new("{\"rail\": ").unwrap();
        let h = unsafe { patala_new(bad.as_ptr(), std::ptr::null_mut()) };
        assert_eq!(h, 0);
    }

    #[test]
    fn freeing_null_is_safe() {
        unsafe { patala_free(std::ptr::null_mut()) };
    }
}

#[cfg(test)]
mod utf8_refusal_tests {
    use super::*;
    use std::ffi::CString;

    /// The security review demonstrated this against the real ABI: a non-UTF-8
    /// byte in a configuration asking for a real processor produced
    /// `handle=1 err=<NULL>`, then `id` reported `mock`, `charge` succeeded for
    /// 999999 minor units, and `verify` returned `{"valid":true}` — a settled
    /// receipt for money that never moved.
    ///
    /// The cause was `as_str` collapsing invalid UTF-8 to `""`, which `open()`
    /// reads as "use the offline default MockRail". An error became a valid
    /// receipt, which is the one direction this library is built to prevent.
    #[test]
    fn a_non_utf8_config_is_refused_and_never_becomes_a_mock() {
        // 0xFF is not valid UTF-8 in any position.
        let bytes = b"{\"kind\":\"stripe\",\"secret\":\"sk_\xff\"}".to_vec();
        let cfg = CString::new(bytes).expect("no interior NUL");
        let mut err: *mut c_char = std::ptr::null_mut();

        let handle = unsafe { patala_new(cfg.as_ptr(), &mut err) };

        assert_eq!(
            handle, 0,
            "a non-UTF-8 configuration produced handle {handle}. If it opened, it opened \
             a MockRail: charge succeeds, verify returns valid, and no money moved."
        );
        assert!(
            !err.is_null(),
            "a refusal must say why; the host has no other signal"
        );
        let msg = unsafe { CStr::from_ptr(err) }
            .to_string_lossy()
            .into_owned();
        unsafe { patala_free(err) };
        assert!(
            msg.contains("UTF-8"),
            "the error should name the cause, got: {msg}"
        );
    }

    /// The other half: NULL and empty still mean "absent", which is a documented
    /// and useful thing to pass. Without this the fix above could be "refuse
    /// everything", which would pass the test above and break every host.
    #[test]
    fn null_and_empty_still_select_the_documented_offline_default() {
        for (label, ptr) in [
            ("NULL", std::ptr::null()),
            (
                "empty",
                CString::new("").unwrap().into_raw() as *const c_char,
            ),
        ] {
            let mut err: *mut c_char = std::ptr::null_mut();
            let handle = unsafe { patala_new(ptr, &mut err) };
            assert_ne!(handle, 0, "{label} config should open the offline MockRail");
            assert!(err.is_null(), "{label} config should not set an error");
            patala_close(handle);
        }
    }
}
