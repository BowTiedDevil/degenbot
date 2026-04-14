# Amount Transformations Reference

This document provides detailed amount transformation formulas with Solidity code snippets for navigating the actual contract source.

## Table of Contents

- [Core Math Operations](#core-math-operations)
- [Collateral Token Transformations](#collateral-token-transformations)
- [Debt Token Transformations](#debt-token-transformations)
- [Interest Accrual](#interest-accrual)
- [Treasury Accrual](#treasury-accrual)
- [Liquidation Calculations](#liquidation-calculations)
- [Flash Loan Premiums](#flash-loan-premiums)
- [E-Mode Calculations](#e-mode-calculations)
- [Version Differences](#version-differences)

---

## Core Math Operations

### WadRayMath

**File:** `contracts/protocol/libraries/math/WadRayMath.sol`

```solidity
uint256 internal constant WAD = 1e18;    // 18 decimals for tokens
uint256 internal constant RAY = 1e27;    // 27 decimals for indices
uint256 internal constant HALF_RAY = RAY / 2;

// Standard half-up rounding
function rayMul(uint256 a, uint256 b) internal pure returns (uint256) {
    return (a * b + HALF_RAY) / RAY;
}

function rayDiv(uint256 a, uint256 b) internal pure returns (uint256) {
    uint256 halfB = b / 2;
    return (a * RAY + halfB) / b;
}

// v4+ variants
function rayMulFloor(uint256 a, uint256 b) internal pure returns (uint256) {
    return (a * b) / RAY;
}

function rayMulCeil(uint256 a, uint256 b) internal pure returns (uint256) {
    uint256 result = (a * b) / RAY;
    if ((a * b) % RAY != 0) result++;
    return result;
}

function rayDivFloor(uint256 a, uint256 b) internal pure returns (uint256) {
    return (a * RAY) / b;
}

function rayDivCeil(uint256 a, uint256 b) internal pure returns (uint256) {
    uint256 result = (a * RAY) / b;
    if ((a * RAY) % b != 0) result++;
    return result;
}
```

### PercentageMath

**File:** `contracts/protocol/libraries/math/PercentageMath.sol`

```solidity
uint256 internal constant PERCENTAGE_FACTOR = 1e4;  // 100.00%

function percentMul(uint256 value, uint256 percentage) internal pure returns (uint256) {
    uint256 halfPercentage = percentage / 2;
    return (value * percentage + halfPercentage) / PERCENTAGE_FACTOR;
}
```

---

## Collateral Token Transformations

### Supply: amount → scaledAmount

**Files:**
- `contracts/protocol/libraries/logic/SupplyLogic.sol:executeSupply()`
- `contracts/protocol/tokenization/AToken.sol:_mintScaled()`

**Transformation:**
```
User Input (WAD decimals)
    ↓
amount.rayDiv(index) → scaledAmount (RAY decimals)
    ↓
Store: _scaledBalance[user] += scaledAmount
```

**Solidity:**
```solidity
// SupplyLogic.sol
uint256 scaledAmount = amount.rayDiv(index);  // v1-3: half-up, v4+: floor
IAToken(reserveCache.aTokenAddress).mint(
    msg.sender,
    onBehalfOf,
    amount,
    index
);

// AToken.sol
function _mintScaled(address user, uint256 scaledAmount, uint256 index) internal {
    _scaledBalance[user] += scaledAmount;
}
```

### Withdraw: scaledAmount → amount

**Files:**
- `contracts/protocol/libraries/logic/SupplyLogic.sol:executeWithdraw()`
- `contracts/protocol/tokenization/AToken.sol:_burnScaled()`

**Transformation:**
```
_scaledBalance[from] (stored scaled)
    ↓
_scaledBalance[from].rayMul(index) = userBalance (WAD)
    ↓
Validation: amount <= userBalance
    ↓
amount.rayDiv(index) → scaledAmountToBurn
    ↓
_scaledBalance[from] -= scaledAmountToBurn
```

**Solidity:**
```solidity
// SupplyLogic.sol
uint256 userBalance = IAToken(reserveCache.aTokenAddress).balanceOf(msg.sender);
if (amount == type(uint256).max) {
    amount = userBalance;
}
uint256 scaledAmount = amount.rayDiv(index);  // v1-3: half-up, v4+: ceil

// AToken.sol
function _burnScaled(address from, uint256 amount, uint256 index) internal {
    uint256 scaledAmount = amount.rayDiv(index);
    _scaledBalance[from] -= scaledAmount;
}
```

### Transfer: scaledAmount Preservation

**File:** `contracts/protocol/tokenization/AToken.sol:_transfer()`

```solidity
uint256 scaledAmount = amount.rayDiv(index);  // v4+: uses rayDivFloor

_scaledBalance[from] -= scaledAmount;
_scaledBalance[to] += scaledAmount;

Pool.finalizeTransfer(
    underlyingAsset,
    from,
    to,
    amount,                   // unscaled
    fromScaledBalanceBefore,  // scaled
    toScaledBalanceBefore     // scaled
);
```

**Used in flows:** [Supply](../flows/supply.md), [Withdraw](../flows/withdraw.md), [Liquidation](../flows/liquidation.md)

---

## Debt Token Transformations

### Variable Borrow: amount → scaledAmount

**Files:**
- `contracts/protocol/libraries/logic/BorrowLogic.sol:executeBorrow()`
- `contracts/protocol/tokenization/VariableDebtToken.sol:_mint()`

**Transformation:**
```
User requests borrow amount (WAD)
    ↓
amount.rayDiv(nextVariableBorrowIndex) → scaledAmount (RAY)
    ↓
_scaledBalance[user] += scaledAmount
```

**Solidity:**
```solidity
// BorrowLogic.sol
IVariableDebtToken(reserveCache.variableDebtTokenAddress).mint(
    msg.sender,
    onBehalfOf,
    amount,
    reserveCache.nextVariableBorrowIndex
);

// VariableDebtToken.sol
function _mint(address user, uint256 amount, uint256 index) internal {
    uint256 scaledAmount = amount.rayDiv(index);  // v1-3: half-up, v4+: ceil
    _scaledBalance[user] += scaledAmount;
}
```

### Variable Repay: scaledAmount → amount

**Files:**
- `contracts/protocol/libraries/logic/BorrowLogic.sol:executeRepay()`
- `contracts/protocol/tokenization/VariableDebtToken.sol:_burn()`

**Transformation:**
```
_scaledBalance[user].rayMul(index) = currentDebt (WAD)
    ↓
Calculate payback amount (min(requested, currentDebt))
    ↓
amount.rayDiv(index) → scaledAmountToBurn
    ↓
_scaledBalance[user] -= scaledAmountToBurn
```

**Solidity:**
```solidity
// BorrowLogic.sol
(
    uint256 stableDebt,
    uint256 variableDebt
) = Helpers.getUserCurrentDebt(onBehalfOf, reserveCache);

IVariableDebtToken(reserveCache.variableDebtTokenAddress).burn(
    onBehalfOf,
    paybackAmount,
    index
);

// VariableDebtToken.sol
function _burn(address user, uint256 amount, uint256 index) internal {
    uint256 scaledAmount = amount.rayDiv(index);  // v4+: uses rayDivFloor
    uint256 scaledBalanceBefore = _scaledBalance[user];
    
    if (scaledAmount > scaledBalanceBefore) {
        scaledAmount = scaledBalanceBefore;
        amount = scaledAmount.rayMul(index);
    }
    
    _scaledBalance[user] -= scaledAmount;
}
```

### Stable Borrow (No Scaling)

**File:** `contracts/protocol/tokenization/StableDebtToken.sol:mint()`

```solidity
// Stable debt is NOT scaled - stored as principal + timestamp
function _mint(address user, uint256 amount, uint256 rate) internal {
    uint256 previousBalance = _balances[user].principal;
    uint256 balanceIncrease = 0;
    
    if (previousBalance != 0) {
        balanceIncrease = previousBalance.rayMul(
            MathUtils.calculateCompoundedInterest(
                _balances[user].stableRate,
                _balances[user].lastUpdateTimestamp
            )
        ) - previousBalance;
    }
    
    _balances[user].principal = previousBalance + amount + balanceIncrease;
    _balances[user].stableRate = getAverageStableRate(
        previousBalance + balanceIncrease,
        _balances[user].stableRate,
        amount,
        rate
    );
    _balances[user].lastUpdateTimestamp = block.timestamp;
}
```

**Used in flows:** [Borrow](../flows/borrow.md), [Repay](../flows/repay.md), [Liquidation](../flows/liquidation.md)

---

## Interest Accrual

### Liquidity Index Update

**Files:**
- `contracts/protocol/libraries/logic/ReserveLogic.sol:_updateIndexes()`
- `contracts/protocol/libraries/math/MathUtils.sol:calculateLinearInterest()`

**Transformation:**
```
OLD: liquidityIndex[t-1]
    ↓
Calculate timeDelta = now - lastUpdateTimestamp
    ↓
cumulatedInterest = 1 + (rate * timeDelta / SECONDS_PER_YEAR)
    ↓
NEW: liquidityIndex[t] = liquidityIndex[t-1].rayMul(cumulatedInterest)
```

**Solidity:**
```solidity
// ReserveLogic.sol
if (reserveCache.currLiquidityRate != 0) {
    uint256 cumulatedLiquidityInterest = MathUtils.calculateLinearInterest(
        reserveCache.currLiquidityRate,
        reserveCache.reserveLastUpdateTimestamp
    );
    
    reserve.liquidityIndex = uint128(
        reserveCache.currLiquidityIndex.rayMul(cumulatedLiquidityInterest)
    );
}

// MathUtils.sol
function calculateLinearInterest(uint256 rate, uint40 lastUpdateTimestamp)
    internal view returns (uint256)
{
    uint256 timeDifference = block.timestamp - uint256(lastUpdateTimestamp);
    return (rate * timeDifference) / SECONDS_PER_YEAR + WadRayMath.RAY;
}
```

### Borrow Index Update

**Files:**
- `contracts/protocol/libraries/logic/ReserveLogic.sol:_updateIndexes()`
- `contracts/protocol/libraries/math/MathUtils.sol:calculateCompoundedInterest()`

**Transformation:** Same pattern as liquidity index, but uses compounded interest

**Solidity:**
```solidity
if (reserveCache.currScaledVariableDebt != 0) {
    uint256 cumulatedVariableBorrowInterest = MathUtils.calculateCompoundedInterest(
        reserveCache.currVariableBorrowRate,
        reserveCache.reserveLastUpdateTimestamp
    );
    
    reserve.variableBorrowIndex = uint128(
        reserveCache.currVariableBorrowIndex.rayMul(cumulatedVariableBorrowInterest)
    );
}

// MathUtils.sol - Uses Taylor series approximation
function calculateCompoundedInterest(uint256 rate, uint40 lastUpdateTimestamp)
    internal view returns (uint256)
{
    uint256 exp = block.timestamp - uint256(lastUpdateTimestamp);
    if (exp == 0) return WadRayMath.RAY;
    
    uint256 expMinusOne = exp - 1;
    uint256 expMinusTwo = exp > 2 ? exp - 2 : 0;
    
    uint256 ratePerSecond = rate / SECONDS_PER_YEAR;
    
    uint256 basePowerTwo = ratePerSecond.rayMul(ratePerSecond);
    uint256 basePowerThree = basePowerTwo.rayMul(ratePerSecond);
    
    uint256 secondTerm = (exp * expMinusOne * basePowerTwo) / 2;
    uint256 thirdTerm = (exp * expMinusOne * expMinusTwo * basePowerThree) / 6;
    
    return WadRayMath.RAY + (ratePerSecond * exp) + secondTerm + thirdTerm;
}
```

**Used in flows:** All flows (via ReserveLogic.updateState)

---

## Treasury Accrual

**Files:**
- `contracts/protocol/libraries/logic/ReserveLogic.sol:_accrueToTreasury()`
- `contracts/protocol/libraries/logic/PoolLogic.sol:executeMintToTreasury()`

**Transformation:**
```
scaledDebtIncrease = newScaledDebt - oldScaledDebt
    ↓
debtAccruedScaled = scaledDebtIncrease.rayMul(reserveFactor)
    ↓
reserve.accruedToTreasury += debtAccruedScaled
    ↓
... (later, during mintToTreasury) ...
    ↓
amountToMint = accruedToTreasury.rayMul(liquidityIndex)
    ↓
scaledAmount = amountToMint.rayDiv(liquidityIndex)
```

**Solidity:**
```solidity
// ReserveLogic.sol:_accrueToTreasury()
if (reserveCache.reserveFactor > 0) {
    uint256 scaledTotalDebt = IVariableDebtToken(
        reserveCache.variableDebtTokenAddress
    ).scaledTotalSupply();
    
    uint256 nextScaledVariableDebt = scaledTotalDebt;
    uint256 currScaledVariableDebt = reserveCache.currScaledVariableDebt;
    
    if (nextScaledVariableDebt > currScaledVariableDebt) {
        uint256 debtAccrued = (nextScaledVariableDebt - currScaledVariableDebt)
            .rayMul(reserveCache.reserveFactor);
        
        reserve.accruedToTreasury += uint128(debtAccrued);
    }
}

// PoolLogic.sol:executeMintToTreasury()
for (uint256 i = 0; i < assets.length; i++) {
    uint256 accruedToTreasury = reserve.accruedToTreasury;
    
    if (accruedToTreasury != 0) {
        uint256 amountToMint = accruedToTreasury.rayMul(
            reserveCache.nextLiquidityIndex
        );
        
        IAToken(reserveCache.aTokenAddress).mintToTreasury(
            amountToMint,
            reserveCache.nextLiquidityIndex
        );
        
        reserve.accruedToTreasury = 0;
    }
}

// AToken.sol:mintToTreasury()
uint256 scaledAmount = amount.rayDiv(index);
_mintScaled(address(this), scaledAmount, index);
```

**Used in flows:** All flows (via updateState)

---

## Liquidation Calculations

**Files:**
- `contracts/protocol/libraries/logic/LiquidationLogic.sol:executeLiquidationCall()`
- `contracts/protocol/libraries/logic/LiquidationLogic.sol:_calculateAvailableCollateralToLiquidate()`

**Transformation:**
```
debtToCover (in debt asset decimals)
    ↓
Convert to collateral value:
collateralAmount = debtToCover
    .percentMul(100% + liquidationBonus)
    .wadToRay()
    .rayDiv(collateralPrice)
    ↓
Cap at available collateral:
if (collateralAmount > maxCollateral):
    recalculate debtToCover
    ↓
Calculate protocol fee:
fee = (collateralAmount - debtValue)
    .percentMul(liquidationProtocolFee)
```

**Solidity:**
```solidity
// executeLiquidationCall()
uint256 collateralPrice = IPriceOracleGetter(params.priceOracle).getAssetPrice(
    params.collateralAsset
);
uint256 debtAssetPrice = IPriceOracleGetter(params.priceOracle).getAssetPrice(
    params.debtAsset
);

vars.closeFactor = userConfig.isUsingAsCollateral(vars.debtReserve.id)
    ? DEFAULT_LIQUIDATION_CLOSE_FACTOR
    : MAX_LIQUIDATION_CLOSE_FACTOR;

(
    vars.actualDebtToLiquidate,
    vars.actualCollateralToLiquidate,
    vars.liquidationProtocolFeeAmount
) = _calculateAvailableCollateralToLiquidate(
    collateralReserve,
    debtReserve,
    collateralAssetPrice,
    debtAssetPrice,
    vars.actualDebtToLiquidate,
    vars.userCollateralBalance,
    liquidationBonus
);

// _calculateAvailableCollateralToLiquidate()
function _calculateAvailableCollateralToLiquidate(
    DataTypes.ReserveData storage collateralReserve,
    DataTypes.ReserveData storage debtReserve,
    uint256 collateralAssetPrice,
    uint256 debtAssetPrice,
    uint256 debtToCover,
    uint256 userCollateralBalance,
    uint256 liquidationBonus
) internal view returns (uint256, uint256, uint256) {
    uint256 collateralAmount = debtToCover
        .percentMul(PercentageMath.PERCENTAGE_FACTOR + liquidationBonus)
        .wadToRay()
        .rayDiv(collateralAssetPrice);
    
    uint256 maxCollateralToLiquidate = userCollateralBalance.rayMul(
        collateralReserve.liquidityIndex
    );
    
    if (collateralAmount > maxCollateralToLiquidate) {
        collateralAmount = maxCollateralToLiquidate;
        debtToCover = collateralAmount
            .rayMul(collateralAssetPrice)
            .rayToWad()
            .percentDiv(PercentageMath.PERCENTAGE_FACTOR + liquidationBonus);
    }
    
    uint256 liquidationProtocolFee = collateralReserve.configuration
        .getLiquidationProtocolFee();
    
    uint256 liquidationProtocolFeeAmount = liquidationProtocolFee > 0
        ? (collateralAmount - debtToCover.rayMul(debtAssetPrice).rayToWad())
            .percentMul(liquidationProtocolFee)
        : 0;
    
    return (
        debtToCover,
        collateralAmount - liquidationProtocolFeeAmount,
        liquidationProtocolFeeAmount
    );
}
```

**Used in flows:** [Liquidation](../flows/liquidation.md)

---

## Flash Loan Premiums

**Files:**
- `contracts/protocol/libraries/logic/FlashLoanLogic.sol:executeFlashLoan()`
- `contracts/protocol/libraries/logic/FlashLoanLogic.sol:_handleFlashLoanRepayment()`

**Transformation:**
```
Flash loan amount: 1000 USDC
    ↓
Calculate premiums:
totalPremium = 1000 * 0.09% = 0.9 USDC
protocolPremium = 1000 * 0.03% = 0.3 USDC
lpPremium = 0.9 - 0.3 = 0.6 USDC
    ↓
Repayment required: 1000 + 0.9 = 1000.9 USDC
    ↓
Treasury accrual (scaled):
treasuryAccrued += 0.3.rayDiv(liquidityIndex)
    ↓
LP distribution (index increase):
newIndex = (oldIndex * totalLiquidity + lpPremium) / totalLiquidity
    ↓
All aToken holders benefit via increased index
```

**Solidity:**
```solidity
// executeFlashLoan()
uint256 totalPremium = amount.percentMul(vars.flashLoanPremiumTotal);
uint256 protocolPremium = amount.percentMul(vars.flashLoanPremiumToProtocol);

IAToken(reserveCache.aTokenAddress).transferUnderlyingTo(
    params.receiverAddress,
    amount
);

require(
    IFlashLoanReceiver(params.receiverAddress).executeOperation(
        params.assets,
        params.amounts,
        premiums,
        msg.sender,
        params.params
    ),
    Errors.INVALID_FLASH_LOAN_EXECUTOR_RETURN
);

_handleFlashLoanRepayment(
    reserve,
    reserveCache,
    params.assets[i],
    params.amounts[i],
    totalPremium,
    protocolPremium
);

// _handleFlashLoanRepayment()
function _handleFlashLoanRepayment(
    DataTypes.ReserveData storage reserve,
    DataTypes.ReserveCache memory reserveCache,
    address asset,
    uint256 amount,
    uint256 premium,
    uint256 protocolPremium
) internal {
    uint256 amountPlusPremium = amount + premium;
    uint256 premiumToProtocol = protocolPremium;
    uint256 premiumToLP = premium - premiumToProtocol;
    
    IERC20(asset).safeTransferFrom(
        msg.sender,
        reserveCache.aTokenAddress,
        amountPlusPremium
    );
    
    reserve.accruedToTreasury += uint128(
        premiumToProtocol.rayDiv(reserveCache.nextLiquidityIndex)
    );
    
    _cumulateToLiquidityIndex(
        reserve,
        reserveCache,
        amount + premium,
        premiumToLP
    );
}

// _cumulateToLiquidityIndex()
function _cumulateToLiquidityIndex(
    DataTypes.ReserveData storage reserve,
    DataTypes.ReserveCache memory reserveCache,
    uint256 totalLiquidity,
    uint256 amount
) internal {
    uint256 liquidityIndex = reserveCache.nextLiquidityIndex;
    uint256 newLiquidityIndex = (liquidityIndex.rayMul(
        totalLiquidity.wadToRay()
    ) + amount.wadToRay()).rayDiv(totalLiquidity.wadToRay());
    
    reserve.liquidityIndex = uint128(newLiquidityIndex);
}
```

**Used in flows:** [Flash Loan](../flows/flash_loan.md)

---

## E-Mode Calculations

**File:** `contracts/protocol/libraries/logic/GenericLogic.sol:calculateUserAccountData()`

**Transformation:**
```
For each collateral asset:
    scaledBalance = _scaledBalance[user]
    unscaledBalance = scaledBalance.rayMul(liquidityIndex)
    
    // E-Mode price override
    price = eMode.active && assetInEMode
        ? eMode.priceSource  // Correlated asset price
        : oraclePrice
    
    valueInETH = unscaledBalance
        .wadToRay()
        .rayMul(price)
        .rayToWad()
    
    totalCollateral += valueInETH

For each debt asset:
    scaledDebt = _scaledBalance[user]
    unscaledDebt = scaledDebt.rayMul(borrowIndex)
    valueInETH = unscaledDebt.wadToRay().rayMul(price).rayToWad()
    totalDebt += valueInETH

// Calculate health factor
healthFactor = totalCollateral.percentMul(liquidationThreshold).wadDiv(totalDebt)
```

**Solidity:**
```solidity
for (uint256 i = 0; i < reservesDataCount; i++) {
    if (!userConfig.isUsingAsCollateralOrBorrowing(i)) continue;
    
    DataTypes.ReserveData memory reserve = reservesData[reservesList[i]];
    DataTypes.ReserveCache memory reserveCache = _cache(reserve);
    
    uint256 reserveUnitPrice = IPriceOracleGetter(params.oracle).getAssetPrice(
        address(reserveCache.aTokenAddress)
    );
    
    if (userConfig.isUsingAsCollateral(i)) {
        uint256 assetUnitPrice = (params.eModeCategory.priceSource != address(0) &&
            params.eModeCategory.assets[reservesList[i]])
            ? IPriceOracleGetter(params.oracle).getAssetPrice(
                params.eModeCategory.priceSource
            )
            : reserveUnitPrice;
        
        uint256 liquidityBalance = IERC20(reserveCache.aTokenAddress)
            .scaledBalanceOf(params.user)
            .rayMul(reserveCache.nextLiquidityIndex);
        
        uint256 liquidityBalanceETH = liquidityBalance
            .wadToRay()
            .rayMul(assetUnitPrice)
            .rayToWad();
        
        totalCollateralInBaseCurrency += liquidityBalanceETH;
        
        if (params.eModeCategory.priceSource != address(0) &&
            params.eModeCategory.assets[reservesList[i]]) {
            avgLtv = params.eModeCategory.ltv;
            avgLiquidationThreshold = params.eModeCategory.liquidationThreshold;
        } else {
            avgLtv += liquidityBalanceETH * reserveCache.ltv;
            avgLiquidationThreshold += liquidityBalanceETH * reserveCache.liquidationThreshold;
        }
    }
    
    if (userConfig.isBorrowing(i)) {
        uint256 borrowBalance = IERC20(reserveCache.variableDebtTokenAddress)
            .scaledBalanceOf(params.user)
            .rayMul(reserveCache.nextVariableBorrowIndex);
        
        uint256 borrowBalanceETH = borrowBalance
            .wadToRay()
            .rayMul(reserveUnitPrice)
            .rayToWad();
        
        totalDebtInBaseCurrency += borrowBalanceETH;
    }
}

vars.healthFactor = totalDebtInBaseCurrency > 0
    ? totalCollateralInBaseCurrency
        .percentMul(avgLiquidationThreshold)
        .wadDiv(totalDebtInBaseCurrency)
    : type(uint256).max;
```

**Used in flows:** All flows (health factor validation), [E-Mode Management](../flows/emode_management.md)

---

## Version Differences

| Operation | Pool v1-3 | Pool v4+ | Reason |
|-----------|-----------|----------|---------|
| AToken Mint | `rayDiv` (half-up) | `rayDivFloor` | Prevent rounding up debt |
| AToken Burn | `rayDiv` (half-up) | `rayDivCeil` | Ensure full repayment |
| VToken Mint | `rayDiv` (half-up) | `rayDivCeil` | Round up debt issued |
| VToken Burn | `rayDiv` (half-up) | `rayDivFloor` | Round down debt reduction |
| Transfer | `rayDiv` (half-up) | `rayDivFloor` | Floor for safety |

**Check version in Solidity:**
```solidity
// In Pool.sol constructor
uint8 poolRevision;

// Mainnet deployments
// v3.0.2: 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2
// v3.1.0: Same proxy, new implementation
// v3.2.0: Same proxy, new implementation
```

**Debugging tip:** v1-3 vs v4+ can have up to 1 wei difference per operation due to rounding changes.
