//! Asset rows shared by offers, balances, transfer resolution and history decoration.

use std::cmp::Reverse;

use alloy::primitives::{Address, U256};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::units;

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub chain_id: u64,
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    pub native: bool,
    pub builtin: bool,
    pub enabled: bool,
    pub source: String,
    #[serde(rename = "logoURI", default, skip_serializing_if = "Option::is_none")]
    pub logo_uri: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub symbol_unknown: bool,
}

impl Asset {
    pub fn from_chain(record: &Value) -> Result<Self, String> {
        let chain_id = record
            .get("chainId")
            .and_then(Value::as_u64)
            .ok_or("chain record has no chainId")?;
        let symbol = record
            .get("nativeSymbol")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let decimals = record
            .get("nativeDecimals")
            .and_then(Value::as_u64)
            .and_then(|n| u8::try_from(n).ok())
            .ok_or_else(|| format!("chain {chain_id} has no usable nativeDecimals metadata"))?;
        Ok(Self {
            chain_id,
            name: record
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&symbol)
                .to_string(),
            symbol_unknown: symbol.is_empty(),
            symbol,
            decimals,
            address: None,
            native: true,
            builtin: true,
            enabled: true,
            source: "native".into(),
            logo_uri: None,
        })
    }

    pub fn from_token_row(chain_id: u64, row: &Value) -> Option<Self> {
        let address = row.get("address")?.as_str()?.trim();
        address.parse::<Address>().ok()?;
        let symbol = row.get("symbol")?.as_str()?.trim();
        if symbol.is_empty() {
            return None;
        }
        Some(Self {
            chain_id,
            symbol: symbol.into(),
            name: row
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(symbol)
                .into(),
            decimals: row
                .get("decimals")
                .and_then(Value::as_u64)
                .and_then(|n| u8::try_from(n).ok())?,
            address: Some(address.into()),
            native: false,
            builtin: row.get("builtin").and_then(Value::as_bool).unwrap_or(false),
            enabled: row.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            source: row
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .into(),
            logo_uri: row
                .get("logoURI")
                .and_then(Value::as_str)
                .map(str::to_string),
            symbol_unknown: false,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TokenSort {
    #[default]
    Alpha,
    Balance,
}

impl TokenSort {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "alpha" => Some(Self::Alpha),
            "balance" => Some(Self::Balance),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alpha => "alpha",
            Self::Balance => "balance",
        }
    }
}

pub fn resolve(offered: &[Asset], key: &str) -> Result<Asset, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("no asset was named".into());
    }
    if key.parse::<Address>().is_ok() {
        return offered
            .iter()
            .find(|asset| {
                asset
                    .address
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(key))
            })
            .cloned()
            .ok_or_else(|| format!("no offered token exists at {key}"));
    }
    let mut hits: Vec<Asset> = offered
        .iter()
        .filter(|asset| asset.symbol.eq_ignore_ascii_case(key))
        .cloned()
        .collect();
    if let Some(native) = hits.iter().position(|asset| asset.native) {
        return Ok(hits.swap_remove(native));
    }
    match hits.len() {
        0 => Err(format!("asset '{key}' is not offered")),
        1 => Ok(hits.remove(0)),
        _ => Err(format!(
            "'{key}' names {} offered contracts ({}); use an address",
            hits.len(),
            hits.iter()
                .filter_map(|a| a.address.as_deref())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

pub fn balance_rows(offered: &[Asset], raw: &[Option<U256>], sort: TokenSort) -> Vec<Value> {
    let mut rows: Vec<Value> = offered
        .iter()
        .enumerate()
        .map(|(index, asset)| {
            let mut row = serde_json::to_value(asset).unwrap_or_else(|_| json!({}));
            row.as_object_mut().map(|object| {
                object.remove("chainId");
                object.remove("enabled");
                object.remove("source");
                object.remove("logoURI");
            });
            let amount = raw.get(index).and_then(|value| *value);
            row["raw"] = json!(amount.map(|v| v.to_string()).unwrap_or_default());
            if let Some(value) = amount {
                units::decorate(&mut row, "amount", &value.to_string(), asset.decimals);
                row["display"] = row.get("amountDisplay").cloned().unwrap_or(Value::Null);
                row["exact"] = row.get("amountExact").cloned().unwrap_or(Value::Null);
            }
            row
        })
        .collect();
    rows.sort_by_cached_key(|row| {
        let after_native = row.get("native").and_then(Value::as_bool) != Some(true);
        let symbol = row
            .get("symbol")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let address = row
            .get("address")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let amount = row
            .get("raw")
            .and_then(Value::as_str)
            .and_then(|s| U256::from_str_radix(s, 10).ok());
        let (band, value) = match sort {
            TokenSort::Alpha => (0, U256::ZERO),
            TokenSort::Balance => match amount {
                Some(value) if !value.is_zero() => (0, value),
                None => (1, U256::ZERO),
                Some(_) => (2, U256::ZERO),
            },
        };
        (after_native, band, Reverse(value), symbol, address)
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(symbol: &str, address: &str) -> Asset {
        Asset {
            chain_id: 1,
            symbol: symbol.into(),
            name: symbol.into(),
            decimals: 18,
            address: Some(address.into()),
            native: false,
            builtin: false,
            enabled: true,
            source: "enabled".into(),
            logo_uri: None,
            symbol_unknown: false,
        }
    }

    #[test]
    fn an_address_is_an_unambiguous_identity() {
        let rows = [
            token("LIT", "0x0000000000000000000000000000000000000001"),
            token("LIT", "0x0000000000000000000000000000000000000002"),
        ];
        assert!(resolve(&rows, "LIT").is_err());
        assert_eq!(
            resolve(&rows, "0x0000000000000000000000000000000000000002").unwrap(),
            rows[1]
        );
    }

    #[test]
    fn the_native_symbol_wins_a_collision() {
        let mut native = token("ETH", "0x0000000000000000000000000000000000000001");
        native.native = true;
        native.address = None;
        assert!(
            resolve(
                &[
                    native.clone(),
                    token("ETH", "0x0000000000000000000000000000000000000002")
                ],
                "ETH"
            )
            .unwrap()
            .native
        );
    }

    #[test]
    fn balance_rows_keep_native_first() {
        let mut native = token("ETH", "0x0000000000000000000000000000000000000001");
        native.native = true;
        native.address = None;
        let rows = balance_rows(
            &[
                token("ZERO", "0x0000000000000000000000000000000000000002"),
                native,
            ],
            &[Some(U256::ZERO), Some(U256::from(1))],
            TokenSort::Balance,
        );
        assert_eq!(rows[0]["symbol"], "ETH");
    }

    #[test]
    fn unread_is_not_zero() {
        let rows = balance_rows(
            &[token("T", "0x0000000000000000000000000000000000000001")],
            &[None],
            TokenSort::Alpha,
        );
        assert_eq!(rows[0]["raw"], "");
        assert!(rows[0].get("display").is_none());
    }

    #[test]
    fn unknown_sort_is_refused() {
        assert_eq!(TokenSort::parse("alpha"), Some(TokenSort::Alpha));
        assert_eq!(TokenSort::parse("balance"), Some(TokenSort::Balance));
        assert_eq!(TokenSort::parse("value"), None);
    }
}
