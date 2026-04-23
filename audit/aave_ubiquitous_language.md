# Ubiquitous Language Audit: Aave Module

Audit of all files in `src/degenbot/aave/`, `src/degenbot/cli/aave/`, and `docs/aave/` for violations of the resolved ubiquitous language rulings.

---

## Ruling 1: Pool vs Market vs Pool Contract

**Pool** = DEX only. **Market** = Aave lending system. **Pool contract** = the on-chain Aave contract named Pool.sol. A Market *has* a Pool contract; they can coexist in context.

### Violations

#### `docs/aave/README.md:3`
> "Comprehensive control flow diagrams and amount transformations for debugging Aave V3 **pool** operations."

Should be: "…Aave V3 **Market** operations."

#### `docs/aave/transformations/index.md:747`
> | Operation | **Pool** v1-3 | **Pool** v4+ | Reason |

This is a comment about Aave Pool contract revisions, not DEX pools. Should say "**Pool contract** v1-3" / "**Pool contract** v4+" to clarify these are Aave contract revisions.

#### `src/degenbot/cli/aave/commands.py:910`
> "1. Bootstrap: Fetch and process proxy creation events to discover **Pool** and PoolConfigurator"

This refers to the on-chain Aave contract named "Pool". Acceptable — it's naming the contract. Could be clarified as "**Pool contract** and PoolConfigurator contract" to avoid ambiguity with DEX pools.

#### `src/degenbot/cli/aave/commands.py:1035-1036`
> "# **Pool** not initialized yet, skip to next chunk"
> "logger.warning(f'**Pool** not initialized for market {market.id}, skipping')"

The variable `pool` holds the Aave Pool contract. The log message and comment should say **Pool contract** to clarify the distinction from a DEX pool: "**Pool contract** not initialized yet" / `logger.warning(f"Pool contract not initialized for market {market.id}, skipping")`. The contract name `"POOL"` (the string literal) stays unchanged.

#### `src/degenbot/cli/aave/commands.py:1041`
> "pool_address=pool.address,"

Variable named `pool` — acceptable as a local variable holding the Pool contract reference. The parameter name `pool_address` is fine since it identifies the Pool contract's address (a specific on-chain contract), not a DEX pool.

#### `docs/aave/flows/gho_borrowing.md:482`
> "1. **No Actual Underlying Transfer**: GHO is minted by the facilitator, not transferred from the **pool**"

Should be: "…not transferred from the **Pool contract**" (since this refers to the on-chain contract's reserves)

#### `docs/aave/flows/gho_borrowing.md:238`
> "// For GHO: underlying is minted via facilitator, not transferred from **pool**"

In a Solidity code comment — should say **Pool contract** to clarify it's the on-chain contract, not a DEX pool.

### Notes

Many uses of "Pool" in the flow docs (`Pool.borrow(…)`, `Pool.supply(…)`, etc.) reference the **on-chain Aave V3 contract** which is literally named `Pool.sol`. These are Solidity function signatures and contract references — they should remain as-is since they are proper nouns from the protocol. The ruling applies to our *prose*, *comments*, and *docstrings*, not to verbatim protocol names and function signatures.

When a docstring or comment *describes* what the contract does in domain terms, use **Market** for the lending system and **Pool contract** for the on-chain contract:
- ✅ `contract_name="POOL"` — on-chain identifier, stays
- ✅ "The Market's **Pool contract** emitted a Supply event"
- ✅ "Fetch events from the **Pool contract** for this Market"
- ✅ "The **Pool contract** is at 0x8787…"
- ❌ "The **Pool** tracks reserves" (ambiguous — use **Market** or **Pool contract**)
- ❌ "The Aave **pool** has 8 reserves" (use **Market**)

---

## Ruling 2: Reserves (DEX) vs Asset (Aave)

**Reserves** (plural) = DEX token balances. **Asset** = Aave lending state for one token. **Reserve** (singular) = V3 contract-internal term only (e.g., `ReserveData`, `getReserveData`).

### Violations

#### `docs/aave/flows/liquidation.md:9` (entry point table)
> "`Pool.liquidationCall(collateralAsset, debtAsset, …)`"

The Solidity function signature uses `collateralAsset`/`debtAsset` — this is a protocol name and stays. But in our prose tables and descriptions, use **Asset**.

#### `docs/aave/flows/eliminate_deficit.md:325`
> "address indexed reserve, // **Asset** address whose deficit is covered"

Comment says "Asset address" — that's correct. But the Solidity parameter name is `reserve`. No violation; the comment correctly uses **Asset** as the domain term while the contract parameter stays as-is.

#### `docs/aave/flows/borrow.md:482`
> "address indexed reserve, // **Asset** address"

Same pattern — comment correctly says **Asset**. No violation.

#### `docs/aave/flows/flash_loan_simple.md:324`
> "address indexed **asset**, // **Asset** address"

No violation — uses **Asset** correctly.

#### `docs/aave/flows/rewards_claiming.md:703`
> "address **asset**; // **Asset** address"

No violation — uses **Asset** correctly.

#### `src/degenbot/cli/aave/db_assets.py:2`
> "**Asset** and token database operations for Aave V3."

No violation — uses **Asset** correctly.

#### `src/degenbot/cli/aave/db_assets.py:72`
> "Get GHO token **asset** for a given market."

Should be: "Get GHO **Asset** for a given Market." (capitalize **Asset** as a domain term, remove "token" qualifier)

#### `src/degenbot/cli/aave/db_assets.py:114`
> "Get AaveV3 **asset** by aToken (collateral) or vToken (debt) address."

Should be: "Get AaveV3 **Asset** by aToken or vToken address." (capitalize)

#### `src/degenbot/cli/aave/db_assets.py:149`
> "Get AaveV3 **asset** by underlying token address."

Should be: "Get AaveV3 **Asset** by underlying Token address." (capitalize both domain terms)

#### `src/degenbot/cli/aave/db_assets.py:165`
> "Get AaveV3 **asset** by ID."

Should be: "Get AaveV3 **Asset** by ID." (capitalize)

#### `src/degenbot/cli/aave/db_assets.py:194`
> "Get the vToken for an underlying **asset** address."

Should be: "…underlying Token address." (use **Token** for the ERC-20 contract address, not "asset address")

#### `src/degenbot/cli/aave/db_assets.py:210-214`
> "Get a human-readable identifier for an **asset**."
> "This provides consistent **asset** identification in debug logs and error messages."

Minor — lowercase "asset" instead of capitalized **Asset**. Should be **Asset** as a domain term.

#### `src/degenbot/aave/enrichment.py:84`
> "# 2. Get underlying **asset** address"

Should be: "Get underlying Token address" (referring to the ERC-20 contract address, not the Asset's lending state)

#### `src/degenbot/aave/enrichment.py:409`
> "msg = f'Could not find **asset** for token {token_address}'"

Should be: "Could not find **Asset** for token…" (capitalize)

#### `src/degenbot/aave/enrichment.py:416,437`
> "Get underlying **asset** address for a token."
> "msg = f'Could not find underlying **asset** for token {token_address}'"

Should be: "Get underlying Token address…" (referring to the ERC-20 contract) / "Could not find underlying Token…"

#### `src/degenbot/aave/models.py:111`
> "underlying_asset: ChecksumAddress = Field(description='Address of the underlying **asset**')"

Should be: "Address of the underlying **Token**" (the field is the ERC-20 contract address)

#### `src/degenbot/aave/models.py:333`
> "default=None, description='Address receiving underlying **asset**'"

Should be: "Address receiving underlying **Token**"

#### `src/degenbot/aave/operation_types.py:5`
> "Types of Aave operations based on **asset** flows."

Should be: "…based on **Asset** flows." (capitalize)

#### `src/degenbot/aave/position_analysis.py:16`
> "- **Asset** prices from Aave oracle (converts to common currency)"

No violation — uses **Asset** correctly.

#### `src/degenbot/aave/position_analysis.py:206,226`
> "liquidity_index: Current liquidity index for the **asset**"
> "borrow_index: Current borrow index for the **asset**"

Should be: "…for the **Asset**" (capitalize)

#### `src/degenbot/aave/position_analysis.py:267,270`
> "**asset** addresses: Set of **asset** addresses to fetch prices for"
> "Dict mapping **asset** address to price"

Should be: "Token addresses: Set of Token addresses to fetch prices for" / "mapping Token address to price" (these are ERC-20 contract addresses being looked up, not the Asset's lending state)

#### `src/degenbot/aave/position_analysis.py:296-319`
Multiple docstring references to "asset" in `_get_effective_liquidation_threshold` and `_get_effective_ltv`:
> "asset: The Aave V3 **asset** record"
> "emode_category_id: The **asset's** eMode category"
> "Check if user is in eMode and **asset** belongs to that category"
> "Use standard **asset** config threshold"

These are correct usages of **Asset** — just need capitalization. Should be **Asset** (capitalized as a domain term).

#### `src/degenbot/aave/position_analysis.py:360-361`
> "collateral_enabled: Whether user has enabled this **asset** as collateral"
> "price: Oracle price for the **asset**"

Should be: "…this **Asset** as collateral" / "Oracle price for the **Asset**" (capitalize)

#### `src/degenbot/aave/position_analysis.py:475`
> "isolation_debt_ceiling: Debt ceiling for isolation mode **asset**"

Should be: "…isolation mode **Asset**" (capitalize)

#### `src/degenbot/aave/position_analysis.py:525`
> "price_map: Map of **asset** address -> oracle price"

Should be: "Map of Token address -> oracle price" (these are ERC-20 contract addresses)

#### `src/degenbot/aave/position_analysis.py:602,605`
> "asset: AaveV3Asset,"
> "Get price for an **asset** from the price map."

Parameter named `asset` — this is fine and matches the domain term. Docstring should capitalize: "Get price for an **Asset** from the price map."

#### `src/degenbot/aave/position_analysis.py:647,688,695`
> "# Collect all unique **asset** addresses first"
> "# Fetch debt positions with full **asset** info"

These refer to Token addresses used to look up Assets. Should be: "Token addresses" / "full Asset info"

#### `src/degenbot/aave/position_analysis.py:737-762`
> "Collect all unique **asset** addresses for a market."
> "Set of underlying **asset** addresses"

Should be: "Token addresses" (these are ERC-20 contract addresses)

#### `src/degenbot/cli/aave/db_market.py:4`
> "Functions for managing market state, eMode categories, and **asset** configurations."

Should be: "…and **Asset** configurations." (capitalize)

#### `src/degenbot/cli/aave/db_market.py:67,82`
> "Get **asset** configuration by **asset** ID."
> "Get existing **asset** config or create new one with defaults."

Should be: "Get **Asset** configuration by **Asset** ID." (capitalize)

#### `src/degenbot/cli/aave/db_market.py:158,163-165`
> "**asset**: AaveV3Asset,"
> "Record an oracle price for an **asset**."
> "Updates the **asset's** last_known_price and last_price_block."

Parameter name `asset` is correct. Docstrings should capitalize: "Record an oracle price for an **Asset**." / "Updates the **Asset's**…"

#### `src/degenbot/cli/aave/db_users.py:28`
> "Queries the AaveV3Asset table to get the v_token_revision for the GHO **asset**."

Should be: "…GHO **Asset**." (capitalize)

#### `src/degenbot/cli/aave/event_handlers.py:47`
> "Process a CollateralConfigurationChanged event to update **asset** configuration."

Should be: "…update **Asset** configuration." (capitalize)

#### `src/degenbot/cli/aave/event_handlers.py:65-66`
> "# Find the **asset** in the database"
> "**asset** = session.scalar(…)"

Variable named `asset` — this is correct and matches the domain term.

#### `src/degenbot/cli/aave/event_handlers.py:278`
> "Updates the **asset's** eMode category assignment."

Should be: "Updates the **Asset's** eMode category assignment." (capitalize)

#### `src/degenbot/cli/aave/event_handlers.py:345-346`
> "This event is emitted when an **asset** is added or removed as collateral"
> "in an eMode category. The category_id in the event is the **asset's** primary"

Should be: "when an **Asset** is added or removed…" / "the **Asset's** primary…" (capitalize)

#### `src/degenbot/cli/aave/event_handlers.py:379`
> "# Only set e_mode_category_id if this **asset** is being added as collateral"

Should be: "…this **Asset** is being added…" (capitalize)

#### `src/degenbot/cli/aave/event_handlers.py:540`
> "Process a ReserveInitialized event to add a new Aave **asset** to the database."

Should be: "…add a new Aave **Asset** to the database." (capitalize)

#### `src/degenbot/cli/aave/event_handlers.py:554`
> "logger.debug(f'Processing **asset** initialization event at block {event['blockNumber']}')"

Should be: "Processing **Asset** initialization event…" (capitalize)

#### `src/degenbot/cli/aave/event_handlers.py:639`
> "logger.info(f'Added new Aave V3 **asset**: {asset.underlying_token!r}')"

Should be: "Added new Aave V3 **Asset**: …" (capitalize)

#### `src/degenbot/cli/aave/commands.py:887-890`
> "click.echo('  **Asset** List:')"
> "for **asset** in market_obj.assets:"

No violation — uses **Asset** correctly. The ORM relationship `.assets` also correctly uses the domain term.

#### `src/degenbot/cli/aave/commands.py:912`
> "2. **Asset** Discovery: Fetch all targeted events and build transaction contexts"

No violation — uses **Asset** correctly.

#### `src/degenbot/cli/aave/token_processor.py:302,432,515,767`
> "# Get collateral **asset**"
> "# Get collateral **asset** first for logging"
> "# Get debt **asset** first for logging"

Should be: "Get collateral **Asset**" / "Get debt **Asset**" (capitalize)

#### `src/degenbot/cli/aave/liquidation_processor.py:61`
> "Since operations are parsed from Pool events…"

This refers to the Aave Pool contract's events. Should be: "**Pool contract** events"

#### `src/degenbot/cli/aave/db_verification.py` (and `verification.py`)
Various uses of `debt_asset` parameter names and "asset" in docstrings.

Parameter names like `debt_asset` are correct (matching domain term). Docstrings should capitalize **Asset**.

#### `src/degenbot/cli/aave/types.py:82`
> "# Pool revision for TokenMath calculations"

This refers to the Aave Pool contract's revision number. Acceptable as-is since it's a contract-level concept (the `pool_revision` field stores the on-chain Pool contract revision). Could be clarified to "# Pool contract revision for TokenMath calculations" if desired.

### Docs violations (Asset vs Token distinction)

Several doc files use "asset" where they mean "Token" (the ERC-20 contract) rather than "Asset" (the token + lending state):

#### `docs/aave/flows/collateral_management.md:3`
> "End-to-end execution flow for enabling or disabling a supplied **asset** as collateral in Aave V3."

This is correct — it refers to the Asset's collateral configuration, not just the Token.

#### `docs/aave/flows/collateral_management.md:372,383`
> "Emitted when a user enables an **asset** as collateral."
> "Emitted when a user disables an **asset** as collateral."

Correct — refers to the Asset's collateral state.

#### `docs/aave/flows/collateral_management.md:403`
> "Has LTV0 collateral but trying to disable non-LTV0 **asset**"

Should be: "…non-LTV0 **Asset**" (capitalize)

#### `docs/aave/flows/emode_management.md:412`
> "User has borrowed **asset** not in target category's `borrowableBitmap`"

Should be: "…borrowed from an **Asset** not in…" (capitalize)

#### `docs/aave/flows/eliminate_deficit.md:327`
> "uint256 amountCovered // Amount of deficit covered (in underlying **asset**)"

Should be: "(in underlying **Token**)" (this is the ERC-20 token amount, not the Asset's state)

#### `docs/aave/flows/eliminate_deficit.md:333`
> "Emitted conditionally when the user burns their entire aToken balance for a collateral **asset**."

Should be: "…for a collateral **Asset**." (capitalize)

#### `docs/aave/flows/flash_loan_simple.md:3`
> "End-to-end execution flow for simple flash loans in Aave V3 (single **asset** only)."

Should be: "…(single **Asset** only)." (capitalize)

#### `docs/aave/flows/flash_loan_simple.md:130,132`
> "`flashLoanSimple` handles a single **asset** (no arrays)"
> "Lower gas overhead for single-**asset** flash loans"

Should be: "single **Asset**" / "single-**Asset** flash loans" (capitalize)

#### `docs/aave/flows/flash_loan_simple.md:349`
> "Flash Loan (Multi-**Asset**)"

No violation — already capitalized.

#### `docs/aave/flows/repay_with_atokens.md:3`
> "End-to-end execution flow for repaying debt using aTokens instead of underlying **assets** in Aave V3."

Should be: "…instead of underlying **Tokens**…" (referring to the ERC-20 contracts, not the Assets' lending state)

---

## Ruling 3: Asset vs Token

**Token** for all ERC-20 contracts. **Asset** for an ERC20 token plus its Aave lending state. **Reserve** is only the V3 on-chain contract term (e.g., `ReserveData`, `getReserveData`).

### Violations

The primary violation pattern is using lowercase "asset" where **Asset** (capitalized) is the domain term, or using "asset" / "asset address" where **Token** or **Token address** is meant (referring to the raw ERC-20 contract, not the lending wrapper).

#### Using "asset address" when meaning Token address

These refer to the ERC-20 contract address, not the Asset's composite state:

- `src/degenbot/aave/enrichment.py:84` — "Get underlying asset address" → **Token address**
- `src/degenbot/aave/enrichment.py:416,437` — "Get underlying asset address" → **Token address**
- `src/degenbot/aave/models.py:111` — "Address of the underlying asset" → **Token**
- `src/degenbot/aave/models.py:333` — "Address receiving underlying asset" → **Token**
- `src/degenbot/aave/position_analysis.py:267,270` — "asset addresses" → **Token addresses**
- `src/degenbot/aave/position_analysis.py:525` — "asset address" → **Token address**
- `src/degenbot/aave/position_analysis.py:647` — "asset addresses" → **Token addresses**
- `src/degenbot/aave/position_analysis.py:737-762` — "asset addresses" → **Token addresses**
- `src/degenbot/cli/aave/db_assets.py:194` — "underlying asset address" → **Token address**

#### Lowercase "asset" where **Asset** should be capitalized as a domain term

Nearly all remaining docstring/comment uses of lowercase "asset" in Aave context should be capitalized **Asset**. The full list is in Ruling 2 above — approximately 30 instances across `position_analysis.py`, `db_market.py`, `event_handlers.py`, `token_processor.py`, `db_assets.py`, `db_users.py`, and `db_verification.py`.

#### Using "reserve" as a domain term instead of Asset

No violations found. The codebase correctly uses `reserve` only in V3 contract-internal contexts (e.g., `ReserveData`, `getReserveData`, Solidity event parameter names). No prose or docstrings incorrectly use "Reserve" as a domain term where **Asset** should be used.

---

## Ruling 4: Factory (on-chain) vs Pool Manager (off-chain)

**Factory** = on-chain contract only. **Pool Manager** = off-chain class only.

### No violations found in the Aave module

The Aave module doesn't use Factory/Pool Manager terminology. No DEX Pool Managers are referenced.

---

## Ruling 5: Fee representations

Use **Fee** generically; qualify with specific representation when precision matters.

### No violations found in the Aave module

Fee representations are a DEX concept. The Aave module uses its own terminology (liquidity rate, borrow rate, etc.) which doesn't collide.

---

## Ruling 6: Solver vs Optimizer

**Solver** = single-path. **Optimizer** = multi-path.

### No violations found in the Aave module

Solver/Optimizer terminology is exclusive to the arbitrage subsystem.

---

## Summary

| Ruling | Violations | Severity |
|--------|-----------|----------|
| 1. Pool vs Market vs Pool Contract | ~8 prose/comment violations, plus many `Pool.function()` protocol references (acceptable) | Medium |
| 2. Reserves (DEX) vs Asset (Aave) | ~9 "asset address" → should be "Token address"; ~30 lowercase "asset" → should be capitalized **Asset** | Medium |
| 3. Asset vs Token | Same violations as Ruling 2 (consolidated there) | — |
| 4. Factory vs Pool Manager | 0 | — |
| 5. Fee representations | 0 | — |
| 6. Solver vs Optimizer | 0 | — |

### Priority recommendations

1. **High impact, low effort**: Fix all prose violations in `docs/aave/` markdown files. These are user-facing and create the most confusion for new contributors. Key patterns:
   - "asset address" → **Token address** (when referring to the ERC-20 contract)
   - lowercase "asset" → **Asset** (when referring to the Aave domain concept)
2. **High impact, medium effort**: Fix docstrings and comments in `src/degenbot/aave/` and `src/degenbot/cli/aave/`. Same patterns as above. Variable names like `asset` are already correct and don't need changing.
3. **No action needed on ORM model**: `AaveV3Asset` and the `.assets` relationship already use the correct domain term. The table name `aave_v3_assets` is also correct.

### Special cases

- **Solidity code and function signatures** (e.g., `Pool.borrow(asset, …)`, `collateralAsset`, `debtAsset`): These are protocol identifiers and should remain unchanged. Only the *comments describing them* should use our domain language.
- **Solidity event parameter names** (e.g., `address indexed reserve`): These are protocol identifiers and stay as-is. Comments should use **Asset** as the domain term.
- **Contract name `"POOL"`**: This is an on-chain registry identifier and stays as-is. Prose around it should say **Pool contract**.
- **`pool_revision` field**: Internally tracks the Aave Pool contract revision. The field name stays (it matches the on-chain `POOL.revision()` concept). Prose can say "Pool contract revision" for clarity.
- **`ReserveInitialized` event name**: This is the on-chain event name and stays as-is. Prose should say "**Asset** initialization" when discussing the domain concept.
