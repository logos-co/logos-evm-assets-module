//! Logos boundary for reusable EVM asset operations.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use alloy::primitives::{Address, U256};
use serde_json::{json, Value};

use crate::assets::{self, Asset, TokenSort};
use crate::budget::{deadline, Budget, LOCAL, READ, RPC, TRANSFER};
use crate::transfer::{self, TransferRequest};
use crate::verified::{self, Answer};
use crate::{codec, rows, units};

pub trait EvmAssetsModule: Send + Sync + 'static {
    /// Native currency followed by the given ERC-20 descriptors, as asset rows.
    fn list_assets(&self, chain_id: i64, tokens_json: String) -> String;
    /// One Multicall3 read for native and every given token balance.
    fn get_balances(
        &self,
        chain_id: i64,
        address: String,
        tokens_json: String,
        token_sort: String,
    ) -> String;
    /// Build exactly one unsigned native or ERC-20 call, resolved among native and the
    /// request's `tokens`. Never signs or broadcasts.
    fn build_transfer(&self, chain_id: i64, request_json: String) -> String;
    /// Resolve native or one given token by exact address or unambiguous symbol.
    fn resolve_asset(&self, chain_id: i64, key: String, tokens_json: String) -> String;
    /// Decorate sender history with `{ "<chainId>": [descriptors] }`, native only for a chain
    /// the map omits. A chain-registry failure leaves that chain's rows intact, reported in
    /// `decorationErrors`; it never erases usable activity from the other chains.
    fn decorate_history(&self, history_json: String, tokens_json: String) -> String;
    fn on_context_ready(&self, _ctx: &RustModuleContext) {}
}

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/generated/provider_gen.rs"
));

#[derive(Default)]
struct EvmAssetsModuleImpl;

fn err(error: impl std::fmt::Display) -> String {
    let message = error.to_string();
    if let Ok(value) = serde_json::from_str::<Value>(&message) {
        if value.get("ok").and_then(Value::as_bool) == Some(false) {
            return value.to_string();
        }
    }
    json!({"ok":false,"error":message}).to_string()
}

fn ok_value(raw: String) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(value.to_string());
    }
    Ok(value)
}

fn asset_value(asset: &Asset) -> Value {
    let mut value = serde_json::to_value(asset).unwrap_or_else(|_| json!({}));
    value.as_object_mut().map(|object| {
        object.remove("chainId");
    });
    value
}

impl EvmAssetsModuleImpl {
    fn chain_record(&self, chain_id: u64, budget: &Budget) -> Result<Value, String> {
        let timeout = budget
            .take(LOCAL)
            .ok_or("no time left to read chain metadata")?;
        let raw = modules()
            .eth_rpc_module
            .list_chain_configs_with_timeout(timeout)
            .map_err(|e| format!("eth_rpc_module: {e:?}"))?;
        let value = ok_value(raw)?;
        value
            .get("chains")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|record| record.get("chainId").and_then(Value::as_u64) == Some(chain_id))
            .cloned()
            .ok_or_else(|| format!("chain {chain_id} is not configured"))
    }

    /// The chain's native asset, then the caller's tokens.
    fn offered(
        &self,
        chain_id: u64,
        tokens: Vec<Asset>,
        budget: &Budget,
    ) -> Result<Vec<Asset>, String> {
        let record = self.chain_record(chain_id, budget)?;
        let mut assets = vec![Asset::from_chain(&record)?];
        assets.extend(tokens);
        Ok(assets)
    }

    fn rpc_call(&self, chain_id: u64, call: Value, budget: &Budget) -> Result<Answer, String> {
        let timeout = budget.take(RPC).ok_or("no time left for the RPC read")?;
        let raw = modules()
            .eth_rpc_module
            .call_with_timeout(
                chain_id as i64,
                &call.to_string(),
                deadline(timeout),
                timeout,
            )
            .map_err(|e| format!("eth_rpc_module: {e:?}"))?;
        verified::unwrap_answer(&raw)
    }

    fn token_balance(
        &self,
        chain_id: u64,
        token: Address,
        owner: Address,
        budget: &Budget,
    ) -> Result<(U256, Option<String>), String> {
        let call = json!({"to":token.to_string(),"data":format!("0x{}", hex::encode(codec::balance_of(owner)))});
        let answer = self.rpc_call(chain_id, call, budget)?;
        let bytes = answer
            .value
            .as_str()
            .and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())
            .ok_or("eth_call returned no decodable token balance")?;
        let held = (bytes.len() >= 32)
            .then(|| U256::from_be_slice(&bytes[..32]))
            .ok_or("token balance response was shorter than uint256")?;
        Ok((held, answer.route))
    }
}

impl EvmAssetsModule for EvmAssetsModuleImpl {
    fn list_assets(&self, chain_id: i64, tokens_json: String) -> String {
        if chain_id < 0 {
            return err("chainId must be non-negative");
        }
        let tokens = match assets::parse_tokens(chain_id as u64, &tokens_json) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let budget = Budget::new(READ);
        match self.offered(chain_id as u64, tokens, &budget) {
            Ok(assets) => json!({"ok":true,"chainId":chain_id,"tokens":assets.iter().map(asset_value).collect::<Vec<_>>()}).to_string(),
            Err(error) => err(error),
        }
    }

    fn get_balances(
        &self,
        chain_id: i64,
        address: String,
        tokens_json: String,
        token_sort: String,
    ) -> String {
        if chain_id < 0 {
            return err("chainId must be non-negative");
        }
        let Some(sort) = TokenSort::parse(&token_sort) else {
            return err("tokenSort must be alpha, balance, or empty");
        };
        let owner = match address.trim().parse::<Address>() {
            Ok(v) => v,
            Err(e) => return err(format!("invalid address: {e}")),
        };
        let tokens = match assets::parse_tokens(chain_id as u64, &tokens_json) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let budget = Budget::new(READ);
        let offered = match self.offered(chain_id as u64, tokens, &budget) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let mut calls = Vec::new();
        for asset in &offered {
            if asset.native {
                calls.push((codec::multicall3(), codec::native_balance(owner)));
            } else if let Some(token) = asset
                .address
                .as_deref()
                .and_then(|a| a.parse::<Address>().ok())
            {
                calls.push((token, codec::balance_of(owner)));
            }
        }
        let call = json!({"to":codec::multicall3().to_string(),"data":format!("0x{}",hex::encode(codec::aggregate(&calls)))});
        let Answer { value, route } = match self.rpc_call(chain_id as u64, call, &budget) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let decoded = value
            .as_str()
            .and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())
            .and_then(|bytes| codec::decode_aggregate(&bytes));
        let Some(decoded) = decoded else {
            return err("could not decode the Multicall3 response");
        };
        let balances = assets::balance_rows(&offered, &decoded, sort);
        json!({"ok":true,"chainId":chain_id,"address":address,"tokenSort":sort.as_str(),
               "balances":balances,"route":verified::fold_route([route.as_deref()])})
        .to_string()
    }

    fn resolve_asset(&self, chain_id: i64, key: String, tokens_json: String) -> String {
        if chain_id < 0 {
            return err("chainId must be non-negative");
        }
        let tokens = match assets::parse_tokens(chain_id as u64, &tokens_json) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let budget = Budget::new(READ);
        let offered = match self.offered(chain_id as u64, tokens, &budget) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        match assets::resolve(&offered, &key) {
            Ok(asset) => {
                json!({"ok":true,"chainId":chain_id,"asset":asset_value(&asset)}).to_string()
            }
            Err(error) => err(error),
        }
    }

    fn build_transfer(&self, chain_id: i64, request_json: String) -> String {
        if chain_id < 0 {
            return err("chainId must be non-negative");
        }
        let request: TransferRequest = match serde_json::from_str(&request_json) {
            Ok(v) => v,
            Err(e) => return err(format!("invalid transfer request: {e}")),
        };
        let tokens = match assets::tokens(chain_id as u64, &request.tokens) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let allowance = request
            .deadline_ms
            .and_then(|ms| u64::try_from(ms).ok())
            .map(Duration::from_millis)
            .map(|d| d.min(TRANSFER))
            .unwrap_or(TRANSFER);
        let budget = Budget::new(allowance);
        let offered = match self.offered(chain_id as u64, tokens, &budget) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let resolved = match transfer::resolve(chain_id as u64, &request, &offered) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let mut route = None;
        if !resolved.asset.native {
            let token = resolved
                .asset
                .address
                .as_deref()
                .and_then(|a| a.parse::<Address>().ok())
                .expect("validated offered token");
            let (held, used) =
                match self.token_balance(chain_id as u64, token, resolved.from, &budget) {
                    Ok(v) => v,
                    Err(e) => return err(e),
                };
            if let Err(error) = transfer::token_affordable(
                held,
                resolved.amount,
                &resolved.asset.symbol,
                resolved.asset.decimals,
            ) {
                return err(error);
            }
            route = used;
        }
        let amount = resolved.amount.to_string();
        let call = transfer::call(&resolved);
        json!({"ok":true,"chainId":chain_id,"from":resolved.from.to_string(),"to":resolved.to.to_string(),
               "native":resolved.asset.native,"symbol":resolved.asset.symbol,"decimals":resolved.asset.decimals,
               "tokenAddress":resolved.asset.address,"nativeSymbol":resolved.native_symbol,"amount":amount,
               "amountDisplay":units::format_display(&amount,resolved.asset.decimals),
               "amountExact":units::format_exact(&amount,resolved.asset.decimals),"calls":[call],
               "purpose":transfer::purpose(&resolved),"route":verified::fold_route([route.as_deref()])}).to_string()
    }

    fn decorate_history(&self, history_json: String, tokens_json: String) -> String {
        let mut value: Value = match serde_json::from_str(&history_json) {
            Ok(v) => v,
            Err(e) => return err(format!("invalid history: {e}")),
        };
        if !value.is_object() {
            return err("history must be a JSON object");
        }
        let mut tokens = match assets::parse_tokens_by_chain(&tokens_json) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let chains: Vec<u64> = value
            .get("transactions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| row.get("chainId").and_then(Value::as_u64))
            .collect();
        let budget = Budget::new(READ);
        let mut cache = BTreeMap::new();
        let mut records = BTreeMap::new();
        let mut attempted = BTreeSet::new();
        let mut decoration_errors = Vec::new();
        for chain in chains {
            if !attempted.insert(chain) {
                continue;
            }
            let record = match self.chain_record(chain, &budget) {
                Ok(v) => v,
                Err(error) => {
                    decoration_errors.push(json!({"chainId":chain,"error":error}));
                    continue;
                }
            };
            let mut offered = match Asset::from_chain(&record) {
                Ok(v) => vec![v],
                Err(error) => {
                    decoration_errors.push(json!({"chainId":chain,"error":error}));
                    continue;
                }
            };
            offered.extend(tokens.remove(&chain).unwrap_or_default());
            cache.insert(chain, offered);
            records.insert(chain, record);
        }
        rows::decorate_history(&mut value, |chain| {
            cache.get(&chain).cloned().unwrap_or_default()
        });
        rows::decorate_history_networks(&mut value, |chain| records.get(&chain).cloned());
        if !decoration_errors.is_empty() {
            value["decorationErrors"] = json!(decoration_errors);
        }
        value.to_string()
    }
}

#[no_mangle]
pub extern "Rust" fn logos_module_install() {
    install::<EvmAssetsModuleImpl>();
}
