# Agent Instructions for Aave V3 Documentation

## Quick Start

**When investigating a specific Aave operation:**

1. **Identify the operation** (supply, borrow, liquidation, etc.)
2. **Load the flow file** from `flows/` directory (e.g., `flows/liquidation.md`)
3. **Follow the Mermaid diagram** - look for color coding:
   - **Red**: Validation checks (can revert)
   - **Green**: Math transformations
   - **Blue**: Storage updates
   - **Yellow**: Event emissions
   - **Magenta**: Bridge paths
   - **Red fill / Black stroke**: Error nodes
4. **Check transformations** by referring to `transformations/index.md`
5. **Review Solidity code** snippets for exact implementation details

## Documentation Structure

```
docs/aave/
├── README.md                    # Navigation hub and quick reference
├── AGENTS.md                    # This file - agent instructions
├── flows/                       # Individual operation flows
│   ├── supply.md               # Deposit flow
│   ├── supply_with_permit.md   # Supply with permit
│   ├── withdraw.md             # Withdrawal flow with HF validation
│   ├── borrow.md               # Borrowing flow (variable + stable)
│   ├── repay.md               # Repayment flow
│   ├── repay_with_atokens.md   # Repay with aTokens
│   ├── repay_with_permit.md    # Repay with permit
│   ├── liquidation.md          # Liquidation flow
│   ├── flash_loan.md           # Flash loan flow
│   ├── flash_loan_simple.md    # Simple flash loan
│   ├── collateral_management.md # Collateral enable/disable
│   ├── emode_management.md    # E-mode category management
│   ├── position_manager.md    # Position manager operations
│   ├── eliminate_deficit.md   # Deficit elimination
│   ├── gho_borrowing.md       # GHO borrowing flow
│   ├── gho_discount.md        # GHO discount mechanism
│   ├── rewards_claiming.md   # Rewards claiming
│   ├── stk_aave_staking.md   # StkAave staking
│   ├── stk_aave_unstaking.md # StkAave unstaking
│   └── stk_aave_slashing.md  # StkAave slashing
└── transformations/             # Amount transformation reference
    └── index.md                # All math operations with Solidity
```

## Navigation Guide

### For Flow Investigation
- Start with [README.md](./README.md) for operation overview
- Load specific flow file (e.g., [flows/liquidation.md](./flows/liquidation.md))
- Each flow includes:
  - Quick reference table (entry point, transformations, events)
  - Enhanced Mermaid diagram with color-coded nodes
  - Step-by-step Solidity code
  - Amount transformation details
  - Event details
  - Error conditions table
  - Related flows
  - Source file locations

### For Amount/Math Questions
- Check [transformations/index.md](./transformations/index.md)
- Sections organized by topic:
  - Core Math (WadRayMath, PercentageMath)
  - Collateral Token Transformations
  - Debt Token Transformations (variable + stable)
  - Interest Accrual (index updates)
  - Treasury Accrual
  - Liquidation Calculations
  - Flash Loan Premiums
  - E-Mode Calculations
  - Version Differences (v1-3 vs v4+)

## Example Debugging Workflows

### Investigating a Liquidation
```markdown
1. Load: flows/liquidation.md
2. Check Quick Reference table for entry point
3. Follow enhanced diagram - note red validation nodes
4. Review "Step-by-Step Execution" for Solidity code
5. Check "Amount Transformations" for collateral math
6. Verify "Error Conditions" match observed revert
```

### Investigating Balance Discrepancy
```markdown
1. Load: transformations/index.md
2. Check "Collateral Token Transformations" section
3. Verify rounding mode (v1-3 half-up vs v4+ floor/ceil)
4. Check "Interest Accrual" for index update formulas
5. Review "Version Differences" for behavior changes
6. Consider treasury accruals reducing LP returns
```

### Investigating Failed Transaction
```markdown
1. Identify operation type from transaction
2. Load corresponding flow file (e.g., flows/borrow.md)
3. Check "Error Conditions" table
4. Match error code to validation check in diagram
5. Review validation logic in Solidity snippets
6. Check if preconditions are met (e.g., HF, caps)
```

## Key Transformation Patterns

```
Collateral (aTokens):
  Storage: _scaledBalance[user] (RAY precision - 27 decimals)
  Unscaled: scaled.rayMul(liquidityIndex) (WAD precision - 18 decimals)
  Interest: Automatic via growing liquidityIndex

Variable Debt:
  Storage: _scaledBalance[user] (RAY precision - 27 decimals)
  Unscaled: scaled.rayMul(borrowIndex) (WAD precision - 18 decimals)
  Interest: Automatic via growing borrowIndex

Stable Debt:
  Storage: principal + timestamp (WAD precision - 18 decimals)
  Interest: Calculated on-demand via compoundInterest()
```

## Mermaid Diagram Features

All diagrams use enhanced Mermaid features for better clarity:

- **classDef styling**: Color-coded nodes by type
- **Subgraphs**: Grouped logical phases (State Updates, Token Operations, etc.)
- **Comments**: Critical debugging notes (%%)
- **linkStyle**: Highlighted critical paths
- **Rich labels**: Multi-line formatted text

---

## Mermaid Diagram Linting

When working with Mermaid diagrams in this directory, you MUST use **Maid** (`@probelabs/maid`) to validate and fix syntax errors.

### Quick Commands

```bash
# Lint a specific file
npx maid flows/supply.md

# Auto-fix issues
npx maid --fix flows/supply.md

# Lint all diagrams in the directory
npx maid .

# Fix all diagrams recursively
npx maid --fix .
```

### Common Issues & Fixes

**Parentheses in labels**: Mermaid doesn't support unquoted parentheses in node labels.

❌ Invalid:
```
A[Pool.supply(asset, amount)] --> B[SupplyLogic.executeSupply]
```

✅ Valid:
```
A["Pool.supply(asset, amount)"] --> B[SupplyLogic.executeSupply]
```

**Square brackets in labels**: Use HTML entities or quotes.

❌ Invalid:
```
A[users[address] = value]
```

✅ Valid:
```
A["users[address] = value"]
```

### Installation

If Maid is not already installed:

```bash
npm install -g @probelabs/maid
```

Or use npx without installing:

```bash
npx -y @probelabs/maid <file>
```

### CI Integration

Mermaid diagrams should be validated locally before committing. A CI mermaid-lint job is not currently configured.

### Resources

- Maid documentation: https://probelabs.com/maid
- Mermaid syntax reference: https://mermaid.js.org
