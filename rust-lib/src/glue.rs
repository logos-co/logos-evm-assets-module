//! Logos boundary for reusable EVM asset operations.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use alloy::primitives::{Address, U256};
use serde_json::{json, Value};

use crate::assets::{self, Asset, TokenSort};
use crate::budget::{deadline, Budget, INIT, LOCAL, PROBE, READ, RPC, STARTUP, TRANSFER};
use crate::depinit::{self, Next};
use crate::transfer::{self, TransferRequest};
use crate::verified::{self, Answer};
use crate::{codec, rows, units};

pub trait EvmAssetsModule: Send + Sync + 'static {
    /// Native currency followed by pinned and enabled ERC-20 rows.
    fn list_offered(&self, chain_id: i64) -> String;
    /// Native-aware, offered-first search and pagination.
    fn list_available(&self, chain_id: i64, query: String, offset: i64, limit: i64) -> String;
    /// One Multicall3 read for native and every offered token balance.
    fn get_balances(&self, chain_id: i64, address: String, token_sort: String) -> String;
    /// Build exactly one unsigned native or ERC-20 call. Never signs or broadcasts.
    fn build_transfer(&self, chain_id: i64, request_json: String) -> String;
    /// Resolve one offered asset by exact address or unambiguous symbol.
    fn resolve_asset(&self, chain_id: i64, key: String) -> String;
    /// Decorate sender history, selecting the offered set by every row's chainId. A
    /// catalogue failure on one chain leaves that chain's rows intact and is reported in
    /// `decorationErrors`; it never erases usable activity from the other chains.
    fn decorate_history(&self, history_json: String) -> String;
    fn on_context_ready(&self, _ctx: &RustModuleContext) {}
}

pub trait EvmAssetsModuleEvents {
    /// Native metadata or the ERC-20 offered set changed for one chain.
    fn offered_changed(&self, chain_id: i64);
}

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/generated/provider_gen.rs"
));

#[derive(Default)]
struct EvmAssetsModuleImpl {
    eth_rpc_settled: AtomicBool,
    token_list_settled: AtomicBool,
    watching_tokens: AtomicBool,
    watching_chains: AtomicBool,
}

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
    fn ensure_eth_rpc(&self, budget: &Budget) {
        if self.eth_rpc_settled.load(Ordering::Relaxed) {
            return;
        }
        let Some(timeout) = budget.take(PROBE) else {
            return;
        };
        let Ok(status) = modules().eth_rpc_module.config_status_with_timeout(timeout) else {
            return;
        };
        match depinit::next_step(&status) {
            Next::Settled => self.eth_rpc_settled.store(true, Ordering::Relaxed),
            Next::Initialize => {
                let Some(timeout) = budget.take(INIT) else {
                    return;
                };
                let initialized = modules()
                    .eth_rpc_module
                    .init_defaults_with_timeout(timeout)
                    .map(|raw| depinit::reply_ok(&raw))
                    .unwrap_or(false);
                if initialized {
                    self.eth_rpc_settled.store(true, Ordering::Relaxed);
                }
            }
            Next::AskAgain => {}
        }
    }

    fn ensure_token_list(&self, budget: &Budget) {
        if self.token_list_settled.load(Ordering::Relaxed) {
            return;
        }
        let Some(timeout) = budget.take(PROBE) else {
            return;
        };
        let Ok(status) = modules()
            .token_list_module
            .config_status_with_timeout(timeout)
        else {
            return;
        };
        match depinit::next_step(&status) {
            Next::Settled => self.token_list_settled.store(true, Ordering::Relaxed),
            Next::Initialize => {
                let Some(timeout) = budget.take(INIT) else {
                    return;
                };
                let initialized = modules()
                    .token_list_module
                    .init_defaults_with_timeout(timeout)
                    .map(|raw| depinit::reply_ok(&raw))
                    .unwrap_or(false);
                if initialized {
                    self.token_list_settled.store(true, Ordering::Relaxed);
                }
            }
            Next::AskAgain => {}
        }
    }

    /// Startup is best effort. Every public fact read calls this again until both providers
    /// explicitly report configured, so an early capability-token rejection cannot strand a
    /// fresh profile at chain 0 for the rest of the process lifetime.
    fn ensure_defaults(&self, budget: &Budget) {
        self.ensure_eth_rpc(budget);
        self.ensure_token_list(budget);
    }

    fn watch(&self) {
        if !self.watching_tokens.swap(true, Ordering::SeqCst) {
            let mut client = modules().token_list_module;
            match client.on_tokens_updated() {
                Ok(stream) => std::thread::spawn(move || {
                    for event in stream {
                        if let Some(event) =
                            token_list_module::TokenListModuleClient::decode_tokens_updated(&event)
                        {
                            emit_offered_changed(event.chain_id);
                        }
                    }
                }),
                Err(_) => {
                    self.watching_tokens.store(false, Ordering::SeqCst);
                    return;
                }
            };
        }
        if !self.watching_chains.swap(true, Ordering::SeqCst) {
            let mut client = modules().eth_rpc_module;
            match client.on_chain_config_changed() {
                Ok(stream) => std::thread::spawn(move || {
                    for event in stream {
                        if let Some(event) =
                            eth_rpc_module::EthRpcModuleClient::decode_chain_config_changed(&event)
                        {
                            emit_offered_changed(event.chain_id);
                        }
                    }
                }),
                Err(_) => {
                    self.watching_chains.store(false, Ordering::SeqCst);
                    return;
                }
            };
        }
    }

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

    fn token_rows(&self, chain_id: u64, budget: &Budget) -> Result<Vec<Value>, String> {
        let timeout = budget
            .take(LOCAL)
            .ok_or("no time left to read offered tokens")?;
        let raw = modules()
            .token_list_module
            .list_offered_with_timeout(chain_id as i64, timeout)
            .map_err(|e| format!("token_list_module: {e:?}"))?;
        let value = ok_value(raw)?;
        Ok(value
            .get("tokens")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    fn offered(&self, chain_id: u64, budget: &Budget) -> Result<Vec<Asset>, String> {
        let record = self.chain_record(chain_id, budget)?;
        let mut assets = vec![Asset::from_chain(&record)?];
        assets.extend(
            self.token_rows(chain_id, budget)?
                .iter()
                .filter_map(|row| Asset::from_token_row(chain_id, row)),
        );
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
    fn on_context_ready(&self, _ctx: &RustModuleContext) {
        let budget = Budget::new(STARTUP);
        self.ensure_defaults(&budget);
        self.watch();
    }

    fn list_offered(&self, chain_id: i64) -> String {
        if chain_id < 0 {
            return err("chainId must be non-negative");
        }
        let budget = Budget::new(READ);
        self.ensure_defaults(&budget);
        self.watch();
        match self.offered(chain_id as u64, &budget) {
            Ok(assets) => json!({"ok":true,"chainId":chain_id,"tokens":assets.iter().map(asset_value).collect::<Vec<_>>()}).to_string(),
            Err(error) => err(error),
        }
    }

    fn list_available(&self, chain_id: i64, query: String, offset: i64, limit: i64) -> String {
        if chain_id < 0 {
            return err("chainId must be non-negative");
        }
        let budget = Budget::new(READ);
        self.ensure_defaults(&budget);
        self.watch();
        let record = match self.chain_record(chain_id as u64, &budget) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let native = match Asset::from_chain(&record) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let needle = query.trim().to_ascii_lowercase();
        let native_matches = needle.is_empty()
            || native.symbol.to_ascii_lowercase().contains(&needle)
            || native.name.to_ascii_lowercase().contains(&needle);
        let offset = usize::try_from(offset).unwrap_or(0);
        let provider_offset = if native_matches {
            offset.saturating_sub(1)
        } else {
            offset
        };
        let timeout = match budget.take(LOCAL) {
            Some(t) => t,
            None => return err("no time left to read the token picker"),
        };
        let raw = match modules().token_list_module.list_available_with_timeout(
            chain_id,
            &query,
            provider_offset as i64,
            limit,
            timeout,
        ) {
            Ok(v) => v,
            Err(e) => return err(format!("token_list_module: {e:?}")),
        };
        let mut value = match ok_value(raw) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let provider_total = value.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
        let mut page = value
            .get_mut("tokens")
            .and_then(Value::as_array_mut)
            .map(std::mem::take)
            .unwrap_or_default();
        for row in &mut page {
            row["native"] = json!(false);
        }
        if native_matches && offset == 0 && limit != 0 {
            page.insert(0, asset_value(&native));
        }
        if let Ok(cut) = usize::try_from(limit) {
            if cut > 0 {
                page.truncate(cut);
            }
        }
        let total = provider_total + usize::from(native_matches);
        value["total"] = json!(total);
        value["offset"] = json!(offset);
        value["shown"] = json!(page.len());
        value["hasMore"] = json!(offset.saturating_add(page.len()) < total);
        value["tokens"] = json!(page);
        value.to_string()
    }

    fn get_balances(&self, chain_id: i64, address: String, token_sort: String) -> String {
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
        let budget = Budget::new(READ);
        self.ensure_defaults(&budget);
        let offered = match self.offered(chain_id as u64, &budget) {
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

    fn resolve_asset(&self, chain_id: i64, key: String) -> String {
        if chain_id < 0 {
            return err("chainId must be non-negative");
        }
        let budget = Budget::new(READ);
        self.ensure_defaults(&budget);
        let offered = match self.offered(chain_id as u64, &budget) {
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
        let allowance = request
            .deadline_ms
            .and_then(|ms| u64::try_from(ms).ok())
            .map(Duration::from_millis)
            .map(|d| d.min(TRANSFER))
            .unwrap_or(TRANSFER);
        let budget = Budget::new(allowance);
        self.ensure_defaults(&budget);
        let offered = match self.offered(chain_id as u64, &budget) {
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

    fn decorate_history(&self, history_json: String) -> String {
        let mut value: Value = match serde_json::from_str(&history_json) {
            Ok(v) => v,
            Err(e) => return err(format!("invalid history: {e}")),
        };
        if !value.is_object() {
            return err("history must be a JSON object");
        }
        let chains: Vec<u64> = value
            .get("transactions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| row.get("chainId").and_then(Value::as_u64))
            .collect();
        let budget = Budget::new(READ);
        self.ensure_defaults(&budget);
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
            match self.token_rows(chain, &budget) {
                Ok(token_rows) => offered.extend(
                    token_rows
                        .iter()
                        .filter_map(|row| Asset::from_token_row(chain, row)),
                ),
                Err(error) => decoration_errors.push(json!({"chainId":chain,"error":error})),
            }
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
