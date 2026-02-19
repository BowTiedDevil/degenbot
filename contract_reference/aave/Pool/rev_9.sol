// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {Address} from '../../../dependencies/openzeppelin/contracts/Address.sol';
import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {IPool} from '../../../interfaces/IPool.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {Errors} from '../helpers/Errors.sol';
import {TokenMath} from '../helpers/TokenMath.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {ReserveLogic} from './ReserveLogic.sol';
import {ValidationLogic} from './ValidationLogic.sol';
import {GenericLogic} from './GenericLogic.sol';
import {IsolationModeLogic} from './IsolationModeLogic.sol';

/**
 * @title PoolLogic library
 * @author Aave
 * @notice Implements the logic for Pool specific functions
 */
library PoolLogic {
  using GPv2SafeERC20 for IERC20;
  using TokenMath for uint256;
  using ReserveLogic for DataTypes.ReserveData;
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;

  /**
   * @notice Initialize an asset reserve and add the reserve to the list of reserves
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param params Additional parameters needed for initiation
   * @return true if appended, false if inserted at existing empty spot
   */
  function executeInitReserve(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    DataTypes.InitReserveParams memory params
  ) external returns (bool) {
    require(Address.isContract(params.asset), Errors.NotContract());
    reservesData[params.asset].init(params.aTokenAddress, params.variableDebtAddress);

    bool reserveAlreadyAdded = reservesData[params.asset].id != 0 ||
      reservesList[0] == params.asset;
    require(!reserveAlreadyAdded, Errors.ReserveAlreadyAdded());

    for (uint16 i = 0; i < params.reservesCount; i++) {
      if (reservesList[i] == address(0)) {
        reservesData[params.asset].id = i;
        reservesList[i] = params.asset;
        return false;
      }
    }

    require(params.reservesCount < params.maxNumberReserves, Errors.NoMoreReservesAllowed());
    reservesData[params.asset].id = params.reservesCount;
    reservesList[params.reservesCount] = params.asset;
    return true;
  }

  /**
   * @notice Accumulates interest to all indexes of the reserve
   * @param reserve The state of the reserve
   */
  function executeSyncIndexesState(DataTypes.ReserveData storage reserve) external {
    DataTypes.ReserveCache memory reserveCache = reserve.cache();

    reserve.updateState(reserveCache);
  }

  /**
   * @notice Updates interest rates on the reserve data
   * @param reserve The state of the reserve
   * @param asset The address of the asset
   * @param interestRateStrategyAddress The address of the interest rate
   */
  function executeSyncRatesState(
    DataTypes.ReserveData storage reserve,
    address asset,
    address interestRateStrategyAddress
  ) external {
    DataTypes.ReserveCache memory reserveCache = reserve.cache();

    reserve.updateInterestRatesAndVirtualBalance(
      reserveCache,
      asset,
      0,
      0,
      interestRateStrategyAddress
    );
  }

  /**
   * @notice Rescue and transfer tokens locked in this contract
   * @param token The address of the token
   * @param to The address of the recipient
   * @param amount The amount of token to transfer
   */
  function executeRescueTokens(address token, address to, uint256 amount) external {
    IERC20(token).safeTransfer(to, amount);
  }

  /**
   * @notice Mints the assets accrued through the reserve factor to the treasury in the form of aTokens
   * @param reservesData The state of all the reserves
   * @param assets The list of reserves for which the minting needs to be executed
   */
  function executeMintToTreasury(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    address[] calldata assets
  ) external {
    for (uint256 i = 0; i < assets.length; i++) {
      address assetAddress = assets[i];

      DataTypes.ReserveData storage reserve = reservesData[assetAddress];

      // this cover both inactive reserves and invalid reserves since the flag will be 0 for both
      if (!reserve.configuration.getActive()) {
        continue;
      }

      uint256 accruedToTreasury = reserve.accruedToTreasury;

      if (accruedToTreasury != 0) {
        reserve.accruedToTreasury = 0;
        uint256 normalizedIncome = reserve.getNormalizedIncome();
        uint256 amountToMint = accruedToTreasury.getATokenBalance(normalizedIncome);
        IAToken(reserve.aTokenAddress).mintToTreasury(accruedToTreasury, normalizedIncome);

        emit IPool.MintedToTreasury(assetAddress, amountToMint);
      }
    }
  }

  /**
   * @notice Resets the isolation mode total debt of the given asset to zero
   * @dev It requires the given asset has zero debt ceiling
   * @param reservesData The state of all the reserves
   * @param asset The address of the underlying asset to reset the isolationModeTotalDebt
   */
  function executeResetIsolationModeTotalDebt(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    address asset
  ) external {
    require(reservesData[asset].configuration.getDebtCeiling() == 0, Errors.DebtCeilingNotZero());

    IsolationModeLogic.setIsolationModeTotalDebt(reservesData[asset], asset, 0);
  }

  /**
   * @notice Sets the liquidation grace period of the asset
   * @param reservesData The state of all the reserves
   * @param asset The address of the underlying asset to set the liquidationGracePeriod
   * @param until Timestamp when the liquidation grace period will end
   */
  function executeSetLiquidationGracePeriod(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    address asset,
    uint40 until
  ) external {
    reservesData[asset].liquidationGracePeriodUntil = until;
  }

  /**
   * @notice Drop a reserve
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param asset The address of the underlying asset of the reserve
   */
  function executeDropReserve(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    address asset
  ) external {
    DataTypes.ReserveData storage reserve = reservesData[asset];
    ValidationLogic.validateDropReserve(reservesList, reserve, asset);
    reservesList[reservesData[asset].id] = address(0);
    delete reservesData[asset];
  }

  /**
   * @notice Returns the user account data across all the reserves
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param params Additional params needed for the calculation
   * @return totalCollateralBase The total collateral of the user in the base currency used by the price feed
   * @return totalDebtBase The total debt of the user in the base currency used by the price feed
   * @return availableBorrowsBase The borrowing power left of the user in the base currency used by the price feed
   * @return currentLiquidationThreshold The liquidation threshold of the user
   * @return ltv The loan to value of The user
   * @return healthFactor The current health factor of the user
   */
  function executeGetUserAccountData(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.CalculateUserAccountDataParams memory params
  )
    external
    view
    returns (
      uint256 totalCollateralBase,
      uint256 totalDebtBase,
      uint256 availableBorrowsBase,
      uint256 currentLiquidationThreshold,
      uint256 ltv,
      uint256 healthFactor
    )
  {
    (
      totalCollateralBase,
      totalDebtBase,
      ltv,
      currentLiquidationThreshold,
      healthFactor,

    ) = GenericLogic.calculateUserAccountData(reservesData, reservesList, eModeCategories, params);

    availableBorrowsBase = GenericLogic.calculateAvailableBorrows(
      totalCollateralBase,
      totalDebtBase,
      ltv
    );
  }
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {IScaledBalanceToken} from '../../../interfaces/IScaledBalanceToken.sol';
import {IPriceOracleGetter} from '../../../interfaces/IPriceOracleGetter.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {UserConfiguration} from '../configuration/UserConfiguration.sol';
import {EModeConfiguration} from '../configuration/EModeConfiguration.sol';
import {PercentageMath} from '../math/PercentageMath.sol';
import {WadRayMath} from '../math/WadRayMath.sol';
import {TokenMath} from '../helpers/TokenMath.sol';
import {MathUtils} from '../math/MathUtils.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {ReserveLogic} from './ReserveLogic.sol';
import {EModeLogic} from './EModeLogic.sol';

/**
 * @title GenericLogic library
 * @author Aave
 * @notice Implements protocol-level logic to calculate and validate the state of a user
 */
library GenericLogic {
  using ReserveLogic for DataTypes.ReserveData;
  using TokenMath for uint256;
  using WadRayMath for uint256;
  using PercentageMath for uint256;
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;
  using UserConfiguration for DataTypes.UserConfigurationMap;

  struct CalculateUserAccountDataVars {
    uint256 assetPrice;
    uint256 assetUnit;
    uint256 userBalanceInBaseCurrency;
    uint256 decimals;
    uint256 ltv;
    uint256 liquidationThreshold;
    uint256 i;
    uint256 healthFactor;
    uint256 totalCollateralInBaseCurrency;
    uint256 totalDebtInBaseCurrency;
    uint256 avgLtv;
    uint256 avgLiquidationThreshold;
    uint256 eModeLtv;
    uint256 eModeLiqThreshold;
    uint128 eModeCollateralBitmap;
    address currentReserveAddress;
    bool hasZeroLtvCollateral;
    bool isInEModeCategory;
  }

  /**
   * @notice Calculates the user data across the reserves.
   * @dev It includes the total liquidity/collateral/borrow balances in the base currency used by the price feed,
   * the average Loan To Value, the average Liquidation Ratio, and the Health factor.
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param params Additional parameters needed for the calculation
   * @return The total collateral of the user in the base currency used by the price feed
   * @return The total debt of the user in the base currency used by the price feed
   * @return The average ltv of the user
   * @return The average liquidation threshold of the user
   * @return The health factor of the user
   * @return True if the ltv is zero, false otherwise
   */
  function calculateUserAccountData(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.CalculateUserAccountDataParams memory params
  ) internal view returns (uint256, uint256, uint256, uint256, uint256, bool) {
    if (params.userConfig.isEmpty()) {
      return (0, 0, 0, 0, type(uint256).max, false);
    }

    CalculateUserAccountDataVars memory vars;

    if (params.userEModeCategory != 0) {
      vars.eModeLtv = eModeCategories[params.userEModeCategory].ltv;
      vars.eModeLiqThreshold = eModeCategories[params.userEModeCategory].liquidationThreshold;
      vars.eModeCollateralBitmap = eModeCategories[params.userEModeCategory].collateralBitmap;
    }

    uint256 userConfigCache = params.userConfig.data;
    bool isBorrowed = false;
    bool isEnabledAsCollateral = false;

    while (userConfigCache != 0) {
      (userConfigCache, isBorrowed, isEnabledAsCollateral) = UserConfiguration.getNextFlags(
        userConfigCache
      );
      if (isEnabledAsCollateral || isBorrowed) {
        vars.currentReserveAddress = reservesList[vars.i];

        if (vars.currentReserveAddress != address(0)) {
          DataTypes.ReserveData storage currentReserve = reservesData[vars.currentReserveAddress];

          (vars.ltv, vars.liquidationThreshold, , vars.decimals, ) = currentReserve
            .configuration
            .getParams();

          unchecked {
            vars.assetUnit = 10 ** vars.decimals;
          }

          vars.assetPrice = IPriceOracleGetter(params.oracle).getAssetPrice(
            vars.currentReserveAddress
          );

          if (vars.liquidationThreshold != 0 && isEnabledAsCollateral) {
            vars.userBalanceInBaseCurrency = _getUserBalanceInBaseCurrency(
              params.user,
              currentReserve,
              vars.assetPrice,
              vars.assetUnit
            );

            vars.totalCollateralInBaseCurrency += vars.userBalanceInBaseCurrency;

            vars.isInEModeCategory =
              params.userEModeCategory != 0 &&
              EModeConfiguration.isReserveEnabledOnBitmap(vars.eModeCollateralBitmap, vars.i);

            if (vars.ltv != 0) {
              vars.avgLtv +=
                vars.userBalanceInBaseCurrency *
                (vars.isInEModeCategory ? vars.eModeLtv : vars.ltv);
            } else {
              vars.hasZeroLtvCollateral = true;
            }

            vars.avgLiquidationThreshold +=
              vars.userBalanceInBaseCurrency *
              (vars.isInEModeCategory ? vars.eModeLiqThreshold : vars.liquidationThreshold);
          }

          if (isBorrowed) {
            vars.totalDebtInBaseCurrency += _getUserDebtInBaseCurrency(
              params.user,
              currentReserve,
              vars.assetPrice,
              vars.assetUnit
            );
          }
        }
      }

      unchecked {
        ++vars.i;
      }
    }

    // @note At this point, `avgLiquidationThreshold` represents
    // `SUM(collateral_base_value_i * liquidation_threshold_i)` for all collateral assets.
    // It has 8 decimals (base currency) + 2 decimals (percentage) = 10 decimals.
    // healthFactor has 18 decimals
    // healthFactor = (avgLiquidationThreshold * WAD / totalDebtInBaseCurrency) / 100_00
    // 18 decimals = (10 decimals * 18 decimals / 8 decimals) / 2 decimals = 18 decimals
    vars.healthFactor = (vars.totalDebtInBaseCurrency == 0)
      ? type(uint256).max
      : vars.avgLiquidationThreshold.wadDiv(vars.totalDebtInBaseCurrency) / 100_00;

    unchecked {
      vars.avgLtv = vars.totalCollateralInBaseCurrency != 0
        ? vars.avgLtv / vars.totalCollateralInBaseCurrency
        : 0;
      vars.avgLiquidationThreshold = vars.totalCollateralInBaseCurrency != 0
        ? vars.avgLiquidationThreshold / vars.totalCollateralInBaseCurrency
        : 0;
    }

    return (
      vars.totalCollateralInBaseCurrency,
      vars.totalDebtInBaseCurrency,
      vars.avgLtv,
      vars.avgLiquidationThreshold,
      vars.healthFactor,
      vars.hasZeroLtvCollateral
    );
  }

  /**
   * @notice Calculates the maximum amount that can be borrowed depending on the available collateral, the total debt
   * and the average Loan To Value
   * @param totalCollateralInBaseCurrency The total collateral in the base currency used by the price feed
   * @param totalDebtInBaseCurrency The total borrow balance in the base currency used by the price feed
   * @param ltv The average loan to value
   * @return The amount available to borrow in the base currency of the used by the price feed
   */
  function calculateAvailableBorrows(
    uint256 totalCollateralInBaseCurrency,
    uint256 totalDebtInBaseCurrency,
    uint256 ltv
  ) internal pure returns (uint256) {
    uint256 availableBorrowsInBaseCurrency = totalCollateralInBaseCurrency.percentMulFloor(ltv);

    if (availableBorrowsInBaseCurrency <= totalDebtInBaseCurrency) {
      return 0;
    }

    availableBorrowsInBaseCurrency = availableBorrowsInBaseCurrency - totalDebtInBaseCurrency;
    return availableBorrowsInBaseCurrency;
  }

  /**
   * @notice Calculates total debt of the user in the based currency used to normalize the values of the assets
   * @dev This fetches the `balanceOf` of the variable debt token for the user. For gas reasons, the
   * variable debt balance is calculated by fetching `scaledBalancesOf` normalized debt, which is cheaper than
   * fetching `balanceOf`
   * @param user The address of the user
   * @param reserve The data of the reserve for which the total debt of the user is being calculated
   * @param assetPrice The price of the asset for which the total debt of the user is being calculated
   * @param assetUnit The value representing one full unit of the asset (10^decimals)
   * @return The total debt of the user normalized to the base currency
   */
  function _getUserDebtInBaseCurrency(
    address user,
    DataTypes.ReserveData storage reserve,
    uint256 assetPrice,
    uint256 assetUnit
  ) private view returns (uint256) {
    uint256 userTotalDebt = IScaledBalanceToken(reserve.variableDebtTokenAddress)
      .scaledBalanceOf(user)
      .getVTokenBalance(reserve.getNormalizedDebt());

    return MathUtils.mulDivCeil(userTotalDebt, assetPrice, assetUnit);
  }

  /**
   * @notice Calculates total aToken balance of the user in the based currency used by the price oracle
   * @dev For gas reasons, the aToken balance is calculated by fetching `scaledBalancesOf` normalized debt, which
   * is cheaper than fetching `balanceOf`
   * @param user The address of the user
   * @param reserve The data of the reserve for which the total aToken balance of the user is being calculated
   * @param assetPrice The price of the asset for which the total aToken balance of the user is being calculated
   * @param assetUnit The value representing one full unit of the asset (10^decimals)
   * @return The total aToken balance of the user normalized to the base currency of the price oracle
   */
  function _getUserBalanceInBaseCurrency(
    address user,
    DataTypes.ReserveData storage reserve,
    uint256 assetPrice,
    uint256 assetUnit
  ) private view returns (uint256) {
    uint256 balance = (
      IScaledBalanceToken(reserve.aTokenAddress).scaledBalanceOf(user).getATokenBalance(
        reserve.getNormalizedIncome()
      )
    ) * assetPrice;

    unchecked {
      return balance / assetUnit;
    }
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

/**
 * @title IAaveIncentivesController
 * @author Aave
 * @notice Defines the basic interface for an Aave Incentives Controller.
 * @dev It only contains one single function, needed as a hook on aToken and debtToken transfers.
 */
interface IAaveIncentivesController {
  /**
   * @dev Called by the corresponding asset on transfer hook in order to update the rewards distribution.
   * @dev The units of `totalSupply` and `userBalance` should be the same.
   * @param user The address of the user whose asset balance has changed
   * @param totalSupply The total supply of the asset prior to user balance change
   * @param userBalance The previous user balance prior to balance change
   */
  function handleAction(address user, uint256 totalSupply, uint256 userBalance) external;
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IERC20} from '../dependencies/openzeppelin/contracts/IERC20.sol';
import {IScaledBalanceToken} from './IScaledBalanceToken.sol';
import {IInitializableAToken} from './IInitializableAToken.sol';

/**
 * @title IAToken
 * @author Aave
 * @notice Defines the basic interface for an AToken.
 */
interface IAToken is IERC20, IScaledBalanceToken, IInitializableAToken {
  /**
   * @dev Emitted during the transfer action
   * @param from The user whose tokens are being transferred
   * @param to The recipient
   * @param value The scaled amount being transferred
   * @param index The next liquidity index of the reserve
   */
  event BalanceTransfer(address indexed from, address indexed to, uint256 value, uint256 index);

  /**
   * @notice Mints `amount` aTokens to `user`
   * @param caller The address performing the mint
   * @param onBehalfOf The address of the user that will receive the minted aTokens
   * @param scaledAmount The scaled amount of tokens getting minted
   * @param index The next liquidity index of the reserve
   * @return `true` if the the previous balance of the user was 0
   */
  function mint(
    address caller,
    address onBehalfOf,
    uint256 scaledAmount,
    uint256 index
  ) external returns (bool);

  /**
   * @notice Burns aTokens from `user` and sends the equivalent amount of underlying to `receiverOfUnderlying`.
   * @dev Passing both the unscaled and scaled amounts enhances precision. The `scaledAmount` is used for precise balance updates,
   * while the `amount` is used for the underlying asset transfer, preventing cumulative rounding errors.
   * @dev In some instances, a mint event may be emitted from a burn transaction if the amount to burn is less than the interest that the user accrued.
   * @param from The address from which the aTokens will be burned
   * @param receiverOfUnderlying The address that will receive the underlying
   * @param amount The amount of underlying to be burned (non scaled)
   * @param scaledAmount The scaled amount of aTokens to be burned (scaled)
   * @param index The next liquidity index of the reserve
   * @return `true` if the the new balance of the user is 0
   */
  function burn(
    address from,
    address receiverOfUnderlying,
    uint256 amount,
    uint256 scaledAmount,
    uint256 index
  ) external returns (bool);

  /**
   * @notice Mints aTokens to the reserve treasury
   * @param scaledAmount The scaled amount of tokens getting minted
   * @param index The next liquidity index of the reserve
   */
  function mintToTreasury(uint256 scaledAmount, uint256 index) external;

  /**
   * @notice Transfers aTokens in the event of a borrow being liquidated, in case the liquidator reclaims the aToken.
   * @dev Passing both the unscaled and scaled amounts enhances precision. The `scaledAmount` is used for precise balance updates,
   * while the `amount` is used for logging and consistency, preventing cumulative rounding errors.
   * @param from The address getting liquidated, current owner of the aTokens
   * @param to The recipient
   * @param amount The amount of tokens getting transferred (non-scaled)
   * @param scaledAmount The scaled amount of tokens getting transferred (scaled)
   * @param index The next liquidity index of the reserve
   */
  function transferOnLiquidation(
    address from,
    address to,
    uint256 amount,
    uint256 scaledAmount,
    uint256 index
  ) external;

  /**
   * @notice Transfers the underlying asset to `target`.
   * @dev Used by the Pool to transfer assets in borrow(), withdraw() and flashLoan()
   * @param target The recipient of the underlying
   * @param amount The amount getting transferred
   */
  function transferUnderlyingTo(address target, uint256 amount) external;

  /**
   * @notice Allow passing a signed message to approve spending
   * @dev implements the permit function as for
   * https://github.com/ethereum/EIPs/blob/8a34d644aacf0f9f8f00815307fd7dd5da07655f/EIPS/eip-2612.md
   * @param owner The owner of the funds
   * @param spender The spender
   * @param value The amount
   * @param deadline The deadline timestamp, type(uint256).max for max deadline
   * @param v Signature param
   * @param s Signature param
   * @param r Signature param
   */
  function permit(
    address owner,
    address spender,
    uint256 value,
    uint256 deadline,
    uint8 v,
    bytes32 r,
    bytes32 s
  ) external;

  /**
   * @notice Returns the address of the underlying asset of this aToken (E.g. WETH for aWETH)
   * @return The address of the underlying asset
   */
  function UNDERLYING_ASSET_ADDRESS() external view returns (address);

  /**
   * @notice Returns the address of the Aave treasury, receiving the fees on this aToken.
   * @return Address of the Aave treasury
   */
  function RESERVE_TREASURY_ADDRESS() external view returns (address);

  /**
   * @notice Get the domain separator for the token
   * @dev Return cached value if chainId matches cache, otherwise recomputes separator
   * @return The domain separator of the token at current chain
   */
  function DOMAIN_SEPARATOR() external view returns (bytes32);

  /**
   * @notice Returns the nonce for owner.
   * @param owner The address of the owner
   * @return The nonce of the owner
   */
  function nonces(address owner) external view returns (uint256);

  /**
   * @notice Rescue and transfer tokens locked in this contract
   * @param token The address of the token
   * @param to The address of the recipient
   * @param amount The amount of token to transfer
   */
  function rescueTokens(address token, address to, uint256 amount) external;
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

/**
 * @title IPoolAddressesProvider
 * @author Aave
 * @notice Defines the basic interface for a Pool Addresses Provider.
 */
interface IPoolAddressesProvider {
  /**
   * @dev Emitted when the market identifier is updated.
   * @param oldMarketId The old id of the market
   * @param newMarketId The new id of the market
   */
  event MarketIdSet(string indexed oldMarketId, string indexed newMarketId);

  /**
   * @dev Emitted when the pool is updated.
   * @param oldAddress The old address of the Pool
   * @param newAddress The new address of the Pool
   */
  event PoolUpdated(address indexed oldAddress, address indexed newAddress);

  /**
   * @dev Emitted when the pool configurator is updated.
   * @param oldAddress The old address of the PoolConfigurator
   * @param newAddress The new address of the PoolConfigurator
   */
  event PoolConfiguratorUpdated(address indexed oldAddress, address indexed newAddress);

  /**
   * @dev Emitted when the price oracle is updated.
   * @param oldAddress The old address of the PriceOracle
   * @param newAddress The new address of the PriceOracle
   */
  event PriceOracleUpdated(address indexed oldAddress, address indexed newAddress);

  /**
   * @dev Emitted when the ACL manager is updated.
   * @param oldAddress The old address of the ACLManager
   * @param newAddress The new address of the ACLManager
   */
  event ACLManagerUpdated(address indexed oldAddress, address indexed newAddress);

  /**
   * @dev Emitted when the ACL admin is updated.
   * @param oldAddress The old address of the ACLAdmin
   * @param newAddress The new address of the ACLAdmin
   */
  event ACLAdminUpdated(address indexed oldAddress, address indexed newAddress);

  /**
   * @dev Emitted when the price oracle sentinel is updated.
   * @param oldAddress The old address of the PriceOracleSentinel
   * @param newAddress The new address of the PriceOracleSentinel
   */
  event PriceOracleSentinelUpdated(address indexed oldAddress, address indexed newAddress);

  /**
   * @dev Emitted when the pool data provider is updated.
   * @param oldAddress The old address of the PoolDataProvider
   * @param newAddress The new address of the PoolDataProvider
   */
  event PoolDataProviderUpdated(address indexed oldAddress, address indexed newAddress);

  /**
   * @dev Emitted when a new proxy is created.
   * @param id The identifier of the proxy
   * @param proxyAddress The address of the created proxy contract
   * @param implementationAddress The address of the implementation contract
   */
  event ProxyCreated(
    bytes32 indexed id,
    address indexed proxyAddress,
    address indexed implementationAddress
  );

  /**
   * @dev Emitted when a new non-proxied contract address is registered.
   * @param id The identifier of the contract
   * @param oldAddress The address of the old contract
   * @param newAddress The address of the new contract
   */
  event AddressSet(bytes32 indexed id, address indexed oldAddress, address indexed newAddress);

  /**
   * @dev Emitted when the implementation of the proxy registered with id is updated
   * @param id The identifier of the contract
   * @param proxyAddress The address of the proxy contract
   * @param oldImplementationAddress The address of the old implementation contract
   * @param newImplementationAddress The address of the new implementation contract
   */
  event AddressSetAsProxy(
    bytes32 indexed id,
    address indexed proxyAddress,
    address oldImplementationAddress,
    address indexed newImplementationAddress
  );

  /**
   * @notice Returns the id of the Aave market to which this contract points to.
   * @return The market id
   */
  function getMarketId() external view returns (string memory);

  /**
   * @notice Associates an id with a specific PoolAddressesProvider.
   * @dev This can be used to create an onchain registry of PoolAddressesProviders to
   * identify and validate multiple Aave markets.
   * @param newMarketId The market id
   */
  function setMarketId(string calldata newMarketId) external;

  /**
   * @notice Returns an address by its identifier.
   * @dev The returned address might be an EOA or a contract, potentially proxied
   * @dev It returns ZERO if there is no registered address with the given id
   * @param id The id
   * @return The address of the registered for the specified id
   */
  function getAddress(bytes32 id) external view returns (address);

  /**
   * @notice General function to update the implementation of a proxy registered with
   * certain `id`. If there is no proxy registered, it will instantiate one and
   * set as implementation the `newImplementationAddress`.
   * @dev IMPORTANT Use this function carefully, only for ids that don't have an explicit
   * setter function, in order to avoid unexpected consequences
   * @param id The id
   * @param newImplementationAddress The address of the new implementation
   */
  function setAddressAsProxy(bytes32 id, address newImplementationAddress) external;

  /**
   * @notice Sets an address for an id replacing the address saved in the addresses map.
   * @dev IMPORTANT Use this function carefully, as it will do a hard replacement
   * @param id The id
   * @param newAddress The address to set
   */
  function setAddress(bytes32 id, address newAddress) external;

  /**
   * @notice Returns the address of the Pool proxy.
   * @return The Pool proxy address
   */
  function getPool() external view returns (address);

  /**
   * @notice Updates the implementation of the Pool, or creates a proxy
   * setting the new `pool` implementation when the function is called for the first time.
   * @param newPoolImpl The new Pool implementation
   */
  function setPoolImpl(address newPoolImpl) external;

  /**
   * @notice Returns the address of the PoolConfigurator proxy.
   * @return The PoolConfigurator proxy address
   */
  function getPoolConfigurator() external view returns (address);

  /**
   * @notice Updates the implementation of the PoolConfigurator, or creates a proxy
   * setting the new `PoolConfigurator` implementation when the function is called for the first time.
   * @param newPoolConfiguratorImpl The new PoolConfigurator implementation
   */
  function setPoolConfiguratorImpl(address newPoolConfiguratorImpl) external;

  /**
   * @notice Returns the address of the price oracle.
   * @return The address of the PriceOracle
   */
  function getPriceOracle() external view returns (address);

  /**
   * @notice Updates the address of the price oracle.
   * @param newPriceOracle The address of the new PriceOracle
   */
  function setPriceOracle(address newPriceOracle) external;

  /**
   * @notice Returns the address of the ACL manager.
   * @return The address of the ACLManager
   */
  function getACLManager() external view returns (address);

  /**
   * @notice Updates the address of the ACL manager.
   * @param newAclManager The address of the new ACLManager
   */
  function setACLManager(address newAclManager) external;

  /**
   * @notice Returns the address of the ACL admin.
   * @return The address of the ACL admin
   */
  function getACLAdmin() external view returns (address);

  /**
   * @notice Updates the address of the ACL admin.
   * @param newAclAdmin The address of the new ACL admin
   */
  function setACLAdmin(address newAclAdmin) external;

  /**
   * @notice Returns the address of the price oracle sentinel.
   * @return The address of the PriceOracleSentinel
   */
  function getPriceOracleSentinel() external view returns (address);

  /**
   * @notice Updates the address of the price oracle sentinel.
   * @param newPriceOracleSentinel The address of the new PriceOracleSentinel
   */
  function setPriceOracleSentinel(address newPriceOracleSentinel) external;

  /**
   * @notice Returns the address of the data provider.
   * @return The address of the DataProvider
   */
  function getPoolDataProvider() external view returns (address);

  /**
   * @notice Updates the address of the data provider.
   * @param newDataProvider The address of the new DataProvider
   */
  function setPoolDataProvider(address newDataProvider) external;
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.0;

/**
 * @title WadRayMath library
 * @author Aave
 * @notice Provides functions to perform calculations with Wad and Ray units
 * @dev Provides mul and div function for wads (decimal numbers with 18 digits of precision) and rays (decimal numbers
 * with 27 digits of precision).
 * @dev Default operations round half up (if a value is >= .5, it will be rounded up, otherwise rounded down).
 * @dev For specific rounding behaviors, functions with `Floor` and `Ceil` suffixes or a `Rounding` parameter are available.
 */
library WadRayMath {
  enum Rounding {
    Floor,
    Ceil
  }

  // HALF_WAD and HALF_RAY expressed with extended notation as constant with operations are not supported in Yul assembly
  uint256 internal constant WAD = 1e18;
  uint256 internal constant HALF_WAD = 0.5e18;

  uint256 internal constant RAY = 1e27;
  uint256 internal constant HALF_RAY = 0.5e27;

  uint256 internal constant WAD_RAY_RATIO = 1e9;

  /**
   * @dev Multiplies two wad, rounding half up to the nearest wad
   * @dev assembly optimized for improved gas savings, see https://twitter.com/transmissions11/status/1451131036377571328
   * @param a Wad
   * @param b Wad
   * @return c = a*b, in wad
   */
  function wadMul(uint256 a, uint256 b) internal pure returns (uint256 c) {
    // to avoid overflow, a <= (type(uint256).max - HALF_WAD) / b
    assembly {
      if iszero(or(iszero(b), iszero(gt(a, div(sub(not(0), HALF_WAD), b))))) {
        revert(0, 0)
      }

      c := div(add(mul(a, b), HALF_WAD), WAD)
    }
  }

  /**
   * @dev Divides two wad, rounding half up to the nearest wad
   * @dev assembly optimized for improved gas savings, see https://twitter.com/transmissions11/status/1451131036377571328
   * @param a Wad
   * @param b Wad
   * @return c = a/b, in wad
   */
  function wadDiv(uint256 a, uint256 b) internal pure returns (uint256 c) {
    // to avoid overflow, a <= (type(uint256).max - halfB) / WAD
    assembly {
      if or(iszero(b), iszero(iszero(gt(a, div(sub(not(0), div(b, 2)), WAD))))) {
        revert(0, 0)
      }

      c := div(add(mul(a, WAD), div(b, 2)), b)
    }
  }

  function rayMul(uint256 a, uint256 b) internal pure returns (uint256 c) {
    assembly {
      // to avoid overflow, a <= (type(uint256).max - HALF_RAY) / b
      if iszero(or(iszero(b), iszero(gt(a, div(sub(not(0), HALF_RAY), b))))) {
        revert(0, 0)
      }
      c := div(add(mul(a, b), HALF_RAY), RAY)
    }
  }

  function rayMul(uint256 a, uint256 b, Rounding rounding) internal pure returns (uint256 c) {
    if (rounding == Rounding.Floor) return rayMulFloor(a, b);
    return rayMulCeil(a, b);
  }

  function rayMulFloor(uint256 a, uint256 b) internal pure returns (uint256 c) {
    assembly {
      // Overflow check: Ensure a * b does not exceed uint256 max
      if iszero(or(iszero(b), iszero(gt(a, div(not(0), b))))) {
        revert(0, 0)
      }

      c := div(mul(a, b), RAY)
    }
  }

  function rayMulCeil(uint256 a, uint256 b) internal pure returns (uint256 c) {
    assembly {
      // Overflow check: Ensure a * b does not exceed uint256 max
      if iszero(or(iszero(b), iszero(gt(a, div(not(0), b))))) {
        revert(0, 0)
      }

      let product := mul(a, b)
      c := add(div(product, RAY), iszero(iszero(mod(product, RAY))))
    }
  }

  /**
   * @notice Divides two ray, rounding half up to the nearest ray
   * @dev assembly optimized for improved gas savings, see https://twitter.com/transmissions11/status/1451131036377571328
   * @param a Ray
   * @param b Ray
   * @return c = a raydiv b
   */
  function rayDiv(uint256 a, uint256 b) internal pure returns (uint256 c) {
    assembly {
      // to avoid overflow, a <= (type(uint256).max - b / 2) / RAY
      if or(iszero(b), iszero(iszero(gt(a, div(sub(not(0), div(b, 2)), RAY))))) {
        revert(0, 0)
      }
      c := div(add(mul(a, RAY), div(b, 2)), b)
    }
  }

  function rayDiv(uint256 a, uint256 b, Rounding rounding) internal pure returns (uint256 c) {
    if (rounding == Rounding.Floor) return rayDivFloor(a, b);
    return rayDivCeil(a, b);
  }

  function rayDivCeil(uint256 a, uint256 b) internal pure returns (uint256 c) {
    assembly {
      // Overflow check: Ensure a * RAY does not exceed uint256 max
      if or(iszero(b), iszero(iszero(gt(a, div(not(0), RAY))))) {
        revert(0, 0)
      }
      let scaled := mul(a, RAY)
      c := add(div(scaled, b), iszero(iszero(mod(scaled, b))))
    }
  }

  function rayDivFloor(uint256 a, uint256 b) internal pure returns (uint256 c) {
    assembly {
      // Overflow check: Ensure a * RAY does not exceed uint256 max
      if or(iszero(b), iszero(iszero(gt(a, div(not(0), RAY))))) {
        revert(0, 0)
      }
      c := div(mul(a, RAY), b)
    }
  }

  /**
   * @dev Casts ray down to wad
   * @dev assembly optimized for improved gas savings, see https://twitter.com/transmissions11/status/1451131036377571328
   * @param a Ray
   * @return b = a converted to wad, rounded half up to the nearest wad
   */
  function rayToWad(uint256 a) internal pure returns (uint256 b) {
    assembly {
      b := div(a, WAD_RAY_RATIO)
      let remainder := mod(a, WAD_RAY_RATIO)
      if iszero(lt(remainder, div(WAD_RAY_RATIO, 2))) {
        b := add(b, 1)
      }
    }
  }

  /**
   * @dev Converts wad up to ray
   * @dev assembly optimized for improved gas savings, see https://twitter.com/transmissions11/status/1451131036377571328
   * @param a Wad
   * @return b = a converted in ray
   */
  function wadToRay(uint256 a) internal pure returns (uint256 b) {
    // to avoid overflow, b/WAD_RAY_RATIO == a
    assembly {
      b := mul(a, WAD_RAY_RATIO)

      if iszero(eq(div(b, WAD_RAY_RATIO), a)) {
        revert(0, 0)
      }
    }
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IERC20} from '../dependencies/openzeppelin/contracts/IERC20.sol';

/**
 * @title IERC20WithPermit
 * @author Aave
 * @notice Interface for the permit function (EIP-2612)
 */
interface IERC20WithPermit is IERC20 {
  /**
   * @notice Allow passing a signed message to approve spending
   * @dev implements the permit function as for
   * https://github.com/ethereum/EIPs/blob/8a34d644aacf0f9f8f00815307fd7dd5da07655f/EIPS/eip-2612.md
   * @param owner The owner of the funds
   * @param spender The spender
   * @param value The amount
   * @param deadline The deadline timestamp, type(uint256).max for max deadline
   * @param v Signature param
   * @param s Signature param
   * @param r Signature param
   */
  function permit(
    address owner,
    address spender,
    uint256 value,
    uint256 deadline,
    uint8 v,
    bytes32 r,
    bytes32 s
  ) external;
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {Errors} from '../helpers/Errors.sol';
import {DataTypes} from '../types/DataTypes.sol';

/**
 * @title ReserveConfiguration library
 * @author Aave
 * @notice Implements the bitmap logic to handle the reserve configuration
 */
library ReserveConfiguration {
  uint256 internal constant LTV_MASK =                       0x000000000000000000000000000000000000000000000000000000000000FFFF; // prettier-ignore
  uint256 internal constant LIQUIDATION_THRESHOLD_MASK =     0x00000000000000000000000000000000000000000000000000000000FFFF0000; // prettier-ignore
  uint256 internal constant LIQUIDATION_BONUS_MASK =         0x0000000000000000000000000000000000000000000000000000FFFF00000000; // prettier-ignore
  uint256 internal constant DECIMALS_MASK =                  0x00000000000000000000000000000000000000000000000000FF000000000000; // prettier-ignore
  uint256 internal constant ACTIVE_MASK =                    0x0000000000000000000000000000000000000000000000000100000000000000; // prettier-ignore
  uint256 internal constant FROZEN_MASK =                    0x0000000000000000000000000000000000000000000000000200000000000000; // prettier-ignore
  uint256 internal constant BORROWING_MASK =                 0x0000000000000000000000000000000000000000000000000400000000000000; // prettier-ignore
  // @notice there is an unoccupied hole of 1 bit at position 59 from pre 3.2 stableBorrowRateEnabled
  uint256 internal constant PAUSED_MASK =                    0x0000000000000000000000000000000000000000000000001000000000000000; // prettier-ignore
  uint256 internal constant BORROWABLE_IN_ISOLATION_MASK =   0x0000000000000000000000000000000000000000000000002000000000000000; // prettier-ignore
  uint256 internal constant SILOED_BORROWING_MASK =          0x0000000000000000000000000000000000000000000000004000000000000000; // prettier-ignore
  uint256 internal constant FLASHLOAN_ENABLED_MASK =         0x0000000000000000000000000000000000000000000000008000000000000000; // prettier-ignore
  uint256 internal constant RESERVE_FACTOR_MASK =            0x00000000000000000000000000000000000000000000FFFF0000000000000000; // prettier-ignore
  uint256 internal constant BORROW_CAP_MASK =                0x00000000000000000000000000000000000FFFFFFFFF00000000000000000000; // prettier-ignore
  uint256 internal constant SUPPLY_CAP_MASK =                0x00000000000000000000000000FFFFFFFFF00000000000000000000000000000; // prettier-ignore
  uint256 internal constant LIQUIDATION_PROTOCOL_FEE_MASK =  0x0000000000000000000000FFFF00000000000000000000000000000000000000; // prettier-ignore
  //@notice there is an unoccupied hole of 8 bits from 168 to 175 left from pre 3.2 eModeCategory
  //@notice there is an unoccupied hole of 34 bits from 176 to 211 left from pre 3.4 unbackedMintCap
  uint256 internal constant DEBT_CEILING_MASK =              0x0FFFFFFFFFF00000000000000000000000000000000000000000000000000000; // prettier-ignore
  //@notice DEPRECATED: in v3.4 all reserves have virtual accounting enabled
  uint256 internal constant VIRTUAL_ACC_ACTIVE_MASK =        0x1000000000000000000000000000000000000000000000000000000000000000; // prettier-ignore

  /// @dev For the LTV, the start bit is 0 (up to 15), hence no bitshifting is needed
  uint256 internal constant LIQUIDATION_THRESHOLD_START_BIT_POSITION = 16;
  uint256 internal constant LIQUIDATION_BONUS_START_BIT_POSITION = 32;
  uint256 internal constant RESERVE_DECIMALS_START_BIT_POSITION = 48;
  uint256 internal constant IS_ACTIVE_START_BIT_POSITION = 56;
  uint256 internal constant IS_FROZEN_START_BIT_POSITION = 57;
  uint256 internal constant BORROWING_ENABLED_START_BIT_POSITION = 58;
  uint256 internal constant IS_PAUSED_START_BIT_POSITION = 60;
  uint256 internal constant BORROWABLE_IN_ISOLATION_START_BIT_POSITION = 61;
  uint256 internal constant SILOED_BORROWING_START_BIT_POSITION = 62;
  uint256 internal constant FLASHLOAN_ENABLED_START_BIT_POSITION = 63;
  uint256 internal constant RESERVE_FACTOR_START_BIT_POSITION = 64;
  uint256 internal constant BORROW_CAP_START_BIT_POSITION = 80;
  uint256 internal constant SUPPLY_CAP_START_BIT_POSITION = 116;
  uint256 internal constant LIQUIDATION_PROTOCOL_FEE_START_BIT_POSITION = 152;
  //@notice there is an unoccupied hole of 8 bits from 168 to 175 left from pre 3.2 eModeCategory
  //@notice there is an unoccupied hole of 34 bits from 176 to 211 left from pre 3.4 unbackedMintCap
  uint256 internal constant DEBT_CEILING_START_BIT_POSITION = 212;
  //@notice DEPRECATED: in v3.4 all reserves have virtual accounting enabled
  uint256 internal constant VIRTUAL_ACC_START_BIT_POSITION = 252;

  uint256 internal constant MAX_VALID_LTV = 65535;
  uint256 internal constant MAX_VALID_LIQUIDATION_THRESHOLD = 65535;
  uint256 internal constant MAX_VALID_LIQUIDATION_BONUS = 65535;
  uint256 internal constant MAX_VALID_DECIMALS = 255;
  uint256 internal constant MAX_VALID_RESERVE_FACTOR = 65535;
  uint256 internal constant MAX_VALID_BORROW_CAP = 68719476735;
  uint256 internal constant MAX_VALID_SUPPLY_CAP = 68719476735;
  uint256 internal constant MAX_VALID_LIQUIDATION_PROTOCOL_FEE = 65535;
  uint256 internal constant MAX_VALID_DEBT_CEILING = 1099511627775;

  uint256 public constant DEBT_CEILING_DECIMALS = 2;
  uint16 public constant MAX_RESERVES_COUNT = 128;

  /**
   * @notice Sets the Loan to Value of the reserve
   * @param self The reserve configuration
   * @param ltv The new ltv
   */
  function setLtv(DataTypes.ReserveConfigurationMap memory self, uint256 ltv) internal pure {
    require(ltv <= MAX_VALID_LTV, Errors.InvalidLtv());

    self.data = (self.data & ~LTV_MASK) | ltv;
  }

  /**
   * @notice Gets the Loan to Value of the reserve
   * @param self The reserve configuration
   * @return The loan to value
   */
  function getLtv(DataTypes.ReserveConfigurationMap memory self) internal pure returns (uint256) {
    return self.data & LTV_MASK;
  }

  /**
   * @notice Sets the liquidation threshold of the reserve
   * @param self The reserve configuration
   * @param threshold The new liquidation threshold
   */
  function setLiquidationThreshold(
    DataTypes.ReserveConfigurationMap memory self,
    uint256 threshold
  ) internal pure {
    require(threshold <= MAX_VALID_LIQUIDATION_THRESHOLD, Errors.InvalidLiquidationThreshold());

    self.data =
      (self.data & ~LIQUIDATION_THRESHOLD_MASK) |
      (threshold << LIQUIDATION_THRESHOLD_START_BIT_POSITION);
  }

  /**
   * @notice Gets the liquidation threshold of the reserve
   * @param self The reserve configuration
   * @return The liquidation threshold
   */
  function getLiquidationThreshold(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256) {
    return (self.data & LIQUIDATION_THRESHOLD_MASK) >> LIQUIDATION_THRESHOLD_START_BIT_POSITION;
  }

  /**
   * @notice Sets the liquidation bonus of the reserve
   * @param self The reserve configuration
   * @param bonus The new liquidation bonus
   */
  function setLiquidationBonus(
    DataTypes.ReserveConfigurationMap memory self,
    uint256 bonus
  ) internal pure {
    require(bonus <= MAX_VALID_LIQUIDATION_BONUS, Errors.InvalidLiquidationBonus());

    self.data =
      (self.data & ~LIQUIDATION_BONUS_MASK) |
      (bonus << LIQUIDATION_BONUS_START_BIT_POSITION);
  }

  /**
   * @notice Gets the liquidation bonus of the reserve
   * @param self The reserve configuration
   * @return The liquidation bonus
   */
  function getLiquidationBonus(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256) {
    return (self.data & LIQUIDATION_BONUS_MASK) >> LIQUIDATION_BONUS_START_BIT_POSITION;
  }

  /**
   * @notice Sets the decimals of the underlying asset of the reserve
   * @param self The reserve configuration
   * @param decimals The decimals
   */
  function setDecimals(
    DataTypes.ReserveConfigurationMap memory self,
    uint256 decimals
  ) internal pure {
    require(decimals <= MAX_VALID_DECIMALS, Errors.InvalidDecimals());

    self.data = (self.data & ~DECIMALS_MASK) | (decimals << RESERVE_DECIMALS_START_BIT_POSITION);
  }

  /**
   * @notice Gets the decimals of the underlying asset of the reserve
   * @param self The reserve configuration
   * @return The decimals of the asset
   */
  function getDecimals(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256) {
    return (self.data & DECIMALS_MASK) >> RESERVE_DECIMALS_START_BIT_POSITION;
  }

  /**
   * @notice Sets the active state of the reserve
   * @param self The reserve configuration
   * @param active The active state
   */
  function setActive(DataTypes.ReserveConfigurationMap memory self, bool active) internal pure {
    self.data =
      (self.data & ~ACTIVE_MASK) |
      (uint256(active ? 1 : 0) << IS_ACTIVE_START_BIT_POSITION);
  }

  /**
   * @notice Gets the active state of the reserve
   * @param self The reserve configuration
   * @return The active state
   */
  function getActive(DataTypes.ReserveConfigurationMap memory self) internal pure returns (bool) {
    return (self.data & ACTIVE_MASK) != 0;
  }

  /**
   * @notice Sets the frozen state of the reserve
   * @param self The reserve configuration
   * @param frozen The frozen state
   */
  function setFrozen(DataTypes.ReserveConfigurationMap memory self, bool frozen) internal pure {
    self.data =
      (self.data & ~FROZEN_MASK) |
      (uint256(frozen ? 1 : 0) << IS_FROZEN_START_BIT_POSITION);
  }

  /**
   * @notice Gets the frozen state of the reserve
   * @param self The reserve configuration
   * @return The frozen state
   */
  function getFrozen(DataTypes.ReserveConfigurationMap memory self) internal pure returns (bool) {
    return (self.data & FROZEN_MASK) != 0;
  }

  /**
   * @notice Sets the paused state of the reserve
   * @param self The reserve configuration
   * @param paused The paused state
   */
  function setPaused(DataTypes.ReserveConfigurationMap memory self, bool paused) internal pure {
    self.data =
      (self.data & ~PAUSED_MASK) |
      (uint256(paused ? 1 : 0) << IS_PAUSED_START_BIT_POSITION);
  }

  /**
   * @notice Gets the paused state of the reserve
   * @param self The reserve configuration
   * @return The paused state
   */
  function getPaused(DataTypes.ReserveConfigurationMap memory self) internal pure returns (bool) {
    return (self.data & PAUSED_MASK) != 0;
  }

  /**
   * @notice Sets the borrowable in isolation flag for the reserve.
   * @dev When this flag is set to true, the asset will be borrowable against isolated collaterals and the borrowed
   * amount will be accumulated in the isolated collateral's total debt exposure.
   * @dev Only assets of the same family (eg USD stablecoins) should be borrowable in isolation mode to keep
   * consistency in the debt ceiling calculations.
   * @param self The reserve configuration
   * @param borrowable True if the asset is borrowable
   */
  function setBorrowableInIsolation(
    DataTypes.ReserveConfigurationMap memory self,
    bool borrowable
  ) internal pure {
    self.data =
      (self.data & ~BORROWABLE_IN_ISOLATION_MASK) |
      (uint256(borrowable ? 1 : 0) << BORROWABLE_IN_ISOLATION_START_BIT_POSITION);
  }

  /**
   * @notice Gets the borrowable in isolation flag for the reserve.
   * @dev If the returned flag is true, the asset is borrowable against isolated collateral. Assets borrowed with
   * isolated collateral is accounted for in the isolated collateral's total debt exposure.
   * @dev Only assets of the same family (eg USD stablecoins) should be borrowable in isolation mode to keep
   * consistency in the debt ceiling calculations.
   * @param self The reserve configuration
   * @return The borrowable in isolation flag
   */
  function getBorrowableInIsolation(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (bool) {
    return (self.data & BORROWABLE_IN_ISOLATION_MASK) != 0;
  }

  /**
   * @notice Sets the siloed borrowing flag for the reserve.
   * @dev When this flag is set to true, users borrowing this asset will not be allowed to borrow any other asset.
   * @param self The reserve configuration
   * @param siloed True if the asset is siloed
   */
  function setSiloedBorrowing(
    DataTypes.ReserveConfigurationMap memory self,
    bool siloed
  ) internal pure {
    self.data =
      (self.data & ~SILOED_BORROWING_MASK) |
      (uint256(siloed ? 1 : 0) << SILOED_BORROWING_START_BIT_POSITION);
  }

  /**
   * @notice Gets the siloed borrowing flag for the reserve.
   * @dev When this flag is set to true, users borrowing this asset will not be allowed to borrow any other asset.
   * @param self The reserve configuration
   * @return The siloed borrowing flag
   */
  function getSiloedBorrowing(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (bool) {
    return (self.data & SILOED_BORROWING_MASK) != 0;
  }

  /**
   * @notice Enables or disables borrowing on the reserve
   * @param self The reserve configuration
   * @param enabled True if the borrowing needs to be enabled, false otherwise
   */
  function setBorrowingEnabled(
    DataTypes.ReserveConfigurationMap memory self,
    bool enabled
  ) internal pure {
    self.data =
      (self.data & ~BORROWING_MASK) |
      (uint256(enabled ? 1 : 0) << BORROWING_ENABLED_START_BIT_POSITION);
  }

  /**
   * @notice Gets the borrowing state of the reserve
   * @param self The reserve configuration
   * @return The borrowing state
   */
  function getBorrowingEnabled(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (bool) {
    return (self.data & BORROWING_MASK) != 0;
  }

  /**
   * @notice Sets the reserve factor of the reserve
   * @param self The reserve configuration
   * @param reserveFactor The reserve factor
   */
  function setReserveFactor(
    DataTypes.ReserveConfigurationMap memory self,
    uint256 reserveFactor
  ) internal pure {
    require(reserveFactor <= MAX_VALID_RESERVE_FACTOR, Errors.InvalidReserveFactor());

    self.data =
      (self.data & ~RESERVE_FACTOR_MASK) |
      (reserveFactor << RESERVE_FACTOR_START_BIT_POSITION);
  }

  /**
   * @notice Gets the reserve factor of the reserve
   * @param self The reserve configuration
   * @return The reserve factor
   */
  function getReserveFactor(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256) {
    return (self.data & RESERVE_FACTOR_MASK) >> RESERVE_FACTOR_START_BIT_POSITION;
  }

  /**
   * @notice Sets the borrow cap of the reserve
   * @param self The reserve configuration
   * @param borrowCap The borrow cap
   */
  function setBorrowCap(
    DataTypes.ReserveConfigurationMap memory self,
    uint256 borrowCap
  ) internal pure {
    require(borrowCap <= MAX_VALID_BORROW_CAP, Errors.InvalidBorrowCap());

    self.data = (self.data & ~BORROW_CAP_MASK) | (borrowCap << BORROW_CAP_START_BIT_POSITION);
  }

  /**
   * @notice Gets the borrow cap of the reserve
   * @param self The reserve configuration
   * @return The borrow cap
   */
  function getBorrowCap(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256) {
    return (self.data & BORROW_CAP_MASK) >> BORROW_CAP_START_BIT_POSITION;
  }

  /**
   * @notice Sets the supply cap of the reserve
   * @param self The reserve configuration
   * @param supplyCap The supply cap
   */
  function setSupplyCap(
    DataTypes.ReserveConfigurationMap memory self,
    uint256 supplyCap
  ) internal pure {
    require(supplyCap <= MAX_VALID_SUPPLY_CAP, Errors.InvalidSupplyCap());

    self.data = (self.data & ~SUPPLY_CAP_MASK) | (supplyCap << SUPPLY_CAP_START_BIT_POSITION);
  }

  /**
   * @notice Gets the supply cap of the reserve
   * @param self The reserve configuration
   * @return The supply cap
   */
  function getSupplyCap(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256) {
    return (self.data & SUPPLY_CAP_MASK) >> SUPPLY_CAP_START_BIT_POSITION;
  }

  /**
   * @notice Sets the debt ceiling in isolation mode for the asset
   * @param self The reserve configuration
   * @param ceiling The maximum debt ceiling for the asset
   */
  function setDebtCeiling(
    DataTypes.ReserveConfigurationMap memory self,
    uint256 ceiling
  ) internal pure {
    require(ceiling <= MAX_VALID_DEBT_CEILING, Errors.InvalidDebtCeiling());

    self.data = (self.data & ~DEBT_CEILING_MASK) | (ceiling << DEBT_CEILING_START_BIT_POSITION);
  }

  /**
   * @notice Gets the debt ceiling for the asset if the asset is in isolation mode
   * @param self The reserve configuration
   * @return The debt ceiling (0 = isolation mode disabled)
   */
  function getDebtCeiling(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256) {
    return (self.data & DEBT_CEILING_MASK) >> DEBT_CEILING_START_BIT_POSITION;
  }

  /**
   * @notice Sets the liquidation protocol fee of the reserve
   * @param self The reserve configuration
   * @param liquidationProtocolFee The liquidation protocol fee
   */
  function setLiquidationProtocolFee(
    DataTypes.ReserveConfigurationMap memory self,
    uint256 liquidationProtocolFee
  ) internal pure {
    require(
      liquidationProtocolFee <= MAX_VALID_LIQUIDATION_PROTOCOL_FEE,
      Errors.InvalidLiquidationProtocolFee()
    );

    self.data =
      (self.data & ~LIQUIDATION_PROTOCOL_FEE_MASK) |
      (liquidationProtocolFee << LIQUIDATION_PROTOCOL_FEE_START_BIT_POSITION);
  }

  /**
   * @dev Gets the liquidation protocol fee
   * @param self The reserve configuration
   * @return The liquidation protocol fee
   */
  function getLiquidationProtocolFee(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256) {
    return
      (self.data & LIQUIDATION_PROTOCOL_FEE_MASK) >> LIQUIDATION_PROTOCOL_FEE_START_BIT_POSITION;
  }

  /**
   * @notice Sets the flashloanable flag for the reserve
   * @param self The reserve configuration
   * @param flashLoanEnabled True if the asset is flashloanable, false otherwise
   */
  function setFlashLoanEnabled(
    DataTypes.ReserveConfigurationMap memory self,
    bool flashLoanEnabled
  ) internal pure {
    self.data =
      (self.data & ~FLASHLOAN_ENABLED_MASK) |
      (uint256(flashLoanEnabled ? 1 : 0) << FLASHLOAN_ENABLED_START_BIT_POSITION);
  }

  /**
   * @notice Gets the flashloanable flag for the reserve
   * @param self The reserve configuration
   * @return The flashloanable flag
   */
  function getFlashLoanEnabled(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (bool) {
    return (self.data & FLASHLOAN_ENABLED_MASK) != 0;
  }

  /**
   * @notice Forcefully set the virtual account active state of the reserve to `true`
   * @dev DEPRECATED: in v3.4 all reserves have virtual accounting enabled.
   * The flag is carried along for backward compatibility with integrations directly querying the configuration.
   * @param self The reserve configuration
   */
  function setVirtualAccActive(DataTypes.ReserveConfigurationMap memory self) internal pure {
    self.data =
      (self.data & ~VIRTUAL_ACC_ACTIVE_MASK) |
      (uint256(1) << VIRTUAL_ACC_START_BIT_POSITION);
  }

  /**
   * @notice Gets the configuration flags of the reserve
   * @param self The reserve configuration
   * @return The state flag representing active
   * @return The state flag representing frozen
   * @return The state flag representing borrowing enabled
   * @return The state flag representing paused
   */
  function getFlags(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (bool, bool, bool, bool) {
    uint256 dataLocal = self.data;

    return (
      (dataLocal & ACTIVE_MASK) != 0,
      (dataLocal & FROZEN_MASK) != 0,
      (dataLocal & BORROWING_MASK) != 0,
      (dataLocal & PAUSED_MASK) != 0
    );
  }

  /**
   * @notice Gets the configuration parameters of the reserve from storage
   * @param self The reserve configuration
   * @return The state param representing ltv
   * @return The state param representing liquidation threshold
   * @return The state param representing liquidation bonus
   * @return The state param representing reserve decimals
   * @return The state param representing reserve factor
   */
  function getParams(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256, uint256, uint256, uint256, uint256) {
    uint256 dataLocal = self.data;

    return (
      dataLocal & LTV_MASK,
      (dataLocal & LIQUIDATION_THRESHOLD_MASK) >> LIQUIDATION_THRESHOLD_START_BIT_POSITION,
      (dataLocal & LIQUIDATION_BONUS_MASK) >> LIQUIDATION_BONUS_START_BIT_POSITION,
      (dataLocal & DECIMALS_MASK) >> RESERVE_DECIMALS_START_BIT_POSITION,
      (dataLocal & RESERVE_FACTOR_MASK) >> RESERVE_FACTOR_START_BIT_POSITION
    );
  }

  /**
   * @notice Gets the caps parameters of the reserve from storage
   * @param self The reserve configuration
   * @return The state param representing borrow cap
   * @return The state param representing supply cap.
   */
  function getCaps(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256, uint256) {
    uint256 dataLocal = self.data;

    return (
      (dataLocal & BORROW_CAP_MASK) >> BORROW_CAP_START_BIT_POSITION,
      (dataLocal & SUPPLY_CAP_MASK) >> SUPPLY_CAP_START_BIT_POSITION
    );
  }
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {IPool} from '../../../interfaces/IPool.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {ValidationLogic} from './ValidationLogic.sol';

/**
 * @title EModeLogic library
 * @author Aave
 * @notice Implements the base logic for all the actions related to the eMode
 */
library EModeLogic {
  /**
   * @notice Updates the user efficiency mode category
   * @dev Will revert if user is borrowing non-compatible asset or change will drop HF < HEALTH_FACTOR_LIQUIDATION_THRESHOLD
   * @dev Emits the `UserEModeSet` event
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param usersEModeCategory The state of all users efficiency mode category
   * @param userConfig The user configuration mapping that tracks the supplied/borrowed assets
   * @param user The selected user
   * @param oracle The address of the oracle
   * @param categoryId The selected eMode categoryId
   */
  function executeSetUserEMode(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    mapping(address => uint8) storage usersEModeCategory,
    DataTypes.UserConfigurationMap storage userConfig,
    address user,
    address oracle,
    uint8 categoryId
  ) external {
    if (usersEModeCategory[user] == categoryId) return;

    ValidationLogic.validateSetUserEMode(eModeCategories, userConfig, categoryId);

    usersEModeCategory[user] = categoryId;

    ValidationLogic.validateHealthFactor(
      reservesData,
      reservesList,
      eModeCategories,
      userConfig,
      user,
      categoryId,
      oracle
    );
    emit IPool.UserEModeSet(user, categoryId);
  }
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {SafeCast} from 'openzeppelin-contracts/contracts/utils/math/SafeCast.sol';
import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {IVariableDebtToken} from '../../../interfaces/IVariableDebtToken.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {IPool} from '../../../interfaces/IPool.sol';
import {TokenMath} from '../../libraries/helpers/TokenMath.sol';
import {UserConfiguration} from '../configuration/UserConfiguration.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {ValidationLogic} from './ValidationLogic.sol';
import {ReserveLogic} from './ReserveLogic.sol';
import {IsolationModeLogic} from './IsolationModeLogic.sol';

/**
 * @title BorrowLogic library
 * @author Aave
 * @notice Implements the base logic for all the actions related to borrowing
 */
library BorrowLogic {
  using TokenMath for uint256;
  using ReserveLogic for DataTypes.ReserveCache;
  using ReserveLogic for DataTypes.ReserveData;
  using GPv2SafeERC20 for IERC20;
  using UserConfiguration for DataTypes.UserConfigurationMap;
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;
  using SafeCast for uint256;

  /**
   * @notice Implements the borrow feature. Borrowing allows users that provided collateral to draw liquidity from the
   * Aave protocol proportionally to their collateralization power. For isolated positions, it also increases the
   * isolated debt.
   * @dev  Emits the `Borrow()` event
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param userConfig The user configuration mapping that tracks the supplied/borrowed assets
   * @param params The additional parameters needed to execute the borrow function
   */
  function executeBorrow(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.UserConfigurationMap storage userConfig,
    DataTypes.ExecuteBorrowParams memory params
  ) external {
    DataTypes.ReserveData storage reserve = reservesData[params.asset];
    DataTypes.ReserveCache memory reserveCache = reserve.cache();

    reserve.updateState(reserveCache);

    uint256 amountScaled = params.amount.getVTokenMintScaledAmount(
      reserveCache.nextVariableBorrowIndex
    );

    ValidationLogic.validateBorrow(
      reservesData,
      reservesList,
      eModeCategories,
      DataTypes.ValidateBorrowParams({
        reserveCache: reserveCache,
        userConfig: userConfig,
        asset: params.asset,
        userAddress: params.onBehalfOf,
        amountScaled: amountScaled,
        interestRateMode: params.interestRateMode,
        oracle: params.oracle,
        userEModeCategory: params.userEModeCategory,
        priceOracleSentinel: params.priceOracleSentinel
      })
    );

    reserveCache.nextScaledVariableDebt = IVariableDebtToken(reserveCache.variableDebtTokenAddress)
      .mint(
        params.user,
        params.onBehalfOf,
        params.amount,
        amountScaled,
        reserveCache.nextVariableBorrowIndex
      );

    uint16 cachedReserveId = reserve.id;
    if (!userConfig.isBorrowing(cachedReserveId)) {
      userConfig.setBorrowing(cachedReserveId, true);
    }

    IsolationModeLogic.increaseIsolatedDebtIfIsolated(
      reservesData,
      reservesList,
      userConfig,
      reserveCache,
      params.amount
    );

    reserve.updateInterestRatesAndVirtualBalance(
      reserveCache,
      params.asset,
      0,
      params.releaseUnderlying ? params.amount : 0,
      params.interestRateStrategyAddress
    );

    if (params.releaseUnderlying) {
      IAToken(reserveCache.aTokenAddress).transferUnderlyingTo(params.user, params.amount);
    }

    ValidationLogic.validateHFAndLtv(
      reservesData,
      reservesList,
      eModeCategories,
      userConfig,
      params.onBehalfOf,
      params.userEModeCategory,
      params.oracle
    );

    emit IPool.Borrow(
      params.asset,
      params.user,
      params.onBehalfOf,
      params.amount,
      DataTypes.InterestRateMode.VARIABLE,
      reserve.currentVariableBorrowRate,
      params.referralCode
    );
  }

  /**
   * @notice Implements the repay feature. Repaying transfers the underlying back to the aToken and clears the
   * equivalent amount of debt for the user by burning the corresponding debt token. For isolated positions, it also
   * reduces the isolated debt.
   * @dev  Emits the `Repay()` event
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param onBehalfOfConfig The user configuration mapping that tracks the supplied/borrowed assets
   * @param params The additional parameters needed to execute the repay function
   * @return The actual amount being repaid
   */
  function executeRepay(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.UserConfigurationMap storage onBehalfOfConfig,
    DataTypes.ExecuteRepayParams memory params
  ) external returns (uint256) {
    DataTypes.ReserveData storage reserve = reservesData[params.asset];
    DataTypes.ReserveCache memory reserveCache = reserve.cache();
    reserve.updateState(reserveCache);

    uint256 userDebtScaled = IVariableDebtToken(reserveCache.variableDebtTokenAddress)
      .scaledBalanceOf(params.onBehalfOf);
    uint256 userDebt = userDebtScaled.getVTokenBalance(reserveCache.nextVariableBorrowIndex);

    ValidationLogic.validateRepay(
      params.user,
      reserveCache,
      params.amount,
      params.interestRateMode,
      params.onBehalfOf,
      userDebtScaled
    );

    uint256 paybackAmount = params.amount;
    if (params.useATokens && params.amount == type(uint256).max) {
      // Allows a user to repay with aTokens without leaving dust from interest.
      paybackAmount = IAToken(reserveCache.aTokenAddress)
        .scaledBalanceOf(params.user)
        .getATokenBalance(reserveCache.nextLiquidityIndex);
    }

    if (paybackAmount > userDebt) {
      paybackAmount = userDebt;
    }

    bool noMoreDebt;
    (noMoreDebt, reserveCache.nextScaledVariableDebt) = IVariableDebtToken(
      reserveCache.variableDebtTokenAddress
    ).burn({
        from: params.onBehalfOf,
        scaledAmount: paybackAmount.getVTokenBurnScaledAmount(reserveCache.nextVariableBorrowIndex),
        index: reserveCache.nextVariableBorrowIndex
      });

    reserve.updateInterestRatesAndVirtualBalance(
      reserveCache,
      params.asset,
      params.useATokens ? 0 : paybackAmount,
      0,
      params.interestRateStrategyAddress
    );

    if (noMoreDebt) {
      onBehalfOfConfig.setBorrowing(reserve.id, false);
    }

    IsolationModeLogic.reduceIsolatedDebtIfIsolated(
      reservesData,
      reservesList,
      onBehalfOfConfig,
      reserveCache,
      paybackAmount
    );

    // in case of aToken repayment the sender must always repay on behalf of itself
    if (params.useATokens) {
      // As aToken.burn rounds up the burned shares, we ensure at least an equivalent of >= paybackAmount is burned.
      bool zeroBalanceAfterBurn = IAToken(reserveCache.aTokenAddress).burn({
        from: params.user,
        receiverOfUnderlying: reserveCache.aTokenAddress,
        amount: paybackAmount,
        scaledAmount: paybackAmount.getATokenBurnScaledAmount(reserveCache.nextLiquidityIndex),
        index: reserveCache.nextLiquidityIndex
      });
      if (onBehalfOfConfig.isUsingAsCollateral(reserve.id)) {
        if (zeroBalanceAfterBurn) {
          onBehalfOfConfig.setUsingAsCollateral(reserve.id, params.asset, params.user, false);
        }

        if (onBehalfOfConfig.isBorrowingAny()) {
          ValidationLogic.validateHealthFactor(
            reservesData,
            reservesList,
            eModeCategories,
            onBehalfOfConfig,
            params.user,
            params.userEModeCategory,
            params.oracle
          );
        }
      }
    } else {
      IERC20(params.asset).safeTransferFrom(params.user, reserveCache.aTokenAddress, paybackAmount);
    }

    emit IPool.Repay(
      params.asset,
      params.onBehalfOf,
      params.user,
      paybackAmount,
      params.useATokens
    );

    return paybackAmount;
  }
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {UserConfiguration} from '../libraries/configuration/UserConfiguration.sol';
import {ReserveConfiguration} from '../libraries/configuration/ReserveConfiguration.sol';
import {ReserveLogic} from '../libraries/logic/ReserveLogic.sol';
import {DataTypes} from '../libraries/types/DataTypes.sol';

/**
 * @title PoolStorage
 * @author Aave
 * @notice Contract used as storage of the Pool contract.
 * @dev It defines the storage layout of the Pool contract.
 */
contract PoolStorage {
  using ReserveLogic for DataTypes.ReserveData;
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;
  using UserConfiguration for DataTypes.UserConfigurationMap;

  // Map of reserves and their data (underlyingAssetOfReserve => reserveData)
  mapping(address => DataTypes.ReserveData) internal _reserves;

  // Map of users address and their configuration data (userAddress => userConfiguration)
  mapping(address => DataTypes.UserConfigurationMap) internal _usersConfig;

  // List of reserves as a map (reserveId => reserve).
  // It is structured as a mapping for gas savings reasons, using the reserve id as index
  mapping(uint256 => address) internal _reservesList;

  // List of eMode categories as a map (eModeCategoryId => eModeCategory).
  // It is structured as a mapping for gas savings reasons, using the eModeCategoryId as index
  mapping(uint8 => DataTypes.EModeCategory) internal _eModeCategories;

  // Map of users address and their eMode category (userAddress => eModeCategoryId)
  mapping(address => uint8) internal _usersEModeCategory;

  // Fee of the protocol bridge, expressed in bps
  uint256 internal __DEPRECATED_bridgeProtocolFee;

  // FlashLoan Premium, expressed in bps.
  // From v3.4 all flashloan premium is paid to treasury.
  uint128 internal _flashLoanPremium;

  // FlashLoan premium paid to protocol treasury, expressed in bps.
  // From v3.4 all flashloan premium is paid to treasury.
  uint128 internal __DEPRECATED_flashLoanPremiumToProtocol;

  // DEPRECATED on v3.2.0
  uint64 internal __DEPRECATED_maxStableRateBorrowSizePercent;

  // Maximum number of active reserves there have been in the protocol. It is the upper bound of the reserves list
  uint16 internal _reservesCount;

  // Allowlisted permissionManagers can enable collaterals & switch eModes on behalf of a user
  mapping(address user => mapping(address permittedPositionManager => bool))
    internal _positionManager;
}

// SPDX-License-Identifier: LGPL-3.0-or-later
pragma solidity ^0.8.10;

import {IERC20} from '../../openzeppelin/contracts/IERC20.sol';

/// @title Gnosis Protocol v2 Safe ERC20 Transfer Library
/// @author Gnosis Developers
/// @dev Gas-efficient version of Openzeppelin's SafeERC20 contract.
library GPv2SafeERC20 {
  /// @dev Wrapper around a call to the ERC20 function `transfer` that reverts
  /// also when the token returns `false`.
  function safeTransfer(IERC20 token, address to, uint256 value) internal {
    bytes4 selector_ = token.transfer.selector;

    // solhint-disable-next-line no-inline-assembly
    assembly {
      let freeMemoryPointer := mload(0x40)
      mstore(freeMemoryPointer, selector_)
      mstore(add(freeMemoryPointer, 4), and(to, 0xffffffffffffffffffffffffffffffffffffffff))
      mstore(add(freeMemoryPointer, 36), value)

      if iszero(call(gas(), token, 0, freeMemoryPointer, 68, 0, 0)) {
        returndatacopy(0, 0, returndatasize())
        revert(0, returndatasize())
      }
    }

    require(getLastTransferResult(token), 'GPv2: failed transfer');
  }

  /// @dev Wrapper around a call to the ERC20 function `transferFrom` that
  /// reverts also when the token returns `false`.
  function safeTransferFrom(IERC20 token, address from, address to, uint256 value) internal {
    bytes4 selector_ = token.transferFrom.selector;

    // solhint-disable-next-line no-inline-assembly
    assembly {
      let freeMemoryPointer := mload(0x40)
      mstore(freeMemoryPointer, selector_)
      mstore(add(freeMemoryPointer, 4), and(from, 0xffffffffffffffffffffffffffffffffffffffff))
      mstore(add(freeMemoryPointer, 36), and(to, 0xffffffffffffffffffffffffffffffffffffffff))
      mstore(add(freeMemoryPointer, 68), value)

      if iszero(call(gas(), token, 0, freeMemoryPointer, 100, 0, 0)) {
        returndatacopy(0, 0, returndatasize())
        revert(0, returndatasize())
      }
    }

    require(getLastTransferResult(token), 'GPv2: failed transferFrom');
  }

  /// @dev Verifies that the last return was a successful `transfer*` call.
  /// This is done by checking that the return data is either empty, or
  /// is a valid ABI encoded boolean.
  function getLastTransferResult(IERC20 token) private view returns (bool success) {
    // NOTE: Inspecting previous return data requires assembly. Note that
    // we write the return data to memory 0 in the case where the return
    // data size is 32, this is OK since the first 64 bytes of memory are
    // reserved by Solidy as a scratch space that can be used within
    // assembly blocks.
    // <https://docs.soliditylang.org/en/v0.7.6/internals/layout_in_memory.html>
    // solhint-disable-next-line no-inline-assembly
    assembly {
      /// @dev Revert with an ABI encoded Solidity error with a message
      /// that fits into 32-bytes.
      ///
      /// An ABI encoded Solidity error has the following memory layout:
      ///
      /// ------------+----------------------------------
      ///  byte range | value
      /// ------------+----------------------------------
      ///  0x00..0x04 |        selector("Error(string)")
      ///  0x04..0x24 |      string offset (always 0x20)
      ///  0x24..0x44 |                    string length
      ///  0x44..0x64 | string value, padded to 32-bytes
      function revertWithMessage(length, message) {
        mstore(0x00, '\x08\xc3\x79\xa0')
        mstore(0x04, 0x20)
        mstore(0x24, length)
        mstore(0x44, message)
        revert(0x00, 0x64)
      }

      switch returndatasize()
      // Non-standard ERC20 transfer without return.
      case 0 {
        // NOTE: When the return data size is 0, verify that there
        // is code at the address. This is done in order to maintain
        // compatibility with Solidity calling conventions.
        // <https://docs.soliditylang.org/en/v0.7.6/control-structures.html#external-function-calls>
        if iszero(extcodesize(token)) {
          revertWithMessage(20, 'GPv2: not a contract')
        }

        success := 1
      }
      // Standard ERC20 transfer returning boolean success value.
      case 32 {
        returndatacopy(0, 0, returndatasize())

        // NOTE: For ABI encoding v1, any non-zero value is accepted
        // as `true` for a boolean. In order to stay compatible with
        // OpenZeppelin's `SafeERC20` library which is known to work
        // with the existing ERC20 implementation we care about,
        // make sure we return success for any non-zero return value
        // from the `transfer*` call.
        success := iszero(iszero(mload(0)))
      }
      default {
        revertWithMessage(31, 'GPv2: malformed transfer result')
      }
    }
  }
}

// SPDX-License-Identifier: MIT
// OpenZeppelin Contracts (last updated v5.1.0) (utils/math/SafeCast.sol)
// This file was procedurally generated from scripts/generate/templates/SafeCast.js.

pragma solidity ^0.8.20;

/**
 * @dev Wrappers over Solidity's uintXX/intXX/bool casting operators with added overflow
 * checks.
 *
 * Downcasting from uint256/int256 in Solidity does not revert on overflow. This can
 * easily result in undesired exploitation or bugs, since developers usually
 * assume that overflows raise errors. `SafeCast` restores this intuition by
 * reverting the transaction when such an operation overflows.
 *
 * Using this library instead of the unchecked operations eliminates an entire
 * class of bugs, so it's recommended to use it always.
 */
library SafeCast {
    /**
     * @dev Value doesn't fit in an uint of `bits` size.
     */
    error SafeCastOverflowedUintDowncast(uint8 bits, uint256 value);

    /**
     * @dev An int value doesn't fit in an uint of `bits` size.
     */
    error SafeCastOverflowedIntToUint(int256 value);

    /**
     * @dev Value doesn't fit in an int of `bits` size.
     */
    error SafeCastOverflowedIntDowncast(uint8 bits, int256 value);

    /**
     * @dev An uint value doesn't fit in an int of `bits` size.
     */
    error SafeCastOverflowedUintToInt(uint256 value);

    /**
     * @dev Returns the downcasted uint248 from uint256, reverting on
     * overflow (when the input is greater than largest uint248).
     *
     * Counterpart to Solidity's `uint248` operator.
     *
     * Requirements:
     *
     * - input must fit into 248 bits
     */
    function toUint248(uint256 value) internal pure returns (uint248) {
        if (value > type(uint248).max) {
            revert SafeCastOverflowedUintDowncast(248, value);
        }
        return uint248(value);
    }

    /**
     * @dev Returns the downcasted uint240 from uint256, reverting on
     * overflow (when the input is greater than largest uint240).
     *
     * Counterpart to Solidity's `uint240` operator.
     *
     * Requirements:
     *
     * - input must fit into 240 bits
     */
    function toUint240(uint256 value) internal pure returns (uint240) {
        if (value > type(uint240).max) {
            revert SafeCastOverflowedUintDowncast(240, value);
        }
        return uint240(value);
    }

    /**
     * @dev Returns the downcasted uint232 from uint256, reverting on
     * overflow (when the input is greater than largest uint232).
     *
     * Counterpart to Solidity's `uint232` operator.
     *
     * Requirements:
     *
     * - input must fit into 232 bits
     */
    function toUint232(uint256 value) internal pure returns (uint232) {
        if (value > type(uint232).max) {
            revert SafeCastOverflowedUintDowncast(232, value);
        }
        return uint232(value);
    }

    /**
     * @dev Returns the downcasted uint224 from uint256, reverting on
     * overflow (when the input is greater than largest uint224).
     *
     * Counterpart to Solidity's `uint224` operator.
     *
     * Requirements:
     *
     * - input must fit into 224 bits
     */
    function toUint224(uint256 value) internal pure returns (uint224) {
        if (value > type(uint224).max) {
            revert SafeCastOverflowedUintDowncast(224, value);
        }
        return uint224(value);
    }

    /**
     * @dev Returns the downcasted uint216 from uint256, reverting on
     * overflow (when the input is greater than largest uint216).
     *
     * Counterpart to Solidity's `uint216` operator.
     *
     * Requirements:
     *
     * - input must fit into 216 bits
     */
    function toUint216(uint256 value) internal pure returns (uint216) {
        if (value > type(uint216).max) {
            revert SafeCastOverflowedUintDowncast(216, value);
        }
        return uint216(value);
    }

    /**
     * @dev Returns the downcasted uint208 from uint256, reverting on
     * overflow (when the input is greater than largest uint208).
     *
     * Counterpart to Solidity's `uint208` operator.
     *
     * Requirements:
     *
     * - input must fit into 208 bits
     */
    function toUint208(uint256 value) internal pure returns (uint208) {
        if (value > type(uint208).max) {
            revert SafeCastOverflowedUintDowncast(208, value);
        }
        return uint208(value);
    }

    /**
     * @dev Returns the downcasted uint200 from uint256, reverting on
     * overflow (when the input is greater than largest uint200).
     *
     * Counterpart to Solidity's `uint200` operator.
     *
     * Requirements:
     *
     * - input must fit into 200 bits
     */
    function toUint200(uint256 value) internal pure returns (uint200) {
        if (value > type(uint200).max) {
            revert SafeCastOverflowedUintDowncast(200, value);
        }
        return uint200(value);
    }

    /**
     * @dev Returns the downcasted uint192 from uint256, reverting on
     * overflow (when the input is greater than largest uint192).
     *
     * Counterpart to Solidity's `uint192` operator.
     *
     * Requirements:
     *
     * - input must fit into 192 bits
     */
    function toUint192(uint256 value) internal pure returns (uint192) {
        if (value > type(uint192).max) {
            revert SafeCastOverflowedUintDowncast(192, value);
        }
        return uint192(value);
    }

    /**
     * @dev Returns the downcasted uint184 from uint256, reverting on
     * overflow (when the input is greater than largest uint184).
     *
     * Counterpart to Solidity's `uint184` operator.
     *
     * Requirements:
     *
     * - input must fit into 184 bits
     */
    function toUint184(uint256 value) internal pure returns (uint184) {
        if (value > type(uint184).max) {
            revert SafeCastOverflowedUintDowncast(184, value);
        }
        return uint184(value);
    }

    /**
     * @dev Returns the downcasted uint176 from uint256, reverting on
     * overflow (when the input is greater than largest uint176).
     *
     * Counterpart to Solidity's `uint176` operator.
     *
     * Requirements:
     *
     * - input must fit into 176 bits
     */
    function toUint176(uint256 value) internal pure returns (uint176) {
        if (value > type(uint176).max) {
            revert SafeCastOverflowedUintDowncast(176, value);
        }
        return uint176(value);
    }

    /**
     * @dev Returns the downcasted uint168 from uint256, reverting on
     * overflow (when the input is greater than largest uint168).
     *
     * Counterpart to Solidity's `uint168` operator.
     *
     * Requirements:
     *
     * - input must fit into 168 bits
     */
    function toUint168(uint256 value) internal pure returns (uint168) {
        if (value > type(uint168).max) {
            revert SafeCastOverflowedUintDowncast(168, value);
        }
        return uint168(value);
    }

    /**
     * @dev Returns the downcasted uint160 from uint256, reverting on
     * overflow (when the input is greater than largest uint160).
     *
     * Counterpart to Solidity's `uint160` operator.
     *
     * Requirements:
     *
     * - input must fit into 160 bits
     */
    function toUint160(uint256 value) internal pure returns (uint160) {
        if (value > type(uint160).max) {
            revert SafeCastOverflowedUintDowncast(160, value);
        }
        return uint160(value);
    }

    /**
     * @dev Returns the downcasted uint152 from uint256, reverting on
     * overflow (when the input is greater than largest uint152).
     *
     * Counterpart to Solidity's `uint152` operator.
     *
     * Requirements:
     *
     * - input must fit into 152 bits
     */
    function toUint152(uint256 value) internal pure returns (uint152) {
        if (value > type(uint152).max) {
            revert SafeCastOverflowedUintDowncast(152, value);
        }
        return uint152(value);
    }

    /**
     * @dev Returns the downcasted uint144 from uint256, reverting on
     * overflow (when the input is greater than largest uint144).
     *
     * Counterpart to Solidity's `uint144` operator.
     *
     * Requirements:
     *
     * - input must fit into 144 bits
     */
    function toUint144(uint256 value) internal pure returns (uint144) {
        if (value > type(uint144).max) {
            revert SafeCastOverflowedUintDowncast(144, value);
        }
        return uint144(value);
    }

    /**
     * @dev Returns the downcasted uint136 from uint256, reverting on
     * overflow (when the input is greater than largest uint136).
     *
     * Counterpart to Solidity's `uint136` operator.
     *
     * Requirements:
     *
     * - input must fit into 136 bits
     */
    function toUint136(uint256 value) internal pure returns (uint136) {
        if (value > type(uint136).max) {
            revert SafeCastOverflowedUintDowncast(136, value);
        }
        return uint136(value);
    }

    /**
     * @dev Returns the downcasted uint128 from uint256, reverting on
     * overflow (when the input is greater than largest uint128).
     *
     * Counterpart to Solidity's `uint128` operator.
     *
     * Requirements:
     *
     * - input must fit into 128 bits
     */
    function toUint128(uint256 value) internal pure returns (uint128) {
        if (value > type(uint128).max) {
            revert SafeCastOverflowedUintDowncast(128, value);
        }
        return uint128(value);
    }

    /**
     * @dev Returns the downcasted uint120 from uint256, reverting on
     * overflow (when the input is greater than largest uint120).
     *
     * Counterpart to Solidity's `uint120` operator.
     *
     * Requirements:
     *
     * - input must fit into 120 bits
     */
    function toUint120(uint256 value) internal pure returns (uint120) {
        if (value > type(uint120).max) {
            revert SafeCastOverflowedUintDowncast(120, value);
        }
        return uint120(value);
    }

    /**
     * @dev Returns the downcasted uint112 from uint256, reverting on
     * overflow (when the input is greater than largest uint112).
     *
     * Counterpart to Solidity's `uint112` operator.
     *
     * Requirements:
     *
     * - input must fit into 112 bits
     */
    function toUint112(uint256 value) internal pure returns (uint112) {
        if (value > type(uint112).max) {
            revert SafeCastOverflowedUintDowncast(112, value);
        }
        return uint112(value);
    }

    /**
     * @dev Returns the downcasted uint104 from uint256, reverting on
     * overflow (when the input is greater than largest uint104).
     *
     * Counterpart to Solidity's `uint104` operator.
     *
     * Requirements:
     *
     * - input must fit into 104 bits
     */
    function toUint104(uint256 value) internal pure returns (uint104) {
        if (value > type(uint104).max) {
            revert SafeCastOverflowedUintDowncast(104, value);
        }
        return uint104(value);
    }

    /**
     * @dev Returns the downcasted uint96 from uint256, reverting on
     * overflow (when the input is greater than largest uint96).
     *
     * Counterpart to Solidity's `uint96` operator.
     *
     * Requirements:
     *
     * - input must fit into 96 bits
     */
    function toUint96(uint256 value) internal pure returns (uint96) {
        if (value > type(uint96).max) {
            revert SafeCastOverflowedUintDowncast(96, value);
        }
        return uint96(value);
    }

    /**
     * @dev Returns the downcasted uint88 from uint256, reverting on
     * overflow (when the input is greater than largest uint88).
     *
     * Counterpart to Solidity's `uint88` operator.
     *
     * Requirements:
     *
     * - input must fit into 88 bits
     */
    function toUint88(uint256 value) internal pure returns (uint88) {
        if (value > type(uint88).max) {
            revert SafeCastOverflowedUintDowncast(88, value);
        }
        return uint88(value);
    }

    /**
     * @dev Returns the downcasted uint80 from uint256, reverting on
     * overflow (when the input is greater than largest uint80).
     *
     * Counterpart to Solidity's `uint80` operator.
     *
     * Requirements:
     *
     * - input must fit into 80 bits
     */
    function toUint80(uint256 value) internal pure returns (uint80) {
        if (value > type(uint80).max) {
            revert SafeCastOverflowedUintDowncast(80, value);
        }
        return uint80(value);
    }

    /**
     * @dev Returns the downcasted uint72 from uint256, reverting on
     * overflow (when the input is greater than largest uint72).
     *
     * Counterpart to Solidity's `uint72` operator.
     *
     * Requirements:
     *
     * - input must fit into 72 bits
     */
    function toUint72(uint256 value) internal pure returns (uint72) {
        if (value > type(uint72).max) {
            revert SafeCastOverflowedUintDowncast(72, value);
        }
        return uint72(value);
    }

    /**
     * @dev Returns the downcasted uint64 from uint256, reverting on
     * overflow (when the input is greater than largest uint64).
     *
     * Counterpart to Solidity's `uint64` operator.
     *
     * Requirements:
     *
     * - input must fit into 64 bits
     */
    function toUint64(uint256 value) internal pure returns (uint64) {
        if (value > type(uint64).max) {
            revert SafeCastOverflowedUintDowncast(64, value);
        }
        return uint64(value);
    }

    /**
     * @dev Returns the downcasted uint56 from uint256, reverting on
     * overflow (when the input is greater than largest uint56).
     *
     * Counterpart to Solidity's `uint56` operator.
     *
     * Requirements:
     *
     * - input must fit into 56 bits
     */
    function toUint56(uint256 value) internal pure returns (uint56) {
        if (value > type(uint56).max) {
            revert SafeCastOverflowedUintDowncast(56, value);
        }
        return uint56(value);
    }

    /**
     * @dev Returns the downcasted uint48 from uint256, reverting on
     * overflow (when the input is greater than largest uint48).
     *
     * Counterpart to Solidity's `uint48` operator.
     *
     * Requirements:
     *
     * - input must fit into 48 bits
     */
    function toUint48(uint256 value) internal pure returns (uint48) {
        if (value > type(uint48).max) {
            revert SafeCastOverflowedUintDowncast(48, value);
        }
        return uint48(value);
    }

    /**
     * @dev Returns the downcasted uint40 from uint256, reverting on
     * overflow (when the input is greater than largest uint40).
     *
     * Counterpart to Solidity's `uint40` operator.
     *
     * Requirements:
     *
     * - input must fit into 40 bits
     */
    function toUint40(uint256 value) internal pure returns (uint40) {
        if (value > type(uint40).max) {
            revert SafeCastOverflowedUintDowncast(40, value);
        }
        return uint40(value);
    }

    /**
     * @dev Returns the downcasted uint32 from uint256, reverting on
     * overflow (when the input is greater than largest uint32).
     *
     * Counterpart to Solidity's `uint32` operator.
     *
     * Requirements:
     *
     * - input must fit into 32 bits
     */
    function toUint32(uint256 value) internal pure returns (uint32) {
        if (value > type(uint32).max) {
            revert SafeCastOverflowedUintDowncast(32, value);
        }
        return uint32(value);
    }

    /**
     * @dev Returns the downcasted uint24 from uint256, reverting on
     * overflow (when the input is greater than largest uint24).
     *
     * Counterpart to Solidity's `uint24` operator.
     *
     * Requirements:
     *
     * - input must fit into 24 bits
     */
    function toUint24(uint256 value) internal pure returns (uint24) {
        if (value > type(uint24).max) {
            revert SafeCastOverflowedUintDowncast(24, value);
        }
        return uint24(value);
    }

    /**
     * @dev Returns the downcasted uint16 from uint256, reverting on
     * overflow (when the input is greater than largest uint16).
     *
     * Counterpart to Solidity's `uint16` operator.
     *
     * Requirements:
     *
     * - input must fit into 16 bits
     */
    function toUint16(uint256 value) internal pure returns (uint16) {
        if (value > type(uint16).max) {
            revert SafeCastOverflowedUintDowncast(16, value);
        }
        return uint16(value);
    }

    /**
     * @dev Returns the downcasted uint8 from uint256, reverting on
     * overflow (when the input is greater than largest uint8).
     *
     * Counterpart to Solidity's `uint8` operator.
     *
     * Requirements:
     *
     * - input must fit into 8 bits
     */
    function toUint8(uint256 value) internal pure returns (uint8) {
        if (value > type(uint8).max) {
            revert SafeCastOverflowedUintDowncast(8, value);
        }
        return uint8(value);
    }

    /**
     * @dev Converts a signed int256 into an unsigned uint256.
     *
     * Requirements:
     *
     * - input must be greater than or equal to 0.
     */
    function toUint256(int256 value) internal pure returns (uint256) {
        if (value < 0) {
            revert SafeCastOverflowedIntToUint(value);
        }
        return uint256(value);
    }

    /**
     * @dev Returns the downcasted int248 from int256, reverting on
     * overflow (when the input is less than smallest int248 or
     * greater than largest int248).
     *
     * Counterpart to Solidity's `int248` operator.
     *
     * Requirements:
     *
     * - input must fit into 248 bits
     */
    function toInt248(int256 value) internal pure returns (int248 downcasted) {
        downcasted = int248(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(248, value);
        }
    }

    /**
     * @dev Returns the downcasted int240 from int256, reverting on
     * overflow (when the input is less than smallest int240 or
     * greater than largest int240).
     *
     * Counterpart to Solidity's `int240` operator.
     *
     * Requirements:
     *
     * - input must fit into 240 bits
     */
    function toInt240(int256 value) internal pure returns (int240 downcasted) {
        downcasted = int240(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(240, value);
        }
    }

    /**
     * @dev Returns the downcasted int232 from int256, reverting on
     * overflow (when the input is less than smallest int232 or
     * greater than largest int232).
     *
     * Counterpart to Solidity's `int232` operator.
     *
     * Requirements:
     *
     * - input must fit into 232 bits
     */
    function toInt232(int256 value) internal pure returns (int232 downcasted) {
        downcasted = int232(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(232, value);
        }
    }

    /**
     * @dev Returns the downcasted int224 from int256, reverting on
     * overflow (when the input is less than smallest int224 or
     * greater than largest int224).
     *
     * Counterpart to Solidity's `int224` operator.
     *
     * Requirements:
     *
     * - input must fit into 224 bits
     */
    function toInt224(int256 value) internal pure returns (int224 downcasted) {
        downcasted = int224(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(224, value);
        }
    }

    /**
     * @dev Returns the downcasted int216 from int256, reverting on
     * overflow (when the input is less than smallest int216 or
     * greater than largest int216).
     *
     * Counterpart to Solidity's `int216` operator.
     *
     * Requirements:
     *
     * - input must fit into 216 bits
     */
    function toInt216(int256 value) internal pure returns (int216 downcasted) {
        downcasted = int216(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(216, value);
        }
    }

    /**
     * @dev Returns the downcasted int208 from int256, reverting on
     * overflow (when the input is less than smallest int208 or
     * greater than largest int208).
     *
     * Counterpart to Solidity's `int208` operator.
     *
     * Requirements:
     *
     * - input must fit into 208 bits
     */
    function toInt208(int256 value) internal pure returns (int208 downcasted) {
        downcasted = int208(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(208, value);
        }
    }

    /**
     * @dev Returns the downcasted int200 from int256, reverting on
     * overflow (when the input is less than smallest int200 or
     * greater than largest int200).
     *
     * Counterpart to Solidity's `int200` operator.
     *
     * Requirements:
     *
     * - input must fit into 200 bits
     */
    function toInt200(int256 value) internal pure returns (int200 downcasted) {
        downcasted = int200(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(200, value);
        }
    }

    /**
     * @dev Returns the downcasted int192 from int256, reverting on
     * overflow (when the input is less than smallest int192 or
     * greater than largest int192).
     *
     * Counterpart to Solidity's `int192` operator.
     *
     * Requirements:
     *
     * - input must fit into 192 bits
     */
    function toInt192(int256 value) internal pure returns (int192 downcasted) {
        downcasted = int192(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(192, value);
        }
    }

    /**
     * @dev Returns the downcasted int184 from int256, reverting on
     * overflow (when the input is less than smallest int184 or
     * greater than largest int184).
     *
     * Counterpart to Solidity's `int184` operator.
     *
     * Requirements:
     *
     * - input must fit into 184 bits
     */
    function toInt184(int256 value) internal pure returns (int184 downcasted) {
        downcasted = int184(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(184, value);
        }
    }

    /**
     * @dev Returns the downcasted int176 from int256, reverting on
     * overflow (when the input is less than smallest int176 or
     * greater than largest int176).
     *
     * Counterpart to Solidity's `int176` operator.
     *
     * Requirements:
     *
     * - input must fit into 176 bits
     */
    function toInt176(int256 value) internal pure returns (int176 downcasted) {
        downcasted = int176(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(176, value);
        }
    }

    /**
     * @dev Returns the downcasted int168 from int256, reverting on
     * overflow (when the input is less than smallest int168 or
     * greater than largest int168).
     *
     * Counterpart to Solidity's `int168` operator.
     *
     * Requirements:
     *
     * - input must fit into 168 bits
     */
    function toInt168(int256 value) internal pure returns (int168 downcasted) {
        downcasted = int168(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(168, value);
        }
    }

    /**
     * @dev Returns the downcasted int160 from int256, reverting on
     * overflow (when the input is less than smallest int160 or
     * greater than largest int160).
     *
     * Counterpart to Solidity's `int160` operator.
     *
     * Requirements:
     *
     * - input must fit into 160 bits
     */
    function toInt160(int256 value) internal pure returns (int160 downcasted) {
        downcasted = int160(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(160, value);
        }
    }

    /**
     * @dev Returns the downcasted int152 from int256, reverting on
     * overflow (when the input is less than smallest int152 or
     * greater than largest int152).
     *
     * Counterpart to Solidity's `int152` operator.
     *
     * Requirements:
     *
     * - input must fit into 152 bits
     */
    function toInt152(int256 value) internal pure returns (int152 downcasted) {
        downcasted = int152(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(152, value);
        }
    }

    /**
     * @dev Returns the downcasted int144 from int256, reverting on
     * overflow (when the input is less than smallest int144 or
     * greater than largest int144).
     *
     * Counterpart to Solidity's `int144` operator.
     *
     * Requirements:
     *
     * - input must fit into 144 bits
     */
    function toInt144(int256 value) internal pure returns (int144 downcasted) {
        downcasted = int144(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(144, value);
        }
    }

    /**
     * @dev Returns the downcasted int136 from int256, reverting on
     * overflow (when the input is less than smallest int136 or
     * greater than largest int136).
     *
     * Counterpart to Solidity's `int136` operator.
     *
     * Requirements:
     *
     * - input must fit into 136 bits
     */
    function toInt136(int256 value) internal pure returns (int136 downcasted) {
        downcasted = int136(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(136, value);
        }
    }

    /**
     * @dev Returns the downcasted int128 from int256, reverting on
     * overflow (when the input is less than smallest int128 or
     * greater than largest int128).
     *
     * Counterpart to Solidity's `int128` operator.
     *
     * Requirements:
     *
     * - input must fit into 128 bits
     */
    function toInt128(int256 value) internal pure returns (int128 downcasted) {
        downcasted = int128(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(128, value);
        }
    }

    /**
     * @dev Returns the downcasted int120 from int256, reverting on
     * overflow (when the input is less than smallest int120 or
     * greater than largest int120).
     *
     * Counterpart to Solidity's `int120` operator.
     *
     * Requirements:
     *
     * - input must fit into 120 bits
     */
    function toInt120(int256 value) internal pure returns (int120 downcasted) {
        downcasted = int120(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(120, value);
        }
    }

    /**
     * @dev Returns the downcasted int112 from int256, reverting on
     * overflow (when the input is less than smallest int112 or
     * greater than largest int112).
     *
     * Counterpart to Solidity's `int112` operator.
     *
     * Requirements:
     *
     * - input must fit into 112 bits
     */
    function toInt112(int256 value) internal pure returns (int112 downcasted) {
        downcasted = int112(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(112, value);
        }
    }

    /**
     * @dev Returns the downcasted int104 from int256, reverting on
     * overflow (when the input is less than smallest int104 or
     * greater than largest int104).
     *
     * Counterpart to Solidity's `int104` operator.
     *
     * Requirements:
     *
     * - input must fit into 104 bits
     */
    function toInt104(int256 value) internal pure returns (int104 downcasted) {
        downcasted = int104(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(104, value);
        }
    }

    /**
     * @dev Returns the downcasted int96 from int256, reverting on
     * overflow (when the input is less than smallest int96 or
     * greater than largest int96).
     *
     * Counterpart to Solidity's `int96` operator.
     *
     * Requirements:
     *
     * - input must fit into 96 bits
     */
    function toInt96(int256 value) internal pure returns (int96 downcasted) {
        downcasted = int96(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(96, value);
        }
    }

    /**
     * @dev Returns the downcasted int88 from int256, reverting on
     * overflow (when the input is less than smallest int88 or
     * greater than largest int88).
     *
     * Counterpart to Solidity's `int88` operator.
     *
     * Requirements:
     *
     * - input must fit into 88 bits
     */
    function toInt88(int256 value) internal pure returns (int88 downcasted) {
        downcasted = int88(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(88, value);
        }
    }

    /**
     * @dev Returns the downcasted int80 from int256, reverting on
     * overflow (when the input is less than smallest int80 or
     * greater than largest int80).
     *
     * Counterpart to Solidity's `int80` operator.
     *
     * Requirements:
     *
     * - input must fit into 80 bits
     */
    function toInt80(int256 value) internal pure returns (int80 downcasted) {
        downcasted = int80(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(80, value);
        }
    }

    /**
     * @dev Returns the downcasted int72 from int256, reverting on
     * overflow (when the input is less than smallest int72 or
     * greater than largest int72).
     *
     * Counterpart to Solidity's `int72` operator.
     *
     * Requirements:
     *
     * - input must fit into 72 bits
     */
    function toInt72(int256 value) internal pure returns (int72 downcasted) {
        downcasted = int72(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(72, value);
        }
    }

    /**
     * @dev Returns the downcasted int64 from int256, reverting on
     * overflow (when the input is less than smallest int64 or
     * greater than largest int64).
     *
     * Counterpart to Solidity's `int64` operator.
     *
     * Requirements:
     *
     * - input must fit into 64 bits
     */
    function toInt64(int256 value) internal pure returns (int64 downcasted) {
        downcasted = int64(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(64, value);
        }
    }

    /**
     * @dev Returns the downcasted int56 from int256, reverting on
     * overflow (when the input is less than smallest int56 or
     * greater than largest int56).
     *
     * Counterpart to Solidity's `int56` operator.
     *
     * Requirements:
     *
     * - input must fit into 56 bits
     */
    function toInt56(int256 value) internal pure returns (int56 downcasted) {
        downcasted = int56(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(56, value);
        }
    }

    /**
     * @dev Returns the downcasted int48 from int256, reverting on
     * overflow (when the input is less than smallest int48 or
     * greater than largest int48).
     *
     * Counterpart to Solidity's `int48` operator.
     *
     * Requirements:
     *
     * - input must fit into 48 bits
     */
    function toInt48(int256 value) internal pure returns (int48 downcasted) {
        downcasted = int48(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(48, value);
        }
    }

    /**
     * @dev Returns the downcasted int40 from int256, reverting on
     * overflow (when the input is less than smallest int40 or
     * greater than largest int40).
     *
     * Counterpart to Solidity's `int40` operator.
     *
     * Requirements:
     *
     * - input must fit into 40 bits
     */
    function toInt40(int256 value) internal pure returns (int40 downcasted) {
        downcasted = int40(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(40, value);
        }
    }

    /**
     * @dev Returns the downcasted int32 from int256, reverting on
     * overflow (when the input is less than smallest int32 or
     * greater than largest int32).
     *
     * Counterpart to Solidity's `int32` operator.
     *
     * Requirements:
     *
     * - input must fit into 32 bits
     */
    function toInt32(int256 value) internal pure returns (int32 downcasted) {
        downcasted = int32(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(32, value);
        }
    }

    /**
     * @dev Returns the downcasted int24 from int256, reverting on
     * overflow (when the input is less than smallest int24 or
     * greater than largest int24).
     *
     * Counterpart to Solidity's `int24` operator.
     *
     * Requirements:
     *
     * - input must fit into 24 bits
     */
    function toInt24(int256 value) internal pure returns (int24 downcasted) {
        downcasted = int24(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(24, value);
        }
    }

    /**
     * @dev Returns the downcasted int16 from int256, reverting on
     * overflow (when the input is less than smallest int16 or
     * greater than largest int16).
     *
     * Counterpart to Solidity's `int16` operator.
     *
     * Requirements:
     *
     * - input must fit into 16 bits
     */
    function toInt16(int256 value) internal pure returns (int16 downcasted) {
        downcasted = int16(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(16, value);
        }
    }

    /**
     * @dev Returns the downcasted int8 from int256, reverting on
     * overflow (when the input is less than smallest int8 or
     * greater than largest int8).
     *
     * Counterpart to Solidity's `int8` operator.
     *
     * Requirements:
     *
     * - input must fit into 8 bits
     */
    function toInt8(int256 value) internal pure returns (int8 downcasted) {
        downcasted = int8(value);
        if (downcasted != value) {
            revert SafeCastOverflowedIntDowncast(8, value);
        }
    }

    /**
     * @dev Converts an unsigned uint256 into a signed int256.
     *
     * Requirements:
     *
     * - input must be less than or equal to maxInt256.
     */
    function toInt256(uint256 value) internal pure returns (int256) {
        // Note: Unsafe cast below is okay because `type(int256).max` is guaranteed to be positive
        if (value > uint256(type(int256).max)) {
            revert SafeCastOverflowedUintToInt(value);
        }
        return int256(value);
    }

    /**
     * @dev Cast a boolean (false or true) to a uint256 (0 or 1) with no jump.
     */
    function toUint(bool b) internal pure returns (uint256 u) {
        assembly ("memory-safe") {
            u := iszero(iszero(b))
        }
    }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IPoolAddressesProvider} from '../../../interfaces/IPoolAddressesProvider.sol';
import {IPool} from '../../../interfaces/IPool.sol';

/**
 * @title IFlashLoanReceiver
 * @author Aave
 * @notice Defines the basic interface of a flashloan-receiver contract.
 * @dev Implement this interface to develop a flashloan-compatible flashLoanReceiver contract
 */
interface IFlashLoanReceiver {
  /**
   * @notice Executes an operation after receiving the flash-borrowed assets
   * @dev Ensure that the contract can return the debt + premium, e.g., has
   *      enough funds to repay and has approved the Pool to pull the total amount
   * @param assets The addresses of the flash-borrowed assets
   * @param amounts The amounts of the flash-borrowed assets
   * @param premiums The fee of each flash-borrowed asset
   * @param initiator The address of the flashloan initiator
   * @param params The byte-encoded params passed when initiating the flashloan
   * @return True if the execution of the operation succeeds, false otherwise
   */
  function executeOperation(
    address[] calldata assets,
    uint256[] calldata amounts,
    uint256[] calldata premiums,
    address initiator,
    bytes calldata params
  ) external returns (bool);

  function ADDRESSES_PROVIDER() external view returns (IPoolAddressesProvider);

  function POOL() external view returns (IPool);
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IPoolAddressesProvider} from '../../../interfaces/IPoolAddressesProvider.sol';
import {IPool} from '../../../interfaces/IPool.sol';

/**
 * @title IFlashLoanSimpleReceiver
 * @author Aave
 * @notice Defines the basic interface of a flashloan-receiver contract.
 * @dev Implement this interface to develop a flashloan-compatible flashLoanReceiver contract
 */
interface IFlashLoanSimpleReceiver {
  /**
   * @notice Executes an operation after receiving the flash-borrowed asset
   * @dev Ensure that the contract can return the debt + premium, e.g., has
   *      enough funds to repay and has approved the Pool to pull the total amount
   * @param asset The address of the flash-borrowed asset
   * @param amount The amount of the flash-borrowed asset
   * @param premium The fee of the flash-borrowed asset
   * @param initiator The address of the flashloan initiator
   * @param params The byte-encoded params passed when initiating the flashloan
   * @return True if the execution of the operation succeeds, false otherwise
   */
  function executeOperation(
    address asset,
    uint256 amount,
    uint256 premium,
    address initiator,
    bytes calldata params
  ) external returns (bool);

  function ADDRESSES_PROVIDER() external view returns (IPoolAddressesProvider);

  function POOL() external view returns (IPool);
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

/**
 * @title IPriceOracleGetter
 * @author Aave
 * @notice Interface for the Aave price oracle.
 */
interface IPriceOracleGetter {
  /**
   * @notice Returns the base currency address
   * @dev Address 0x0 is reserved for USD as base currency.
   * @return Returns the base currency address.
   */
  function BASE_CURRENCY() external view returns (address);

  /**
   * @notice Returns the base currency unit
   * @dev 1 ether for ETH, 1e8 for USD.
   * @return Returns the base currency unit.
   */
  function BASE_CURRENCY_UNIT() external view returns (uint256);

  /**
   * @notice Returns the asset price in the base currency
   * @param asset The address of the asset
   * @return The price of the asset
   */
  function getAssetPrice(address asset) external view returns (uint256);
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

/**
 * @title IScaledBalanceToken
 * @author Aave
 * @notice Defines the basic interface for a scaled-balance token.
 */
interface IScaledBalanceToken {
  /**
   * @dev Emitted after the mint action
   * @param caller The address performing the mint
   * @param onBehalfOf The address of the user that will receive the minted tokens
   * @param value The scaled-up amount being minted (based on user entered amount and balance increase from interest)
   * @param balanceIncrease The increase in scaled-up balance since the last action of 'onBehalfOf'
   * @param index The next liquidity index of the reserve
   */
  event Mint(
    address indexed caller,
    address indexed onBehalfOf,
    uint256 value,
    uint256 balanceIncrease,
    uint256 index
  );

  /**
   * @dev Emitted after the burn action
   * @dev If the burn function does not involve a transfer of the underlying asset, the target defaults to zero address
   * @param from The address from which the tokens will be burned
   * @param target The address that will receive the underlying, if any
   * @param value The scaled-up amount being burned (user entered amount - balance increase from interest)
   * @param balanceIncrease The increase in scaled-up balance since the last action of 'from'
   * @param index The next liquidity index of the reserve
   */
  event Burn(
    address indexed from,
    address indexed target,
    uint256 value,
    uint256 balanceIncrease,
    uint256 index
  );

  /**
   * @notice Returns the scaled balance of the user.
   * @dev The scaled balance is the sum of all the updated stored balance divided by the reserve's liquidity index
   * at the moment of the update
   * @param user The user whose balance is calculated
   * @return The scaled balance of the user
   */
  function scaledBalanceOf(address user) external view returns (uint256);

  /**
   * @notice Returns the scaled balance of the user and the scaled total supply.
   * @param user The address of the user
   * @return The scaled balance of the user
   * @return The scaled total supply
   */
  function getScaledUserBalanceAndSupply(address user) external view returns (uint256, uint256);

  /**
   * @notice Returns the scaled total supply of the scaled balance token. Represents sum(debt/index)
   * @return The scaled total supply
   */
  function scaledTotalSupply() external view returns (uint256);

  /**
   * @notice Returns last index interest was accrued to the user's balance
   * @param user The address of the user
   * @return The last index interest was accrued to the user's balance, expressed in ray
   */
  function getPreviousIndex(address user) external view returns (uint256);
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.10;

import {IERC20} from './IERC20.sol';

interface IERC20Detailed is IERC20 {
  function name() external view returns (string memory);

  function symbol() external view returns (string memory);

  function decimals() external view returns (uint8);
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.0;

import {Pool} from '../protocol/pool/Pool.sol';
import {IPoolAddressesProvider} from '../interfaces/IPoolAddressesProvider.sol';
import {IReserveInterestRateStrategy} from '../interfaces/IReserveInterestRateStrategy.sol';
import {Errors} from '../protocol/libraries/helpers/Errors.sol';

/**
 * @title Aave Pool Instance
 * @author BGD Labs
 * @notice Instance of the Pool for the Aave protocol
 */
contract PoolInstance is Pool {
  uint256 public constant POOL_REVISION = 9;

  constructor(
    IPoolAddressesProvider provider,
    IReserveInterestRateStrategy interestRateStrategy_
  ) Pool(provider, interestRateStrategy_) {}

  /**
   * @notice Initializes the Pool.
   * @dev Function is invoked by the proxy contract when the Pool contract is added to the
   * PoolAddressesProvider of the market.
   * @dev The passed PoolAddressesProvider is validated against the POOL.ADDRESSES_PROVIDER, to ensure the upgrade is done with correct intention.
   * @param provider The address of the PoolAddressesProvider
   */
  function initialize(IPoolAddressesProvider provider) external virtual override initializer {
    require(provider == ADDRESSES_PROVIDER, Errors.InvalidAddressesProvider());
  }

  function getRevision() internal pure virtual override returns (uint256) {
    return POOL_REVISION;
  }
}

// SPDX-License-Identifier: MIT
// OpenZeppelin Contracts (last updated v5.0.1) (utils/Multicall.sol)

pragma solidity ^0.8.20;

import {Address} from "./Address.sol";
import {Context} from "./Context.sol";

/**
 * @dev Provides a function to batch together multiple calls in a single external call.
 *
 * Consider any assumption about calldata validation performed by the sender may be violated if it's not especially
 * careful about sending transactions invoking {multicall}. For example, a relay address that filters function
 * selectors won't filter calls nested within a {multicall} operation.
 *
 * NOTE: Since 5.0.1 and 4.9.4, this contract identifies non-canonical contexts (i.e. `msg.sender` is not {_msgSender}).
 * If a non-canonical context is identified, the following self `delegatecall` appends the last bytes of `msg.data`
 * to the subcall. This makes it safe to use with {ERC2771Context}. Contexts that don't affect the resolution of
 * {_msgSender} are not propagated to subcalls.
 */
abstract contract Multicall is Context {
    /**
     * @dev Receives and executes a batch of function calls on this contract.
     * @custom:oz-upgrades-unsafe-allow-reachable delegatecall
     */
    function multicall(bytes[] calldata data) external virtual returns (bytes[] memory results) {
        bytes memory context = msg.sender == _msgSender()
            ? new bytes(0)
            : msg.data[msg.data.length - _contextSuffixLength():];

        results = new bytes[](data.length);
        for (uint256 i = 0; i < data.length; i++) {
            results[i] = Address.functionDelegateCall(address(this), bytes.concat(data[i], context));
        }
        return results;
    }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IPoolAddressesProvider} from './IPoolAddressesProvider.sol';

/**
 * @title IACLManager
 * @author Aave
 * @notice Defines the basic interface for the ACL Manager
 */
interface IACLManager {
  /**
   * @notice Returns the contract address of the PoolAddressesProvider
   * @return The address of the PoolAddressesProvider
   */
  function ADDRESSES_PROVIDER() external view returns (IPoolAddressesProvider);

  /**
   * @notice Returns the identifier of the PoolAdmin role
   * @return The id of the PoolAdmin role
   */
  function POOL_ADMIN_ROLE() external view returns (bytes32);

  /**
   * @notice Returns the identifier of the EmergencyAdmin role
   * @return The id of the EmergencyAdmin role
   */
  function EMERGENCY_ADMIN_ROLE() external view returns (bytes32);

  /**
   * @notice Returns the identifier of the RiskAdmin role
   * @return The id of the RiskAdmin role
   */
  function RISK_ADMIN_ROLE() external view returns (bytes32);

  /**
   * @notice Returns the identifier of the FlashBorrower role
   * @return The id of the FlashBorrower role
   */
  function FLASH_BORROWER_ROLE() external view returns (bytes32);

  /**
   * @notice Returns the identifier of the Bridge role
   * @return The id of the Bridge role
   */
  function BRIDGE_ROLE() external view returns (bytes32);

  /**
   * @notice Returns the identifier of the AssetListingAdmin role
   * @return The id of the AssetListingAdmin role
   */
  function ASSET_LISTING_ADMIN_ROLE() external view returns (bytes32);

  /**
   * @notice Set the role as admin of a specific role.
   * @dev By default the admin role for all roles is `DEFAULT_ADMIN_ROLE`.
   * @param role The role to be managed by the admin role
   * @param adminRole The admin role
   */
  function setRoleAdmin(bytes32 role, bytes32 adminRole) external;

  /**
   * @notice Adds a new admin as PoolAdmin
   * @param admin The address of the new admin
   */
  function addPoolAdmin(address admin) external;

  /**
   * @notice Removes an admin as PoolAdmin
   * @param admin The address of the admin to remove
   */
  function removePoolAdmin(address admin) external;

  /**
   * @notice Returns true if the address is PoolAdmin, false otherwise
   * @param admin The address to check
   * @return True if the given address is PoolAdmin, false otherwise
   */
  function isPoolAdmin(address admin) external view returns (bool);

  /**
   * @notice Adds a new admin as EmergencyAdmin
   * @param admin The address of the new admin
   */
  function addEmergencyAdmin(address admin) external;

  /**
   * @notice Removes an admin as EmergencyAdmin
   * @param admin The address of the admin to remove
   */
  function removeEmergencyAdmin(address admin) external;

  /**
   * @notice Returns true if the address is EmergencyAdmin, false otherwise
   * @param admin The address to check
   * @return True if the given address is EmergencyAdmin, false otherwise
   */
  function isEmergencyAdmin(address admin) external view returns (bool);

  /**
   * @notice Adds a new admin as RiskAdmin
   * @param admin The address of the new admin
   */
  function addRiskAdmin(address admin) external;

  /**
   * @notice Removes an admin as RiskAdmin
   * @param admin The address of the admin to remove
   */
  function removeRiskAdmin(address admin) external;

  /**
   * @notice Returns true if the address is RiskAdmin, false otherwise
   * @param admin The address to check
   * @return True if the given address is RiskAdmin, false otherwise
   */
  function isRiskAdmin(address admin) external view returns (bool);

  /**
   * @notice Adds a new address as FlashBorrower
   * @param borrower The address of the new FlashBorrower
   */
  function addFlashBorrower(address borrower) external;

  /**
   * @notice Removes an address as FlashBorrower
   * @param borrower The address of the FlashBorrower to remove
   */
  function removeFlashBorrower(address borrower) external;

  /**
   * @notice Returns true if the address is FlashBorrower, false otherwise
   * @param borrower The address to check
   * @return True if the given address is FlashBorrower, false otherwise
   */
  function isFlashBorrower(address borrower) external view returns (bool);

  /**
   * @notice Adds a new address as Bridge
   * @param bridge The address of the new Bridge
   */
  function addBridge(address bridge) external;

  /**
   * @notice Removes an address as Bridge
   * @param bridge The address of the bridge to remove
   */
  function removeBridge(address bridge) external;

  /**
   * @notice Returns true if the address is Bridge, false otherwise
   * @param bridge The address to check
   * @return True if the given address is Bridge, false otherwise
   */
  function isBridge(address bridge) external view returns (bool);

  /**
   * @notice Adds a new admin as AssetListingAdmin
   * @param admin The address of the new admin
   */
  function addAssetListingAdmin(address admin) external;

  /**
   * @notice Removes an admin as AssetListingAdmin
   * @param admin The address of the admin to remove
   */
  function removeAssetListingAdmin(address admin) external;

  /**
   * @notice Returns true if the address is AssetListingAdmin, false otherwise
   * @param admin The address to check
   * @return True if the given address is AssetListingAdmin, false otherwise
   */
  function isAssetListingAdmin(address admin) external view returns (bool);
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {DataTypes} from '../protocol/libraries/types/DataTypes.sol';

/**
 * @title IReserveInterestRateStrategy
 * @author BGD Labs
 * @notice Basic interface for any rate strategy used by the Aave protocol
 */
interface IReserveInterestRateStrategy {
  /**
   * @notice Sets interest rate data for an Aave rate strategy
   * @param reserve The reserve to update
   * @param rateData The abi encoded reserve interest rate data to apply to the given reserve
   *   Abstracted this way as rate strategies can be custom
   */
  function setInterestRateParams(address reserve, bytes calldata rateData) external;

  /**
   * @notice Calculates the interest rates depending on the reserve's state and configurations
   * @param params The parameters needed to calculate interest rates
   * @return liquidityRate The liquidity rate expressed in ray
   * @return variableBorrowRate The variable borrow rate expressed in ray
   */
  function calculateInterestRates(
    DataTypes.CalculateInterestRatesParams memory params
  ) external view returns (uint256, uint256);
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {Address} from '../../../dependencies/openzeppelin/contracts/Address.sol';
import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {IPriceOracleGetter} from '../../../interfaces/IPriceOracleGetter.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {IPriceOracleSentinel} from '../../../interfaces/IPriceOracleSentinel.sol';
import {IPoolAddressesProvider} from '../../../interfaces/IPoolAddressesProvider.sol';
import {IAccessControl} from '../../../dependencies/openzeppelin/contracts/IAccessControl.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {UserConfiguration} from '../configuration/UserConfiguration.sol';
import {EModeConfiguration} from '../configuration/EModeConfiguration.sol';
import {Errors} from '../helpers/Errors.sol';
import {TokenMath} from '../helpers/TokenMath.sol';
import {PercentageMath} from '../math/PercentageMath.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {ReserveLogic} from './ReserveLogic.sol';
import {GenericLogic} from './GenericLogic.sol';
import {SafeCast} from 'openzeppelin-contracts/contracts/utils/math/SafeCast.sol';
import {IncentivizedERC20} from '../../tokenization/base/IncentivizedERC20.sol';
import {MathUtils} from '../math/MathUtils.sol';

/**
 * @title ValidationLogic library
 * @author Aave
 * @notice Implements functions to validate the different actions of the protocol
 */
library ValidationLogic {
  using ReserveLogic for DataTypes.ReserveData;
  using TokenMath for uint256;
  using PercentageMath for uint256;
  using SafeCast for uint256;
  using GPv2SafeERC20 for IERC20;
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;
  using UserConfiguration for DataTypes.UserConfigurationMap;
  using Address for address;

  // Factor to apply to "only-variable-debt" liquidity rate to get threshold for rebalancing, expressed in bps
  // A value of 0.9e4 results in 90%
  uint256 public constant REBALANCE_UP_LIQUIDITY_RATE_THRESHOLD = 0.9e4;

  // Minimum health factor allowed under any circumstance
  // A value of 0.95e18 results in 0.95
  uint256 public constant MINIMUM_HEALTH_FACTOR_LIQUIDATION_THRESHOLD = 0.95e18;

  /**
   * @dev Minimum health factor to consider a user position healthy
   * A value of 1e18 results in 1
   */
  uint256 public constant HEALTH_FACTOR_LIQUIDATION_THRESHOLD = 1e18;

  /**
   * @dev Role identifier for the role allowed to supply isolated reserves as collateral
   */
  bytes32 public constant ISOLATED_COLLATERAL_SUPPLIER_ROLE =
    keccak256('ISOLATED_COLLATERAL_SUPPLIER');

  /**
   * @notice Validates a supply action.
   * @param reserveCache The cached data of the reserve
   * @param scaledAmount The scaledAmount to be supplied
   */
  function validateSupply(
    DataTypes.ReserveCache memory reserveCache,
    DataTypes.ReserveData storage reserve,
    uint256 scaledAmount,
    address onBehalfOf
  ) internal view {
    require(scaledAmount != 0, Errors.InvalidAmount());

    (bool isActive, bool isFrozen, , bool isPaused) = reserveCache.reserveConfiguration.getFlags();
    require(isActive, Errors.ReserveInactive());
    require(!isPaused, Errors.ReservePaused());
    require(!isFrozen, Errors.ReserveFrozen());
    require(onBehalfOf != reserveCache.aTokenAddress, Errors.SupplyToAToken());

    uint256 supplyCap = reserveCache.reserveConfiguration.getSupplyCap();
    require(
      supplyCap == 0 ||
        (
          (IAToken(reserveCache.aTokenAddress).scaledTotalSupply() +
            scaledAmount +
            uint256(reserve.accruedToTreasury)).getATokenBalance(reserveCache.nextLiquidityIndex)
        ) <=
        supplyCap * (10 ** reserveCache.reserveConfiguration.getDecimals()),
      Errors.SupplyCapExceeded()
    );
  }

  /**
   * @notice Validates a withdraw action.
   * @param reserveCache The cached data of the reserve
   * @param scaledAmount The scaled amount to be withdrawn
   * @param scaledUserBalance The scaled balance of the user
   */
  function validateWithdraw(
    DataTypes.ReserveCache memory reserveCache,
    uint256 scaledAmount,
    uint256 scaledUserBalance
  ) internal pure {
    require(scaledAmount != 0, Errors.InvalidAmount());
    require(scaledAmount <= scaledUserBalance, Errors.NotEnoughAvailableUserBalance());

    (bool isActive, , , bool isPaused) = reserveCache.reserveConfiguration.getFlags();
    require(isActive, Errors.ReserveInactive());
    require(!isPaused, Errors.ReservePaused());
  }

  struct ValidateBorrowLocalVars {
    uint256 amount;
    uint256 userDebtInBaseCurrency;
    uint256 availableLiquidity;
    uint256 totalDebt;
    uint256 reserveDecimals;
    uint256 borrowCap;
    uint256 amountInBaseCurrency;
    uint256 assetUnit;
    address siloedBorrowingAddress;
    bool isActive;
    bool isFrozen;
    bool isPaused;
    bool borrowingEnabled;
    bool siloedBorrowingEnabled;
  }

  /**
   * @notice Validates a borrow action.
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param params Additional params needed for the validation
   */
  function validateBorrow(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.ValidateBorrowParams memory params
  ) internal view {
    require(params.amountScaled != 0, Errors.InvalidAmount());

    ValidateBorrowLocalVars memory vars;
    vars.amount = params.amountScaled.getVTokenBalance(params.reserveCache.nextVariableBorrowIndex);

    (vars.isActive, vars.isFrozen, vars.borrowingEnabled, vars.isPaused) = params
      .reserveCache
      .reserveConfiguration
      .getFlags();

    require(vars.isActive, Errors.ReserveInactive());
    require(!vars.isPaused, Errors.ReservePaused());
    require(!vars.isFrozen, Errors.ReserveFrozen());
    require(vars.borrowingEnabled, Errors.BorrowingNotEnabled());
    require(
      IERC20(params.reserveCache.aTokenAddress).totalSupply() >= vars.amount,
      Errors.InvalidAmount()
    );

    require(
      params.priceOracleSentinel == address(0) ||
        IPriceOracleSentinel(params.priceOracleSentinel).isBorrowAllowed(),
      Errors.PriceOracleSentinelCheckFailed()
    );

    //validate interest rate mode
    require(
      params.interestRateMode == DataTypes.InterestRateMode.VARIABLE,
      Errors.InvalidInterestRateModeSelected()
    );

    vars.reserveDecimals = params.reserveCache.reserveConfiguration.getDecimals();
    vars.borrowCap = params.reserveCache.reserveConfiguration.getBorrowCap();
    unchecked {
      vars.assetUnit = 10 ** vars.reserveDecimals;
    }

    if (vars.borrowCap != 0) {
      vars.totalDebt = (params.reserveCache.currScaledVariableDebt + params.amountScaled)
        .getVTokenBalance(params.reserveCache.nextVariableBorrowIndex);

      unchecked {
        require(vars.totalDebt <= vars.borrowCap * vars.assetUnit, Errors.BorrowCapExceeded());
      }
    }

    if (params.userEModeCategory != 0) {
      require(
        EModeConfiguration.isReserveEnabledOnBitmap(
          eModeCategories[params.userEModeCategory].borrowableBitmap,
          reservesData[params.asset].id
        ),
        Errors.NotBorrowableInEMode()
      );
    }

    if (params.userConfig.isBorrowingAny()) {
      (vars.siloedBorrowingEnabled, vars.siloedBorrowingAddress) = params
        .userConfig
        .getSiloedBorrowingState(reservesData, reservesList);

      if (vars.siloedBorrowingEnabled) {
        require(vars.siloedBorrowingAddress == params.asset, Errors.SiloedBorrowingViolation());
      } else {
        require(
          !params.reserveCache.reserveConfiguration.getSiloedBorrowing(),
          Errors.SiloedBorrowingViolation()
        );
      }
    }
  }

  /**
   * @notice Validates a repay action.
   * @param user The user initiating the repayment
   * @param reserveCache The cached data of the reserve
   * @param amountSent The amount sent for the repayment. Can be an actual value or type(uint256).max
   * @param onBehalfOf The address of the user sender is repaying for
   * @param debtScaled The borrow scaled balance of the user
   */
  function validateRepay(
    address user,
    DataTypes.ReserveCache memory reserveCache,
    uint256 amountSent,
    DataTypes.InterestRateMode interestRateMode,
    address onBehalfOf,
    uint256 debtScaled
  ) internal pure {
    require(amountSent != 0, Errors.InvalidAmount());
    require(
      interestRateMode == DataTypes.InterestRateMode.VARIABLE,
      Errors.InvalidInterestRateModeSelected()
    );
    require(
      amountSent != type(uint256).max || user == onBehalfOf,
      Errors.NoExplicitAmountToRepayOnBehalf()
    );

    (bool isActive, , , bool isPaused) = reserveCache.reserveConfiguration.getFlags();
    require(isActive, Errors.ReserveInactive());
    require(!isPaused, Errors.ReservePaused());

    require(debtScaled != 0, Errors.NoDebtOfSelectedType());
  }

  /**
   * @notice Validates the action of setting an asset as collateral.
   * @param reserveConfig The config of the reserve
   */
  function validateSetUseReserveAsCollateral(
    DataTypes.ReserveConfigurationMap memory reserveConfig
  ) internal pure {
    (bool isActive, , , bool isPaused) = reserveConfig.getFlags();
    require(isActive, Errors.ReserveInactive());
    require(!isPaused, Errors.ReservePaused());
  }

  /**
   * @notice Validates a flashloan action.
   * @param reservesData The state of all the reserves
   * @param assets The assets being flash-borrowed
   * @param amounts The amounts for each asset being borrowed
   */
  function validateFlashloan(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    address[] memory assets,
    uint256[] memory amounts
  ) internal view {
    require(assets.length == amounts.length, Errors.InconsistentFlashloanParams());
    for (uint256 i = 0; i < assets.length; i++) {
      for (uint256 j = i + 1; j < assets.length; j++) {
        require(assets[i] != assets[j], Errors.InconsistentFlashloanParams());
      }
      validateFlashloanSimple(reservesData[assets[i]], amounts[i]);
    }
  }

  /**
   * @notice Validates a flashloan action.
   * @param reserve The state of the reserve
   */
  function validateFlashloanSimple(
    DataTypes.ReserveData storage reserve,
    uint256 amount
  ) internal view {
    DataTypes.ReserveConfigurationMap memory configuration = reserve.configuration;
    require(!configuration.getPaused(), Errors.ReservePaused());
    require(configuration.getActive(), Errors.ReserveInactive());
    require(configuration.getFlashLoanEnabled(), Errors.FlashloanDisabled());
    require(IERC20(reserve.aTokenAddress).totalSupply() >= amount, Errors.InvalidAmount());
  }

  struct ValidateLiquidationCallLocalVars {
    bool collateralReserveActive;
    bool collateralReservePaused;
    bool principalReserveActive;
    bool principalReservePaused;
    bool isCollateralEnabled;
  }

  /**
   * @notice Validates the liquidation action.
   * @param borrowerConfig The user configuration mapping
   * @param collateralReserve The reserve data of the collateral
   * @param debtReserve The reserve data of the debt
   * @param params Additional parameters needed for the validation
   */
  function validateLiquidationCall(
    DataTypes.UserConfigurationMap storage borrowerConfig,
    DataTypes.ReserveData storage collateralReserve,
    DataTypes.ReserveData storage debtReserve,
    DataTypes.ValidateLiquidationCallParams memory params
  ) internal view {
    ValidateLiquidationCallLocalVars memory vars;

    require(params.borrower != params.liquidator, Errors.SelfLiquidation());

    (vars.collateralReserveActive, , , vars.collateralReservePaused) = collateralReserve
      .configuration
      .getFlags();

    (vars.principalReserveActive, , , vars.principalReservePaused) = params
      .debtReserveCache
      .reserveConfiguration
      .getFlags();

    require(vars.collateralReserveActive && vars.principalReserveActive, Errors.ReserveInactive());
    require(!vars.collateralReservePaused && !vars.principalReservePaused, Errors.ReservePaused());

    require(
      params.priceOracleSentinel == address(0) ||
        params.healthFactor < MINIMUM_HEALTH_FACTOR_LIQUIDATION_THRESHOLD ||
        IPriceOracleSentinel(params.priceOracleSentinel).isLiquidationAllowed(),
      Errors.PriceOracleSentinelCheckFailed()
    );

    require(
      collateralReserve.liquidationGracePeriodUntil < uint40(block.timestamp) &&
        debtReserve.liquidationGracePeriodUntil < uint40(block.timestamp),
      Errors.LiquidationGraceSentinelCheckFailed()
    );

    require(
      params.healthFactor < HEALTH_FACTOR_LIQUIDATION_THRESHOLD,
      Errors.HealthFactorNotBelowThreshold()
    );

    vars.isCollateralEnabled =
      collateralReserve.configuration.getLiquidationThreshold() != 0 &&
      borrowerConfig.isUsingAsCollateral(collateralReserve.id);

    //if collateral isn't enabled as collateral by user, it cannot be liquidated
    require(vars.isCollateralEnabled, Errors.CollateralCannotBeLiquidated());
    require(params.totalDebt != 0, Errors.SpecifiedCurrencyNotBorrowedByUser());
  }

  /**
   * @notice Validates the health factor of a user.
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param userConfig The state of the user for the specific reserve
   * @param user The user to validate health factor of
   * @param userEModeCategory The users active efficiency mode category
   * @param oracle The price oracle
   */
  function validateHealthFactor(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.UserConfigurationMap memory userConfig,
    address user,
    uint8 userEModeCategory,
    address oracle
  ) internal view returns (uint256, bool) {
    (, , , , uint256 healthFactor, bool hasZeroLtvCollateral) = GenericLogic
      .calculateUserAccountData(
        reservesData,
        reservesList,
        eModeCategories,
        DataTypes.CalculateUserAccountDataParams({
          userConfig: userConfig,
          user: user,
          oracle: oracle,
          userEModeCategory: userEModeCategory
        })
      );

    require(
      healthFactor >= HEALTH_FACTOR_LIQUIDATION_THRESHOLD,
      Errors.HealthFactorLowerThanLiquidationThreshold()
    );

    return (healthFactor, hasZeroLtvCollateral);
  }

  /**
   * @notice Validates the health factor of a user and the ltv of the asset being borrowed.
   *         The ltv validation is a measure to prevent accidental borrowing close to liquidations.
   *         Sophisticated users can work around this validation in various ways.
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param userConfig The state of the user for the specific reserve
   * @param user The user from which the aTokens are being transferred
   * @param userEModeCategory The users active efficiency mode category
   * @param oracle The price oracle
   */
  function validateHFAndLtv(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.UserConfigurationMap memory userConfig,
    address user,
    uint8 userEModeCategory,
    address oracle
  ) internal view {
    (
      uint256 userCollateralInBaseCurrency,
      uint256 userDebtInBaseCurrency,
      uint256 currentLtv,
      ,
      uint256 healthFactor,

    ) = GenericLogic.calculateUserAccountData(
        reservesData,
        reservesList,
        eModeCategories,
        DataTypes.CalculateUserAccountDataParams({
          userConfig: userConfig,
          user: user,
          oracle: oracle,
          userEModeCategory: userEModeCategory
        })
      );

    require(currentLtv != 0, Errors.LtvValidationFailed());

    require(
      healthFactor >= HEALTH_FACTOR_LIQUIDATION_THRESHOLD,
      Errors.HealthFactorLowerThanLiquidationThreshold()
    );

    require(
      userCollateralInBaseCurrency >= userDebtInBaseCurrency.percentDivCeil(currentLtv),
      Errors.CollateralCannotCoverNewBorrow()
    );
  }

  /**
   * @notice Validates the health factor of a user and the ltvzero configuration for the asset being withdrawn/transferred or disabled as collateral.
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param userConfig The state of the user for the specific reserve
   * @param asset The asset for which the ltv will be validated
   * @param from The user from which the aTokens are being transferred
   * @param oracle The price oracle
   * @param userEModeCategory The users active efficiency mode category
   */
  function validateHFAndLtvzero(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.UserConfigurationMap memory userConfig,
    address asset,
    address from,
    address oracle,
    uint8 userEModeCategory
  ) internal view {
    (, bool hasZeroLtvCollateral) = validateHealthFactor(
      reservesData,
      reservesList,
      eModeCategories,
      userConfig,
      from,
      userEModeCategory,
      oracle
    );

    require(
      !hasZeroLtvCollateral || reservesData[asset].configuration.getLtv() == 0,
      Errors.LtvValidationFailed()
    );
  }

  /**
   * @notice Validates a transfer action.
   * @param reserve The reserve object
   */
  function validateTransfer(DataTypes.ReserveData storage reserve) internal view {
    require(!reserve.configuration.getPaused(), Errors.ReservePaused());
  }

  /**
   * @notice Validates a drop reserve action.
   * @param reservesList The addresses of all the active reserves
   * @param reserve The reserve object
   * @param asset The address of the reserve's underlying asset
   */
  function validateDropReserve(
    mapping(uint256 => address) storage reservesList,
    DataTypes.ReserveData storage reserve,
    address asset
  ) internal view {
    require(asset != address(0), Errors.ZeroAddressNotValid());
    require(reserve.id != 0 || reservesList[0] == asset, Errors.AssetNotListed());
    require(
      IERC20(reserve.variableDebtTokenAddress).totalSupply() == 0,
      Errors.VariableDebtSupplyNotZero()
    );
    require(
      IERC20(reserve.aTokenAddress).totalSupply() == 0 && reserve.accruedToTreasury == 0,
      Errors.UnderlyingClaimableRightsNotZero()
    );
  }

  /**
   * @notice Validates the action of setting efficiency mode.
   * @param eModeCategories a mapping storing configurations for all efficiency mode categories
   * @param userConfig the user configuration
   * @param categoryId The id of the category
   */
  function validateSetUserEMode(
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.UserConfigurationMap memory userConfig,
    uint8 categoryId
  ) internal view {
    DataTypes.EModeCategory storage eModeCategory = eModeCategories[categoryId];
    // category is invalid if the liq threshold is not set
    require(
      categoryId == 0 || eModeCategory.liquidationThreshold != 0,
      Errors.InconsistentEModeCategory()
    );

    // eMode can always be enabled if the user hasn't supplied anything
    if (userConfig.isEmpty()) {
      return;
    }

    // if user is trying to set another category than default we require that
    // either the user is not borrowing, or it's borrowing assets of categoryId
    if (categoryId != 0) {
      uint256 i = 0;
      bool isBorrowed = false;
      uint128 cachedBorrowableBitmap = eModeCategory.borrowableBitmap;
      uint256 cachedUserConfig = userConfig.data;
      unchecked {
        while (cachedUserConfig != 0) {
          (cachedUserConfig, isBorrowed, ) = UserConfiguration.getNextFlags(cachedUserConfig);

          if (isBorrowed) {
            require(
              EModeConfiguration.isReserveEnabledOnBitmap(cachedBorrowableBitmap, i),
              Errors.NotBorrowableInEMode()
            );
          }
          ++i;
        }
      }
    }
  }

  /**
   * @notice Validates the action of activating the asset as collateral.
   * @dev Only possible if the asset has non-zero LTV and the user is not in isolation mode
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param userConfig the user configuration
   * @param reserveConfig The reserve configuration
   * @return True if the asset can be activated as collateral, false otherwise
   */
  function validateUseAsCollateral(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    DataTypes.UserConfigurationMap storage userConfig,
    DataTypes.ReserveConfigurationMap memory reserveConfig
  ) internal view returns (bool) {
    if (reserveConfig.getLtv() == 0) {
      return false;
    }
    if (!userConfig.isUsingAsCollateralAny()) {
      return true;
    }
    (bool isolationModeActive, , ) = userConfig.getIsolationModeState(reservesData, reservesList);

    return (!isolationModeActive && reserveConfig.getDebtCeiling() == 0);
  }

  /**
   * @notice Validates if an asset should be automatically activated as collateral in the following actions: supply,
   * transfer, and liquidate
   * @dev This is used to ensure that isolated assets are not enabled as collateral automatically
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param userConfig the user configuration
   * @param reserveConfig The reserve configuration
   * @return True if the asset can be activated as collateral, false otherwise
   */
  function validateAutomaticUseAsCollateral(
    address sender,
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    DataTypes.UserConfigurationMap storage userConfig,
    DataTypes.ReserveConfigurationMap memory reserveConfig,
    address aTokenAddress
  ) internal view returns (bool) {
    if (reserveConfig.getDebtCeiling() != 0) {
      // ensures only the ISOLATED_COLLATERAL_SUPPLIER_ROLE can enable collateral as side-effect of an action
      IPoolAddressesProvider addressesProvider = IncentivizedERC20(aTokenAddress)
        .POOL()
        .ADDRESSES_PROVIDER();
      if (
        !IAccessControl(addressesProvider.getACLManager()).hasRole(
          ISOLATED_COLLATERAL_SUPPLIER_ROLE,
          sender
        )
      ) return false;
    }
    return validateUseAsCollateral(reservesData, reservesList, userConfig, reserveConfig);
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.10;

import {Context} from '../../../dependencies/openzeppelin/contracts/Context.sol';
import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {IERC20Detailed} from '../../../dependencies/openzeppelin/contracts/IERC20Detailed.sol';
import {SafeCast} from 'openzeppelin-contracts/contracts/utils/math/SafeCast.sol';
import {WadRayMath} from '../../libraries/math/WadRayMath.sol';
import {Errors} from '../../libraries/helpers/Errors.sol';
import {IAaveIncentivesController} from '../../../interfaces/IAaveIncentivesController.sol';
import {IPoolAddressesProvider} from '../../../interfaces/IPoolAddressesProvider.sol';
import {IPool} from '../../../interfaces/IPool.sol';
import {IACLManager} from '../../../interfaces/IACLManager.sol';
import {DelegationMode} from './DelegationMode.sol';

/**
 * @title IncentivizedERC20
 * @author Aave, inspired by the Openzeppelin ERC20 implementation
 * @notice Basic ERC20 implementation
 */
abstract contract IncentivizedERC20 is Context, IERC20Detailed {
  using WadRayMath for uint256;
  using SafeCast for uint256;

  /**
   * @dev Indicates a failure with the `spender`’s `allowance`. Used in transfers.
   * @param spender Address that may be allowed to operate on tokens without being their owner.
   * @param allowance Amount of tokens a `spender` is allowed to operate with.
   * @param needed Minimum amount required to perform a transfer.
   */
  error ERC20InsufficientAllowance(address spender, uint256 allowance, uint256 needed);

  /**
   * @dev Only pool admin can call functions marked by this modifier.
   */
  modifier onlyPoolAdmin() {
    IACLManager aclManager = IACLManager(_addressesProvider.getACLManager());
    require(aclManager.isPoolAdmin(_msgSender()), Errors.CallerNotPoolAdmin());
    _;
  }

  /**
   * @dev Only pool can call functions marked by this modifier.
   */
  modifier onlyPool() {
    require(_msgSender() == address(POOL), Errors.CallerMustBePool());
    _;
  }

  /**
   * @dev UserState - additionalData is a flexible field.
   * ATokens and VariableDebtTokens use this field store the index of the
   * user's last supply/withdrawal/borrow/repayment.
   */
  struct UserState {
    uint120 balance;
    DelegationMode delegationMode;
    uint128 additionalData;
  }
  // Map of users address and their state data (userAddress => userStateData)
  mapping(address => UserState) internal _userState;

  // Map of allowances (delegator => delegatee => allowanceAmount)
  mapping(address => mapping(address => uint256)) private _allowances;

  uint256 internal _totalSupply;
  string private _name;
  string private _symbol;
  uint8 private _decimals;
  // @dev deprecated on v3.4.0, replaced with immutable REWARDS_CONTROLLER
  IAaveIncentivesController internal __deprecated_incentivesController;
  IPoolAddressesProvider internal immutable _addressesProvider;
  IPool public immutable POOL;
  /**
   * @notice Returns the address of the Incentives Controller contract
   * @return The address of the Incentives Controller
   */
  IAaveIncentivesController public immutable REWARDS_CONTROLLER;

  /**
   * @dev Constructor.
   * @param pool The reference to the main Pool contract
   * @param name_ The name of the token
   * @param symbol_ The symbol of the token
   * @param decimals_ The number of decimals of the token
   * @param rewardsController The address of the rewards controller contract
   */
  constructor(
    IPool pool,
    string memory name_,
    string memory symbol_,
    uint8 decimals_,
    address rewardsController
  ) {
    _addressesProvider = pool.ADDRESSES_PROVIDER();
    _name = name_;
    _symbol = symbol_;
    _decimals = decimals_;
    POOL = pool;
    REWARDS_CONTROLLER = IAaveIncentivesController(rewardsController);
  }

  /// @inheritdoc IERC20Detailed
  function name() public view override returns (string memory) {
    return _name;
  }

  /// @inheritdoc IERC20Detailed
  function symbol() external view override returns (string memory) {
    return _symbol;
  }

  /// @inheritdoc IERC20Detailed
  function decimals() external view override returns (uint8) {
    return _decimals;
  }

  /// @inheritdoc IERC20
  function totalSupply() public view virtual override returns (uint256) {
    return _totalSupply;
  }

  /// @inheritdoc IERC20
  function balanceOf(address account) public view virtual override returns (uint256) {
    return _userState[account].balance;
  }

  /**
   * @notice Returns the address of the Incentives Controller contract
   * @return The address of the Incentives Controller
   */
  function getIncentivesController() external view virtual returns (IAaveIncentivesController) {
    return REWARDS_CONTROLLER;
  }

  /// @inheritdoc IERC20
  function transfer(address recipient, uint256 amount) external virtual override returns (bool) {
    uint120 castAmount = amount.toUint120();
    _transfer(_msgSender(), recipient, castAmount);
    return true;
  }

  /// @inheritdoc IERC20
  function allowance(
    address owner,
    address spender
  ) external view virtual override returns (uint256) {
    return _allowances[owner][spender];
  }

  /// @inheritdoc IERC20
  function approve(address spender, uint256 amount) external virtual override returns (bool) {
    _approve(_msgSender(), spender, amount);
    return true;
  }

  /// @inheritdoc IERC20
  function transferFrom(
    address sender,
    address recipient,
    uint256 amount
  ) external virtual override returns (bool) {
    uint120 castAmount = amount.toUint120();
    _approve(sender, _msgSender(), _allowances[sender][_msgSender()] - castAmount);
    _transfer(sender, recipient, castAmount);
    return true;
  }

  /**
   * @notice Increases the allowance of spender to spend _msgSender() tokens
   * @param spender The user allowed to spend on behalf of _msgSender()
   * @param addedValue The amount being added to the allowance
   * @return `true`
   */
  function increaseAllowance(address spender, uint256 addedValue) external virtual returns (bool) {
    _approve(_msgSender(), spender, _allowances[_msgSender()][spender] + addedValue);
    return true;
  }

  /**
   * @notice Decreases the allowance of spender to spend _msgSender() tokens
   * @param spender The user allowed to spend on behalf of _msgSender()
   * @param subtractedValue The amount being subtracted to the allowance
   * @return `true`
   */
  function decreaseAllowance(
    address spender,
    uint256 subtractedValue
  ) external virtual returns (bool) {
    _approve(_msgSender(), spender, _allowances[_msgSender()][spender] - subtractedValue);
    return true;
  }

  /**
   * @dev Updates `owner`'s allowance for `spender` based on spent `value`.
   *
   * Revert if not enough allowance is available.
   *
   * @param owner The owner of the tokens
   * @param spender The user allowed to spend on behalf of owner
   * @param amount The minimum amount being consumed from the allowance
   * @param correctedAmount The maximum amount being consumed from the allowance
   */
  function _spendAllowance(
    address owner,
    address spender,
    uint256 amount,
    uint256 correctedAmount
  ) internal virtual {
    uint256 currentAllowance = _allowances[owner][spender];
    if (currentAllowance < amount) {
      revert ERC20InsufficientAllowance(spender, currentAllowance, amount);
    }

    uint256 consumption = currentAllowance >= correctedAmount ? correctedAmount : currentAllowance;
    _approve(owner, spender, currentAllowance - consumption);
  }

  /**
   * @notice Transfers tokens between two users and apply incentives if defined.
   * @param sender The source address
   * @param recipient The destination address
   * @param amount The amount getting transferred
   */
  function _transfer(address sender, address recipient, uint120 amount) internal virtual {
    uint120 oldSenderBalance = _userState[sender].balance;
    _userState[sender].balance = oldSenderBalance - amount;
    uint120 oldRecipientBalance = _userState[recipient].balance;
    _userState[recipient].balance = oldRecipientBalance + amount;

    if (address(REWARDS_CONTROLLER) != address(0)) {
      uint256 currentTotalSupply = _totalSupply;
      REWARDS_CONTROLLER.handleAction(sender, currentTotalSupply, oldSenderBalance);
      if (sender != recipient) {
        REWARDS_CONTROLLER.handleAction(recipient, currentTotalSupply, oldRecipientBalance);
      }
    }
  }

  /**
   * @notice Approve `spender` to use `amount` of `owner`s balance
   * @param owner The address owning the tokens
   * @param spender The address approved for spending
   * @param amount The amount of tokens to approve spending of
   */
  function _approve(address owner, address spender, uint256 amount) internal virtual {
    _allowances[owner][spender] = amount;
    emit Approval(owner, spender, amount);
  }

  /**
   * @notice Update the name of the token
   * @param newName The new name for the token
   */
  function _setName(string memory newName) internal {
    _name = newName;
  }

  /**
   * @notice Update the symbol for the token
   * @param newSymbol The new symbol for the token
   */
  function _setSymbol(string memory newSymbol) internal {
    _symbol = newSymbol;
  }

  /**
   * @notice Update the number of decimals for the token
   * @param newDecimals The new number of decimals for the token
   */
  function _setDecimals(uint8 newDecimals) internal {
    _decimals = newDecimals;
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

/**
 * @title Errors library
 * @author Aave
 * @notice Defines the error messages emitted by the different contracts of the Aave protocol
 */
library Errors {
  error CallerNotPoolAdmin(); // 'The caller of the function is not a pool admin'
  error CallerNotPoolOrEmergencyAdmin(); // 'The caller of the function is not a pool or emergency admin'
  error CallerNotRiskOrPoolAdmin(); // 'The caller of the function is not a risk or pool admin'
  error CallerNotAssetListingOrPoolAdmin(); // 'The caller of the function is not an asset listing or pool admin'
  error AddressesProviderNotRegistered(); // 'Pool addresses provider is not registered'
  error InvalidAddressesProviderId(); // 'Invalid id for the pool addresses provider'
  error NotContract(); // 'Address is not a contract'
  error CallerNotPoolConfigurator(); // 'The caller of the function is not the pool configurator'
  error CallerNotAToken(); // 'The caller of the function is not an AToken'
  error InvalidAddressesProvider(); // 'The address of the pool addresses provider is invalid'
  error InvalidFlashloanExecutorReturn(); // 'Invalid return value of the flashloan executor function'
  error ReserveAlreadyAdded(); // 'Reserve has already been added to reserve list'
  error NoMoreReservesAllowed(); // 'Maximum amount of reserves in the pool reached'
  error EModeCategoryReserved(); // 'Zero eMode category is reserved for volatile heterogeneous assets'
  error ReserveLiquidityNotZero(); // 'The liquidity of the reserve needs to be 0'
  error FlashloanPremiumInvalid(); // 'Invalid flashloan premium'
  error InvalidReserveParams(); // 'Invalid risk parameters for the reserve'
  error InvalidEmodeCategoryParams(); // 'Invalid risk parameters for the eMode category'
  error CallerMustBePool(); // 'The caller of this function must be a pool'
  error InvalidMintAmount(); // 'Invalid amount to mint'
  error InvalidBurnAmount(); // 'Invalid amount to burn'
  error InvalidAmount(); // 'Amount must be greater than 0'
  error ReserveInactive(); // 'Action requires an active reserve'
  error ReserveFrozen(); // 'Action cannot be performed because the reserve is frozen'
  error ReservePaused(); // 'Action cannot be performed because the reserve is paused'
  error BorrowingNotEnabled(); // 'Borrowing is not enabled'
  error NotEnoughAvailableUserBalance(); // 'User cannot withdraw more than the available balance'
  error InvalidInterestRateModeSelected(); // 'Invalid interest rate mode selected'
  error HealthFactorLowerThanLiquidationThreshold(); // 'Health factor is below the liquidation threshold'
  error CollateralCannotCoverNewBorrow(); // 'There is not enough collateral to cover a new borrow'
  error NoDebtOfSelectedType(); // 'For repayment of a specific type of debt, the user needs to have debt that type'
  error NoExplicitAmountToRepayOnBehalf(); // 'To repay on behalf of a user an explicit amount to repay is needed'
  error UnderlyingBalanceZero(); // 'The underlying balance needs to be greater than 0'
  error HealthFactorNotBelowThreshold(); // 'Health factor is not below the threshold'
  error CollateralCannotBeLiquidated(); // 'The collateral chosen cannot be liquidated'
  error SpecifiedCurrencyNotBorrowedByUser(); // 'User did not borrow the specified currency'
  error InconsistentFlashloanParams(); // 'Inconsistent flashloan parameters'
  error BorrowCapExceeded(); // 'Borrow cap is exceeded'
  error SupplyCapExceeded(); // 'Supply cap is exceeded'
  error DebtCeilingExceeded(); // 'Debt ceiling is exceeded'
  error UnderlyingClaimableRightsNotZero(); // 'Claimable rights over underlying not zero (aToken supply or accruedToTreasury)'
  error VariableDebtSupplyNotZero(); // 'Variable debt supply is not zero'
  error LtvValidationFailed(); // 'Ltv validation failed'
  error InconsistentEModeCategory(); // 'Inconsistent eMode category'
  error PriceOracleSentinelCheckFailed(); // 'Price oracle sentinel validation failed'
  error AssetNotBorrowableInIsolation(); // 'Asset is not borrowable in isolation mode'
  error ReserveAlreadyInitialized(); // 'Reserve has already been initialized'
  error UserInIsolationModeOrLtvZero(); // 'User is in isolation mode or ltv is zero'
  error InvalidLtv(); // 'Invalid ltv parameter for the reserve'
  error InvalidLiquidationThreshold(); // 'Invalid liquidity threshold parameter for the reserve'
  error InvalidLiquidationBonus(); // 'Invalid liquidity bonus parameter for the reserve'
  error InvalidDecimals(); // 'Invalid decimals parameter of the underlying asset of the reserve'
  error InvalidReserveFactor(); // 'Invalid reserve factor parameter for the reserve'
  error InvalidBorrowCap(); // 'Invalid borrow cap for the reserve'
  error InvalidSupplyCap(); // 'Invalid supply cap for the reserve'
  error InvalidLiquidationProtocolFee(); // 'Invalid liquidation protocol fee for the reserve'
  error InvalidDebtCeiling(); // 'Invalid debt ceiling for the reserve'
  error InvalidReserveIndex(); // 'Invalid reserve index'
  error AclAdminCannotBeZero(); // 'ACL admin cannot be set to the zero address'
  error InconsistentParamsLength(); // 'Array parameters that should be equal length are not'
  error ZeroAddressNotValid(); // 'Zero address not valid'
  error InvalidExpiration(); // 'Invalid expiration'
  error InvalidSignature(); // 'Invalid signature'
  error OperationNotSupported(); // 'Operation not supported'
  error DebtCeilingNotZero(); // 'Debt ceiling is not zero'
  error AssetNotListed(); // 'Asset is not listed'
  error InvalidOptimalUsageRatio(); // 'Invalid optimal usage ratio'
  error UnderlyingCannotBeRescued(); // 'The underlying asset cannot be rescued'
  error AddressesProviderAlreadyAdded(); // 'Reserve has already been added to reserve list'
  error PoolAddressesDoNotMatch(); // 'The token implementation pool address and the pool address provided by the initializing pool do not match'
  error SiloedBorrowingViolation(); // 'User is trying to borrow multiple assets including a siloed one'
  error ReserveDebtNotZero(); // the total debt of the reserve needs to be 0
  error FlashloanDisabled(); // FlashLoaning for this asset is disabled
  error InvalidMaxRate(); // The expect maximum borrow rate is invalid
  error WithdrawToAToken(); // Withdrawing to the aToken is not allowed
  error SupplyToAToken(); // Supplying to the aToken is not allowed
  error Slope2MustBeGteSlope1(); // Variable interest rate slope 2 can not be lower than slope 1
  error CallerNotRiskOrPoolOrEmergencyAdmin(); // 'The caller of the function is not a risk, pool or emergency admin'
  error LiquidationGraceSentinelCheckFailed(); // 'Liquidation grace sentinel validation failed'
  error InvalidGracePeriod(); // Grace period above a valid range
  error InvalidFreezeState(); // Reserve is already in the passed freeze state
  error NotBorrowableInEMode(); // Asset not borrowable in eMode
  error CallerNotUmbrella(); // The caller of the function is not the umbrella contract
  error ReserveNotInDeficit(); // The reserve is not in deficit
  error MustNotLeaveDust(); // Below a certain threshold liquidators need to take the full position
  error UserCannotHaveDebt(); // Thrown when a user tries to interact with a method that requires a position without debt
  error SelfLiquidation(); // Thrown when a user tries to liquidate themselves
  error CallerNotPositionManager(); // Thrown when the caller has not been enabled as a position manager of the on-behalf-of user
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.10;

/**
 * @title VersionedInitializable
 * @author Aave, inspired by the OpenZeppelin Initializable contract
 * @notice Helper contract to implement initializer functions. To use it, replace
 * the constructor with a function that has the `initializer` modifier.
 * @dev WARNING: Unlike constructors, initializer functions must be manually
 * invoked. This applies both to deploying an Initializable contract, as well
 * as extending an Initializable contract via inheritance.
 * WARNING: When used with inheritance, manual care must be taken to not invoke
 * a parent initializer twice, or ensure that all initializers are idempotent,
 * because this is not dealt with automatically as with constructors.
 */
abstract contract VersionedInitializable {
  /**
   * @dev Initializes the implementation contract at the current revision.
   * In practice this breaks further initialization of the implementation.
   */
  constructor() {
    // break the initialize
    lastInitializedRevision = getRevision();
  }

  /**
   * @dev Indicates that the contract has been initialized.
   */
  uint256 private lastInitializedRevision = 0;

  /**
   * @dev Indicates that the contract is in the process of being initialized.
   */
  bool private initializing;

  /**
   * @dev Modifier to use in the initializer function of a contract.
   */
  modifier initializer() {
    uint256 revision = getRevision();
    require(
      initializing || isConstructor() || revision > lastInitializedRevision,
      'Contract instance has already been initialized'
    );

    bool isTopLevelCall = !initializing;
    if (isTopLevelCall) {
      initializing = true;
      lastInitializedRevision = revision;
    }

    _;

    if (isTopLevelCall) {
      initializing = false;
    }
  }

  /**
   * @notice Returns the revision number of the contract
   * @dev Needs to be defined in the inherited class as a constant.
   * @return The revision number
   */
  function getRevision() internal pure virtual returns (uint256);

  /**
   * @notice Returns true if and only if the function is running in the constructor
   * @return True if the function is running in the constructor
   */
  function isConstructor() private view returns (bool) {
    // extcodesize checks the size of the code stored in an address, and
    // address returns the current address. Since the code is still not
    // deployed when running a constructor, any checks on its code size will
    // yield zero, making it an effective way to detect if a contract is
    // under construction or not.
    uint256 cs;
    //solium-disable-next-line
    assembly {
      cs := extcodesize(address())
    }
    return cs == 0;
  }

  // Reserved storage space to allow for layout changes in the future.
  uint256[50] private ______gap;
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

library DataTypes {
  /**
   * This exists specifically to maintain the `getReserveData()` interface, since the new, internal
   * `ReserveData` struct includes the reserve's `virtualUnderlyingBalance`.
   */
  struct ReserveDataLegacy {
    //stores the reserve configuration
    ReserveConfigurationMap configuration;
    //the liquidity index. Expressed in ray
    uint128 liquidityIndex;
    //the current supply rate. Expressed in ray
    uint128 currentLiquidityRate;
    //variable borrow index. Expressed in ray
    uint128 variableBorrowIndex;
    //the current variable borrow rate. Expressed in ray
    uint128 currentVariableBorrowRate;
    // DEPRECATED on v3.2.0
    uint128 currentStableBorrowRate;
    //timestamp of last update
    uint40 lastUpdateTimestamp;
    //the id of the reserve. Represents the position in the list of the active reserves
    uint16 id;
    //aToken address
    address aTokenAddress;
    // DEPRECATED on v3.2.0
    address stableDebtTokenAddress;
    //variableDebtToken address
    address variableDebtTokenAddress;
    // DEPRECATED on v3.4.0, should use the `RESERVE_INTEREST_RATE_STRATEGY` variable from the Pool contract
    address interestRateStrategyAddress;
    //the current treasury balance, scaled
    uint128 accruedToTreasury;
    // DEPRECATED on v3.4.0
    uint128 unbacked;
    //the outstanding debt borrowed against this asset in isolation mode
    uint128 isolationModeTotalDebt;
  }

  struct ReserveData {
    //stores the reserve configuration
    ReserveConfigurationMap configuration;
    //the liquidity index. Expressed in ray
    uint128 liquidityIndex;
    //the current supply rate. Expressed in ray
    uint128 currentLiquidityRate;
    //variable borrow index. Expressed in ray
    uint128 variableBorrowIndex;
    //the current variable borrow rate. Expressed in ray
    uint128 currentVariableBorrowRate;
    /// @notice reused `__deprecatedStableBorrowRate` storage from pre 3.2
    // the current accumulate deficit in underlying tokens
    uint128 deficit;
    //timestamp of last update
    uint40 lastUpdateTimestamp;
    //the id of the reserve. Represents the position in the list of the active reserves
    uint16 id;
    //timestamp until when liquidations are not allowed on the reserve, if set to past liquidations will be allowed
    uint40 liquidationGracePeriodUntil;
    //aToken address
    address aTokenAddress;
    // DEPRECATED on v3.2.0
    address __deprecatedStableDebtTokenAddress;
    //variableDebtToken address
    address variableDebtTokenAddress;
    // DEPRECATED on v3.4.0, should use the `RESERVE_INTEREST_RATE_STRATEGY` variable from the Pool contract
    address __deprecatedInterestRateStrategyAddress;
    //the current treasury balance, scaled
    uint128 accruedToTreasury;
    // In aave 3.3.0 this storage slot contained the `unbacked`
    uint128 virtualUnderlyingBalance;
    //the outstanding debt borrowed against this asset in isolation mode
    uint128 isolationModeTotalDebt;
    //the amount of underlying accounted for by the protocol
    // DEPRECATED on v3.4.0. Moved into the same slot as accruedToTreasury for optimized storage access.
    uint128 __deprecatedVirtualUnderlyingBalance;
  }

  struct ReserveConfigurationMap {
    //bit 0-15: LTV
    //bit 16-31: Liq. threshold
    //bit 32-47: Liq. bonus
    //bit 48-55: Decimals
    //bit 56: reserve is active
    //bit 57: reserve is frozen
    //bit 58: borrowing is enabled
    //bit 59: DEPRECATED: stable rate borrowing enabled
    //bit 60: asset is paused
    //bit 61: borrowing in isolation mode is enabled
    //bit 62: siloed borrowing enabled
    //bit 63: flashloaning enabled
    //bit 64-79: reserve factor
    //bit 80-115: borrow cap in whole tokens, borrowCap == 0 => no cap
    //bit 116-151: supply cap in whole tokens, supplyCap == 0 => no cap
    //bit 152-167: liquidation protocol fee
    //bit 168-175: DEPRECATED: eMode category
    //bit 176-211: DEPRECATED: unbacked mint cap
    //bit 212-251: debt ceiling for isolation mode with (ReserveConfiguration::DEBT_CEILING_DECIMALS) decimals
    //bit 252: DEPRECATED: virtual accounting is enabled for the reserve
    //bit 253-255 unused

    uint256 data;
  }

  struct UserConfigurationMap {
    /**
     * @dev Bitmap of the users collaterals and borrows. It is divided in pairs of bits, one pair per asset.
     * The first bit indicates if an asset is used as collateral by the user, the second whether an
     * asset is borrowed by the user.
     */
    uint256 data;
  }

  // DEPRECATED: kept for backwards compatibility, might be removed in a future version
  struct EModeCategoryLegacy {
    // each eMode category has a custom ltv and liquidation threshold
    uint16 ltv;
    uint16 liquidationThreshold;
    uint16 liquidationBonus;
    // DEPRECATED
    address priceSource;
    string label;
  }

  struct CollateralConfig {
    uint16 ltv;
    uint16 liquidationThreshold;
    uint16 liquidationBonus;
  }

  struct EModeCategoryBaseConfiguration {
    uint16 ltv;
    uint16 liquidationThreshold;
    uint16 liquidationBonus;
    string label;
  }

  struct EModeCategory {
    // each eMode category has a custom ltv and liquidation threshold
    uint16 ltv;
    uint16 liquidationThreshold;
    uint16 liquidationBonus;
    uint128 collateralBitmap;
    string label;
    uint128 borrowableBitmap;
  }

  enum InterestRateMode {
    NONE,
    __DEPRECATED,
    VARIABLE
  }

  struct ReserveCache {
    uint256 currScaledVariableDebt;
    uint256 nextScaledVariableDebt;
    uint256 currLiquidityIndex;
    uint256 nextLiquidityIndex;
    uint256 currVariableBorrowIndex;
    uint256 nextVariableBorrowIndex;
    uint256 currLiquidityRate;
    uint256 currVariableBorrowRate;
    uint256 reserveFactor;
    ReserveConfigurationMap reserveConfiguration;
    address aTokenAddress;
    address variableDebtTokenAddress;
    uint40 reserveLastUpdateTimestamp;
  }

  struct ExecuteLiquidationCallParams {
    address liquidator;
    uint256 debtToCover;
    address collateralAsset;
    address debtAsset;
    address borrower;
    bool receiveAToken;
    address priceOracle;
    uint8 borrowerEModeCategory;
    address priceOracleSentinel;
    address interestRateStrategyAddress;
  }

  struct ExecuteSupplyParams {
    address user;
    address asset;
    address interestRateStrategyAddress;
    uint256 amount;
    address onBehalfOf;
    uint16 referralCode;
  }

  struct ExecuteBorrowParams {
    address asset;
    address user;
    address onBehalfOf;
    address interestRateStrategyAddress;
    uint256 amount;
    InterestRateMode interestRateMode;
    uint16 referralCode;
    bool releaseUnderlying;
    address oracle;
    uint8 userEModeCategory;
    address priceOracleSentinel;
  }

  struct ExecuteRepayParams {
    address asset;
    address user;
    address interestRateStrategyAddress;
    uint256 amount;
    InterestRateMode interestRateMode;
    address onBehalfOf;
    bool useATokens;
    address oracle;
    uint8 userEModeCategory;
  }

  struct ExecuteWithdrawParams {
    address user;
    address asset;
    address interestRateStrategyAddress;
    uint256 amount;
    address to;
    address oracle;
    uint8 userEModeCategory;
  }

  struct ExecuteEliminateDeficitParams {
    address user;
    address asset;
    address interestRateStrategyAddress;
    uint256 amount;
  }

  struct FinalizeTransferParams {
    address asset;
    address from;
    address to;
    uint256 scaledAmount;
    uint256 scaledBalanceFromBefore;
    uint256 scaledBalanceToBefore;
    address oracle;
    uint8 fromEModeCategory;
  }

  struct FlashloanParams {
    address user;
    address receiverAddress;
    address[] assets;
    uint256[] amounts;
    uint256[] interestRateModes;
    address interestRateStrategyAddress;
    address onBehalfOf;
    bytes params;
    uint16 referralCode;
    uint256 flashLoanPremium;
    address addressesProvider;
    address pool;
    uint8 userEModeCategory;
    bool isAuthorizedFlashBorrower;
  }

  struct FlashloanSimpleParams {
    address user;
    address receiverAddress;
    address asset;
    address interestRateStrategyAddress;
    uint256 amount;
    bytes params;
    uint16 referralCode;
    uint256 flashLoanPremium;
  }

  struct FlashLoanRepaymentParams {
    address user;
    uint256 amount;
    uint256 totalPremium;
    address asset;
    address interestRateStrategyAddress;
    address receiverAddress;
    uint16 referralCode;
  }

  struct CalculateUserAccountDataParams {
    UserConfigurationMap userConfig;
    address user;
    address oracle;
    uint8 userEModeCategory;
  }

  struct ValidateBorrowParams {
    ReserveCache reserveCache;
    UserConfigurationMap userConfig;
    address asset;
    address userAddress;
    uint256 amountScaled;
    InterestRateMode interestRateMode;
    address oracle;
    uint8 userEModeCategory;
    address priceOracleSentinel;
  }

  struct ValidateLiquidationCallParams {
    ReserveCache debtReserveCache;
    uint256 totalDebt;
    uint256 healthFactor;
    address priceOracleSentinel;
    address borrower;
    address liquidator;
  }

  struct CalculateInterestRatesParams {
    uint256 unbacked;
    uint256 liquidityAdded;
    uint256 liquidityTaken;
    uint256 totalDebt;
    uint256 reserveFactor;
    address reserve;
    // @notice DEPRECATED in 3.4, but kept for backwards compatibility
    bool usingVirtualBalance;
    uint256 virtualUnderlyingBalance;
  }

  struct InitReserveParams {
    address asset;
    address aTokenAddress;
    address variableDebtAddress;
    uint16 reservesCount;
    uint16 maxNumberReserves;
  }
}

// SPDX-License-Identifier: MIT
// OpenZeppelin Contracts v4.4.1 (utils/Address.sol)

pragma solidity ^0.8.0;

/**
 * @dev Collection of functions related to the address type
 */
library Address {
  /**
   * @dev Returns true if `account` is a contract.
   *
   * [IMPORTANT]
   * ====
   * It is unsafe to assume that an address for which this function returns
   * false is an externally-owned account (EOA) and not a contract.
   *
   * Among others, `isContract` will return false for the following
   * types of addresses:
   *
   *  - an externally-owned account
   *  - a contract in construction
   *  - an address where a contract will be created
   *  - an address where a contract lived, but was destroyed
   * ====
   */
  function isContract(address account) internal view returns (bool) {
    // This method relies on extcodesize, which returns 0 for contracts in
    // construction, since the code is only stored at the end of the
    // constructor execution.

    uint256 size;
    assembly {
      size := extcodesize(account)
    }
    return size > 0;
  }

  /**
   * @dev Replacement for Solidity's `transfer`: sends `amount` wei to
   * `recipient`, forwarding all available gas and reverting on errors.
   *
   * https://eips.ethereum.org/EIPS/eip-1884[EIP1884] increases the gas cost
   * of certain opcodes, possibly making contracts go over the 2300 gas limit
   * imposed by `transfer`, making them unable to receive funds via
   * `transfer`. {sendValue} removes this limitation.
   *
   * https://diligence.consensys.net/posts/2019/09/stop-using-soliditys-transfer-now/[Learn more].
   *
   * IMPORTANT: because control is transferred to `recipient`, care must be
   * taken to not create reentrancy vulnerabilities. Consider using
   * {ReentrancyGuard} or the
   * https://solidity.readthedocs.io/en/v0.5.11/security-considerations.html#use-the-checks-effects-interactions-pattern[checks-effects-interactions pattern].
   */
  function sendValue(address payable recipient, uint256 amount) internal {
    require(address(this).balance >= amount, 'Address: insufficient balance');

    (bool success, ) = recipient.call{value: amount}('');
    require(success, 'Address: unable to send value, recipient may have reverted');
  }

  /**
   * @dev Performs a Solidity function call using a low level `call`. A
   * plain `call` is an unsafe replacement for a function call: use this
   * function instead.
   *
   * If `target` reverts with a revert reason, it is bubbled up by this
   * function (like regular Solidity function calls).
   *
   * Returns the raw returned data. To convert to the expected return value,
   * use https://solidity.readthedocs.io/en/latest/units-and-global-variables.html?highlight=abi.decode#abi-encoding-and-decoding-functions[`abi.decode`].
   *
   * Requirements:
   *
   * - `target` must be a contract.
   * - calling `target` with `data` must not revert.
   *
   * _Available since v3.1._
   */
  function functionCall(address target, bytes memory data) internal returns (bytes memory) {
    return functionCall(target, data, 'Address: low-level call failed');
  }

  /**
   * @dev Same as {xref-Address-functionCall-address-bytes-}[`functionCall`], but with
   * `errorMessage` as a fallback revert reason when `target` reverts.
   *
   * _Available since v3.1._
   */
  function functionCall(
    address target,
    bytes memory data,
    string memory errorMessage
  ) internal returns (bytes memory) {
    return functionCallWithValue(target, data, 0, errorMessage);
  }

  /**
   * @dev Same as {xref-Address-functionCall-address-bytes-}[`functionCall`],
   * but also transferring `value` wei to `target`.
   *
   * Requirements:
   *
   * - the calling contract must have an ETH balance of at least `value`.
   * - the called Solidity function must be `payable`.
   *
   * _Available since v3.1._
   */
  function functionCallWithValue(
    address target,
    bytes memory data,
    uint256 value
  ) internal returns (bytes memory) {
    return functionCallWithValue(target, data, value, 'Address: low-level call with value failed');
  }

  /**
   * @dev Same as {xref-Address-functionCallWithValue-address-bytes-uint256-}[`functionCallWithValue`], but
   * with `errorMessage` as a fallback revert reason when `target` reverts.
   *
   * _Available since v3.1._
   */
  function functionCallWithValue(
    address target,
    bytes memory data,
    uint256 value,
    string memory errorMessage
  ) internal returns (bytes memory) {
    require(address(this).balance >= value, 'Address: insufficient balance for call');
    require(isContract(target), 'Address: call to non-contract');

    (bool success, bytes memory returndata) = target.call{value: value}(data);
    return verifyCallResult(success, returndata, errorMessage);
  }

  /**
   * @dev Same as {xref-Address-functionCall-address-bytes-}[`functionCall`],
   * but performing a static call.
   *
   * _Available since v3.3._
   */
  function functionStaticCall(
    address target,
    bytes memory data
  ) internal view returns (bytes memory) {
    return functionStaticCall(target, data, 'Address: low-level static call failed');
  }

  /**
   * @dev Same as {xref-Address-functionCall-address-bytes-string-}[`functionCall`],
   * but performing a static call.
   *
   * _Available since v3.3._
   */
  function functionStaticCall(
    address target,
    bytes memory data,
    string memory errorMessage
  ) internal view returns (bytes memory) {
    require(isContract(target), 'Address: static call to non-contract');

    (bool success, bytes memory returndata) = target.staticcall(data);
    return verifyCallResult(success, returndata, errorMessage);
  }

  /**
   * @dev Same as {xref-Address-functionCall-address-bytes-}[`functionCall`],
   * but performing a delegate call.
   *
   * _Available since v3.4._
   */
  function functionDelegateCall(address target, bytes memory data) internal returns (bytes memory) {
    return functionDelegateCall(target, data, 'Address: low-level delegate call failed');
  }

  /**
   * @dev Same as {xref-Address-functionCall-address-bytes-string-}[`functionCall`],
   * but performing a delegate call.
   *
   * _Available since v3.4._
   */
  function functionDelegateCall(
    address target,
    bytes memory data,
    string memory errorMessage
  ) internal returns (bytes memory) {
    require(isContract(target), 'Address: delegate call to non-contract');

    (bool success, bytes memory returndata) = target.delegatecall(data);
    return verifyCallResult(success, returndata, errorMessage);
  }

  /**
   * @dev Tool to verifies that a low level call was successful, and revert if it wasn't, either by bubbling the
   * revert reason using the provided one.
   *
   * _Available since v4.3._
   */
  function verifyCallResult(
    bool success,
    bytes memory returndata,
    string memory errorMessage
  ) internal pure returns (bytes memory) {
    if (success) {
      return returndata;
    } else {
      // Look for revert reason and bubble it up if present
      if (returndata.length > 0) {
        // The easiest way to bubble the revert reason is using memory via assembly

        assembly {
          let returndata_size := mload(returndata)
          revert(add(32, returndata), returndata_size)
        }
      } else {
        revert(errorMessage);
      }
    }
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IPoolAddressesProvider} from './IPoolAddressesProvider.sol';
import {DataTypes} from '../protocol/libraries/types/DataTypes.sol';

/**
 * @title IPool
 * @author Aave
 * @notice Defines the basic interface for an Aave Pool.
 */
interface IPool {
  /**
   * @dev Emitted on supply()
   * @param reserve The address of the underlying asset of the reserve
   * @param user The address initiating the supply
   * @param onBehalfOf The beneficiary of the supply, receiving the aTokens
   * @param amount The amount supplied
   * @param referralCode The referral code used
   */
  event Supply(
    address indexed reserve,
    address user,
    address indexed onBehalfOf,
    uint256 amount,
    uint16 indexed referralCode
  );

  /**
   * @dev Emitted on withdraw()
   * @param reserve The address of the underlying asset being withdrawn
   * @param user The address initiating the withdrawal, owner of aTokens
   * @param to The address that will receive the underlying
   * @param amount The amount to be withdrawn
   */
  event Withdraw(address indexed reserve, address indexed user, address indexed to, uint256 amount);

  /**
   * @dev Emitted on borrow() and flashLoan() when debt needs to be opened
   * @param reserve The address of the underlying asset being borrowed
   * @param user The address of the user initiating the borrow(), receiving the funds on borrow() or just
   * initiator of the transaction on flashLoan()
   * @param onBehalfOf The address that will be getting the debt
   * @param amount The amount borrowed out
   * @param interestRateMode The rate mode: 2 for Variable, 1 is deprecated (changed on v3.2.0)
   * @param borrowRate The numeric rate at which the user has borrowed, expressed in ray
   * @param referralCode The referral code used
   */
  event Borrow(
    address indexed reserve,
    address user,
    address indexed onBehalfOf,
    uint256 amount,
    DataTypes.InterestRateMode interestRateMode,
    uint256 borrowRate,
    uint16 indexed referralCode
  );

  /**
   * @dev Emitted on repay()
   * @param reserve The address of the underlying asset of the reserve
   * @param user The beneficiary of the repayment, getting his debt reduced
   * @param repayer The address of the user initiating the repay(), providing the funds
   * @param amount The amount repaid
   * @param useATokens True if the repayment is done using aTokens, `false` if done with underlying asset directly
   */
  event Repay(
    address indexed reserve,
    address indexed user,
    address indexed repayer,
    uint256 amount,
    bool useATokens
  );

  /**
   * @dev Emitted on borrow(), repay() and liquidationCall() when using isolated assets
   * @param asset The address of the underlying asset of the reserve
   * @param totalDebt The total isolation mode debt for the reserve
   */
  event IsolationModeTotalDebtUpdated(address indexed asset, uint256 totalDebt);

  /**
   * @dev Emitted when the user selects a certain asset category for eMode
   * @param user The address of the user
   * @param categoryId The category id
   */
  event UserEModeSet(address indexed user, uint8 categoryId);

  /**
   * @dev Emitted on setUserUseReserveAsCollateral()
   * @param reserve The address of the underlying asset of the reserve
   * @param user The address of the user enabling the usage as collateral
   */
  event ReserveUsedAsCollateralEnabled(address indexed reserve, address indexed user);

  /**
   * @dev Emitted on setUserUseReserveAsCollateral()
   * @param reserve The address of the underlying asset of the reserve
   * @param user The address of the user enabling the usage as collateral
   */
  event ReserveUsedAsCollateralDisabled(address indexed reserve, address indexed user);

  /**
   * @dev Emitted on flashLoan()
   * @param target The address of the flash loan receiver contract
   * @param initiator The address initiating the flash loan
   * @param asset The address of the asset being flash borrowed
   * @param amount The amount flash borrowed
   * @param interestRateMode The flashloan mode: 0 for regular flashloan,
   *        1 for Stable (Deprecated on v3.2.0), 2 for Variable
   * @param premium The fee flash borrowed
   * @param referralCode The referral code used
   */
  event FlashLoan(
    address indexed target,
    address initiator,
    address indexed asset,
    uint256 amount,
    DataTypes.InterestRateMode interestRateMode,
    uint256 premium,
    uint16 indexed referralCode
  );

  /**
   * @dev Emitted when a borrower is liquidated.
   * @param collateralAsset The address of the underlying asset used as collateral, to receive as result of the liquidation
   * @param debtAsset The address of the underlying borrowed asset to be repaid with the liquidation
   * @param user The address of the borrower getting liquidated
   * @param debtToCover The debt amount of borrowed `asset` the liquidator wants to cover
   * @param liquidatedCollateralAmount The amount of collateral received by the liquidator
   * @param liquidator The address of the liquidator
   * @param receiveAToken True if the liquidators wants to receive the collateral aTokens, `false` if he wants
   * to receive the underlying collateral asset directly
   */
  event LiquidationCall(
    address indexed collateralAsset,
    address indexed debtAsset,
    address indexed user,
    uint256 debtToCover,
    uint256 liquidatedCollateralAmount,
    address liquidator,
    bool receiveAToken
  );

  /**
   * @dev Emitted when the state of a reserve is updated.
   * @param reserve The address of the underlying asset of the reserve
   * @param liquidityRate The next liquidity rate
   * @param stableBorrowRate The next stable borrow rate @note deprecated on v3.2.0
   * @param variableBorrowRate The next variable borrow rate
   * @param liquidityIndex The next liquidity index
   * @param variableBorrowIndex The next variable borrow index
   */
  event ReserveDataUpdated(
    address indexed reserve,
    uint256 liquidityRate,
    uint256 stableBorrowRate,
    uint256 variableBorrowRate,
    uint256 liquidityIndex,
    uint256 variableBorrowIndex
  );

  /**
   * @dev Emitted when the deficit of a reserve is covered.
   * @param reserve The address of the underlying asset of the reserve
   * @param caller The caller that triggered the DeficitCovered event
   * @param amountCovered The amount of deficit covered
   */
  event DeficitCovered(address indexed reserve, address caller, uint256 amountCovered);

  /**
   * @dev Emitted when the protocol treasury receives minted aTokens from the accrued interest.
   * @param reserve The address of the reserve
   * @param amountMinted The amount minted to the treasury
   */
  event MintedToTreasury(address indexed reserve, uint256 amountMinted);

  /**
   * @dev Emitted when deficit is realized on a liquidation.
   * @param user The user address where the bad debt will be burned
   * @param debtAsset The address of the underlying borrowed asset to be burned
   * @param amountCreated The amount of deficit created
   */
  event DeficitCreated(address indexed user, address indexed debtAsset, uint256 amountCreated);

  /**
   * @dev Emitted when a position manager is approved by the user.
   * @param user The user address
   * @param positionManager The address of the position manager
   */
  event PositionManagerApproved(address indexed user, address indexed positionManager);

  /**
   * @dev Emitted when a position manager is revoked by the user.
   * @param user The user address
   * @param positionManager The address of the position manager
   */
  event PositionManagerRevoked(address indexed user, address indexed positionManager);

  /**
   * @notice Supplies an `amount` of underlying asset into the reserve, receiving in return overlying aTokens.
   * - E.g. User supplies 100 USDC and gets in return 100 aUSDC
   * @param asset The address of the underlying asset to supply
   * @param amount The amount to be supplied
   * @param onBehalfOf The address that will receive the aTokens, same as msg.sender if the user
   *   wants to receive them on his own wallet, or a different address if the beneficiary of aTokens
   *   is a different wallet
   * @param referralCode Code used to register the integrator originating the operation, for potential rewards.
   *   0 if the action is executed directly by the user, without any middle-man
   */
  function supply(address asset, uint256 amount, address onBehalfOf, uint16 referralCode) external;

  /**
   * @notice Supply with transfer approval of asset to be supplied done via permit function
   * see: https://eips.ethereum.org/EIPS/eip-2612 and https://eips.ethereum.org/EIPS/eip-713
   * @param asset The address of the underlying asset to supply
   * @param amount The amount to be supplied
   * @param onBehalfOf The address that will receive the aTokens, same as msg.sender if the user
   *   wants to receive them on his own wallet, or a different address if the beneficiary of aTokens
   *   is a different wallet
   * @param deadline The deadline timestamp that the permit is valid
   * @param referralCode Code used to register the integrator originating the operation, for potential rewards.
   *   0 if the action is executed directly by the user, without any middle-man
   * @param permitV The V parameter of ERC712 permit sig
   * @param permitR The R parameter of ERC712 permit sig
   * @param permitS The S parameter of ERC712 permit sig
   */
  function supplyWithPermit(
    address asset,
    uint256 amount,
    address onBehalfOf,
    uint16 referralCode,
    uint256 deadline,
    uint8 permitV,
    bytes32 permitR,
    bytes32 permitS
  ) external;

  /**
   * @notice Withdraws an `amount` of underlying asset from the reserve, burning the equivalent aTokens owned
   * E.g. User has 100 aUSDC, calls withdraw() and receives 100 USDC, burning the 100 aUSDC
   * @param asset The address of the underlying asset to withdraw
   * @param amount The underlying amount to be withdrawn
   *   - Send the value type(uint256).max in order to withdraw the whole aToken balance
   * @param to The address that will receive the underlying, same as msg.sender if the user
   *   wants to receive it on his own wallet, or a different address if the beneficiary is a
   *   different wallet
   * @return The final amount withdrawn
   */
  function withdraw(address asset, uint256 amount, address to) external returns (uint256);

  /**
   * @notice Allows users to borrow a specific `amount` of the reserve underlying asset, provided that the borrower
   * already supplied enough collateral, or he was given enough allowance by a credit delegator on the VariableDebtToken
   * - E.g. User borrows 100 USDC passing as `onBehalfOf` his own address, receiving the 100 USDC in his wallet
   *   and 100 variable debt tokens
   * @param asset The address of the underlying asset to borrow
   * @param amount The amount to be borrowed
   * @param interestRateMode 2 for Variable, 1 is deprecated on v3.2.0
   * @param referralCode The code used to register the integrator originating the operation, for potential rewards.
   *   0 if the action is executed directly by the user, without any middle-man
   * @param onBehalfOf The address of the user who will receive the debt. Should be the address of the borrower itself
   * calling the function if he wants to borrow against his own collateral, or the address of the credit delegator
   * if he has been given credit delegation allowance
   */
  function borrow(
    address asset,
    uint256 amount,
    uint256 interestRateMode,
    uint16 referralCode,
    address onBehalfOf
  ) external;

  /**
   * @notice Repays a borrowed `amount` on a specific reserve, burning the equivalent debt tokens owned
   * - E.g. User repays 100 USDC, burning 100 variable debt tokens of the `onBehalfOf` address
   * @param asset The address of the borrowed underlying asset previously borrowed
   * @param amount The amount to repay
   * - Send the value type(uint256).max in order to repay the whole debt for `asset` on the specific `debtMode`
   * @param interestRateMode 2 for Variable, 1 is deprecated on v3.2.0
   * @param onBehalfOf The address of the user who will get his debt reduced/removed. Should be the address of the
   * user calling the function if he wants to reduce/remove his own debt, or the address of any other
   * other borrower whose debt should be removed
   * @return The final amount repaid
   */
  function repay(
    address asset,
    uint256 amount,
    uint256 interestRateMode,
    address onBehalfOf
  ) external returns (uint256);

  /**
   * @notice Repay with transfer approval of asset to be repaid done via permit function
   * see: https://eips.ethereum.org/EIPS/eip-2612 and https://eips.ethereum.org/EIPS/eip-713
   * @param asset The address of the borrowed underlying asset previously borrowed
   * @param amount The amount to repay
   * - Send the value type(uint256).max in order to repay the whole debt for `asset` on the specific `debtMode`
   * @param interestRateMode 2 for Variable, 1 is deprecated on v3.2.0
   * @param onBehalfOf Address of the user who will get his debt reduced/removed. Should be the address of the
   * user calling the function if he wants to reduce/remove his own debt, or the address of any other
   * other borrower whose debt should be removed
   * @param deadline The deadline timestamp that the permit is valid
   * @param permitV The V parameter of ERC712 permit sig
   * @param permitR The R parameter of ERC712 permit sig
   * @param permitS The S parameter of ERC712 permit sig
   * @return The final amount repaid
   */
  function repayWithPermit(
    address asset,
    uint256 amount,
    uint256 interestRateMode,
    address onBehalfOf,
    uint256 deadline,
    uint8 permitV,
    bytes32 permitR,
    bytes32 permitS
  ) external returns (uint256);

  /**
   * @notice Repays a borrowed `amount` on a specific reserve using the reserve aTokens, burning the
   * equivalent debt tokens
   * - E.g. User repays 100 USDC using 100 aUSDC, burning 100 variable debt tokens
   * @dev  Passing uint256.max as amount will clean up any residual aToken dust balance, if the user aToken
   * balance is not enough to cover the whole debt
   * @param asset The address of the borrowed underlying asset previously borrowed
   * @param amount The amount to repay
   * - Send the value type(uint256).max in order to repay the whole debt for `asset` on the specific `debtMode`
   * @param interestRateMode DEPRECATED in v3.2.0
   * @return The final amount repaid
   */
  function repayWithATokens(
    address asset,
    uint256 amount,
    uint256 interestRateMode
  ) external returns (uint256);

  /**
   * @notice Allows suppliers to enable/disable a specific supplied asset as collateral
   * @param asset The address of the underlying asset supplied
   * @param useAsCollateral True if the user wants to use the supply as collateral, false otherwise
   */
  function setUserUseReserveAsCollateral(address asset, bool useAsCollateral) external;

  /**
   * @notice Function to liquidate a non-healthy position collateral-wise, with Health Factor below 1
   * - The caller (liquidator) covers `debtToCover` amount of debt of the user getting liquidated, and receives
   *   a proportionally amount of the `collateralAsset` plus a bonus to cover market risk
   * @param collateralAsset The address of the underlying asset used as collateral, to receive as result of the liquidation
   * @param debtAsset The address of the underlying borrowed asset to be repaid with the liquidation
   * @param borrower The address of the borrower getting liquidated
   * @param debtToCover The debt amount of borrowed `asset` the liquidator wants to cover
   * @param receiveAToken True if the liquidators wants to receive the collateral aTokens, `false` if he wants
   * to receive the underlying collateral asset directly
   */
  function liquidationCall(
    address collateralAsset,
    address debtAsset,
    address borrower,
    uint256 debtToCover,
    bool receiveAToken
  ) external;

  /**
   * @notice Allows smartcontracts to access the liquidity of the pool within one transaction,
   * as long as the amount taken plus a fee is returned.
   * @dev IMPORTANT There are security concerns for developers of flashloan receiver contracts that must be kept
   * into consideration. For further details please visit https://docs.aave.com/developers/
   * @param receiverAddress The address of the contract receiving the funds, implementing IFlashLoanReceiver interface
   * @param assets The addresses of the assets being flash-borrowed
   * @param amounts The amounts of the assets being flash-borrowed
   * @param interestRateModes Types of the debt to open if the flash loan is not returned:
   *   0 -> Don't open any debt, just revert if funds can't be transferred from the receiver
   *   1 -> Deprecated on v3.2.0
   *   2 -> Open debt at variable rate for the value of the amount flash-borrowed to the `onBehalfOf` address
   * @param onBehalfOf The address  that will receive the debt in the case of using 2 on `modes`
   * @param params Variadic packed params to pass to the receiver as extra information
   * @param referralCode The code used to register the integrator originating the operation, for potential rewards.
   *   0 if the action is executed directly by the user, without any middle-man
   */
  function flashLoan(
    address receiverAddress,
    address[] calldata assets,
    uint256[] calldata amounts,
    uint256[] calldata interestRateModes,
    address onBehalfOf,
    bytes calldata params,
    uint16 referralCode
  ) external;

  /**
   * @notice Allows smartcontracts to access the liquidity of the pool within one transaction,
   * as long as the amount taken plus a fee is returned.
   * @dev IMPORTANT There are security concerns for developers of flashloan receiver contracts that must be kept
   * into consideration. For further details please visit https://docs.aave.com/developers/
   * @param receiverAddress The address of the contract receiving the funds, implementing IFlashLoanSimpleReceiver interface
   * @param asset The address of the asset being flash-borrowed
   * @param amount The amount of the asset being flash-borrowed
   * @param params Variadic packed params to pass to the receiver as extra information
   * @param referralCode The code used to register the integrator originating the operation, for potential rewards.
   *   0 if the action is executed directly by the user, without any middle-man
   */
  function flashLoanSimple(
    address receiverAddress,
    address asset,
    uint256 amount,
    bytes calldata params,
    uint16 referralCode
  ) external;

  /**
   * @notice Returns the user account data across all the reserves
   * @param user The address of the user
   * @return totalCollateralBase The total collateral of the user in the base currency used by the price feed
   * @return totalDebtBase The total debt of the user in the base currency used by the price feed
   * @return availableBorrowsBase The borrowing power left of the user in the base currency used by the price feed
   * @return currentLiquidationThreshold The liquidation threshold of the user
   * @return ltv The loan to value of The user
   * @return healthFactor The current health factor of the user
   */
  function getUserAccountData(
    address user
  )
    external
    view
    returns (
      uint256 totalCollateralBase,
      uint256 totalDebtBase,
      uint256 availableBorrowsBase,
      uint256 currentLiquidationThreshold,
      uint256 ltv,
      uint256 healthFactor
    );

  /**
   * @notice Initializes a reserve, activating it, assigning an aToken and debt tokens
   * @dev Only callable by the PoolConfigurator contract
   * @param asset The address of the underlying asset of the reserve
   * @param aTokenAddress The address of the aToken that will be assigned to the reserve
   * @param variableDebtAddress The address of the VariableDebtToken that will be assigned to the reserve
   */
  function initReserve(address asset, address aTokenAddress, address variableDebtAddress) external;

  /**
   * @notice Drop a reserve
   * @dev Only callable by the PoolConfigurator contract
   * @dev Does not reset eMode flags, which must be considered when reusing the same reserve id for a different reserve.
   * @param asset The address of the underlying asset of the reserve
   */
  function dropReserve(address asset) external;

  /**
   * @notice Accumulates interest to all indexes of the reserve
   * @dev Only callable by the PoolConfigurator contract
   * @dev To be used when required by the configurator, for example when updating interest rates strategy data
   * @param asset The address of the underlying asset of the reserve
   */
  function syncIndexesState(address asset) external;

  /**
   * @notice Updates interest rates on the reserve data
   * @dev Only callable by the PoolConfigurator contract
   * @dev To be used when required by the configurator, for example when updating interest rates strategy data
   * @param asset The address of the underlying asset of the reserve
   */
  function syncRatesState(address asset) external;

  /**
   * @notice Sets the configuration bitmap of the reserve as a whole
   * @dev Only callable by the PoolConfigurator contract
   * @param asset The address of the underlying asset of the reserve
   * @param configuration The new configuration bitmap
   */
  function setConfiguration(
    address asset,
    DataTypes.ReserveConfigurationMap calldata configuration
  ) external;

  /**
   * @notice Returns the configuration of the reserve
   * @param asset The address of the underlying asset of the reserve
   * @return The configuration of the reserve
   */
  function getConfiguration(
    address asset
  ) external view returns (DataTypes.ReserveConfigurationMap memory);

  /**
   * @notice Returns the configuration of the user across all the reserves
   * @param user The user address
   * @return The configuration of the user
   */
  function getUserConfiguration(
    address user
  ) external view returns (DataTypes.UserConfigurationMap memory);

  /**
   * @notice Returns the normalized income of the reserve
   * @param asset The address of the underlying asset of the reserve
   * @return The reserve's normalized income
   */
  function getReserveNormalizedIncome(address asset) external view returns (uint256);

  /**
   * @notice Returns the normalized variable debt per unit of asset
   * @dev WARNING: This function is intended to be used primarily by the protocol itself to get a
   * "dynamic" variable index based on time, current stored index and virtual rate at the current
   * moment (approx. a borrower would get if opening a position). This means that is always used in
   * combination with variable debt supply/balances.
   * If using this function externally, consider that is possible to have an increasing normalized
   * variable debt that is not equivalent to how the variable debt index would be updated in storage
   * (e.g. only updates with non-zero variable debt supply)
   * @param asset The address of the underlying asset of the reserve
   * @return The reserve normalized variable debt
   */
  function getReserveNormalizedVariableDebt(address asset) external view returns (uint256);

  /**
   * @notice Returns the state and configuration of the reserve
   * @param asset The address of the underlying asset of the reserve
   * @return The state and configuration data of the reserve
   */
  function getReserveData(address asset) external view returns (DataTypes.ReserveDataLegacy memory);

  /**
   * @notice Returns the virtual underlying balance of the reserve
   * @param asset The address of the underlying asset of the reserve
   * @return The reserve virtual underlying balance
   */
  function getVirtualUnderlyingBalance(address asset) external view returns (uint128);

  /**
   * @notice Validates and finalizes an aToken transfer
   * @dev Only callable by the overlying aToken of the `asset`
   * @param asset The address of the underlying asset of the aToken
   * @param from The user from which the aTokens are transferred
   * @param to The user receiving the aTokens
   * @param scaledAmount The scaled amount being transferred/withdrawn
   * @param scaledBalanceFromBefore The aToken scaled balance of the `from` user before the transfer
   * @param scaledBalanceToBefore The aToken scaled balance of the `to` user before the transfer
   */
  function finalizeTransfer(
    address asset,
    address from,
    address to,
    uint256 scaledAmount,
    uint256 scaledBalanceFromBefore,
    uint256 scaledBalanceToBefore
  ) external;

  /**
   * @notice Returns the list of the underlying assets of all the initialized reserves
   * @dev It does not include dropped reserves
   * @return The addresses of the underlying assets of the initialized reserves
   */
  function getReservesList() external view returns (address[] memory);

  /**
   * @notice Returns the number of initialized reserves
   * @dev It includes dropped reserves
   * @return The count
   */
  function getReservesCount() external view returns (uint256);

  /**
   * @notice Returns the address of the underlying asset of a reserve by the reserve id as stored in the DataTypes.ReserveData struct
   * @param id The id of the reserve as stored in the DataTypes.ReserveData struct
   * @return The address of the reserve associated with id
   */
  function getReserveAddressById(uint16 id) external view returns (address);

  /**
   * @notice Returns the PoolAddressesProvider connected to this contract
   * @return The address of the PoolAddressesProvider
   */
  function ADDRESSES_PROVIDER() external view returns (IPoolAddressesProvider);

  /**
   * @notice Returns the ReserveInterestRateStrategy connected to all the reserves
   * @return The address of the ReserveInterestRateStrategy contract
   */
  function RESERVE_INTEREST_RATE_STRATEGY() external view returns (address);

  /**
   * @notice Updates flash loan premium. All this premium is collected by the protocol treasury.
   * @dev The premium is calculated on the total borrowed amount
   * @dev Only callable by the PoolConfigurator contract
   * @param flashLoanPremium The flash loan premium, expressed in bps
   */
  function updateFlashloanPremium(uint128 flashLoanPremium) external;

  /**
   * @notice Configures a new or alters an existing collateral configuration of an eMode.
   * @dev In eMode, the protocol allows very high borrowing power to borrow assets of the same category.
   * The category 0 is reserved as it's the default for volatile assets
   * @param id The id of the category
   * @param config The configuration of the category
   */
  function configureEModeCategory(
    uint8 id,
    DataTypes.EModeCategoryBaseConfiguration memory config
  ) external;

  /**
   * @notice Replaces the current eMode collateralBitmap.
   * @param id The id of the category
   * @param collateralBitmap The collateralBitmap of the category
   */
  function configureEModeCategoryCollateralBitmap(uint8 id, uint128 collateralBitmap) external;

  /**
   * @notice Replaces the current eMode borrowableBitmap.
   * @param id The id of the category
   * @param borrowableBitmap The borrowableBitmap of the category
   */
  function configureEModeCategoryBorrowableBitmap(uint8 id, uint128 borrowableBitmap) external;

  /**
   * @notice Returns the data of an eMode category
   * @dev DEPRECATED use independent getters instead
   * @param id The id of the category
   * @return The configuration data of the category
   */
  function getEModeCategoryData(
    uint8 id
  ) external view returns (DataTypes.EModeCategoryLegacy memory);

  /**
   * @notice Returns the label of an eMode category
   * @param id The id of the category
   * @return The label of the category
   */
  function getEModeCategoryLabel(uint8 id) external view returns (string memory);

  /**
   * @notice Returns the collateral config of an eMode category
   * @param id The id of the category
   * @return The ltv,lt,lb of the category
   */
  function getEModeCategoryCollateralConfig(
    uint8 id
  ) external view returns (DataTypes.CollateralConfig memory);

  /**
   * @notice Returns the collateralBitmap of an eMode category
   * @param id The id of the category
   * @return The collateralBitmap of the category
   */
  function getEModeCategoryCollateralBitmap(uint8 id) external view returns (uint128);

  /**
   * @notice Returns the borrowableBitmap of an eMode category
   * @param id The id of the category
   * @return The borrowableBitmap of the category
   */
  function getEModeCategoryBorrowableBitmap(uint8 id) external view returns (uint128);

  /**
   * @notice Allows a user to use the protocol in eMode
   * @param categoryId The id of the category
   */
  function setUserEMode(uint8 categoryId) external;

  /**
   * @notice Returns the eMode the user is using
   * @param user The address of the user
   * @return The eMode id
   */
  function getUserEMode(address user) external view returns (uint256);

  /**
   * @notice Resets the isolation mode total debt of the given asset to zero
   * @dev It requires the given asset has zero debt ceiling
   * @param asset The address of the underlying asset to reset the isolationModeTotalDebt
   */
  function resetIsolationModeTotalDebt(address asset) external;

  /**
   * @notice Sets the liquidation grace period of the given asset
   * @dev To enable a liquidation grace period, a timestamp in the future should be set,
   *      To disable a liquidation grace period, any timestamp in the past works, like 0
   * @param asset The address of the underlying asset to set the liquidationGracePeriod
   * @param until Timestamp when the liquidation grace period will end
   **/
  function setLiquidationGracePeriod(address asset, uint40 until) external;

  /**
   * @notice Returns the liquidation grace period of the given asset
   * @param asset The address of the underlying asset
   * @return Timestamp when the liquidation grace period will end
   **/
  function getLiquidationGracePeriod(address asset) external view returns (uint40);

  /**
   * @notice Returns the total fee on flash loans.
   * @dev From v3.4 all flashloan fees will be send to the treasury.
   * @return The total fee on flashloans
   */
  function FLASHLOAN_PREMIUM_TOTAL() external view returns (uint128);

  /**
   * @notice Returns the part of the flashloan fees sent to protocol
   * @dev From v3.4 all flashloan fees will be send to the treasury and this value
   *      is always 100_00.
   * @return The flashloan fee sent to the protocol treasury
   */
  function FLASHLOAN_PREMIUM_TO_PROTOCOL() external view returns (uint128);

  /**
   * @notice Returns the maximum number of reserves supported to be listed in this Pool
   * @return The maximum number of reserves supported
   */
  function MAX_NUMBER_RESERVES() external view returns (uint16);

  /**
   * @notice Mints the assets accrued through the reserve factor to the treasury in the form of aTokens
   * @param assets The list of reserves for which the minting needs to be executed
   */
  function mintToTreasury(address[] calldata assets) external;

  /**
   * @notice Rescue and transfer tokens locked in this contract
   * @param token The address of the token
   * @param to The address of the recipient
   * @param amount The amount of token to transfer
   */
  function rescueTokens(address token, address to, uint256 amount) external;

  /**
   * @notice Supplies an `amount` of underlying asset into the reserve, receiving in return overlying aTokens.
   * - E.g. User supplies 100 USDC and gets in return 100 aUSDC
   * @dev Deprecated: Use the `supply` function instead
   * @param asset The address of the underlying asset to supply
   * @param amount The amount to be supplied
   * @param onBehalfOf The address that will receive the aTokens, same as msg.sender if the user
   *   wants to receive them on his own wallet, or a different address if the beneficiary of aTokens
   *   is a different wallet
   * @param referralCode Code used to register the integrator originating the operation, for potential rewards.
   *   0 if the action is executed directly by the user, without any middle-man
   */
  function deposit(address asset, uint256 amount, address onBehalfOf, uint16 referralCode) external;

  /**
   * @notice It covers the deficit of a specified reserve by burning the equivalent aToken `amount` for assets
   * @dev The deficit of a reserve can occur due to situations where borrowed assets are not repaid, leading to bad debt.
   * @param asset The address of the underlying asset to cover the deficit.
   * @param amount The amount to be covered, in aToken
   * @return The amount of tokens burned
   */
  function eliminateReserveDeficit(address asset, uint256 amount) external returns (uint256);

  /**
   * @notice Approves or disapproves a position manager. This position manager will be able
   * to call the `setUserUseReserveAsCollateralOnBehalfOf` and the
   * `setUserEModeOnBehalfOf` function on behalf of the user.
   * @param positionManager The address of the position manager
   * @param approve True if the position manager should be approved, false otherwise
   */
  function approvePositionManager(address positionManager, bool approve) external;

  /**
   * @notice Renounces a position manager role for a given user.
   * @param user The address of the user
   */
  function renouncePositionManagerRole(address user) external;

  /**
   * @notice Sets the use as collateral flag for the user on the specific reserve on behalf of the user.
   * @param asset The address of the underlying asset of the reserve
   * @param useAsCollateral True if the user wants to use the reserve as collateral, false otherwise
   * @param onBehalfOf The address of the user
   */
  function setUserUseReserveAsCollateralOnBehalfOf(
    address asset,
    bool useAsCollateral,
    address onBehalfOf
  ) external;

  /**
   * @notice Sets the eMode category for the user on the specific reserve on behalf of the user.
   * @param categoryId The id of the category
   * @param onBehalfOf The address of the user
   */
  function setUserEModeOnBehalfOf(uint8 categoryId, address onBehalfOf) external;

  /*
   * @notice Returns true if the `positionManager` address is approved to use the position manager role on behalf of the user.
   * @param user The address of the user
   * @param positionManager The address of the position manager
   * @return True if the user is approved to use the position manager, false otherwise
   */
  function isApprovedPositionManager(
    address user,
    address positionManager
  ) external view returns (bool);

  /**
   * @notice Returns the current deficit of a reserve.
   * @param asset The address of the underlying asset of the reserve
   * @return The current deficit of the reserve
   */
  function getReserveDeficit(address asset) external view returns (uint256);

  /**
   * @notice Returns the aToken address of a reserve.
   * @param asset The address of the underlying asset of the reserve
   * @return The address of the aToken
   */
  function getReserveAToken(address asset) external view returns (address);

  /**
   * @notice Returns the variableDebtToken address of a reserve.
   * @param asset The address of the underlying asset of the reserve
   * @return The address of the variableDebtToken
   */
  function getReserveVariableDebtToken(address asset) external view returns (address);

  /**
   * @notice Gets the address of the external FlashLoanLogic
   */
  function getFlashLoanLogic() external view returns (address);

  /**
   * @notice Gets the address of the external BorrowLogic
   */
  function getBorrowLogic() external view returns (address);

  /**
   * @notice Gets the address of the external EModeLogic
   */
  function getEModeLogic() external view returns (address);

  /**
   * @notice Gets the address of the external LiquidationLogic
   */
  function getLiquidationLogic() external view returns (address);

  /**
   * @notice Gets the address of the external PoolLogic
   */
  function getPoolLogic() external view returns (address);

  /**
   * @notice Gets the address of the external SupplyLogic
   */
  function getSupplyLogic() external view returns (address);
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {Errors} from '../helpers/Errors.sol';
import {ReserveConfiguration} from './ReserveConfiguration.sol';

/**
 * @title EModeConfiguration library
 * @author BGD Labs
 * @notice Implements the bitmap logic to handle the eMode configuration
 */
library EModeConfiguration {
  /**
   * @notice Sets a bit in a given bitmap that represents the reserve index range
   * @dev The supplied bitmap is supposed to be a uint128 in which each bit represents a reserve
   * @param bitmap The bitmap
   * @param reserveIndex The index of the reserve in the bitmap
   * @param enabled True if the reserveIndex should be enabled on the bitmap, false otherwise
   * @return The altered bitmap
   */
  function setReserveBitmapBit(
    uint128 bitmap,
    uint256 reserveIndex,
    bool enabled
  ) internal pure returns (uint128) {
    unchecked {
      require(reserveIndex < ReserveConfiguration.MAX_RESERVES_COUNT, Errors.InvalidReserveIndex());
      uint128 bit = uint128(1 << reserveIndex);
      if (enabled) {
        return bitmap | bit;
      } else {
        return bitmap & ~bit;
      }
    }
  }

  /**
   * @notice Validates if a reserveIndex is flagged as enabled on a given bitmap
   * @param bitmap The bitmap
   * @param reserveIndex The index of the reserve in the bitmap
   * @return True if the reserveindex is flagged true
   */
  function isReserveEnabledOnBitmap(
    uint128 bitmap,
    uint256 reserveIndex
  ) internal pure returns (bool) {
    unchecked {
      require(reserveIndex < ReserveConfiguration.MAX_RESERVES_COUNT, Errors.InvalidReserveIndex());
      return (bitmap >> reserveIndex) & 1 != 0;
    }
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

enum DelegationMode {
  NO_DELEGATION,
  VOTING_DELEGATED,
  PROPOSITION_DELEGATED,
  FULL_POWER_DELEGATED
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {Multicall, Context} from 'openzeppelin-contracts/contracts/utils/Multicall.sol';
import {VersionedInitializable} from '../../misc/aave-upgradeability/VersionedInitializable.sol';
import {Errors} from '../libraries/helpers/Errors.sol';
import {ReserveConfiguration} from '../libraries/configuration/ReserveConfiguration.sol';
import {PoolLogic} from '../libraries/logic/PoolLogic.sol';
import {ReserveLogic} from '../libraries/logic/ReserveLogic.sol';
import {EModeLogic} from '../libraries/logic/EModeLogic.sol';
import {SupplyLogic} from '../libraries/logic/SupplyLogic.sol';
import {FlashLoanLogic} from '../libraries/logic/FlashLoanLogic.sol';
import {BorrowLogic} from '../libraries/logic/BorrowLogic.sol';
import {LiquidationLogic} from '../libraries/logic/LiquidationLogic.sol';
import {DataTypes} from '../libraries/types/DataTypes.sol';
import {IERC20WithPermit} from '../../interfaces/IERC20WithPermit.sol';
import {IPoolAddressesProvider} from '../../interfaces/IPoolAddressesProvider.sol';
import {IReserveInterestRateStrategy} from '../../interfaces/IReserveInterestRateStrategy.sol';
import {IPool} from '../../interfaces/IPool.sol';
import {IACLManager} from '../../interfaces/IACLManager.sol';
import {PoolStorage} from './PoolStorage.sol';

/**
 * @title Pool contract
 * @author Aave
 * @notice Main point of interaction with an Aave protocol's market
 * - Users can:
 *   # Supply
 *   # Withdraw
 *   # Borrow
 *   # Repay
 *   # Enable/disable their supplied assets as collateral
 *   # Liquidate positions
 *   # Execute Flash Loans
 * @dev To be covered by a proxy contract, owned by the PoolAddressesProvider of the specific market
 * @dev All admin functions are callable by the PoolConfigurator contract defined also in the
 *   PoolAddressesProvider
 */
abstract contract Pool is VersionedInitializable, PoolStorage, IPool, Multicall {
  using ReserveLogic for DataTypes.ReserveData;

  IPoolAddressesProvider public immutable ADDRESSES_PROVIDER;

  address public immutable RESERVE_INTEREST_RATE_STRATEGY;

  // @notice The name used to fetch the UMBRELLA contract
  bytes32 public constant UMBRELLA = 'UMBRELLA';

  /**
   * @dev Only pool configurator can call functions marked by this modifier.
   */
  modifier onlyPoolConfigurator() {
    _onlyPoolConfigurator();
    _;
  }

  /**
   * @dev Only pool admin can call functions marked by this modifier.
   */
  modifier onlyPoolAdmin() {
    _onlyPoolAdmin();
    _;
  }

  /**
   * @dev Only an approved position manager can call functions marked by this modifier.
   */
  modifier onlyPositionManager(address onBehalfOf) {
    _onlyPositionManager(onBehalfOf);
    _;
  }

  /**
   * @dev Only the umbrella contract can call functions marked by this modifier.
   */
  modifier onlyUmbrella() {
    require(ADDRESSES_PROVIDER.getAddress(UMBRELLA) == _msgSender(), Errors.CallerNotUmbrella());
    _;
  }

  function _onlyPoolConfigurator() internal view virtual {
    require(
      ADDRESSES_PROVIDER.getPoolConfigurator() == _msgSender(),
      Errors.CallerNotPoolConfigurator()
    );
  }

  function _onlyPoolAdmin() internal view virtual {
    require(
      IACLManager(ADDRESSES_PROVIDER.getACLManager()).isPoolAdmin(_msgSender()),
      Errors.CallerNotPoolAdmin()
    );
  }

  function _onlyPositionManager(address onBehalfOf) internal view virtual {
    require(_positionManager[onBehalfOf][_msgSender()], Errors.CallerNotPositionManager());
  }

  /**
   * @dev Constructor.
   * @param provider The address of the PoolAddressesProvider contract
   */
  constructor(IPoolAddressesProvider provider, IReserveInterestRateStrategy interestRateStrategy) {
    ADDRESSES_PROVIDER = provider;
    require(address(interestRateStrategy) != address(0), Errors.ZeroAddressNotValid());
    RESERVE_INTEREST_RATE_STRATEGY = address(interestRateStrategy);
  }

  /**
   * @notice Initializes the Pool.
   * @dev Function is invoked by the proxy contract when the Pool contract is added to the
   * PoolAddressesProvider of the market.
   * @dev Caching the address of the PoolAddressesProvider in order to reduce gas consumption on subsequent operations
   * @param provider The address of the PoolAddressesProvider
   */
  function initialize(IPoolAddressesProvider provider) external virtual;

  /// @inheritdoc IPool
  function supply(
    address asset,
    uint256 amount,
    address onBehalfOf,
    uint16 referralCode
  ) public virtual override {
    SupplyLogic.executeSupply(
      _reserves,
      _reservesList,
      _usersConfig[onBehalfOf],
      DataTypes.ExecuteSupplyParams({
        user: _msgSender(),
        asset: asset,
        interestRateStrategyAddress: RESERVE_INTEREST_RATE_STRATEGY,
        amount: amount,
        onBehalfOf: onBehalfOf,
        referralCode: referralCode
      })
    );
  }

  /// @inheritdoc IPool
  function supplyWithPermit(
    address asset,
    uint256 amount,
    address onBehalfOf,
    uint16 referralCode,
    uint256 deadline,
    uint8 permitV,
    bytes32 permitR,
    bytes32 permitS
  ) public virtual override {
    try
      IERC20WithPermit(asset).permit(
        _msgSender(),
        address(this),
        amount,
        deadline,
        permitV,
        permitR,
        permitS
      )
    {} catch {}
    SupplyLogic.executeSupply(
      _reserves,
      _reservesList,
      _usersConfig[onBehalfOf],
      DataTypes.ExecuteSupplyParams({
        user: _msgSender(),
        asset: asset,
        interestRateStrategyAddress: RESERVE_INTEREST_RATE_STRATEGY,
        amount: amount,
        onBehalfOf: onBehalfOf,
        referralCode: referralCode
      })
    );
  }

  /// @inheritdoc IPool
  function withdraw(
    address asset,
    uint256 amount,
    address to
  ) public virtual override returns (uint256) {
    return
      SupplyLogic.executeWithdraw(
        _reserves,
        _reservesList,
        _eModeCategories,
        _usersConfig[_msgSender()],
        DataTypes.ExecuteWithdrawParams({
          user: _msgSender(),
          asset: asset,
          interestRateStrategyAddress: RESERVE_INTEREST_RATE_STRATEGY,
          amount: amount,
          to: to,
          oracle: ADDRESSES_PROVIDER.getPriceOracle(),
          userEModeCategory: _usersEModeCategory[_msgSender()]
        })
      );
  }

  /// @inheritdoc IPool
  function borrow(
    address asset,
    uint256 amount,
    uint256 interestRateMode,
    uint16 referralCode,
    address onBehalfOf
  ) public virtual override {
    BorrowLogic.executeBorrow(
      _reserves,
      _reservesList,
      _eModeCategories,
      _usersConfig[onBehalfOf],
      DataTypes.ExecuteBorrowParams({
        asset: asset,
        interestRateStrategyAddress: RESERVE_INTEREST_RATE_STRATEGY,
        user: _msgSender(),
        onBehalfOf: onBehalfOf,
        amount: amount,
        interestRateMode: DataTypes.InterestRateMode(interestRateMode),
        referralCode: referralCode,
        releaseUnderlying: true,
        oracle: ADDRESSES_PROVIDER.getPriceOracle(),
        userEModeCategory: _usersEModeCategory[onBehalfOf],
        priceOracleSentinel: ADDRESSES_PROVIDER.getPriceOracleSentinel()
      })
    );
  }

  /// @inheritdoc IPool
  function repay(
    address asset,
    uint256 amount,
    uint256 interestRateMode,
    address onBehalfOf
  ) public virtual override returns (uint256) {
    return
      BorrowLogic.executeRepay(
        _reserves,
        _reservesList,
        _eModeCategories,
        _usersConfig[onBehalfOf],
        DataTypes.ExecuteRepayParams({
          asset: asset,
          user: _msgSender(),
          interestRateStrategyAddress: RESERVE_INTEREST_RATE_STRATEGY,
          amount: amount,
          interestRateMode: DataTypes.InterestRateMode(interestRateMode),
          onBehalfOf: onBehalfOf,
          useATokens: false,
          oracle: ADDRESSES_PROVIDER.getPriceOracle(),
          userEModeCategory: _usersEModeCategory[onBehalfOf]
        })
      );
  }

  /// @inheritdoc IPool
  function repayWithPermit(
    address asset,
    uint256 amount,
    uint256 interestRateMode,
    address onBehalfOf,
    uint256 deadline,
    uint8 permitV,
    bytes32 permitR,
    bytes32 permitS
  ) public virtual override returns (uint256) {
    try
      IERC20WithPermit(asset).permit(
        _msgSender(),
        address(this),
        amount,
        deadline,
        permitV,
        permitR,
        permitS
      )
    {} catch {}

    {
      DataTypes.ExecuteRepayParams memory params = DataTypes.ExecuteRepayParams({
        asset: asset,
        user: _msgSender(),
        interestRateStrategyAddress: RESERVE_INTEREST_RATE_STRATEGY,
        amount: amount,
        interestRateMode: DataTypes.InterestRateMode(interestRateMode),
        onBehalfOf: onBehalfOf,
        useATokens: false,
        oracle: ADDRESSES_PROVIDER.getPriceOracle(),
        userEModeCategory: _usersEModeCategory[onBehalfOf]
      });
      return
        BorrowLogic.executeRepay(
          _reserves,
          _reservesList,
          _eModeCategories,
          _usersConfig[onBehalfOf],
          params
        );
    }
  }

  /// @inheritdoc IPool
  function repayWithATokens(
    address asset,
    uint256 amount,
    uint256 interestRateMode
  ) public virtual override returns (uint256) {
    return
      BorrowLogic.executeRepay(
        _reserves,
        _reservesList,
        _eModeCategories,
        _usersConfig[_msgSender()],
        DataTypes.ExecuteRepayParams({
          asset: asset,
          user: _msgSender(),
          interestRateStrategyAddress: RESERVE_INTEREST_RATE_STRATEGY,
          amount: amount,
          interestRateMode: DataTypes.InterestRateMode(interestRateMode),
          onBehalfOf: _msgSender(),
          useATokens: true,
          oracle: ADDRESSES_PROVIDER.getPriceOracle(),
          userEModeCategory: _usersEModeCategory[_msgSender()]
        })
      );
  }

  /// @inheritdoc IPool
  function setUserUseReserveAsCollateral(
    address asset,
    bool useAsCollateral
  ) public virtual override {
    SupplyLogic.executeUseReserveAsCollateral(
      _reserves,
      _reservesList,
      _eModeCategories,
      _usersConfig[_msgSender()],
      _msgSender(),
      asset,
      useAsCollateral,
      ADDRESSES_PROVIDER.getPriceOracle(),
      _usersEModeCategory[_msgSender()]
    );
  }

  /// @inheritdoc IPool
  function liquidationCall(
    address collateralAsset,
    address debtAsset,
    address borrower,
    uint256 debtToCover,
    bool receiveAToken
  ) public virtual override {
    LiquidationLogic.executeLiquidationCall(
      _reserves,
      _reservesList,
      _usersConfig,
      _eModeCategories,
      DataTypes.ExecuteLiquidationCallParams({
        liquidator: _msgSender(),
        debtToCover: debtToCover,
        collateralAsset: collateralAsset,
        debtAsset: debtAsset,
        borrower: borrower,
        receiveAToken: receiveAToken,
        priceOracle: ADDRESSES_PROVIDER.getPriceOracle(),
        borrowerEModeCategory: _usersEModeCategory[borrower],
        priceOracleSentinel: ADDRESSES_PROVIDER.getPriceOracleSentinel(),
        interestRateStrategyAddress: RESERVE_INTEREST_RATE_STRATEGY
      })
    );
  }

  /// @inheritdoc IPool
  function flashLoan(
    address receiverAddress,
    address[] calldata assets,
    uint256[] calldata amounts,
    uint256[] calldata interestRateModes,
    address onBehalfOf,
    bytes calldata params,
    uint16 referralCode
  ) public virtual override {
    DataTypes.FlashloanParams memory flashParams = DataTypes.FlashloanParams({
      user: _msgSender(),
      receiverAddress: receiverAddress,
      assets: assets,
      amounts: amounts,
      interestRateModes: interestRateModes,
      interestRateStrategyAddress: RESERVE_INTEREST_RATE_STRATEGY,
      onBehalfOf: onBehalfOf,
      params: params,
      referralCode: referralCode,
      flashLoanPremium: _flashLoanPremium,
      addressesProvider: address(ADDRESSES_PROVIDER),
      pool: address(this),
      userEModeCategory: _usersEModeCategory[onBehalfOf],
      isAuthorizedFlashBorrower: IACLManager(ADDRESSES_PROVIDER.getACLManager()).isFlashBorrower(
        _msgSender()
      )
    });

    FlashLoanLogic.executeFlashLoan(
      _reserves,
      _reservesList,
      _eModeCategories,
      _usersConfig[onBehalfOf],
      flashParams
    );
  }

  /// @inheritdoc IPool
  function flashLoanSimple(
    address receiverAddress,
    address asset,
    uint256 amount,
    bytes calldata params,
    uint16 referralCode
  ) public virtual override {
    DataTypes.FlashloanSimpleParams memory flashParams = DataTypes.FlashloanSimpleParams({
      user: _msgSender(),
      receiverAddress: receiverAddress,
      asset: asset,
      interestRateStrategyAddress: RESERVE_INTEREST_RATE_STRATEGY,
      amount: amount,
      params: params,
      referralCode: referralCode,
      flashLoanPremium: _flashLoanPremium
    });
    FlashLoanLogic.executeFlashLoanSimple(_reserves[asset], flashParams);
  }

  /// @inheritdoc IPool
  function mintToTreasury(address[] calldata assets) external virtual override {
    PoolLogic.executeMintToTreasury(_reserves, assets);
  }

  /// @inheritdoc IPool
  function getReserveData(
    address asset
  ) external view virtual override returns (DataTypes.ReserveDataLegacy memory res) {
    DataTypes.ReserveData storage reserve = _reserves[asset];
    res.configuration = reserve.configuration;
    res.liquidityIndex = reserve.liquidityIndex;
    res.currentLiquidityRate = reserve.currentLiquidityRate;
    res.variableBorrowIndex = reserve.variableBorrowIndex;
    res.currentVariableBorrowRate = reserve.currentVariableBorrowRate;
    res.lastUpdateTimestamp = reserve.lastUpdateTimestamp;
    res.id = reserve.id;
    res.aTokenAddress = reserve.aTokenAddress;
    res.variableDebtTokenAddress = reserve.variableDebtTokenAddress;
    res.interestRateStrategyAddress = RESERVE_INTEREST_RATE_STRATEGY;
    res.accruedToTreasury = reserve.accruedToTreasury;
    res.unbacked = 0;
    res.isolationModeTotalDebt = reserve.isolationModeTotalDebt;
    // This is a temporary workaround for integrations that are broken by Aave 3.2
    // While the new pool data provider is backward compatible, some integrations hard-code an old implementation
    // To allow them to not have any infrastructural blocker, a mock must be configured in the Aave Pool Addresses Provider, returning zero on all required view methods, instead of reverting
    res.stableDebtTokenAddress = ADDRESSES_PROVIDER.getAddress(bytes32('MOCK_STABLE_DEBT'));
  }

  /// @inheritdoc IPool
  function getVirtualUnderlyingBalance(
    address asset
  ) external view virtual override returns (uint128) {
    return _reserves[asset].virtualUnderlyingBalance;
  }

  /// @inheritdoc IPool
  function getUserAccountData(
    address user
  )
    external
    view
    virtual
    override
    returns (
      uint256 totalCollateralBase,
      uint256 totalDebtBase,
      uint256 availableBorrowsBase,
      uint256 currentLiquidationThreshold,
      uint256 ltv,
      uint256 healthFactor
    )
  {
    return
      PoolLogic.executeGetUserAccountData(
        _reserves,
        _reservesList,
        _eModeCategories,
        DataTypes.CalculateUserAccountDataParams({
          userConfig: _usersConfig[user],
          user: user,
          oracle: ADDRESSES_PROVIDER.getPriceOracle(),
          userEModeCategory: _usersEModeCategory[user]
        })
      );
  }

  /// @inheritdoc IPool
  function getConfiguration(
    address asset
  ) external view virtual override returns (DataTypes.ReserveConfigurationMap memory) {
    return _reserves[asset].configuration;
  }

  /// @inheritdoc IPool
  function getUserConfiguration(
    address user
  ) external view virtual override returns (DataTypes.UserConfigurationMap memory) {
    return _usersConfig[user];
  }

  /// @inheritdoc IPool
  function getReserveNormalizedIncome(
    address asset
  ) external view virtual override returns (uint256) {
    return _reserves[asset].getNormalizedIncome();
  }

  /// @inheritdoc IPool
  function getReserveNormalizedVariableDebt(
    address asset
  ) external view virtual override returns (uint256) {
    return _reserves[asset].getNormalizedDebt();
  }

  /// @inheritdoc IPool
  function getReservesList() external view virtual override returns (address[] memory) {
    uint256 reservesListCount = _reservesCount;
    uint256 droppedReservesCount = 0;
    address[] memory reservesList = new address[](reservesListCount);

    for (uint256 i = 0; i < reservesListCount; i++) {
      if (_reservesList[i] != address(0)) {
        reservesList[i - droppedReservesCount] = _reservesList[i];
      } else {
        droppedReservesCount++;
      }
    }

    // Reduces the length of the reserves array by `droppedReservesCount`
    assembly {
      mstore(reservesList, sub(reservesListCount, droppedReservesCount))
    }
    return reservesList;
  }

  /// @inheritdoc IPool
  function getReservesCount() external view virtual override returns (uint256) {
    return _reservesCount;
  }

  /// @inheritdoc IPool
  function getReserveAddressById(uint16 id) external view returns (address) {
    return _reservesList[id];
  }

  /// @inheritdoc IPool
  function FLASHLOAN_PREMIUM_TOTAL() public view virtual override returns (uint128) {
    return _flashLoanPremium;
  }

  /// @inheritdoc IPool
  function FLASHLOAN_PREMIUM_TO_PROTOCOL() public view virtual override returns (uint128) {
    return 100_00;
  }

  /// @inheritdoc IPool
  function MAX_NUMBER_RESERVES() public view virtual override returns (uint16) {
    return ReserveConfiguration.MAX_RESERVES_COUNT;
  }

  /// @inheritdoc IPool
  function finalizeTransfer(
    address asset,
    address from,
    address to,
    uint256 scaledAmount,
    uint256 scaledBalanceFromBefore,
    uint256 scaledBalanceToBefore
  ) external virtual override {
    require(_msgSender() == _reserves[asset].aTokenAddress, Errors.CallerNotAToken());
    SupplyLogic.executeFinalizeTransfer(
      _reserves,
      _reservesList,
      _eModeCategories,
      _usersConfig,
      DataTypes.FinalizeTransferParams({
        asset: asset,
        from: from,
        to: to,
        scaledAmount: scaledAmount,
        scaledBalanceFromBefore: scaledBalanceFromBefore,
        scaledBalanceToBefore: scaledBalanceToBefore,
        oracle: ADDRESSES_PROVIDER.getPriceOracle(),
        fromEModeCategory: _usersEModeCategory[from]
      })
    );
  }

  /// @inheritdoc IPool
  function initReserve(
    address asset,
    address aTokenAddress,
    address variableDebtAddress
  ) external virtual override onlyPoolConfigurator {
    if (
      PoolLogic.executeInitReserve(
        _reserves,
        _reservesList,
        DataTypes.InitReserveParams({
          asset: asset,
          aTokenAddress: aTokenAddress,
          variableDebtAddress: variableDebtAddress,
          reservesCount: _reservesCount,
          maxNumberReserves: MAX_NUMBER_RESERVES()
        })
      )
    ) {
      _reservesCount++;
    }
  }

  /// @inheritdoc IPool
  function dropReserve(address asset) external virtual override onlyPoolConfigurator {
    PoolLogic.executeDropReserve(_reserves, _reservesList, asset);
  }

  /// @inheritdoc IPool
  function syncIndexesState(address asset) external virtual override onlyPoolConfigurator {
    PoolLogic.executeSyncIndexesState(_reserves[asset]);
  }

  /// @inheritdoc IPool
  function syncRatesState(address asset) external virtual override onlyPoolConfigurator {
    PoolLogic.executeSyncRatesState(_reserves[asset], asset, RESERVE_INTEREST_RATE_STRATEGY);
  }

  /// @inheritdoc IPool
  function setConfiguration(
    address asset,
    DataTypes.ReserveConfigurationMap calldata configuration
  ) external virtual override onlyPoolConfigurator {
    require(asset != address(0), Errors.ZeroAddressNotValid());
    require(_reserves[asset].id != 0 || _reservesList[0] == asset, Errors.AssetNotListed());
    _reserves[asset].configuration = configuration;
  }

  /// @inheritdoc IPool
  function updateFlashloanPremium(
    uint128 flashLoanPremium
  ) external virtual override onlyPoolConfigurator {
    _flashLoanPremium = flashLoanPremium;
  }

  /// @inheritdoc IPool
  function configureEModeCategory(
    uint8 id,
    DataTypes.EModeCategoryBaseConfiguration calldata category
  ) external virtual override onlyPoolConfigurator {
    // category 0 is reserved for volatile heterogeneous assets and it's always disabled
    require(id != 0, Errors.EModeCategoryReserved());
    _eModeCategories[id].ltv = category.ltv;
    _eModeCategories[id].liquidationThreshold = category.liquidationThreshold;
    _eModeCategories[id].liquidationBonus = category.liquidationBonus;
    _eModeCategories[id].label = category.label;
  }

  /// @inheritdoc IPool
  function configureEModeCategoryCollateralBitmap(
    uint8 id,
    uint128 collateralBitmap
  ) external virtual override onlyPoolConfigurator {
    // category 0 is reserved for volatile heterogeneous assets and it's always disabled
    require(id != 0, Errors.EModeCategoryReserved());
    _eModeCategories[id].collateralBitmap = collateralBitmap;
  }

  /// @inheritdoc IPool
  function configureEModeCategoryBorrowableBitmap(
    uint8 id,
    uint128 borrowableBitmap
  ) external virtual override onlyPoolConfigurator {
    // category 0 is reserved for volatile heterogeneous assets and it's always disabled
    require(id != 0, Errors.EModeCategoryReserved());
    _eModeCategories[id].borrowableBitmap = borrowableBitmap;
  }

  /// @inheritdoc IPool
  function getEModeCategoryData(
    uint8 id
  ) external view virtual override returns (DataTypes.EModeCategoryLegacy memory) {
    DataTypes.EModeCategory storage category = _eModeCategories[id];
    return
      DataTypes.EModeCategoryLegacy({
        ltv: category.ltv,
        liquidationThreshold: category.liquidationThreshold,
        liquidationBonus: category.liquidationBonus,
        priceSource: address(0),
        label: category.label
      });
  }

  /// @inheritdoc IPool
  function getEModeCategoryCollateralConfig(
    uint8 id
  ) external view returns (DataTypes.CollateralConfig memory res) {
    res.ltv = _eModeCategories[id].ltv;
    res.liquidationThreshold = _eModeCategories[id].liquidationThreshold;
    res.liquidationBonus = _eModeCategories[id].liquidationBonus;
  }

  /// @inheritdoc IPool
  function getEModeCategoryLabel(uint8 id) external view returns (string memory) {
    return _eModeCategories[id].label;
  }

  /// @inheritdoc IPool
  function getEModeCategoryCollateralBitmap(uint8 id) external view returns (uint128) {
    return _eModeCategories[id].collateralBitmap;
  }

  /// @inheritdoc IPool
  function getEModeCategoryBorrowableBitmap(uint8 id) external view returns (uint128) {
    return _eModeCategories[id].borrowableBitmap;
  }

  /// @inheritdoc IPool
  function setUserEMode(uint8 categoryId) external virtual override {
    EModeLogic.executeSetUserEMode(
      _reserves,
      _reservesList,
      _eModeCategories,
      _usersEModeCategory,
      _usersConfig[_msgSender()],
      _msgSender(),
      ADDRESSES_PROVIDER.getPriceOracle(),
      categoryId
    );
  }

  /// @inheritdoc IPool
  function getUserEMode(address user) external view virtual override returns (uint256) {
    return _usersEModeCategory[user];
  }

  /// @inheritdoc IPool
  function resetIsolationModeTotalDebt(
    address asset
  ) external virtual override onlyPoolConfigurator {
    PoolLogic.executeResetIsolationModeTotalDebt(_reserves, asset);
  }

  /// @inheritdoc IPool
  function getLiquidationGracePeriod(
    address asset
  ) external view virtual override returns (uint40) {
    return _reserves[asset].liquidationGracePeriodUntil;
  }

  /// @inheritdoc IPool
  function setLiquidationGracePeriod(
    address asset,
    uint40 until
  ) external virtual override onlyPoolConfigurator {
    require(_reserves[asset].id != 0 || _reservesList[0] == asset, Errors.AssetNotListed());
    PoolLogic.executeSetLiquidationGracePeriod(_reserves, asset, until);
  }

  /// @inheritdoc IPool
  function rescueTokens(
    address token,
    address to,
    uint256 amount
  ) external virtual override onlyPoolAdmin {
    PoolLogic.executeRescueTokens(token, to, amount);
  }

  /// @inheritdoc IPool
  /// @dev Deprecated: maintained for compatibility purposes
  function deposit(
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
        user: _msgSender(),
        asset: asset,
        interestRateStrategyAddress: RESERVE_INTEREST_RATE_STRATEGY,
        amount: amount,
        onBehalfOf: onBehalfOf,
        referralCode: referralCode
      })
    );
  }

  /// @inheritdoc IPool
  function eliminateReserveDeficit(
    address asset,
    uint256 amount
  ) external override onlyUmbrella returns (uint256) {
    return
      LiquidationLogic.executeEliminateDeficit(
        _reserves,
        _usersConfig[_msgSender()],
        DataTypes.ExecuteEliminateDeficitParams({
          user: _msgSender(),
          asset: asset,
          amount: amount,
          interestRateStrategyAddress: RESERVE_INTEREST_RATE_STRATEGY
        })
      );
  }

  /// @inheritdoc IPool
  function approvePositionManager(address positionManager, bool approve) external override {
    if (_positionManager[_msgSender()][positionManager] == approve) return;
    _positionManager[_msgSender()][positionManager] = approve;

    if (approve) {
      emit PositionManagerApproved({user: _msgSender(), positionManager: positionManager});
    } else {
      emit PositionManagerRevoked({user: _msgSender(), positionManager: positionManager});
    }
  }

  /// @inheritdoc IPool
  function renouncePositionManagerRole(address user) external override {
    if (_positionManager[user][_msgSender()] == false) return;
    _positionManager[user][_msgSender()] = false;
    emit PositionManagerRevoked({user: user, positionManager: _msgSender()});
  }

  /// @inheritdoc IPool
  function setUserUseReserveAsCollateralOnBehalfOf(
    address asset,
    bool useAsCollateral,
    address onBehalfOf
  ) external override onlyPositionManager(onBehalfOf) {
    SupplyLogic.executeUseReserveAsCollateral(
      _reserves,
      _reservesList,
      _eModeCategories,
      _usersConfig[onBehalfOf],
      onBehalfOf,
      asset,
      useAsCollateral,
      ADDRESSES_PROVIDER.getPriceOracle(),
      _usersEModeCategory[onBehalfOf]
    );
  }

  /// @inheritdoc IPool
  function setUserEModeOnBehalfOf(
    uint8 categoryId,
    address onBehalfOf
  ) external override onlyPositionManager(onBehalfOf) {
    EModeLogic.executeSetUserEMode(
      _reserves,
      _reservesList,
      _eModeCategories,
      _usersEModeCategory,
      _usersConfig[onBehalfOf],
      onBehalfOf,
      ADDRESSES_PROVIDER.getPriceOracle(),
      categoryId
    );
  }

  /// @inheritdoc IPool
  function isApprovedPositionManager(
    address user,
    address positionManager
  ) external view override returns (bool) {
    return _positionManager[user][positionManager];
  }

  /// @inheritdoc IPool
  function getReserveDeficit(address asset) external view virtual returns (uint256) {
    return _reserves[asset].deficit;
  }

  /// @inheritdoc IPool
  function getReserveAToken(address asset) external view virtual returns (address) {
    return _reserves[asset].aTokenAddress;
  }

  /// @inheritdoc IPool
  function getReserveVariableDebtToken(address asset) external view virtual returns (address) {
    return _reserves[asset].variableDebtTokenAddress;
  }

  /// @inheritdoc IPool
  function getFlashLoanLogic() external pure returns (address) {
    return address(FlashLoanLogic);
  }

  /// @inheritdoc IPool
  function getBorrowLogic() external pure returns (address) {
    return address(BorrowLogic);
  }

  /// @inheritdoc IPool
  function getEModeLogic() external pure returns (address) {
    return address(EModeLogic);
  }

  /// @inheritdoc IPool
  function getLiquidationLogic() external pure returns (address) {
    return address(LiquidationLogic);
  }

  /// @inheritdoc IPool
  function getPoolLogic() external pure returns (address) {
    return address(PoolLogic);
  }

  /// @inheritdoc IPool
  function getSupplyLogic() external pure returns (address) {
    return address(SupplyLogic);
  }
}

// SPDX-License-Identifier: MIT
// OpenZeppelin Contracts (last updated v5.1.0) (utils/Address.sol)

pragma solidity ^0.8.20;

import {Errors} from "./Errors.sol";

/**
 * @dev Collection of functions related to the address type
 */
library Address {
    /**
     * @dev There's no code at `target` (it is not a contract).
     */
    error AddressEmptyCode(address target);

    /**
     * @dev Replacement for Solidity's `transfer`: sends `amount` wei to
     * `recipient`, forwarding all available gas and reverting on errors.
     *
     * https://eips.ethereum.org/EIPS/eip-1884[EIP1884] increases the gas cost
     * of certain opcodes, possibly making contracts go over the 2300 gas limit
     * imposed by `transfer`, making them unable to receive funds via
     * `transfer`. {sendValue} removes this limitation.
     *
     * https://consensys.net/diligence/blog/2019/09/stop-using-soliditys-transfer-now/[Learn more].
     *
     * IMPORTANT: because control is transferred to `recipient`, care must be
     * taken to not create reentrancy vulnerabilities. Consider using
     * {ReentrancyGuard} or the
     * https://solidity.readthedocs.io/en/v0.8.20/security-considerations.html#use-the-checks-effects-interactions-pattern[checks-effects-interactions pattern].
     */
    function sendValue(address payable recipient, uint256 amount) internal {
        if (address(this).balance < amount) {
            revert Errors.InsufficientBalance(address(this).balance, amount);
        }

        (bool success, ) = recipient.call{value: amount}("");
        if (!success) {
            revert Errors.FailedCall();
        }
    }

    /**
     * @dev Performs a Solidity function call using a low level `call`. A
     * plain `call` is an unsafe replacement for a function call: use this
     * function instead.
     *
     * If `target` reverts with a revert reason or custom error, it is bubbled
     * up by this function (like regular Solidity function calls). However, if
     * the call reverted with no returned reason, this function reverts with a
     * {Errors.FailedCall} error.
     *
     * Returns the raw returned data. To convert to the expected return value,
     * use https://solidity.readthedocs.io/en/latest/units-and-global-variables.html?highlight=abi.decode#abi-encoding-and-decoding-functions[`abi.decode`].
     *
     * Requirements:
     *
     * - `target` must be a contract.
     * - calling `target` with `data` must not revert.
     */
    function functionCall(address target, bytes memory data) internal returns (bytes memory) {
        return functionCallWithValue(target, data, 0);
    }

    /**
     * @dev Same as {xref-Address-functionCall-address-bytes-}[`functionCall`],
     * but also transferring `value` wei to `target`.
     *
     * Requirements:
     *
     * - the calling contract must have an ETH balance of at least `value`.
     * - the called Solidity function must be `payable`.
     */
    function functionCallWithValue(address target, bytes memory data, uint256 value) internal returns (bytes memory) {
        if (address(this).balance < value) {
            revert Errors.InsufficientBalance(address(this).balance, value);
        }
        (bool success, bytes memory returndata) = target.call{value: value}(data);
        return verifyCallResultFromTarget(target, success, returndata);
    }

    /**
     * @dev Same as {xref-Address-functionCall-address-bytes-}[`functionCall`],
     * but performing a static call.
     */
    function functionStaticCall(address target, bytes memory data) internal view returns (bytes memory) {
        (bool success, bytes memory returndata) = target.staticcall(data);
        return verifyCallResultFromTarget(target, success, returndata);
    }

    /**
     * @dev Same as {xref-Address-functionCall-address-bytes-}[`functionCall`],
     * but performing a delegate call.
     */
    function functionDelegateCall(address target, bytes memory data) internal returns (bytes memory) {
        (bool success, bytes memory returndata) = target.delegatecall(data);
        return verifyCallResultFromTarget(target, success, returndata);
    }

    /**
     * @dev Tool to verify that a low level call to smart-contract was successful, and reverts if the target
     * was not a contract or bubbling up the revert reason (falling back to {Errors.FailedCall}) in case
     * of an unsuccessful call.
     */
    function verifyCallResultFromTarget(
        address target,
        bool success,
        bytes memory returndata
    ) internal view returns (bytes memory) {
        if (!success) {
            _revert(returndata);
        } else {
            // only check if target is a contract if the call was successful and the return data is empty
            // otherwise we already know that it was a contract
            if (returndata.length == 0 && target.code.length == 0) {
                revert AddressEmptyCode(target);
            }
            return returndata;
        }
    }

    /**
     * @dev Tool to verify that a low level call was successful, and reverts if it wasn't, either by bubbling the
     * revert reason or with a default {Errors.FailedCall} error.
     */
    function verifyCallResult(bool success, bytes memory returndata) internal pure returns (bytes memory) {
        if (!success) {
            _revert(returndata);
        } else {
            return returndata;
        }
    }

    /**
     * @dev Reverts with returndata if present. Otherwise reverts with {Errors.FailedCall}.
     */
    function _revert(bytes memory returndata) private pure {
        // Look for revert reason and bubble it up if present
        if (returndata.length > 0) {
            // The easiest way to bubble the revert reason is using memory via assembly
            assembly ("memory-safe") {
                let returndata_size := mload(returndata)
                revert(add(32, returndata), returndata_size)
            }
        } else {
            revert Errors.FailedCall();
        }
    }
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {Errors} from '../helpers/Errors.sol';
import {IPool} from '../../../interfaces/IPool.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {UserConfiguration} from '../configuration/UserConfiguration.sol';
import {SafeCast} from 'openzeppelin-contracts/contracts/utils/math/SafeCast.sol';

/**
 * @title IsolationModeLogic library
 * @author Aave
 * @notice Implements the base logic for handling repayments for assets borrowed in isolation mode
 */
library IsolationModeLogic {
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;
  using UserConfiguration for DataTypes.UserConfigurationMap;
  using SafeCast for uint256;

  /**
   * @notice increases the isolated debt whenever user borrows against isolated collateral asset
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param userConfig The user configuration mapping
   * @param reserveCache The cached data of the reserve
   * @param borrowAmount The amount being borrowed
   */
  function increaseIsolatedDebtIfIsolated(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    DataTypes.UserConfigurationMap storage userConfig,
    DataTypes.ReserveCache memory reserveCache,
    uint256 borrowAmount
  ) internal {
    (
      bool isolationModeActive,
      address isolationModeCollateralAddress,
      uint256 isolationModeDebtCeiling
    ) = userConfig.getIsolationModeState(reservesData, reservesList);

    if (isolationModeActive) {
      // check that the asset being borrowed is borrowable in isolation mode AND
      // the total exposure is no bigger than the collateral debt ceiling
      require(
        reserveCache.reserveConfiguration.getBorrowableInIsolation(),
        Errors.AssetNotBorrowableInIsolation()
      );

      uint128 nextIsolationModeTotalDebt = reservesData[isolationModeCollateralAddress]
        .isolationModeTotalDebt + convertToIsolatedDebtUnits(reserveCache, borrowAmount);

      require(nextIsolationModeTotalDebt <= isolationModeDebtCeiling, Errors.DebtCeilingExceeded());

      setIsolationModeTotalDebt(
        reservesData[isolationModeCollateralAddress],
        isolationModeCollateralAddress,
        nextIsolationModeTotalDebt
      );
    }
  }

  /**
   * @notice updated the isolated debt whenever a position collateralized by an isolated asset is repaid
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param userConfig The user configuration mapping
   * @param reserveCache The cached data of the reserve
   * @param repayAmount The amount being repaid
   */
  function reduceIsolatedDebtIfIsolated(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    DataTypes.UserConfigurationMap storage userConfig,
    DataTypes.ReserveCache memory reserveCache,
    uint256 repayAmount
  ) internal {
    (bool isolationModeActive, address isolationModeCollateralAddress, ) = userConfig
      .getIsolationModeState(reservesData, reservesList);

    if (isolationModeActive) {
      updateIsolatedDebt(reservesData, reserveCache, repayAmount, isolationModeCollateralAddress);
    }
  }

  /**
   * @notice updated the isolated debt whenever a position collateralized by an isolated asset is liquidated
   * @param reservesData The state of all the reserves
   * @param reserveCache The cached data of the reserve
   * @param repayAmount The amount being repaid
   * @param isolationModeCollateralAddress The address of the isolated collateral
   */
  function updateIsolatedDebt(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    DataTypes.ReserveCache memory reserveCache,
    uint256 repayAmount,
    address isolationModeCollateralAddress
  ) internal {
    uint128 isolationModeTotalDebt = reservesData[isolationModeCollateralAddress]
      .isolationModeTotalDebt;

    uint128 isolatedDebtRepaid = convertToIsolatedDebtUnits(reserveCache, repayAmount);

    // since the debt ceiling does not take into account the interest accrued, it might happen that amount
    // repaid > debt in isolation mode
    uint128 newIsolationModeTotalDebt = isolationModeTotalDebt > isolatedDebtRepaid
      ? isolationModeTotalDebt - isolatedDebtRepaid
      : 0;
    setIsolationModeTotalDebt(
      reservesData[isolationModeCollateralAddress],
      isolationModeCollateralAddress,
      newIsolationModeTotalDebt
    );
  }

  /**
   * @notice Sets the isolation mode total debt of the given asset to a certain value
   * @param reserveData The state of the reserve
   * @param isolationModeCollateralAddress The address of the isolation mode collateral
   * @param newIsolationModeTotalDebt The new isolation mode total debt
   */
  function setIsolationModeTotalDebt(
    DataTypes.ReserveData storage reserveData,
    address isolationModeCollateralAddress,
    uint128 newIsolationModeTotalDebt
  ) internal {
    reserveData.isolationModeTotalDebt = newIsolationModeTotalDebt;

    emit IPool.IsolationModeTotalDebtUpdated(
      isolationModeCollateralAddress,
      newIsolationModeTotalDebt
    );
  }

  /**
   * @notice utility function to convert an amount into the isolated debt units, which usually has less decimals
   * @param reserveCache The cached data of the reserve
   * @param amount The amount being added or removed from isolated debt
   */
  function convertToIsolatedDebtUnits(
    DataTypes.ReserveCache memory reserveCache,
    uint256 amount
  ) private pure returns (uint128) {
    return
      (amount /
        10 **
          (reserveCache.reserveConfiguration.getDecimals() -
            ReserveConfiguration.DEBT_CEILING_DECIMALS)).toUint128();
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IScaledBalanceToken} from './IScaledBalanceToken.sol';
import {IInitializableDebtToken} from './IInitializableDebtToken.sol';

/**
 * @title IVariableDebtToken
 * @author Aave
 * @notice Defines the basic interface for a variable debt token.
 */
interface IVariableDebtToken is IScaledBalanceToken, IInitializableDebtToken {
  /**
   * @notice Mints debt token to the `onBehalfOf` address.
   * @dev Passing both the unscaled and scaled amounts enhances precision. The `scaledAmount` is used for precise balance updates,
   * while the `amount` is used for allowance checks, preventing cumulative rounding errors.
   * @param user The address receiving the borrowed underlying, being the delegatee in case
   * of credit delegate, or same as `onBehalfOf` otherwise
   * @param onBehalfOf The address receiving the debt tokens
   * @param amount The unscaled amount of debt to be accounted for allowance
   * @param scaledAmount The scaled amount of debt tokens to mint
   * @param index The variable debt index of the reserve
   * @return The scaled total debt of the reserve
   */
  function mint(
    address user,
    address onBehalfOf,
    uint256 amount,
    uint256 scaledAmount,
    uint256 index
  ) external returns (uint256);

  /**
   * @notice Burns user variable debt.
   * @dev Passing the scaled amount allows for more precise calculations and avoids cumulative errors from repeated conversions.
   * @dev In some instances, a burn transaction will emit a mint event if the amount to burn is less than the interest that the user accrued.
   * @param from The address from which the debt will be burned
   * @param scaledAmount The scaled amount of debt getting burned
   * @param index The variable debt index of the reserve
   * @return True if the new balance is zero
   * @return The scaled total debt of the reserve
   */
  function burn(address from, uint256 scaledAmount, uint256 index) external returns (bool, uint256);

  /**
   * @notice Returns the address of the underlying asset of this debtToken (E.g. WETH for variableDebtWETH)
   * @return The address of the underlying asset
   */
  function UNDERLYING_ASSET_ADDRESS() external view returns (address);
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {IVariableDebtToken} from '../../../interfaces/IVariableDebtToken.sol';
import {IReserveInterestRateStrategy} from '../../../interfaces/IReserveInterestRateStrategy.sol';
import {IPool} from '../../../interfaces/IPool.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {MathUtils} from '../math/MathUtils.sol';
import {WadRayMath} from '../math/WadRayMath.sol';
import {PercentageMath} from '../math/PercentageMath.sol';
import {Errors} from '../helpers/Errors.sol';
import {TokenMath} from '../helpers/TokenMath.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {SafeCast} from 'openzeppelin-contracts/contracts/utils/math/SafeCast.sol';

/**
 * @title ReserveLogic library
 * @author Aave
 * @notice Implements the logic to update the reserves state
 */
library ReserveLogic {
  using WadRayMath for uint256;
  using TokenMath for uint256;
  using PercentageMath for uint256;
  using SafeCast for uint256;
  using GPv2SafeERC20 for IERC20;
  using ReserveLogic for DataTypes.ReserveData;
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;

  /**
   * @notice Returns the ongoing normalized income for the reserve.
   * @dev A value of 1e27 means there is no income. As time passes, the income is accrued
   * @dev A value of 2*1e27 means for each unit of asset one unit of income has been accrued
   * @param reserve The reserve object
   * @return The normalized income, expressed in ray
   */
  function getNormalizedIncome(
    DataTypes.ReserveData storage reserve
  ) internal view returns (uint256) {
    uint40 timestamp = reserve.lastUpdateTimestamp;

    //solium-disable-next-line
    if (timestamp == block.timestamp) {
      //if the index was updated in the same block, no need to perform any calculation
      return reserve.liquidityIndex;
    } else {
      return
        MathUtils.calculateLinearInterest(reserve.currentLiquidityRate, timestamp).rayMul(
          reserve.liquidityIndex
        );
    }
  }

  /**
   * @notice Returns the ongoing normalized variable debt for the reserve.
   * @dev A value of 1e27 means there is no debt. As time passes, the debt is accrued
   * @dev A value of 2*1e27 means that for each unit of debt, one unit worth of interest has been accumulated
   * @param reserve The reserve object
   * @return The normalized variable debt, expressed in ray
   */
  function getNormalizedDebt(
    DataTypes.ReserveData storage reserve
  ) internal view returns (uint256) {
    uint40 timestamp = reserve.lastUpdateTimestamp;

    //solium-disable-next-line
    if (timestamp == block.timestamp) {
      //if the index was updated in the same block, no need to perform any calculation
      return reserve.variableBorrowIndex;
    } else {
      return
        MathUtils.calculateCompoundedInterest(reserve.currentVariableBorrowRate, timestamp).rayMul(
          reserve.variableBorrowIndex
        );
    }
  }

  /**
   * @notice Updates the liquidity cumulative index, the variable borrow index and the timestamp of the update.
   * @param reserve The reserve object
   * @param reserveCache The caching layer for the reserve data
   */
  function updateState(
    DataTypes.ReserveData storage reserve,
    DataTypes.ReserveCache memory reserveCache
  ) internal {
    // If time didn't pass since last stored timestamp, skip state update
    //solium-disable-next-line
    if (reserveCache.reserveLastUpdateTimestamp == uint40(block.timestamp)) {
      return;
    }

    _updateIndexes(reserve, reserveCache);
    _accrueToTreasury(reserve, reserveCache);

    //solium-disable-next-line
    reserve.lastUpdateTimestamp = uint40(block.timestamp);
    reserveCache.reserveLastUpdateTimestamp = uint40(block.timestamp);
  }

  /**
   * @notice Initializes a reserve.
   * @param reserve The reserve object
   * @param aTokenAddress The address of the overlying atoken contract
   * @param variableDebtTokenAddress The address of the overlying variable debt token contract
   */
  function init(
    DataTypes.ReserveData storage reserve,
    address aTokenAddress,
    address variableDebtTokenAddress
  ) internal {
    require(reserve.aTokenAddress == address(0), Errors.ReserveAlreadyInitialized());

    reserve.liquidityIndex = uint128(WadRayMath.RAY);
    reserve.variableBorrowIndex = uint128(WadRayMath.RAY);
    reserve.aTokenAddress = aTokenAddress;
    reserve.variableDebtTokenAddress = variableDebtTokenAddress;
  }

  /**
   * @notice Updates the reserve current variable borrow rate and the current liquidity rate.
   * @param reserve The reserve reserve to be updated
   * @param reserveCache The caching layer for the reserve data
   * @param reserveAddress The address of the reserve to be updated
   * @param liquidityAdded The amount of liquidity added to the protocol (supply or repay) in the previous action
   * @param liquidityTaken The amount of liquidity taken from the protocol (redeem or borrow)
   */
  function updateInterestRatesAndVirtualBalance(
    DataTypes.ReserveData storage reserve,
    DataTypes.ReserveCache memory reserveCache,
    address reserveAddress,
    uint256 liquidityAdded,
    uint256 liquidityTaken,
    address interestRateStrategyAddress
  ) internal {
    uint256 totalVariableDebt = reserveCache.nextScaledVariableDebt.getVTokenBalance(
      reserveCache.nextVariableBorrowIndex
    );

    (uint256 nextLiquidityRate, uint256 nextVariableRate) = IReserveInterestRateStrategy(
      interestRateStrategyAddress
    ).calculateInterestRates(
        DataTypes.CalculateInterestRatesParams({
          unbacked: reserve.deficit,
          liquidityAdded: liquidityAdded,
          liquidityTaken: liquidityTaken,
          totalDebt: totalVariableDebt,
          reserveFactor: reserveCache.reserveFactor,
          reserve: reserveAddress,
          usingVirtualBalance: true,
          virtualUnderlyingBalance: reserve.virtualUnderlyingBalance
        })
      );

    reserve.currentLiquidityRate = nextLiquidityRate.toUint128();
    reserve.currentVariableBorrowRate = nextVariableRate.toUint128();

    if (liquidityAdded > 0) {
      reserve.virtualUnderlyingBalance += liquidityAdded.toUint128();
    }
    if (liquidityTaken > 0) {
      reserve.virtualUnderlyingBalance -= liquidityTaken.toUint128();
    }

    emit IPool.ReserveDataUpdated(
      reserveAddress,
      nextLiquidityRate,
      0,
      nextVariableRate,
      reserveCache.nextLiquidityIndex,
      reserveCache.nextVariableBorrowIndex
    );
  }

  /**
   * @notice Mints part of the repaid interest to the reserve treasury as a function of the reserve factor for the
   * specific asset.
   * @param reserve The reserve to be updated
   * @param reserveCache The caching layer for the reserve data
   */
  function _accrueToTreasury(
    DataTypes.ReserveData storage reserve,
    DataTypes.ReserveCache memory reserveCache
  ) internal {
    if (reserveCache.reserveFactor == 0) {
      return;
    }

    // debt accrued is the sum of the current debt minus the sum of the debt at the last update
    // Rounding down to undermint to the treasury and keep the invariant healthy.
    uint256 totalDebtAccrued = reserveCache.currScaledVariableDebt.rayMulFloor(
      reserveCache.nextVariableBorrowIndex - reserveCache.currVariableBorrowIndex
    );

    uint256 amountToMint = totalDebtAccrued.percentMul(reserveCache.reserveFactor);

    if (amountToMint != 0) {
      reserve.accruedToTreasury += amountToMint
        .getATokenMintScaledAmount(reserveCache.nextLiquidityIndex)
        .toUint128();
    }
  }

  /**
   * @notice Updates the reserve indexes.
   * @param reserve The reserve reserve to be updated
   * @param reserveCache The cache layer holding the cached protocol data
   */
  function _updateIndexes(
    DataTypes.ReserveData storage reserve,
    DataTypes.ReserveCache memory reserveCache
  ) internal {
    // Only cumulating on the supply side if there is any income being produced
    // The case of Reserve Factor 100% is not a problem (currentLiquidityRate == 0),
    // as liquidity index should not be updated
    if (reserveCache.currLiquidityRate != 0) {
      uint256 cumulatedLiquidityInterest = MathUtils.calculateLinearInterest(
        reserveCache.currLiquidityRate,
        reserveCache.reserveLastUpdateTimestamp
      );
      reserveCache.nextLiquidityIndex = cumulatedLiquidityInterest.rayMul(
        reserveCache.currLiquidityIndex
      );
      reserve.liquidityIndex = reserveCache.nextLiquidityIndex.toUint128();
    }

    // Variable borrow index only gets updated if there is any variable debt.
    // reserveCache.currVariableBorrowRate != 0 is not a correct validation,
    // because a positive base variable rate can be stored on
    // reserveCache.currVariableBorrowRate, but the index should not increase
    if (reserveCache.currScaledVariableDebt != 0) {
      uint256 cumulatedVariableBorrowInterest = MathUtils.calculateCompoundedInterest(
        reserveCache.currVariableBorrowRate,
        reserveCache.reserveLastUpdateTimestamp
      );
      reserveCache.nextVariableBorrowIndex = cumulatedVariableBorrowInterest.rayMul(
        reserveCache.currVariableBorrowIndex
      );
      reserve.variableBorrowIndex = reserveCache.nextVariableBorrowIndex.toUint128();
    }
  }

  /**
   * @notice Creates a cache object to avoid repeated storage reads and external contract calls when updating state and
   * interest rates.
   * @param reserve The reserve object for which the cache will be filled
   * @return The cache object
   */
  function cache(
    DataTypes.ReserveData storage reserve
  ) internal view returns (DataTypes.ReserveCache memory) {
    DataTypes.ReserveCache memory reserveCache;

    reserveCache.reserveConfiguration = reserve.configuration;
    reserveCache.reserveFactor = reserveCache.reserveConfiguration.getReserveFactor();
    reserveCache.currLiquidityIndex = reserveCache.nextLiquidityIndex = reserve.liquidityIndex;
    reserveCache.currVariableBorrowIndex = reserveCache.nextVariableBorrowIndex = reserve
      .variableBorrowIndex;
    reserveCache.currLiquidityRate = reserve.currentLiquidityRate;
    reserveCache.currVariableBorrowRate = reserve.currentVariableBorrowRate;

    reserveCache.aTokenAddress = reserve.aTokenAddress;
    reserveCache.variableDebtTokenAddress = reserve.variableDebtTokenAddress;

    reserveCache.reserveLastUpdateTimestamp = reserve.lastUpdateTimestamp;

    reserveCache.currScaledVariableDebt = reserveCache.nextScaledVariableDebt = IVariableDebtToken(
      reserveCache.variableDebtTokenAddress
    ).scaledTotalSupply();

    return reserveCache;
  }
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.0;

/**
 * @title PercentageMath library
 * @author Aave
 * @notice Provides functions to perform percentage calculations
 * @dev Percentages are defined by default with 2 decimals of precision (100.00). The precision is indicated by PERCENTAGE_FACTOR
 * @dev Operations are rounded. If a value is >=.5, will be rounded up, otherwise rounded down.
 */
library PercentageMath {
  // Maximum percentage factor (100.00%)
  uint256 internal constant PERCENTAGE_FACTOR = 1e4;

  // Half percentage factor (50.00%)
  uint256 internal constant HALF_PERCENTAGE_FACTOR = 0.5e4;

  /**
   * @notice Executes a percentage multiplication
   * @dev assembly optimized for improved gas savings, see https://twitter.com/transmissions11/status/1451131036377571328
   * @param value The value of which the percentage needs to be calculated
   * @param percentage The percentage of the value to be calculated
   * @return result value percentmul percentage
   */
  function percentMul(uint256 value, uint256 percentage) internal pure returns (uint256 result) {
    // to avoid overflow, value <= (type(uint256).max - HALF_PERCENTAGE_FACTOR) / percentage
    assembly {
      if iszero(
        or(
          iszero(percentage),
          iszero(gt(value, div(sub(not(0), HALF_PERCENTAGE_FACTOR), percentage)))
        )
      ) {
        revert(0, 0)
      }

      result := div(add(mul(value, percentage), HALF_PERCENTAGE_FACTOR), PERCENTAGE_FACTOR)
    }
  }

  function percentMulCeil(
    uint256 value,
    uint256 percentage
  ) internal pure returns (uint256 result) {
    // to avoid overflow, value <= type(uint256).max / percentage
    assembly {
      if iszero(or(iszero(percentage), iszero(gt(value, div(not(0), percentage))))) {
        revert(0, 0)
      }

      let product := mul(value, percentage)
      result := add(
        div(product, PERCENTAGE_FACTOR),
        iszero(iszero(mod(product, PERCENTAGE_FACTOR)))
      )
    }
  }

  function percentMulFloor(
    uint256 value,
    uint256 percentage
  ) internal pure returns (uint256 result) {
    // to avoid overflow, value <= type(uint256).max / percentage
    assembly {
      if iszero(or(iszero(percentage), iszero(gt(value, div(not(0), percentage))))) {
        revert(0, 0)
      }

      result := div(mul(value, percentage), PERCENTAGE_FACTOR)
    }
  }

  /**
   * @notice Executes a percentage division
   * @dev assembly optimized for improved gas savings, see https://twitter.com/transmissions11/status/1451131036377571328
   * @param value The value of which the percentage needs to be calculated
   * @param percentage The percentage of the value to be calculated
   * @return result value percentdiv percentage
   */
  function percentDiv(uint256 value, uint256 percentage) internal pure returns (uint256 result) {
    // to avoid overflow, value <= (type(uint256).max - halfPercentage) / PERCENTAGE_FACTOR
    assembly {
      if or(
        iszero(percentage),
        iszero(iszero(gt(value, div(sub(not(0), div(percentage, 2)), PERCENTAGE_FACTOR))))
      ) {
        revert(0, 0)
      }

      result := div(add(mul(value, PERCENTAGE_FACTOR), div(percentage, 2)), percentage)
    }
  }

  function percentDivCeil(
    uint256 value,
    uint256 percentage
  ) internal pure returns (uint256 result) {
    // to avoid overflow, value <= type(uint256).max / PERCENTAGE_FACTOR
    assembly {
      if or(iszero(percentage), iszero(iszero(gt(value, div(not(0), PERCENTAGE_FACTOR))))) {
        revert(0, 0)
      }
      let val := mul(value, PERCENTAGE_FACTOR)
      result := add(div(val, percentage), iszero(iszero(mod(val, percentage))))
    }
  }
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.0;

import {WadRayMath} from './WadRayMath.sol';

/**
 * @title MathUtils library
 * @author Aave
 * @notice Provides functions to perform linear and compounded interest calculations
 */
library MathUtils {
  using WadRayMath for uint256;

  /// @dev Ignoring leap years
  uint256 internal constant SECONDS_PER_YEAR = 365 days;

  /**
   * @dev Function to calculate the interest accumulated using a linear interest rate formula
   * @param rate The interest rate, in ray
   * @param lastUpdateTimestamp The timestamp of the last update of the interest
   * @return The interest rate linearly accumulated during the timeDelta, in ray
   */
  function calculateLinearInterest(
    uint256 rate,
    uint40 lastUpdateTimestamp
  ) internal view returns (uint256) {
    //solium-disable-next-line
    uint256 result = rate * (block.timestamp - uint256(lastUpdateTimestamp));
    unchecked {
      result = result / SECONDS_PER_YEAR;
    }

    return WadRayMath.RAY + result;
  }

  /**
   * @dev Function to calculate the interest using a compounded interest rate formula
   * To avoid expensive exponentiation, the calculation is performed using a binomial approximation:
   *
   *  (1+x)^n = 1+n*x+[n/2*(n-1)]*x^2+[n/6*(n-1)*(n-2)*x^3...
   *
   * The approximation slightly underpays liquidity providers and undercharges borrowers, with the advantage of great
   * gas cost reductions. The whitepaper contains reference to the approximation and a table showing the margin of
   * error per different time periods
   *
   * @param rate The interest rate, in ray
   * @param lastUpdateTimestamp The timestamp of the last update of the interest
   * @return The interest rate compounded during the timeDelta, in ray
   */
  function calculateCompoundedInterest(
    uint256 rate,
    uint40 lastUpdateTimestamp,
    uint256 currentTimestamp
  ) internal pure returns (uint256) {
    //solium-disable-next-line
    uint256 exp = currentTimestamp - uint256(lastUpdateTimestamp);

    if (exp == 0) {
      return WadRayMath.RAY;
    }

    // calculations compound interest using the ideal formula - e^(rate per year * number of years)
    // 100_000% per year = 1_000 * 100, passed 10_000 years:
    // e^(1_000 * 10_000) = 6.5922325346184394895608861310659088446667722661221381641234330770... × 10^4342944

    // The current formula in the contract returns:
    // 1.66666716666676666667 × 10^20
    // This happens because the contract uses a polynomial approximation of the ideal formula
    // and on big numbers the ideal formula with exponential function has much more speed.
    // Used approximation in contracts is not precise enough on such big numbers.
    //
    // But we can be sure that the current formula in contracts can't overflow on such big numbers
    // and we can use unchecked arithmetics to save gas.
    //
    // Also, if we take into an account the fact that all timestamps are stored in uint32/40 types
    // we can only have 100 years left until we will have overflows in timestamps.
    // Because of that realistically we can't overflow in this formula.

    unchecked {
      // this can't overflow because rate is always fits in 128 bits and exp always fits in 40 bits
      uint256 x = (rate * exp) / SECONDS_PER_YEAR;

      return WadRayMath.RAY + x + x.rayMul(x / 2 + x.rayMul(x / 6));
    }
  }

  /**
   * @dev Calculates the compounded interest between the timestamp of the last update and the current block timestamp
   * @param rate The interest rate (in ray)
   * @param lastUpdateTimestamp The timestamp from which the interest accumulation needs to be calculated
   * @return The interest rate compounded between lastUpdateTimestamp and current block timestamp, in ray
   */
  function calculateCompoundedInterest(
    uint256 rate,
    uint40 lastUpdateTimestamp
  ) internal view returns (uint256) {
    return calculateCompoundedInterest(rate, lastUpdateTimestamp, block.timestamp);
  }

  function mulDivCeil(uint256 a, uint256 b, uint256 c) internal pure returns (uint256 d) {
    assembly {
      // Revert if c == 0 to avoid division by zero
      if iszero(c) {
        revert(0, 0)
      }

      // Overflow check: Ensure a * b does not exceed uint256 max
      if iszero(or(iszero(b), iszero(gt(a, div(not(0), b))))) {
        revert(0, 0)
      }

      let product := mul(a, b)
      d := add(div(product, c), iszero(iszero(mod(product, c))))
    }
  }
}

// SPDX-License-Identifier: MIT
// OpenZeppelin Contracts (last updated v5.1.0) (utils/Errors.sol)

pragma solidity ^0.8.20;

/**
 * @dev Collection of common custom errors used in multiple contracts
 *
 * IMPORTANT: Backwards compatibility is not guaranteed in future versions of the library.
 * It is recommended to avoid relying on the error API for critical functionality.
 *
 * _Available since v5.1._
 */
library Errors {
    /**
     * @dev The ETH balance of the account is not enough to perform the operation.
     */
    error InsufficientBalance(uint256 balance, uint256 needed);

    /**
     * @dev A call to an address target failed. The target may have reverted.
     */
    error FailedCall();

    /**
     * @dev The deployment failed.
     */
    error FailedDeployment();

    /**
     * @dev A necessary precompile is missing.
     */
    error MissingPrecompile(address);
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IAaveIncentivesController} from './IAaveIncentivesController.sol';
import {IPool} from './IPool.sol';

/**
 * @title IInitializableAToken
 * @author Aave
 * @notice Interface for the initialize function on AToken
 */
interface IInitializableAToken {
  /**
   * @dev Emitted when an aToken is initialized
   * @param underlyingAsset The address of the underlying asset
   * @param pool The address of the associated pool
   * @param treasury The address of the treasury
   * @param incentivesController The address of the incentives controller for this aToken
   * @param aTokenDecimals The decimals of the underlying
   * @param aTokenName The name of the aToken
   * @param aTokenSymbol The symbol of the aToken
   * @param params A set of encoded parameters for additional initialization
   */
  event Initialized(
    address indexed underlyingAsset,
    address indexed pool,
    address treasury,
    address incentivesController,
    uint8 aTokenDecimals,
    string aTokenName,
    string aTokenSymbol,
    bytes params
  );

  /**
   * @notice Initializes the aToken
   * @param pool The pool contract that is initializing this contract
   * @param underlyingAsset The address of the underlying asset of this aToken (E.g. WETH for aWETH)
   * @param aTokenDecimals The decimals of the aToken, same as the underlying asset's
   * @param aTokenName The name of the aToken
   * @param aTokenSymbol The symbol of the aToken
   * @param params A set of encoded parameters for additional initialization
   */
  function initialize(
    IPool pool,
    address underlyingAsset,
    uint8 aTokenDecimals,
    string calldata aTokenName,
    string calldata aTokenSymbol,
    bytes calldata params
  ) external;
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

/**
 * @dev Interface of the ERC20 standard as defined in the EIP.
 */
interface IERC20 {
  /**
   * @dev Returns the amount of tokens in existence.
   */
  function totalSupply() external view returns (uint256);

  /**
   * @dev Returns the amount of tokens owned by `account`.
   */
  function balanceOf(address account) external view returns (uint256);

  /**
   * @dev Moves `amount` tokens from the caller's account to `recipient`.
   *
   * Returns a boolean value indicating whether the operation succeeded.
   *
   * Emits a {Transfer} event.
   */
  function transfer(address recipient, uint256 amount) external returns (bool);

  /**
   * @dev Returns the remaining number of tokens that `spender` will be
   * allowed to spend on behalf of `owner` through {transferFrom}. This is
   * zero by default.
   *
   * This value changes when {approve} or {transferFrom} are called.
   */
  function allowance(address owner, address spender) external view returns (uint256);

  /**
   * @dev Sets `amount` as the allowance of `spender` over the caller's tokens.
   *
   * Returns a boolean value indicating whether the operation succeeded.
   *
   * IMPORTANT: Beware that changing an allowance with this method brings the risk
   * that someone may use both the old and the new allowance by unfortunate
   * transaction ordering. One possible solution to mitigate this race
   * condition is to first reduce the spender's allowance to 0 and set the
   * desired value afterwards:
   * https://github.com/ethereum/EIPs/issues/20#issuecomment-263524729
   *
   * Emits an {Approval} event.
   */
  function approve(address spender, uint256 amount) external returns (bool);

  /**
   * @dev Moves `amount` tokens from `sender` to `recipient` using the
   * allowance mechanism. `amount` is then deducted from the caller's
   * allowance.
   *
   * Returns a boolean value indicating whether the operation succeeded.
   *
   * Emits a {Transfer} event.
   */
  function transferFrom(address sender, address recipient, uint256 amount) external returns (bool);

  /**
   * @dev Emitted when `value` tokens are moved from one account (`from`) to
   * another (`to`).
   *
   * Note that `value` may be zero.
   */
  event Transfer(address indexed from, address indexed to, uint256 value);

  /**
   * @dev Emitted when the allowance of a `spender` for an `owner` is set by
   * a call to {approve}. `value` is the new allowance.
   */
  event Approval(address indexed owner, address indexed spender, uint256 value);
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IPoolAddressesProvider} from './IPoolAddressesProvider.sol';

/**
 * @title IPriceOracleSentinel
 * @author Aave
 * @notice Defines the basic interface for the PriceOracleSentinel
 */
interface IPriceOracleSentinel {
  /**
   * @dev Emitted after the sequencer oracle is updated
   * @param newSequencerOracle The new sequencer oracle
   */
  event SequencerOracleUpdated(address newSequencerOracle);

  /**
   * @dev Emitted after the grace period is updated
   * @param newGracePeriod The new grace period value
   */
  event GracePeriodUpdated(uint256 newGracePeriod);

  /**
   * @notice Returns the PoolAddressesProvider
   * @return The address of the PoolAddressesProvider contract
   */
  function ADDRESSES_PROVIDER() external view returns (IPoolAddressesProvider);

  /**
   * @notice Returns true if the `borrow` operation is allowed.
   * @dev Operation not allowed when PriceOracle is down or grace period not passed.
   * @return True if the `borrow` operation is allowed, false otherwise.
   */
  function isBorrowAllowed() external view returns (bool);

  /**
   * @notice Returns true if the `liquidation` operation is allowed.
   * @dev Operation not allowed when PriceOracle is down or grace period not passed.
   * @return True if the `liquidation` operation is allowed, false otherwise.
   */
  function isLiquidationAllowed() external view returns (bool);

  /**
   * @notice Updates the address of the sequencer oracle
   * @param newSequencerOracle The address of the new Sequencer Oracle to use
   */
  function setSequencerOracle(address newSequencerOracle) external;

  /**
   * @notice Updates the duration of the grace period
   * @param newGracePeriod The value of the new grace period duration
   */
  function setGracePeriod(uint256 newGracePeriod) external;

  /**
   * @notice Returns the SequencerOracle
   * @return The address of the sequencer oracle contract
   */
  function getSequencerOracle() external view returns (address);

  /**
   * @notice Returns the grace period
   * @return The duration of the grace period
   */
  function getGracePeriod() external view returns (uint256);
}

// SPDX-License-Identifier: MIT

pragma solidity ^0.8.10;

/**
 * @dev External interface of AccessControl declared to support ERC165 detection.
 */
interface IAccessControl {
  /**
   * @dev Emitted when `newAdminRole` is set as ``role``'s admin role, replacing `previousAdminRole`
   *
   * `DEFAULT_ADMIN_ROLE` is the starting admin for all roles, despite
   * {RoleAdminChanged} not being emitted signaling this.
   *
   * _Available since v3.1._
   */
  event RoleAdminChanged(
    bytes32 indexed role,
    bytes32 indexed previousAdminRole,
    bytes32 indexed newAdminRole
  );

  /**
   * @dev Emitted when `account` is granted `role`.
   *
   * `sender` is the account that originated the contract call, an admin role
   * bearer except when using {AccessControl-_setupRole}.
   */
  event RoleGranted(bytes32 indexed role, address indexed account, address indexed sender);

  /**
   * @dev Emitted when `account` is revoked `role`.
   *
   * `sender` is the account that originated the contract call:
   *   - if using `revokeRole`, it is the admin role bearer
   *   - if using `renounceRole`, it is the role bearer (i.e. `account`)
   */
  event RoleRevoked(bytes32 indexed role, address indexed account, address indexed sender);

  /**
   * @dev Returns `true` if `account` has been granted `role`.
   */
  function hasRole(bytes32 role, address account) external view returns (bool);

  /**
   * @dev Returns the admin role that controls `role`. See {grantRole} and
   * {revokeRole}.
   *
   * To change a role's admin, use {AccessControl-_setRoleAdmin}.
   */
  function getRoleAdmin(bytes32 role) external view returns (bytes32);

  /**
   * @dev Grants `role` to `account`.
   *
   * If `account` had not been already granted `role`, emits a {RoleGranted}
   * event.
   *
   * Requirements:
   *
   * - the caller must have ``role``'s admin role.
   */
  function grantRole(bytes32 role, address account) external;

  /**
   * @dev Revokes `role` from `account`.
   *
   * If `account` had been granted `role`, emits a {RoleRevoked} event.
   *
   * Requirements:
   *
   * - the caller must have ``role``'s admin role.
   */
  function revokeRole(bytes32 role, address account) external;

  /**
   * @dev Revokes `role` from the calling account.
   *
   * Roles are often managed via {grantRole} and {revokeRole}: this function's
   * purpose is to provide a mechanism for accounts to lose their privileges
   * if they are compromised (such as when a trusted device is misplaced).
   *
   * If the calling account had been granted `role`, emits a {RoleRevoked}
   * event.
   *
   * Requirements:
   *
   * - the caller must be `account`.
   */
  function renounceRole(bytes32 role, address account) external;
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.10;

/*
 * @dev Provides information about the current execution context, including the
 * sender of the transaction and its data. While these are generally available
 * via msg.sender and msg.data, they should not be accessed in such a direct
 * manner, since when dealing with GSN meta-transactions the account sending and
 * paying for execution may not be the actual sender (as far as an application
 * is concerned).
 *
 * This contract is only required for intermediate, library-like contracts.
 */
abstract contract Context {
  function _msgSender() internal view virtual returns (address payable) {
    return payable(msg.sender);
  }

  function _msgData() internal view virtual returns (bytes memory) {
    this; // silence state mutability warning without generating bytecode - see https://github.com/ethereum/solidity/issues/2691
    return msg.data;
  }
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {SafeCast} from 'openzeppelin-contracts/contracts/utils/math/SafeCast.sol';
import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {IPool} from '../../../interfaces/IPool.sol';
import {IFlashLoanReceiver} from '../../../misc/flashloan/interfaces/IFlashLoanReceiver.sol';
import {IFlashLoanSimpleReceiver} from '../../../misc/flashloan/interfaces/IFlashLoanSimpleReceiver.sol';
import {IPoolAddressesProvider} from '../../../interfaces/IPoolAddressesProvider.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {Errors} from '../helpers/Errors.sol';
import {TokenMath} from '../helpers/TokenMath.sol';
import {PercentageMath} from '../math/PercentageMath.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {ValidationLogic} from './ValidationLogic.sol';
import {BorrowLogic} from './BorrowLogic.sol';
import {ReserveLogic} from './ReserveLogic.sol';

/**
 * @title FlashLoanLogic library
 * @author Aave
 * @notice Implements the logic for the flash loans
 */
library FlashLoanLogic {
  using ReserveLogic for DataTypes.ReserveCache;
  using ReserveLogic for DataTypes.ReserveData;
  using GPv2SafeERC20 for IERC20;
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;
  using TokenMath for uint256;
  using PercentageMath for uint256;
  using SafeCast for uint256;

  // Helper struct for internal variables used in the `executeFlashLoan` function
  struct FlashLoanLocalVars {
    IFlashLoanReceiver receiver;
    address currentAsset;
    uint256 currentAmount;
    uint256[] totalPremiums;
    uint256 flashloanPremium;
  }

  /**
   * @notice Implements the flashloan feature that allow users to access liquidity of the pool for one transaction
   * as long as the amount taken plus fee is returned or debt is opened.
   * @dev For authorized flashborrowers the fee is waived
   * @dev At the end of the transaction the pool will pull amount borrowed + fee from the receiver,
   * if the receiver have not approved the pool the transaction will revert.
   * @dev Emits the `FlashLoan()` event
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param userConfig The user configuration mapping that tracks the supplied/borrowed assets
   * @param params The additional parameters needed to execute the flashloan function
   */
  function executeFlashLoan(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.UserConfigurationMap storage userConfig,
    DataTypes.FlashloanParams memory params
  ) external {
    // The usual action flow (cache -> updateState -> validation -> changeState -> updateRates)
    // is altered to (validation -> user payload -> cache -> updateState -> changeState -> updateRates) for flashloans.
    // This is done to protect against reentrance and rate manipulation within the user specified payload.

    ValidationLogic.validateFlashloan(reservesData, params.assets, params.amounts);

    FlashLoanLocalVars memory vars;

    vars.totalPremiums = new uint256[](params.assets.length);

    vars.receiver = IFlashLoanReceiver(params.receiverAddress);
    vars.flashloanPremium = params.isAuthorizedFlashBorrower ? 0 : params.flashLoanPremium;

    for (uint256 i = 0; i < params.assets.length; i++) {
      vars.currentAmount = params.amounts[i];
      vars.totalPremiums[i] = DataTypes.InterestRateMode(params.interestRateModes[i]) ==
        DataTypes.InterestRateMode.NONE
        ? vars.currentAmount.percentMulCeil(vars.flashloanPremium)
        : 0;

      reservesData[params.assets[i]].virtualUnderlyingBalance -= vars.currentAmount.toUint128();

      IAToken(reservesData[params.assets[i]].aTokenAddress).transferUnderlyingTo(
        params.receiverAddress,
        vars.currentAmount
      );
    }

    require(
      vars.receiver.executeOperation(
        params.assets,
        params.amounts,
        vars.totalPremiums,
        params.user,
        params.params
      ),
      Errors.InvalidFlashloanExecutorReturn()
    );

    for (uint256 i = 0; i < params.assets.length; i++) {
      vars.currentAsset = params.assets[i];
      vars.currentAmount = params.amounts[i];

      if (
        DataTypes.InterestRateMode(params.interestRateModes[i]) == DataTypes.InterestRateMode.NONE
      ) {
        _handleFlashLoanRepayment(
          reservesData[vars.currentAsset],
          DataTypes.FlashLoanRepaymentParams({
            user: params.user,
            asset: vars.currentAsset,
            interestRateStrategyAddress: params.interestRateStrategyAddress,
            receiverAddress: params.receiverAddress,
            amount: vars.currentAmount,
            totalPremium: vars.totalPremiums[i],
            referralCode: params.referralCode
          })
        );
      } else {
        // If the user chose to not return the funds, the system checks if there is enough collateral and
        // eventually opens a debt position
        BorrowLogic.executeBorrow(
          reservesData,
          reservesList,
          eModeCategories,
          userConfig,
          DataTypes.ExecuteBorrowParams({
            asset: vars.currentAsset,
            interestRateStrategyAddress: params.interestRateStrategyAddress,
            user: params.user,
            onBehalfOf: params.onBehalfOf,
            amount: vars.currentAmount,
            interestRateMode: DataTypes.InterestRateMode(params.interestRateModes[i]),
            referralCode: params.referralCode,
            releaseUnderlying: false,
            oracle: IPoolAddressesProvider(params.addressesProvider).getPriceOracle(),
            userEModeCategory: IPool(params.pool).getUserEMode(params.onBehalfOf).toUint8(),
            priceOracleSentinel: IPoolAddressesProvider(params.addressesProvider)
              .getPriceOracleSentinel()
          })
        );
        // no premium is paid when taking on the flashloan as debt
        emit IPool.FlashLoan(
          params.receiverAddress,
          params.user,
          vars.currentAsset,
          vars.currentAmount,
          DataTypes.InterestRateMode(params.interestRateModes[i]),
          0,
          params.referralCode
        );
      }
    }
  }

  /**
   * @notice Implements the simple flashloan feature that allow users to access liquidity of ONE reserve for one
   * transaction as long as the amount taken plus fee is returned.
   * @dev Does not waive fee for approved flashborrowers nor allow taking on debt instead of repaying to save gas
   * @dev At the end of the transaction the pool will pull amount borrowed + fee from the receiver,
   * if the receiver have not approved the pool the transaction will revert.
   * @dev Emits the `FlashLoan()` event
   * @param reserve The state of the flashloaned reserve
   * @param params The additional parameters needed to execute the simple flashloan function
   */
  function executeFlashLoanSimple(
    DataTypes.ReserveData storage reserve,
    DataTypes.FlashloanSimpleParams memory params
  ) external {
    // The usual action flow (cache -> updateState -> validation -> changeState -> updateRates)
    // is altered to (validation -> user payload -> cache -> updateState -> changeState -> updateRates) for flashloans.
    // This is done to protect against reentrance and rate manipulation within the user specified payload.

    ValidationLogic.validateFlashloanSimple(reserve, params.amount);

    IFlashLoanSimpleReceiver receiver = IFlashLoanSimpleReceiver(params.receiverAddress);
    uint256 totalPremium = params.amount.percentMulCeil(params.flashLoanPremium);

    reserve.virtualUnderlyingBalance -= params.amount.toUint128();

    IAToken(reserve.aTokenAddress).transferUnderlyingTo(params.receiverAddress, params.amount);

    require(
      receiver.executeOperation(
        params.asset,
        params.amount,
        totalPremium,
        params.user,
        params.params
      ),
      Errors.InvalidFlashloanExecutorReturn()
    );

    _handleFlashLoanRepayment(
      reserve,
      DataTypes.FlashLoanRepaymentParams({
        user: params.user,
        asset: params.asset,
        interestRateStrategyAddress: params.interestRateStrategyAddress,
        receiverAddress: params.receiverAddress,
        amount: params.amount,
        totalPremium: totalPremium,
        referralCode: params.referralCode
      })
    );
  }

  /**
   * @notice Handles repayment of flashloaned assets + premium
   * @dev Will pull the amount + premium from the receiver, so must have approved pool
   * @param reserve The state of the flashloaned reserve
   * @param params The additional parameters needed to execute the repayment function
   */
  function _handleFlashLoanRepayment(
    DataTypes.ReserveData storage reserve,
    DataTypes.FlashLoanRepaymentParams memory params
  ) internal {
    uint256 amountPlusPremium = params.amount + params.totalPremium;

    DataTypes.ReserveCache memory reserveCache = reserve.cache();
    reserve.updateState(reserveCache);

    reserve.accruedToTreasury += params
      .totalPremium
      .getATokenMintScaledAmount(reserveCache.nextLiquidityIndex)
      .toUint128();

    reserve.updateInterestRatesAndVirtualBalance(
      reserveCache,
      params.asset,
      amountPlusPremium,
      0,
      params.interestRateStrategyAddress
    );

    IERC20(params.asset).safeTransferFrom(
      params.receiverAddress,
      reserveCache.aTokenAddress,
      amountPlusPremium
    );

    emit IPool.FlashLoan(
      params.receiverAddress,
      params.user,
      params.asset,
      params.amount,
      DataTypes.InterestRateMode.NONE,
      params.totalPremium,
      params.referralCode
    );
  }
}

// SPDX-License-Identifier: MIT
// OpenZeppelin Contracts (last updated v5.0.1) (utils/Context.sol)

pragma solidity ^0.8.20;

/**
 * @dev Provides information about the current execution context, including the
 * sender of the transaction and its data. While these are generally available
 * via msg.sender and msg.data, they should not be accessed in such a direct
 * manner, since when dealing with meta-transactions the account sending and
 * paying for execution may not be the actual sender (as far as an application
 * is concerned).
 *
 * This contract is only required for intermediate, library-like contracts.
 */
abstract contract Context {
    function _msgSender() internal view virtual returns (address) {
        return msg.sender;
    }

    function _msgData() internal view virtual returns (bytes calldata) {
        return msg.data;
    }

    function _contextSuffixLength() internal view virtual returns (uint256) {
        return 0;
    }
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {IPool} from '../../../interfaces/IPool.sol';
import {Errors} from '../helpers/Errors.sol';
import {UserConfiguration} from '../configuration/UserConfiguration.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {PercentageMath} from '../math/PercentageMath.sol';
import {ValidationLogic} from './ValidationLogic.sol';
import {ReserveLogic} from './ReserveLogic.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {TokenMath} from '../helpers/TokenMath.sol';

/**
 * @title SupplyLogic library
 * @author Aave
 * @notice Implements the base logic for supply/withdraw
 */
library SupplyLogic {
  using ReserveLogic for DataTypes.ReserveCache;
  using ReserveLogic for DataTypes.ReserveData;
  using GPv2SafeERC20 for IERC20;
  using UserConfiguration for DataTypes.UserConfigurationMap;
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;
  using TokenMath for uint256;
  using PercentageMath for uint256;

  /**
   * @notice Implements the supply feature. Through `supply()`, users supply assets to the Aave protocol.
   * @dev Emits the `Supply()` event.
   * @dev In the first supply action, `ReserveUsedAsCollateralEnabled()` is emitted, if the asset can be enabled as
   * collateral.
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param userConfig The user configuration mapping that tracks the supplied/borrowed assets
   * @param params The additional parameters needed to execute the supply function
   */
  function executeSupply(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    DataTypes.UserConfigurationMap storage userConfig,
    DataTypes.ExecuteSupplyParams memory params
  ) external {
    DataTypes.ReserveData storage reserve = reservesData[params.asset];
    DataTypes.ReserveCache memory reserveCache = reserve.cache();

    reserve.updateState(reserveCache);
    uint256 scaledAmount = params.amount.getATokenMintScaledAmount(reserveCache.nextLiquidityIndex);

    ValidationLogic.validateSupply(reserveCache, reserve, scaledAmount, params.onBehalfOf);

    reserve.updateInterestRatesAndVirtualBalance(
      reserveCache,
      params.asset,
      params.amount,
      0,
      params.interestRateStrategyAddress
    );

    IERC20(params.asset).safeTransferFrom(params.user, reserveCache.aTokenAddress, params.amount);

    // As aToken.mint rounds down the minted shares, we ensure an equivalent of <= params.amount shares is minted.
    bool isFirstSupply = IAToken(reserveCache.aTokenAddress).mint(
      params.user,
      params.onBehalfOf,
      scaledAmount,
      reserveCache.nextLiquidityIndex
    );

    if (isFirstSupply) {
      if (
        ValidationLogic.validateAutomaticUseAsCollateral(
          params.user,
          reservesData,
          reservesList,
          userConfig,
          reserveCache.reserveConfiguration,
          reserveCache.aTokenAddress
        )
      ) {
        userConfig.setUsingAsCollateral(reserve.id, params.asset, params.onBehalfOf, true);
      }
    }

    emit IPool.Supply(
      params.asset,
      params.user,
      params.onBehalfOf,
      params.amount,
      params.referralCode
    );
  }

  /**
   * @notice Implements the withdraw feature. Through `withdraw()`, users redeem their aTokens for the underlying asset
   * previously supplied in the Aave protocol.
   * @dev Emits the `Withdraw()` event.
   * @dev If the user withdraws everything, `ReserveUsedAsCollateralDisabled()` is emitted.
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param userConfig The user configuration mapping that tracks the supplied/borrowed assets
   * @param params The additional parameters needed to execute the withdraw function
   * @return The actual amount withdrawn
   */
  function executeWithdraw(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.UserConfigurationMap storage userConfig,
    DataTypes.ExecuteWithdrawParams memory params
  ) external returns (uint256) {
    DataTypes.ReserveData storage reserve = reservesData[params.asset];
    DataTypes.ReserveCache memory reserveCache = reserve.cache();

    require(params.to != reserveCache.aTokenAddress, Errors.WithdrawToAToken());

    reserve.updateState(reserveCache);

    uint256 scaledUserBalance = IAToken(reserveCache.aTokenAddress).scaledBalanceOf(params.user);

    uint256 amountToWithdraw;
    uint256 scaledAmountToWithdraw;
    if (params.amount == type(uint256).max) {
      scaledAmountToWithdraw = scaledUserBalance;

      amountToWithdraw = scaledUserBalance.getATokenBalance(reserveCache.nextLiquidityIndex);
    } else {
      scaledAmountToWithdraw = params.amount.getATokenBurnScaledAmount(
        reserveCache.nextLiquidityIndex
      );

      amountToWithdraw = params.amount;
    }

    ValidationLogic.validateWithdraw(reserveCache, scaledAmountToWithdraw, scaledUserBalance);

    reserve.updateInterestRatesAndVirtualBalance(
      reserveCache,
      params.asset,
      0,
      amountToWithdraw,
      params.interestRateStrategyAddress
    );

    // As aToken.burn rounds up the burned shares, we ensure at least an equivalent of >= amountToWithdraw is burned.
    bool zeroBalanceAfterBurn = IAToken(reserveCache.aTokenAddress).burn({
      from: params.user,
      receiverOfUnderlying: params.to,
      amount: amountToWithdraw,
      scaledAmount: scaledAmountToWithdraw,
      index: reserveCache.nextLiquidityIndex
    });

    if (userConfig.isUsingAsCollateral(reserve.id)) {
      if (zeroBalanceAfterBurn) {
        userConfig.setUsingAsCollateral(reserve.id, params.asset, params.user, false);
      }
      if (userConfig.isBorrowingAny()) {
        ValidationLogic.validateHFAndLtvzero(
          reservesData,
          reservesList,
          eModeCategories,
          userConfig,
          params.asset,
          params.user,
          params.oracle,
          params.userEModeCategory
        );
      }
    }

    emit IPool.Withdraw(params.asset, params.user, params.to, amountToWithdraw);

    return amountToWithdraw;
  }

  /**
   * @notice Validates a transfer of aTokens. The sender is subjected to health factor validation to avoid
   * collateralization constraints violation.
   * @dev Emits the `ReserveUsedAsCollateralEnabled()` event for the `to` account, if the asset is being activated as
   * collateral.
   * @dev In case the `from` user transfers everything, `ReserveUsedAsCollateralDisabled()` is emitted for `from`.
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param usersConfig The users configuration mapping that track the supplied/borrowed assets
   * @param params The additional parameters needed to execute the finalizeTransfer function
   */
  function executeFinalizeTransfer(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    mapping(address => DataTypes.UserConfigurationMap) storage usersConfig,
    DataTypes.FinalizeTransferParams memory params
  ) external {
    DataTypes.ReserveData storage reserve = reservesData[params.asset];

    ValidationLogic.validateTransfer(reserve);

    uint256 reserveId = reserve.id;

    if (params.from != params.to && params.scaledAmount != 0) {
      DataTypes.UserConfigurationMap storage fromConfig = usersConfig[params.from];

      if (fromConfig.isUsingAsCollateral(reserveId)) {
        if (params.scaledBalanceFromBefore == params.scaledAmount) {
          fromConfig.setUsingAsCollateral(reserveId, params.asset, params.from, false);
        }
        if (fromConfig.isBorrowingAny()) {
          ValidationLogic.validateHFAndLtvzero(
            reservesData,
            reservesList,
            eModeCategories,
            usersConfig[params.from],
            params.asset,
            params.from,
            params.oracle,
            params.fromEModeCategory
          );
        }
      }

      if (params.scaledBalanceToBefore == 0) {
        DataTypes.UserConfigurationMap storage toConfig = usersConfig[params.to];
        if (
          ValidationLogic.validateAutomaticUseAsCollateral(
            params.from,
            reservesData,
            reservesList,
            toConfig,
            reserve.configuration,
            reserve.aTokenAddress
          )
        ) {
          toConfig.setUsingAsCollateral(reserveId, params.asset, params.to, true);
        }
      }
    }
  }

  /**
   * @notice Executes the 'set as collateral' feature. A user can choose to activate or deactivate an asset as
   * collateral at any point in time. Deactivating an asset as collateral is subjected to the usual health factor
   * checks to ensure collateralization.
   * @dev Emits the `ReserveUsedAsCollateralEnabled()` event if the asset can be activated as collateral.
   * @dev In case the asset is being deactivated as collateral, `ReserveUsedAsCollateralDisabled()` is emitted.
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param userConfig The users configuration mapping that track the supplied/borrowed assets
   * @param user The user calling the method
   * @param asset The address of the asset being configured as collateral
   * @param useAsCollateral True if the user wants to set the asset as collateral, false otherwise
   * @param priceOracle The address of the price oracle
   * @param userEModeCategory The eMode category chosen by the user
   */
  function executeUseReserveAsCollateral(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.UserConfigurationMap storage userConfig,
    address user,
    address asset,
    bool useAsCollateral,
    address priceOracle,
    uint8 userEModeCategory
  ) external {
    DataTypes.ReserveData storage reserve = reservesData[asset];
    DataTypes.ReserveConfigurationMap memory reserveConfigCached = reserve.configuration;

    ValidationLogic.validateSetUseReserveAsCollateral(reserveConfigCached);

    if (useAsCollateral == userConfig.isUsingAsCollateral(reserve.id)) return;

    if (useAsCollateral) {
      // When enabeling a reserve as collateral, we want to ensure the user has at least some collateral
      require(
        IAToken(reserve.aTokenAddress).scaledBalanceOf(user) != 0,
        Errors.UnderlyingBalanceZero()
      );

      require(
        ValidationLogic.validateUseAsCollateral(
          reservesData,
          reservesList,
          userConfig,
          reserveConfigCached
        ),
        Errors.UserInIsolationModeOrLtvZero()
      );

      userConfig.setUsingAsCollateral(reserve.id, asset, user, true);
    } else {
      userConfig.setUsingAsCollateral(reserve.id, asset, user, false);
      ValidationLogic.validateHFAndLtvzero(
        reservesData,
        reservesList,
        eModeCategories,
        userConfig,
        asset,
        user,
        priceOracle,
        userEModeCategory
      );
    }
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IPool} from '../../../interfaces/IPool.sol';
import {Errors} from '../helpers/Errors.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {ReserveConfiguration} from './ReserveConfiguration.sol';

/**
 * @title UserConfiguration library
 * @author Aave
 * @notice Implements the bitmap logic to handle the user configuration
 */
library UserConfiguration {
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;

  uint256 internal constant BORROWING_MASK =
    0x5555555555555555555555555555555555555555555555555555555555555555;
  uint256 internal constant COLLATERAL_MASK =
    0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA;

  /**
   * @notice Sets if the user is borrowing the reserve identified by reserveIndex
   * @param self The configuration object
   * @param reserveIndex The index of the reserve in the bitmap
   * @param borrowing True if the user is borrowing the reserve, false otherwise
   */
  function setBorrowing(
    DataTypes.UserConfigurationMap storage self,
    uint256 reserveIndex,
    bool borrowing
  ) internal {
    unchecked {
      require(reserveIndex < ReserveConfiguration.MAX_RESERVES_COUNT, Errors.InvalidReserveIndex());
      uint256 bit = 1 << (reserveIndex << 1);
      if (borrowing) {
        self.data |= bit;
      } else {
        self.data &= ~bit;
      }
    }
  }

  /**
   * @notice Sets if the user is using as collateral the reserve identified by reserveIndex
   * @param self The configuration object
   * @param reserveIndex The index of the reserve in the bitmap
   * @param asset The address of the reserve
   * @param user The address of the user
   * @param usingAsCollateral True if the user is using the reserve as collateral, false otherwise
   */
  function setUsingAsCollateral(
    DataTypes.UserConfigurationMap storage self,
    uint256 reserveIndex,
    address asset,
    address user,
    bool usingAsCollateral
  ) internal {
    unchecked {
      require(reserveIndex < ReserveConfiguration.MAX_RESERVES_COUNT, Errors.InvalidReserveIndex());
      uint256 bit = 1 << ((reserveIndex << 1) + 1);
      if (usingAsCollateral) {
        self.data |= bit;
        emit IPool.ReserveUsedAsCollateralEnabled(asset, user);
      } else {
        self.data &= ~bit;
        emit IPool.ReserveUsedAsCollateralDisabled(asset, user);
      }
    }
  }

  /**
   * @notice Returns if a user has been using the reserve for borrowing or as collateral
   * @param self The configuration object
   * @param reserveIndex The index of the reserve in the bitmap
   * @return True if the user has been using a reserve for borrowing or as collateral, false otherwise
   */
  function isUsingAsCollateralOrBorrowing(
    DataTypes.UserConfigurationMap memory self,
    uint256 reserveIndex
  ) internal pure returns (bool) {
    unchecked {
      require(reserveIndex < ReserveConfiguration.MAX_RESERVES_COUNT, Errors.InvalidReserveIndex());
      return (self.data >> (reserveIndex << 1)) & 3 != 0;
    }
  }

  /**
   * @notice Validate a user has been using the reserve for borrowing
   * @param self The configuration object
   * @param reserveIndex The index of the reserve in the bitmap
   * @return True if the user has been using a reserve for borrowing, false otherwise
   */
  function isBorrowing(
    DataTypes.UserConfigurationMap memory self,
    uint256 reserveIndex
  ) internal pure returns (bool) {
    unchecked {
      require(reserveIndex < ReserveConfiguration.MAX_RESERVES_COUNT, Errors.InvalidReserveIndex());
      return (self.data >> (reserveIndex << 1)) & 1 != 0;
    }
  }

  /**
   * @notice Validate a user has been using the reserve as collateral
   * @param self The configuration object
   * @param reserveIndex The index of the reserve in the bitmap
   * @return True if the user has been using a reserve as collateral, false otherwise
   */
  function isUsingAsCollateral(
    DataTypes.UserConfigurationMap memory self,
    uint256 reserveIndex
  ) internal pure returns (bool) {
    unchecked {
      require(reserveIndex < ReserveConfiguration.MAX_RESERVES_COUNT, Errors.InvalidReserveIndex());
      return (self.data >> ((reserveIndex << 1) + 1)) & 1 != 0;
    }
  }

  /**
   * @notice Checks if a user has been supplying only one reserve as collateral
   * @dev this uses a simple trick - if a number is a power of two (only one bit set) then n & (n - 1) == 0
   * @param self The configuration object
   * @return True if the user has been supplying as collateral one reserve, false otherwise
   */
  function isUsingAsCollateralOne(
    DataTypes.UserConfigurationMap memory self
  ) internal pure returns (bool) {
    uint256 collateralData = self.data & COLLATERAL_MASK;
    return collateralData != 0 && (collateralData & (collateralData - 1) == 0);
  }

  /**
   * @notice Checks if a user has been supplying any reserve as collateral
   * @param self The configuration object
   * @return True if the user has been supplying as collateral any reserve, false otherwise
   */
  function isUsingAsCollateralAny(
    DataTypes.UserConfigurationMap memory self
  ) internal pure returns (bool) {
    return self.data & COLLATERAL_MASK != 0;
  }

  /**
   * @notice Checks if a user has been borrowing only one asset
   * @dev this uses a simple trick - if a number is a power of two (only one bit set) then n & (n - 1) == 0
   * @param self The configuration object
   * @return True if the user has been supplying as collateral one reserve, false otherwise
   */
  function isBorrowingOne(DataTypes.UserConfigurationMap memory self) internal pure returns (bool) {
    uint256 borrowingData = self.data & BORROWING_MASK;
    return borrowingData != 0 && (borrowingData & (borrowingData - 1) == 0);
  }

  /**
   * @notice Checks if a user has been borrowing from any reserve
   * @param self The configuration object
   * @return True if the user has been borrowing any reserve, false otherwise
   */
  function isBorrowingAny(DataTypes.UserConfigurationMap memory self) internal pure returns (bool) {
    return self.data & BORROWING_MASK != 0;
  }

  /**
   * @notice Checks if a user has not been using any reserve for borrowing or supply
   * @param self The configuration object
   * @return True if the user has not been borrowing or supplying any reserve, false otherwise
   */
  function isEmpty(DataTypes.UserConfigurationMap memory self) internal pure returns (bool) {
    return self.data == 0;
  }

  /**
   * @notice Returns the Isolation Mode state of the user
   * @param self The configuration object
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @return True if the user is in isolation mode, false otherwise
   * @return The address of the only asset used as collateral
   * @return The debt ceiling of the reserve
   */
  function getIsolationModeState(
    DataTypes.UserConfigurationMap memory self,
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList
  ) internal view returns (bool, address, uint256) {
    if (isUsingAsCollateralOne(self)) {
      uint256 assetId = _getFirstAssetIdByMask(self, COLLATERAL_MASK);

      address assetAddress = reservesList[assetId];
      uint256 ceiling = reservesData[assetAddress].configuration.getDebtCeiling();
      if (ceiling != 0) {
        return (true, assetAddress, ceiling);
      }
    }
    return (false, address(0), 0);
  }

  /**
   * @notice Returns the siloed borrowing state for the user
   * @param self The configuration object
   * @param reservesData The data of all the reserves
   * @param reservesList The reserve list
   * @return True if the user has borrowed a siloed asset, false otherwise
   * @return The address of the only borrowed asset
   */
  function getSiloedBorrowingState(
    DataTypes.UserConfigurationMap memory self,
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList
  ) internal view returns (bool, address) {
    if (isBorrowingOne(self)) {
      uint256 assetId = _getFirstAssetIdByMask(self, BORROWING_MASK);
      address assetAddress = reservesList[assetId];
      if (reservesData[assetAddress].configuration.getSiloedBorrowing()) {
        return (true, assetAddress);
      }
    }

    return (false, address(0));
  }

  /**
   * @notice Returns the borrowed and collateral flags for the first asset on the bitmap and the bitmap shifted by two.
   * @dev This function mutates the input and the 2 bit slots in the bitmap will no longer correspond to the reserve index.
   * This is useful in situations where we want to iterate the bitmap as it allows for early exit once the bitmap turns zero.
   * @param data The configuration uint256
   * @return The bitmap shifted by 2 bits, so that the first asset points to the *next* asset.
   * @return True if the first asset in the bitmap is borrowed.
   * @return True if the first asset in the bitmap is a collateral.
   */
  function getNextFlags(uint256 data) internal pure returns (uint256, bool, bool) {
    bool isBorrowed = data & 1 == 1;
    bool isEnabledAsCollateral = data & 2 == 2;
    return (data >> 2, isBorrowed, isEnabledAsCollateral);
  }

  /**
   * @notice Returns the address of the first asset flagged in the bitmap given the corresponding bitmask
   * @param self The configuration object
   * @return The index of the first asset flagged in the bitmap once the corresponding mask is applied
   */
  function _getFirstAssetIdByMask(
    DataTypes.UserConfigurationMap memory self,
    uint256 mask
  ) internal pure returns (uint256) {
    unchecked {
      uint256 bitmapData = self.data & mask;
      uint256 firstAssetPosition = bitmapData & ~(bitmapData - 1);
      uint256 id;

      while ((firstAssetPosition >>= 2) != 0) {
        id += 1;
      }
      return id;
    }
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {WadRayMath} from '../../libraries/math/WadRayMath.sol';

/**
 * @title TokenMath
 * @author BGD Labs
 * @notice Provides utility functions for calculating scaled amounts and balances for aTokens and vTokens,
 *         applying specific rounding rules (floor/ceil) as per Aave v3.5's rounding improvements.
 *         The rounding behavior of the operations is in line with the ERC-4626 token standard.
 *         In practice, this means rounding in favor of the protocol.
 */
library TokenMath {
  using WadRayMath for uint256;

  /**
   * @notice Calculates the scaled amount of aTokens to mint when supplying underlying assets.
   *         The amount is rounded down to ensure the minted aTokens are less than or equal to the supplied amount.
   * @param amount The amount of underlying asset supplied.
   * @param liquidityIndex The current aToken liquidityIndex.
   * @return The scaled amount of aTokens to mint.
   */
  function getATokenMintScaledAmount(
    uint256 amount,
    uint256 liquidityIndex
  ) internal pure returns (uint256) {
    return amount.rayDivFloor(liquidityIndex);
  }

  /**
   * @notice Calculates the scaled amount of aTokens to burn when withdrawing underlying assets.
   *         The scaled amount is rounded up to ensure the user's aToken balance is sufficiently reduced.
   * @param amount The amount of underlying asset to withdraw.
   * @param liquidityIndex The current aToken liquidityIndex.
   * @return The scaled amount of aTokens to burn.
   */
  function getATokenBurnScaledAmount(
    uint256 amount,
    uint256 liquidityIndex
  ) internal pure returns (uint256) {
    return amount.rayDivCeil(liquidityIndex);
  }

  /**
   * @notice Calculates the scaled amount of aTokens to transfer.
   *         The scaled amount is rounded up to ensure the recipient receives at least the requested amount.
   * @param amount The amount of aTokens to transfer.
   * @param liquidityIndex The current aToken liquidityIndex.
   * @return The scaled amount of aTokens for transfer.
   */
  function getATokenTransferScaledAmount(
    uint256 amount,
    uint256 liquidityIndex
  ) internal pure returns (uint256) {
    return amount.rayDivCeil(liquidityIndex);
  }

  /**
   * @notice Calculates the actual aToken balance from a scaled balance and the current liquidityIndex.
   *         The balance is rounded down to prevent overaccounting.
   * @param scaledAmount The scaled aToken balance.
   * @param liquidityIndex The current aToken liquidityIndex.
   * @return The actual aToken balance.
   */
  function getATokenBalance(
    uint256 scaledAmount,
    uint256 liquidityIndex
  ) internal pure returns (uint256) {
    return scaledAmount.rayMulFloor(liquidityIndex);
  }

  /**
   * @notice Calculates the scaled amount of vTokens to mint when borrowing.
   *         The amount is rounded up to ensure the protocol never underaccounts the user's debt.
   * @param amount The amount of underlying asset borrowed.
   * @param variableBorrowIndex The current vToken variableBorrowIndex.
   * @return The scaled amount of vTokens to mint.
   */
  function getVTokenMintScaledAmount(
    uint256 amount,
    uint256 variableBorrowIndex
  ) internal pure returns (uint256) {
    return amount.rayDivCeil(variableBorrowIndex);
  }

  /**
   * @notice Calculates the scaled amount of vTokens to burn.
   *         The scaled amount is rounded down to prevent over-burning of vTokens.
   * @param amount The amount of underlying asset corresponding to the vTokens to burn.
   * @param variableBorrowIndex The current vToken variableBorrowIndex.
   * @return The scaled amount of vTokens to burn.
   */
  function getVTokenBurnScaledAmount(
    uint256 amount,
    uint256 variableBorrowIndex
  ) internal pure returns (uint256) {
    return amount.rayDivFloor(variableBorrowIndex);
  }

  /**
   * @notice Calculates the actual vToken balance (debt) from a scaled balance and the current variableBorrowIndex.
   *         The balance is rounded up to prevent underaccounting the user's debt.
   * @param scaledAmount The scaled vToken balance.
   * @param variableBorrowIndex The current vToken variableBorrowIndex.
   * @return The actual vToken balance (debt).
   */
  function getVTokenBalance(
    uint256 scaledAmount,
    uint256 variableBorrowIndex
  ) internal pure returns (uint256) {
    return scaledAmount.rayMulCeil(variableBorrowIndex);
  }
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {IERC20} from '../../../dependencies/openzeppelin/contracts//IERC20.sol';
import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {PercentageMath} from '../../libraries/math/PercentageMath.sol';
import {MathUtils} from '../../libraries/math/MathUtils.sol';
import {TokenMath} from '../../libraries/helpers/TokenMath.sol';
import {DataTypes} from '../../libraries/types/DataTypes.sol';
import {ReserveLogic} from './ReserveLogic.sol';
import {ValidationLogic} from './ValidationLogic.sol';
import {GenericLogic} from './GenericLogic.sol';
import {IsolationModeLogic} from './IsolationModeLogic.sol';
import {UserConfiguration} from '../../libraries/configuration/UserConfiguration.sol';
import {ReserveConfiguration} from '../../libraries/configuration/ReserveConfiguration.sol';
import {EModeConfiguration} from '../../libraries/configuration/EModeConfiguration.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {IPool} from '../../../interfaces/IPool.sol';
import {IVariableDebtToken} from '../../../interfaces/IVariableDebtToken.sol';
import {IPriceOracleGetter} from '../../../interfaces/IPriceOracleGetter.sol';
import {SafeCast} from 'openzeppelin-contracts/contracts/utils/math/SafeCast.sol';
import {Errors} from '../helpers/Errors.sol';

/**
 * @title LiquidationLogic library
 * @author Aave
 * @notice Implements actions involving management of collateral in the protocol, the main one being the liquidations
 */
library LiquidationLogic {
  using TokenMath for uint256;
  using PercentageMath for uint256;
  using ReserveLogic for DataTypes.ReserveCache;
  using ReserveLogic for DataTypes.ReserveData;
  using UserConfiguration for DataTypes.UserConfigurationMap;
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;
  using GPv2SafeERC20 for IERC20;
  using SafeCast for uint256;

  /**
   * @dev Default percentage of borrower's debt to be repaid in a liquidation.
   * @dev Percentage applied when the users health factor is above `CLOSE_FACTOR_HF_THRESHOLD`
   * Expressed in bps, a value of 0.5e4 results in 50.00%
   */
  uint256 internal constant DEFAULT_LIQUIDATION_CLOSE_FACTOR = 0.5e4;

  /**
   * @dev This constant represents the upper bound on the health factor, below(inclusive) which the full amount of debt becomes liquidatable.
   * A value of 0.95e18 results in 0.95
   */
  uint256 public constant CLOSE_FACTOR_HF_THRESHOLD = 0.95e18;

  /**
   * @dev This constant represents a base value threshold.
   * If the total collateral or debt on a position is below this threshold, the close factor is raised to 100%.
   * @notice The default value assumes that the basePrice is usd denominated by 8 decimals and needs to be adjusted in a non USD-denominated pool.
   */
  uint256 public constant MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD = 2000e8;

  /**
   * @dev This constant represents the minimum amount of assets in base currency that need to be leftover after a liquidation, if not clearing a position completely.
   * This parameter is inferred from MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD as the logic is dependent.
   * Assuming a MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD of `n` a liquidation of `n+1` might result in `n/2` leftover which is assumed to be still economically liquidatable.
   * This mechanic was introduced to ensure liquidators don't optimize gas by leaving some wei on the liquidation.
   */
  uint256 public constant MIN_LEFTOVER_BASE = MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD / 2;

  /**
   * @notice Reduces a portion or all of the deficit of a specified reserve by burning the equivalent aToken `amount`
   * The caller of this method MUST always be the Umbrella contract and the Umbrella contract is assumed to never have debt.
   * @dev Emits the `DeficitCovered() event`.
   * @dev If the coverage admin covers its entire balance, `ReserveUsedAsCollateralDisabled()` is emitted.
   * @param reservesData The state of all the reserves
   * @param userConfig The user configuration mapping that tracks the supplied/borrowed assets
   * @param params The additional parameters needed to execute the eliminateDeficit function
   * @return The amount of deficit covered
   */
  function executeEliminateDeficit(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    DataTypes.UserConfigurationMap storage userConfig,
    DataTypes.ExecuteEliminateDeficitParams memory params
  ) external returns (uint256) {
    require(params.amount != 0, Errors.InvalidAmount());

    DataTypes.ReserveData storage reserve = reservesData[params.asset];
    uint256 currentDeficit = reserve.deficit;

    require(currentDeficit != 0, Errors.ReserveNotInDeficit());
    require(!userConfig.isBorrowingAny(), Errors.UserCannotHaveDebt());

    DataTypes.ReserveCache memory reserveCache = reserve.cache();
    reserve.updateState(reserveCache);
    bool isActive = reserveCache.reserveConfiguration.getActive();
    require(isActive, Errors.ReserveInactive());

    uint256 balanceWriteOff = params.amount;

    if (params.amount > currentDeficit) {
      balanceWriteOff = currentDeficit;
    }

    uint256 userScaledBalance = IAToken(reserveCache.aTokenAddress).scaledBalanceOf(params.user);
    uint256 scaledBalanceWriteOff = balanceWriteOff.getATokenBurnScaledAmount(
      reserveCache.nextLiquidityIndex
    );
    require(scaledBalanceWriteOff <= userScaledBalance, Errors.NotEnoughAvailableUserBalance());

    bool isCollateral = userConfig.isUsingAsCollateral(reserve.id);
    if (isCollateral && scaledBalanceWriteOff == userScaledBalance) {
      userConfig.setUsingAsCollateral(reserve.id, params.asset, params.user, false);
    }

    IAToken(reserveCache.aTokenAddress).burn({
      from: params.user,
      receiverOfUnderlying: reserveCache.aTokenAddress,
      amount: balanceWriteOff,
      scaledAmount: scaledBalanceWriteOff,
      index: reserveCache.nextLiquidityIndex
    });

    reserve.deficit -= balanceWriteOff.toUint128();

    reserve.updateInterestRatesAndVirtualBalance(
      reserveCache,
      params.asset,
      0,
      0,
      params.interestRateStrategyAddress
    );

    emit IPool.DeficitCovered(params.asset, params.user, balanceWriteOff);

    return balanceWriteOff;
  }

  struct LiquidationCallLocalVars {
    uint256 borrowerCollateralBalance;
    uint256 borrowerReserveDebt;
    uint256 actualDebtToLiquidate;
    uint256 actualCollateralToLiquidate;
    uint256 liquidationBonus;
    uint256 healthFactor;
    uint256 liquidationProtocolFeeAmount;
    uint256 totalCollateralInBaseCurrency;
    uint256 totalDebtInBaseCurrency;
    uint256 collateralToLiquidateInBaseCurrency;
    uint256 borrowerReserveDebtInBaseCurrency;
    uint256 borrowerReserveCollateralInBaseCurrency;
    uint256 collateralAssetPrice;
    uint256 debtAssetPrice;
    uint256 collateralAssetUnit;
    uint256 debtAssetUnit;
    DataTypes.ReserveCache debtReserveCache;
    DataTypes.ReserveCache collateralReserveCache;
  }

  /**
   * @notice Function to liquidate a position if its Health Factor drops below 1. The caller (liquidator)
   * covers `debtToCover` amount of debt of the user getting liquidated, and receives
   * a proportional amount of the `collateralAsset` plus a bonus to cover market risk
   * @dev Emits the `LiquidationCall()` event, and the `DeficitCreated()` event if the liquidation results in bad debt
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param usersConfig The users configuration mapping that track the supplied/borrowed assets
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param params The additional parameters needed to execute the liquidation function
   */
  function executeLiquidationCall(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(address => DataTypes.UserConfigurationMap) storage usersConfig,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.ExecuteLiquidationCallParams memory params
  ) external {
    LiquidationCallLocalVars memory vars;

    DataTypes.ReserveData storage collateralReserve = reservesData[params.collateralAsset];
    DataTypes.ReserveData storage debtReserve = reservesData[params.debtAsset];
    DataTypes.UserConfigurationMap storage borrowerConfig = usersConfig[params.borrower];
    vars.debtReserveCache = debtReserve.cache();
    vars.collateralReserveCache = collateralReserve.cache();
    debtReserve.updateState(vars.debtReserveCache);
    collateralReserve.updateState(vars.collateralReserveCache);

    (
      vars.totalCollateralInBaseCurrency,
      vars.totalDebtInBaseCurrency,
      ,
      ,
      vars.healthFactor,

    ) = GenericLogic.calculateUserAccountData(
      reservesData,
      reservesList,
      eModeCategories,
      DataTypes.CalculateUserAccountDataParams({
        userConfig: borrowerConfig,
        user: params.borrower,
        oracle: params.priceOracle,
        userEModeCategory: params.borrowerEModeCategory
      })
    );

    vars.borrowerCollateralBalance = IAToken(vars.collateralReserveCache.aTokenAddress)
      .scaledBalanceOf(params.borrower)
      .getATokenBalance(vars.collateralReserveCache.nextLiquidityIndex);
    vars.borrowerReserveDebt = IVariableDebtToken(vars.debtReserveCache.variableDebtTokenAddress)
      .scaledBalanceOf(params.borrower)
      .getVTokenBalance(vars.debtReserveCache.nextVariableBorrowIndex);

    ValidationLogic.validateLiquidationCall(
      borrowerConfig,
      collateralReserve,
      debtReserve,
      DataTypes.ValidateLiquidationCallParams({
        debtReserveCache: vars.debtReserveCache,
        totalDebt: vars.borrowerReserveDebt,
        healthFactor: vars.healthFactor,
        priceOracleSentinel: params.priceOracleSentinel,
        borrower: params.borrower,
        liquidator: params.liquidator
      })
    );

    if (
      params.borrowerEModeCategory != 0 &&
      EModeConfiguration.isReserveEnabledOnBitmap(
        eModeCategories[params.borrowerEModeCategory].collateralBitmap,
        collateralReserve.id
      )
    ) {
      vars.liquidationBonus = eModeCategories[params.borrowerEModeCategory].liquidationBonus;
    } else {
      vars.liquidationBonus = vars
        .collateralReserveCache
        .reserveConfiguration
        .getLiquidationBonus();
    }
    vars.collateralAssetPrice = IPriceOracleGetter(params.priceOracle).getAssetPrice(
      params.collateralAsset
    );
    vars.debtAssetPrice = IPriceOracleGetter(params.priceOracle).getAssetPrice(params.debtAsset);
    vars.collateralAssetUnit = 10 ** vars.collateralReserveCache.reserveConfiguration.getDecimals();
    vars.debtAssetUnit = 10 ** vars.debtReserveCache.reserveConfiguration.getDecimals();

    vars.borrowerReserveDebtInBaseCurrency = MathUtils.mulDivCeil(
      vars.borrowerReserveDebt,
      vars.debtAssetPrice,
      vars.debtAssetUnit
    );

    // @note floor rounding
    vars.borrowerReserveCollateralInBaseCurrency =
      (vars.borrowerCollateralBalance * vars.collateralAssetPrice) /
      vars.collateralAssetUnit;

    // by default whole debt in the reserve could be liquidated
    uint256 maxLiquidatableDebt = vars.borrowerReserveDebt;
    // but if debt and collateral is above or equal MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD
    // and health factor is above CLOSE_FACTOR_HF_THRESHOLD this amount may be adjusted
    if (
      vars.borrowerReserveCollateralInBaseCurrency >= MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD &&
      vars.borrowerReserveDebtInBaseCurrency >= MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD &&
      vars.healthFactor > CLOSE_FACTOR_HF_THRESHOLD
    ) {
      uint256 totalDefaultLiquidatableDebtInBaseCurrency = vars.totalDebtInBaseCurrency.percentMul(
        DEFAULT_LIQUIDATION_CLOSE_FACTOR
      );

      // if the debt is more then DEFAULT_LIQUIDATION_CLOSE_FACTOR % of the whole,
      // then we CAN liquidate only up to DEFAULT_LIQUIDATION_CLOSE_FACTOR %
      if (vars.borrowerReserveDebtInBaseCurrency > totalDefaultLiquidatableDebtInBaseCurrency) {
        maxLiquidatableDebt =
          (totalDefaultLiquidatableDebtInBaseCurrency * vars.debtAssetUnit) /
          vars.debtAssetPrice;
      }
    }

    vars.actualDebtToLiquidate = params.debtToCover > maxLiquidatableDebt
      ? maxLiquidatableDebt
      : params.debtToCover;

    (
      vars.actualCollateralToLiquidate,
      vars.actualDebtToLiquidate,
      vars.liquidationProtocolFeeAmount,
      vars.collateralToLiquidateInBaseCurrency
    ) = _calculateAvailableCollateralToLiquidate(
      vars.collateralReserveCache.reserveConfiguration,
      vars.collateralAssetPrice,
      vars.collateralAssetUnit,
      vars.debtAssetPrice,
      vars.debtAssetUnit,
      vars.actualDebtToLiquidate,
      vars.borrowerCollateralBalance,
      vars.liquidationBonus
    );

    // to prevent accumulation of dust on the protocol, it is enforced that you either
    // 1. liquidate all debt
    // 2. liquidate all collateral
    // 3. leave more than MIN_LEFTOVER_BASE of collateral & debt
    if (
      vars.actualDebtToLiquidate < vars.borrowerReserveDebt &&
      vars.actualCollateralToLiquidate + vars.liquidationProtocolFeeAmount <
      vars.borrowerCollateralBalance
    ) {
      bool isDebtMoreThanLeftoverThreshold = MathUtils.mulDivCeil(
        vars.borrowerReserveDebt - vars.actualDebtToLiquidate,
        vars.debtAssetPrice,
        vars.debtAssetUnit
      ) >= MIN_LEFTOVER_BASE;

      // @note floor rounding
      bool isCollateralMoreThanLeftoverThreshold = ((vars.borrowerCollateralBalance -
        vars.actualCollateralToLiquidate -
        vars.liquidationProtocolFeeAmount) * vars.collateralAssetPrice) /
        vars.collateralAssetUnit >=
        MIN_LEFTOVER_BASE;

      require(
        isDebtMoreThanLeftoverThreshold && isCollateralMoreThanLeftoverThreshold,
        Errors.MustNotLeaveDust()
      );
    }

    // If the collateral being liquidated is equal to the user balance,
    // we set the currency as not being used as collateral anymore
    if (
      vars.actualCollateralToLiquidate + vars.liquidationProtocolFeeAmount ==
      vars.borrowerCollateralBalance
    ) {
      borrowerConfig.setUsingAsCollateral(
        collateralReserve.id,
        params.collateralAsset,
        params.borrower,
        false
      );
    }

    bool hasNoCollateralLeft = vars.totalCollateralInBaseCurrency ==
      vars.collateralToLiquidateInBaseCurrency;
    _burnDebtTokens(
      vars.debtReserveCache,
      debtReserve,
      borrowerConfig,
      params.borrower,
      params.debtAsset,
      vars.borrowerReserveDebt,
      vars.actualDebtToLiquidate,
      hasNoCollateralLeft,
      params.interestRateStrategyAddress
    );

    // An asset can only be ceiled if it has no supply or if it was not a collateral previously.
    // Therefore we can be sure that no inconsistent state can be reached in which a user has multiple collaterals, with one being ceiled.
    // This allows for the implicit assumption that: if the asset was a collateral & the asset was ceiled, the user must have been in isolation.
    if (vars.collateralReserveCache.reserveConfiguration.getDebtCeiling() != 0) {
      // IsolationModeTotalDebt only discounts `actualDebtToLiquidate`, not the fully burned amount in case of deficit creation.
      // This is by design as otherwise the debt ceiling would render ineffective if a collateral asset faces bad debt events.
      // The governance can decide the raise the ceiling to discount manifested deficit.
      IsolationModeLogic.updateIsolatedDebt(
        reservesData,
        vars.debtReserveCache,
        vars.actualDebtToLiquidate,
        params.collateralAsset
      );
    }

    if (params.receiveAToken) {
      _liquidateATokens(reservesData, reservesList, usersConfig, collateralReserve, params, vars);
    } else {
      // @note Manually updating the cache in case the debt and collateral are the same asset.
      // This ensures the rates are updated correctly, considering the burning of debt
      // in the `_burnDebtTokens` function.
      if (params.collateralAsset == params.debtAsset) {
        vars.collateralReserveCache.nextScaledVariableDebt = vars
          .debtReserveCache
          .nextScaledVariableDebt;
      }

      _burnCollateralATokens(collateralReserve, params, vars);
    }

    // Transfer fee to treasury if it is non-zero
    if (vars.liquidationProtocolFeeAmount != 0) {
      // getATokenTransferScaledAmount has been used because under the hood, transferOnLiquidation is calling AToken.transfer
      uint256 scaledDownLiquidationProtocolFee = vars
        .liquidationProtocolFeeAmount
        .getATokenTransferScaledAmount(vars.collateralReserveCache.nextLiquidityIndex);
      uint256 scaledDownBorrowerBalance = IAToken(vars.collateralReserveCache.aTokenAddress)
        .scaledBalanceOf(params.borrower);
      // To avoid trying to send more aTokens than available on balance, due to 1 wei imprecision
      if (scaledDownLiquidationProtocolFee > scaledDownBorrowerBalance) {
        scaledDownLiquidationProtocolFee = scaledDownBorrowerBalance;
        vars.liquidationProtocolFeeAmount = scaledDownBorrowerBalance.getATokenBalance(
          vars.collateralReserveCache.nextLiquidityIndex
        );
      }
      IAToken(vars.collateralReserveCache.aTokenAddress).transferOnLiquidation({
        from: params.borrower,
        to: IAToken(vars.collateralReserveCache.aTokenAddress).RESERVE_TREASURY_ADDRESS(),
        amount: vars.liquidationProtocolFeeAmount,
        scaledAmount: scaledDownLiquidationProtocolFee,
        index: vars.collateralReserveCache.nextLiquidityIndex
      });
    }

    // burn bad debt if necessary
    // Each additional debt asset already adds around ~75k gas to the liquidation.
    // To keep the liquidation gas under control, 0 usd collateral positions are not touched, as there is no immediate benefit in burning or transferring to treasury.
    if (hasNoCollateralLeft && borrowerConfig.isBorrowingAny()) {
      _burnBadDebt(reservesData, reservesList, borrowerConfig, params);
    }

    // Transfers the debt asset being repaid to the aToken, where the liquidity is kept
    IERC20(params.debtAsset).safeTransferFrom(
      params.liquidator,
      vars.debtReserveCache.aTokenAddress,
      vars.actualDebtToLiquidate
    );

    emit IPool.LiquidationCall(
      params.collateralAsset,
      params.debtAsset,
      params.borrower,
      vars.actualDebtToLiquidate,
      vars.actualCollateralToLiquidate,
      params.liquidator,
      params.receiveAToken
    );
  }

  /**
   * @notice Burns the collateral aTokens and transfers the underlying to the liquidator.
   * @dev   The function also updates the state and the interest rate of the collateral reserve.
   * @param collateralReserve The data of the collateral reserve
   * @param params The additional parameters needed to execute the liquidation function
   * @param vars The executeLiquidationCall() function local vars
   */
  function _burnCollateralATokens(
    DataTypes.ReserveData storage collateralReserve,
    DataTypes.ExecuteLiquidationCallParams memory params,
    LiquidationCallLocalVars memory vars
  ) internal {
    collateralReserve.updateInterestRatesAndVirtualBalance(
      vars.collateralReserveCache,
      params.collateralAsset,
      0,
      vars.actualCollateralToLiquidate,
      params.interestRateStrategyAddress
    );

    // Burn the equivalent amount of aToken, sending the underlying to the liquidator
    IAToken(vars.collateralReserveCache.aTokenAddress).burn({
      from: params.borrower,
      receiverOfUnderlying: params.liquidator,
      amount: vars.actualCollateralToLiquidate,
      scaledAmount: vars.actualCollateralToLiquidate.getATokenBurnScaledAmount(
        vars.collateralReserveCache.nextLiquidityIndex
      ),
      index: vars.collateralReserveCache.nextLiquidityIndex
    });
  }

  /**
   * @notice Liquidates the user aTokens by transferring them to the liquidator.
   * @dev   The function also checks the state of the liquidator and activates the aToken as collateral
   *        as in standard transfers if the isolation mode constraints are respected.
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param usersConfig The users configuration mapping that track the supplied/borrowed assets
   * @param collateralReserve The data of the collateral reserve
   * @param params The additional parameters needed to execute the liquidation function
   * @param vars The executeLiquidationCall() function local vars
   */
  function _liquidateATokens(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(address => DataTypes.UserConfigurationMap) storage usersConfig,
    DataTypes.ReserveData storage collateralReserve,
    DataTypes.ExecuteLiquidationCallParams memory params,
    LiquidationCallLocalVars memory vars
  ) internal {
    uint256 liquidatorPreviousATokenBalance = IAToken(vars.collateralReserveCache.aTokenAddress)
      .scaledBalanceOf(params.liquidator);
    IAToken(vars.collateralReserveCache.aTokenAddress).transferOnLiquidation(
      params.borrower,
      params.liquidator,
      vars.actualCollateralToLiquidate,
      vars.actualCollateralToLiquidate.getATokenTransferScaledAmount(
        vars.collateralReserveCache.nextLiquidityIndex
      ),
      vars.collateralReserveCache.nextLiquidityIndex
    );

    if (liquidatorPreviousATokenBalance == 0) {
      DataTypes.UserConfigurationMap storage liquidatorConfig = usersConfig[params.liquidator];
      if (
        ValidationLogic.validateAutomaticUseAsCollateral(
          params.liquidator,
          reservesData,
          reservesList,
          liquidatorConfig,
          vars.collateralReserveCache.reserveConfiguration,
          vars.collateralReserveCache.aTokenAddress
        )
      ) {
        liquidatorConfig.setUsingAsCollateral(
          collateralReserve.id,
          params.collateralAsset,
          params.liquidator,
          true
        );
      }
    }
  }

  /**
   * @notice Burns the debt tokens of the user up to the amount being repaid by the liquidator
   * or the entire debt if the user is in a bad debt scenario.
   * @dev The function alters the `debtReserveCache` state in `vars` to update the debt related data.
   * @param debtReserveCache The cached debt reserve parameters
   * @param debtReserve The storage pointer of the debt reserve parameters
   * @param borrowerConfig The pointer of the user configuration
   * @param borrower The user address
   * @param debtAsset The debt asset address
   * @param actualDebtToLiquidate The actual debt to liquidate
   * @param hasNoCollateralLeft The flag representing, will user will have no collateral left after liquidation
   */
  function _burnDebtTokens(
    DataTypes.ReserveCache memory debtReserveCache,
    DataTypes.ReserveData storage debtReserve,
    DataTypes.UserConfigurationMap storage borrowerConfig,
    address borrower,
    address debtAsset,
    uint256 borrowerReserveDebt,
    uint256 actualDebtToLiquidate,
    bool hasNoCollateralLeft,
    address interestRateStrategyAddress
  ) internal {
    bool noMoreDebt = true;
    // Prior v3.1, there were cases where, after liquidation, the `isBorrowing` flag was left on
    // even after the user debt was fully repaid, so to avoid this function reverting in the `_burnScaled`
    // (see ScaledBalanceTokenBase contract), we check for any debt remaining.
    if (borrowerReserveDebt != 0) {
      uint256 burnAmount = hasNoCollateralLeft ? borrowerReserveDebt : actualDebtToLiquidate;

      // As vDebt.burn rounds down, we ensure an equivalent of <= amount debt is burned.
      (noMoreDebt, debtReserveCache.nextScaledVariableDebt) = IVariableDebtToken(
        debtReserveCache.variableDebtTokenAddress
      ).burn({
          from: borrower,
          scaledAmount: burnAmount.getVTokenBurnScaledAmount(
            debtReserveCache.nextVariableBorrowIndex
          ),
          index: debtReserveCache.nextVariableBorrowIndex
        });
    }

    uint256 outstandingDebt = borrowerReserveDebt - actualDebtToLiquidate;
    if (hasNoCollateralLeft && outstandingDebt != 0) {
      debtReserve.deficit += outstandingDebt.toUint128();
      emit IPool.DeficitCreated(borrower, debtAsset, outstandingDebt);
    }

    if (noMoreDebt) {
      borrowerConfig.setBorrowing(debtReserve.id, false);
    }

    debtReserve.updateInterestRatesAndVirtualBalance(
      debtReserveCache,
      debtAsset,
      actualDebtToLiquidate,
      0,
      interestRateStrategyAddress
    );
  }

  struct AvailableCollateralToLiquidateLocalVars {
    uint256 maxCollateralToLiquidate;
    uint256 baseCollateral;
    uint256 bonusCollateral;
    uint256 collateralAmount;
    uint256 debtAmountNeeded;
    uint256 liquidationProtocolFeePercentage;
    uint256 liquidationProtocolFee;
    uint256 collateralToLiquidateInBaseCurrency;
    uint256 collateralAssetPrice;
  }

  /**
   * @notice Calculates how much of a specific collateral can be liquidated, given
   * a certain amount of debt asset.
   * @dev This function needs to be called after all the checks to validate the liquidation have been performed,
   *   otherwise it might fail.
   * @param collateralReserveConfiguration The data of the collateral reserve
   * @param collateralAssetPrice The price of the underlying asset used as collateral
   * @param collateralAssetUnit The asset units of the collateral
   * @param debtAssetPrice The price of the underlying borrowed asset to be repaid with the liquidation
   * @param debtAssetUnit The asset units of the debt
   * @param debtToCover The debt amount of borrowed `asset` the liquidator wants to cover
   * @param borrowerCollateralBalance The collateral balance for the specific `collateralAsset` of the user being liquidated
   * @param liquidationBonus The collateral bonus percentage to receive as result of the liquidation
   * @return The maximum amount that is possible to liquidate given all the liquidation constraints (user balance, close factor)
   * @return The amount to repay with the liquidation
   * @return The fee taken from the liquidation bonus amount to be paid to the protocol
   * @return The collateral amount to liquidate in the base currency used by the price feed
   */
  function _calculateAvailableCollateralToLiquidate(
    DataTypes.ReserveConfigurationMap memory collateralReserveConfiguration,
    uint256 collateralAssetPrice,
    uint256 collateralAssetUnit,
    uint256 debtAssetPrice,
    uint256 debtAssetUnit,
    uint256 debtToCover,
    uint256 borrowerCollateralBalance,
    uint256 liquidationBonus
  ) internal pure returns (uint256, uint256, uint256, uint256) {
    AvailableCollateralToLiquidateLocalVars memory vars;
    vars.collateralAssetPrice = collateralAssetPrice;
    vars.liquidationProtocolFeePercentage = collateralReserveConfiguration
      .getLiquidationProtocolFee();

    // This is the base collateral to liquidate based on the given debt to cover
    vars.baseCollateral =
      (debtAssetPrice * debtToCover * collateralAssetUnit) /
      (vars.collateralAssetPrice * debtAssetUnit);

    vars.maxCollateralToLiquidate = vars.baseCollateral.percentMul(liquidationBonus);

    if (vars.maxCollateralToLiquidate > borrowerCollateralBalance) {
      vars.collateralAmount = borrowerCollateralBalance;
      vars.debtAmountNeeded = ((vars.collateralAssetPrice * vars.collateralAmount * debtAssetUnit) /
        (debtAssetPrice * collateralAssetUnit)).percentDivCeil(liquidationBonus);
    } else {
      vars.collateralAmount = vars.maxCollateralToLiquidate;
      vars.debtAmountNeeded = debtToCover;
    }

    vars.collateralToLiquidateInBaseCurrency =
      (vars.collateralAmount * vars.collateralAssetPrice) /
      collateralAssetUnit;

    if (vars.liquidationProtocolFeePercentage != 0) {
      vars.bonusCollateral =
        vars.collateralAmount -
        vars.collateralAmount.percentDiv(liquidationBonus);

      vars.liquidationProtocolFee = vars.bonusCollateral.percentMul(
        vars.liquidationProtocolFeePercentage
      );
      vars.collateralAmount -= vars.liquidationProtocolFee;
    }
    return (
      vars.collateralAmount,
      vars.debtAmountNeeded,
      vars.liquidationProtocolFee,
      vars.collateralToLiquidateInBaseCurrency
    );
  }

  /**
   * @notice Remove a user's bad debt by burning debt tokens.
   * @dev This function iterates through all active reserves where the user has a debt position,
   * updates their state, and performs the necessary burn.
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param borrowerConfig The user configuration
   * @param params The txn params
   */
  function _burnBadDebt(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    DataTypes.UserConfigurationMap storage borrowerConfig,
    DataTypes.ExecuteLiquidationCallParams memory params
  ) internal {
    uint256 cachedBorrowerConfig = borrowerConfig.data;
    uint256 i = 0;
    bool isBorrowed = false;
    while (cachedBorrowerConfig != 0) {
      (cachedBorrowerConfig, isBorrowed, ) = UserConfiguration.getNextFlags(cachedBorrowerConfig);
      if (isBorrowed) {
        address reserveAddress = reservesList[i];
        if (reserveAddress != address(0)) {
          DataTypes.ReserveCache memory reserveCache = reservesData[reserveAddress].cache();
          if (reserveCache.reserveConfiguration.getActive()) {
            reservesData[reserveAddress].updateState(reserveCache);

            _burnDebtTokens(
              reserveCache,
              reservesData[reserveAddress],
              borrowerConfig,
              params.borrower,
              reserveAddress,
              IVariableDebtToken(reserveCache.variableDebtTokenAddress)
                .scaledBalanceOf(params.borrower)
                .getVTokenBalance(reserveCache.nextVariableBorrowIndex),
              0,
              true,
              params.interestRateStrategyAddress
            );
          }
        }
      }
      unchecked {
        ++i;
      }
    }
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IAaveIncentivesController} from './IAaveIncentivesController.sol';
import {IPool} from './IPool.sol';

/**
 * @title IInitializableDebtToken
 * @author Aave
 * @notice Interface for the initialize function common between debt tokens
 */
interface IInitializableDebtToken {
  /**
   * @dev Emitted when a debt token is initialized
   * @param underlyingAsset The address of the underlying asset
   * @param pool The address of the associated pool
   * @param incentivesController The address of the incentives controller for this aToken
   * @param debtTokenDecimals The decimals of the debt token
   * @param debtTokenName The name of the debt token
   * @param debtTokenSymbol The symbol of the debt token
   * @param params A set of encoded parameters for additional initialization
   */
  event Initialized(
    address indexed underlyingAsset,
    address indexed pool,
    address incentivesController,
    uint8 debtTokenDecimals,
    string debtTokenName,
    string debtTokenSymbol,
    bytes params
  );

  /**
   * @notice Initializes the debt token.
   * @param pool The pool contract that is initializing this contract
   * @param underlyingAsset The address of the underlying asset of this aToken (E.g. WETH for aWETH)
   * @param debtTokenDecimals The decimals of the debtToken, same as the underlying asset's
   * @param debtTokenName The name of the token
   * @param debtTokenSymbol The symbol of the token
   * @param params A set of encoded parameters for additional initialization
   */
  function initialize(
    IPool pool,
    address underlyingAsset,
    uint8 debtTokenDecimals,
    string memory debtTokenName,
    string memory debtTokenSymbol,
    bytes calldata params
  ) external;
}

