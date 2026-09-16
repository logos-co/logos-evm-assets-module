//! Pure transfer resolution and the unsigned call handed to a sender by a composer.

use alloy::primitives::{Address, U256};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::assets::{self, Asset};
use crate::{codec, units};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub amount: Option<String>,
    #[serde(default)]
    pub amount_units: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub token_address: Option<String>,
    #[serde(default)]
    pub deadline_ms: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct Resolved {
    pub chain_id: u64,
    pub from: Address,
    pub to: Address,
    pub asset: Asset,
    pub amount: U256,
    pub native_symbol: String,
}

pub fn resolve(
    chain_id: u64,
    request: &TransferRequest,
    offered: &[Asset],
) -> Result<Resolved, String> {
    let from = request
        .from
        .trim()
        .parse::<Address>()
        .map_err(|e| format!("invalid `from` address: {e}"))?;
    let to = request
        .to
        .trim()
        .parse::<Address>()
        .map_err(|e| format!("invalid `to` address: {e}"))?;
    let native = offered
        .iter()
        .find(|asset| asset.native)
        .cloned()
        .ok_or_else(|| format!("chain {chain_id} has no native asset metadata"))?;
    let key = request
        .token_address
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or(request.token.as_deref())
        .unwrap_or(&native.symbol);
    let asset = assets::resolve(offered, key)?;
    let amount = units::resolve_amount(
        request.amount.as_deref(),
        request.amount_units.as_deref(),
        asset.decimals,
        &asset.symbol,
    )?;
    Ok(Resolved {
        chain_id,
        from,
        to,
        amount,
        native_symbol: native.symbol,
        asset,
    })
}

pub fn token_affordable(
    held: U256,
    amount: U256,
    symbol: &str,
    decimals: u8,
) -> Result<(), String> {
    if amount <= held {
        return Ok(());
    }
    let asked =
        units::format_exact(&amount.to_string(), decimals).unwrap_or_else(|| amount.to_string());
    let available =
        units::format_exact(&held.to_string(), decimals).unwrap_or_else(|| held.to_string());
    Err(format!(
        "cannot send {asked} {symbol}; the account holds {available} {symbol}"
    ))
}

pub fn purpose(resolved: &Resolved) -> String {
    let amount = units::format_exact(&resolved.amount.to_string(), resolved.asset.decimals)
        .unwrap_or_else(|| resolved.amount.to_string());
    format!(
        "Send {amount} {} from {} to {}",
        resolved.asset.symbol, resolved.from, resolved.to
    )
}

pub fn call(resolved: &Resolved) -> Value {
    let meta = if resolved.asset.native {
        json!({ "kind": "native", "recipient": resolved.to.to_string(),
                "amount": resolved.amount.to_string() })
    } else {
        json!({ "kind": "erc20", "token": resolved.asset.address, "tokenSymbol": resolved.asset.symbol,
                "tokenDecimals": resolved.asset.decimals, "recipient": resolved.to.to_string(),
                "amount": resolved.amount.to_string() })
    };
    if resolved.asset.native {
        json!({ "to": resolved.to.to_string(), "value": format!("0x{:x}", resolved.amount),
                "data": "0x", "label": format!("Send {}", resolved.asset.symbol), "meta": meta })
    } else {
        let contract = resolved
            .asset
            .address
            .as_deref()
            .and_then(|a| a.parse::<Address>().ok())
            .expect("offered token addresses are validated");
        json!({ "to": contract.to_string(), "value": "0x0",
                "data": format!("0x{}", hex::encode(codec::transfer(resolved.to, resolved.amount))),
                "label": format!("Send {}", resolved.asset.symbol), "meta": meta })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native() -> Asset {
        Asset {
            chain_id: 1,
            symbol: "ETH".into(),
            name: "Ether".into(),
            decimals: 18,
            address: None,
            native: true,
            builtin: true,
            enabled: true,
            source: "native".into(),
            logo_uri: None,
            symbol_unknown: false,
        }
    }
    fn req() -> TransferRequest {
        serde_json::from_value(json!({"from":"0x0000000000000000000000000000000000000001",
            "to":"0x0000000000000000000000000000000000000002","amountUnits":"1.5"}))
        .unwrap()
    }

    #[test]
    fn native_request_builds_one_plain_call() {
        let resolved = resolve(1, &req(), &[native()]).unwrap();
        assert_eq!(resolved.amount.to_string(), "1500000000000000000");
        let call = call(&resolved);
        assert_eq!(call["data"], "0x");
        assert_eq!(call["meta"]["kind"], "native");
    }

    #[test]
    fn purpose_names_every_human_fact() {
        let resolved = resolve(1, &req(), &[native()]).unwrap();
        let line = purpose(&resolved);
        let from = resolved.from.to_string();
        let to = resolved.to.to_string();
        for text in ["1.5 ETH", from.as_str(), to.as_str()] {
            assert!(line.contains(text));
        }
    }

    #[test]
    fn overdrawn_token_is_refused_in_token_units() {
        let error =
            token_affordable(U256::from(1_000_000), U256::from(2_000_000), "USDC", 6).unwrap_err();
        assert!(error.contains("2 USDC") && error.contains("1 USDC"));
    }

    #[test]
    fn deadline_is_a_caller_budget_not_an_asset_fact() {
        let mut request = req();
        request.deadline_ms = Some(4500);
        assert_eq!(resolve(1, &request, &[native()]).unwrap().chain_id, 1);
    }
}
