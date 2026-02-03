# GHO Variable Debt Token - Revision 3

## Overview
Complete source code for the GHO Variable Debt Token implementation at revision 3, active at block 18,780,889 (Dec 14, 2023).

## Contract Details
- **Implementation Address:** `0x20cb2f303ede313e2cc44549ad8653a5e8c0050e`
- **Proxy Address:** `0x786dbff3f1292ae8f92ea68cf93c30b34b1ed04b`
- **Revision:** `0x3` (version 3)
- **Contract Name:** GhoVariableDebtToken
- **Chain:** Ethereum Mainnet
- **Solidity Version:** 0.8.10

## All 25 Source Files (Flattened)

### GHO Core Contracts
| # | Contract | Size | Purpose |
|---|----------|------|---------|
| 1 | **GhoVariableDebtToken.sol** | 18KB | Main vToken implementation |
| 2 | **IGhoVariableDebtToken.sol** | 4KB | GHO vToken interface |
| 3 | **ScaledBalanceTokenBase.sol** | 4KB | Scaled balance base logic |
| 4 | **IGhoDiscountRateStrategy.sol** | 1KB | Discount strategy interface |

### Aave V3 Interfaces
| # | Contract | Size | Purpose |
|---|----------|------|---------|
| 5 | **IVariableDebtToken.sol** | 2KB | Variable debt token interface |
| 6 | **ICreditDelegationToken.sol** | 2KB | Credit delegation interface |
| 7 | **IInitializableDebtToken.sol** | 2KB | Initialization interface |
| 8 | **IScaledBalanceToken.sol** | 3KB | Scaled balance interface |
| 9 | **IPool.sol** | 33KB | Aave Pool interface |
| 10 | **IPoolAddressesProvider.sol** | 8KB | Addresses provider |
| 11 | **IAaveIncentivesController.sol** | 1KB | Incentives controller |
| 12 | **IACLManager.sol** | 5KB | Access control manager |

### Aave V3 Tokenization Base
| # | Contract | Size | Purpose |
|---|----------|------|---------|
| 13 | **DebtTokenBase.sol** | 4KB | Base debt token |
| 14 | **IncentivizedERC20.sol** | 8KB | ERC20 with incentives |
| 15 | **MintableIncentivizedERC20.sol** | 2KB | Mintable variant |
| 16 | **EIP712Base.sol** | 2KB | EIP-712 signatures |

### Aave V3 Libraries
| # | Contract | Size | Purpose |
|---|----------|------|---------|
| 17 | **VersionedInitializable.sol** | 3KB | Upgradeability pattern |
| 18 | **DataTypes.sol** | 7KB | Data structures |
| 19 | **WadRayMath.sol** | 4KB | 27-decimal math |
| 20 | **PercentageMath.sol** | 2KB | Percentage calculations |
| 21 | **Errors.sol** | 10KB | Error definitions |

### OpenZeppelin Dependencies
| # | Contract | Size | Purpose |
|---|----------|------|---------|
| 22 | **IERC20.sol** | 3KB | ERC20 interface |
| 23 | **IERC20Detailed.sol** | 1KB | Detailed ERC20 |
| 24 | **Context.sol** | 1KB | Context utilities |
| 25 | **SafeCast.sol** | 7KB | Type casting |

## Key Features

### 1. Discount System
- Users holding **stkAAVE** (discount token) receive interest discounts
- Discount calculated by `GhoDiscountRateStrategy` at `0x4c38ec4d1d2068540dfc11dfa4de41f733ddf812`
- Up to **30% discount** based on stkAAVE holdings

### 2. Non-Transferable
- Debt tokens **cannot be transferred or traded**
- All transfer functions revert with `OPERATION_NOT_SUPPORTED`

### 3. Credit Delegation
- Supports **EIP-712** delegation with signatures
- Users can delegate borrowing power

## Contract Relationships

```
GhoVariableDebtToken (Proxy: 0x786d...b04b)
├── Implementation: 0x20cb...050e
├── GHO Token: 0x40d1...6c2f
├── GHO AToken: 0x0090...4977
├── Aave Pool: 0x8787...4e2e
├── Discount Strategy: 0x4c38...f812
└── Discount Token (stkAAVE): 0x4da2...70f5
```

## Transaction Context
- **Transaction:** `0x7bfb64a4bfc4c2a2e38daefd5e7f682f243edc35fea54a1888ee8ae3621e7f65`
- **Block:** 18,780,889
- **Date:** Dec 14, 2023
- **Type:** GHO Debt Repayment (523.22 GHO)

## Fetched At
2026-02-02 via `cast source` from Etherscan
