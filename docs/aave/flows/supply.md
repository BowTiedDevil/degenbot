# Supply Flow

End-to-end execution flow for depositing assets into Aave V3.

## Quick Reference

| Aspect | Details |
|--------|---------|
| **Entry Point** | `Pool.supply(asset, amount, onBehalfOf, referralCode)` |
| **Key Transformations** | [Amount → Scaled Balance](../transformations/index.md#collateral-token-transformations) |
| **State Changes** | `_scaledBalance[onBehalfOf] += scaledAmount` |
| **Events Emitted** | `Supply`, `ReserveUsedAsCollateralEnabled` (conditional) |

---

## Flow Diagram

```mermaid
flowchart TD
    %% Styling definitions
    classDef validation fill:#ffcccc,stroke:#ff0000,stroke-width:2px
    classDef transformation fill:#ccffcc,stroke:#00aa00,stroke-width:2px
    classDef storage fill:#ccccff,stroke:#0000ff,stroke-width:2px
    classDef event fill:#ffffcc,stroke:#aaaa00,stroke-width:2px
    classDef error fill:#ff0000,stroke:#000000,color:#fff
    classDef bridge fill:#ffccff,stroke:#aa00aa,stroke-width:2px
    
    %% Entry point
    Entry["Pool.supply
    asset, amount,
    onBehalfOf,
    referralCode"] --> Branch{Bridge Path?}
    
    %% Bridge path
    Branch -->|Yes| BridgePath["BridgeLogic
    executeMintUnbacked"]
    class BridgePath bridge
    BridgePath --> BridgeEvent["EMIT
    MintUnbacked"]
    class BridgeEvent event
    BridgeEvent --> BridgeCollat["EMIT
    ReserveUsedAsCollateralEnabled"]
    class BridgeCollat event
    
    %% Main flow
    Branch -->|No| MainPath["SupplyLogic
    executeSupply"]
    
    subgraph StateUpdate ["1. State Updates"]
        direction TB
        UpdateState["ReserveLogic
        updateState
        Updates indexes,
        accrues treasury"] --> Validate["ValidationLogic
        validateSupply"]
        class Validate validation
        
        Validate --> UpdateRates["ReserveLogic
        updateInterestRates"]
    end
    
    MainPath --> StateUpdate
    
    subgraph TokenFlow ["2. Token Flow"]
        direction TB
        Transfer["IERC20
        safeTransferFrom
        msg.sender -> aToken"] --> Mint["AToken
        mint"]
        
        Mint --> Transform["TRANSFORMATION
        scaledAmount =
        amount.rayDiv
        (liquidityIndex)"]
        class Transform transformation
        
        Transform --> Store["STORAGE UPDATE
        _scaledBalance
        [onBehalfOf] +=
        scaledAmount"]
        class Store storage
    end
    
    UpdateRates --> TokenFlow
    
    subgraph CollateralCheck ["3. Collateral Configuration"]
        direction TB
        FirstCheck{"First Supply?"} -->|Yes| ValidateCollat["ValidationLogic
        validateUseAsCollateral"]
        class ValidateCollat validation
        
        ValidateCollat --> CanCollat{"Can Use as
        Collateral?"}
        
        CanCollat -->|Yes| SetCollat["UserConfig
        setUsingAsCollateral
        (reserve.id, true)"]
        class SetCollat storage
        
        SetCollat --> CollatEvent["EMIT
        ReserveUsedAsCollateralEnabled"]
        class CollatEvent event
        
        CanCollat -->|No| Skip1[Continue]
        FirstCheck -->|No| Skip1
    end
    
    Store --> FirstCheck
    
    Skip1 --> FinalEvent["EMIT
    Supply"]
    class FinalEvent event
    
    %% Error annotations
    %% CRITICAL: All validations must pass or transaction reverts
    %% CRITICAL: Supply cap check prevents overflow attacks
    
    %% Link styles for critical paths
    linkStyle 8 stroke:#ff0000,stroke-width:3px
    linkStyle 16 stroke:#ff0000,stroke-width:3px
```

---

## Step-by-Step Execution

### 1. Entry Point

**File:** `contracts/protocol/pool/Pool.sol`

```solidity
function supply(
    address asset,
    uint256 amount,
    address onBehalfOf,
    uint16 referralCode
) external virtual override {
    SupplyLogic.executeSupply(
        _reserves,
        _reservesList,
        _usersConfig[onBehalfOf],
        DataTypes.ExecuteSupplyParams({
            asset: asset,
            amount: amount,
            onBehalfOf: onBehalfOf,
            referralCode: referralCode
        })
    );
}
```

### 2. Execute Supply

**File:** `contracts/protocol/libraries/logic/SupplyLogic.sol`

```solidity
function executeSupply(
    mapping(address => DataTypes.ReserveData) storage reserves,
    mapping(uint256 => address) storage reservesList,
    DataTypes.UserConfigurationMap storage userConfig,
    DataTypes.ExecuteSupplyParams memory params
) external {
    DataTypes.ReserveData storage reserve = reserves[params.asset];
    DataTypes.ReserveCache memory reserveCache = reserve.cache();
    
    // Update state (indexes, timestamp)
    reserve.updateState(reserveCache);
    
    // Validate supply
    ValidationLogic.validateSupply(
        reserves,
        reserveCache,
        params.amount,
        params.onBehalfOf
    );
    
    // Update interest rates
    reserve.updateInterestRates(
        reserveCache,
        params.asset,
        0,  // liquidityAdded
        0   // liquidityTaken
    );
    
    // Transfer from user
    IERC20(params.asset).safeTransferFrom(
        msg.sender,
        reserveCache.aTokenAddress,
        params.amount
    );
    
    // Mint aTokens
    bool isFirstSupply = IAToken(reserveCache.aTokenAddress).mint(
        msg.sender,
        params.onBehalfOf,
        params.amount,
        reserveCache.nextLiquidityIndex
    );
    
    // Handle collateral configuration
    if (isFirstSupply) {
        bool canUseAsCollateral = ValidationLogic.validateUseAsCollateral(
            reserves,
            reservesList,
            reserveCache
        );
        
        if (canUseAsCollateral) {
            userConfig.setUsingAsCollateral(reserve.id, true);
            emit ReserveUsedAsCollateralEnabled(
                params.asset,
                params.onBehalfOf
            );
        }
    }
    
    emit Supply(
        params.asset,
        msg.sender,
        params.onBehalfOf,
        params.amount,
        params.referralCode
    );
}
```

### 3. AToken Mint

**File:** `contracts/protocol/tokenization/AToken.sol`

```solidity
function mint(
    address caller,
    address onBehalfOf,
    uint256 amount,
    uint256 index
) external override onlyPool returns (bool) {
    return _mintScaled(caller, onBehalfOf, amount, index);
}

function _mintScaled(
    address caller,
    address onBehalfOf,
    uint256 amount,
    uint256 index
) internal returns (bool) {
    uint256 scaledAmount = amount.rayDiv(index);  // [TRANSFORMATION]
    _scaledBalance[onBehalfOf] += scaledAmount;
    
    // Return true if first supply
    return (scaledAmount != 0 && _scaledBalance[onBehalfOf] == scaledAmount);
}
```

**[TRANSFORMATION]:** See [Collateral Token Transformations](../transformations/index.md#collateral-token-transformations) for details on `amount.rayDiv(index)`

### 4. Validation Checks

**File:** `contracts/protocol/libraries/logic/ValidationLogic.sol`

```solidity
function validateSupply(
    mapping(address => DataTypes.ReserveData) storage reserves,
    DataTypes.ReserveCache memory reserveCache,
    uint256 amount,
    address onBehalfOf
) internal view {
    require(amount != 0, Errors.INVALID_AMOUNT);
    
    // Check reserve is active and not frozen
    require(
        reserveCache.reserveConfiguration.getActive(),
        Errors.RESERVE_INACTIVE
    );
    require(
        !reserveCache.reserveConfiguration.getFrozen(),
        Errors.RESERVE_FROZEN
    );
    
    // Check supply cap
    uint256 supplyCap = reserveCache.reserveConfiguration.getSupplyCap();
    if (supplyCap != 0) {
        uint256 totalSupply = IERC20(reserveCache.aTokenAddress)
            .scaledTotalSupply()
            .rayMul(reserveCache.nextLiquidityIndex);
        
        uint256 scaledCap = supplyCap * 10**reserveCache.reserveConfiguration.getDecimals();
        require(totalSupply + amount <= scaledCap, Errors.SUPPLY_CAP_EXCEEDED);
    }
    
    // Validate onBehalfOf can receive aTokens
    _validateERC20Getter(onBehalfOf);
}
```

---

## Amount Transformations

### Input → Storage

```
User Input (WAD decimals)
    ↓
amount = 1000 * 10^18  // 1000 tokens
    ↓
liquidityIndex = 1.0001 * 10^27  // Current index
    ↓
scaledAmount = amount.rayDiv(liquidityIndex)
             = (1000 * 10^18 * 10^27) / (1.0001 * 10^27)
             = 999.9 * 10^18  (approximate)
    ↓
_scaledBalance[onBehalfOf] += scaledAmount
```

**Key Points:**
- User provides WAD-decimal amount (18 decimals)
- Scaled balance uses RAY precision (27 decimals)
- Index accrues interest over time
- Later withdrawal: `scaledAmount.rayMul(currentIndex)` gives amount + interest

---

## Event Details

### Supply Event

```solidity
event Supply(
    address indexed reserve,      // Asset address
    address indexed user,         // msg.sender
    address indexed onBehalfOf,   // Recipient of aTokens
    uint256 amount,               // Amount supplied
    uint16 referralCode          // Referral code (0 if none)
);
```

### ReserveUsedAsCollateralEnabled Event

Emitted only on first supply if asset can be used as collateral.

```solidity
event ReserveUsedAsCollateralEnabled(
    address indexed reserve,
    address indexed user
);
```

---

## Error Conditions

| Error | Condition | File |
|-------|-----------|------|
| `INVALID_AMOUNT` | `amount == 0` | ValidationLogic.sol |
| `RESERVE_INACTIVE` | Reserve is not active | ValidationLogic.sol |
| `RESERVE_FROZEN` | Reserve is frozen | ValidationLogic.sol |
| `SUPPLY_CAP_EXCEEDED` | `totalSupply + amount > supplyCap` | ValidationLogic.sol |

---

## Related Flows

- [Withdraw Flow](./withdraw.md) - Reverse operation
- [Collateral Management](./collateral_management.md) - Enabling/disabling collateral
- [Liquidation Flow](./liquidation.md) - When collateral is seized

---

## Source File Locations

```
contracts/protocol/pool/Pool.sol
contracts/protocol/libraries/logic/SupplyLogic.sol
contracts/protocol/libraries/logic/ValidationLogic.sol
contracts/protocol/tokenization/AToken.sol
contracts/protocol/libraries/logic/ReserveLogic.sol
```
