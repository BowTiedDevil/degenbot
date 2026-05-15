# Context — Aave

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Market** | An Aave lending system comprising a **Pool contract**, its configurator, oracle, and all associated Assets, positions, and risk parameters | Aave pool, lending pool |
| **Asset** | An ERC20 Token plus its lending and borrowing state within an Aave Market: supply/borrow info, caps, APYs, collateral config, eMode, isolation mode, and price; the official Aave term across all versions; composes an **Erc20Token** (contract metadata) and an **AssetSummary** (protocol metrics); **never** use for DEX pool balances — those are **Reserves** (plural) | aave reserve |
| **Reserve** | The on-chain contract term for an **Asset** within Aave V3 (e.g., `getReserveData`, `ReserveConfigurationMap`); use **Asset** as the domain term, **Reserve** only when referring to the specific V3 contract storage or function names | — |
| **Collateral** | A Token deposited by a user as security for borrowing, represented by an aToken balance within an Asset | Deposit, supply |
| **Debt** | A Token borrowed by a user, represented by a vToken balance within an Asset | Loan, borrow |
| **aToken** | The interest-bearing Token minted to represent Collateral supplied to an Asset | Collateral token, aToken |
| **vToken** | The variable-rate debt Token tracking a user's borrowed amount plus accrued interest within an Asset | Debt token, variableDebtToken |
| **GHO** | Aave's native stablecoin with special discount mechanics for borrowers | — |
| **Health Factor** | The ratio of adjusted collateral value to debt value; below 1.0 the position can be liquidated | HF, safety factor |
| **Liquidation Threshold** | The percentage of collateral value usable for health factor calculation (e.g., 80% = 8000 bps) | LT |
| **Liquidation** | The forced repayment of a borrower's debt using their collateral when health factor falls below 1.0 | Liquidation event, liq |
| **Liquidation Pattern** | The on-chain event structure for multi-liquidations: SINGLE, COMBINED_BURN, or SEPARATE_BURNS | — |
| **Operation** | A user action on Aave: Supply, Withdraw, Borrow, Repay, Liquidation, etc. | Transaction, action |
| **Scaled Amount** | A token amount normalized by the current index (raw ÷ index), used for interest-accruing balance tracking | Normalized balance |
| **Raw Amount** | The actual token quantity before index-based scaling | Actual amount, wei amount |
| **Index** | The cumulative interest rate multiplier (liquidity index or borrow index) used to convert between raw and scaled amounts | Rate index, accumulator |
| **Enrichment** | The process of augmenting raw Aave events with computed scaled amounts and contextual data | — |
| **Processor** | A versioned component that calculates balance changes for a specific Aave contract revision and event type | Handler, calculator |
| **E-Mode** | Efficiency mode: higher LTV/liquidation thresholds for correlated assets within a category | High efficiency mode |
| **Isolation Mode** | A restriction where an asset can only be borrowed up to a debt ceiling, with no other assets usable as collateral | — |

## Relationships

- An **Aave Market** contains many **Assets**, each wrapping an **Erc20Token** plus lending state (supply/borrow info, caps, APYs, collateral config); the Market's **Pool contract** handles user-facing operations
- **Collateral** is represented by an **aToken** balance within an **Asset**; **Debt** is represented by a **vToken** balance within an **Asset**
- A **Health Factor** is computed from all **Collateral** and **Debt** positions of a single user
- A **Liquidation** occurs when a **Health Factor** drops below 1.0
- **GHO** debt uses a discount mechanism not present in standard **Debt**

## Cross-module rulings

- **Pool vs Market vs Pool Contract** — "Market" is the canonical term for an Aave lending system; "Pool" is reserved for DEX contracts. See [CONTEXT-MAP.md](../../../CONTEXT-MAP.md) for the full ruling with examples.
- **Asset vs Token** — "Asset" = ERC-20 token + lending state; "Token" = bare ERC-20 contract. See [CONTEXT-MAP.md](../../../CONTEXT-MAP.md) for the full ruling with examples.
- **Reserves (DEX) vs Asset (Aave)** — "Reserves" (plural) = DEX token balances; "Asset" = Aave lending state. See [CONTEXT-MAP.md](../../../CONTEXT-MAP.md) for the full ruling with examples.

## Example dialogue

> **Dev:** "The Aave **pool** has 8 reserves with different borrow rates."
> **Domain expert:** "Use **Market** and **Assets**. A **Market** is the Aave lending system; an **Asset** is one token's lending state within it. 'Pool' is for DEX contracts; 'Reserves' is for DEX token balances."
>
> **Dev:** "But the on-chain contract is literally called Pool.sol."
> **Domain expert:** "Right — you can say '**Pool contract**' when you mean the on-chain contract specifically. But the lending system as a whole is a **Market**. 'The Market's Pool contract emitted a Supply event' is fine."
>
> **Dev:** "And an **Asset** is different from a **Token**?"
> **Domain expert:** "Yes. A **Token** is just the ERC-20 contract — address, symbol, decimals. An **Asset** is the Token plus its lending state: supply/borrow rates, collateral config, caps. Every Asset wraps a Token; not every Token is an Asset."
>
> **Dev:** "What about the **Health Factor** — when does it trigger a liquidation?"
> **Domain expert:** "When it drops below 1.0. It's the ratio of adjusted **Collateral** value to **Debt** value. The threshold depends on each Asset's **Liquidation Threshold**."
>
> **Dev:** "And **Scaled Amounts** vs **Raw Amounts**?"
> **Domain expert:** "A **Raw Amount** is the actual token quantity. A **Scaled Amount** is normalized by the current **Index** — raw ÷ index. Scaled amounts are used for interest-accruing balance tracking because they stay constant between accruals."
