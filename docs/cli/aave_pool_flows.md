# Aave V3 Pool Contract - Mermaid Flow Diagrams

This document contains Mermaid diagrams visualizing the execution flows of the Aave V3 Pool contract.

## 1. Supply Flow

```mermaid
flowchart TD
    A[supply asset, amount, onBehalfOf, referralCode] --> B{onlyBridge?}
    B -->|Yes| C[BridgeLogic.executeMintUnbacked]
    B -->|No| D[SupplyLogic.executeSupply]
    
    D --> E[ReserveLogic.updateState]
    E --> F[ValidationLogic.validateSupply]
    F --> G[ReserveLogic.updateInterestRates]
    G --> H[IERC20.safeTransferFrom]
    H --> I[IAToken.mint]
    I --> J{isFirstSupply?}
    
    J -->|Yes| K[ValidationLogic.validateUseAsCollateral]
    K --> L{canUseAsCollateral?}
    L -->|Yes| M[UserConfiguration.setUsingAsCollateral true]
    M --> N[EMIT ReserveUsedAsCollateralEnabled]
    L -->|No| O[Continue]
    
    J -->|No| O
    O --> P[EMIT Supply]
    
    C --> Q[EMIT MintUnbacked]
    Q --> R[EMIT ReserveUsedAsCollateralEnabled]
```

## 2. Withdraw Flow

```mermaid
flowchart TD
    A[withdraw asset, amount, to] --> B[SupplyLogic.executeWithdraw]
    B --> C[ReserveLogic.updateState]
    C --> D[Calculate userBalance from scaledBalance]
    D --> E{amount == MAX?}
    E -->|Yes| F[amount = userBalance]
    E -->|No| G[Use provided amount]
    F --> H[ValidationLogic.validateWithdraw]
    G --> H
    H --> I[ReserveLogic.updateInterestRates]
    I --> J[IAToken.burn]
    J --> K{isCollateral && amount == userBalance?}
    K -->|Yes| L[UserConfiguration.setUsingAsCollateral false]
    L --> M[EMIT ReserveUsedAsCollateralDisabled]
    K -->|No| N[Continue]
    M --> O{isCollateral && isBorrowing?}
    N --> O
    O -->|Yes| P[ValidationLogic.validateHFAndLtv]
    P --> Q[EMIT Withdraw]
    O -->|No| Q
```

## 3. Borrow Flow

```mermaid
flowchart TD
    A[borrow asset, amount, interestRateMode, referralCode, onBehalfOf] --> B[BorrowLogic.executeBorrow]
    B --> C[ReserveLogic.updateState]
    C --> D[ValidationLogic.validateBorrow]
    D --> E[GenericLogic.calculateUserAccountData]
    E --> F{IsolationModeActive?}
    F -->|Yes| G[Update isolationModeTotalDebt]
    G --> H[EMIT IsolationModeTotalDebtUpdated]
    F -->|No| I[Continue]
    H --> I
    I --> J{InterestRateMode}
    J -->|STABLE| K[IStableDebtToken.mint]
    J -->|VARIABLE| L[IVariableDebtToken.mint]
    K --> M{isFirstBorrowing?}
    L --> M
    M -->|Yes| N[UserConfiguration.setBorrowing true]
    M -->|No| O[Continue]
    N --> O
    O --> P[ReserveLogic.updateInterestRates]
    P --> Q[IAToken.transferUnderlyingTo]
    Q --> R[EMIT Borrow]
```

## 4. Repay Flow

```mermaid
flowchart TD
    A[repay asset, amount, interestRateMode, onBehalfOf] --> B[BorrowLogic.executeRepay]
    B --> C[ReserveLogic.updateState]
    C --> D[Helpers.getUserCurrentDebt]
    D --> E[ValidationLogic.validateRepay]
    E --> F{useATokens?}
    F -->|Yes| G[Calculate payback from aToken balance]
    F -->|No| H[Use provided amount]
    G --> I{InterestRateMode}
    H --> I
    I -->|STABLE| J[IStableDebtToken.burn]
    I -->|VARIABLE| K[IVariableDebtToken.burn]
    J --> L[ReserveLogic.updateInterestRates]
    K --> L
    L --> M[IsolationModeLogic.updateIsolatedDebtIfIsolated]
    M --> N{useATokens?}
    N -->|Yes| O[IAToken.burn]
    N -->|No| P[IERC20.safeTransferFrom]
    O --> Q[EMIT Repay]
    P --> Q
```

## 5. Liquidation Flow

```mermaid
flowchart TD
    A[liquidationCall collateralAsset, debtAsset, user, debtToCover, receiveAToken] --> B[LiquidationLogic.executeLiquidationCall]
    B --> C[ReserveLogic.updateState debtReserve]
    C --> D[GenericLogic.calculateUserAccountData]
    D --> E[ValidationLogic.validateLiquidationCall]
    E --> F[_calculateDebt]
    F --> G[_getConfigurationData]
    G --> H[_calculateAvailableCollateralToLiquidate]
    H --> I[_burnDebtTokens]
    I --> J[IVariableDebtToken.burn]
    I --> K[IStableDebtToken.burn]
    J --> L[IsolationModeLogic.updateIsolatedDebtIfIsolated]
    K --> L
    L --> M[ReserveLogic.updateInterestRates]
    M --> N{receiveAToken?}
    N -->|Yes| O[_liquidateATokens]
    O --> P[Transfer aTokens to liquidator]
    P --> Q[ValidationLogic.validateUseAsCollateral]
    Q --> R[UserConfiguration.setUsingAsCollateral true]
    R --> S[EMIT ReserveUsedAsCollateralEnabled liquidator]
    N -->|No| T[_burnCollateralATokens]
    T --> U[IAToken.burn]
    U --> V[Transfer underlying to liquidator]
    S --> W[IERC20.safeTransferFrom debt repayment]
    V --> W
    W --> X[EMIT LiquidationCall]
```

## 6. Flash Loan Flow

```mermaid
flowchart TD
    A["flashLoan receiverAddress, assets[], amounts[], interestRateModes[], onBehalfOf, params, referralCode"] --> B[FlashLoanLogic.executeFlashLoan]
    B --> C[ValidationLogic.validateFlashloan]
    C --> D[Loop over assets]
    D --> E[Calculate totalPremium]
    E --> F[IAToken.transferUnderlyingTo]
    F --> G[IFlashLoanReceiver.executeOperation]
    G --> H{interestRateMode == NONE?}
    H -->|Yes| I[_handleFlashLoanRepayment]
    I --> J[IERC20.safeTransferFrom]
    J --> K[EMIT FlashLoan with premium]
    H -->|No| L[BorrowLogic.executeBorrow]
    L --> M[EMIT FlashLoan premium=0]
    
    subgraph FlashLoanSimple
    N[flashLoanSimple receiverAddress, asset, amount, params, referralCode] --> O[FlashLoanLogic.executeFlashLoanSimple]
    O --> P[ValidationLogic.validateFlashloanSimple]
    P --> Q[IAToken.transferUnderlyingTo]
    Q --> R[IFlashLoanSimpleReceiver.executeOperation]
    R --> S[_handleFlashLoanRepayment]
    S --> T[EMIT FlashLoan]
    end
```

## 7. Rate Management Flows

```mermaid
flowchart TD
    subgraph SwapBorrowRateMode
    A[swapBorrowRateMode asset, interestRateMode] --> B[BorrowLogic.executeSwapBorrowRateMode]
    B --> C[ReserveLogic.updateState]
    C --> D[ValidationLogic.validateSwapRateMode]
    D --> E{currentRateMode}
    E -->|STABLE| F[IStableDebtToken.burn + IVariableDebtToken.mint]
    E -->|VARIABLE| G[IVariableDebtToken.burn + IStableDebtToken.mint]
    F --> H[ReserveLogic.updateInterestRates]
    G --> H
    H --> I[EMIT SwapBorrowRateMode]
    end
    
    subgraph RebalanceStableBorrowRate
    J[rebalanceStableBorrowRate asset, user] --> K[BorrowLogic.executeRebalanceStableBorrowRate]
    K --> L[ReserveLogic.updateState]
    L --> M[ValidationLogic.validateRebalanceStableBorrowRate]
    M --> N[IStableDebtToken.burn]
    N --> O[IStableDebtToken.mint with new rate]
    O --> P[ReserveLogic.updateInterestRates]
    P --> Q[EMIT RebalanceStableBorrowRate]
    end
```

## 8. Collateral Management Flow

```mermaid
flowchart TD
    A[setUserUseReserveAsCollateral asset, useAsCollateral] --> B[SupplyLogic.executeUseReserveAsCollateral]
    B --> C[ValidationLogic.validateSetUseReserveAsCollateral]
    C --> D{useAsCollateral}
    D -->|true| E[ValidationLogic.validateUseAsCollateral]
    E --> F{canUseAsCollateral?}
    F -->|Yes| G[UserConfiguration.setUsingAsCollateral true]
    G --> H[EMIT ReserveUsedAsCollateralEnabled]
    F -->|No| I[Revert USER_IN_ISOLATION_MODE]
    D -->|false| J[UserConfiguration.setUsingAsCollateral false]
    J --> K[ValidationLogic.validateHFAndLtv]
    K --> L[EMIT ReserveUsedAsCollateralDisabled]
```

## 9. E-Mode Management Flow

```mermaid
flowchart TD
    A[setUserEMode categoryId] --> B[EModeLogic.executeSetUserEMode]
    B --> C[ValidationLogic.validateSetUserEMode]
    C --> D{prevCategoryId != 0?}
    D -->|Yes| E[ValidationLogic.validateHealthFactor]
    E --> F[GenericLogic.calculateUserAccountData]
    D -->|No| G[Continue]
    F --> H[Update usersEModeCategory]
    G --> H
    H --> I[EMIT UserEModeSet]
```

## 10. Bridge Operations Flow

```mermaid
flowchart TD
    subgraph MintUnbacked
    A[mintUnbacked asset, amount, onBehalfOf, referralCode] --> B[BridgeLogic.executeMintUnbacked]
    B --> C[ReserveLogic.updateState]
    C --> D[ValidationLogic.validateSupply]
    D --> E[Check unbackedMintCap]
    E --> F[ReserveLogic.updateInterestRates]
    F --> G[IAToken.mint]
    G --> H{isFirstSupply?}
    H -->|Yes| I[ValidationLogic.validateUseAsCollateral]
    I --> J[UserConfiguration.setUsingAsCollateral true]
    J --> K[EMIT ReserveUsedAsCollateralEnabled]
    H -->|No| L[Continue]
    K --> M[EMIT MintUnbacked]
    L --> M
    end
    
    subgraph BackUnbacked
    N[backUnbacked asset, amount, fee] --> O[BridgeLogic.executeBackUnbacked]
    O --> P[ReserveLogic.updateState]
    P --> Q[Calculate backingAmount]
    Q --> R[Calculate fee splits]
    R --> S[ReserveLogic.cumulateToLiquidityIndex]
    S --> T[Update accruedToTreasury]
    T --> U[ReserveLogic.updateInterestRates]
    U --> V[IERC20.safeTransferFrom]
    V --> W[EMIT BackUnbacked]
    end
```

## 11. Admin Operations Flow

```mermaid
flowchart TD
    subgraph InitReserve
    A[initReserve asset, aTokenAddress, stableDebtAddress, variableDebtAddress, interestRateStrategyAddress] --> B[PoolLogic.executeInitReserve]
    B --> C[Address.isContract asset]
    C --> D[ReserveLogic.init]
    D --> E[Update reservesList]
    E --> F[Increment reservesCount]
    end
    
    subgraph DropReserve
    G[dropReserve asset] --> H[PoolLogic.executeDropReserve]
    H --> I[ValidationLogic.validateDropReserve]
    I --> J{all supplies and debts are zero?}
    J -->|Yes| K[Clear reservesList entry]
    K --> L[Delete reservesData entry]
    J -->|No| M[Revert]
    end
    
    subgraph MintToTreasury
    N["mintToTreasury assets[]"] --> O[PoolLogic.executeMintToTreasury]
    O --> P[Loop over assets]
    P --> Q[Calculate amountToMint]
    Q --> R[IAToken.mintToTreasury]
    R --> S[EMIT MintedToTreasury]
    end
    
    subgraph ConfigureEMode
    T[configureEModeCategory id, category] --> U{onlyPoolConfigurator?}
    U -->|Yes| V[id != 0?]
    V -->|Yes| W[Update eModeCategories id]
    V -->|No| X[Revert EMODE_CATEGORY_RESERVED]
    U -->|No| Y[Revert CALLER_NOT_POOL_CONFIGURATOR]
    end
```

## 12. Interest Rate Update Flow

```mermaid
flowchart TD
    A[updateInterestRates reserve, reserveCache, reserveAddress, liquidityAdded, liquidityTaken] --> B[IReserveInterestRateStrategy.calculateInterestRates]
    B --> C[Get nextLiquidityRate, nextStableRate, nextVariableRate]
    C --> D[Update reserve storage]
    D --> E[EMIT ReserveDataUpdated]
    
    subgraph AccrueToTreasury
    F[_accrueToTreasury reserve, reserveCache] --> G{reserveFactor > 0?}
    G -->|Yes| H[Calculate debt accrued]
    H --> I[Calculate amountToMint]
    I --> J[Update accruedToTreasury]
    G -->|No| K[Skip]
    end
    
    subgraph UpdateIndexes
    L[_updateIndexes reserve, reserveCache] --> M{currLiquidityRate != 0?}
    M -->|Yes| N[Calculate cumulatedLiquidityInterest]
    N --> O[Update liquidityIndex]
    M -->|No| P[Skip]
    O --> Q{currScaledVariableDebt != 0?}
    P --> Q
    Q -->|Yes| R[Calculate cumulatedVariableBorrowInterest]
    R --> S[Update variableBorrowIndex]
    Q -->|No| T[Skip]
    end
```

## 13. Finalize Transfer Flow (Internal)

```mermaid
flowchart TD
    A[finalizeTransfer asset, from, to, amount, balanceFromBefore, balanceToBefore] --> B{Caller is aToken?}
    B -->|Yes| C[SupplyLogic.executeFinalizeTransfer]
    B -->|No| D[Revert CALLER_NOT_ATOKEN]
    C --> E[ValidationLogic.validateTransfer]
    E --> F{from != to && amount > 0?}
    F -->|Yes| G[Check from.isUsingAsCollateral]
    G --> H{from.isBorrowingAny?}
    H -->|Yes| I[ValidationLogic.validateHFAndLtv from]
    H -->|No| J[Continue]
    I --> K{balanceFromBefore == amount?}
    J --> K
    K -->|Yes| L[UserConfiguration.setUsingAsCollateral false from]
    L --> M[EMIT ReserveUsedAsCollateralDisabled from]
    K -->|No| N[Continue]
    N --> O{to.balanceToBefore == 0?}
    O -->|Yes| P[ValidationLogic.validateUseAsCollateral to]
    P --> Q[UserConfiguration.setUsingAsCollateral true to]
    Q --> R[EMIT ReserveUsedAsCollateralEnabled to]
    O -->|No| S[Continue]
    F -->|No| T[Skip checks]
```

## 14. Events Summary Diagram

```mermaid
flowchart LR
    subgraph CoreEvents
    A[Supply]
    B[Withdraw]
    C[Borrow]
    D[Repay]
    E[LiquidationCall]
    F[FlashLoan]
    end
    
    subgraph CollateralEvents
    G[ReserveUsedAsCollateralEnabled]
    H[ReserveUsedAsCollateralDisabled]
    end
    
    subgraph RateEvents
    I[ReserveDataUpdated]
    J[SwapBorrowRateMode]
    K[RebalanceStableBorrowRate]
    end
    
    subgraph ModeEvents
    L[UserEModeSet]
    M[IsolationModeTotalDebtUpdated]
    end
    
    subgraph TreasuryEvents
    N[MintedToTreasury]
    O[MintUnbacked]
    P[BackUnbacked]
    end
    
    A --> G
    B --> H
    C --> M
    D --> M
    E --> H
    E --> G
```

## 15. Library Dependencies

```mermaid
flowchart TB
    subgraph PoolContract
    A[Pool.sol]
    end
    
    subgraph CoreLogic
    B[SupplyLogic]
    C[BorrowLogic]
    D[LiquidationLogic]
    E[FlashLoanLogic]
    F[BridgeLogic]
    G[PoolLogic]
    H[EModeLogic]
    end
    
    subgraph StateManagement
    I[ReserveLogic]
    J[ValidationLogic]
    K[GenericLogic]
    L[IsolationModeLogic]
    end
    
    subgraph Configuration
    M[ReserveConfiguration]
    N[UserConfiguration]
    end
    
    subgraph MathLibraries
    O[WadRayMath]
    P[PercentageMath]
    Q[MathUtils]
    end
    
    subgraph ExternalLibs
    R[GPv2SafeERC20]
    S[SafeCast]
    T[Address]
    end
    
    A --> B
    A --> C
    A --> D
    A --> E
    A --> F
    A --> G
    A --> H
    
    B --> I
    B --> J
    B --> N
    
    C --> I
    C --> J
    C --> K
    C --> L
    C --> N
    
    D --> I
    D --> J
    D --> K
    D --> L
    D --> N
    
    E --> I
    E --> J
    E --> C
    
    F --> I
    F --> J
    F --> N
    
    G --> I
    G --> J
    
    H --> J
    H --> K
    
    I --> O
    I --> P
    I --> Q
    I --> S
    
    J --> O
    J --> P
    J --> K
    
    K --> O
    K --> P
    K --> H
    
    L --> J
    L --> N
    L --> M
    
    B --> R
    C --> R
    D --> R
    E --> R
    F --> R
    G --> R
    
    I --> R
    J --> R
```

## Usage

To view these diagrams:
1. Install a Mermaid viewer extension in your IDE, or
2. Use the Mermaid Live Editor at https://mermaid.live
3. Copy any diagram code block and paste it into the editor

## Key Patterns

1. **State Update Pattern**: Every write operation starts with `ReserveLogic.updateState()` which:
   - Updates indexes
   - Accrues interest to treasury
   - Updates timestamps

2. **Validation Pattern**: Operations validate inputs via `ValidationLogic` before state changes

3. **Rate Update Pattern**: Operations affecting liquidity end with `ReserveLogic.updateInterestRates()`

4. **Event Emission Pattern**: Events are emitted after successful state changes for off-chain tracking

5. **Library Delegation Pattern**: Pool acts as router, delegating to specialized libraries
