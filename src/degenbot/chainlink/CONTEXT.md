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

### Bot dependency for RPC access

`ChainlinkPriceContract` takes `bot: Bot | None` and reads `self._bot.provider` directly in its `decimals` and `price` properties. This bypasses the I/O-free architecture used by pool classes (which receive all data through builders and `external_update()`). A future refactoring should replace the `bot` parameter with a `ProviderAdapter` or `PoolIO` parameter, making the class testable without a live `Bot` instance.
