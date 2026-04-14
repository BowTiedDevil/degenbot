# Aave V3 Protocol Documentation

Comprehensive control flow diagrams and amount transformations for debugging Aave V3 pool operations.

## Quick Navigation

### Core Operation Flows

| Operation | Entry Point | Key Transformation |
|-----------|-------------|-------------------|
| [Supply](./flows/supply.md) | `Pool.supply(asset, amount, onBehalfOf, referralCode)` | [amount → scaledBalance](./transformations/index.md#collateral-token-transformations) |
| [Withdraw](./flows/withdraw.md) | `Pool.withdraw(asset, amount, to)` | [scaledBalance → amount](./transformations/index.md#collateral-token-transformations) |
| [Borrow](./flows/borrow.md) | `Pool.borrow(asset, amount, interestRateMode, referralCode, onBehalfOf)` | [amount → scaledDebt](./transformations/index.md#debt-token-transformations) |
| [Repay](./flows/repay.md) | `Pool.repay(asset, amount, interestRateMode, onBehalfOf)` | [scaledDebt → amount](./transformations/index.md#debt-token-transformations) |
| [Liquidation](./flows/liquidation.md) | `Pool.liquidationCall(collateralAsset, debtAsset, user, debtToCover, receiveAToken)` | [debtValue → collateralAmount](./transformations/index.md#liquidation-calculations) |

### Amount Transformations Reference

- [Core Math Operations](./transformations/index.md#core-math-operations) - WadRayMath, PercentageMath
- [Collateral Token Transformations](./transformations/index.md#collateral-token-transformations) - aToken mint/burn/transfer
- [Debt Token Transformations](./transformations/index.md#debt-token-transformations) - Variable and stable debt
- [Interest Accrual](./transformations/index.md#interest-accrual) - Index updates
- [Treasury Accrual](./transformations/index.md#treasury-accrual) - Protocol fees
- [Liquidation Calculations](./transformations/index.md#liquidation-calculations) - Collateral seizure math
- [Flash Loan Premiums](./transformations/index.md#flash-loan-premiums) - Premium distribution
- [E-Mode Calculations](./transformations/index.md#e-mode-calculations) - Correlated asset pricing
- [Version Differences](./transformations/index.md#version-differences) - v1-3 vs v4+ rounding

---

## For Automated Agents

### Debugging Workflow

1. **Identify the operation** being investigated (supply, borrow, liquidation, etc.)
2. **Load the flow file** for that operation to see the execution path
3. **Follow the transformations** - refer to the transformations/index.md for detailed math
4. **Check validation** - each flow shows all validation checks and error conditions
5. **Verify state changes** - see exactly what storage is modified

### Common Debugging Patterns

```markdown
**Investigating balance discrepancy:**
1. Check [Collateral Transformations](./transformations/index.md#collateral-token-transformations)
2. Verify rounding mode (v1-3 vs v4+)
3. Check index at specific block height
4. Look for treasury accruals

**Investigating liquidation:**
1. Check [Liquidation Flow](./flows/liquidation.md)
2. Verify health factor calculation in [E-Mode](./transformations/index.md#e-mode-calculations)
3. Check collateral conversion math
4. Verify close factor logic

**Investigating failed transaction:**
1. Find the operation flow
2. Check "Error Conditions" section
3. Match error to validation check
4. See validation logic in Solidity
```

### Solidity Source Mapping

Each flow file includes:
- **File paths** to actual contract source
- **Function names** with line-level references
- **Complete code snippets** showing actual implementation
- **Transformation markers** linking to detailed math

Example:
```solidity
// File: contracts/protocol/libraries/logic/SupplyLogic.sol
// Function: executeSupply()

uint256 scaledAmount = amount.rayDiv(index);  // [TRANSFORMATION]
// Refer to transformations/index.md for:
// - Exact formula
// - Solidity implementation
// - Version differences
// - Edge cases
```

---

## Document Structure

```
docs/aave/
├── README.md                          # This file - navigation hub
├── AGENTS.md                          # Instructions for automated agents
├── flows/                             # Individual operation flows
│   ├── supply.md                      # Supply execution flow
│   ├── supply_with_permit.md           # Supply with permit
│   ├── withdraw.md                    # Withdraw execution flow
│   ├── borrow.md                      # Borrow execution flow
│   ├── repay.md                       # Repay execution flow
│   ├── repay_with_atokens.md          # Repay with aTokens
│   ├── repay_with_permit.md           # Repay with permit
│   ├── liquidation.md                 # Liquidation execution flow
│   ├── collateral_management.md       # Collateral enable/disable
│   ├── emode_management.md            # E-Mode category management
│   ├── flash_loan.md                  # Flash loan (with callback)
│   ├── flash_loan_simple.md           # Simple flash loan
│   ├── gho_borrowing.md               # GHO borrowing
│   ├── gho_discount.md                # GHO discount mechanism
│   ├── rewards_claiming.md            # Rewards claiming
│   ├── position_manager.md            # Position manager operations
│   ├── eliminate_deficit.md           # Umbrella deficit elimination
│   ├── stk_aave_staking.md           # stkAAVE staking
│   ├── stk_aave_unstaking.md          # stkAAVE unstaking
│   └── stk_aave_slashing.md           # stkAAVE slashing
└── transformations/                   # Amount transformation reference
    └── index.md                       # All math operations
```

---

## Key Concepts

### Scaled Balances vs. Unscaled Amounts

**Collateral (aTokens):**
- **Storage:** Scaled balance (`_scaledBalance[user]`)
- **Precision:** RAY (27 decimals)
- **Conversion:** `unscaled = scaled.rayMul(liquidityIndex)`
- **Interest:** Automatic via index growth

**Variable Debt:**
- **Storage:** Scaled balance (`_scaledBalance[user]`)
- **Precision:** RAY (27 decimals)
- **Conversion:** `unscaled = scaled.rayMul(borrowIndex)`
- **Interest:** Automatic via index growth

**Stable Debt:**
- **Storage:** Principal + timestamp
- **Precision:** WAD (18 decimals)
- **Conversion:** Calculate interest on-demand
- **Interest:** Compounded at borrow/repay time

### Index Updates

```solidity
// Liquidity Index (for collateral)
liquidityIndex[t] = liquidityIndex[t-1].rayMul(1 + rate * timeDelta / YEAR)

// Borrow Index (for variable debt)
borrowIndex[t] = borrowIndex[t-1].rayMul(compoundInterest(rate, timeDelta))
```

See [Interest Accrual](./transformations/index.md#interest-accrual) for full details.

### Rounding Modes

| Operation | v1-3 | v4+ | Reason |
|-----------|------|-----|---------|
| aToken Mint | `rayDiv` (half-up) | `rayDivFloor` | Prevent rounding up debt |
| aToken Burn | `rayDiv` (half-up) | `rayDivCeil` | Ensure full repayment |
| vToken Mint | `rayDiv` (half-up) | `rayDivCeil` | Round up debt issued |
| vToken Burn | `rayDiv` (half-up) | `rayDivFloor` | Round down debt reduction |

See [Version Differences](./transformations/index.md#version-differences).

---

## Aave V3 Contract Addresses (Mainnet)

```
Pool Proxy:           0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2
PoolAddressesProvider: 0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e
Oracle:               0x54586bE62E3c3580375aE3723C145253060Ca0C2
```

---

## Validation

All Mermaid diagrams in this documentation are validated using [Maid](https://probelabs.com/maid):

```bash
# Validate all diagrams
npx maid .

# Auto-fix issues
npx maid --fix .
```

---

## Contributing

When adding new flows or transformations:
1. Include complete Solidity code snippets
2. Add links between flows and transformations
3. Document all error conditions
4. Show version differences where applicable
5. Run `npx maid --fix` before committing
