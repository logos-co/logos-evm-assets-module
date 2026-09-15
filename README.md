# logos-evm-assets-module

Reusable EVM asset composition for wallets and dapps. The module combines chain-native
currency metadata from `eth_rpc_module` with the pinned and user-enabled ERC-20 set from
`token_list_module`. It owns balance reads, exact amount conversion, unsigned transfer
construction, and transaction-history decoration.

It deliberately has no keystore or sender dependency. It cannot request approval, sign, or
broadcast a transaction; callers receive one unsigned call and decide how to submit it.

On a fresh profile it asks both fact providers to apply their own defaults. Only an explicit
`unconfigured` status licenses that write. Startup is best effort, and every public fact read
retries until both providers explicitly report a settled configuration, so an early IPC or
capability-token race cannot leave the process permanently unconfigured.

## Contract

```text
list_offered(chain_id)
list_available(chain_id, query, offset, limit)
get_balances(chain_id, address, token_sort)
resolve_asset(chain_id, key)
build_transfer(chain_id, request_json)
decorate_history(history_json)
```

`list_offered` returns the native asset first, followed by built-in and enabled ERC-20 rows.
Native symbol and decimals come only from the chain registry; missing metadata is reported,
never guessed. `list_available` preserves the token-list module's offered-first paging while
giving a matching native asset the first slot on page zero.

`get_balances` batches the native and ERC-20 reads in one Multicall3 request. An unreadable
leg is distinct from a zero balance. Every raw amount is a decimal string and is accompanied
by exact and display-safe rendering when the read succeeded.

`build_transfer` accepts:

```json
{
  "from": "0x...",
  "to": "0x...",
  "amountUnits": "1.5",
  "token": "ETH",
  "deadlineMs": 4500
}
```

`amount` is already in base units; `amountUnits` is a human decimal and is scaled exactly.
Supplying both is refused. A symbol that names multiple offered contracts is refused unless
the exact `tokenAddress` is supplied. ERC-20 transfers make one `balanceOf` read before
returning one `transfer` call. Native affordability and fees remain the sender's concern.

RPC calls always flow through `eth_rpc_module`. Its structured `verified_blocked` refusal is
relayed unchanged, including the verdict. Successful replies include the weakest route used.

## Events

`offered_changed(chainId)` is emitted when token membership or chain-native metadata changes.

## Build and test

```bash
cargo test --manifest-path rust-lib/Cargo.toml --no-default-features --locked
nix build .#default .#lgx
```
