//! The ISO-4217 minor-unit currency table, shared by every rail in this
//! crate.
//!
//! **Ported from cackle's `internal/money/currency.go`** (the `CurrencyInfo`
//! table, `Lookup`/`Exponent`/`Validate`/`Normalize`/`Name`/
//! `SupportedCurrencies`) **and `internal/payments/currency.go`** (the
//! `minorToMajorString`/`majorStringToMinor` conversion helpers, rebuilt here
//! on top of the same table instead of a second, separately-tabulated
//! exponent map).
//!
//! Cackle kept TWO copies of this table on purpose: `internal/money` (the
//! canonical one, used for order totals/UI formatting) and a self-contained
//! local copy inside `internal/payments` (scoped to that package's own
//! provider-subunit conversions, so it wouldn't take on a hard build-order
//! dependency on a sibling package that might not exist yet — see
//! `payments/currency.go`'s own doc comment, which explicitly says *"if the
//! two ever disagree, internal/money is the one to trust."*). `patala-fiat`
//! has no such build-order constraint — this module IS both of those, merged
//! into ONE canonical table, exactly as cackle's own comment says the
//! canonical one should look. Per `PATALA.md` §8 and this task's brief: this
//! is money-critical code, a wrong exponent is a 100x over/undercharge, so
//! every currency below is transcribed byte-for-byte from cackle's table,
//! not re-derived or guessed.
//!
//! **A deliberate, disclosed deviation from `payments/currency.go`'s
//! `minorUnitExponent`:** that function defaults an unknown/malformed
//! currency code to exponent 2 rather than erroring (its own doc comment
//! calls this out as a compromise for a self-contained local copy). Since
//! this module IS the canonical table cackle's own comment defers to
//! (`internal/money`'s `Exponent`, which errors on anything unknown), this
//! port always fails closed on an unknown/malformed code — see
//! [`CurrencyError`] — rather than silently assuming 2 decimal places. This
//! matches `PATALA.md` §8 ("errors fail closed") and money's own behaviour;
//! it does not match `payments/currency.go`'s specific fallback, which this
//! module intentionally does not carry forward (see the currency-table tests
//! below for exactly which cases this affects).
//!
//! **Not ported:** cackle's `money.Amount` type and its `Add`/`Sub`/`Mul`
//! ledger arithmetic (`internal/money/amount.go`). No rail in this crate
//! needs to add/subtract/multiply money — `PaymentRail::charge` takes a
//! single `amount_minor` from the caller and moves it — so only the
//! currency-table lookups and the minor-unit<->major-unit string conversions
//! actually used by an adapter are ported here. If a future adapter needs
//! ledger arithmetic, port `Amount`'s `Add`/`Sub`/`Mul`/`Major`/`ParseMajor`
//! then; `formatMinor`/`parseDecimalToMinor`'s int64-overflow-safe unsigned
//! magnitude tricks are moot here anyway since `patala-core::PayRequest`
//! carries `amount_minor: u64` (never negative), unlike cackle's signed
//! `int64` `AmountMinor`.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Mirrors cackle's `money.ErrInvalidCurrency` / `.ErrUnknownCurrency` /
/// `.ErrInvalidAmount`. cackle's `ErrCurrencyMismatch` and `ErrOverflow` are
/// not ported — see the module docs on why `Amount` arithmetic is out of
/// scope.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CurrencyError {
    /// Mirrors `money.ErrInvalidCurrency`: the input isn't shaped like an
    /// ISO-4217 alpha-3 code at all (wrong length, non-letters).
    #[error("invalid currency code {0:?} (must be a 3-letter ISO-4217 code)")]
    InvalidCurrency(String),
    /// Mirrors `money.ErrUnknownCurrency`: syntactically a 3-letter code but
    /// not in this module's table.
    #[error("unknown currency code {0:?}")]
    UnknownCurrency(String),
    /// Mirrors `money.ErrInvalidAmount`: a non-numeric, empty, negative, or
    /// over-precise decimal amount string.
    #[error("invalid amount: {0}")]
    InvalidAmount(String),
}

/// Mirrors cackle's `money.CurrencyInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrencyInfo {
    pub code: &'static str,
    /// How many digits follow the decimal point in this currency's
    /// major-unit display: 0 (JPY, ...), 2 (the common case), or 3 (KWD,
    /// ...). Never assume 2 — always look it up.
    pub exponent: u8,
    pub name: &'static str,
}

/// Mirrors cackle's `currencyDefs` — transcribed verbatim, same order, same
/// codes/exponents/names. Zero-decimal (exponent 0): BIF, CLP, DJF, GNF,
/// ISK, JPY, KMF, KRW, PYG, RWF, UGX, VND, VUV, XAF, XOF, XPF. Three-decimal
/// (exponent 3): BHD, IQD, JOD, KWD, LYD, OMR, TND. Everything else is 2.
#[rustfmt::skip]
static CURRENCY_DEFS: &[CurrencyInfo] = &[
    CurrencyInfo { code: "AED", exponent: 2, name: "United Arab Emirates Dirham" },
    CurrencyInfo { code: "AFN", exponent: 2, name: "Afghan Afghani" },
    CurrencyInfo { code: "ALL", exponent: 2, name: "Albanian Lek" },
    CurrencyInfo { code: "AMD", exponent: 2, name: "Armenian Dram" },
    CurrencyInfo { code: "ANG", exponent: 2, name: "Netherlands Antillean Guilder" },
    CurrencyInfo { code: "AOA", exponent: 2, name: "Angolan Kwanza" },
    CurrencyInfo { code: "ARS", exponent: 2, name: "Argentine Peso" },
    CurrencyInfo { code: "AUD", exponent: 2, name: "Australian Dollar" },
    CurrencyInfo { code: "AWG", exponent: 2, name: "Aruban Florin" },
    CurrencyInfo { code: "AZN", exponent: 2, name: "Azerbaijani Manat" },
    CurrencyInfo { code: "BAM", exponent: 2, name: "Bosnia-Herzegovina Convertible Mark" },
    CurrencyInfo { code: "BBD", exponent: 2, name: "Barbadian Dollar" },
    CurrencyInfo { code: "BDT", exponent: 2, name: "Bangladeshi Taka" },
    CurrencyInfo { code: "BGN", exponent: 2, name: "Bulgarian Lev" },
    CurrencyInfo { code: "BHD", exponent: 3, name: "Bahraini Dinar" },
    CurrencyInfo { code: "BIF", exponent: 0, name: "Burundian Franc" },
    CurrencyInfo { code: "BMD", exponent: 2, name: "Bermudian Dollar" },
    CurrencyInfo { code: "BND", exponent: 2, name: "Brunei Dollar" },
    CurrencyInfo { code: "BOB", exponent: 2, name: "Bolivian Boliviano" },
    CurrencyInfo { code: "BRL", exponent: 2, name: "Brazilian Real" },
    CurrencyInfo { code: "BSD", exponent: 2, name: "Bahamian Dollar" },
    CurrencyInfo { code: "BTN", exponent: 2, name: "Bhutanese Ngultrum" },
    CurrencyInfo { code: "BWP", exponent: 2, name: "Botswana Pula" },
    CurrencyInfo { code: "BYN", exponent: 2, name: "Belarusian Ruble" },
    CurrencyInfo { code: "BZD", exponent: 2, name: "Belize Dollar" },
    CurrencyInfo { code: "CAD", exponent: 2, name: "Canadian Dollar" },
    CurrencyInfo { code: "CDF", exponent: 2, name: "Congolese Franc" },
    CurrencyInfo { code: "CHF", exponent: 2, name: "Swiss Franc" },
    CurrencyInfo { code: "CLP", exponent: 0, name: "Chilean Peso" },
    CurrencyInfo { code: "CNY", exponent: 2, name: "Chinese Yuan" },
    CurrencyInfo { code: "COP", exponent: 2, name: "Colombian Peso" },
    CurrencyInfo { code: "CRC", exponent: 2, name: "Costa Rican Colon" },
    CurrencyInfo { code: "CUP", exponent: 2, name: "Cuban Peso" },
    CurrencyInfo { code: "CVE", exponent: 2, name: "Cape Verdean Escudo" },
    CurrencyInfo { code: "CZK", exponent: 2, name: "Czech Koruna" },
    CurrencyInfo { code: "DJF", exponent: 0, name: "Djiboutian Franc" },
    CurrencyInfo { code: "DKK", exponent: 2, name: "Danish Krone" },
    CurrencyInfo { code: "DOP", exponent: 2, name: "Dominican Peso" },
    CurrencyInfo { code: "DZD", exponent: 2, name: "Algerian Dinar" },
    CurrencyInfo { code: "EGP", exponent: 2, name: "Egyptian Pound" },
    CurrencyInfo { code: "ETB", exponent: 2, name: "Ethiopian Birr" },
    CurrencyInfo { code: "EUR", exponent: 2, name: "Euro" },
    CurrencyInfo { code: "FJD", exponent: 2, name: "Fijian Dollar" },
    CurrencyInfo { code: "GBP", exponent: 2, name: "British Pound Sterling" },
    CurrencyInfo { code: "GEL", exponent: 2, name: "Georgian Lari" },
    CurrencyInfo { code: "GHS", exponent: 2, name: "Ghanaian Cedi" },
    CurrencyInfo { code: "GMD", exponent: 2, name: "Gambian Dalasi" },
    CurrencyInfo { code: "GNF", exponent: 0, name: "Guinean Franc" },
    CurrencyInfo { code: "GTQ", exponent: 2, name: "Guatemalan Quetzal" },
    CurrencyInfo { code: "GYD", exponent: 2, name: "Guyanese Dollar" },
    CurrencyInfo { code: "HKD", exponent: 2, name: "Hong Kong Dollar" },
    CurrencyInfo { code: "HNL", exponent: 2, name: "Honduran Lempira" },
    CurrencyInfo { code: "HTG", exponent: 2, name: "Haitian Gourde" },
    CurrencyInfo { code: "HUF", exponent: 2, name: "Hungarian Forint" },
    CurrencyInfo { code: "IDR", exponent: 2, name: "Indonesian Rupiah" },
    CurrencyInfo { code: "ILS", exponent: 2, name: "Israeli New Shekel" },
    CurrencyInfo { code: "INR", exponent: 2, name: "Indian Rupee" },
    CurrencyInfo { code: "IQD", exponent: 3, name: "Iraqi Dinar" },
    CurrencyInfo { code: "IRR", exponent: 2, name: "Iranian Rial" },
    CurrencyInfo { code: "ISK", exponent: 0, name: "Icelandic Krona" },
    CurrencyInfo { code: "JMD", exponent: 2, name: "Jamaican Dollar" },
    CurrencyInfo { code: "JOD", exponent: 3, name: "Jordanian Dinar" },
    CurrencyInfo { code: "JPY", exponent: 0, name: "Japanese Yen" },
    CurrencyInfo { code: "KES", exponent: 2, name: "Kenyan Shilling" },
    CurrencyInfo { code: "KGS", exponent: 2, name: "Kyrgyzstani Som" },
    CurrencyInfo { code: "KHR", exponent: 2, name: "Cambodian Riel" },
    CurrencyInfo { code: "KMF", exponent: 0, name: "Comorian Franc" },
    CurrencyInfo { code: "KRW", exponent: 0, name: "South Korean Won" },
    CurrencyInfo { code: "KWD", exponent: 3, name: "Kuwaiti Dinar" },
    CurrencyInfo { code: "KYD", exponent: 2, name: "Cayman Islands Dollar" },
    CurrencyInfo { code: "KZT", exponent: 2, name: "Kazakhstani Tenge" },
    CurrencyInfo { code: "LAK", exponent: 2, name: "Lao Kip" },
    CurrencyInfo { code: "LBP", exponent: 2, name: "Lebanese Pound" },
    CurrencyInfo { code: "LKR", exponent: 2, name: "Sri Lankan Rupee" },
    CurrencyInfo { code: "LRD", exponent: 2, name: "Liberian Dollar" },
    CurrencyInfo { code: "LSL", exponent: 2, name: "Lesotho Loti" },
    CurrencyInfo { code: "LYD", exponent: 3, name: "Libyan Dinar" },
    CurrencyInfo { code: "MAD", exponent: 2, name: "Moroccan Dirham" },
    CurrencyInfo { code: "MDL", exponent: 2, name: "Moldovan Leu" },
    CurrencyInfo { code: "MGA", exponent: 2, name: "Malagasy Ariary" },
    CurrencyInfo { code: "MKD", exponent: 2, name: "Macedonian Denar" },
    CurrencyInfo { code: "MMK", exponent: 2, name: "Myanmar Kyat" },
    CurrencyInfo { code: "MNT", exponent: 2, name: "Mongolian Tugrik" },
    CurrencyInfo { code: "MOP", exponent: 2, name: "Macanese Pataca" },
    CurrencyInfo { code: "MRU", exponent: 2, name: "Mauritanian Ouguiya" },
    CurrencyInfo { code: "MUR", exponent: 2, name: "Mauritian Rupee" },
    CurrencyInfo { code: "MVR", exponent: 2, name: "Maldivian Rufiyaa" },
    CurrencyInfo { code: "MWK", exponent: 2, name: "Malawian Kwacha" },
    CurrencyInfo { code: "MXN", exponent: 2, name: "Mexican Peso" },
    CurrencyInfo { code: "MYR", exponent: 2, name: "Malaysian Ringgit" },
    CurrencyInfo { code: "MZN", exponent: 2, name: "Mozambican Metical" },
    CurrencyInfo { code: "NAD", exponent: 2, name: "Namibian Dollar" },
    CurrencyInfo { code: "NGN", exponent: 2, name: "Nigerian Naira" },
    CurrencyInfo { code: "NIO", exponent: 2, name: "Nicaraguan Cordoba" },
    CurrencyInfo { code: "NOK", exponent: 2, name: "Norwegian Krone" },
    CurrencyInfo { code: "NPR", exponent: 2, name: "Nepalese Rupee" },
    CurrencyInfo { code: "NZD", exponent: 2, name: "New Zealand Dollar" },
    CurrencyInfo { code: "OMR", exponent: 3, name: "Omani Rial" },
    CurrencyInfo { code: "PAB", exponent: 2, name: "Panamanian Balboa" },
    CurrencyInfo { code: "PEN", exponent: 2, name: "Peruvian Sol" },
    CurrencyInfo { code: "PGK", exponent: 2, name: "Papua New Guinean Kina" },
    CurrencyInfo { code: "PHP", exponent: 2, name: "Philippine Peso" },
    CurrencyInfo { code: "PKR", exponent: 2, name: "Pakistani Rupee" },
    CurrencyInfo { code: "PLN", exponent: 2, name: "Polish Zloty" },
    CurrencyInfo { code: "PYG", exponent: 0, name: "Paraguayan Guarani" },
    CurrencyInfo { code: "QAR", exponent: 2, name: "Qatari Riyal" },
    CurrencyInfo { code: "RON", exponent: 2, name: "Romanian Leu" },
    CurrencyInfo { code: "RSD", exponent: 2, name: "Serbian Dinar" },
    CurrencyInfo { code: "RUB", exponent: 2, name: "Russian Ruble" },
    CurrencyInfo { code: "RWF", exponent: 0, name: "Rwandan Franc" },
    CurrencyInfo { code: "SAR", exponent: 2, name: "Saudi Riyal" },
    CurrencyInfo { code: "SBD", exponent: 2, name: "Solomon Islands Dollar" },
    CurrencyInfo { code: "SCR", exponent: 2, name: "Seychellois Rupee" },
    CurrencyInfo { code: "SDG", exponent: 2, name: "Sudanese Pound" },
    CurrencyInfo { code: "SEK", exponent: 2, name: "Swedish Krona" },
    CurrencyInfo { code: "SGD", exponent: 2, name: "Singapore Dollar" },
    CurrencyInfo { code: "SLE", exponent: 2, name: "Sierra Leonean Leone" },
    CurrencyInfo { code: "SOS", exponent: 2, name: "Somali Shilling" },
    CurrencyInfo { code: "SRD", exponent: 2, name: "Surinamese Dollar" },
    CurrencyInfo { code: "SSP", exponent: 2, name: "South Sudanese Pound" },
    CurrencyInfo { code: "STN", exponent: 2, name: "Sao Tome and Principe Dobra" },
    CurrencyInfo { code: "SZL", exponent: 2, name: "Eswatini Lilangeni" },
    CurrencyInfo { code: "THB", exponent: 2, name: "Thai Baht" },
    CurrencyInfo { code: "TJS", exponent: 2, name: "Tajikistani Somoni" },
    CurrencyInfo { code: "TMT", exponent: 2, name: "Turkmenistani Manat" },
    CurrencyInfo { code: "TND", exponent: 3, name: "Tunisian Dinar" },
    CurrencyInfo { code: "TOP", exponent: 2, name: "Tongan Pa'anga" },
    CurrencyInfo { code: "TRY", exponent: 2, name: "Turkish Lira" },
    CurrencyInfo { code: "TTD", exponent: 2, name: "Trinidad and Tobago Dollar" },
    CurrencyInfo { code: "TWD", exponent: 2, name: "New Taiwan Dollar" },
    CurrencyInfo { code: "TZS", exponent: 2, name: "Tanzanian Shilling" },
    CurrencyInfo { code: "UAH", exponent: 2, name: "Ukrainian Hryvnia" },
    CurrencyInfo { code: "UGX", exponent: 0, name: "Ugandan Shilling" },
    CurrencyInfo { code: "USD", exponent: 2, name: "United States Dollar" },
    CurrencyInfo { code: "UYU", exponent: 2, name: "Uruguayan Peso" },
    CurrencyInfo { code: "UZS", exponent: 2, name: "Uzbekistani Som" },
    CurrencyInfo { code: "VES", exponent: 2, name: "Venezuelan Bolivar Soberano" },
    CurrencyInfo { code: "VND", exponent: 0, name: "Vietnamese Dong" },
    CurrencyInfo { code: "VUV", exponent: 0, name: "Vanuatu Vatu" },
    CurrencyInfo { code: "WST", exponent: 2, name: "Samoan Tala" },
    CurrencyInfo { code: "XAF", exponent: 0, name: "Central African CFA Franc" },
    CurrencyInfo { code: "XCD", exponent: 2, name: "East Caribbean Dollar" },
    CurrencyInfo { code: "XOF", exponent: 0, name: "West African CFA Franc" },
    CurrencyInfo { code: "XPF", exponent: 0, name: "CFP Franc" },
    CurrencyInfo { code: "YER", exponent: 2, name: "Yemeni Rial" },
    CurrencyInfo { code: "ZAR", exponent: 2, name: "South African Rand" },
    CurrencyInfo { code: "ZMW", exponent: 2, name: "Zambian Kwacha" },
];

fn table() -> &'static HashMap<&'static str, CurrencyInfo> {
    static TABLE: OnceLock<HashMap<&'static str, CurrencyInfo>> = OnceLock::new();
    TABLE.get_or_init(|| CURRENCY_DEFS.iter().map(|c| (c.code, *c)).collect())
}

/// Mirrors `money.normalizeCode`: upper-cases and trims, then rejects
/// anything that isn't exactly 3 ASCII letters, BEFORE the table lookup.
fn normalize_code(code: &str) -> Result<String, CurrencyError> {
    let c = code.trim().to_ascii_uppercase();
    if c.len() != 3 || !c.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(CurrencyError::InvalidCurrency(code.to_string()));
    }
    Ok(c)
}

/// Mirrors `money.Lookup`.
pub fn lookup(code: &str) -> Result<CurrencyInfo, CurrencyError> {
    let norm = normalize_code(code)?;
    table()
        .get(norm.as_str())
        .copied()
        .ok_or(CurrencyError::UnknownCurrency(norm))
}

/// Mirrors `money.Exponent`. Never assume 2 — always call this (or
/// [`lookup`]).
pub fn exponent(code: &str) -> Result<u8, CurrencyError> {
    Ok(lookup(code)?.exponent)
}

/// Mirrors `money.Validate`. Accepts lowercase input.
pub fn validate(code: &str) -> Result<(), CurrencyError> {
    lookup(code).map(|_| ())
}

/// Mirrors `money.Normalize`.
pub fn normalize(code: &str) -> Result<String, CurrencyError> {
    Ok(lookup(code)?.code.to_string())
}

/// Mirrors `money.Name`.
pub fn name(code: &str) -> Result<&'static str, CurrencyError> {
    Ok(lookup(code)?.name)
}

/// Mirrors `money.SupportedCurrencies` — every code this module knows about,
/// in table order.
pub fn supported_currencies() -> Vec<&'static str> {
    CURRENCY_DEFS.iter().map(|c| c.code).collect()
}

/// Mirrors `payments.minorToMajorString`, rebuilt on [`exponent`] rather
/// than a second table. `amount_minor` is `u64` (never negative), unlike
/// cackle's signed `int64` — so the negative-amount rejection branch cackle
/// has is structurally impossible here and is not ported (see module docs).
pub fn minor_to_major_string(amount_minor: u64, currency: &str) -> Result<String, CurrencyError> {
    let exp = exponent(currency)? as u32;
    if exp == 0 {
        return Ok(amount_minor.to_string());
    }
    let div = 10u64.pow(exp);
    let whole = amount_minor / div;
    let frac = amount_minor % div;
    Ok(format!("{whole}.{frac:0width$}", width = exp as usize))
}

/// Mirrors `payments.majorStringToMinor`: the inverse of
/// [`minor_to_major_string`]. Fails closed on anything that isn't a plain
/// non-negative decimal number with no more fractional digits than the
/// currency's exponent allows (extra precision is refused, never silently
/// truncated).
pub fn major_string_to_minor(s: &str, currency: &str) -> Result<u64, CurrencyError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(CurrencyError::InvalidAmount("empty amount string".into()));
    }
    if let Some(stripped) = trimmed.strip_prefix('-') {
        return Err(CurrencyError::InvalidAmount(format!(
            "negative amount {:?} is not valid",
            format!("-{stripped}")
        )));
    }
    let exp = exponent(currency)? as usize;

    let (whole_part, frac_part) = match trimmed.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (trimmed, None),
    };
    let whole_part = if whole_part.is_empty() {
        "0"
    } else {
        whole_part
    };
    if !whole_part.bytes().all(|b| b.is_ascii_digit()) {
        return Err(CurrencyError::InvalidAmount(format!(
            "malformed amount string {trimmed:?}"
        )));
    }

    let frac_digits: String = match frac_part {
        Some(f) => {
            if !f.bytes().all(|b| b.is_ascii_digit()) {
                return Err(CurrencyError::InvalidAmount(format!(
                    "malformed amount string {trimmed:?}"
                )));
            }
            if f.len() > exp {
                return Err(CurrencyError::InvalidAmount(format!(
                    "amount string {trimmed:?} has more fractional digits than {}'s {exp}-decimal exponent",
                    currency.trim().to_ascii_uppercase()
                )));
            }
            format!("{f:0<width$}", width = exp)
        }
        None => "0".repeat(exp),
    };

    let whole_n: u64 = whole_part.parse().map_err(|_| {
        CurrencyError::InvalidAmount(format!("malformed amount string {trimmed:?}"))
    })?;
    let frac_n: u64 = if exp > 0 {
        frac_digits.parse().map_err(|_| {
            CurrencyError::InvalidAmount(format!("malformed amount string {trimmed:?}"))
        })?
    } else {
        0
    };
    let mul = 10u64.pow(exp as u32);
    whole_n
        .checked_mul(mul)
        .and_then(|w| w.checked_add(frac_n))
        .ok_or_else(|| {
            CurrencyError::InvalidAmount(format!("amount string {trimmed:?} overflows u64"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Exponent / Validate / Normalize / Lookup ---------------------
    // Ported from cackle's internal/money/money_test.go.

    #[test]
    fn exponent_zero_decimal_currencies() {
        for code in ["JPY", "KRW", "VND", "CLP", "ISK"] {
            assert_eq!(exponent(code).unwrap(), 0, "{code}");
        }
    }

    #[test]
    fn exponent_three_decimal_currencies() {
        // Cackle's money_test.go covers KWD/BHD/JOD/OMR/TND; this port also
        // covers IQD/LYD since payments/currency_test.go's fuller
        // three-decimal list includes them too and they're in the same
        // table entry.
        for code in ["KWD", "BHD", "JOD", "OMR", "TND", "IQD", "LYD"] {
            assert_eq!(exponent(code).unwrap(), 3, "{code}");
        }
    }

    #[test]
    fn exponent_two_decimal_currencies() {
        for code in ["USD", "EUR", "GBP", "ZAR", "NGN", "INR"] {
            assert_eq!(exponent(code).unwrap(), 2, "{code}");
        }
    }

    #[test]
    fn exponent_unknown_code() {
        assert_eq!(
            exponent("XXX").unwrap_err(),
            CurrencyError::UnknownCurrency("XXX".into())
        );
    }

    #[test]
    fn exponent_invalid_shape() {
        // NOTE: unlike payments/currency.go's own `minorUnitExponent` (which
        // defaults "" to exponent 2), this module always fails closed on an
        // unrecognisable code -- see the module docs' "deliberate, disclosed
        // deviation" note. "" is therefore asserted as an error here, not a
        // silent 2.
        for code in ["", "US", "USDD", "12A", "US$"] {
            assert!(
                matches!(exponent(code), Err(CurrencyError::InvalidCurrency(_))),
                "{code:?}"
            );
        }
    }

    #[test]
    fn validate_lowercase_accepted() {
        assert!(validate("usd").is_ok());
        assert!(validate("jpy").is_ok());
    }

    #[test]
    fn validate_whitespace_trimmed() {
        assert!(validate(" USD ").is_ok());
    }

    #[test]
    fn normalize_uppercases() {
        assert_eq!(normalize("usd").unwrap(), "USD");
    }

    #[test]
    fn name_known_currency() {
        assert!(!name("JPY").unwrap().is_empty());
    }

    #[test]
    fn supported_currencies_contains_core_set() {
        let set: std::collections::HashSet<_> = supported_currencies().into_iter().collect();
        for code in [
            "USD", "EUR", "JPY", "KWD", "ZAR", "NGN", "INR", "IDR", "BRL",
        ] {
            assert!(set.contains(code), "missing {code}");
        }
    }

    // --- minor_to_major_string / major_string_to_minor -----------------
    // Ported from cackle's internal/payments/currency_test.go.

    #[test]
    fn minor_to_major_string_cases() {
        let cases: &[(u64, &str, &str)] = &[
            (10050, "ZAR", "100.50"),
            (100, "USD", "1.00"),
            (5, "USD", "0.05"),
            (1000, "JPY", "1000"),
            (1, "JPY", "1"),
            (1000, "KWD", "1.000"),
            (1500, "KWD", "1.500"),
            (0, "ZAR", "0.00"),
        ];
        for (minor, currency, want) in cases {
            assert_eq!(
                minor_to_major_string(*minor, currency).unwrap(),
                *want,
                "{minor} {currency}"
            );
        }
    }

    #[test]
    fn major_string_to_minor_cases() {
        let cases: &[(&str, &str, u64)] = &[
            ("100.50", "ZAR", 10050),
            ("1", "USD", 100),
            ("1.00", "USD", 100),
            ("0.05", "USD", 5),
            ("1000", "JPY", 1000),
            ("1.000", "KWD", 1000),
            ("1.5", "KWD", 1500),
            ("0", "ZAR", 0),
            (".5", "USD", 50),
        ];
        for (s, currency, want) in cases {
            assert_eq!(
                major_string_to_minor(s, currency).unwrap(),
                *want,
                "{s} {currency}"
            );
        }
    }

    #[test]
    fn major_string_to_minor_jpy_rejects_fractional_digits_beyond_zero_decimal_exponent() {
        // "1000.00" for JPY (0 decimals): 2 fractional digits > exponent 0
        // must be rejected outright, never silently truncated.
        assert!(major_string_to_minor("1000.00", "JPY").is_err());
    }

    #[test]
    fn major_string_to_minor_fails_closed() {
        let bad: &[(&str, &str)] = &[
            ("", "USD"),
            ("-1.00", "USD"),
            ("abc", "USD"),
            ("1.2.3", "USD"),
            ("1.005", "USD"),  // 3 fractional digits, USD only allows 2
            ("1.0000", "KWD"), // 4 fractional digits, KWD only allows 3
            ("1.x", "USD"),
        ];
        for (s, currency) in bad {
            assert!(
                major_string_to_minor(s, currency).is_err(),
                "{s} {currency}"
            );
        }
    }

    #[test]
    fn minor_major_round_trip() {
        let currencies = ["ZAR", "USD", "JPY", "KWD"];
        let amounts: &[u64] = &[0, 1, 5, 99, 100, 1000, 123456];
        for currency in currencies {
            for &amount in amounts {
                let s = minor_to_major_string(amount, currency).unwrap();
                let back = major_string_to_minor(&s, currency).unwrap();
                assert_eq!(back, amount, "{amount} {currency} -> {s:?} -> {back}");
            }
        }
    }
}
