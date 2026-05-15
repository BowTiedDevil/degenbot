# Context — Tokens

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Token** | An ERC-20 compatible fungible token contract on an EVM chain; **always** use this term for ERC-20 contracts regardless of context | Coin |
| **Ether Placeholder** | A Token-like adapter for native ETH in pools that use the zero-address or all-Es convention | ETH token, WETH placeholder |
| **Wrapped Native Token** | The WETH/WETH-like ERC-20 token that wraps the chain's native currency | Native token wrapper |
| **Chain ID** | The integer identifying an EVM-compatible blockchain (e.g., 1 = Ethereum, 8453 = Base) | Network ID, chain id |

## Example dialogue

> **Dev:** "I need the **coin** address for USDC on Base."
> **Domain expert:** "Use **Token** — we don't say 'coin' here. The USDC **Token** address on Base (chain ID 8453) is 0x8335…"
>
> **Dev:** "And for native ETH in a pool — is that a **Token** too?"
> **Domain expert:** "It depends. If the pool uses the zero-address convention, it's represented by an **Ether Placeholder**, which behaves like a **Token** but stands in for native ETH. If the pool uses wrapped ETH, that's the **Wrapped Native Token** (WETH on Ethereum, WBNB on BSC)."
>
> **Dev:** "What about **Token0** and **Token1** — are those ERC-20 terms?"
> **Domain expert:** "No — that's a Uniswap pool ordering convention, not a Token concept. See the Uniswap context for that distinction."
