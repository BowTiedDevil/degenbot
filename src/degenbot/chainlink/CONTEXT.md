# Context — Chainlink Price Oracles

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **Price Feed** | An on-chain contract that provides the nominal price of an asset in a reference currency (e.g., ETH/USD) | Oracle, price contract |
| **Aggregator** | The underlying contract that stores and updates the price answer | Price source |
| **Round Data** | A single price observation containing round ID, answer, started-at, updated-at, and answered-in-round | Price update, round |
| **Latest Answer** | The most recent price value from the aggregator | Current price, spot price |

## Relationships

- A **Price Feed** wraps an **Aggregator** contract and exposes a simplified `price` property
- An **Erc20Token** may reference a **Price Feed** for USD-denominated price discovery

## Resolved ambiguities

### Price Feed vs Oracle

**Ruling: **Price Feed** for the Chainlink proxy contract. **Oracle** as the abstract concept. Use "Chainlink Price Feed" specifically, "oracle" generically.**

- ✅ "The ETH/USD **Price Feed** returns 1,500.00"
- ✅ "We need an **oracle** for this token — use a Chainlink Price Feed"
- ❌ "The Chainlink oracle returned 1,500.00" (use **Price Feed**)

## Known issues

### Bot dependency for RPC access (resolved by the PyO3 cutover, task `3O2ZPN`)

`ChainlinkPriceContract` retains its `bot: Bot | None` parameter for caller
compatibility, but its `decimals` and `price` properties now delegate to the
Rust `PyChainlinkPriceFeed` reader (the `degenbot-price` core crate, ADR-005).
The shell builds the Rust reader lazily via `bot.provider.to_alloy_provider()`
(`ProviderAdapter.to_alloy_provider` resolves the held `AlloyProvider` directly
for alloy-backed bots, or builds one from the underlying web3 IPC path / HTTP
endpoint), so `eth_call` + canonical ABI decode run in Rust with no Python
`provider.call_raw` / `abi_decode` round-trip. The float `price` stays Python
(display layer: `float(answer) / 10**decimals`), preserving the prior
float-exact behavior. The prior I/O-free-architecture concern is addressed —
the mechanism is Rust-owned; the `bot` parameter is now a thin handle to the
provider, not a direct RPC coupling.
