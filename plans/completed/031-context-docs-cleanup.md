# Plan 031: Clean Up Context Documentation

Align all `CONTEXT.md` files and `CONTEXT-MAP.md` with the grill-with-docs skill format: **a glossary and nothing else** — no implementation details, no code, no changelog, no debugging guides.

## Problem

The project's context documentation has drifted from the skill's core rule: *"CONTEXT.md should be totally devoid of implementation details. Do not treat CONTEXT.md as a spec, a scratch pad, or a repository for implementation decisions."*

Specific issues:

1. **No example dialogues** — the skill format requires one per module; only the root map has one
2. **curve/CONTEXT.md is a spec** — ~300 lines of debugging workflow, detection heuristics, formulas, error types, and exhaustive enum value listings belong in a reference doc, not a glossary
3. **Implementation details in glossaries** — code examples in types/, pool creation patterns and AI agent notes in uniswap/, benefits lists, "replaces X" changelog entries
4. **Duplicate definitions** — V4PoolKey in 3 modules, Pool Manager in 2, Fee representations with conflicting phrasing in 2, Pool Invariant still defined in uniswap/ despite Plan 020 rename
5. **Overlong definitions** — multi-sentence definitions with changelog and "avoid" notes embedded in the definition text rather than the aliases column
6. **Overly detailed CONTEXT-MAP.md summaries** — paragraph-length bullet entries listing every term and ruling
7. **Changelog in relationships** — plan references and "replaces" notes in cross-module relationships
8. **Missing cross-module ruling echo** — Aave context doesn't reference the Pool vs Market and Asset vs Token rulings that are centrally about its terms

## Steps

### Step 1 — Add example dialogues to all module CONTEXT.md files

Add a `## Example dialogue` section to each of the 8 module contexts. Each dialogue should exercise the trickiest distinctions in that module.

| Module | Key distinctions to exercise |
|--------|-----------------------------|
| `types/` | Pool Manager vs Factory (off-chain vs on-chain); PoolFamily vs Pool Invariant (identity vs dispatch); Fee representations across V2/V3/V4 |
| `uniswap/` | Pool vs Pool Manager vs PoolManager (3-way); V4 Pool ID vs address; Token0/Token1 ordering; Price vs Exchange Rate |
| `erc20/` | Token vs Coin; Ether Placeholder vs Wrapped Native Token |
| `registry/` | Registry vs Manager (passive vs active); Bot-owned registry vs module-level Pool Type Registry; Pool Type Registry vs the removed Pool Class Registry |
| `arbitrage/` | Solver vs Optimizer (single-path vs multi-path); Swap Vector vs Swap Amounts; EncodedCall vs PayloadComposer role |
| `aave/` | Market vs Pool (Aave vs DEX); Asset vs Token (lending state vs bare ERC-20); Reserve (contract term) vs Asset (domain term); Scaled vs Raw Amount |
| `curve/` | Coin vs Token; Stableswap vs Crypto pool; LendingRateFetcher vs the removed provider_call; Metapool vs Base Pool; variant enums vs the removed address frozensets |
| `connection/` | Bot session pattern; ConnectionManager class vs module; Pool State Message flow |

### Step 2 — Split curve/CONTEXT.md into glossary + reference doc

Extract all implementation-detail content from `curve/CONTEXT.md` into a new `docs/curve-pool-reference.md`.

**Move to reference doc:**
- "Debugging Swap Mismatches" section (Steps 1–4, common pitfalls, sUSD pattern, Y pool sub-variants, A_PRECISION notes)
- "Detection Heuristics" section (metapool detection steps, lending token detection table, coin indexing, precision multiplier warnings, crypto pool detection)
- "Crypto Pool Details" table (component → purpose → I/O required)
- "Crypto Pool Parameters" table
- "Error Types" table
- "Dynamic fee formula" in Crypto Pool Details
- Exhaustive enum value listings (SwapStyle values, MetapoolRateStyle values, MetapoolUnderlyingStyle values, LendingRateStyle values, DVariant values, YVariant values, YDVariant values) — replace with one-line summary of what the enum represents

**Keep in CONTEXT.md:**
- Term tables (trimmed to one-sentence definitions, aliases in alias column)
- Variant/Strategy enum terms (DVariant, YVariant, YDVariant, SwapStyle, etc.) with one-line definitions only
- Fetcher protocol table (term + purpose — trim "Called When" to a phrase, not a paragraph)
- Relationships
- Resolved ambiguities
- New example dialogue (from Step 1)

### Step 3 — Remove implementation details from types/ and uniswap/ contexts

**types/CONTEXT.md:**
- Remove code example in "I/O-Free Architecture Pattern" section
- Remove "Benefits" list (Testability, Async Flexibility, Separation of Concerns) — rationale lives in ADR-001
- Move the entire "I/O-Free Architecture Pattern (Fetcher Protocol)" subsection's implementation detail to a brief cross-reference to ADR-001 and curve/CONTEXT.md
- Remove code example for `PoolStrategies` / builder variant method if present
- Remove the `CurveStableswapPool(...)` construction example

**uniswap/CONTEXT.md:**
- Remove "Pool Creation Patterns" section (V2/V3/V4 code examples) — this is usage documentation
- Remove "Notes for AI Agents" section — instructions, not domain language
- Remove "Auto-Update" and "State Cache" terms if they're general programming concepts, not domain-specific

### Step 4 — Deduplicate term definitions

Establish a single authoritative home for each term. Other modules reference it rather than redefining.

| Term | Authoritative home | Currently duplicated in | Action |
|------|-------------------|------------------------|--------|
| **Pool Manager** (off-chain) | `types/CONTEXT.md` | `uniswap/CONTEXT.md` | Remove from uniswap/; add "See types context" note |
| **V4PoolKey** | `types/CONTEXT.md` (it's a type-level concept) | `arbitrage/CONTEXT.md`, `registry/CONTEXT.md` | Remove from arbitrage/ and registry/; cross-reference |
| **Fee** (generic + representations) | `types/CONTEXT.md` | `uniswap/CONTEXT.md` (fee section with different phrasing) | Remove fee ambiguity ruling from uniswap/; keep the version-specific notes in uniswap/ only where they add info not in types/ (e.g., V2 Fraction = `Fraction(3, 1000)`) |
| **Pool Invariant** | `types/hop_types.py` context (PoolInvariant for solver dispatch) | `uniswap/CONTEXT.md` still defines it as "mathematical relationship" | Remove from uniswap/ or redefine as a cross-reference to **PoolFamily** in types/ |
| **PoolFamily** | `types/CONTEXT.md` | Also mentioned in AGENTS.md and CONTEXT-MAP.md (fine — those are references, not definitions) | No change needed outside uniswap/ cleanup |

### Step 5 — Trim definitions to one sentence

Audit every term definition across all modules. Apply these rules:
- One sentence max
- Define what it IS, not what it does
- "Avoid" aliases go in the aliases column, not in the definition
- "Replaces X" / "formerly Y" changelog goes in plan documents, not definitions
- Remove parenthetical implementation details (e.g., "inherits from `AddressRegistry[AbstractLiquidityPool]`" — that's code, not domain language)

Specific trims:
- **Pool Type Registry** definition: remove "replaces the old Pool Class Registry, FACTORY_DEPLOYMENTS lookups, _KIND_TO_DESCRIPTOR, and _variant_from_class"
- **Asset** (Aave): move "never use for DEX pool balances" to cross-module ambiguity; shorten the composition description
- **PoolStrategies**: remove "passed to the pool constructor"
- All **Variant enum values** and **Strategy enum values** in curve/: move exhaustive listings to the reference doc; keep one-line enum term definitions

### Step 6 — Shorten CONTEXT-MAP.md bullet summaries

Replace paragraph-length entries with one-line summaries following the skill's format:

```md
- [Pool Types & Managers](src/degenbot/types/CONTEXT.md) — pool types, type resolution, managers, and fee representations
- [Uniswap](src/degenbot/uniswap/CONTEXT.md) — V2/V3/V4 pools, concentrated liquidity, tick mechanics
- [Tokens](src/degenbot/erc20/CONTEXT.md) — ERC-20 tokens, ether placeholder, chain ID
- [Pool Registries](src/degenbot/registry/CONTEXT.md) — address-based registries and pool type registry
- [Arbitrage, Solvers & Adapters](src/degenbot/arbitrage/CONTEXT.md) — arbitrage cycles, solvers, adapters, and swap encoding
- [Aave](src/degenbot/aave/CONTEXT.md) — lending markets, assets, collateral, debt, and liquidation
- [Curve StableSwap](src/degenbot/curve/CONTEXT.md) — StableSwap pools, fetcher protocols, variant and strategy enums
- [Infrastructure](src/degenbot/connection/CONTEXT.md) — providers, connection management, and the Bot session
```

Remove the `· Ambiguity rulings: ...` suffixes — those are discoverable by reading the module context.

### Step 7 — Remove plan references and "replaces" notes from cross-module relationships

In `CONTEXT-MAP.md`, edit the `## Cross-module relationships` section:

- Remove "(Plan 020)" from PoolFamily entry
- Remove "(Plan 019)" from CacheablePool entry
- Remove "(Plan 021)" from Swap Amounts and V4PoolKey entries
- Remove "ADR-001 Phase 3 complete" from V2/V3/V4/Aerodrome I/O-free entry
- Remove "replaces the former `PoolClassRegistry` (removed)" — the Pool Type Registry entry already defines what it is

These are historical artifacts. The plan documents and ADR-001 already record the history.

### Step 8 — Add cross-module ruling references to Aave context

Add a `## Cross-module rulings` section to `aave/CONTEXT.md` that references the two rulings in CONTEXT-MAP.md that are centrally about Aave terms:

```md
## Cross-module rulings

- **Pool vs Market vs Pool Contract** — "Market" is the canonical term for an Aave lending system; "Pool" is reserved for DEX contracts. See [CONTEXT-MAP.md](../../../CONTEXT-MAP.md) for the full ruling with examples.
- **Asset vs Token** — "Asset" = ERC-20 token + lending state; "Token" = bare ERC-20 contract. See [CONTEXT-MAP.md](../../../CONTEXT-MAP.md) for the full ruling with examples.
- **Reserves (DEX) vs Asset (Aave)** — "Reserves" (plural) = DEX token balances; "Asset" = Aave lending state. See [CONTEXT-MAP.md](../../../CONTEXT-MAP.md) for the full ruling with examples.
```

### Step 9 — Consider ADRs for qualifying decisions

Evaluate whether these decisions warrant ADRs (per the skill's three criteria: hard to reverse, surprising without context, result of a real trade-off):

| Decision | Hard to reverse? | Surprising? | Real trade-off? | Verdict |
|----------|-----------------|-------------|-----------------|---------|
| Pool Type Registry as module-level singleton (everything else is Bot-owned) | Yes — all DEX modules import it | Yes — why is this one global? | Yes — global static mapping vs per-session flexibility | ✅ Create ADR-002 |
| PoolFamily rename from PoolInvariant | Yes — all references updated | Yes — same word used differently in two modules | Yes — clarity vs backward compat (already paid) | ❌ Skip — already complete, backward compat cost already paid; Plan 020 records it |

Create `docs/adr/ADR-002-pool-type-registry-singleton.md`:

```md
# ADR-002: Pool Type Registry as Module-Level Singleton

The Pool Type Registry (`pool_type_registry`) is a module-level singleton, while all other registries (Pool, Token, Managed Pool) are class instances owned by Bot. This is intentional: the (chain ID, factory address) → class + identity + deployment data mapping is global knowledge that does not vary between Bot instances. Making it Bot-owned would require every DEX module to accept a Bot parameter at import time to register its classes, coupling module initialization to session creation. The singleton pattern allows each DEX module to self-register at import time via `pool_type_registry.register()`, independently of any Bot instance.
```

## Dependency graph

```
Step 1 (dialogues) ─────────────────────────── independent
Step 2 (split curve/) ──────────────────────── independent
Step 3 (remove impl details) ───────────────── independent
Step 4 (deduplicate) ────────────────────────── depends on Steps 2, 3 (removes content that may shift)
Step 5 (trim definitions) ───────────────────── depends on Step 4 (dedup first, then trim)
Step 6 (shorten map summaries) ──────────────── depends on Steps 2–5 (summaries should reflect final state)
Step 7 (remove plan refs from map) ──────────── independent
Step 8 (Aave cross-refs) ────────────────────── independent
Step 9 (ADR-002) ────────────────────────────── independent
```

Recommended order: 1 → 2 → 3 → 4 → 5 → 7 → 8 → 9 → 6 (map last, since it summarizes the final state).

## Scope

Documentation only. No code changes. No changes to AGENTS.md unless plan references there need updating after Step 7.

## Verification

After all steps, re-read each CONTEXT.md against the skill's CONTEXT-FORMAT.md and confirm:
- [ ] Every module has an example dialogue
- [ ] No implementation details (code, benefits, usage patterns, debugging guides) remain in any CONTEXT.md
- [ ] Every term has one authoritative definition; others cross-reference
- [ ] Every definition is one sentence max
- [ ] CONTEXT-MAP.md summaries are one line each
- [ ] No plan references in CONTEXT-MAP.md relationships
- [ ] Aave context references the cross-module rulings about its terms
- [ ] ADR-002 exists if the singleton decision qualifies
