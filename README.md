# logos-evm-assets-module

Reusable EVM asset composition for wallets and dapps. The module combines chain-native
currency metadata from `eth_rpc_module` with the ERC-20 tokens its caller names in each
request; it does not decide which tokens exist. It owns balance reads, exact amount
conversion, unsigned transfer construction, and transaction-history decoration.

It deliberately has no keystore or sender dependency. It cannot request approval, sign, or
broadcast a transaction; callers receive one unsigned call and decide how to submit it.

On a fresh profile it asks `eth_rpc_module` to apply its own defaults. Only an explicit
`unconfigured` status licenses that write. Startup is best effort, and every public fact read
retries until the provider explicitly reports a settled configuration, so an early IPC or
capability-token race cannot leave the process permanently unconfigured.

## Contract

```text
list_assets(chain_id, tokens_json)
get_balances(chain_id, address, tokens_json, token_sort)
resolve_asset(chain_id, key, tokens_json)
build_transfer(chain_id, request_json)
decorate_history(history_json, tokens_json)
```

`tokens_json` is a JSON array of token descriptors, `token_list_module`'s `list_offered` rows
passed through unchanged: `{ address, symbol, decimals, name?, builtin?, enabled?, source?,
logoURI? }`; `""` means `[]`. The native asset is never a descriptor: it always comes from the
chain registry. A descriptor with a bad or zero address, an empty symbol, or missing or
out-of-range decimals is refused as `{ "ok": false, "code": "bad_token", "error":
"tokens[<i>]: <why>" }`. A repeated address keeps its first occurrence.

`list_assets` returns the native asset first, followed by the given tokens as asset rows.
Native symbol and decimals come only from the chain registry; missing metadata is reported,
never guessed.

`get_balances` batches the native and ERC-20 reads in one Multicall3 request. An unreadable
leg is distinct from a zero balance. Every raw amount is a decimal string and is accompanied
by exact and display-safe rendering when the read succeeded. The proof-backed RPC hop is
bounded at twelve seconds: long enough for a multi-token proof on a healthy verified proxy,
while the whole asset read remains bounded at fourteen seconds.

`build_transfer` accepts:

```json
{
  "from": "0x...",
  "to": "0x...",
  "amountUnits": "1.5",
  "token": "USDC",
  "tokens": [{ "address": "0x...", "symbol": "USDC", "decimals": 6 }],
  "deadlineMs": 4500
}
```

`token` or `tokenAddress` resolves against the native asset and the request's `tokens`.
`amount` is already in base units; `amountUnits` is a human decimal and is scaled exactly.
Supplying both is refused. A symbol that names multiple given contracts is refused unless
the exact `tokenAddress` is supplied. ERC-20 transfers make one `balanceOf` read before
returning one `transfer` call. Native affordability and fees remain the sender's concern.

`decorate_history` takes `tokens_json` as `{ "<chainId>": [descriptors] }` and decorates each
row with its own chain's tokens; a chain absent from the map decorates with its native asset
only. A bad descriptor there is refused as `tokens["<chainId>"][<i>]: <why>`. A chain whose
registry record cannot be read keeps its rows intact and is reported in `decorationErrors`.

RPC calls always flow through `eth_rpc_module`. Its structured `verified_blocked` refusal is
relayed unchanged, including the verdict. Successful replies include the weakest route used.

## Events

None. Token membership is the caller's input, so its change feed is the caller's too; native
metadata changes arrive as `eth_rpc_module`'s `chain_config_changed`.

## Build and test

```bash
cargo test --manifest-path rust-lib/Cargo.toml --no-default-features --locked
nix build .#default .#lgx
```
