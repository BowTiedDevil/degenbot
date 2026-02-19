// SPDX-License-Identifier: MIT
pragma solidity ^0.8.10;

import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {Address} from '../../../dependencies/openzeppelin/contracts/Address.sol';
import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {IReserveInterestRateStrategy} from '../../../interfaces/IReserveInterestRateStrategy.sol';
import {IStableDebtToken} from '../../../interfaces/IStableDebtToken.sol';
import {IScaledBalanceToken} from '../../../interfaces/IScaledBalanceToken.sol';
import {IPriceOracleGetter} from '../../../interfaces/IPriceOracleGetter.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {IPriceOracleSentinel} from '../../../interfaces/IPriceOracleSentinel.sol';
import {IPoolAddressesProvider} from '../../../interfaces/IPoolAddressesProvider.sol';
import {IAccessControl} from '../../../dependencies/openzeppelin/contracts/IAccessControl.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {UserConfiguration} from '../configuration/UserConfiguration.sol';
import {Errors} from '../helpers/Errors.sol';
import {WadRayMath} from '../math/WadRayMath.sol';
import {PercentageMath} from '../math/PercentageMath.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {ReserveLogic} from './ReserveLogic.sol';
import {GenericLogic} from './GenericLogic.sol';
import {SafeCast} from '../../../dependencies/openzeppelin/contracts/SafeCast.sol';
import {IncentivizedERC20} from '../../tokenization/base/IncentivizedERC20.sol';

/**
 * @title ReserveLogic library
 * @author Aave
 * @notice Implements functions to validate the different actions of the protocol
 */
library ValidationLogic {
  using ReserveLogic for DataTypes.ReserveData;
  using WadRayMath for uint256;
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
   * @param amount The amount to be supplied
   */
  function validateSupply(
    DataTypes.ReserveCache memory reserveCache,
    DataTypes.ReserveData storage reserve,
    uint256 amount
  ) internal view {
    require(amount != 0, Errors.INVALID_AMOUNT);

    (bool isActive, bool isFrozen, , , bool isPaused) = reserveCache
      .reserveConfiguration
      .getFlags();
    require(isActive, Errors.RESERVE_INACTIVE);
    require(!isPaused, Errors.RESERVE_PAUSED);
    require(!isFrozen, Errors.RESERVE_FROZEN);

    uint256 supplyCap = reserveCache.reserveConfiguration.getSupplyCap();
    require(
      supplyCap == 0 ||
        ((IAToken(reserveCache.aTokenAddress).scaledTotalSupply() +
          uint256(reserve.accruedToTreasury)).rayMul(reserveCache.nextLiquidityIndex) + amount) <=
        supplyCap * (10 ** reserveCache.reserveConfiguration.getDecimals()),
      Errors.SUPPLY_CAP_EXCEEDED
    );
  }

  /**
   * @notice Validates a withdraw action.
   * @param reserveCache The cached data of the reserve
   * @param amount The amount to be withdrawn
   * @param userBalance The balance of the user
   */
  function validateWithdraw(
    DataTypes.ReserveCache memory reserveCache,
    uint256 amount,
    uint256 userBalance
  ) internal pure {
    require(amount != 0, Errors.INVALID_AMOUNT);
    require(amount <= userBalance, Errors.NOT_ENOUGH_AVAILABLE_USER_BALANCE);

    (bool isActive, , , , bool isPaused) = reserveCache.reserveConfiguration.getFlags();
    require(isActive, Errors.RESERVE_INACTIVE);
    require(!isPaused, Errors.RESERVE_PAUSED);
  }

  struct ValidateBorrowLocalVars {
    uint256 currentLtv;
    uint256 collateralNeededInBaseCurrency;
    uint256 userCollateralInBaseCurrency;
    uint256 userDebtInBaseCurrency;
    uint256 availableLiquidity;
    uint256 healthFactor;
    uint256 totalDebt;
    uint256 totalSupplyVariableDebt;
    uint256 reserveDecimals;
    uint256 borrowCap;
    uint256 amountInBaseCurrency;
    uint256 assetUnit;
    address eModePriceSource;
    address siloedBorrowingAddress;
    bool isActive;
    bool isFrozen;
    bool isPaused;
    bool borrowingEnabled;
    bool stableRateBorrowingEnabled;
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
    require(params.amount != 0, Errors.INVALID_AMOUNT);

    ValidateBorrowLocalVars memory vars;

    (
      vars.isActive,
      vars.isFrozen,
      vars.borrowingEnabled,
      vars.stableRateBorrowingEnabled,
      vars.isPaused
    ) = params.reserveCache.reserveConfiguration.getFlags();

    require(vars.isActive, Errors.RESERVE_INACTIVE);
    require(!vars.isPaused, Errors.RESERVE_PAUSED);
    require(!vars.isFrozen, Errors.RESERVE_FROZEN);
    require(vars.borrowingEnabled, Errors.BORROWING_NOT_ENABLED);

    require(
      params.priceOracleSentinel == address(0) ||
        IPriceOracleSentinel(params.priceOracleSentinel).isBorrowAllowed(),
      Errors.PRICE_ORACLE_SENTINEL_CHECK_FAILED
    );

    //validate interest rate mode
    require(
      params.interestRateMode == DataTypes.InterestRateMode.VARIABLE ||
        params.interestRateMode == DataTypes.InterestRateMode.STABLE,
      Errors.INVALID_INTEREST_RATE_MODE_SELECTED
    );

    vars.reserveDecimals = params.reserveCache.reserveConfiguration.getDecimals();
    vars.borrowCap = params.reserveCache.reserveConfiguration.getBorrowCap();
    unchecked {
      vars.assetUnit = 10 ** vars.reserveDecimals;
    }

    if (vars.borrowCap != 0) {
      vars.totalSupplyVariableDebt = params.reserveCache.currScaledVariableDebt.rayMul(
        params.reserveCache.nextVariableBorrowIndex
      );

      vars.totalDebt =
        params.reserveCache.currTotalStableDebt +
        vars.totalSupplyVariableDebt +
        params.amount;

      unchecked {
        require(vars.totalDebt <= vars.borrowCap * vars.assetUnit, Errors.BORROW_CAP_EXCEEDED);
      }
    }

    if (params.isolationModeActive) {
      // check that the asset being borrowed is borrowable in isolation mode AND
      // the total exposure is no bigger than the collateral debt ceiling
      require(
        params.reserveCache.reserveConfiguration.getBorrowableInIsolation(),
        Errors.ASSET_NOT_BORROWABLE_IN_ISOLATION
      );

      require(
        reservesData[params.isolationModeCollateralAddress].isolationModeTotalDebt +
          (params.amount /
            10 ** (vars.reserveDecimals - ReserveConfiguration.DEBT_CEILING_DECIMALS))
            .toUint128() <=
          params.isolationModeDebtCeiling,
        Errors.DEBT_CEILING_EXCEEDED
      );
    }

    if (params.userEModeCategory != 0) {
      require(
        params.reserveCache.reserveConfiguration.getEModeCategory() == params.userEModeCategory,
        Errors.INCONSISTENT_EMODE_CATEGORY
      );
      vars.eModePriceSource = eModeCategories[params.userEModeCategory].priceSource;
    }

    (
      vars.userCollateralInBaseCurrency,
      vars.userDebtInBaseCurrency,
      vars.currentLtv,
      ,
      vars.healthFactor,

    ) = GenericLogic.calculateUserAccountData(
      reservesData,
      reservesList,
      eModeCategories,
      DataTypes.CalculateUserAccountDataParams({
        userConfig: params.userConfig,
        reservesCount: params.reservesCount,
        user: params.userAddress,
        oracle: params.oracle,
        userEModeCategory: params.userEModeCategory
      })
    );

    require(vars.userCollateralInBaseCurrency != 0, Errors.COLLATERAL_BALANCE_IS_ZERO);
    require(vars.currentLtv != 0, Errors.LTV_VALIDATION_FAILED);

    require(
      vars.healthFactor > HEALTH_FACTOR_LIQUIDATION_THRESHOLD,
      Errors.HEALTH_FACTOR_LOWER_THAN_LIQUIDATION_THRESHOLD
    );

    vars.amountInBaseCurrency =
      IPriceOracleGetter(params.oracle).getAssetPrice(
        vars.eModePriceSource != address(0) ? vars.eModePriceSource : params.asset
      ) *
      params.amount;
    unchecked {
      vars.amountInBaseCurrency /= vars.assetUnit;
    }

    //add the current already borrowed amount to the amount requested to calculate the total collateral needed.
    vars.collateralNeededInBaseCurrency = (vars.userDebtInBaseCurrency + vars.amountInBaseCurrency)
      .percentDiv(vars.currentLtv); //LTV is calculated in percentage

    require(
      vars.collateralNeededInBaseCurrency <= vars.userCollateralInBaseCurrency,
      Errors.COLLATERAL_CANNOT_COVER_NEW_BORROW
    );

    /**
     * Following conditions need to be met if the user is borrowing at a stable rate:
     * 1. Reserve must be enabled for stable rate borrowing
     * 2. Users cannot borrow from the reserve if their collateral is (mostly) the same currency
     *    they are borrowing, to prevent abuses.
     * 3. Users will be able to borrow only a portion of the total available liquidity
     */

    if (params.interestRateMode == DataTypes.InterestRateMode.STABLE) {
      //check if the borrow mode is stable and if stable rate borrowing is enabled on this reserve

      require(vars.stableRateBorrowingEnabled, Errors.STABLE_BORROWING_NOT_ENABLED);

      require(
        !params.userConfig.isUsingAsCollateral(reservesData[params.asset].id) ||
          params.reserveCache.reserveConfiguration.getLtv() == 0 ||
          params.amount > IERC20(params.reserveCache.aTokenAddress).balanceOf(params.userAddress),
        Errors.COLLATERAL_SAME_AS_BORROWING_CURRENCY
      );

      vars.availableLiquidity = IERC20(params.asset).balanceOf(params.reserveCache.aTokenAddress);

      //calculate the max available loan size in stable rate mode as a percentage of the
      //available liquidity
      uint256 maxLoanSizeStable = vars.availableLiquidity.percentMul(params.maxStableLoanPercent);

      require(params.amount <= maxLoanSizeStable, Errors.AMOUNT_BIGGER_THAN_MAX_LOAN_SIZE_STABLE);
    }

    if (params.userConfig.isBorrowingAny()) {
      (vars.siloedBorrowingEnabled, vars.siloedBorrowingAddress) = params
        .userConfig
        .getSiloedBorrowingState(reservesData, reservesList);

      if (vars.siloedBorrowingEnabled) {
        require(vars.siloedBorrowingAddress == params.asset, Errors.SILOED_BORROWING_VIOLATION);
      } else {
        require(
          !params.reserveCache.reserveConfiguration.getSiloedBorrowing(),
          Errors.SILOED_BORROWING_VIOLATION
        );
      }
    }
  }

  /**
   * @notice Validates a repay action.
   * @param reserveCache The cached data of the reserve
   * @param amountSent The amount sent for the repayment. Can be an actual value or uint(-1)
   * @param interestRateMode The interest rate mode of the debt being repaid
   * @param onBehalfOf The address of the user msg.sender is repaying for
   * @param stableDebt The borrow balance of the user
   * @param variableDebt The borrow balance of the user
   */
  function validateRepay(
    DataTypes.ReserveCache memory reserveCache,
    uint256 amountSent,
    DataTypes.InterestRateMode interestRateMode,
    address onBehalfOf,
    uint256 stableDebt,
    uint256 variableDebt
  ) internal view {
    require(amountSent != 0, Errors.INVALID_AMOUNT);
    require(
      amountSent != type(uint256).max || msg.sender == onBehalfOf,
      Errors.NO_EXPLICIT_AMOUNT_TO_REPAY_ON_BEHALF
    );

    (bool isActive, , , , bool isPaused) = reserveCache.reserveConfiguration.getFlags();
    require(isActive, Errors.RESERVE_INACTIVE);
    require(!isPaused, Errors.RESERVE_PAUSED);

    require(
      (stableDebt != 0 && interestRateMode == DataTypes.InterestRateMode.STABLE) ||
        (variableDebt != 0 && interestRateMode == DataTypes.InterestRateMode.VARIABLE),
      Errors.NO_DEBT_OF_SELECTED_TYPE
    );
  }

  /**
   * @notice Validates a swap of borrow rate mode.
   * @param reserve The reserve state on which the user is swapping the rate
   * @param reserveCache The cached data of the reserve
   * @param userConfig The user reserves configuration
   * @param stableDebt The stable debt of the user
   * @param variableDebt The variable debt of the user
   * @param currentRateMode The rate mode of the debt being swapped
   */
  function validateSwapRateMode(
    DataTypes.ReserveData storage reserve,
    DataTypes.ReserveCache memory reserveCache,
    DataTypes.UserConfigurationMap storage userConfig,
    uint256 stableDebt,
    uint256 variableDebt,
    DataTypes.InterestRateMode currentRateMode
  ) internal view {
    (bool isActive, bool isFrozen, , bool stableRateEnabled, bool isPaused) = reserveCache
      .reserveConfiguration
      .getFlags();
    require(isActive, Errors.RESERVE_INACTIVE);
    require(!isPaused, Errors.RESERVE_PAUSED);
    require(!isFrozen, Errors.RESERVE_FROZEN);

    if (currentRateMode == DataTypes.InterestRateMode.STABLE) {
      require(stableDebt != 0, Errors.NO_OUTSTANDING_STABLE_DEBT);
    } else if (currentRateMode == DataTypes.InterestRateMode.VARIABLE) {
      require(variableDebt != 0, Errors.NO_OUTSTANDING_VARIABLE_DEBT);
      /**
       * user wants to swap to stable, before swapping we need to ensure that
       * 1. stable borrow rate is enabled on the reserve
       * 2. user is not trying to abuse the reserve by supplying
       * more collateral than he is borrowing, artificially lowering
       * the interest rate, borrowing at variable, and switching to stable
       */
      require(stableRateEnabled, Errors.STABLE_BORROWING_NOT_ENABLED);

      require(
        !userConfig.isUsingAsCollateral(reserve.id) ||
          reserveCache.reserveConfiguration.getLtv() == 0 ||
          stableDebt + variableDebt > IERC20(reserveCache.aTokenAddress).balanceOf(msg.sender),
        Errors.COLLATERAL_SAME_AS_BORROWING_CURRENCY
      );
    } else {
      revert(Errors.INVALID_INTEREST_RATE_MODE_SELECTED);
    }
  }

  /**
   * @notice Validates a stable borrow rate rebalance action.
   * @dev Rebalancing is accepted when depositors are earning <= 90% of their earnings in pure supply/demand market (variable rate only)
   * For this to be the case, there has to be quite large stable debt with an interest rate below the current variable rate.
   * @param reserve The reserve state on which the user is getting rebalanced
   * @param reserveCache The cached state of the reserve
   * @param reserveAddress The address of the reserve
   */
  function validateRebalanceStableBorrowRate(
    DataTypes.ReserveData storage reserve,
    DataTypes.ReserveCache memory reserveCache,
    address reserveAddress
  ) internal view {
    (bool isActive, , , , bool isPaused) = reserveCache.reserveConfiguration.getFlags();
    require(isActive, Errors.RESERVE_INACTIVE);
    require(!isPaused, Errors.RESERVE_PAUSED);

    uint256 totalDebt = IERC20(reserveCache.stableDebtTokenAddress).totalSupply() +
      IERC20(reserveCache.variableDebtTokenAddress).totalSupply();

    (uint256 liquidityRateVariableDebtOnly, , ) = IReserveInterestRateStrategy(
      reserve.interestRateStrategyAddress
    ).calculateInterestRates(
        DataTypes.CalculateInterestRatesParams({
          unbacked: reserve.unbacked,
          liquidityAdded: 0,
          liquidityTaken: 0,
          totalStableDebt: 0,
          totalVariableDebt: totalDebt,
          averageStableBorrowRate: 0,
          reserveFactor: reserveCache.reserveFactor,
          reserve: reserveAddress,
          aToken: reserveCache.aTokenAddress
        })
      );

    require(
      reserveCache.currLiquidityRate <=
        liquidityRateVariableDebtOnly.percentMul(REBALANCE_UP_LIQUIDITY_RATE_THRESHOLD),
      Errors.INTEREST_RATE_REBALANCE_CONDITIONS_NOT_MET
    );
  }

  /**
   * @notice Validates the action of setting an asset as collateral.
   * @param reserveCache The cached data of the reserve
   * @param userBalance The balance of the user
   */
  function validateSetUseReserveAsCollateral(
    DataTypes.ReserveCache memory reserveCache,
    uint256 userBalance
  ) internal pure {
    require(userBalance != 0, Errors.UNDERLYING_BALANCE_ZERO);

    (bool isActive, , , , bool isPaused) = reserveCache.reserveConfiguration.getFlags();
    require(isActive, Errors.RESERVE_INACTIVE);
    require(!isPaused, Errors.RESERVE_PAUSED);
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
    require(assets.length == amounts.length, Errors.INCONSISTENT_FLASHLOAN_PARAMS);
    for (uint256 i = 0; i < assets.length; i++) {
      validateFlashloanSimple(reservesData[assets[i]]);
    }
  }

  /**
   * @notice Validates a flashloan action.
   * @param reserve The state of the reserve
   */
  function validateFlashloanSimple(DataTypes.ReserveData storage reserve) internal view {
    DataTypes.ReserveConfigurationMap memory configuration = reserve.configuration;
    require(!configuration.getPaused(), Errors.RESERVE_PAUSED);
    require(configuration.getActive(), Errors.RESERVE_INACTIVE);
    require(configuration.getFlashLoanEnabled(), Errors.FLASHLOAN_DISABLED);
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
   * @param userConfig The user configuration mapping
   * @param collateralReserve The reserve data of the collateral
   * @param params Additional parameters needed for the validation
   */
  function validateLiquidationCall(
    DataTypes.UserConfigurationMap storage userConfig,
    DataTypes.ReserveData storage collateralReserve,
    DataTypes.ValidateLiquidationCallParams memory params
  ) internal view {
    ValidateLiquidationCallLocalVars memory vars;

    (vars.collateralReserveActive, , , , vars.collateralReservePaused) = collateralReserve
      .configuration
      .getFlags();

    (vars.principalReserveActive, , , , vars.principalReservePaused) = params
      .debtReserveCache
      .reserveConfiguration
      .getFlags();

    require(vars.collateralReserveActive && vars.principalReserveActive, Errors.RESERVE_INACTIVE);
    require(!vars.collateralReservePaused && !vars.principalReservePaused, Errors.RESERVE_PAUSED);

    require(
      params.priceOracleSentinel == address(0) ||
        params.healthFactor < MINIMUM_HEALTH_FACTOR_LIQUIDATION_THRESHOLD ||
        IPriceOracleSentinel(params.priceOracleSentinel).isLiquidationAllowed(),
      Errors.PRICE_ORACLE_SENTINEL_CHECK_FAILED
    );

    require(
      params.healthFactor < HEALTH_FACTOR_LIQUIDATION_THRESHOLD,
      Errors.HEALTH_FACTOR_NOT_BELOW_THRESHOLD
    );

    vars.isCollateralEnabled =
      collateralReserve.configuration.getLiquidationThreshold() != 0 &&
      userConfig.isUsingAsCollateral(collateralReserve.id);

    //if collateral isn't enabled as collateral by user, it cannot be liquidated
    require(vars.isCollateralEnabled, Errors.COLLATERAL_CANNOT_BE_LIQUIDATED);
    require(params.totalDebt != 0, Errors.SPECIFIED_CURRENCY_NOT_BORROWED_BY_USER);
  }

  /**
   * @notice Validates the health factor of a user.
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param userConfig The state of the user for the specific reserve
   * @param user The user to validate health factor of
   * @param userEModeCategory The users active efficiency mode category
   * @param reservesCount The number of available reserves
   * @param oracle The price oracle
   */
  function validateHealthFactor(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.UserConfigurationMap memory userConfig,
    address user,
    uint8 userEModeCategory,
    uint256 reservesCount,
    address oracle
  ) internal view returns (uint256, bool) {
    (, , , , uint256 healthFactor, bool hasZeroLtvCollateral) = GenericLogic
      .calculateUserAccountData(
        reservesData,
        reservesList,
        eModeCategories,
        DataTypes.CalculateUserAccountDataParams({
          userConfig: userConfig,
          reservesCount: reservesCount,
          user: user,
          oracle: oracle,
          userEModeCategory: userEModeCategory
        })
      );

    require(
      healthFactor >= HEALTH_FACTOR_LIQUIDATION_THRESHOLD,
      Errors.HEALTH_FACTOR_LOWER_THAN_LIQUIDATION_THRESHOLD
    );

    return (healthFactor, hasZeroLtvCollateral);
  }

  /**
   * @notice Validates the health factor of a user and the ltv of the asset being withdrawn.
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param userConfig The state of the user for the specific reserve
   * @param asset The asset for which the ltv will be validated
   * @param from The user from which the aTokens are being transferred
   * @param reservesCount The number of available reserves
   * @param oracle The price oracle
   * @param userEModeCategory The users active efficiency mode category
   */
  function validateHFAndLtv(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.UserConfigurationMap memory userConfig,
    address asset,
    address from,
    uint256 reservesCount,
    address oracle,
    uint8 userEModeCategory
  ) internal view {
    DataTypes.ReserveData memory reserve = reservesData[asset];

    (, bool hasZeroLtvCollateral) = validateHealthFactor(
      reservesData,
      reservesList,
      eModeCategories,
      userConfig,
      from,
      userEModeCategory,
      reservesCount,
      oracle
    );

    require(
      !hasZeroLtvCollateral || reserve.configuration.getLtv() == 0,
      Errors.LTV_VALIDATION_FAILED
    );
  }

  /**
   * @notice Validates a transfer action.
   * @param reserve The reserve object
   */
  function validateTransfer(DataTypes.ReserveData storage reserve) internal view {
    require(!reserve.configuration.getPaused(), Errors.RESERVE_PAUSED);
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
    require(asset != address(0), Errors.ZERO_ADDRESS_NOT_VALID);
    require(reserve.id != 0 || reservesList[0] == asset, Errors.ASSET_NOT_LISTED);
    require(IERC20(reserve.stableDebtTokenAddress).totalSupply() == 0, Errors.STABLE_DEBT_NOT_ZERO);
    require(
      IERC20(reserve.variableDebtTokenAddress).totalSupply() == 0,
      Errors.VARIABLE_DEBT_SUPPLY_NOT_ZERO
    );
    require(
      IERC20(reserve.aTokenAddress).totalSupply() == 0 && reserve.accruedToTreasury == 0,
      Errors.UNDERLYING_CLAIMABLE_RIGHTS_NOT_ZERO
    );
  }

  /**
   * @notice Validates the action of setting efficiency mode.
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories a mapping storing configurations for all efficiency mode categories
   * @param userConfig the user configuration
   * @param reservesCount The total number of valid reserves
   * @param categoryId The id of the category
   */
  function validateSetUserEMode(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.UserConfigurationMap memory userConfig,
    uint256 reservesCount,
    uint8 categoryId
  ) internal view {
    // category is invalid if the liq threshold is not set
    require(
      categoryId == 0 || eModeCategories[categoryId].liquidationThreshold != 0,
      Errors.INCONSISTENT_EMODE_CATEGORY
    );

    // eMode can always be enabled if the user hasn't supplied anything
    if (userConfig.isEmpty()) {
      return;
    }

    // if user is trying to set another category than default we require that
    // either the user is not borrowing, or it's borrowing assets of categoryId
    if (categoryId != 0) {
      unchecked {
        for (uint256 i = 0; i < reservesCount; i++) {
          if (userConfig.isBorrowing(i)) {
            DataTypes.ReserveConfigurationMap memory configuration = reservesData[reservesList[i]]
              .configuration;
            require(
              configuration.getEModeCategory() == categoryId,
              Errors.INCONSISTENT_EMODE_CATEGORY
            );
          }
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
   * transfer, mint unbacked, and liquidate
   * @dev This is used to ensure that isolated assets are not enabled as collateral automatically
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param userConfig the user configuration
   * @param reserveConfig The reserve configuration
   * @return True if the asset can be activated as collateral, false otherwise
   */
  function validateAutomaticUseAsCollateral(
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
          msg.sender
        )
      ) return false;
    }
    return validateUseAsCollateral(reservesData, reservesList, userConfig, reserveConfig);
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.10;

import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {IScaledBalanceToken} from '../../../interfaces/IScaledBalanceToken.sol';
import {IPriceOracleGetter} from '../../../interfaces/IPriceOracleGetter.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {UserConfiguration} from '../configuration/UserConfiguration.sol';
import {PercentageMath} from '../math/PercentageMath.sol';
import {WadRayMath} from '../math/WadRayMath.sol';
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
    uint256 eModeAssetPrice;
    uint256 eModeLtv;
    uint256 eModeLiqThreshold;
    uint256 eModeAssetCategory;
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
      (vars.eModeLtv, vars.eModeLiqThreshold, vars.eModeAssetPrice) = EModeLogic
        .getEModeConfiguration(
          eModeCategories[params.userEModeCategory],
          IPriceOracleGetter(params.oracle)
        );
    }

    while (vars.i < params.reservesCount) {
      if (!params.userConfig.isUsingAsCollateralOrBorrowing(vars.i)) {
        unchecked {
          ++vars.i;
        }
        continue;
      }

      vars.currentReserveAddress = reservesList[vars.i];

      if (vars.currentReserveAddress == address(0)) {
        unchecked {
          ++vars.i;
        }
        continue;
      }

      DataTypes.ReserveData storage currentReserve = reservesData[vars.currentReserveAddress];

      (
        vars.ltv,
        vars.liquidationThreshold,
        ,
        vars.decimals,
        ,
        vars.eModeAssetCategory
      ) = currentReserve.configuration.getParams();

      unchecked {
        vars.assetUnit = 10 ** vars.decimals;
      }

      vars.assetPrice = vars.eModeAssetPrice != 0 &&
        params.userEModeCategory == vars.eModeAssetCategory
        ? vars.eModeAssetPrice
        : IPriceOracleGetter(params.oracle).getAssetPrice(vars.currentReserveAddress);

      if (vars.liquidationThreshold != 0 && params.userConfig.isUsingAsCollateral(vars.i)) {
        vars.userBalanceInBaseCurrency = _getUserBalanceInBaseCurrency(
          params.user,
          currentReserve,
          vars.assetPrice,
          vars.assetUnit
        );

        vars.totalCollateralInBaseCurrency += vars.userBalanceInBaseCurrency;

        vars.isInEModeCategory = EModeLogic.isInEModeCategory(
          params.userEModeCategory,
          vars.eModeAssetCategory
        );

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

      if (params.userConfig.isBorrowing(vars.i)) {
        vars.totalDebtInBaseCurrency += _getUserDebtInBaseCurrency(
          params.user,
          currentReserve,
          vars.assetPrice,
          vars.assetUnit
        );
      }

      unchecked {
        ++vars.i;
      }
    }

    unchecked {
      vars.avgLtv = vars.totalCollateralInBaseCurrency != 0
        ? vars.avgLtv / vars.totalCollateralInBaseCurrency
        : 0;
      vars.avgLiquidationThreshold = vars.totalCollateralInBaseCurrency != 0
        ? vars.avgLiquidationThreshold / vars.totalCollateralInBaseCurrency
        : 0;
    }

    vars.healthFactor = (vars.totalDebtInBaseCurrency == 0)
      ? type(uint256).max
      : (vars.totalCollateralInBaseCurrency.percentMul(vars.avgLiquidationThreshold)).wadDiv(
        vars.totalDebtInBaseCurrency
      );
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
    uint256 availableBorrowsInBaseCurrency = totalCollateralInBaseCurrency.percentMul(ltv);

    if (availableBorrowsInBaseCurrency < totalDebtInBaseCurrency) {
      return 0;
    }

    availableBorrowsInBaseCurrency = availableBorrowsInBaseCurrency - totalDebtInBaseCurrency;
    return availableBorrowsInBaseCurrency;
  }

  /**
   * @notice Calculates total debt of the user in the based currency used to normalize the values of the assets
   * @dev This fetches the `balanceOf` of the stable and variable debt tokens for the user. For gas reasons, the
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
    // fetching variable debt
    uint256 userTotalDebt = IScaledBalanceToken(reserve.variableDebtTokenAddress).scaledBalanceOf(
      user
    );
    if (userTotalDebt != 0) {
      userTotalDebt = userTotalDebt.rayMul(reserve.getNormalizedDebt());
    }

    userTotalDebt = userTotalDebt + IERC20(reserve.stableDebtTokenAddress).balanceOf(user);

    userTotalDebt = assetPrice * userTotalDebt;

    unchecked {
      return userTotalDebt / assetUnit;
    }
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
    uint256 normalizedIncome = reserve.getNormalizedIncome();
    uint256 balance = (
      IScaledBalanceToken(reserve.aTokenAddress).scaledBalanceOf(user).rayMul(normalizedIncome)
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

import {VersionedInitializable} from '../libraries/aave-upgradeability/VersionedInitializable.sol';
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
import {BridgeLogic} from '../libraries/logic/BridgeLogic.sol';
import {IERC20WithPermit} from '../../interfaces/IERC20WithPermit.sol';
import {IPoolAddressesProvider} from '../../interfaces/IPoolAddressesProvider.sol';
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
 *   # Swap their loans between variable and stable rate
 *   # Enable/disable their supplied assets as collateral rebalance stable rate borrow positions
 *   # Liquidate positions
 *   # Execute Flash Loans
 * @dev To be covered by a proxy contract, owned by the PoolAddressesProvider of the specific market
 * @dev All admin functions are callable by the PoolConfigurator contract defined also in the
 *   PoolAddressesProvider
 */
contract Pool is VersionedInitializable, PoolStorage, IPool {
  using ReserveLogic for DataTypes.ReserveData;

  uint256 public constant POOL_REVISION = 0x3;
  IPoolAddressesProvider public immutable ADDRESSES_PROVIDER;

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
   * @dev Only bridge can call functions marked by this modifier.
   */
  modifier onlyBridge() {
    _onlyBridge();
    _;
  }

  function _onlyPoolConfigurator() internal view virtual {
    require(
      ADDRESSES_PROVIDER.getPoolConfigurator() == msg.sender,
      Errors.CALLER_NOT_POOL_CONFIGURATOR
    );
  }

  function _onlyPoolAdmin() internal view virtual {
    require(
      IACLManager(ADDRESSES_PROVIDER.getACLManager()).isPoolAdmin(msg.sender),
      Errors.CALLER_NOT_POOL_ADMIN
    );
  }

  function _onlyBridge() internal view virtual {
    require(
      IACLManager(ADDRESSES_PROVIDER.getACLManager()).isBridge(msg.sender),
      Errors.CALLER_NOT_BRIDGE
    );
  }

  function getRevision() internal pure virtual override returns (uint256) {
    return POOL_REVISION;
  }

  /**
   * @dev Constructor.
   * @param provider The address of the PoolAddressesProvider contract
   */
  constructor(IPoolAddressesProvider provider) {
    ADDRESSES_PROVIDER = provider;
  }

  /**
   * @notice Initializes the Pool.
   * @dev Function is invoked by the proxy contract when the Pool contract is added to the
   * PoolAddressesProvider of the market.
   * @dev Caching the address of the PoolAddressesProvider in order to reduce gas consumption on subsequent operations
   * @param provider The address of the PoolAddressesProvider
   */
  function initialize(IPoolAddressesProvider provider) external virtual initializer {
    require(provider == ADDRESSES_PROVIDER, Errors.INVALID_ADDRESSES_PROVIDER);
    _maxStableRateBorrowSizePercent = 0.25e4;
  }

  /// @inheritdoc IPool
  function mintUnbacked(
    address asset,
    uint256 amount,
    address onBehalfOf,
    uint16 referralCode
  ) external virtual override onlyBridge {
    BridgeLogic.executeMintUnbacked(
      _reserves,
      _reservesList,
      _usersConfig[onBehalfOf],
      asset,
      amount,
      onBehalfOf,
      referralCode
    );
  }

  /// @inheritdoc IPool
  function backUnbacked(
    address asset,
    uint256 amount,
    uint256 fee
  ) external virtual override onlyBridge returns (uint256) {
    return
      BridgeLogic.executeBackUnbacked(_reserves[asset], asset, amount, fee, _bridgeProtocolFee);
  }

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
        asset: asset,
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
    IERC20WithPermit(asset).permit(
      msg.sender,
      address(this),
      amount,
      deadline,
      permitV,
      permitR,
      permitS
    );
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
        _usersConfig[msg.sender],
        DataTypes.ExecuteWithdrawParams({
          asset: asset,
          amount: amount,
          to: to,
          reservesCount: _reservesCount,
          oracle: ADDRESSES_PROVIDER.getPriceOracle(),
          userEModeCategory: _usersEModeCategory[msg.sender]
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
        user: msg.sender,
        onBehalfOf: onBehalfOf,
        amount: amount,
        interestRateMode: DataTypes.InterestRateMode(interestRateMode),
        referralCode: referralCode,
        releaseUnderlying: true,
        maxStableRateBorrowSizePercent: _maxStableRateBorrowSizePercent,
        reservesCount: _reservesCount,
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
        _usersConfig[onBehalfOf],
        DataTypes.ExecuteRepayParams({
          asset: asset,
          amount: amount,
          interestRateMode: DataTypes.InterestRateMode(interestRateMode),
          onBehalfOf: onBehalfOf,
          useATokens: false
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
    {
      IERC20WithPermit(asset).permit(
        msg.sender,
        address(this),
        amount,
        deadline,
        permitV,
        permitR,
        permitS
      );
    }
    {
      DataTypes.ExecuteRepayParams memory params = DataTypes.ExecuteRepayParams({
        asset: asset,
        amount: amount,
        interestRateMode: DataTypes.InterestRateMode(interestRateMode),
        onBehalfOf: onBehalfOf,
        useATokens: false
      });
      return BorrowLogic.executeRepay(_reserves, _reservesList, _usersConfig[onBehalfOf], params);
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
        _usersConfig[msg.sender],
        DataTypes.ExecuteRepayParams({
          asset: asset,
          amount: amount,
          interestRateMode: DataTypes.InterestRateMode(interestRateMode),
          onBehalfOf: msg.sender,
          useATokens: true
        })
      );
  }

  /// @inheritdoc IPool
  function swapBorrowRateMode(address asset, uint256 interestRateMode) public virtual override {
    BorrowLogic.executeSwapBorrowRateMode(
      _reserves[asset],
      _usersConfig[msg.sender],
      asset,
      DataTypes.InterestRateMode(interestRateMode)
    );
  }

  /// @inheritdoc IPool
  function rebalanceStableBorrowRate(address asset, address user) public virtual override {
    BorrowLogic.executeRebalanceStableBorrowRate(_reserves[asset], asset, user);
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
      _usersConfig[msg.sender],
      asset,
      useAsCollateral,
      _reservesCount,
      ADDRESSES_PROVIDER.getPriceOracle(),
      _usersEModeCategory[msg.sender]
    );
  }

  /// @inheritdoc IPool
  function liquidationCall(
    address collateralAsset,
    address debtAsset,
    address user,
    uint256 debtToCover,
    bool receiveAToken
  ) public virtual override {
    LiquidationLogic.executeLiquidationCall(
      _reserves,
      _reservesList,
      _usersConfig,
      _eModeCategories,
      DataTypes.ExecuteLiquidationCallParams({
        reservesCount: _reservesCount,
        debtToCover: debtToCover,
        collateralAsset: collateralAsset,
        debtAsset: debtAsset,
        user: user,
        receiveAToken: receiveAToken,
        priceOracle: ADDRESSES_PROVIDER.getPriceOracle(),
        userEModeCategory: _usersEModeCategory[user],
        priceOracleSentinel: ADDRESSES_PROVIDER.getPriceOracleSentinel()
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
      receiverAddress: receiverAddress,
      assets: assets,
      amounts: amounts,
      interestRateModes: interestRateModes,
      onBehalfOf: onBehalfOf,
      params: params,
      referralCode: referralCode,
      flashLoanPremiumToProtocol: _flashLoanPremiumToProtocol,
      flashLoanPremiumTotal: _flashLoanPremiumTotal,
      maxStableRateBorrowSizePercent: _maxStableRateBorrowSizePercent,
      reservesCount: _reservesCount,
      addressesProvider: address(ADDRESSES_PROVIDER),
      pool: address(this),
      userEModeCategory: _usersEModeCategory[onBehalfOf],
      isAuthorizedFlashBorrower: IACLManager(ADDRESSES_PROVIDER.getACLManager()).isFlashBorrower(
        msg.sender
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
      receiverAddress: receiverAddress,
      asset: asset,
      amount: amount,
      params: params,
      referralCode: referralCode,
      flashLoanPremiumToProtocol: _flashLoanPremiumToProtocol,
      flashLoanPremiumTotal: _flashLoanPremiumTotal
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
  ) external view virtual override returns (DataTypes.ReserveData memory) {
    return _reserves[asset];
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
          reservesCount: _reservesCount,
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
  function MAX_STABLE_RATE_BORROW_SIZE_PERCENT() public view virtual override returns (uint256) {
    return _maxStableRateBorrowSizePercent;
  }

  /// @inheritdoc IPool
  function BRIDGE_PROTOCOL_FEE() public view virtual override returns (uint256) {
    return _bridgeProtocolFee;
  }

  /// @inheritdoc IPool
  function FLASHLOAN_PREMIUM_TOTAL() public view virtual override returns (uint128) {
    return _flashLoanPremiumTotal;
  }

  /// @inheritdoc IPool
  function FLASHLOAN_PREMIUM_TO_PROTOCOL() public view virtual override returns (uint128) {
    return _flashLoanPremiumToProtocol;
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
    uint256 amount,
    uint256 balanceFromBefore,
    uint256 balanceToBefore
  ) external virtual override {
    require(msg.sender == _reserves[asset].aTokenAddress, Errors.CALLER_NOT_ATOKEN);
    SupplyLogic.executeFinalizeTransfer(
      _reserves,
      _reservesList,
      _eModeCategories,
      _usersConfig,
      DataTypes.FinalizeTransferParams({
        asset: asset,
        from: from,
        to: to,
        amount: amount,
        balanceFromBefore: balanceFromBefore,
        balanceToBefore: balanceToBefore,
        reservesCount: _reservesCount,
        oracle: ADDRESSES_PROVIDER.getPriceOracle(),
        fromEModeCategory: _usersEModeCategory[from]
      })
    );
  }

  /// @inheritdoc IPool
  function initReserve(
    address asset,
    address aTokenAddress,
    address stableDebtAddress,
    address variableDebtAddress,
    address interestRateStrategyAddress
  ) external virtual override onlyPoolConfigurator {
    if (
      PoolLogic.executeInitReserve(
        _reserves,
        _reservesList,
        DataTypes.InitReserveParams({
          asset: asset,
          aTokenAddress: aTokenAddress,
          stableDebtAddress: stableDebtAddress,
          variableDebtAddress: variableDebtAddress,
          interestRateStrategyAddress: interestRateStrategyAddress,
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
  function setReserveInterestRateStrategyAddress(
    address asset,
    address rateStrategyAddress
  ) external virtual override onlyPoolConfigurator {
    require(asset != address(0), Errors.ZERO_ADDRESS_NOT_VALID);
    require(_reserves[asset].id != 0 || _reservesList[0] == asset, Errors.ASSET_NOT_LISTED);
    _reserves[asset].interestRateStrategyAddress = rateStrategyAddress;
  }

  /// @inheritdoc IPool
  function setConfiguration(
    address asset,
    DataTypes.ReserveConfigurationMap calldata configuration
  ) external virtual override onlyPoolConfigurator {
    require(asset != address(0), Errors.ZERO_ADDRESS_NOT_VALID);
    require(_reserves[asset].id != 0 || _reservesList[0] == asset, Errors.ASSET_NOT_LISTED);
    _reserves[asset].configuration = configuration;
  }

  /// @inheritdoc IPool
  function updateBridgeProtocolFee(
    uint256 protocolFee
  ) external virtual override onlyPoolConfigurator {
    _bridgeProtocolFee = protocolFee;
  }

  /// @inheritdoc IPool
  function updateFlashloanPremiums(
    uint128 flashLoanPremiumTotal,
    uint128 flashLoanPremiumToProtocol
  ) external virtual override onlyPoolConfigurator {
    _flashLoanPremiumTotal = flashLoanPremiumTotal;
    _flashLoanPremiumToProtocol = flashLoanPremiumToProtocol;
  }

  /// @inheritdoc IPool
  function configureEModeCategory(
    uint8 id,
    DataTypes.EModeCategory memory category
  ) external virtual override onlyPoolConfigurator {
    // category 0 is reserved for volatile heterogeneous assets and it's always disabled
    require(id != 0, Errors.EMODE_CATEGORY_RESERVED);
    _eModeCategories[id] = category;
  }

  /// @inheritdoc IPool
  function getEModeCategoryData(
    uint8 id
  ) external view virtual override returns (DataTypes.EModeCategory memory) {
    return _eModeCategories[id];
  }

  /// @inheritdoc IPool
  function setUserEMode(uint8 categoryId) external virtual override {
    EModeLogic.executeSetUserEMode(
      _reserves,
      _reservesList,
      _eModeCategories,
      _usersEModeCategory,
      _usersConfig[msg.sender],
      DataTypes.ExecuteSetUserEModeParams({
        reservesCount: _reservesCount,
        oracle: ADDRESSES_PROVIDER.getPriceOracle(),
        categoryId: categoryId
      })
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
        asset: asset,
        amount: amount,
        onBehalfOf: onBehalfOf,
        referralCode: referralCode
      })
    );
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.10;

import {Context} from '../../../dependencies/openzeppelin/contracts/Context.sol';
import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {IERC20Detailed} from '../../../dependencies/openzeppelin/contracts/IERC20Detailed.sol';
import {SafeCast} from '../../../dependencies/openzeppelin/contracts/SafeCast.sol';
import {WadRayMath} from '../../libraries/math/WadRayMath.sol';
import {Errors} from '../../libraries/helpers/Errors.sol';
import {IAaveIncentivesController} from '../../../interfaces/IAaveIncentivesController.sol';
import {IPoolAddressesProvider} from '../../../interfaces/IPoolAddressesProvider.sol';
import {IPool} from '../../../interfaces/IPool.sol';
import {IACLManager} from '../../../interfaces/IACLManager.sol';

/**
 * @title IncentivizedERC20
 * @author Aave, inspired by the Openzeppelin ERC20 implementation
 * @notice Basic ERC20 implementation
 */
abstract contract IncentivizedERC20 is Context, IERC20Detailed {
  using WadRayMath for uint256;
  using SafeCast for uint256;

  /**
   * @dev Only pool admin can call functions marked by this modifier.
   */
  modifier onlyPoolAdmin() {
    IACLManager aclManager = IACLManager(_addressesProvider.getACLManager());
    require(aclManager.isPoolAdmin(msg.sender), Errors.CALLER_NOT_POOL_ADMIN);
    _;
  }

  /**
   * @dev Only pool can call functions marked by this modifier.
   */
  modifier onlyPool() {
    require(_msgSender() == address(POOL), Errors.CALLER_MUST_BE_POOL);
    _;
  }

  /**
   * @dev UserState - additionalData is a flexible field.
   * ATokens and VariableDebtTokens use this field store the index of the
   * user's last supply/withdrawal/borrow/repayment. StableDebtTokens use
   * this field to store the user's stable rate.
   */
  struct UserState {
    uint128 balance;
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
  IAaveIncentivesController internal _incentivesController;
  IPoolAddressesProvider internal immutable _addressesProvider;
  IPool public immutable POOL;

  /**
   * @dev Constructor.
   * @param pool The reference to the main Pool contract
   * @param name The name of the token
   * @param symbol The symbol of the token
   * @param decimals The number of decimals of the token
   */
  constructor(IPool pool, string memory name, string memory symbol, uint8 decimals) {
    _addressesProvider = pool.ADDRESSES_PROVIDER();
    _name = name;
    _symbol = symbol;
    _decimals = decimals;
    POOL = pool;
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
    return _incentivesController;
  }

  /**
   * @notice Sets a new Incentives Controller
   * @param controller the new Incentives controller
   */
  function setIncentivesController(IAaveIncentivesController controller) external onlyPoolAdmin {
    _incentivesController = controller;
  }

  /// @inheritdoc IERC20
  function transfer(address recipient, uint256 amount) external virtual override returns (bool) {
    uint128 castAmount = amount.toUint128();
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
    uint128 castAmount = amount.toUint128();
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
   * @notice Transfers tokens between two users and apply incentives if defined.
   * @param sender The source address
   * @param recipient The destination address
   * @param amount The amount getting transferred
   */
  function _transfer(address sender, address recipient, uint128 amount) internal virtual {
    uint128 oldSenderBalance = _userState[sender].balance;
    _userState[sender].balance = oldSenderBalance - amount;
    uint128 oldRecipientBalance = _userState[recipient].balance;
    _userState[recipient].balance = oldRecipientBalance + amount;

    IAaveIncentivesController incentivesControllerLocal = _incentivesController;
    if (address(incentivesControllerLocal) != address(0)) {
      uint256 currentTotalSupply = _totalSupply;
      incentivesControllerLocal.handleAction(sender, currentTotalSupply, oldSenderBalance);
      if (sender != recipient) {
        incentivesControllerLocal.handleAction(recipient, currentTotalSupply, oldRecipientBalance);
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
pragma solidity ^0.8.10;

import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {SafeCast} from '../../../dependencies/openzeppelin/contracts/SafeCast.sol';
import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {IPool} from '../../../interfaces/IPool.sol';
import {IFlashLoanReceiver} from '../../../flashloan/interfaces/IFlashLoanReceiver.sol';
import {IFlashLoanSimpleReceiver} from '../../../flashloan/interfaces/IFlashLoanSimpleReceiver.sol';
import {IPoolAddressesProvider} from '../../../interfaces/IPoolAddressesProvider.sol';
import {UserConfiguration} from '../configuration/UserConfiguration.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {Errors} from '../helpers/Errors.sol';
import {WadRayMath} from '../math/WadRayMath.sol';
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
  using WadRayMath for uint256;
  using PercentageMath for uint256;
  using SafeCast for uint256;

  // See `IPool` for descriptions
  event FlashLoan(
    address indexed target,
    address initiator,
    address indexed asset,
    uint256 amount,
    DataTypes.InterestRateMode interestRateMode,
    uint256 premium,
    uint16 indexed referralCode
  );

  // Helper struct for internal variables used in the `executeFlashLoan` function
  struct FlashLoanLocalVars {
    IFlashLoanReceiver receiver;
    uint256 i;
    address currentAsset;
    uint256 currentAmount;
    uint256[] totalPremiums;
    uint256 flashloanPremiumTotal;
    uint256 flashloanPremiumToProtocol;
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
    (vars.flashloanPremiumTotal, vars.flashloanPremiumToProtocol) = params.isAuthorizedFlashBorrower
      ? (0, 0)
      : (params.flashLoanPremiumTotal, params.flashLoanPremiumToProtocol);

    for (vars.i = 0; vars.i < params.assets.length; vars.i++) {
      vars.currentAmount = params.amounts[vars.i];
      vars.totalPremiums[vars.i] = DataTypes.InterestRateMode(params.interestRateModes[vars.i]) ==
        DataTypes.InterestRateMode.NONE
        ? vars.currentAmount.percentMul(vars.flashloanPremiumTotal)
        : 0;
      IAToken(reservesData[params.assets[vars.i]].aTokenAddress).transferUnderlyingTo(
        params.receiverAddress,
        vars.currentAmount
      );
    }

    require(
      vars.receiver.executeOperation(
        params.assets,
        params.amounts,
        vars.totalPremiums,
        msg.sender,
        params.params
      ),
      Errors.INVALID_FLASHLOAN_EXECUTOR_RETURN
    );

    for (vars.i = 0; vars.i < params.assets.length; vars.i++) {
      vars.currentAsset = params.assets[vars.i];
      vars.currentAmount = params.amounts[vars.i];

      if (
        DataTypes.InterestRateMode(params.interestRateModes[vars.i]) ==
        DataTypes.InterestRateMode.NONE
      ) {
        _handleFlashLoanRepayment(
          reservesData[vars.currentAsset],
          DataTypes.FlashLoanRepaymentParams({
            asset: vars.currentAsset,
            receiverAddress: params.receiverAddress,
            amount: vars.currentAmount,
            totalPremium: vars.totalPremiums[vars.i],
            flashLoanPremiumToProtocol: vars.flashloanPremiumToProtocol,
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
            user: msg.sender,
            onBehalfOf: params.onBehalfOf,
            amount: vars.currentAmount,
            interestRateMode: DataTypes.InterestRateMode(params.interestRateModes[vars.i]),
            referralCode: params.referralCode,
            releaseUnderlying: false,
            maxStableRateBorrowSizePercent: IPool(params.pool).MAX_STABLE_RATE_BORROW_SIZE_PERCENT(),
            reservesCount: IPool(params.pool).getReservesCount(),
            oracle: IPoolAddressesProvider(params.addressesProvider).getPriceOracle(),
            userEModeCategory: IPool(params.pool).getUserEMode(params.onBehalfOf).toUint8(),
            priceOracleSentinel: IPoolAddressesProvider(params.addressesProvider)
              .getPriceOracleSentinel()
          })
        );
        // no premium is paid when taking on the flashloan as debt
        emit FlashLoan(
          params.receiverAddress,
          msg.sender,
          vars.currentAsset,
          vars.currentAmount,
          DataTypes.InterestRateMode(params.interestRateModes[vars.i]),
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

    ValidationLogic.validateFlashloanSimple(reserve);

    IFlashLoanSimpleReceiver receiver = IFlashLoanSimpleReceiver(params.receiverAddress);
    uint256 totalPremium = params.amount.percentMul(params.flashLoanPremiumTotal);
    IAToken(reserve.aTokenAddress).transferUnderlyingTo(params.receiverAddress, params.amount);

    require(
      receiver.executeOperation(
        params.asset,
        params.amount,
        totalPremium,
        msg.sender,
        params.params
      ),
      Errors.INVALID_FLASHLOAN_EXECUTOR_RETURN
    );

    _handleFlashLoanRepayment(
      reserve,
      DataTypes.FlashLoanRepaymentParams({
        asset: params.asset,
        receiverAddress: params.receiverAddress,
        amount: params.amount,
        totalPremium: totalPremium,
        flashLoanPremiumToProtocol: params.flashLoanPremiumToProtocol,
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
    uint256 premiumToProtocol = params.totalPremium.percentMul(params.flashLoanPremiumToProtocol);
    uint256 premiumToLP = params.totalPremium - premiumToProtocol;
    uint256 amountPlusPremium = params.amount + params.totalPremium;

    DataTypes.ReserveCache memory reserveCache = reserve.cache();
    reserve.updateState(reserveCache);
    reserveCache.nextLiquidityIndex = reserve.cumulateToLiquidityIndex(
      IERC20(reserveCache.aTokenAddress).totalSupply() +
        uint256(reserve.accruedToTreasury).rayMul(reserveCache.nextLiquidityIndex),
      premiumToLP
    );

    reserve.accruedToTreasury += premiumToProtocol
      .rayDiv(reserveCache.nextLiquidityIndex)
      .toUint128();

    reserve.updateInterestRates(reserveCache, params.asset, amountPlusPremium, 0);

    IERC20(params.asset).safeTransferFrom(
      params.receiverAddress,
      reserveCache.aTokenAddress,
      amountPlusPremium
    );

    IAToken(reserveCache.aTokenAddress).handleRepayment(
      params.receiverAddress,
      params.receiverAddress,
      amountPlusPremium
    );

    emit FlashLoan(
      params.receiverAddress,
      msg.sender,
      params.asset,
      params.amount,
      DataTypes.InterestRateMode(0),
      params.totalPremium,
      params.referralCode
    );
  }
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

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IInitializableDebtToken} from './IInitializableDebtToken.sol';

/**
 * @title IStableDebtToken
 * @author Aave
 * @notice Defines the interface for the stable debt token
 * @dev It does not inherit from IERC20 to save in code size
 */
interface IStableDebtToken is IInitializableDebtToken {
  /**
   * @dev Emitted when new stable debt is minted
   * @param user The address of the user who triggered the minting
   * @param onBehalfOf The recipient of stable debt tokens
   * @param amount The amount minted (user entered amount + balance increase from interest)
   * @param currentBalance The balance of the user based on the previous balance and balance increase from interest
   * @param balanceIncrease The increase in balance since the last action of the user 'onBehalfOf'
   * @param newRate The rate of the debt after the minting
   * @param avgStableRate The next average stable rate after the minting
   * @param newTotalSupply The next total supply of the stable debt token after the action
   */
  event Mint(
    address indexed user,
    address indexed onBehalfOf,
    uint256 amount,
    uint256 currentBalance,
    uint256 balanceIncrease,
    uint256 newRate,
    uint256 avgStableRate,
    uint256 newTotalSupply
  );

  /**
   * @dev Emitted when new stable debt is burned
   * @param from The address from which the debt will be burned
   * @param amount The amount being burned (user entered amount - balance increase from interest)
   * @param currentBalance The balance of the user based on the previous balance and balance increase from interest
   * @param balanceIncrease The increase in balance since the last action of 'from'
   * @param avgStableRate The next average stable rate after the burning
   * @param newTotalSupply The next total supply of the stable debt token after the action
   */
  event Burn(
    address indexed from,
    uint256 amount,
    uint256 currentBalance,
    uint256 balanceIncrease,
    uint256 avgStableRate,
    uint256 newTotalSupply
  );

  /**
   * @notice Mints debt token to the `onBehalfOf` address.
   * @dev The resulting rate is the weighted average between the rate of the new debt
   * and the rate of the previous debt
   * @param user The address receiving the borrowed underlying, being the delegatee in case
   * of credit delegate, or same as `onBehalfOf` otherwise
   * @param onBehalfOf The address receiving the debt tokens
   * @param amount The amount of debt tokens to mint
   * @param rate The rate of the debt being minted
   * @return True if it is the first borrow, false otherwise
   * @return The total stable debt
   * @return The average stable borrow rate
   */
  function mint(
    address user,
    address onBehalfOf,
    uint256 amount,
    uint256 rate
  ) external returns (bool, uint256, uint256);

  /**
   * @notice Burns debt of `user`
   * @dev The resulting rate is the weighted average between the rate of the new debt
   * and the rate of the previous debt
   * @dev In some instances, a burn transaction will emit a mint event
   * if the amount to burn is less than the interest the user earned
   * @param from The address from which the debt will be burned
   * @param amount The amount of debt tokens getting burned
   * @return The total stable debt
   * @return The average stable borrow rate
   */
  function burn(address from, uint256 amount) external returns (uint256, uint256);

  /**
   * @notice Returns the average rate of all the stable rate loans.
   * @return The average stable rate
   */
  function getAverageStableRate() external view returns (uint256);

  /**
   * @notice Returns the stable rate of the user debt
   * @param user The address of the user
   * @return The stable rate of the user
   */
  function getUserStableRate(address user) external view returns (uint256);

  /**
   * @notice Returns the timestamp of the last update of the user
   * @param user The address of the user
   * @return The timestamp
   */
  function getUserLastUpdated(address user) external view returns (uint40);

  /**
   * @notice Returns the principal, the total supply, the average stable rate and the timestamp for the last update
   * @return The principal
   * @return The total supply
   * @return The average stable rate
   * @return The timestamp of the last update
   */
  function getSupplyData() external view returns (uint256, uint256, uint256, uint40);

  /**
   * @notice Returns the timestamp of the last update of the total supply
   * @return The timestamp
   */
  function getTotalSupplyLastUpdated() external view returns (uint40);

  /**
   * @notice Returns the total supply and the average stable rate
   * @return The total supply
   * @return The average rate
   */
  function getTotalSupplyAndAvgRate() external view returns (uint256, uint256);

  /**
   * @notice Returns the principal debt balance of the user
   * @return The debt balance of the user since the last burn/mint action
   */
  function principalBalanceOf(address user) external view returns (uint256);

  /**
   * @notice Returns the address of the underlying asset of this stableDebtToken (E.g. WETH for stableDebtWETH)
   * @return The address of the underlying asset
   */
  function UNDERLYING_ASSET_ADDRESS() external view returns (address);
}

// SPDX-License-Identifier: MIT
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
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.10;

import {DataTypes} from '../types/DataTypes.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {UserConfiguration} from '../configuration/UserConfiguration.sol';
import {SafeCast} from '../../../dependencies/openzeppelin/contracts/SafeCast.sol';

/**
 * @title IsolationModeLogic library
 * @author Aave
 * @notice Implements the base logic for handling repayments for assets borrowed in isolation mode
 */
library IsolationModeLogic {
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;
  using UserConfiguration for DataTypes.UserConfigurationMap;
  using SafeCast for uint256;

  // See `IPool` for descriptions
  event IsolationModeTotalDebtUpdated(address indexed asset, uint256 totalDebt);

  /**
   * @notice updated the isolated debt whenever a position collateralized by an isolated asset is repaid or liquidated
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param userConfig The user configuration mapping
   * @param reserveCache The cached data of the reserve
   * @param repayAmount The amount being repaid
   */
  function updateIsolatedDebtIfIsolated(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    DataTypes.UserConfigurationMap storage userConfig,
    DataTypes.ReserveCache memory reserveCache,
    uint256 repayAmount
  ) internal {
    (bool isolationModeActive, address isolationModeCollateralAddress, ) = userConfig
      .getIsolationModeState(reservesData, reservesList);

    if (isolationModeActive) {
      uint128 isolationModeTotalDebt = reservesData[isolationModeCollateralAddress]
        .isolationModeTotalDebt;

      uint128 isolatedDebtRepaid = (repayAmount /
        10 **
          (reserveCache.reserveConfiguration.getDecimals() -
            ReserveConfiguration.DEBT_CEILING_DECIMALS)).toUint128();

      // since the debt ceiling does not take into account the interest accrued, it might happen that amount
      // repaid > debt in isolation mode
      if (isolationModeTotalDebt <= isolatedDebtRepaid) {
        reservesData[isolationModeCollateralAddress].isolationModeTotalDebt = 0;
        emit IsolationModeTotalDebtUpdated(isolationModeCollateralAddress, 0);
      } else {
        uint256 nextIsolationModeTotalDebt = reservesData[isolationModeCollateralAddress]
          .isolationModeTotalDebt = isolationModeTotalDebt - isolatedDebtRepaid;
        emit IsolationModeTotalDebtUpdated(
          isolationModeCollateralAddress,
          nextIsolationModeTotalDebt
        );
      }
    }
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.10;

import {IERC20} from './IERC20.sol';

interface IERC20Detailed is IERC20 {
  function name() external view returns (string memory);

  function symbol() external view returns (string memory);

  function decimals() external view returns (uint8);
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
   * @dev Emitted on mintUnbacked()
   * @param reserve The address of the underlying asset of the reserve
   * @param user The address initiating the supply
   * @param onBehalfOf The beneficiary of the supplied assets, receiving the aTokens
   * @param amount The amount of supplied assets
   * @param referralCode The referral code used
   */
  event MintUnbacked(
    address indexed reserve,
    address user,
    address indexed onBehalfOf,
    uint256 amount,
    uint16 indexed referralCode
  );

  /**
   * @dev Emitted on backUnbacked()
   * @param reserve The address of the underlying asset of the reserve
   * @param backer The address paying for the backing
   * @param amount The amount added as backing
   * @param fee The amount paid in fees
   */
  event BackUnbacked(address indexed reserve, address indexed backer, uint256 amount, uint256 fee);

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
   * @param interestRateMode The rate mode: 1 for Stable, 2 for Variable
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
   * @dev Emitted on swapBorrowRateMode()
   * @param reserve The address of the underlying asset of the reserve
   * @param user The address of the user swapping his rate mode
   * @param interestRateMode The current interest rate mode of the position being swapped: 1 for Stable, 2 for Variable
   */
  event SwapBorrowRateMode(
    address indexed reserve,
    address indexed user,
    DataTypes.InterestRateMode interestRateMode
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
   * @dev Emitted on rebalanceStableBorrowRate()
   * @param reserve The address of the underlying asset of the reserve
   * @param user The address of the user for which the rebalance has been executed
   */
  event RebalanceStableBorrowRate(address indexed reserve, address indexed user);

  /**
   * @dev Emitted on flashLoan()
   * @param target The address of the flash loan receiver contract
   * @param initiator The address initiating the flash loan
   * @param asset The address of the asset being flash borrowed
   * @param amount The amount flash borrowed
   * @param interestRateMode The flashloan mode: 0 for regular flashloan, 1 for Stable debt, 2 for Variable debt
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
   * @param stableBorrowRate The next stable borrow rate
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
   * @dev Emitted when the protocol treasury receives minted aTokens from the accrued interest.
   * @param reserve The address of the reserve
   * @param amountMinted The amount minted to the treasury
   */
  event MintedToTreasury(address indexed reserve, uint256 amountMinted);

  /**
   * @notice Mints an `amount` of aTokens to the `onBehalfOf`
   * @param asset The address of the underlying asset to mint
   * @param amount The amount to mint
   * @param onBehalfOf The address that will receive the aTokens
   * @param referralCode Code used to register the integrator originating the operation, for potential rewards.
   *   0 if the action is executed directly by the user, without any middle-man
   */
  function mintUnbacked(
    address asset,
    uint256 amount,
    address onBehalfOf,
    uint16 referralCode
  ) external;

  /**
   * @notice Back the current unbacked underlying with `amount` and pay `fee`.
   * @param asset The address of the underlying asset to back
   * @param amount The amount to back
   * @param fee The amount paid in fees
   * @return The backed amount
   */
  function backUnbacked(address asset, uint256 amount, uint256 fee) external returns (uint256);

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
   * already supplied enough collateral, or he was given enough allowance by a credit delegator on the
   * corresponding debt token (StableDebtToken or VariableDebtToken)
   * - E.g. User borrows 100 USDC passing as `onBehalfOf` his own address, receiving the 100 USDC in his wallet
   *   and 100 stable/variable debt tokens, depending on the `interestRateMode`
   * @param asset The address of the underlying asset to borrow
   * @param amount The amount to be borrowed
   * @param interestRateMode The interest rate mode at which the user wants to borrow: 1 for Stable, 2 for Variable
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
   * - E.g. User repays 100 USDC, burning 100 variable/stable debt tokens of the `onBehalfOf` address
   * @param asset The address of the borrowed underlying asset previously borrowed
   * @param amount The amount to repay
   * - Send the value type(uint256).max in order to repay the whole debt for `asset` on the specific `debtMode`
   * @param interestRateMode The interest rate mode at of the debt the user wants to repay: 1 for Stable, 2 for Variable
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
   * @param interestRateMode The interest rate mode at of the debt the user wants to repay: 1 for Stable, 2 for Variable
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
   * - E.g. User repays 100 USDC using 100 aUSDC, burning 100 variable/stable debt tokens
   * @dev  Passing uint256.max as amount will clean up any residual aToken dust balance, if the user aToken
   * balance is not enough to cover the whole debt
   * @param asset The address of the borrowed underlying asset previously borrowed
   * @param amount The amount to repay
   * - Send the value type(uint256).max in order to repay the whole debt for `asset` on the specific `debtMode`
   * @param interestRateMode The interest rate mode at of the debt the user wants to repay: 1 for Stable, 2 for Variable
   * @return The final amount repaid
   */
  function repayWithATokens(
    address asset,
    uint256 amount,
    uint256 interestRateMode
  ) external returns (uint256);

  /**
   * @notice Allows a borrower to swap his debt between stable and variable mode, or vice versa
   * @param asset The address of the underlying asset borrowed
   * @param interestRateMode The current interest rate mode of the position being swapped: 1 for Stable, 2 for Variable
   */
  function swapBorrowRateMode(address asset, uint256 interestRateMode) external;

  /**
   * @notice Rebalances the stable interest rate of a user to the current stable rate defined on the reserve.
   * - Users can be rebalanced if the following conditions are satisfied:
   *     1. Usage ratio is above 95%
   *     2. the current supply APY is below REBALANCE_UP_THRESHOLD * maxVariableBorrowRate, which means that too
   *        much has been borrowed at a stable rate and suppliers are not earning enough
   * @param asset The address of the underlying asset borrowed
   * @param user The address of the user to be rebalanced
   */
  function rebalanceStableBorrowRate(address asset, address user) external;

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
   * @param user The address of the borrower getting liquidated
   * @param debtToCover The debt amount of borrowed `asset` the liquidator wants to cover
   * @param receiveAToken True if the liquidators wants to receive the collateral aTokens, `false` if he wants
   * to receive the underlying collateral asset directly
   */
  function liquidationCall(
    address collateralAsset,
    address debtAsset,
    address user,
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
   *   1 -> Open debt at stable rate for the value of the amount flash-borrowed to the `onBehalfOf` address
   *   2 -> Open debt at variable rate for the value of the amount flash-borrowed to the `onBehalfOf` address
   * @param onBehalfOf The address  that will receive the debt in the case of using on `modes` 1 or 2
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
   * @notice Initializes a reserve, activating it, assigning an aToken and debt tokens and an
   * interest rate strategy
   * @dev Only callable by the PoolConfigurator contract
   * @param asset The address of the underlying asset of the reserve
   * @param aTokenAddress The address of the aToken that will be assigned to the reserve
   * @param stableDebtAddress The address of the StableDebtToken that will be assigned to the reserve
   * @param variableDebtAddress The address of the VariableDebtToken that will be assigned to the reserve
   * @param interestRateStrategyAddress The address of the interest rate strategy contract
   */
  function initReserve(
    address asset,
    address aTokenAddress,
    address stableDebtAddress,
    address variableDebtAddress,
    address interestRateStrategyAddress
  ) external;

  /**
   * @notice Drop a reserve
   * @dev Only callable by the PoolConfigurator contract
   * @param asset The address of the underlying asset of the reserve
   */
  function dropReserve(address asset) external;

  /**
   * @notice Updates the address of the interest rate strategy contract
   * @dev Only callable by the PoolConfigurator contract
   * @param asset The address of the underlying asset of the reserve
   * @param rateStrategyAddress The address of the interest rate strategy contract
   */
  function setReserveInterestRateStrategyAddress(
    address asset,
    address rateStrategyAddress
  ) external;

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
  function getReserveData(address asset) external view returns (DataTypes.ReserveData memory);

  /**
   * @notice Validates and finalizes an aToken transfer
   * @dev Only callable by the overlying aToken of the `asset`
   * @param asset The address of the underlying asset of the aToken
   * @param from The user from which the aTokens are transferred
   * @param to The user receiving the aTokens
   * @param amount The amount being transferred/withdrawn
   * @param balanceFromBefore The aToken balance of the `from` user before the transfer
   * @param balanceToBefore The aToken balance of the `to` user before the transfer
   */
  function finalizeTransfer(
    address asset,
    address from,
    address to,
    uint256 amount,
    uint256 balanceFromBefore,
    uint256 balanceToBefore
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
   * @notice Updates the protocol fee on the bridging
   * @param bridgeProtocolFee The part of the premium sent to the protocol treasury
   */
  function updateBridgeProtocolFee(uint256 bridgeProtocolFee) external;

  /**
   * @notice Updates flash loan premiums. Flash loan premium consists of two parts:
   * - A part is sent to aToken holders as extra, one time accumulated interest
   * - A part is collected by the protocol treasury
   * @dev The total premium is calculated on the total borrowed amount
   * @dev The premium to protocol is calculated on the total premium, being a percentage of `flashLoanPremiumTotal`
   * @dev Only callable by the PoolConfigurator contract
   * @param flashLoanPremiumTotal The total premium, expressed in bps
   * @param flashLoanPremiumToProtocol The part of the premium sent to the protocol treasury, expressed in bps
   */
  function updateFlashloanPremiums(
    uint128 flashLoanPremiumTotal,
    uint128 flashLoanPremiumToProtocol
  ) external;

  /**
   * @notice Configures a new category for the eMode.
   * @dev In eMode, the protocol allows very high borrowing power to borrow assets of the same category.
   * The category 0 is reserved as it's the default for volatile assets
   * @param id The id of the category
   * @param config The configuration of the category
   */
  function configureEModeCategory(uint8 id, DataTypes.EModeCategory memory config) external;

  /**
   * @notice Returns the data of an eMode category
   * @param id The id of the category
   * @return The configuration data of the category
   */
  function getEModeCategoryData(uint8 id) external view returns (DataTypes.EModeCategory memory);

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
   * @notice Returns the percentage of available liquidity that can be borrowed at once at stable rate
   * @return The percentage of available liquidity to borrow, expressed in bps
   */
  function MAX_STABLE_RATE_BORROW_SIZE_PERCENT() external view returns (uint256);

  /**
   * @notice Returns the total fee on flash loans
   * @return The total fee on flashloans
   */
  function FLASHLOAN_PREMIUM_TOTAL() external view returns (uint128);

  /**
   * @notice Returns the part of the bridge fees sent to protocol
   * @return The bridge fee sent to the protocol treasury
   */
  function BRIDGE_PROTOCOL_FEE() external view returns (uint256);

  /**
   * @notice Returns the part of the flashloan fees sent to protocol
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
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.10;

import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {SafeCast} from '../../../dependencies/openzeppelin/contracts/SafeCast.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {UserConfiguration} from '../configuration/UserConfiguration.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {WadRayMath} from '../math/WadRayMath.sol';
import {PercentageMath} from '../math/PercentageMath.sol';
import {Errors} from '../helpers/Errors.sol';
import {ValidationLogic} from './ValidationLogic.sol';
import {ReserveLogic} from './ReserveLogic.sol';

library BridgeLogic {
  using ReserveLogic for DataTypes.ReserveCache;
  using ReserveLogic for DataTypes.ReserveData;
  using UserConfiguration for DataTypes.UserConfigurationMap;
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;
  using WadRayMath for uint256;
  using PercentageMath for uint256;
  using SafeCast for uint256;
  using GPv2SafeERC20 for IERC20;

  // See `IPool` for descriptions
  event ReserveUsedAsCollateralEnabled(address indexed reserve, address indexed user);
  event MintUnbacked(
    address indexed reserve,
    address user,
    address indexed onBehalfOf,
    uint256 amount,
    uint16 indexed referralCode
  );
  event BackUnbacked(address indexed reserve, address indexed backer, uint256 amount, uint256 fee);

  /**
   * @notice Mint unbacked aTokens to a user and updates the unbacked for the reserve.
   * @dev Essentially a supply without transferring the underlying.
   * @dev Emits the `MintUnbacked` event
   * @dev Emits the `ReserveUsedAsCollateralEnabled` if asset is set as collateral
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param userConfig The user configuration mapping that tracks the supplied/borrowed assets
   * @param asset The address of the underlying asset to mint aTokens of
   * @param amount The amount to mint
   * @param onBehalfOf The address that will receive the aTokens
   * @param referralCode Code used to register the integrator originating the operation, for potential rewards.
   *   0 if the action is executed directly by the user, without any middle-man
   */
  function executeMintUnbacked(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    DataTypes.UserConfigurationMap storage userConfig,
    address asset,
    uint256 amount,
    address onBehalfOf,
    uint16 referralCode
  ) external {
    DataTypes.ReserveData storage reserve = reservesData[asset];
    DataTypes.ReserveCache memory reserveCache = reserve.cache();

    reserve.updateState(reserveCache);

    ValidationLogic.validateSupply(reserveCache, reserve, amount);

    uint256 unbackedMintCap = reserveCache.reserveConfiguration.getUnbackedMintCap();
    uint256 reserveDecimals = reserveCache.reserveConfiguration.getDecimals();

    uint256 unbacked = reserve.unbacked += amount.toUint128();

    require(
      unbacked <= unbackedMintCap * (10 ** reserveDecimals),
      Errors.UNBACKED_MINT_CAP_EXCEEDED
    );

    reserve.updateInterestRates(reserveCache, asset, 0, 0);

    bool isFirstSupply = IAToken(reserveCache.aTokenAddress).mint(
      msg.sender,
      onBehalfOf,
      amount,
      reserveCache.nextLiquidityIndex
    );

    if (isFirstSupply) {
      if (
        ValidationLogic.validateAutomaticUseAsCollateral(
          reservesData,
          reservesList,
          userConfig,
          reserveCache.reserveConfiguration,
          reserveCache.aTokenAddress
        )
      ) {
        userConfig.setUsingAsCollateral(reserve.id, true);
        emit ReserveUsedAsCollateralEnabled(asset, onBehalfOf);
      }
    }

    emit MintUnbacked(asset, msg.sender, onBehalfOf, amount, referralCode);
  }

  /**
   * @notice Back the current unbacked with `amount` and pay `fee`.
   * @dev It is not possible to back more than the existing unbacked amount of the reserve
   * @dev Emits the `BackUnbacked` event
   * @param reserve The reserve to back unbacked for
   * @param asset The address of the underlying asset to repay
   * @param amount The amount to back
   * @param fee The amount paid in fees
   * @param protocolFeeBps The fraction of fees in basis points paid to the protocol
   * @return The backed amount
   */
  function executeBackUnbacked(
    DataTypes.ReserveData storage reserve,
    address asset,
    uint256 amount,
    uint256 fee,
    uint256 protocolFeeBps
  ) external returns (uint256) {
    DataTypes.ReserveCache memory reserveCache = reserve.cache();

    reserve.updateState(reserveCache);

    uint256 backingAmount = (amount < reserve.unbacked) ? amount : reserve.unbacked;

    uint256 feeToProtocol = fee.percentMul(protocolFeeBps);
    uint256 feeToLP = fee - feeToProtocol;
    uint256 added = backingAmount + fee;

    reserveCache.nextLiquidityIndex = reserve.cumulateToLiquidityIndex(
      IERC20(reserveCache.aTokenAddress).totalSupply() +
        uint256(reserve.accruedToTreasury).rayMul(reserveCache.nextLiquidityIndex),
      feeToLP
    );

    reserve.accruedToTreasury += feeToProtocol.rayDiv(reserveCache.nextLiquidityIndex).toUint128();

    reserve.unbacked -= backingAmount.toUint128();
    reserve.updateInterestRates(reserveCache, asset, added, 0);

    IERC20(asset).safeTransferFrom(msg.sender, reserveCache.aTokenAddress, added);

    emit BackUnbacked(asset, msg.sender, backingAmount, fee);

    return backingAmount;
  }
}

// SPDX-License-Identifier: MIT
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
  uint256 internal _bridgeProtocolFee;

  // Total FlashLoan Premium, expressed in bps
  uint128 internal _flashLoanPremiumTotal;

  // FlashLoan premium paid to protocol treasury, expressed in bps
  uint128 internal _flashLoanPremiumToProtocol;

  // Available liquidity that can be borrowed at once at stable rate, expressed in bps
  uint64 internal _maxStableRateBorrowSizePercent;

  // Maximum number of active reserves there have been in the protocol. It is the upper bound of the reserves list
  uint16 internal _reservesCount;
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

import {IPoolAddressesProvider} from '../../interfaces/IPoolAddressesProvider.sol';
import {IPool} from '../../interfaces/IPool.sol';

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
// OpenZeppelin Contracts v4.4.1 (utils/math/SafeCast.sol)
pragma solidity ^0.8.10;

/**
 * @dev Wrappers over Solidity's uintXX/intXX casting operators with added overflow
 * checks.
 *
 * Downcasting from uint256/int256 in Solidity does not revert on overflow. This can
 * easily result in undesired exploitation or bugs, since developers usually
 * assume that overflows raise errors. `SafeCast` restores this intuition by
 * reverting the transaction when such an operation overflows.
 *
 * Using this library instead of the unchecked operations eliminates an entire
 * class of bugs, so it's recommended to use it always.
 *
 * Can be combined with {SafeMath} and {SignedSafeMath} to extend it to smaller types, by performing
 * all math on `uint256` and `int256` and then downcasting.
 */
library SafeCast {
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
    require(value <= type(uint224).max, "SafeCast: value doesn't fit in 224 bits");
    return uint224(value);
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
    require(value <= type(uint128).max, "SafeCast: value doesn't fit in 128 bits");
    return uint128(value);
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
    require(value <= type(uint96).max, "SafeCast: value doesn't fit in 96 bits");
    return uint96(value);
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
    require(value <= type(uint64).max, "SafeCast: value doesn't fit in 64 bits");
    return uint64(value);
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
    require(value <= type(uint32).max, "SafeCast: value doesn't fit in 32 bits");
    return uint32(value);
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
    require(value <= type(uint16).max, "SafeCast: value doesn't fit in 16 bits");
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
   * - input must fit into 8 bits.
   */
  function toUint8(uint256 value) internal pure returns (uint8) {
    require(value <= type(uint8).max, "SafeCast: value doesn't fit in 8 bits");
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
    require(value >= 0, 'SafeCast: value must be positive');
    return uint256(value);
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
   *
   * _Available since v3.1._
   */
  function toInt128(int256 value) internal pure returns (int128) {
    require(
      value >= type(int128).min && value <= type(int128).max,
      "SafeCast: value doesn't fit in 128 bits"
    );
    return int128(value);
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
   *
   * _Available since v3.1._
   */
  function toInt64(int256 value) internal pure returns (int64) {
    require(
      value >= type(int64).min && value <= type(int64).max,
      "SafeCast: value doesn't fit in 64 bits"
    );
    return int64(value);
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
   *
   * _Available since v3.1._
   */
  function toInt32(int256 value) internal pure returns (int32) {
    require(
      value >= type(int32).min && value <= type(int32).max,
      "SafeCast: value doesn't fit in 32 bits"
    );
    return int32(value);
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
   *
   * _Available since v3.1._
   */
  function toInt16(int256 value) internal pure returns (int16) {
    require(
      value >= type(int16).min && value <= type(int16).max,
      "SafeCast: value doesn't fit in 16 bits"
    );
    return int16(value);
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
   * - input must fit into 8 bits.
   *
   * _Available since v3.1._
   */
  function toInt8(int256 value) internal pure returns (int8) {
    require(
      value >= type(int8).min && value <= type(int8).max,
      "SafeCast: value doesn't fit in 8 bits"
    );
    return int8(value);
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
    require(value <= uint256(type(int256).max), "SafeCast: value doesn't fit in an int256");
    return int256(value);
  }
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

library DataTypes {
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
    //the current stable borrow rate. Expressed in ray
    uint128 currentStableBorrowRate;
    //timestamp of last update
    uint40 lastUpdateTimestamp;
    //the id of the reserve. Represents the position in the list of the active reserves
    uint16 id;
    //aToken address
    address aTokenAddress;
    //stableDebtToken address
    address stableDebtTokenAddress;
    //variableDebtToken address
    address variableDebtTokenAddress;
    //address of the interest rate strategy
    address interestRateStrategyAddress;
    //the current treasury balance, scaled
    uint128 accruedToTreasury;
    //the outstanding unbacked aTokens minted through the bridging feature
    uint128 unbacked;
    //the outstanding debt borrowed against this asset in isolation mode
    uint128 isolationModeTotalDebt;
  }

  struct ReserveConfigurationMap {
    //bit 0-15: LTV
    //bit 16-31: Liq. threshold
    //bit 32-47: Liq. bonus
    //bit 48-55: Decimals
    //bit 56: reserve is active
    //bit 57: reserve is frozen
    //bit 58: borrowing is enabled
    //bit 59: stable rate borrowing enabled
    //bit 60: asset is paused
    //bit 61: borrowing in isolation mode is enabled
    //bit 62: siloed borrowing enabled
    //bit 63: flashloaning enabled
    //bit 64-79: reserve factor
    //bit 80-115 borrow cap in whole tokens, borrowCap == 0 => no cap
    //bit 116-151 supply cap in whole tokens, supplyCap == 0 => no cap
    //bit 152-167 liquidation protocol fee
    //bit 168-175 eMode category
    //bit 176-211 unbacked mint cap in whole tokens, unbackedMintCap == 0 => minting disabled
    //bit 212-251 debt ceiling for isolation mode with (ReserveConfiguration::DEBT_CEILING_DECIMALS) decimals
    //bit 252-255 unused

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

  struct EModeCategory {
    // each eMode category has a custom ltv and liquidation threshold
    uint16 ltv;
    uint16 liquidationThreshold;
    uint16 liquidationBonus;
    // each eMode category may or may not have a custom oracle to override the individual assets price oracles
    address priceSource;
    string label;
  }

  enum InterestRateMode {
    NONE,
    STABLE,
    VARIABLE
  }

  struct ReserveCache {
    uint256 currScaledVariableDebt;
    uint256 nextScaledVariableDebt;
    uint256 currPrincipalStableDebt;
    uint256 currAvgStableBorrowRate;
    uint256 currTotalStableDebt;
    uint256 nextAvgStableBorrowRate;
    uint256 nextTotalStableDebt;
    uint256 currLiquidityIndex;
    uint256 nextLiquidityIndex;
    uint256 currVariableBorrowIndex;
    uint256 nextVariableBorrowIndex;
    uint256 currLiquidityRate;
    uint256 currVariableBorrowRate;
    uint256 reserveFactor;
    ReserveConfigurationMap reserveConfiguration;
    address aTokenAddress;
    address stableDebtTokenAddress;
    address variableDebtTokenAddress;
    uint40 reserveLastUpdateTimestamp;
    uint40 stableDebtLastUpdateTimestamp;
  }

  struct ExecuteLiquidationCallParams {
    uint256 reservesCount;
    uint256 debtToCover;
    address collateralAsset;
    address debtAsset;
    address user;
    bool receiveAToken;
    address priceOracle;
    uint8 userEModeCategory;
    address priceOracleSentinel;
  }

  struct ExecuteSupplyParams {
    address asset;
    uint256 amount;
    address onBehalfOf;
    uint16 referralCode;
  }

  struct ExecuteBorrowParams {
    address asset;
    address user;
    address onBehalfOf;
    uint256 amount;
    InterestRateMode interestRateMode;
    uint16 referralCode;
    bool releaseUnderlying;
    uint256 maxStableRateBorrowSizePercent;
    uint256 reservesCount;
    address oracle;
    uint8 userEModeCategory;
    address priceOracleSentinel;
  }

  struct ExecuteRepayParams {
    address asset;
    uint256 amount;
    InterestRateMode interestRateMode;
    address onBehalfOf;
    bool useATokens;
  }

  struct ExecuteWithdrawParams {
    address asset;
    uint256 amount;
    address to;
    uint256 reservesCount;
    address oracle;
    uint8 userEModeCategory;
  }

  struct ExecuteSetUserEModeParams {
    uint256 reservesCount;
    address oracle;
    uint8 categoryId;
  }

  struct FinalizeTransferParams {
    address asset;
    address from;
    address to;
    uint256 amount;
    uint256 balanceFromBefore;
    uint256 balanceToBefore;
    uint256 reservesCount;
    address oracle;
    uint8 fromEModeCategory;
  }

  struct FlashloanParams {
    address receiverAddress;
    address[] assets;
    uint256[] amounts;
    uint256[] interestRateModes;
    address onBehalfOf;
    bytes params;
    uint16 referralCode;
    uint256 flashLoanPremiumToProtocol;
    uint256 flashLoanPremiumTotal;
    uint256 maxStableRateBorrowSizePercent;
    uint256 reservesCount;
    address addressesProvider;
    address pool;
    uint8 userEModeCategory;
    bool isAuthorizedFlashBorrower;
  }

  struct FlashloanSimpleParams {
    address receiverAddress;
    address asset;
    uint256 amount;
    bytes params;
    uint16 referralCode;
    uint256 flashLoanPremiumToProtocol;
    uint256 flashLoanPremiumTotal;
  }

  struct FlashLoanRepaymentParams {
    uint256 amount;
    uint256 totalPremium;
    uint256 flashLoanPremiumToProtocol;
    address asset;
    address receiverAddress;
    uint16 referralCode;
  }

  struct CalculateUserAccountDataParams {
    UserConfigurationMap userConfig;
    uint256 reservesCount;
    address user;
    address oracle;
    uint8 userEModeCategory;
  }

  struct ValidateBorrowParams {
    ReserveCache reserveCache;
    UserConfigurationMap userConfig;
    address asset;
    address userAddress;
    uint256 amount;
    InterestRateMode interestRateMode;
    uint256 maxStableLoanPercent;
    uint256 reservesCount;
    address oracle;
    uint8 userEModeCategory;
    address priceOracleSentinel;
    bool isolationModeActive;
    address isolationModeCollateralAddress;
    uint256 isolationModeDebtCeiling;
  }

  struct ValidateLiquidationCallParams {
    ReserveCache debtReserveCache;
    uint256 totalDebt;
    uint256 healthFactor;
    address priceOracleSentinel;
  }

  struct CalculateInterestRatesParams {
    uint256 unbacked;
    uint256 liquidityAdded;
    uint256 liquidityTaken;
    uint256 totalStableDebt;
    uint256 totalVariableDebt;
    uint256 averageStableBorrowRate;
    uint256 reserveFactor;
    address reserve;
    address aToken;
  }

  struct InitReserveParams {
    address asset;
    address aTokenAddress;
    address stableDebtAddress;
    address variableDebtAddress;
    address interestRateStrategyAddress;
    uint16 reservesCount;
    uint16 maxNumberReserves;
  }
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
  uint256 internal constant LTV_MASK =                       0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF0000; // prettier-ignore
  uint256 internal constant LIQUIDATION_THRESHOLD_MASK =     0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF0000FFFF; // prettier-ignore
  uint256 internal constant LIQUIDATION_BONUS_MASK =         0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF0000FFFFFFFF; // prettier-ignore
  uint256 internal constant DECIMALS_MASK =                  0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF00FFFFFFFFFFFF; // prettier-ignore
  uint256 internal constant ACTIVE_MASK =                    0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFFFFFFFFFF; // prettier-ignore
  uint256 internal constant FROZEN_MASK =                    0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFDFFFFFFFFFFFFFF; // prettier-ignore
  uint256 internal constant BORROWING_MASK =                 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFBFFFFFFFFFFFFFF; // prettier-ignore
  uint256 internal constant STABLE_BORROWING_MASK =          0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF7FFFFFFFFFFFFFF; // prettier-ignore
  uint256 internal constant PAUSED_MASK =                    0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFFFFFFFFFFF; // prettier-ignore
  uint256 internal constant BORROWABLE_IN_ISOLATION_MASK =   0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFDFFFFFFFFFFFFFFF; // prettier-ignore
  uint256 internal constant SILOED_BORROWING_MASK =          0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFBFFFFFFFFFFFFFFF; // prettier-ignore
  uint256 internal constant FLASHLOAN_ENABLED_MASK =         0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF7FFFFFFFFFFFFFFF; // prettier-ignore
  uint256 internal constant RESERVE_FACTOR_MASK =            0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF0000FFFFFFFFFFFFFFFF; // prettier-ignore
  uint256 internal constant BORROW_CAP_MASK =                0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF000000000FFFFFFFFFFFFFFFFFFFF; // prettier-ignore
  uint256 internal constant SUPPLY_CAP_MASK =                0xFFFFFFFFFFFFFFFFFFFFFFFFFF000000000FFFFFFFFFFFFFFFFFFFFFFFFFFFFF; // prettier-ignore
  uint256 internal constant LIQUIDATION_PROTOCOL_FEE_MASK =  0xFFFFFFFFFFFFFFFFFFFFFF0000FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF; // prettier-ignore
  uint256 internal constant EMODE_CATEGORY_MASK =            0xFFFFFFFFFFFFFFFFFFFF00FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF; // prettier-ignore
  uint256 internal constant UNBACKED_MINT_CAP_MASK =         0xFFFFFFFFFFF000000000FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF; // prettier-ignore
  uint256 internal constant DEBT_CEILING_MASK =              0xF0000000000FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF; // prettier-ignore

  /// @dev For the LTV, the start bit is 0 (up to 15), hence no bitshifting is needed
  uint256 internal constant LIQUIDATION_THRESHOLD_START_BIT_POSITION = 16;
  uint256 internal constant LIQUIDATION_BONUS_START_BIT_POSITION = 32;
  uint256 internal constant RESERVE_DECIMALS_START_BIT_POSITION = 48;
  uint256 internal constant IS_ACTIVE_START_BIT_POSITION = 56;
  uint256 internal constant IS_FROZEN_START_BIT_POSITION = 57;
  uint256 internal constant BORROWING_ENABLED_START_BIT_POSITION = 58;
  uint256 internal constant STABLE_BORROWING_ENABLED_START_BIT_POSITION = 59;
  uint256 internal constant IS_PAUSED_START_BIT_POSITION = 60;
  uint256 internal constant BORROWABLE_IN_ISOLATION_START_BIT_POSITION = 61;
  uint256 internal constant SILOED_BORROWING_START_BIT_POSITION = 62;
  uint256 internal constant FLASHLOAN_ENABLED_START_BIT_POSITION = 63;
  uint256 internal constant RESERVE_FACTOR_START_BIT_POSITION = 64;
  uint256 internal constant BORROW_CAP_START_BIT_POSITION = 80;
  uint256 internal constant SUPPLY_CAP_START_BIT_POSITION = 116;
  uint256 internal constant LIQUIDATION_PROTOCOL_FEE_START_BIT_POSITION = 152;
  uint256 internal constant EMODE_CATEGORY_START_BIT_POSITION = 168;
  uint256 internal constant UNBACKED_MINT_CAP_START_BIT_POSITION = 176;
  uint256 internal constant DEBT_CEILING_START_BIT_POSITION = 212;

  uint256 internal constant MAX_VALID_LTV = 65535;
  uint256 internal constant MAX_VALID_LIQUIDATION_THRESHOLD = 65535;
  uint256 internal constant MAX_VALID_LIQUIDATION_BONUS = 65535;
  uint256 internal constant MAX_VALID_DECIMALS = 255;
  uint256 internal constant MAX_VALID_RESERVE_FACTOR = 65535;
  uint256 internal constant MAX_VALID_BORROW_CAP = 68719476735;
  uint256 internal constant MAX_VALID_SUPPLY_CAP = 68719476735;
  uint256 internal constant MAX_VALID_LIQUIDATION_PROTOCOL_FEE = 65535;
  uint256 internal constant MAX_VALID_EMODE_CATEGORY = 255;
  uint256 internal constant MAX_VALID_UNBACKED_MINT_CAP = 68719476735;
  uint256 internal constant MAX_VALID_DEBT_CEILING = 1099511627775;

  uint256 public constant DEBT_CEILING_DECIMALS = 2;
  uint16 public constant MAX_RESERVES_COUNT = 128;

  /**
   * @notice Sets the Loan to Value of the reserve
   * @param self The reserve configuration
   * @param ltv The new ltv
   */
  function setLtv(DataTypes.ReserveConfigurationMap memory self, uint256 ltv) internal pure {
    require(ltv <= MAX_VALID_LTV, Errors.INVALID_LTV);

    self.data = (self.data & LTV_MASK) | ltv;
  }

  /**
   * @notice Gets the Loan to Value of the reserve
   * @param self The reserve configuration
   * @return The loan to value
   */
  function getLtv(DataTypes.ReserveConfigurationMap memory self) internal pure returns (uint256) {
    return self.data & ~LTV_MASK;
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
    require(threshold <= MAX_VALID_LIQUIDATION_THRESHOLD, Errors.INVALID_LIQ_THRESHOLD);

    self.data =
      (self.data & LIQUIDATION_THRESHOLD_MASK) |
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
    return (self.data & ~LIQUIDATION_THRESHOLD_MASK) >> LIQUIDATION_THRESHOLD_START_BIT_POSITION;
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
    require(bonus <= MAX_VALID_LIQUIDATION_BONUS, Errors.INVALID_LIQ_BONUS);

    self.data =
      (self.data & LIQUIDATION_BONUS_MASK) |
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
    return (self.data & ~LIQUIDATION_BONUS_MASK) >> LIQUIDATION_BONUS_START_BIT_POSITION;
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
    require(decimals <= MAX_VALID_DECIMALS, Errors.INVALID_DECIMALS);

    self.data = (self.data & DECIMALS_MASK) | (decimals << RESERVE_DECIMALS_START_BIT_POSITION);
  }

  /**
   * @notice Gets the decimals of the underlying asset of the reserve
   * @param self The reserve configuration
   * @return The decimals of the asset
   */
  function getDecimals(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256) {
    return (self.data & ~DECIMALS_MASK) >> RESERVE_DECIMALS_START_BIT_POSITION;
  }

  /**
   * @notice Sets the active state of the reserve
   * @param self The reserve configuration
   * @param active The active state
   */
  function setActive(DataTypes.ReserveConfigurationMap memory self, bool active) internal pure {
    self.data =
      (self.data & ACTIVE_MASK) |
      (uint256(active ? 1 : 0) << IS_ACTIVE_START_BIT_POSITION);
  }

  /**
   * @notice Gets the active state of the reserve
   * @param self The reserve configuration
   * @return The active state
   */
  function getActive(DataTypes.ReserveConfigurationMap memory self) internal pure returns (bool) {
    return (self.data & ~ACTIVE_MASK) != 0;
  }

  /**
   * @notice Sets the frozen state of the reserve
   * @param self The reserve configuration
   * @param frozen The frozen state
   */
  function setFrozen(DataTypes.ReserveConfigurationMap memory self, bool frozen) internal pure {
    self.data =
      (self.data & FROZEN_MASK) |
      (uint256(frozen ? 1 : 0) << IS_FROZEN_START_BIT_POSITION);
  }

  /**
   * @notice Gets the frozen state of the reserve
   * @param self The reserve configuration
   * @return The frozen state
   */
  function getFrozen(DataTypes.ReserveConfigurationMap memory self) internal pure returns (bool) {
    return (self.data & ~FROZEN_MASK) != 0;
  }

  /**
   * @notice Sets the paused state of the reserve
   * @param self The reserve configuration
   * @param paused The paused state
   */
  function setPaused(DataTypes.ReserveConfigurationMap memory self, bool paused) internal pure {
    self.data =
      (self.data & PAUSED_MASK) |
      (uint256(paused ? 1 : 0) << IS_PAUSED_START_BIT_POSITION);
  }

  /**
   * @notice Gets the paused state of the reserve
   * @param self The reserve configuration
   * @return The paused state
   */
  function getPaused(DataTypes.ReserveConfigurationMap memory self) internal pure returns (bool) {
    return (self.data & ~PAUSED_MASK) != 0;
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
      (self.data & BORROWABLE_IN_ISOLATION_MASK) |
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
    return (self.data & ~BORROWABLE_IN_ISOLATION_MASK) != 0;
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
      (self.data & SILOED_BORROWING_MASK) |
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
    return (self.data & ~SILOED_BORROWING_MASK) != 0;
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
      (self.data & BORROWING_MASK) |
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
    return (self.data & ~BORROWING_MASK) != 0;
  }

  /**
   * @notice Enables or disables stable rate borrowing on the reserve
   * @param self The reserve configuration
   * @param enabled True if the stable rate borrowing needs to be enabled, false otherwise
   */
  function setStableRateBorrowingEnabled(
    DataTypes.ReserveConfigurationMap memory self,
    bool enabled
  ) internal pure {
    self.data =
      (self.data & STABLE_BORROWING_MASK) |
      (uint256(enabled ? 1 : 0) << STABLE_BORROWING_ENABLED_START_BIT_POSITION);
  }

  /**
   * @notice Gets the stable rate borrowing state of the reserve
   * @param self The reserve configuration
   * @return The stable rate borrowing state
   */
  function getStableRateBorrowingEnabled(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (bool) {
    return (self.data & ~STABLE_BORROWING_MASK) != 0;
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
    require(reserveFactor <= MAX_VALID_RESERVE_FACTOR, Errors.INVALID_RESERVE_FACTOR);

    self.data =
      (self.data & RESERVE_FACTOR_MASK) |
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
    return (self.data & ~RESERVE_FACTOR_MASK) >> RESERVE_FACTOR_START_BIT_POSITION;
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
    require(borrowCap <= MAX_VALID_BORROW_CAP, Errors.INVALID_BORROW_CAP);

    self.data = (self.data & BORROW_CAP_MASK) | (borrowCap << BORROW_CAP_START_BIT_POSITION);
  }

  /**
   * @notice Gets the borrow cap of the reserve
   * @param self The reserve configuration
   * @return The borrow cap
   */
  function getBorrowCap(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256) {
    return (self.data & ~BORROW_CAP_MASK) >> BORROW_CAP_START_BIT_POSITION;
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
    require(supplyCap <= MAX_VALID_SUPPLY_CAP, Errors.INVALID_SUPPLY_CAP);

    self.data = (self.data & SUPPLY_CAP_MASK) | (supplyCap << SUPPLY_CAP_START_BIT_POSITION);
  }

  /**
   * @notice Gets the supply cap of the reserve
   * @param self The reserve configuration
   * @return The supply cap
   */
  function getSupplyCap(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256) {
    return (self.data & ~SUPPLY_CAP_MASK) >> SUPPLY_CAP_START_BIT_POSITION;
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
    require(ceiling <= MAX_VALID_DEBT_CEILING, Errors.INVALID_DEBT_CEILING);

    self.data = (self.data & DEBT_CEILING_MASK) | (ceiling << DEBT_CEILING_START_BIT_POSITION);
  }

  /**
   * @notice Gets the debt ceiling for the asset if the asset is in isolation mode
   * @param self The reserve configuration
   * @return The debt ceiling (0 = isolation mode disabled)
   */
  function getDebtCeiling(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256) {
    return (self.data & ~DEBT_CEILING_MASK) >> DEBT_CEILING_START_BIT_POSITION;
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
      Errors.INVALID_LIQUIDATION_PROTOCOL_FEE
    );

    self.data =
      (self.data & LIQUIDATION_PROTOCOL_FEE_MASK) |
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
      (self.data & ~LIQUIDATION_PROTOCOL_FEE_MASK) >> LIQUIDATION_PROTOCOL_FEE_START_BIT_POSITION;
  }

  /**
   * @notice Sets the unbacked mint cap of the reserve
   * @param self The reserve configuration
   * @param unbackedMintCap The unbacked mint cap
   */
  function setUnbackedMintCap(
    DataTypes.ReserveConfigurationMap memory self,
    uint256 unbackedMintCap
  ) internal pure {
    require(unbackedMintCap <= MAX_VALID_UNBACKED_MINT_CAP, Errors.INVALID_UNBACKED_MINT_CAP);

    self.data =
      (self.data & UNBACKED_MINT_CAP_MASK) |
      (unbackedMintCap << UNBACKED_MINT_CAP_START_BIT_POSITION);
  }

  /**
   * @dev Gets the unbacked mint cap of the reserve
   * @param self The reserve configuration
   * @return The unbacked mint cap
   */
  function getUnbackedMintCap(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256) {
    return (self.data & ~UNBACKED_MINT_CAP_MASK) >> UNBACKED_MINT_CAP_START_BIT_POSITION;
  }

  /**
   * @notice Sets the eMode asset category
   * @param self The reserve configuration
   * @param category The asset category when the user selects the eMode
   */
  function setEModeCategory(
    DataTypes.ReserveConfigurationMap memory self,
    uint256 category
  ) internal pure {
    require(category <= MAX_VALID_EMODE_CATEGORY, Errors.INVALID_EMODE_CATEGORY);

    self.data = (self.data & EMODE_CATEGORY_MASK) | (category << EMODE_CATEGORY_START_BIT_POSITION);
  }

  /**
   * @dev Gets the eMode asset category
   * @param self The reserve configuration
   * @return The eMode category for the asset
   */
  function getEModeCategory(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256) {
    return (self.data & ~EMODE_CATEGORY_MASK) >> EMODE_CATEGORY_START_BIT_POSITION;
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
      (self.data & FLASHLOAN_ENABLED_MASK) |
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
    return (self.data & ~FLASHLOAN_ENABLED_MASK) != 0;
  }

  /**
   * @notice Gets the configuration flags of the reserve
   * @param self The reserve configuration
   * @return The state flag representing active
   * @return The state flag representing frozen
   * @return The state flag representing borrowing enabled
   * @return The state flag representing stableRateBorrowing enabled
   * @return The state flag representing paused
   */
  function getFlags(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (bool, bool, bool, bool, bool) {
    uint256 dataLocal = self.data;

    return (
      (dataLocal & ~ACTIVE_MASK) != 0,
      (dataLocal & ~FROZEN_MASK) != 0,
      (dataLocal & ~BORROWING_MASK) != 0,
      (dataLocal & ~STABLE_BORROWING_MASK) != 0,
      (dataLocal & ~PAUSED_MASK) != 0
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
   * @return The state param representing eMode category
   */
  function getParams(
    DataTypes.ReserveConfigurationMap memory self
  ) internal pure returns (uint256, uint256, uint256, uint256, uint256, uint256) {
    uint256 dataLocal = self.data;

    return (
      dataLocal & ~LTV_MASK,
      (dataLocal & ~LIQUIDATION_THRESHOLD_MASK) >> LIQUIDATION_THRESHOLD_START_BIT_POSITION,
      (dataLocal & ~LIQUIDATION_BONUS_MASK) >> LIQUIDATION_BONUS_START_BIT_POSITION,
      (dataLocal & ~DECIMALS_MASK) >> RESERVE_DECIMALS_START_BIT_POSITION,
      (dataLocal & ~RESERVE_FACTOR_MASK) >> RESERVE_FACTOR_START_BIT_POSITION,
      (dataLocal & ~EMODE_CATEGORY_MASK) >> EMODE_CATEGORY_START_BIT_POSITION
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
      (dataLocal & ~BORROW_CAP_MASK) >> BORROW_CAP_START_BIT_POSITION,
      (dataLocal & ~SUPPLY_CAP_MASK) >> SUPPLY_CAP_START_BIT_POSITION
    );
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.10;

import {IERC20} from '../../../dependencies/openzeppelin/contracts//IERC20.sol';
import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {PercentageMath} from '../../libraries/math/PercentageMath.sol';
import {WadRayMath} from '../../libraries/math/WadRayMath.sol';
import {Helpers} from '../../libraries/helpers/Helpers.sol';
import {DataTypes} from '../../libraries/types/DataTypes.sol';
import {ReserveLogic} from './ReserveLogic.sol';
import {ValidationLogic} from './ValidationLogic.sol';
import {GenericLogic} from './GenericLogic.sol';
import {IsolationModeLogic} from './IsolationModeLogic.sol';
import {EModeLogic} from './EModeLogic.sol';
import {UserConfiguration} from '../../libraries/configuration/UserConfiguration.sol';
import {ReserveConfiguration} from '../../libraries/configuration/ReserveConfiguration.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {IStableDebtToken} from '../../../interfaces/IStableDebtToken.sol';
import {IVariableDebtToken} from '../../../interfaces/IVariableDebtToken.sol';
import {IPriceOracleGetter} from '../../../interfaces/IPriceOracleGetter.sol';

/**
 * @title LiquidationLogic library
 * @author Aave
 * @notice Implements actions involving management of collateral in the protocol, the main one being the liquidations
 */
library LiquidationLogic {
  using WadRayMath for uint256;
  using PercentageMath for uint256;
  using ReserveLogic for DataTypes.ReserveCache;
  using ReserveLogic for DataTypes.ReserveData;
  using UserConfiguration for DataTypes.UserConfigurationMap;
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;
  using GPv2SafeERC20 for IERC20;

  // See `IPool` for descriptions
  event ReserveUsedAsCollateralEnabled(address indexed reserve, address indexed user);
  event ReserveUsedAsCollateralDisabled(address indexed reserve, address indexed user);
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
   * @dev Default percentage of borrower's debt to be repaid in a liquidation.
   * @dev Percentage applied when the users health factor is above `CLOSE_FACTOR_HF_THRESHOLD`
   * Expressed in bps, a value of 0.5e4 results in 50.00%
   */
  uint256 internal constant DEFAULT_LIQUIDATION_CLOSE_FACTOR = 0.5e4;

  /**
   * @dev Maximum percentage of borrower's debt to be repaid in a liquidation
   * @dev Percentage applied when the users health factor is below `CLOSE_FACTOR_HF_THRESHOLD`
   * Expressed in bps, a value of 1e4 results in 100.00%
   */
  uint256 public constant MAX_LIQUIDATION_CLOSE_FACTOR = 1e4;

  /**
   * @dev This constant represents below which health factor value it is possible to liquidate
   * an amount of debt corresponding to `MAX_LIQUIDATION_CLOSE_FACTOR`.
   * A value of 0.95e18 results in 0.95
   */
  uint256 public constant CLOSE_FACTOR_HF_THRESHOLD = 0.95e18;

  struct LiquidationCallLocalVars {
    uint256 userCollateralBalance;
    uint256 userVariableDebt;
    uint256 userTotalDebt;
    uint256 actualDebtToLiquidate;
    uint256 actualCollateralToLiquidate;
    uint256 liquidationBonus;
    uint256 healthFactor;
    uint256 liquidationProtocolFeeAmount;
    address collateralPriceSource;
    address debtPriceSource;
    IAToken collateralAToken;
    DataTypes.ReserveCache debtReserveCache;
  }

  /**
   * @notice Function to liquidate a position if its Health Factor drops below 1. The caller (liquidator)
   * covers `debtToCover` amount of debt of the user getting liquidated, and receives
   * a proportional amount of the `collateralAsset` plus a bonus to cover market risk
   * @dev Emits the `LiquidationCall()` event
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
    DataTypes.UserConfigurationMap storage userConfig = usersConfig[params.user];
    vars.debtReserveCache = debtReserve.cache();
    debtReserve.updateState(vars.debtReserveCache);

    (, , , , vars.healthFactor, ) = GenericLogic.calculateUserAccountData(
      reservesData,
      reservesList,
      eModeCategories,
      DataTypes.CalculateUserAccountDataParams({
        userConfig: userConfig,
        reservesCount: params.reservesCount,
        user: params.user,
        oracle: params.priceOracle,
        userEModeCategory: params.userEModeCategory
      })
    );

    (vars.userVariableDebt, vars.userTotalDebt, vars.actualDebtToLiquidate) = _calculateDebt(
      vars.debtReserveCache,
      params,
      vars.healthFactor
    );

    ValidationLogic.validateLiquidationCall(
      userConfig,
      collateralReserve,
      DataTypes.ValidateLiquidationCallParams({
        debtReserveCache: vars.debtReserveCache,
        totalDebt: vars.userTotalDebt,
        healthFactor: vars.healthFactor,
        priceOracleSentinel: params.priceOracleSentinel
      })
    );

    (
      vars.collateralAToken,
      vars.collateralPriceSource,
      vars.debtPriceSource,
      vars.liquidationBonus
    ) = _getConfigurationData(eModeCategories, collateralReserve, params);

    vars.userCollateralBalance = vars.collateralAToken.balanceOf(params.user);

    (
      vars.actualCollateralToLiquidate,
      vars.actualDebtToLiquidate,
      vars.liquidationProtocolFeeAmount
    ) = _calculateAvailableCollateralToLiquidate(
      collateralReserve,
      vars.debtReserveCache,
      vars.collateralPriceSource,
      vars.debtPriceSource,
      vars.actualDebtToLiquidate,
      vars.userCollateralBalance,
      vars.liquidationBonus,
      IPriceOracleGetter(params.priceOracle)
    );

    if (vars.userTotalDebt == vars.actualDebtToLiquidate) {
      userConfig.setBorrowing(debtReserve.id, false);
    }

    // If the collateral being liquidated is equal to the user balance,
    // we set the currency as not being used as collateral anymore
    if (
      vars.actualCollateralToLiquidate + vars.liquidationProtocolFeeAmount ==
      vars.userCollateralBalance
    ) {
      userConfig.setUsingAsCollateral(collateralReserve.id, false);
      emit ReserveUsedAsCollateralDisabled(params.collateralAsset, params.user);
    }

    _burnDebtTokens(params, vars);

    debtReserve.updateInterestRates(
      vars.debtReserveCache,
      params.debtAsset,
      vars.actualDebtToLiquidate,
      0
    );

    IsolationModeLogic.updateIsolatedDebtIfIsolated(
      reservesData,
      reservesList,
      userConfig,
      vars.debtReserveCache,
      vars.actualDebtToLiquidate
    );

    if (params.receiveAToken) {
      _liquidateATokens(reservesData, reservesList, usersConfig, collateralReserve, params, vars);
    } else {
      _burnCollateralATokens(collateralReserve, params, vars);
    }

    // Transfer fee to treasury if it is non-zero
    if (vars.liquidationProtocolFeeAmount != 0) {
      uint256 liquidityIndex = collateralReserve.getNormalizedIncome();
      uint256 scaledDownLiquidationProtocolFee = vars.liquidationProtocolFeeAmount.rayDiv(
        liquidityIndex
      );
      uint256 scaledDownUserBalance = vars.collateralAToken.scaledBalanceOf(params.user);
      // To avoid trying to send more aTokens than available on balance, due to 1 wei imprecision
      if (scaledDownLiquidationProtocolFee > scaledDownUserBalance) {
        vars.liquidationProtocolFeeAmount = scaledDownUserBalance.rayMul(liquidityIndex);
      }
      vars.collateralAToken.transferOnLiquidation(
        params.user,
        vars.collateralAToken.RESERVE_TREASURY_ADDRESS(),
        vars.liquidationProtocolFeeAmount
      );
    }

    // Transfers the debt asset being repaid to the aToken, where the liquidity is kept
    IERC20(params.debtAsset).safeTransferFrom(
      msg.sender,
      vars.debtReserveCache.aTokenAddress,
      vars.actualDebtToLiquidate
    );

    IAToken(vars.debtReserveCache.aTokenAddress).handleRepayment(
      msg.sender,
      params.user,
      vars.actualDebtToLiquidate
    );

    emit LiquidationCall(
      params.collateralAsset,
      params.debtAsset,
      params.user,
      vars.actualDebtToLiquidate,
      vars.actualCollateralToLiquidate,
      msg.sender,
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
    DataTypes.ReserveCache memory collateralReserveCache = collateralReserve.cache();
    collateralReserve.updateState(collateralReserveCache);
    collateralReserve.updateInterestRates(
      collateralReserveCache,
      params.collateralAsset,
      0,
      vars.actualCollateralToLiquidate
    );

    // Burn the equivalent amount of aToken, sending the underlying to the liquidator
    vars.collateralAToken.burn(
      params.user,
      msg.sender,
      vars.actualCollateralToLiquidate,
      collateralReserveCache.nextLiquidityIndex
    );
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
    uint256 liquidatorPreviousATokenBalance = IERC20(vars.collateralAToken).balanceOf(msg.sender);
    vars.collateralAToken.transferOnLiquidation(
      params.user,
      msg.sender,
      vars.actualCollateralToLiquidate
    );

    if (liquidatorPreviousATokenBalance == 0) {
      DataTypes.UserConfigurationMap storage liquidatorConfig = usersConfig[msg.sender];
      if (
        ValidationLogic.validateAutomaticUseAsCollateral(
          reservesData,
          reservesList,
          liquidatorConfig,
          collateralReserve.configuration,
          collateralReserve.aTokenAddress
        )
      ) {
        liquidatorConfig.setUsingAsCollateral(collateralReserve.id, true);
        emit ReserveUsedAsCollateralEnabled(params.collateralAsset, msg.sender);
      }
    }
  }

  /**
   * @notice Burns the debt tokens of the user up to the amount being repaid by the liquidator.
   * @dev The function alters the `debtReserveCache` state in `vars` to update the debt related data.
   * @param params The additional parameters needed to execute the liquidation function
   * @param vars the executeLiquidationCall() function local vars
   */
  function _burnDebtTokens(
    DataTypes.ExecuteLiquidationCallParams memory params,
    LiquidationCallLocalVars memory vars
  ) internal {
    if (vars.userVariableDebt >= vars.actualDebtToLiquidate) {
      vars.debtReserveCache.nextScaledVariableDebt = IVariableDebtToken(
        vars.debtReserveCache.variableDebtTokenAddress
      ).burn(
          params.user,
          vars.actualDebtToLiquidate,
          vars.debtReserveCache.nextVariableBorrowIndex
        );
    } else {
      // If the user doesn't have variable debt, no need to try to burn variable debt tokens
      if (vars.userVariableDebt != 0) {
        vars.debtReserveCache.nextScaledVariableDebt = IVariableDebtToken(
          vars.debtReserveCache.variableDebtTokenAddress
        ).burn(params.user, vars.userVariableDebt, vars.debtReserveCache.nextVariableBorrowIndex);
      }
      (
        vars.debtReserveCache.nextTotalStableDebt,
        vars.debtReserveCache.nextAvgStableBorrowRate
      ) = IStableDebtToken(vars.debtReserveCache.stableDebtTokenAddress).burn(
        params.user,
        vars.actualDebtToLiquidate - vars.userVariableDebt
      );
    }
  }

  /**
   * @notice Calculates the total debt of the user and the actual amount to liquidate depending on the health factor
   * and corresponding close factor.
   * @dev If the Health Factor is below CLOSE_FACTOR_HF_THRESHOLD, the close factor is increased to MAX_LIQUIDATION_CLOSE_FACTOR
   * @param debtReserveCache The reserve cache data object of the debt reserve
   * @param params The additional parameters needed to execute the liquidation function
   * @param healthFactor The health factor of the position
   * @return The variable debt of the user
   * @return The total debt of the user
   * @return The actual debt to liquidate as a function of the closeFactor
   */
  function _calculateDebt(
    DataTypes.ReserveCache memory debtReserveCache,
    DataTypes.ExecuteLiquidationCallParams memory params,
    uint256 healthFactor
  ) internal view returns (uint256, uint256, uint256) {
    (uint256 userStableDebt, uint256 userVariableDebt) = Helpers.getUserCurrentDebt(
      params.user,
      debtReserveCache
    );

    uint256 userTotalDebt = userStableDebt + userVariableDebt;

    uint256 closeFactor = healthFactor > CLOSE_FACTOR_HF_THRESHOLD
      ? DEFAULT_LIQUIDATION_CLOSE_FACTOR
      : MAX_LIQUIDATION_CLOSE_FACTOR;

    uint256 maxLiquidatableDebt = userTotalDebt.percentMul(closeFactor);

    uint256 actualDebtToLiquidate = params.debtToCover > maxLiquidatableDebt
      ? maxLiquidatableDebt
      : params.debtToCover;

    return (userVariableDebt, userTotalDebt, actualDebtToLiquidate);
  }

  /**
   * @notice Returns the configuration data for the debt and the collateral reserves.
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param collateralReserve The data of the collateral reserve
   * @param params The additional parameters needed to execute the liquidation function
   * @return The collateral aToken
   * @return The address to use as price source for the collateral
   * @return The address to use as price source for the debt
   * @return The liquidation bonus to apply to the collateral
   */
  function _getConfigurationData(
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.ReserveData storage collateralReserve,
    DataTypes.ExecuteLiquidationCallParams memory params
  ) internal view returns (IAToken, address, address, uint256) {
    IAToken collateralAToken = IAToken(collateralReserve.aTokenAddress);
    uint256 liquidationBonus = collateralReserve.configuration.getLiquidationBonus();

    address collateralPriceSource = params.collateralAsset;
    address debtPriceSource = params.debtAsset;

    if (params.userEModeCategory != 0) {
      address eModePriceSource = eModeCategories[params.userEModeCategory].priceSource;

      if (
        EModeLogic.isInEModeCategory(
          params.userEModeCategory,
          collateralReserve.configuration.getEModeCategory()
        )
      ) {
        liquidationBonus = eModeCategories[params.userEModeCategory].liquidationBonus;

        if (eModePriceSource != address(0)) {
          collateralPriceSource = eModePriceSource;
        }
      }

      // when in eMode, debt will always be in the same eMode category, can skip matching category check
      if (eModePriceSource != address(0)) {
        debtPriceSource = eModePriceSource;
      }
    }

    return (collateralAToken, collateralPriceSource, debtPriceSource, liquidationBonus);
  }

  struct AvailableCollateralToLiquidateLocalVars {
    uint256 collateralPrice;
    uint256 debtAssetPrice;
    uint256 maxCollateralToLiquidate;
    uint256 baseCollateral;
    uint256 bonusCollateral;
    uint256 debtAssetDecimals;
    uint256 collateralDecimals;
    uint256 collateralAssetUnit;
    uint256 debtAssetUnit;
    uint256 collateralAmount;
    uint256 debtAmountNeeded;
    uint256 liquidationProtocolFeePercentage;
    uint256 liquidationProtocolFee;
  }

  /**
   * @notice Calculates how much of a specific collateral can be liquidated, given
   * a certain amount of debt asset.
   * @dev This function needs to be called after all the checks to validate the liquidation have been performed,
   *   otherwise it might fail.
   * @param collateralReserve The data of the collateral reserve
   * @param debtReserveCache The cached data of the debt reserve
   * @param collateralAsset The address of the underlying asset used as collateral, to receive as result of the liquidation
   * @param debtAsset The address of the underlying borrowed asset to be repaid with the liquidation
   * @param debtToCover The debt amount of borrowed `asset` the liquidator wants to cover
   * @param userCollateralBalance The collateral balance for the specific `collateralAsset` of the user being liquidated
   * @param liquidationBonus The collateral bonus percentage to receive as result of the liquidation
   * @return The maximum amount that is possible to liquidate given all the liquidation constraints (user balance, close factor)
   * @return The amount to repay with the liquidation
   * @return The fee taken from the liquidation bonus amount to be paid to the protocol
   */
  function _calculateAvailableCollateralToLiquidate(
    DataTypes.ReserveData storage collateralReserve,
    DataTypes.ReserveCache memory debtReserveCache,
    address collateralAsset,
    address debtAsset,
    uint256 debtToCover,
    uint256 userCollateralBalance,
    uint256 liquidationBonus,
    IPriceOracleGetter oracle
  ) internal view returns (uint256, uint256, uint256) {
    AvailableCollateralToLiquidateLocalVars memory vars;

    vars.collateralPrice = oracle.getAssetPrice(collateralAsset);
    vars.debtAssetPrice = oracle.getAssetPrice(debtAsset);

    vars.collateralDecimals = collateralReserve.configuration.getDecimals();
    vars.debtAssetDecimals = debtReserveCache.reserveConfiguration.getDecimals();

    unchecked {
      vars.collateralAssetUnit = 10 ** vars.collateralDecimals;
      vars.debtAssetUnit = 10 ** vars.debtAssetDecimals;
    }

    vars.liquidationProtocolFeePercentage = collateralReserve
      .configuration
      .getLiquidationProtocolFee();

    // This is the base collateral to liquidate based on the given debt to cover
    vars.baseCollateral =
      ((vars.debtAssetPrice * debtToCover * vars.collateralAssetUnit)) /
      (vars.collateralPrice * vars.debtAssetUnit);

    vars.maxCollateralToLiquidate = vars.baseCollateral.percentMul(liquidationBonus);

    if (vars.maxCollateralToLiquidate > userCollateralBalance) {
      vars.collateralAmount = userCollateralBalance;
      vars.debtAmountNeeded = ((vars.collateralPrice * vars.collateralAmount * vars.debtAssetUnit) /
        (vars.debtAssetPrice * vars.collateralAssetUnit)).percentDiv(liquidationBonus);
    } else {
      vars.collateralAmount = vars.maxCollateralToLiquidate;
      vars.debtAmountNeeded = debtToCover;
    }

    if (vars.liquidationProtocolFeePercentage != 0) {
      vars.bonusCollateral =
        vars.collateralAmount -
        vars.collateralAmount.percentDiv(liquidationBonus);

      vars.liquidationProtocolFee = vars.bonusCollateral.percentMul(
        vars.liquidationProtocolFeePercentage
      );

      return (
        vars.collateralAmount - vars.liquidationProtocolFee,
        vars.debtAmountNeeded,
        vars.liquidationProtocolFee
      );
    } else {
      return (vars.collateralAmount, vars.debtAmountNeeded, 0);
    }
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.10;

import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {Address} from '../../../dependencies/openzeppelin/contracts/Address.sol';
import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {Errors} from '../helpers/Errors.sol';
import {WadRayMath} from '../math/WadRayMath.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {ReserveLogic} from './ReserveLogic.sol';
import {ValidationLogic} from './ValidationLogic.sol';
import {GenericLogic} from './GenericLogic.sol';

/**
 * @title PoolLogic library
 * @author Aave
 * @notice Implements the logic for Pool specific functions
 */
library PoolLogic {
  using GPv2SafeERC20 for IERC20;
  using WadRayMath for uint256;
  using ReserveLogic for DataTypes.ReserveData;
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;

  // See `IPool` for descriptions
  event MintedToTreasury(address indexed reserve, uint256 amountMinted);
  event IsolationModeTotalDebtUpdated(address indexed asset, uint256 totalDebt);

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
    require(Address.isContract(params.asset), Errors.NOT_CONTRACT);
    reservesData[params.asset].init(
      params.aTokenAddress,
      params.stableDebtAddress,
      params.variableDebtAddress,
      params.interestRateStrategyAddress
    );

    bool reserveAlreadyAdded = reservesData[params.asset].id != 0 ||
      reservesList[0] == params.asset;
    require(!reserveAlreadyAdded, Errors.RESERVE_ALREADY_ADDED);

    for (uint16 i = 0; i < params.reservesCount; i++) {
      if (reservesList[i] == address(0)) {
        reservesData[params.asset].id = i;
        reservesList[i] = params.asset;
        return false;
      }
    }

    require(params.reservesCount < params.maxNumberReserves, Errors.NO_MORE_RESERVES_ALLOWED);
    reservesData[params.asset].id = params.reservesCount;
    reservesList[params.reservesCount] = params.asset;
    return true;
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
        uint256 amountToMint = accruedToTreasury.rayMul(normalizedIncome);
        IAToken(reserve.aTokenAddress).mintToTreasury(amountToMint, normalizedIncome);

        emit MintedToTreasury(assetAddress, amountToMint);
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
    require(reservesData[asset].configuration.getDebtCeiling() == 0, Errors.DEBT_CEILING_NOT_ZERO);
    reservesData[asset].isolationModeTotalDebt = 0;
    emit IsolationModeTotalDebtUpdated(asset, 0);
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

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IPoolAddressesProvider} from '../../interfaces/IPoolAddressesProvider.sol';
import {IPool} from '../../interfaces/IPool.sol';

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

    uint256 expMinusOne;
    uint256 expMinusTwo;
    uint256 basePowerTwo;
    uint256 basePowerThree;
    unchecked {
      expMinusOne = exp - 1;

      expMinusTwo = exp > 2 ? exp - 2 : 0;

      basePowerTwo = rate.rayMul(rate) / (SECONDS_PER_YEAR * SECONDS_PER_YEAR);
      basePowerThree = basePowerTwo.rayMul(rate) / SECONDS_PER_YEAR;
    }

    uint256 secondTerm = exp * expMinusOne * basePowerTwo;
    unchecked {
      secondTerm /= 2;
    }
    uint256 thirdTerm = exp * expMinusOne * expMinusTwo * basePowerThree;
    unchecked {
      thirdTerm /= 6;
    }

    return WadRayMath.RAY + (rate * exp) / SECONDS_PER_YEAR + secondTerm + thirdTerm;
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
pragma solidity ^0.8.0;

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
      require(reserveIndex < ReserveConfiguration.MAX_RESERVES_COUNT, Errors.INVALID_RESERVE_INDEX);
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
   * @param usingAsCollateral True if the user is using the reserve as collateral, false otherwise
   */
  function setUsingAsCollateral(
    DataTypes.UserConfigurationMap storage self,
    uint256 reserveIndex,
    bool usingAsCollateral
  ) internal {
    unchecked {
      require(reserveIndex < ReserveConfiguration.MAX_RESERVES_COUNT, Errors.INVALID_RESERVE_INDEX);
      uint256 bit = 1 << ((reserveIndex << 1) + 1);
      if (usingAsCollateral) {
        self.data |= bit;
      } else {
        self.data &= ~bit;
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
      require(reserveIndex < ReserveConfiguration.MAX_RESERVES_COUNT, Errors.INVALID_RESERVE_INDEX);
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
      require(reserveIndex < ReserveConfiguration.MAX_RESERVES_COUNT, Errors.INVALID_RESERVE_INDEX);
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
      require(reserveIndex < ReserveConfiguration.MAX_RESERVES_COUNT, Errors.INVALID_RESERVE_INDEX);
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

import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {DataTypes} from '../types/DataTypes.sol';

/**
 * @title Helpers library
 * @author Aave
 */
library Helpers {
  /**
   * @notice Fetches the user current stable and variable debt balances
   * @param user The user address
   * @param reserveCache The reserve cache data object
   * @return The stable debt balance
   * @return The variable debt balance
   */
  function getUserCurrentDebt(
    address user,
    DataTypes.ReserveCache memory reserveCache
  ) internal view returns (uint256, uint256) {
    return (
      IERC20(reserveCache.stableDebtTokenAddress).balanceOf(user),
      IERC20(reserveCache.variableDebtTokenAddress).balanceOf(user)
    );
  }
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
   * @param amount The amount of tokens getting minted
   * @param index The next liquidity index of the reserve
   * @return `true` if the the previous balance of the user was 0
   */
  function mint(
    address caller,
    address onBehalfOf,
    uint256 amount,
    uint256 index
  ) external returns (bool);

  /**
   * @notice Burns aTokens from `user` and sends the equivalent amount of underlying to `receiverOfUnderlying`
   * @dev In some instances, the mint event could be emitted from a burn transaction
   * if the amount to burn is less than the interest that the user accrued
   * @param from The address from which the aTokens will be burned
   * @param receiverOfUnderlying The address that will receive the underlying
   * @param amount The amount being burned
   * @param index The next liquidity index of the reserve
   */
  function burn(address from, address receiverOfUnderlying, uint256 amount, uint256 index) external;

  /**
   * @notice Mints aTokens to the reserve treasury
   * @param amount The amount of tokens getting minted
   * @param index The next liquidity index of the reserve
   */
  function mintToTreasury(uint256 amount, uint256 index) external;

  /**
   * @notice Transfers aTokens in the event of a borrow being liquidated, in case the liquidators reclaims the aToken
   * @param from The address getting liquidated, current owner of the aTokens
   * @param to The recipient
   * @param value The amount of tokens getting transferred
   */
  function transferOnLiquidation(address from, address to, uint256 value) external;

  /**
   * @notice Transfers the underlying asset to `target`.
   * @dev Used by the Pool to transfer assets in borrow(), withdraw() and flashLoan()
   * @param target The recipient of the underlying
   * @param amount The amount getting transferred
   */
  function transferUnderlyingTo(address target, uint256 amount) external;

  /**
   * @notice Handles the underlying received by the aToken after the transfer has been completed.
   * @dev The default implementation is empty as with standard ERC20 tokens, nothing needs to be done after the
   * transfer is concluded. However in the future there may be aTokens that allow for example to stake the underlying
   * to receive LM rewards. In that case, `handleRepayment()` would perform the staking of the underlying asset.
   * @param user The user executing the repayment
   * @param onBehalfOf The address of the user who will get his debt reduced/removed
   * @param amount The amount getting repaid
   */
  function handleRepayment(address user, address onBehalfOf, uint256 amount) external;

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

import {IScaledBalanceToken} from './IScaledBalanceToken.sol';
import {IInitializableDebtToken} from './IInitializableDebtToken.sol';

/**
 * @title IVariableDebtToken
 * @author Aave
 * @notice Defines the basic interface for a variable debt token.
 */
interface IVariableDebtToken is IScaledBalanceToken, IInitializableDebtToken {
  /**
   * @notice Mints debt token to the `onBehalfOf` address
   * @param user The address receiving the borrowed underlying, being the delegatee in case
   * of credit delegate, or same as `onBehalfOf` otherwise
   * @param onBehalfOf The address receiving the debt tokens
   * @param amount The amount of debt being minted
   * @param index The variable debt index of the reserve
   * @return True if the previous balance of the user is 0, false otherwise
   * @return The scaled total debt of the reserve
   */
  function mint(
    address user,
    address onBehalfOf,
    uint256 amount,
    uint256 index
  ) external returns (bool, uint256);

  /**
   * @notice Burns user variable debt
   * @dev In some instances, a burn transaction will emit a mint event
   * if the amount to burn is less than the interest that the user accrued
   * @param from The address from which the debt will be burned
   * @param amount The amount getting burned
   * @param index The variable debt index of the reserve
   * @return The scaled total debt of the reserve
   */
  function burn(address from, uint256 amount, uint256 index) external returns (uint256);

  /**
   * @notice Returns the address of the underlying asset of this debtToken (E.g. WETH for variableDebtWETH)
   * @return The address of the underlying asset
   */
  function UNDERLYING_ASSET_ADDRESS() external view returns (address);
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.10;

import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {Errors} from '../helpers/Errors.sol';
import {UserConfiguration} from '../configuration/UserConfiguration.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {WadRayMath} from '../math/WadRayMath.sol';
import {PercentageMath} from '../math/PercentageMath.sol';
import {ValidationLogic} from './ValidationLogic.sol';
import {ReserveLogic} from './ReserveLogic.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';

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
  using WadRayMath for uint256;
  using PercentageMath for uint256;

  // See `IPool` for descriptions
  event ReserveUsedAsCollateralEnabled(address indexed reserve, address indexed user);
  event ReserveUsedAsCollateralDisabled(address indexed reserve, address indexed user);
  event Withdraw(address indexed reserve, address indexed user, address indexed to, uint256 amount);
  event Supply(
    address indexed reserve,
    address user,
    address indexed onBehalfOf,
    uint256 amount,
    uint16 indexed referralCode
  );

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

    ValidationLogic.validateSupply(reserveCache, reserve, params.amount);

    reserve.updateInterestRates(reserveCache, params.asset, params.amount, 0);

    IERC20(params.asset).safeTransferFrom(msg.sender, reserveCache.aTokenAddress, params.amount);

    bool isFirstSupply = IAToken(reserveCache.aTokenAddress).mint(
      msg.sender,
      params.onBehalfOf,
      params.amount,
      reserveCache.nextLiquidityIndex
    );

    if (isFirstSupply) {
      if (
        ValidationLogic.validateAutomaticUseAsCollateral(
          reservesData,
          reservesList,
          userConfig,
          reserveCache.reserveConfiguration,
          reserveCache.aTokenAddress
        )
      ) {
        userConfig.setUsingAsCollateral(reserve.id, true);
        emit ReserveUsedAsCollateralEnabled(params.asset, params.onBehalfOf);
      }
    }

    emit Supply(params.asset, msg.sender, params.onBehalfOf, params.amount, params.referralCode);
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

    reserve.updateState(reserveCache);

    uint256 userBalance = IAToken(reserveCache.aTokenAddress).scaledBalanceOf(msg.sender).rayMul(
      reserveCache.nextLiquidityIndex
    );

    uint256 amountToWithdraw = params.amount;

    if (params.amount == type(uint256).max) {
      amountToWithdraw = userBalance;
    }

    ValidationLogic.validateWithdraw(reserveCache, amountToWithdraw, userBalance);

    reserve.updateInterestRates(reserveCache, params.asset, 0, amountToWithdraw);

    bool isCollateral = userConfig.isUsingAsCollateral(reserve.id);

    if (isCollateral && amountToWithdraw == userBalance) {
      userConfig.setUsingAsCollateral(reserve.id, false);
      emit ReserveUsedAsCollateralDisabled(params.asset, msg.sender);
    }

    IAToken(reserveCache.aTokenAddress).burn(
      msg.sender,
      params.to,
      amountToWithdraw,
      reserveCache.nextLiquidityIndex
    );

    if (isCollateral && userConfig.isBorrowingAny()) {
      ValidationLogic.validateHFAndLtv(
        reservesData,
        reservesList,
        eModeCategories,
        userConfig,
        params.asset,
        msg.sender,
        params.reservesCount,
        params.oracle,
        params.userEModeCategory
      );
    }

    emit Withdraw(params.asset, msg.sender, params.to, amountToWithdraw);

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

    if (params.from != params.to && params.amount != 0) {
      DataTypes.UserConfigurationMap storage fromConfig = usersConfig[params.from];

      if (fromConfig.isUsingAsCollateral(reserveId)) {
        if (fromConfig.isBorrowingAny()) {
          ValidationLogic.validateHFAndLtv(
            reservesData,
            reservesList,
            eModeCategories,
            usersConfig[params.from],
            params.asset,
            params.from,
            params.reservesCount,
            params.oracle,
            params.fromEModeCategory
          );
        }
        if (params.balanceFromBefore == params.amount) {
          fromConfig.setUsingAsCollateral(reserveId, false);
          emit ReserveUsedAsCollateralDisabled(params.asset, params.from);
        }
      }

      if (params.balanceToBefore == 0) {
        DataTypes.UserConfigurationMap storage toConfig = usersConfig[params.to];
        if (
          ValidationLogic.validateAutomaticUseAsCollateral(
            reservesData,
            reservesList,
            toConfig,
            reserve.configuration,
            reserve.aTokenAddress
          )
        ) {
          toConfig.setUsingAsCollateral(reserveId, true);
          emit ReserveUsedAsCollateralEnabled(params.asset, params.to);
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
   * @param asset The address of the asset being configured as collateral
   * @param useAsCollateral True if the user wants to set the asset as collateral, false otherwise
   * @param reservesCount The number of initialized reserves
   * @param priceOracle The address of the price oracle
   * @param userEModeCategory The eMode category chosen by the user
   */
  function executeUseReserveAsCollateral(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    DataTypes.UserConfigurationMap storage userConfig,
    address asset,
    bool useAsCollateral,
    uint256 reservesCount,
    address priceOracle,
    uint8 userEModeCategory
  ) external {
    DataTypes.ReserveData storage reserve = reservesData[asset];
    DataTypes.ReserveCache memory reserveCache = reserve.cache();

    uint256 userBalance = IERC20(reserveCache.aTokenAddress).balanceOf(msg.sender);

    ValidationLogic.validateSetUseReserveAsCollateral(reserveCache, userBalance);

    if (useAsCollateral == userConfig.isUsingAsCollateral(reserve.id)) return;

    if (useAsCollateral) {
      require(
        ValidationLogic.validateUseAsCollateral(
          reservesData,
          reservesList,
          userConfig,
          reserveCache.reserveConfiguration
        ),
        Errors.USER_IN_ISOLATION_MODE_OR_LTV_ZERO
      );

      userConfig.setUsingAsCollateral(reserve.id, true);
      emit ReserveUsedAsCollateralEnabled(asset, msg.sender);
    } else {
      userConfig.setUsingAsCollateral(reserve.id, false);
      ValidationLogic.validateHFAndLtv(
        reservesData,
        reservesList,
        eModeCategories,
        userConfig,
        asset,
        msg.sender,
        reservesCount,
        priceOracle,
        userEModeCategory
      );

      emit ReserveUsedAsCollateralDisabled(asset, msg.sender);
    }
  }
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
   * @param treasury The address of the Aave treasury, receiving the fees on this aToken
   * @param underlyingAsset The address of the underlying asset of this aToken (E.g. WETH for aWETH)
   * @param incentivesController The smart contract managing potential incentives distribution
   * @param aTokenDecimals The decimals of the aToken, same as the underlying asset's
   * @param aTokenName The name of the aToken
   * @param aTokenSymbol The symbol of the aToken
   * @param params A set of encoded parameters for additional initialization
   */
  function initialize(
    IPool pool,
    address treasury,
    address underlyingAsset,
    IAaveIncentivesController incentivesController,
    uint8 aTokenDecimals,
    string calldata aTokenName,
    string calldata aTokenSymbol,
    bytes calldata params
  ) external;
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {DataTypes} from '../protocol/libraries/types/DataTypes.sol';

/**
 * @title IReserveInterestRateStrategy
 * @author Aave
 * @notice Interface for the calculation of the interest rates
 */
interface IReserveInterestRateStrategy {
  /**
   * @notice Calculates the interest rates depending on the reserve's state and configurations
   * @param params The parameters needed to calculate interest rates
   * @return liquidityRate The liquidity rate expressed in rays
   * @return stableBorrowRate The stable borrow rate expressed in rays
   * @return variableBorrowRate The variable borrow rate expressed in rays
   */
  function calculateInterestRates(
    DataTypes.CalculateInterestRatesParams memory params
  ) external view returns (uint256, uint256, uint256);
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
   * @param incentivesController The smart contract managing potential incentives distribution
   * @param debtTokenDecimals The decimals of the debtToken, same as the underlying asset's
   * @param debtTokenName The name of the token
   * @param debtTokenSymbol The symbol of the token
   * @param params A set of encoded parameters for additional initialization
   */
  function initialize(
    IPool pool,
    address underlyingAsset,
    IAaveIncentivesController incentivesController,
    uint8 debtTokenDecimals,
    string memory debtTokenName,
    string memory debtTokenSymbol,
    bytes calldata params
  ) external;
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
pragma solidity ^0.8.10;

import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {SafeCast} from '../../../dependencies/openzeppelin/contracts/SafeCast.sol';
import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {IStableDebtToken} from '../../../interfaces/IStableDebtToken.sol';
import {IVariableDebtToken} from '../../../interfaces/IVariableDebtToken.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {UserConfiguration} from '../configuration/UserConfiguration.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {Helpers} from '../helpers/Helpers.sol';
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
  using ReserveLogic for DataTypes.ReserveCache;
  using ReserveLogic for DataTypes.ReserveData;
  using GPv2SafeERC20 for IERC20;
  using UserConfiguration for DataTypes.UserConfigurationMap;
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;
  using SafeCast for uint256;

  // See `IPool` for descriptions
  event Borrow(
    address indexed reserve,
    address user,
    address indexed onBehalfOf,
    uint256 amount,
    DataTypes.InterestRateMode interestRateMode,
    uint256 borrowRate,
    uint16 indexed referralCode
  );
  event Repay(
    address indexed reserve,
    address indexed user,
    address indexed repayer,
    uint256 amount,
    bool useATokens
  );
  event RebalanceStableBorrowRate(address indexed reserve, address indexed user);
  event SwapBorrowRateMode(
    address indexed reserve,
    address indexed user,
    DataTypes.InterestRateMode interestRateMode
  );
  event IsolationModeTotalDebtUpdated(address indexed asset, uint256 totalDebt);

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
  ) public {
    DataTypes.ReserveData storage reserve = reservesData[params.asset];
    DataTypes.ReserveCache memory reserveCache = reserve.cache();

    reserve.updateState(reserveCache);

    (
      bool isolationModeActive,
      address isolationModeCollateralAddress,
      uint256 isolationModeDebtCeiling
    ) = userConfig.getIsolationModeState(reservesData, reservesList);

    ValidationLogic.validateBorrow(
      reservesData,
      reservesList,
      eModeCategories,
      DataTypes.ValidateBorrowParams({
        reserveCache: reserveCache,
        userConfig: userConfig,
        asset: params.asset,
        userAddress: params.onBehalfOf,
        amount: params.amount,
        interestRateMode: params.interestRateMode,
        maxStableLoanPercent: params.maxStableRateBorrowSizePercent,
        reservesCount: params.reservesCount,
        oracle: params.oracle,
        userEModeCategory: params.userEModeCategory,
        priceOracleSentinel: params.priceOracleSentinel,
        isolationModeActive: isolationModeActive,
        isolationModeCollateralAddress: isolationModeCollateralAddress,
        isolationModeDebtCeiling: isolationModeDebtCeiling
      })
    );

    uint256 currentStableRate = 0;
    bool isFirstBorrowing = false;

    if (params.interestRateMode == DataTypes.InterestRateMode.STABLE) {
      currentStableRate = reserve.currentStableBorrowRate;

      (
        isFirstBorrowing,
        reserveCache.nextTotalStableDebt,
        reserveCache.nextAvgStableBorrowRate
      ) = IStableDebtToken(reserveCache.stableDebtTokenAddress).mint(
        params.user,
        params.onBehalfOf,
        params.amount,
        currentStableRate
      );
    } else {
      (isFirstBorrowing, reserveCache.nextScaledVariableDebt) = IVariableDebtToken(
        reserveCache.variableDebtTokenAddress
      ).mint(params.user, params.onBehalfOf, params.amount, reserveCache.nextVariableBorrowIndex);
    }

    if (isFirstBorrowing) {
      userConfig.setBorrowing(reserve.id, true);
    }

    if (isolationModeActive) {
      uint256 nextIsolationModeTotalDebt = reservesData[isolationModeCollateralAddress]
        .isolationModeTotalDebt += (params.amount /
        10 **
          (reserveCache.reserveConfiguration.getDecimals() -
            ReserveConfiguration.DEBT_CEILING_DECIMALS)).toUint128();
      emit IsolationModeTotalDebtUpdated(
        isolationModeCollateralAddress,
        nextIsolationModeTotalDebt
      );
    }

    reserve.updateInterestRates(
      reserveCache,
      params.asset,
      0,
      params.releaseUnderlying ? params.amount : 0
    );

    if (params.releaseUnderlying) {
      IAToken(reserveCache.aTokenAddress).transferUnderlyingTo(params.user, params.amount);
    }

    emit Borrow(
      params.asset,
      params.user,
      params.onBehalfOf,
      params.amount,
      params.interestRateMode,
      params.interestRateMode == DataTypes.InterestRateMode.STABLE
        ? currentStableRate
        : reserve.currentVariableBorrowRate,
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
   * @param userConfig The user configuration mapping that tracks the supplied/borrowed assets
   * @param params The additional parameters needed to execute the repay function
   * @return The actual amount being repaid
   */
  function executeRepay(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    DataTypes.UserConfigurationMap storage userConfig,
    DataTypes.ExecuteRepayParams memory params
  ) external returns (uint256) {
    DataTypes.ReserveData storage reserve = reservesData[params.asset];
    DataTypes.ReserveCache memory reserveCache = reserve.cache();
    reserve.updateState(reserveCache);

    (uint256 stableDebt, uint256 variableDebt) = Helpers.getUserCurrentDebt(
      params.onBehalfOf,
      reserveCache
    );

    ValidationLogic.validateRepay(
      reserveCache,
      params.amount,
      params.interestRateMode,
      params.onBehalfOf,
      stableDebt,
      variableDebt
    );

    uint256 paybackAmount = params.interestRateMode == DataTypes.InterestRateMode.STABLE
      ? stableDebt
      : variableDebt;

    // Allows a user to repay with aTokens without leaving dust from interest.
    if (params.useATokens && params.amount == type(uint256).max) {
      params.amount = IAToken(reserveCache.aTokenAddress).balanceOf(msg.sender);
    }

    if (params.amount < paybackAmount) {
      paybackAmount = params.amount;
    }

    if (params.interestRateMode == DataTypes.InterestRateMode.STABLE) {
      (reserveCache.nextTotalStableDebt, reserveCache.nextAvgStableBorrowRate) = IStableDebtToken(
        reserveCache.stableDebtTokenAddress
      ).burn(params.onBehalfOf, paybackAmount);
    } else {
      reserveCache.nextScaledVariableDebt = IVariableDebtToken(
        reserveCache.variableDebtTokenAddress
      ).burn(params.onBehalfOf, paybackAmount, reserveCache.nextVariableBorrowIndex);
    }

    reserve.updateInterestRates(
      reserveCache,
      params.asset,
      params.useATokens ? 0 : paybackAmount,
      0
    );

    if (stableDebt + variableDebt - paybackAmount == 0) {
      userConfig.setBorrowing(reserve.id, false);
    }

    IsolationModeLogic.updateIsolatedDebtIfIsolated(
      reservesData,
      reservesList,
      userConfig,
      reserveCache,
      paybackAmount
    );

    if (params.useATokens) {
      IAToken(reserveCache.aTokenAddress).burn(
        msg.sender,
        reserveCache.aTokenAddress,
        paybackAmount,
        reserveCache.nextLiquidityIndex
      );
    } else {
      IERC20(params.asset).safeTransferFrom(msg.sender, reserveCache.aTokenAddress, paybackAmount);
      IAToken(reserveCache.aTokenAddress).handleRepayment(
        msg.sender,
        params.onBehalfOf,
        paybackAmount
      );
    }

    emit Repay(params.asset, params.onBehalfOf, msg.sender, paybackAmount, params.useATokens);

    return paybackAmount;
  }

  /**
   * @notice Implements the rebalance stable borrow rate feature. In case of liquidity crunches on the protocol, stable
   * rate borrows might need to be rebalanced to bring back equilibrium between the borrow and supply APYs.
   * @dev The rules that define if a position can be rebalanced are implemented in `ValidationLogic.validateRebalanceStableBorrowRate()`
   * @dev Emits the `RebalanceStableBorrowRate()` event
   * @param reserve The state of the reserve of the asset being repaid
   * @param asset The asset of the position being rebalanced
   * @param user The user being rebalanced
   */
  function executeRebalanceStableBorrowRate(
    DataTypes.ReserveData storage reserve,
    address asset,
    address user
  ) external {
    DataTypes.ReserveCache memory reserveCache = reserve.cache();
    reserve.updateState(reserveCache);

    ValidationLogic.validateRebalanceStableBorrowRate(reserve, reserveCache, asset);

    IStableDebtToken stableDebtToken = IStableDebtToken(reserveCache.stableDebtTokenAddress);
    uint256 stableDebt = IERC20(address(stableDebtToken)).balanceOf(user);

    stableDebtToken.burn(user, stableDebt);

    (, reserveCache.nextTotalStableDebt, reserveCache.nextAvgStableBorrowRate) = stableDebtToken
      .mint(user, user, stableDebt, reserve.currentStableBorrowRate);

    reserve.updateInterestRates(reserveCache, asset, 0, 0);

    emit RebalanceStableBorrowRate(asset, user);
  }

  /**
   * @notice Implements the swap borrow rate feature. Borrowers can swap from variable to stable positions at any time.
   * @dev Emits the `Swap()` event
   * @param reserve The of the reserve of the asset being repaid
   * @param userConfig The user configuration mapping that tracks the supplied/borrowed assets
   * @param asset The asset of the position being swapped
   * @param interestRateMode The current interest rate mode of the position being swapped
   */
  function executeSwapBorrowRateMode(
    DataTypes.ReserveData storage reserve,
    DataTypes.UserConfigurationMap storage userConfig,
    address asset,
    DataTypes.InterestRateMode interestRateMode
  ) external {
    DataTypes.ReserveCache memory reserveCache = reserve.cache();

    reserve.updateState(reserveCache);

    (uint256 stableDebt, uint256 variableDebt) = Helpers.getUserCurrentDebt(
      msg.sender,
      reserveCache
    );

    ValidationLogic.validateSwapRateMode(
      reserve,
      reserveCache,
      userConfig,
      stableDebt,
      variableDebt,
      interestRateMode
    );

    if (interestRateMode == DataTypes.InterestRateMode.STABLE) {
      (reserveCache.nextTotalStableDebt, reserveCache.nextAvgStableBorrowRate) = IStableDebtToken(
        reserveCache.stableDebtTokenAddress
      ).burn(msg.sender, stableDebt);

      (, reserveCache.nextScaledVariableDebt) = IVariableDebtToken(
        reserveCache.variableDebtTokenAddress
      ).mint(msg.sender, msg.sender, stableDebt, reserveCache.nextVariableBorrowIndex);
    } else {
      reserveCache.nextScaledVariableDebt = IVariableDebtToken(
        reserveCache.variableDebtTokenAddress
      ).burn(msg.sender, variableDebt, reserveCache.nextVariableBorrowIndex);

      (, reserveCache.nextTotalStableDebt, reserveCache.nextAvgStableBorrowRate) = IStableDebtToken(
        reserveCache.stableDebtTokenAddress
      ).mint(msg.sender, msg.sender, variableDebt, reserve.currentStableBorrowRate);
    }

    reserve.updateInterestRates(reserveCache, asset, 0, 0);

    emit SwapBorrowRateMode(asset, msg.sender, interestRateMode);
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.10;

import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {IStableDebtToken} from '../../../interfaces/IStableDebtToken.sol';
import {IVariableDebtToken} from '../../../interfaces/IVariableDebtToken.sol';
import {IReserveInterestRateStrategy} from '../../../interfaces/IReserveInterestRateStrategy.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {MathUtils} from '../math/MathUtils.sol';
import {WadRayMath} from '../math/WadRayMath.sol';
import {PercentageMath} from '../math/PercentageMath.sol';
import {Errors} from '../helpers/Errors.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {SafeCast} from '../../../dependencies/openzeppelin/contracts/SafeCast.sol';

/**
 * @title ReserveLogic library
 * @author Aave
 * @notice Implements the logic to update the reserves state
 */
library ReserveLogic {
  using WadRayMath for uint256;
  using PercentageMath for uint256;
  using SafeCast for uint256;
  using GPv2SafeERC20 for IERC20;
  using ReserveLogic for DataTypes.ReserveData;
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;

  // See `IPool` for descriptions
  event ReserveDataUpdated(
    address indexed reserve,
    uint256 liquidityRate,
    uint256 stableBorrowRate,
    uint256 variableBorrowRate,
    uint256 liquidityIndex,
    uint256 variableBorrowIndex
  );

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
   * @notice Updates the liquidity cumulative index and the variable borrow index.
   * @param reserve The reserve object
   * @param reserveCache The caching layer for the reserve data
   */
  function updateState(
    DataTypes.ReserveData storage reserve,
    DataTypes.ReserveCache memory reserveCache
  ) internal {
    // If time didn't pass since last stored timestamp, skip state update
    //solium-disable-next-line
    if (reserve.lastUpdateTimestamp == uint40(block.timestamp)) {
      return;
    }

    _updateIndexes(reserve, reserveCache);
    _accrueToTreasury(reserve, reserveCache);

    //solium-disable-next-line
    reserve.lastUpdateTimestamp = uint40(block.timestamp);
  }

  /**
   * @notice Accumulates a predefined amount of asset to the reserve as a fixed, instantaneous income. Used for example
   * to accumulate the flashloan fee to the reserve, and spread it between all the suppliers.
   * @param reserve The reserve object
   * @param totalLiquidity The total liquidity available in the reserve
   * @param amount The amount to accumulate
   * @return The next liquidity index of the reserve
   */
  function cumulateToLiquidityIndex(
    DataTypes.ReserveData storage reserve,
    uint256 totalLiquidity,
    uint256 amount
  ) internal returns (uint256) {
    //next liquidity index is calculated this way: `((amount / totalLiquidity) + 1) * liquidityIndex`
    //division `amount / totalLiquidity` done in ray for precision
    uint256 result = (amount.wadToRay().rayDiv(totalLiquidity.wadToRay()) + WadRayMath.RAY).rayMul(
      reserve.liquidityIndex
    );
    reserve.liquidityIndex = result.toUint128();
    return result;
  }

  /**
   * @notice Initializes a reserve.
   * @param reserve The reserve object
   * @param aTokenAddress The address of the overlying atoken contract
   * @param stableDebtTokenAddress The address of the overlying stable debt token contract
   * @param variableDebtTokenAddress The address of the overlying variable debt token contract
   * @param interestRateStrategyAddress The address of the interest rate strategy contract
   */
  function init(
    DataTypes.ReserveData storage reserve,
    address aTokenAddress,
    address stableDebtTokenAddress,
    address variableDebtTokenAddress,
    address interestRateStrategyAddress
  ) internal {
    require(reserve.aTokenAddress == address(0), Errors.RESERVE_ALREADY_INITIALIZED);

    reserve.liquidityIndex = uint128(WadRayMath.RAY);
    reserve.variableBorrowIndex = uint128(WadRayMath.RAY);
    reserve.aTokenAddress = aTokenAddress;
    reserve.stableDebtTokenAddress = stableDebtTokenAddress;
    reserve.variableDebtTokenAddress = variableDebtTokenAddress;
    reserve.interestRateStrategyAddress = interestRateStrategyAddress;
  }

  struct UpdateInterestRatesLocalVars {
    uint256 nextLiquidityRate;
    uint256 nextStableRate;
    uint256 nextVariableRate;
    uint256 totalVariableDebt;
  }

  /**
   * @notice Updates the reserve current stable borrow rate, the current variable borrow rate and the current liquidity rate.
   * @param reserve The reserve reserve to be updated
   * @param reserveCache The caching layer for the reserve data
   * @param reserveAddress The address of the reserve to be updated
   * @param liquidityAdded The amount of liquidity added to the protocol (supply or repay) in the previous action
   * @param liquidityTaken The amount of liquidity taken from the protocol (redeem or borrow)
   */
  function updateInterestRates(
    DataTypes.ReserveData storage reserve,
    DataTypes.ReserveCache memory reserveCache,
    address reserveAddress,
    uint256 liquidityAdded,
    uint256 liquidityTaken
  ) internal {
    UpdateInterestRatesLocalVars memory vars;

    vars.totalVariableDebt = reserveCache.nextScaledVariableDebt.rayMul(
      reserveCache.nextVariableBorrowIndex
    );

    (
      vars.nextLiquidityRate,
      vars.nextStableRate,
      vars.nextVariableRate
    ) = IReserveInterestRateStrategy(reserve.interestRateStrategyAddress).calculateInterestRates(
      DataTypes.CalculateInterestRatesParams({
        unbacked: reserve.unbacked,
        liquidityAdded: liquidityAdded,
        liquidityTaken: liquidityTaken,
        totalStableDebt: reserveCache.nextTotalStableDebt,
        totalVariableDebt: vars.totalVariableDebt,
        averageStableBorrowRate: reserveCache.nextAvgStableBorrowRate,
        reserveFactor: reserveCache.reserveFactor,
        reserve: reserveAddress,
        aToken: reserveCache.aTokenAddress
      })
    );

    reserve.currentLiquidityRate = vars.nextLiquidityRate.toUint128();
    reserve.currentStableBorrowRate = vars.nextStableRate.toUint128();
    reserve.currentVariableBorrowRate = vars.nextVariableRate.toUint128();

    emit ReserveDataUpdated(
      reserveAddress,
      vars.nextLiquidityRate,
      vars.nextStableRate,
      vars.nextVariableRate,
      reserveCache.nextLiquidityIndex,
      reserveCache.nextVariableBorrowIndex
    );
  }

  struct AccrueToTreasuryLocalVars {
    uint256 prevTotalStableDebt;
    uint256 prevTotalVariableDebt;
    uint256 currTotalVariableDebt;
    uint256 cumulatedStableInterest;
    uint256 totalDebtAccrued;
    uint256 amountToMint;
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
    AccrueToTreasuryLocalVars memory vars;

    if (reserveCache.reserveFactor == 0) {
      return;
    }

    //calculate the total variable debt at moment of the last interaction
    vars.prevTotalVariableDebt = reserveCache.currScaledVariableDebt.rayMul(
      reserveCache.currVariableBorrowIndex
    );

    //calculate the new total variable debt after accumulation of the interest on the index
    vars.currTotalVariableDebt = reserveCache.currScaledVariableDebt.rayMul(
      reserveCache.nextVariableBorrowIndex
    );

    //calculate the stable debt until the last timestamp update
    vars.cumulatedStableInterest = MathUtils.calculateCompoundedInterest(
      reserveCache.currAvgStableBorrowRate,
      reserveCache.stableDebtLastUpdateTimestamp,
      reserveCache.reserveLastUpdateTimestamp
    );

    vars.prevTotalStableDebt = reserveCache.currPrincipalStableDebt.rayMul(
      vars.cumulatedStableInterest
    );

    //debt accrued is the sum of the current debt minus the sum of the debt at the last update
    vars.totalDebtAccrued =
      vars.currTotalVariableDebt +
      reserveCache.currTotalStableDebt -
      vars.prevTotalVariableDebt -
      vars.prevTotalStableDebt;

    vars.amountToMint = vars.totalDebtAccrued.percentMul(reserveCache.reserveFactor);

    if (vars.amountToMint != 0) {
      reserve.accruedToTreasury += vars
        .amountToMint
        .rayDiv(reserveCache.nextLiquidityIndex)
        .toUint128();
    }
  }

  /**
   * @notice Updates the reserve indexes and the timestamp of the update.
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
    reserveCache.stableDebtTokenAddress = reserve.stableDebtTokenAddress;
    reserveCache.variableDebtTokenAddress = reserve.variableDebtTokenAddress;

    reserveCache.reserveLastUpdateTimestamp = reserve.lastUpdateTimestamp;

    reserveCache.currScaledVariableDebt = reserveCache.nextScaledVariableDebt = IVariableDebtToken(
      reserveCache.variableDebtTokenAddress
    ).scaledTotalSupply();

    (
      reserveCache.currPrincipalStableDebt,
      reserveCache.currTotalStableDebt,
      reserveCache.currAvgStableBorrowRate,
      reserveCache.stableDebtLastUpdateTimestamp
    ) = IStableDebtToken(reserveCache.stableDebtTokenAddress).getSupplyData();

    // by default the actions are considered as not affecting the debt balances.
    // if the action involves mint/burn of debt, the cache needs to be updated
    reserveCache.nextTotalStableDebt = reserveCache.currTotalStableDebt;
    reserveCache.nextAvgStableBorrowRate = reserveCache.currAvgStableBorrowRate;

    return reserveCache;
  }
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

/**
 * @title WadRayMath library
 * @author Aave
 * @notice Provides functions to perform calculations with Wad and Ray units
 * @dev Provides mul and div function for wads (decimal numbers with 18 digits of precision) and rays (decimal numbers
 * with 27 digits of precision)
 * @dev Operations are rounded. If a value is >=.5, will be rounded up, otherwise rounded down.
 */
library WadRayMath {
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

  /**
   * @notice Multiplies two ray, rounding half up to the nearest ray
   * @dev assembly optimized for improved gas savings, see https://twitter.com/transmissions11/status/1451131036377571328
   * @param a Ray
   * @param b Ray
   * @return c = a raymul b
   */
  function rayMul(uint256 a, uint256 b) internal pure returns (uint256 c) {
    // to avoid overflow, a <= (type(uint256).max - HALF_RAY) / b
    assembly {
      if iszero(or(iszero(b), iszero(gt(a, div(sub(not(0), HALF_RAY), b))))) {
        revert(0, 0)
      }

      c := div(add(mul(a, b), HALF_RAY), RAY)
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
    // to avoid overflow, a <= (type(uint256).max - halfB) / RAY
    assembly {
      if or(iszero(b), iszero(iszero(gt(a, div(sub(not(0), div(b, 2)), RAY))))) {
        revert(0, 0)
      }

      c := div(add(mul(a, RAY), div(b, 2)), b)
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
pragma solidity ^0.8.10;

import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {IPriceOracleGetter} from '../../../interfaces/IPriceOracleGetter.sol';
import {UserConfiguration} from '../configuration/UserConfiguration.sol';
import {Errors} from '../helpers/Errors.sol';
import {WadRayMath} from '../math/WadRayMath.sol';
import {PercentageMath} from '../math/PercentageMath.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {ValidationLogic} from './ValidationLogic.sol';
import {ReserveLogic} from './ReserveLogic.sol';

/**
 * @title EModeLogic library
 * @author Aave
 * @notice Implements the base logic for all the actions related to the eMode
 */
library EModeLogic {
  using ReserveLogic for DataTypes.ReserveCache;
  using ReserveLogic for DataTypes.ReserveData;
  using GPv2SafeERC20 for IERC20;
  using UserConfiguration for DataTypes.UserConfigurationMap;
  using WadRayMath for uint256;
  using PercentageMath for uint256;

  // See `IPool` for descriptions
  event UserEModeSet(address indexed user, uint8 categoryId);

  /**
   * @notice Updates the user efficiency mode category
   * @dev Will revert if user is borrowing non-compatible asset or change will drop HF < HEALTH_FACTOR_LIQUIDATION_THRESHOLD
   * @dev Emits the `UserEModeSet` event
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param usersEModeCategory The state of all users efficiency mode category
   * @param userConfig The user configuration mapping that tracks the supplied/borrowed assets
   * @param params The additional parameters needed to execute the setUserEMode function
   */
  function executeSetUserEMode(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    mapping(uint8 => DataTypes.EModeCategory) storage eModeCategories,
    mapping(address => uint8) storage usersEModeCategory,
    DataTypes.UserConfigurationMap storage userConfig,
    DataTypes.ExecuteSetUserEModeParams memory params
  ) external {
    ValidationLogic.validateSetUserEMode(
      reservesData,
      reservesList,
      eModeCategories,
      userConfig,
      params.reservesCount,
      params.categoryId
    );

    uint8 prevCategoryId = usersEModeCategory[msg.sender];
    usersEModeCategory[msg.sender] = params.categoryId;

    if (prevCategoryId != 0) {
      ValidationLogic.validateHealthFactor(
        reservesData,
        reservesList,
        eModeCategories,
        userConfig,
        msg.sender,
        params.categoryId,
        params.reservesCount,
        params.oracle
      );
    }
    emit UserEModeSet(msg.sender, params.categoryId);
  }

  /**
   * @notice Gets the eMode configuration and calculates the eMode asset price if a custom oracle is configured
   * @dev The eMode asset price returned is 0 if no oracle is specified
   * @param category The user eMode category
   * @param oracle The price oracle
   * @return The eMode ltv
   * @return The eMode liquidation threshold
   * @return The eMode asset price
   */
  function getEModeConfiguration(
    DataTypes.EModeCategory storage category,
    IPriceOracleGetter oracle
  ) internal view returns (uint256, uint256, uint256) {
    uint256 eModeAssetPrice = 0;
    address eModePriceSource = category.priceSource;

    if (eModePriceSource != address(0)) {
      eModeAssetPrice = oracle.getAssetPrice(eModePriceSource);
    }

    return (category.ltv, category.liquidationThreshold, eModeAssetPrice);
  }

  /**
   * @notice Checks if eMode is active for a user and if yes, if the asset belongs to the eMode category chosen
   * @param eModeUserCategory The user eMode category
   * @param eModeAssetCategory The asset eMode category
   * @return True if eMode is active and the asset belongs to the eMode category chosen by the user, false otherwise
   */
  function isInEModeCategory(
    uint256 eModeUserCategory,
    uint256 eModeAssetCategory
  ) internal pure returns (bool) {
    return (eModeUserCategory != 0 && eModeAssetCategory == eModeUserCategory);
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
  string public constant CALLER_NOT_POOL_ADMIN = '1'; // 'The caller of the function is not a pool admin'
  string public constant CALLER_NOT_EMERGENCY_ADMIN = '2'; // 'The caller of the function is not an emergency admin'
  string public constant CALLER_NOT_POOL_OR_EMERGENCY_ADMIN = '3'; // 'The caller of the function is not a pool or emergency admin'
  string public constant CALLER_NOT_RISK_OR_POOL_ADMIN = '4'; // 'The caller of the function is not a risk or pool admin'
  string public constant CALLER_NOT_ASSET_LISTING_OR_POOL_ADMIN = '5'; // 'The caller of the function is not an asset listing or pool admin'
  string public constant CALLER_NOT_BRIDGE = '6'; // 'The caller of the function is not a bridge'
  string public constant ADDRESSES_PROVIDER_NOT_REGISTERED = '7'; // 'Pool addresses provider is not registered'
  string public constant INVALID_ADDRESSES_PROVIDER_ID = '8'; // 'Invalid id for the pool addresses provider'
  string public constant NOT_CONTRACT = '9'; // 'Address is not a contract'
  string public constant CALLER_NOT_POOL_CONFIGURATOR = '10'; // 'The caller of the function is not the pool configurator'
  string public constant CALLER_NOT_ATOKEN = '11'; // 'The caller of the function is not an AToken'
  string public constant INVALID_ADDRESSES_PROVIDER = '12'; // 'The address of the pool addresses provider is invalid'
  string public constant INVALID_FLASHLOAN_EXECUTOR_RETURN = '13'; // 'Invalid return value of the flashloan executor function'
  string public constant RESERVE_ALREADY_ADDED = '14'; // 'Reserve has already been added to reserve list'
  string public constant NO_MORE_RESERVES_ALLOWED = '15'; // 'Maximum amount of reserves in the pool reached'
  string public constant EMODE_CATEGORY_RESERVED = '16'; // 'Zero eMode category is reserved for volatile heterogeneous assets'
  string public constant INVALID_EMODE_CATEGORY_ASSIGNMENT = '17'; // 'Invalid eMode category assignment to asset'
  string public constant RESERVE_LIQUIDITY_NOT_ZERO = '18'; // 'The liquidity of the reserve needs to be 0'
  string public constant FLASHLOAN_PREMIUM_INVALID = '19'; // 'Invalid flashloan premium'
  string public constant INVALID_RESERVE_PARAMS = '20'; // 'Invalid risk parameters for the reserve'
  string public constant INVALID_EMODE_CATEGORY_PARAMS = '21'; // 'Invalid risk parameters for the eMode category'
  string public constant BRIDGE_PROTOCOL_FEE_INVALID = '22'; // 'Invalid bridge protocol fee'
  string public constant CALLER_MUST_BE_POOL = '23'; // 'The caller of this function must be a pool'
  string public constant INVALID_MINT_AMOUNT = '24'; // 'Invalid amount to mint'
  string public constant INVALID_BURN_AMOUNT = '25'; // 'Invalid amount to burn'
  string public constant INVALID_AMOUNT = '26'; // 'Amount must be greater than 0'
  string public constant RESERVE_INACTIVE = '27'; // 'Action requires an active reserve'
  string public constant RESERVE_FROZEN = '28'; // 'Action cannot be performed because the reserve is frozen'
  string public constant RESERVE_PAUSED = '29'; // 'Action cannot be performed because the reserve is paused'
  string public constant BORROWING_NOT_ENABLED = '30'; // 'Borrowing is not enabled'
  string public constant STABLE_BORROWING_NOT_ENABLED = '31'; // 'Stable borrowing is not enabled'
  string public constant NOT_ENOUGH_AVAILABLE_USER_BALANCE = '32'; // 'User cannot withdraw more than the available balance'
  string public constant INVALID_INTEREST_RATE_MODE_SELECTED = '33'; // 'Invalid interest rate mode selected'
  string public constant COLLATERAL_BALANCE_IS_ZERO = '34'; // 'The collateral balance is 0'
  string public constant HEALTH_FACTOR_LOWER_THAN_LIQUIDATION_THRESHOLD = '35'; // 'Health factor is lesser than the liquidation threshold'
  string public constant COLLATERAL_CANNOT_COVER_NEW_BORROW = '36'; // 'There is not enough collateral to cover a new borrow'
  string public constant COLLATERAL_SAME_AS_BORROWING_CURRENCY = '37'; // 'Collateral is (mostly) the same currency that is being borrowed'
  string public constant AMOUNT_BIGGER_THAN_MAX_LOAN_SIZE_STABLE = '38'; // 'The requested amount is greater than the max loan size in stable rate mode'
  string public constant NO_DEBT_OF_SELECTED_TYPE = '39'; // 'For repayment of a specific type of debt, the user needs to have debt that type'
  string public constant NO_EXPLICIT_AMOUNT_TO_REPAY_ON_BEHALF = '40'; // 'To repay on behalf of a user an explicit amount to repay is needed'
  string public constant NO_OUTSTANDING_STABLE_DEBT = '41'; // 'User does not have outstanding stable rate debt on this reserve'
  string public constant NO_OUTSTANDING_VARIABLE_DEBT = '42'; // 'User does not have outstanding variable rate debt on this reserve'
  string public constant UNDERLYING_BALANCE_ZERO = '43'; // 'The underlying balance needs to be greater than 0'
  string public constant INTEREST_RATE_REBALANCE_CONDITIONS_NOT_MET = '44'; // 'Interest rate rebalance conditions were not met'
  string public constant HEALTH_FACTOR_NOT_BELOW_THRESHOLD = '45'; // 'Health factor is not below the threshold'
  string public constant COLLATERAL_CANNOT_BE_LIQUIDATED = '46'; // 'The collateral chosen cannot be liquidated'
  string public constant SPECIFIED_CURRENCY_NOT_BORROWED_BY_USER = '47'; // 'User did not borrow the specified currency'
  string public constant INCONSISTENT_FLASHLOAN_PARAMS = '49'; // 'Inconsistent flashloan parameters'
  string public constant BORROW_CAP_EXCEEDED = '50'; // 'Borrow cap is exceeded'
  string public constant SUPPLY_CAP_EXCEEDED = '51'; // 'Supply cap is exceeded'
  string public constant UNBACKED_MINT_CAP_EXCEEDED = '52'; // 'Unbacked mint cap is exceeded'
  string public constant DEBT_CEILING_EXCEEDED = '53'; // 'Debt ceiling is exceeded'
  string public constant UNDERLYING_CLAIMABLE_RIGHTS_NOT_ZERO = '54'; // 'Claimable rights over underlying not zero (aToken supply or accruedToTreasury)'
  string public constant STABLE_DEBT_NOT_ZERO = '55'; // 'Stable debt supply is not zero'
  string public constant VARIABLE_DEBT_SUPPLY_NOT_ZERO = '56'; // 'Variable debt supply is not zero'
  string public constant LTV_VALIDATION_FAILED = '57'; // 'Ltv validation failed'
  string public constant INCONSISTENT_EMODE_CATEGORY = '58'; // 'Inconsistent eMode category'
  string public constant PRICE_ORACLE_SENTINEL_CHECK_FAILED = '59'; // 'Price oracle sentinel validation failed'
  string public constant ASSET_NOT_BORROWABLE_IN_ISOLATION = '60'; // 'Asset is not borrowable in isolation mode'
  string public constant RESERVE_ALREADY_INITIALIZED = '61'; // 'Reserve has already been initialized'
  string public constant USER_IN_ISOLATION_MODE_OR_LTV_ZERO = '62'; // 'User is in isolation mode or ltv is zero'
  string public constant INVALID_LTV = '63'; // 'Invalid ltv parameter for the reserve'
  string public constant INVALID_LIQ_THRESHOLD = '64'; // 'Invalid liquidity threshold parameter for the reserve'
  string public constant INVALID_LIQ_BONUS = '65'; // 'Invalid liquidity bonus parameter for the reserve'
  string public constant INVALID_DECIMALS = '66'; // 'Invalid decimals parameter of the underlying asset of the reserve'
  string public constant INVALID_RESERVE_FACTOR = '67'; // 'Invalid reserve factor parameter for the reserve'
  string public constant INVALID_BORROW_CAP = '68'; // 'Invalid borrow cap for the reserve'
  string public constant INVALID_SUPPLY_CAP = '69'; // 'Invalid supply cap for the reserve'
  string public constant INVALID_LIQUIDATION_PROTOCOL_FEE = '70'; // 'Invalid liquidation protocol fee for the reserve'
  string public constant INVALID_EMODE_CATEGORY = '71'; // 'Invalid eMode category for the reserve'
  string public constant INVALID_UNBACKED_MINT_CAP = '72'; // 'Invalid unbacked mint cap for the reserve'
  string public constant INVALID_DEBT_CEILING = '73'; // 'Invalid debt ceiling for the reserve
  string public constant INVALID_RESERVE_INDEX = '74'; // 'Invalid reserve index'
  string public constant ACL_ADMIN_CANNOT_BE_ZERO = '75'; // 'ACL admin cannot be set to the zero address'
  string public constant INCONSISTENT_PARAMS_LENGTH = '76'; // 'Array parameters that should be equal length are not'
  string public constant ZERO_ADDRESS_NOT_VALID = '77'; // 'Zero address not valid'
  string public constant INVALID_EXPIRATION = '78'; // 'Invalid expiration'
  string public constant INVALID_SIGNATURE = '79'; // 'Invalid signature'
  string public constant OPERATION_NOT_SUPPORTED = '80'; // 'Operation not supported'
  string public constant DEBT_CEILING_NOT_ZERO = '81'; // 'Debt ceiling is not zero'
  string public constant ASSET_NOT_LISTED = '82'; // 'Asset is not listed'
  string public constant INVALID_OPTIMAL_USAGE_RATIO = '83'; // 'Invalid optimal usage ratio'
  string public constant INVALID_OPTIMAL_STABLE_TO_TOTAL_DEBT_RATIO = '84'; // 'Invalid optimal stable to total debt ratio'
  string public constant UNDERLYING_CANNOT_BE_RESCUED = '85'; // 'The underlying asset cannot be rescued'
  string public constant ADDRESSES_PROVIDER_ALREADY_ADDED = '86'; // 'Reserve has already been added to reserve list'
  string public constant POOL_ADDRESSES_DO_NOT_MATCH = '87'; // 'The token implementation pool address and the pool address provided by the initializing pool do not match'
  string public constant STABLE_BORROWING_ENABLED = '88'; // 'Stable borrowing is enabled'
  string public constant SILOED_BORROWING_VIOLATION = '89'; // 'User is trying to borrow multiple assets including a siloed one'
  string public constant RESERVE_DEBT_NOT_ZERO = '90'; // the total debt of the reserve needs to be 0
  string public constant FLASHLOAN_DISABLED = '91'; // FlashLoaning for this asset is disabled
}

