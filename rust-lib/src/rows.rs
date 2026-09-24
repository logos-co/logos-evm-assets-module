//! Decorate sender history with the assets the caller gave for each row's own chain.

use alloy::primitives::Address;
use serde_json::{json, Value};

use crate::assets::Asset;
use crate::units;

/// EIP-7708's emitter. Its Transfer logs move ether: it is never a token contract.
const SYSTEM_ADDRESS: &str = "0xfffffffffffffffffffffffffffffffffffffffe";

fn is_system(address: &str) -> bool {
    address.eq_ignore_ascii_case(SYSTEM_ADDRESS)
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn at<'a>(offered: &'a [Asset], address: &str) -> Option<&'a Asset> {
    // Whatever a token list claims, the system address names no token.
    if is_system(address) {
        return None;
    }
    offered.iter().find(|asset| {
        asset
            .address
            .as_deref()
            .is_some_and(|a| a.eq_ignore_ascii_case(address))
    })
}

fn checksum(value: &str) -> String {
    value
        .parse::<Address>()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| value.into())
}

pub fn decorate_row(row: &mut Value, offered: &[Asset]) {
    let meta = row.get("meta").cloned().unwrap_or(Value::Null);
    if text(&meta, "kind") == Some("erc20") {
        row["kind"] = json!("erc20");
        if let Some(token) = text(&meta, "token") {
            row["token"] = json!(checksum(token));
        }
        if let Some(recipient) = text(&meta, "recipient") {
            row["to"] = json!(checksum(recipient));
        }
        if let Some(symbol) = text(&meta, "tokenSymbol") {
            row["tokenSymbol"] = json!(symbol);
            row["valueSymbol"] = json!(symbol);
        }
        if let (Some(amount), Some(decimals)) = (
            text(&meta, "amount"),
            meta.get("tokenDecimals")
                .and_then(Value::as_u64)
                .and_then(|n| u8::try_from(n).ok()),
        ) {
            row["value"] = json!(amount);
            row["valueDecimals"] = json!(decimals);
            units::decorate(row, "value", amount, decimals);
        }
        for key in ["totalWei", "totalWeiDisplay", "totalWeiExact"] {
            row.as_object_mut().map(|object| object.remove(key));
        }
    }
    if let Some(target) = text(row, "txTo").map(str::to_string) {
        let recipient = text(row, "to").unwrap_or("");
        row["interactedWithDiffers"] = json!(!target.eq_ignore_ascii_case(recipient));
        if let Some(asset) = at(offered, &target) {
            row["interactedWithSymbol"] = json!(asset.symbol);
        }
    }
    if let Some(transfers) = row.get_mut("transfers").and_then(Value::as_array_mut) {
        // A sender that predates EIP-7708 files the system address's ether logs here.
        transfers.retain(|t| !is_system(text(t, "contract").unwrap_or("")));
        for transfer in transfers.iter_mut() {
            let contract = text(transfer, "contract").unwrap_or("").to_string();
            let asset = at(offered, &contract);
            transfer["known"] = json!(asset.is_some());
            if let Some(asset) = asset {
                transfer["symbol"] = json!(asset.symbol);
                transfer["decimals"] = json!(asset.decimals);
                if let Some(amount) = text(transfer, "amount").map(str::to_string) {
                    units::decorate(transfer, "amount", &amount, asset.decimals);
                }
            }
        }
    }
    if row.get("transfers").and_then(Value::as_array).is_some_and(Vec::is_empty) {
        row.as_object_mut().map(|object| object.remove("transfers"));
    }
    // Ether transfers are priced in the chain's own asset, and only where it has a symbol.
    let native = offered.iter().find(|asset| asset.native && !asset.symbol_unknown);
    if let (Some(native), Some(entries)) =
        (native, row.get_mut("nativeTransfers").and_then(Value::as_array_mut))
    {
        for entry in entries {
            entry["symbol"] = json!(native.symbol);
            entry["decimals"] = json!(native.decimals);
            if let Some(amount) = text(entry, "amount").map(str::to_string) {
                units::decorate(entry, "amount", &amount, native.decimals);
            }
        }
    }
}

pub fn decorate_history(value: &mut Value, mut offered_for: impl FnMut(u64) -> Vec<Asset>) {
    if let Some(rows) = value.get_mut("transactions").and_then(Value::as_array_mut) {
        let mut current_chain = None;
        let mut offered = Vec::new();
        for row in rows {
            let chain_id = row
                .get("chainId")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            if current_chain != Some(chain_id) {
                offered = offered_for(chain_id);
                current_chain = Some(chain_id);
            }
            decorate_row(row, &offered);
        }
    }
}

/// Attach display metadata from the authoritative chain registry to every history row.
/// The sender owns transaction state; the assets module owns how a chain-scoped asset row is
/// presented, so consumers never need their own chain-name join.
pub fn decorate_history_networks(
    value: &mut Value,
    mut record_for: impl FnMut(u64) -> Option<Value>,
) {
    if let Some(rows) = value.get_mut("transactions").and_then(Value::as_array_mut) {
        for row in rows {
            let chain_id = row
                .get("chainId")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let Some(record) = record_for(chain_id) else {
                continue;
            };
            if let Some(name) = record.get("name") {
                row["network"] = name.clone();
            }
            if let Some(symbol) = record.get("nativeSymbol") {
                row["nativeSymbol"] = symbol.clone();
            }
            if let Some(testnet) = record.get("testnet") {
                row["testnet"] = testnet.clone();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weth() -> Asset {
        Asset {
            chain_id: 1,
            symbol: "WETH".into(),
            name: "Wrapped Ether".into(),
            decimals: 18,
            address: Some("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".into()),
            native: false,
            builtin: true,
            enabled: true,
            source: "builtin".into(),
            logo_uri: None,
            symbol_unknown: false,
        }
    }

    #[test]
    fn an_erc20_send_is_read_back_as_a_transfer() {
        let mut row = json!({"kind":"call","to":"0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            "txTo":"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2","value":"0","totalWei":"7",
            "meta":{"kind":"erc20","token":"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
                    "recipient":"0x0000000000000000000000000000000000000002","amount":"1000000000000000000",
                    "tokenSymbol":"WETH","tokenDecimals":18}});
        decorate_row(&mut row, &[weth()]);
        assert_eq!(row["kind"], "erc20");
        assert_eq!(row["valueExact"], "1");
        assert!(row.get("totalWei").is_none());
    }

    #[test]
    fn receipt_transfers_use_the_offered_contract_table() {
        let mut row = json!({"transfers":[{"contract":"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2","amount":"1"}]});
        decorate_row(&mut row, &[weth()]);
        assert_eq!(row["transfers"][0]["symbol"], "WETH");
        assert_eq!(row["transfers"][0]["known"], true);
    }

    fn ether() -> Asset {
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

    /// EIP-7708: the system address logs ether with the ERC-20 Transfer topic. However a sender
    /// or a token list presents it, it is never decorated, looked up or named as a token.
    #[test]
    fn a_system_emitted_transfer_is_never_a_token() {
        let system = "0xfffffffffffffffffffffffffffffffffffffffe";
        let fake = Asset {
            symbol: "FAKE".into(),
            address: Some(system.to_uppercase().replace("0X", "0x")),
            ..weth()
        };
        let mut row = json!({"txTo": system, "to": system, "transfers": [
            {"contract": system, "amount": "5"},
            {"contract": "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2", "amount": "1"}]});
        decorate_row(&mut row, &[weth(), fake.clone()]);
        let transfers = row["transfers"].as_array().unwrap();
        assert_eq!(transfers.len(), 1, "{row}");
        assert_eq!(transfers[0]["symbol"], "WETH");
        assert!(row.get("interactedWithSymbol").is_none(), "no token lives at the system address");

        let mut only = json!({"transfers": [{"contract": system, "amount": "5"}]});
        decorate_row(&mut only, &[fake]);
        assert!(only.get("transfers").is_none(), "no token moved, so no token list: {only}");
    }

    #[test]
    fn ether_transfers_are_priced_in_the_chains_native_asset() {
        let mut row = json!({"nativeTransfers": [{"amount": "500000000000000000"}]});
        decorate_row(&mut row, &[ether(), weth()]);
        let entry = &row["nativeTransfers"][0];
        assert_eq!((entry["symbol"].clone(), entry["decimals"].clone()), (json!("ETH"), json!(18)));
        assert_eq!(entry["amountDisplay"], "0.5");
        assert!(entry.get("known").is_none(), "ether is not looked up in a token table");

        let nameless = Asset { symbol: String::new(), symbol_unknown: true, ..ether() };
        let mut row = json!({"nativeTransfers": [{"amount": "5"}]});
        decorate_row(&mut row, &[nameless]);
        assert_eq!(row["nativeTransfers"][0], json!({"amount": "5"}), "no unit, so no figure");
    }

    #[test]
    fn each_history_row_uses_its_own_chain() {
        let mut history = json!({"transactions":[{"chainId":1},{"chainId":10}]});
        let mut seen = Vec::new();
        decorate_history(&mut history, |chain| {
            seen.push(chain);
            Vec::new()
        });
        assert_eq!(seen, [1, 10]);
    }

    #[test]
    fn tokens_decorate_only_the_chain_they_are_given_for() {
        let given = json!({"1":[{"address":weth().address,"symbol":"WETH","decimals":18}]});
        let tokens = crate::assets::parse_tokens_by_chain(&given.to_string()).unwrap();
        let transfers =
            json!([{"contract":"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2","amount":"1"}]);
        let mut history = json!({"transactions":[{"chainId":1,"transfers":transfers},
                                                 {"chainId":10,"transfers":transfers}]});
        decorate_history(&mut history, |chain| {
            tokens.get(&chain).cloned().unwrap_or_default()
        });
        assert_eq!(history["transactions"][0]["transfers"][0]["symbol"], "WETH");
        assert_eq!(history["transactions"][1]["transfers"][0]["known"], false);
    }

    #[test]
    fn history_rows_receive_chain_display_metadata() {
        let mut history = json!({"transactions":[{"chainId":1}]});
        decorate_history_networks(&mut history, |chain| {
            (chain == 1).then(|| {
                json!({
                    "name":"Ethereum", "nativeSymbol":"ETH", "testnet":false
                })
            })
        });
        assert_eq!(history["transactions"][0]["network"], "Ethereum");
        assert_eq!(history["transactions"][0]["nativeSymbol"], "ETH");
        assert_eq!(history["transactions"][0]["testnet"], false);
    }
}
