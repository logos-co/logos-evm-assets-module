//! Exact decimal-string and U256 conversion. No floating point belongs in an asset module.

use alloy::primitives::U256;
use serde_json::{json, Value};

pub const DISPLAY_PLACES: u8 = 5;

pub fn parse_units(input: &str, decimals: u8) -> Result<U256, String> {
    let text = input.trim();
    if text.is_empty() {
        return Err("enter an amount".into());
    }
    if text.starts_with('-') {
        return Err("an amount cannot be negative".into());
    }
    if text.bytes().filter(|b| *b == b'.').count() > 1
        || !text.bytes().all(|b| b.is_ascii_digit() || b == b'.')
    {
        return Err(format!(
            "'{input}' is not an amount — use digits and at most one decimal point"
        ));
    }
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() && fraction.is_empty() {
        return Err("enter an amount".into());
    }
    if fraction.len() > decimals as usize {
        return Err(format!(
            "that amount has {} decimals; at most {decimals} are allowed",
            fraction.len()
        ));
    }
    let mut digits = format!("{whole}{fraction}");
    digits.extend(std::iter::repeat_n('0', decimals as usize - fraction.len()));
    U256::from_str_radix(&digits, 10).map_err(|_| "that amount is too large".into())
}

pub fn parse_u256_any(input: &str) -> Option<U256> {
    let text = input.trim();
    let (digits, radix) = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .map(|digits| (digits, 16))
        .unwrap_or((text, 10));
    (!digits.is_empty())
        .then(|| U256::from_str_radix(digits, radix).ok())
        .flatten()
}

pub fn resolve_amount(
    amount: Option<&str>,
    amount_units: Option<&str>,
    decimals: u8,
    symbol: &str,
) -> Result<U256, String> {
    match (amount, amount_units) {
        (Some(_), Some(_)) => Err("send either `amount` or `amountUnits`, not both".into()),
        (None, None) => Err("no amount".into()),
        (Some(raw), None) => {
            parse_u256_any(raw).ok_or_else(|| format!("amount '{raw}' is not a number"))
        }
        (None, Some(display)) => parse_units(display, decimals).map_err(|e| {
            if e.contains("at most") {
                format!("{symbol} has {decimals} decimals; {e}")
            } else {
                e
            }
        }),
    }
}

fn decimal(raw: &str) -> Option<&str> {
    let text = raw.trim();
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())).then_some(text)
}

pub fn format_exact(raw: &str, decimals: u8) -> Option<String> {
    let digits = decimal(raw)?;
    if decimals == 0 {
        return Some(digits.trim_start_matches('0').to_string()).map(|v| {
            if v.is_empty() {
                "0".into()
            } else {
                v
            }
        });
    }
    let width = decimals as usize + 1;
    let padded = format!(
        "{}{}",
        "0".repeat(width.saturating_sub(digits.len())),
        digits
    );
    let split = padded.len() - decimals as usize;
    let whole = padded[..split].trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let fraction = padded[split..].trim_end_matches('0');
    Some(if fraction.is_empty() {
        whole.into()
    } else {
        format!("{whole}.{fraction}")
    })
}

pub fn format_display(raw: &str, decimals: u8) -> Option<String> {
    let value = U256::from_str_radix(decimal(raw)?, 10).ok()?;
    if value.is_zero() {
        return Some("0".into());
    }
    let places = DISPLAY_PLACES.min(decimals);
    if places == 0 {
        return Some(value.to_string());
    }
    let threshold = U256::from(10).checked_pow(U256::from(decimals - places))?;
    if value < threshold {
        return Some(format!("<0.{}1", "0".repeat(places as usize - 1)));
    }
    let exact = format_exact(raw, decimals)?;
    let Some((whole, fraction)) = exact.split_once('.') else {
        return Some(exact);
    };
    let cut = fraction[..fraction.len().min(places as usize)].trim_end_matches('0');
    Some(if cut.is_empty() {
        whole.into()
    } else {
        format!("{whole}.{cut}")
    })
}

pub fn decorate(value: &mut Value, key: &str, raw: &str, decimals: u8) {
    if let Some(display) = format_display(raw, decimals) {
        value[format!("{key}Display")] = json!(display);
    }
    if let Some(exact) = format_exact(raw, decimals) {
        value[format!("{key}Exact")] = json!(exact);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_human_units_exactly() {
        assert_eq!(
            parse_units("0.1", 18).unwrap().to_string(),
            "100000000000000000"
        );
        assert_eq!(parse_units("1.000001", 6).unwrap().to_string(), "1000001");
        assert_eq!(parse_units(".5", 2).unwrap().to_string(), "50");
    }

    #[test]
    fn refuses_ambiguous_or_lossy_amounts() {
        for value in ["", "-1", "1e18", "1_000", "1.2.3", "0x1"] {
            assert!(parse_units(value, 18).is_err(), "{value}");
        }
        assert!(parse_units("1.0000001", 6).is_err());
    }

    #[test]
    fn exact_rendering_keeps_every_digit() {
        assert_eq!(
            format_exact("1234567890123456789", 18).as_deref(),
            Some("1.234567890123456789")
        );
        assert_eq!(
            format_exact("1", 18).as_deref(),
            Some("0.000000000000000001")
        );
        assert_eq!(format_exact("0", 6).as_deref(), Some("0"));
    }

    #[test]
    fn display_never_turns_dust_into_zero() {
        assert_eq!(format_display("1", 18).as_deref(), Some("<0.00001"));
        assert_eq!(format_display("0", 18).as_deref(), Some("0"));
    }

    #[test]
    fn base_amount_keeps_legacy_semantics() {
        assert_eq!(
            resolve_amount(Some("0x2a"), None, 18, "ETH").unwrap(),
            U256::from(42)
        );
        assert!(resolve_amount(Some("1"), Some("1"), 18, "ETH").is_err());
    }
}
