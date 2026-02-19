// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {IERC20} from '../../../dependencies/openzeppelin/contracts//IERC20.sol';
import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {PercentageMath} from '../../libraries/math/PercentageMath.sol';
import {WadRayMath} from '../../libraries/math/WadRayMath.sol';
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
  using WadRayMath for uint256;
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
   */
  function executeEliminateDeficit(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    DataTypes.UserConfigurationMap storage userConfig,
    DataTypes.ExecuteEliminateDeficitParams memory params
  ) external {
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

    uint256 userBalance = IAToken(reserveCache.aTokenAddress).scaledBalanceOf(params.user).rayMul(
      reserveCache.nextLiquidityIndex
    );
    require(balanceWriteOff <= userBalance, Errors.NotEnoughAvailableUserBalance());

    bool isCollateral = userConfig.isUsingAsCollateral(reserve.id);
    if (isCollateral && balanceWriteOff == userBalance) {
      userConfig.setUsingAsCollateral(reserve.id, params.asset, params.user, false);
    }

    IAToken(reserveCache.aTokenAddress).burn(
      params.user,
      reserveCache.aTokenAddress,
      balanceWriteOff,
      reserveCache.nextLiquidityIndex
    );

    reserve.deficit -= balanceWriteOff.toUint128();

    reserve.updateInterestRatesAndVirtualBalance(
      reserveCache,
      params.asset,
      0,
      0,
      params.interestRateStrategyAddress
    );

    emit IPool.DeficitCovered(params.asset, params.user, balanceWriteOff);
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
    IAToken collateralAToken;
    DataTypes.ReserveCache debtReserveCache;
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
    debtReserve.updateState(vars.debtReserveCache);

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

    vars.collateralAToken = IAToken(collateralReserve.aTokenAddress);
    vars.borrowerCollateralBalance = vars.collateralAToken.balanceOf(params.borrower);
    vars.borrowerReserveDebt = IVariableDebtToken(vars.debtReserveCache.variableDebtTokenAddress)
      .scaledBalanceOf(params.borrower)
      .rayMul(vars.debtReserveCache.nextVariableBorrowIndex);

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
      vars.liquidationBonus = collateralReserve.configuration.getLiquidationBonus();
    }
    vars.collateralAssetPrice = IPriceOracleGetter(params.priceOracle).getAssetPrice(
      params.collateralAsset
    );
    vars.debtAssetPrice = IPriceOracleGetter(params.priceOracle).getAssetPrice(params.debtAsset);
    vars.collateralAssetUnit = 10 ** collateralReserve.configuration.getDecimals();
    vars.debtAssetUnit = 10 ** vars.debtReserveCache.reserveConfiguration.getDecimals();

    vars.borrowerReserveDebtInBaseCurrency =
      (vars.borrowerReserveDebt * vars.debtAssetPrice) /
      vars.debtAssetUnit;

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
      collateralReserve.configuration,
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
      bool isDebtMoreThanLeftoverThreshold = ((vars.borrowerReserveDebt -
        vars.actualDebtToLiquidate) * vars.debtAssetPrice) /
        vars.debtAssetUnit >=
        MIN_LEFTOVER_BASE;

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
    if (collateralReserve.configuration.getDebtCeiling() != 0) {
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
      _burnCollateralATokens(collateralReserve, params, vars);
    }

    // Transfer fee to treasury if it is non-zero
    if (vars.liquidationProtocolFeeAmount != 0) {
      uint256 liquidityIndex = collateralReserve.getNormalizedIncome();
      uint256 scaledDownLiquidationProtocolFee = vars.liquidationProtocolFeeAmount.rayDiv(
        liquidityIndex
      );
      uint256 scaledDownBorrowerBalance = vars.collateralAToken.scaledBalanceOf(params.borrower);
      // To avoid trying to send more aTokens than available on balance, due to 1 wei imprecision
      if (scaledDownLiquidationProtocolFee > scaledDownBorrowerBalance) {
        vars.liquidationProtocolFeeAmount = scaledDownBorrowerBalance.rayMul(liquidityIndex);
      }
      vars.collateralAToken.transferOnLiquidation(
        params.borrower,
        vars.collateralAToken.RESERVE_TREASURY_ADDRESS(),
        vars.liquidationProtocolFeeAmount,
        liquidityIndex
      );
    }

    // burn bad debt if necessary
    // Each additional debt asset already adds around ~75k gas to the liquidation.
    // To keep the liquidation gas under control, 0 usd collateral positions are not touched, as there is no immediate benefit in burning or transferring to treasury.
    if (hasNoCollateralLeft && borrowerConfig.isBorrowingAny()) {
      _burnBadDebt(
        reservesData,
        reservesList,
        borrowerConfig,
        params.borrower,
        params.interestRateStrategyAddress
      );
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
    DataTypes.ReserveCache memory collateralReserveCache = collateralReserve.cache();
    collateralReserve.updateState(collateralReserveCache);
    collateralReserve.updateInterestRatesAndVirtualBalance(
      collateralReserveCache,
      params.collateralAsset,
      0,
      vars.actualCollateralToLiquidate,
      params.interestRateStrategyAddress
    );

    // Burn the equivalent amount of aToken, sending the underlying to the liquidator
    vars.collateralAToken.burn(
      params.borrower,
      params.liquidator,
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
    uint256 liquidatorPreviousATokenBalance = IAToken(vars.collateralAToken).scaledBalanceOf(
      params.liquidator
    );
    vars.collateralAToken.transferOnLiquidation(
      params.borrower,
      params.liquidator,
      vars.actualCollateralToLiquidate,
      collateralReserve.getNormalizedIncome()
    );

    if (liquidatorPreviousATokenBalance == 0) {
      DataTypes.UserConfigurationMap storage liquidatorConfig = usersConfig[params.liquidator];
      if (
        ValidationLogic.validateAutomaticUseAsCollateral(
          params.liquidator,
          reservesData,
          reservesList,
          liquidatorConfig,
          collateralReserve.configuration,
          collateralReserve.aTokenAddress
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
      (noMoreDebt, debtReserveCache.nextScaledVariableDebt) = IVariableDebtToken(
        debtReserveCache.variableDebtTokenAddress
      ).burn(
          borrower,
          hasNoCollateralLeft ? borrowerReserveDebt : actualDebtToLiquidate,
          debtReserveCache.nextVariableBorrowIndex
        );
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
      ((debtAssetPrice * debtToCover * collateralAssetUnit)) /
      (vars.collateralAssetPrice * debtAssetUnit);

    vars.maxCollateralToLiquidate = vars.baseCollateral.percentMul(liquidationBonus);

    if (vars.maxCollateralToLiquidate > borrowerCollateralBalance) {
      vars.collateralAmount = borrowerCollateralBalance;
      vars.debtAmountNeeded = ((vars.collateralAssetPrice * vars.collateralAmount * debtAssetUnit) /
        (debtAssetPrice * collateralAssetUnit)).percentDiv(liquidationBonus);
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
   * @param borrower The user from which the debt will be burned.
   */
  function _burnBadDebt(
    mapping(address => DataTypes.ReserveData) storage reservesData,
    mapping(uint256 => address) storage reservesList,
    DataTypes.UserConfigurationMap storage borrowerConfig,
    address borrower,
    address interestRateStrategyAddress
  ) internal {
    uint256 cachedBorrowerConfig = borrowerConfig.data;
    uint256 i = 0;
    bool isBorrowed = false;
    while (cachedBorrowerConfig != 0) {
      (cachedBorrowerConfig, isBorrowed, ) = UserConfiguration.getNextFlags(cachedBorrowerConfig);
      if (isBorrowed) {
        address reserveAddress = reservesList[i];
        if (reserveAddress != address(0)) {
          DataTypes.ReserveData storage currentReserve = reservesData[reserveAddress];
          DataTypes.ReserveCache memory reserveCache = currentReserve.cache();
          if (reserveCache.reserveConfiguration.getActive()) {
            currentReserve.updateState(reserveCache);

            _burnDebtTokens(
              reserveCache,
              currentReserve,
              borrowerConfig,
              borrower,
              reserveAddress,
              IERC20(reserveCache.variableDebtTokenAddress).balanceOf(borrower),
              0,
              true,
              interestRateStrategyAddress
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
        _usersConfig[onBehalfOf],
        DataTypes.ExecuteRepayParams({
          asset: asset,
          user: _msgSender(),
          interestRateStrategyAddress: RESERVE_INTEREST_RATE_STRATEGY,
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
        _usersConfig[_msgSender()],
        DataTypes.ExecuteRepayParams({
          asset: asset,
          user: _msgSender(),
          interestRateStrategyAddress: RESERVE_INTEREST_RATE_STRATEGY,
          amount: amount,
          interestRateMode: DataTypes.InterestRateMode(interestRateMode),
          onBehalfOf: _msgSender(),
          useATokens: true
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
    uint256 amount,
    uint256 balanceFromBefore,
    uint256 balanceToBefore
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
        amount: amount,
        balanceFromBefore: balanceFromBefore,
        balanceToBefore: balanceToBefore,
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
  function eliminateReserveDeficit(address asset, uint256 amount) external override onlyUmbrella {
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
        ? vars.currentAmount.percentMul(vars.flashloanPremium)
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
    uint256 totalPremium = params.amount.percentMul(params.flashLoanPremium);

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
      .rayDiv(reserveCache.nextLiquidityIndex)
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
pragma solidity ^0.8.10;

import {IERC20} from './IERC20.sol';

interface IERC20Detailed is IERC20 {
  function name() external view returns (string memory);

  function symbol() external view returns (string memory);

  function decimals() external view returns (uint8);
}

// SPDX-License-Identifier: MIT
pragma solidity >=0.6.0;

import {DataTypes} from 'aave-v3-origin/contracts/protocol/libraries/types/DataTypes.sol';
import {Errors} from 'aave-v3-origin/contracts/protocol/libraries/helpers/Errors.sol';
import {ConfiguratorInputTypes} from 'aave-v3-origin/contracts/protocol/libraries/types/ConfiguratorInputTypes.sol';
import {IPoolAddressesProvider} from 'aave-v3-origin/contracts/interfaces/IPoolAddressesProvider.sol';
import {IAToken} from 'aave-v3-origin/contracts/interfaces/IAToken.sol';
import {IPool} from 'aave-v3-origin/contracts/interfaces/IPool.sol';
import {IPoolConfigurator} from 'aave-v3-origin/contracts/interfaces/IPoolConfigurator.sol';
import {IPriceOracleGetter} from 'aave-v3-origin/contracts/interfaces/IPriceOracleGetter.sol';
import {IAaveOracle} from 'aave-v3-origin/contracts/interfaces/IAaveOracle.sol';
import {IACLManager as BasicIACLManager} from 'aave-v3-origin/contracts/interfaces/IACLManager.sol';
import {IPoolDataProvider} from 'aave-v3-origin/contracts/interfaces/IPoolDataProvider.sol';
import {IDefaultInterestRateStrategyV2} from 'aave-v3-origin/contracts/interfaces/IDefaultInterestRateStrategyV2.sol';
import {IReserveInterestRateStrategy} from 'aave-v3-origin/contracts/interfaces/IReserveInterestRateStrategy.sol';
import {IPoolDataProvider as IAaveProtocolDataProvider} from 'aave-v3-origin/contracts/interfaces/IPoolDataProvider.sol';
import {AggregatorInterface} from 'aave-v3-origin/contracts/dependencies/chainlink/AggregatorInterface.sol';
import {ICollector} from 'aave-v3-origin/contracts/treasury/ICollector.sol';

interface IACLManager is BasicIACLManager {
  function hasRole(bytes32 role, address account) external view returns (bool);

  function DEFAULT_ADMIN_ROLE() external pure returns (bytes32);

  function renounceRole(bytes32 role, address account) external;

  function getRoleAdmin(bytes32 role) external view returns (bytes32);

  function grantRole(bytes32 role, address account) external;

  function revokeRole(bytes32 role, address account) external;
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
  uint256 public constant POOL_REVISION = 8;

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

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {DataTypes} from "aave-v3-origin/contracts/protocol/pool/PoolStorage.sol";

library CustomInitialize {
  function _initialize(
    uint256 reservesCount,
    mapping(uint256 => address) storage _reservesList,
    mapping(address => DataTypes.ReserveData) storage _reserves
  ) internal {
    for (uint256 i = 0; i < reservesCount; i++) {
      address currentReserveAddress = _reservesList[i];
      DataTypes.ReserveData storage currentReserve = _reserves[currentReserveAddress];

      // @note The storage slot for `__deprecatedVirtualUnderlyingBalance` was deprecated in v3.4.
      //       Its purpose was effectively moved to `virtualUnderlyingBalance`. This `virtualUnderlyingBalance` slot,
      //       in turn, reuses the storage location previously occupied by the `unbacked` variable
      //       (which existed in v3.3 reserves but was removed in v3.4).
      //       Therefore, this function migrates the value from the old `__deprecatedVirtualUnderlyingBalance` slot
      //       to the new `virtualUnderlyingBalance` slot (and zeroes out the old slot).

      uint128 currentVB = currentReserve.__deprecatedVirtualUnderlyingBalance;
      if (currentVB != 0) {
        currentReserve.virtualUnderlyingBalance = currentVB;
        currentReserve.__deprecatedVirtualUnderlyingBalance = 0;
      }
    }
  }
}
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {ConfiguratorInputTypes} from '../protocol/libraries/types/ConfiguratorInputTypes.sol';
import {IDefaultInterestRateStrategyV2} from './IDefaultInterestRateStrategyV2.sol';

/**
 * @title IPoolConfigurator
 * @author Aave
 * @notice Defines the basic interface for a Pool configurator.
 */
interface IPoolConfigurator {
  /**
   * @dev Emitted when a reserve is initialized.
   * @param asset The address of the underlying asset of the reserve
   * @param aToken The address of the associated aToken contract
   * @param stableDebtToken, DEPRECATED in v3.2.0
   * @param variableDebtToken The address of the associated variable rate debt token
   * @param interestRateStrategyAddress The address of the interest rate strategy for the reserve
   */
  event ReserveInitialized(
    address indexed asset,
    address indexed aToken,
    address stableDebtToken,
    address variableDebtToken,
    address interestRateStrategyAddress
  );

  /**
   * @dev Emitted when borrowing is enabled or disabled on a reserve.
   * @param asset The address of the underlying asset of the reserve
   * @param enabled True if borrowing is enabled, false otherwise
   */
  event ReserveBorrowing(address indexed asset, bool enabled);

  /**
   * @dev Emitted when flashloans are enabled or disabled on a reserve.
   * @param asset The address of the underlying asset of the reserve
   * @param enabled True if flashloans are enabled, false otherwise
   */
  event ReserveFlashLoaning(address indexed asset, bool enabled);

  /**
   * @dev Emitted when the ltv is set for the frozen asset.
   * @param asset The address of the underlying asset of the reserve
   * @param ltv The loan to value of the asset when used as collateral
   */
  event PendingLtvChanged(address indexed asset, uint256 ltv);

  /**
   * @dev Emitted when the collateralization risk parameters for the specified asset are updated.
   * @param asset The address of the underlying asset of the reserve
   * @param ltv The loan to value of the asset when used as collateral
   * @param liquidationThreshold The threshold at which loans using this asset as collateral will be considered undercollateralized
   * @param liquidationBonus The bonus liquidators receive to liquidate this asset
   */
  event CollateralConfigurationChanged(
    address indexed asset,
    uint256 ltv,
    uint256 liquidationThreshold,
    uint256 liquidationBonus
  );

  /**
   * @dev Emitted when a reserve is activated or deactivated
   * @param asset The address of the underlying asset of the reserve
   * @param active True if reserve is active, false otherwise
   */
  event ReserveActive(address indexed asset, bool active);

  /**
   * @dev Emitted when a reserve is frozen or unfrozen
   * @param asset The address of the underlying asset of the reserve
   * @param frozen True if reserve is frozen, false otherwise
   */
  event ReserveFrozen(address indexed asset, bool frozen);

  /**
   * @dev Emitted when a reserve is paused or unpaused
   * @param asset The address of the underlying asset of the reserve
   * @param paused True if reserve is paused, false otherwise
   */
  event ReservePaused(address indexed asset, bool paused);

  /**
   * @dev Emitted when a reserve is dropped.
   * @param asset The address of the underlying asset of the reserve
   */
  event ReserveDropped(address indexed asset);

  /**
   * @dev Emitted when a reserve factor is updated.
   * @param asset The address of the underlying asset of the reserve
   * @param oldReserveFactor The old reserve factor, expressed in bps
   * @param newReserveFactor The new reserve factor, expressed in bps
   */
  event ReserveFactorChanged(
    address indexed asset,
    uint256 oldReserveFactor,
    uint256 newReserveFactor
  );

  /**
   * @dev Emitted when the borrow cap of a reserve is updated.
   * @param asset The address of the underlying asset of the reserve
   * @param oldBorrowCap The old borrow cap
   * @param newBorrowCap The new borrow cap
   */
  event BorrowCapChanged(address indexed asset, uint256 oldBorrowCap, uint256 newBorrowCap);

  /**
   * @dev Emitted when the supply cap of a reserve is updated.
   * @param asset The address of the underlying asset of the reserve
   * @param oldSupplyCap The old supply cap
   * @param newSupplyCap The new supply cap
   */
  event SupplyCapChanged(address indexed asset, uint256 oldSupplyCap, uint256 newSupplyCap);

  /**
   * @dev Emitted when the liquidation protocol fee of a reserve is updated.
   * @param asset The address of the underlying asset of the reserve
   * @param oldFee The old liquidation protocol fee, expressed in bps
   * @param newFee The new liquidation protocol fee, expressed in bps
   */
  event LiquidationProtocolFeeChanged(address indexed asset, uint256 oldFee, uint256 newFee);

  /**
   * @dev Emitted when the liquidation grace period is updated.
   * @param asset The address of the underlying asset of the reserve
   * @param gracePeriodUntil Timestamp until when liquidations will not be allowed post-unpause
   */
  event LiquidationGracePeriodChanged(address indexed asset, uint40 gracePeriodUntil);

  /**
   * @dev Emitted when the liquidation grace period is disabled.
   * @param asset The address of the underlying asset of the reserve
   */
  event LiquidationGracePeriodDisabled(address indexed asset);

  /**
   * @dev Emitted when an collateral configuration of an asset in an eMode is changed.
   * @param asset The address of the underlying asset of the reserve
   * @param categoryId The eMode category
   * @param collateral True if the asset is enabled as collateral in the eMode, false otherwise.
   */
  event AssetCollateralInEModeChanged(address indexed asset, uint8 categoryId, bool collateral);

  /**
   * @dev Emitted when the borrowable configuration of an asset in an eMode changed.
   * @param asset The address of the underlying asset of the reserve
   * @param categoryId The eMode category
   * @param borrowable True if the asset is enabled as borrowable in the eMode, false otherwise.
   */
  event AssetBorrowableInEModeChanged(address indexed asset, uint8 categoryId, bool borrowable);

  /**
   * @dev Emitted when a new eMode category is added or an existing category is altered.
   * @param categoryId The new eMode category id
   * @param ltv The ltv for the asset category in eMode
   * @param liquidationThreshold The liquidationThreshold for the asset category in eMode
   * @param liquidationBonus The liquidationBonus for the asset category in eMode
   * @param oracle DEPRECATED in v3.2.0
   * @param label A human readable identifier for the category
   */
  event EModeCategoryAdded(
    uint8 indexed categoryId,
    uint256 ltv,
    uint256 liquidationThreshold,
    uint256 liquidationBonus,
    address oracle,
    string label
  );

  /**
   * @dev Emitted when a reserve interest strategy contract is updated.
   * @param asset The address of the underlying asset of the reserve
   * @param oldStrategy The address of the old interest strategy contract
   * @param newStrategy The address of the new interest strategy contract
   */
  event ReserveInterestRateStrategyChanged(
    address indexed asset,
    address oldStrategy,
    address newStrategy
  );

  /**
   * @dev Emitted when the data of a reserve interest strategy contract is updated.
   * @param asset The address of the underlying asset of the reserve
   * @param data abi encoded data
   */
  event ReserveInterestRateDataChanged(address indexed asset, address indexed strategy, bytes data);

  /**
   * @dev Emitted when an aToken implementation is upgraded.
   * @param asset The address of the underlying asset of the reserve
   * @param proxy The aToken proxy address
   * @param implementation The new aToken implementation
   */
  event ATokenUpgraded(
    address indexed asset,
    address indexed proxy,
    address indexed implementation
  );

  /**
   * @dev Emitted when the implementation of a variable debt token is upgraded.
   * @param asset The address of the underlying asset of the reserve
   * @param proxy The variable debt token proxy address
   * @param implementation The new aToken implementation
   */
  event VariableDebtTokenUpgraded(
    address indexed asset,
    address indexed proxy,
    address indexed implementation
  );

  /**
   * @dev Emitted when the debt ceiling of an asset is set.
   * @param asset The address of the underlying asset of the reserve
   * @param oldDebtCeiling The old debt ceiling
   * @param newDebtCeiling The new debt ceiling
   */
  event DebtCeilingChanged(address indexed asset, uint256 oldDebtCeiling, uint256 newDebtCeiling);

  /**
   * @dev Emitted when the the siloed borrowing state for an asset is changed.
   * @param asset The address of the underlying asset of the reserve
   * @param oldState The old siloed borrowing state
   * @param newState The new siloed borrowing state
   */
  event SiloedBorrowingChanged(address indexed asset, bool oldState, bool newState);

  /**
   * @dev Emitted when the bridge protocol fee is updated.
   * @param oldBridgeProtocolFee The old protocol fee, expressed in bps
   * @param newBridgeProtocolFee The new protocol fee, expressed in bps
   */
  event BridgeProtocolFeeUpdated(uint256 oldBridgeProtocolFee, uint256 newBridgeProtocolFee);

  /**
   * @dev Emitted when the total premium on flashloans is updated.
   * @param oldFlashloanPremiumTotal The old premium, expressed in bps
   * @param newFlashloanPremiumTotal The new premium, expressed in bps
   */
  event FlashloanPremiumTotalUpdated(
    uint128 oldFlashloanPremiumTotal,
    uint128 newFlashloanPremiumTotal
  );

  /**
   * @dev Emitted when the part of the premium that goes to protocol is updated.
          Deprecated, from the v3.4 version the `flashloanPremiumToProtocol` value
          is always 100%.
   * @param oldFlashloanPremiumToProtocol The old premium, expressed in bps
   * @param newFlashloanPremiumToProtocol The new premium, expressed in bps
   */
  event FlashloanPremiumToProtocolUpdated(
    uint128 oldFlashloanPremiumToProtocol,
    uint128 newFlashloanPremiumToProtocol
  );

  /**
   * @dev Emitted when the reserve is set as borrowable/non borrowable in isolation mode.
   * @param asset The address of the underlying asset of the reserve
   * @param borrowable True if the reserve is borrowable in isolation, false otherwise
   */
  event BorrowableInIsolationChanged(address asset, bool borrowable);

  /**
   * @notice Initializes multiple reserves.
   * @param input The array of initialization parameters
   */
  function initReserves(ConfiguratorInputTypes.InitReserveInput[] calldata input) external;

  /**
   * @dev Updates the aToken implementation for the reserve.
   * @param input The aToken update parameters
   */
  function updateAToken(ConfiguratorInputTypes.UpdateATokenInput calldata input) external;

  /**
   * @notice Updates the variable debt token implementation for the asset.
   * @param input The variableDebtToken update parameters
   */
  function updateVariableDebtToken(
    ConfiguratorInputTypes.UpdateDebtTokenInput calldata input
  ) external;

  /**
   * @notice Configures borrowing on a reserve.
   * @param asset The address of the underlying asset of the reserve
   * @param enabled True if borrowing needs to be enabled, false otherwise
   */
  function setReserveBorrowing(address asset, bool enabled) external;

  /**
   * @notice Configures the reserve collateralization parameters.
   * @dev All the values are expressed in bps. A value of 10000, results in 100.00%
   * @dev The `liquidationBonus` is always above 100%. A value of 105% means the liquidator will receive a 5% bonus
   * @param asset The address of the underlying asset of the reserve
   * @param ltv The loan to value of the asset when used as collateral
   * @param liquidationThreshold The threshold at which loans using this asset as collateral will be considered undercollateralized
   * @param liquidationBonus The bonus liquidators receive to liquidate this asset
   */
  function configureReserveAsCollateral(
    address asset,
    uint256 ltv,
    uint256 liquidationThreshold,
    uint256 liquidationBonus
  ) external;

  /**
   * @notice Enable or disable flashloans on a reserve
   * @param asset The address of the underlying asset of the reserve
   * @param enabled True if flashloans need to be enabled, false otherwise
   */
  function setReserveFlashLoaning(address asset, bool enabled) external;

  /**
   * @notice Activate or deactivate a reserve
   * @param asset The address of the underlying asset of the reserve
   * @param active True if the reserve needs to be active, false otherwise
   */
  function setReserveActive(address asset, bool active) external;

  /**
   * @notice Freeze or unfreeze a reserve. A frozen reserve doesn't allow any new supply, borrow
   * or rate swap but allows repayments, liquidations, rate rebalances and withdrawals.
   * @param asset The address of the underlying asset of the reserve
   * @param freeze True if the reserve needs to be frozen, false otherwise
   */
  function setReserveFreeze(address asset, bool freeze) external;

  /**
   * @notice Sets the borrowable in isolation flag for the reserve.
   * @dev When this flag is set to true, the asset will be borrowable against isolated collaterals and the
   * borrowed amount will be accumulated in the isolated collateral's total debt exposure
   * @dev Only assets of the same family (e.g. USD stablecoins) should be borrowable in isolation mode to keep
   * consistency in the debt ceiling calculations
   * @param asset The address of the underlying asset of the reserve
   * @param borrowable True if the asset should be borrowable in isolation, false otherwise
   */
  function setBorrowableInIsolation(address asset, bool borrowable) external;

  /**
   * @notice Pauses a reserve. A paused reserve does not allow any interaction (supply, borrow, repay,
   * swap interest rate, liquidate, atoken transfers).
   * @param asset The address of the underlying asset of the reserve
   * @param paused True if pausing the reserve, false if unpausing
   * @param gracePeriod Count of seconds after unpause during which liquidations will not be available
   *   - Only applicable whenever unpausing (`paused` as false)
   *   - Passing 0 means no grace period
   *   - Capped to maximum MAX_GRACE_PERIOD
   */
  function setReservePause(address asset, bool paused, uint40 gracePeriod) external;

  /**
   * @notice Pauses a reserve. A paused reserve does not allow any interaction (supply, borrow, repay,
   * swap interest rate, liquidate, atoken transfers).
   * @dev Version with no grace period
   * @param asset The address of the underlying asset of the reserve
   * @param paused True if pausing the reserve, false if unpausing
   */
  function setReservePause(address asset, bool paused) external;

  /**
   * @notice Disables liquidation grace period for the asset. The liquidation grace period is set in the past
   * so that liquidations are allowed for the asset.
   * @param asset The address of the underlying asset of the reserve
   */
  function disableLiquidationGracePeriod(address asset) external;

  /**
   * @notice Updates the reserve factor of a reserve.
   * @param asset The address of the underlying asset of the reserve
   * @param newReserveFactor The new reserve factor of the reserve
   */
  function setReserveFactor(address asset, uint256 newReserveFactor) external;

  /**
   * @notice Sets interest rate data for a reserve
   * @param asset The address of the underlying asset of the reserve
   * @param rateData bytes-encoded rate data. In this format in order to allow the rate strategy contract
   *  to de-structure custom data
   */
  function setReserveInterestRateData(address asset, bytes calldata rateData) external;

  /**
   * @notice Pauses or unpauses all the protocol reserves. In the paused state all the protocol interactions
   * are suspended.
   * @param paused True if protocol needs to be paused, false otherwise
   * @param gracePeriod Count of seconds after unpause during which liquidations will not be available
   *   - Only applicable whenever unpausing (`paused` as false)
   *   - Passing 0 means no grace period
   *   - Capped to maximum MAX_GRACE_PERIOD
   */
  function setPoolPause(bool paused, uint40 gracePeriod) external;

  /**
   * @notice Pauses or unpauses all the protocol reserves. In the paused state all the protocol interactions
   * are suspended.
   * @dev Version with no grace period
   * @param paused True if protocol needs to be paused, false otherwise
   */
  function setPoolPause(bool paused) external;

  /**
   * @notice Updates the borrow cap of a reserve.
   * @param asset The address of the underlying asset of the reserve
   * @param newBorrowCap The new borrow cap of the reserve
   */
  function setBorrowCap(address asset, uint256 newBorrowCap) external;

  /**
   * @notice Updates the supply cap of a reserve.
   * @param asset The address of the underlying asset of the reserve
   * @param newSupplyCap The new supply cap of the reserve
   */
  function setSupplyCap(address asset, uint256 newSupplyCap) external;

  /**
   * @notice Updates the liquidation protocol fee of reserve.
   * @param asset The address of the underlying asset of the reserve
   * @param newFee The new liquidation protocol fee of the reserve, expressed in bps
   */
  function setLiquidationProtocolFee(address asset, uint256 newFee) external;

  /**
   * @notice Enables/disables an asset to be borrowable in a selected eMode.
   * - eMode.borrowable always has less priority then reserve.borrowable
   * @param asset The address of the underlying asset of the reserve
   * @param categoryId The eMode categoryId
   * @param borrowable True if the asset should be borrowable in the given eMode category, false otherwise.
   */
  function setAssetBorrowableInEMode(address asset, uint8 categoryId, bool borrowable) external;

  /**
   * @notice Enables/disables an asset to be collateral in a selected eMode.
   * @param asset The address of the underlying asset of the reserve
   * @param categoryId The eMode categoryId
   * @param collateral True if the asset should be collateral in the given eMode category, false otherwise.
   */
  function setAssetCollateralInEMode(address asset, uint8 categoryId, bool collateral) external;

  /**
   * @notice Adds a new efficiency mode (eMode) category or alters a existing one.
   * @param categoryId The id of the category to be configured
   * @param ltv The ltv associated with the category
   * @param liquidationThreshold The liquidation threshold associated with the category
   * @param liquidationBonus The liquidation bonus associated with the category
   * @param label A label identifying the category
   */
  function setEModeCategory(
    uint8 categoryId,
    uint16 ltv,
    uint16 liquidationThreshold,
    uint16 liquidationBonus,
    string calldata label
  ) external;

  /**
   * @notice Drops a reserve entirely.
   * @param asset The address of the reserve to drop
   */
  function dropReserve(address asset) external;

  /**
   * @notice Updates the flash loan premium. All this premium
   *         will be collected by the treasury.
   * @dev Expressed in bps
   * @dev The premium is calculated on the total amount borrowed
   * @param newFlashloanPremium The flashloan premium
   */
  function updateFlashloanPremium(uint128 newFlashloanPremium) external;

  /**
   * @notice Sets the debt ceiling for an asset.
   * @param newDebtCeiling The new debt ceiling
   */
  function setDebtCeiling(address asset, uint256 newDebtCeiling) external;

  /**
   * @notice Sets siloed borrowing for an asset
   * @param siloed The new siloed borrowing state
   */
  function setSiloedBorrowing(address asset, bool siloed) external;

  /**
   * @notice Gets pending ltv value
   * @param asset The new siloed borrowing state
   */
  function getPendingLtv(address asset) external view returns (uint256);

  /**
   * @notice Gets the address of the external ConfiguratorLogic
   */
  function getConfiguratorLogic() external view returns (address);

  /**
   * @notice Gets the maximum liquidations grace period allowed, in seconds
   */
  function MAX_GRACE_PERIOD() external view returns (uint40);
}

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {IERC20} from "openzeppelin-contracts/contracts/token/ERC20/IERC20.sol";
import {IScaledBalanceToken} from "aave-v3-origin/contracts/interfaces/IScaledBalanceToken.sol";
import {PoolInstance} from "aave-v3-origin/contracts/instances/PoolInstance.sol";
import {Errors} from "aave-v3-origin/contracts/protocol/libraries/helpers/Errors.sol";
import {IPoolAddressesProvider} from "aave-v3-origin/contracts/interfaces/IPoolAddressesProvider.sol";
import {IReserveInterestRateStrategy} from "aave-v3-origin/contracts/interfaces/IReserveInterestRateStrategy.sol";
import {DataTypes} from "aave-v3-origin/contracts/protocol/pool/Pool.sol";
import {ReserveConfiguration} from "aave-v3-origin/contracts/protocol/libraries/configuration/ReserveConfiguration.sol";

import {AaveV3EthereumAssets, AaveV3Ethereum} from "aave-address-book/AaveV3Ethereum.sol";

import {CustomInitialize} from "./CustomInitialize.sol";

contract MainnetCorePoolInstanceWithCustomInitialize is PoolInstance {
  using ReserveConfiguration for DataTypes.ReserveConfigurationMap;

  constructor(IPoolAddressesProvider provider, IReserveInterestRateStrategy interestRateStrategy_)
    PoolInstance(provider, interestRateStrategy_)
  {}

  /// @inheritdoc PoolInstance
  function initialize(IPoolAddressesProvider provider) external virtual override initializer {
    require(provider == ADDRESSES_PROVIDER, Errors.InvalidAddressesProvider());

    CustomInitialize._initialize(_reservesCount, _reservesList, _reserves);

    // 1. Explicitly activate the virtual account feature in the GHO reserve's configuration.
    //    Although virtual accounting is standard for all reserves in v3.4,
    //    this call ensures the specific configuration bit for GHO is set to true.
    DataTypes.ReserveData storage ghoReserveData = _reserves[AaveV3EthereumAssets.GHO_UNDERLYING];
    DataTypes.ReserveConfigurationMap memory ghoConfig = ghoReserveData.configuration;

    ghoConfig.setVirtualAccActive();

    ghoReserveData.configuration = ghoConfig;

    // 2. Initialize `accruedToTreasury` for the GHO reserve.
    //    Due to GHO's reserve factor being set to 100%, this `accruedToTreasury` variable must
    //    capture the entirety of historical GHO interest that has accrued on currently active
    //    (outstanding) loans and is payable to the treasury.
    //
    //    Understanding the state and component values for this calculation:
    //    - `vTokenTotalSupply` (GHO): This represents the total outstanding GHO variable debt. It is the sum of all
    //      currently active (outstanding) GHO principal amounts borrowed by users, plus all interest that has accrued
    //      on this outstanding principal up to this moment.
    //
    //    - `ghoPrincipalComponent`:
    //      This value represents the total *outstanding* GHO principal that was originally minted and backed by the old GHO AToken
    //      facilitator mechanism and is currently borrowed by users. Here's its derivation:
    //        a) In `UpgradePayloadMainnet` (step 2), `levelFromOldFacilitator` was fetched. This `level` was the net GHO
    //           minted by the old GHO AToken facilitator that is still outstanding (i.e., not yet repaid by users and subsequently burned by the facilitator mechanism),
    //           effectively representing the total *currently outstanding* GHO principal borrowed by users under that original facilitation mechanism.
    //        b) In `UpgradePayloadMainnet` (step 6), the new `FACILITATOR` (GhoDirectMinter) minted GHO equal to this
    //           `levelFromOldFacilitator` and supplied it to the Pool, receiving an equivalent amount of GHO ATokens (`aGHO`).
    //        c) These `aGHO` tokens are now held by the `FACILITATOR`, and their `scaledTotalSupply` (assuming GHO liquidity
    //           index is 1 RAY) equals `levelFromOldFacilitator`.
    //      Thus, `ghoPrincipalComponent` accurately reflects the total principal portion of the *currently outstanding* GHO debt.
    //
    uint256 vTokenTotalSupply = IERC20(ghoReserveData.variableDebtTokenAddress).totalSupply();
    uint256 ghoPrincipalComponent = IScaledBalanceToken(AaveV3EthereumAssets.GHO_A_TOKEN).scaledTotalSupply();

    // Calculation for `accruedToTreasury`:
    //   Total Outstanding GHO Debt (vTokenTotalSupply) = (Total Outstanding GHO Principal Borrowed by Users) + (Total Accrued GHO Interest on Outstanding Principal)
    //   GHO Principal Component (ghoPrincipalComponent) = (Total Outstanding GHO Principal Borrowed by Users)
    //
    //   Therefore: `accruedToTreasury` = `vTokenTotalSupply` - `ghoPrincipalComponent`
    //                                 = (Total Accrued GHO Interest on Outstanding Principal).
    //
    // This calculation assumes GHO's liquidity index is effectively 1 RAY (1e27). If the index is 1 RAY,
    // `scaledTotalSupply()` of GHO AToken directly represents the actual GHO token amount for the principal.
    // The 100% reserve factor for GHO helps maintain this stable index.
    ghoReserveData.accruedToTreasury = uint128(vTokenTotalSupply - ghoPrincipalComponent);
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
import {WadRayMath} from '../math/WadRayMath.sol';
import {PercentageMath} from '../math/PercentageMath.sol';
import {DataTypes} from '../types/DataTypes.sol';
import {ReserveLogic} from './ReserveLogic.sol';
import {GenericLogic} from './GenericLogic.sol';
import {SafeCast} from 'openzeppelin-contracts/contracts/utils/math/SafeCast.sol';
import {IncentivizedERC20} from '../../tokenization/base/IncentivizedERC20.sol';

/**
 * @title ValidationLogic library
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
    uint256 amount,
    address onBehalfOf
  ) internal view {
    require(amount != 0, Errors.InvalidAmount());

    (bool isActive, bool isFrozen, , bool isPaused) = reserveCache.reserveConfiguration.getFlags();
    require(isActive, Errors.ReserveInactive());
    require(!isPaused, Errors.ReservePaused());
    require(!isFrozen, Errors.ReserveFrozen());
    require(onBehalfOf != reserveCache.aTokenAddress, Errors.SupplyToAToken());

    uint256 supplyCap = reserveCache.reserveConfiguration.getSupplyCap();
    require(
      supplyCap == 0 ||
        ((IAToken(reserveCache.aTokenAddress).scaledTotalSupply() +
          uint256(reserve.accruedToTreasury)).rayMul(reserveCache.nextLiquidityIndex) + amount) <=
        supplyCap * (10 ** reserveCache.reserveConfiguration.getDecimals()),
      Errors.SupplyCapExceeded()
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
    require(amount != 0, Errors.InvalidAmount());
    require(amount <= userBalance, Errors.NotEnoughAvailableUserBalance());

    (bool isActive, , , bool isPaused) = reserveCache.reserveConfiguration.getFlags();
    require(isActive, Errors.ReserveInactive());
    require(!isPaused, Errors.ReservePaused());
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
    require(params.amount != 0, Errors.InvalidAmount());

    ValidateBorrowLocalVars memory vars;

    (vars.isActive, vars.isFrozen, vars.borrowingEnabled, vars.isPaused) = params
      .reserveCache
      .reserveConfiguration
      .getFlags();

    require(vars.isActive, Errors.ReserveInactive());
    require(!vars.isPaused, Errors.ReservePaused());
    require(!vars.isFrozen, Errors.ReserveFrozen());
    require(vars.borrowingEnabled, Errors.BorrowingNotEnabled());
    require(
      IERC20(params.reserveCache.aTokenAddress).totalSupply() >= params.amount,
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
      vars.totalSupplyVariableDebt = params.reserveCache.currScaledVariableDebt.rayMul(
        params.reserveCache.nextVariableBorrowIndex
      );

      vars.totalDebt = vars.totalSupplyVariableDebt + params.amount;

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
        user: params.userAddress,
        oracle: params.oracle,
        userEModeCategory: params.userEModeCategory
      })
    );

    require(vars.userCollateralInBaseCurrency != 0, Errors.CollateralBalanceIsZero());
    require(vars.currentLtv != 0, Errors.LtvValidationFailed());

    require(
      vars.healthFactor > HEALTH_FACTOR_LIQUIDATION_THRESHOLD,
      Errors.HealthFactorLowerThanLiquidationThreshold()
    );

    vars.amountInBaseCurrency =
      IPriceOracleGetter(params.oracle).getAssetPrice(params.asset) *
      params.amount;
    unchecked {
      vars.amountInBaseCurrency /= vars.assetUnit;
    }

    //add the current already borrowed amount to the amount requested to calculate the total collateral needed.
    vars.collateralNeededInBaseCurrency = (vars.userDebtInBaseCurrency + vars.amountInBaseCurrency)
      .percentDiv(vars.currentLtv); //LTV is calculated in percentage

    require(
      vars.collateralNeededInBaseCurrency <= vars.userCollateralInBaseCurrency,
      Errors.CollateralCannotCoverNewBorrow()
    );

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
   * @param debt The borrow balance of the user
   */
  function validateRepay(
    address user,
    DataTypes.ReserveCache memory reserveCache,
    uint256 amountSent,
    DataTypes.InterestRateMode interestRateMode,
    address onBehalfOf,
    uint256 debt
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

    require(debt != 0, Errors.NoDebtOfSelectedType());
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
   * @notice Validates the health factor of a user and the ltv of the asset being withdrawn.
   * @param reservesData The state of all the reserves
   * @param reservesList The addresses of all the active reserves
   * @param eModeCategories The configuration of all the efficiency mode categories
   * @param userConfig The state of the user for the specific reserve
   * @param asset The asset for which the ltv will be validated
   * @param from The user from which the aTokens are being transferred
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
   */
  function eliminateReserveDeficit(address asset, uint256 amount) external;

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

import {IPoolAddressesProvider} from './IPoolAddressesProvider.sol';
import {IPool} from './IPool.sol';

/**
 * @title IPoolDataProvider
 * @author Aave
 * @notice Defines the basic interface of a PoolDataProvider
 */
interface IPoolDataProvider {
  struct TokenData {
    string symbol;
    address tokenAddress;
  }

  /**
   * @notice Returns the address for the PoolAddressesProvider contract.
   * @return The address for the PoolAddressesProvider contract
   */
  function ADDRESSES_PROVIDER() external view returns (IPoolAddressesProvider);

  /**
   * @notice Returns the address for the Pool contract.
   * @return The address for the Pool contract
   */
  function POOL() external view returns (IPool);

  /**
   * @notice Returns the list of the existing reserves in the pool.
   * @dev Handling MKR and ETH in a different way since they do not have standard `symbol` functions.
   * @return The list of reserves, pairs of symbols and addresses
   */
  function getAllReservesTokens() external view returns (TokenData[] memory);

  /**
   * @notice Returns the list of the existing ATokens in the pool.
   * @return The list of ATokens, pairs of symbols and addresses
   */
  function getAllATokens() external view returns (TokenData[] memory);

  /**
   * @notice Returns the configuration data of the reserve
   * @dev Not returning borrow and supply caps for compatibility, nor pause flag
   * @param asset The address of the underlying asset of the reserve
   * @return decimals The number of decimals of the reserve
   * @return ltv The ltv of the reserve
   * @return liquidationThreshold The liquidationThreshold of the reserve
   * @return liquidationBonus The liquidationBonus of the reserve
   * @return reserveFactor The reserveFactor of the reserve
   * @return usageAsCollateralEnabled True if the usage as collateral is enabled, false otherwise
   * @return borrowingEnabled True if borrowing is enabled, false otherwise
   * @return stableBorrowRateEnabled True if stable rate borrowing is enabled, false otherwise
   * @return isActive True if it is active, false otherwise
   * @return isFrozen True if it is frozen, false otherwise
   */
  function getReserveConfigurationData(
    address asset
  )
    external
    view
    returns (
      uint256 decimals,
      uint256 ltv,
      uint256 liquidationThreshold,
      uint256 liquidationBonus,
      uint256 reserveFactor,
      bool usageAsCollateralEnabled,
      bool borrowingEnabled,
      bool stableBorrowRateEnabled,
      bool isActive,
      bool isFrozen
    );

  /**
   * @notice Returns the caps parameters of the reserve
   * @param asset The address of the underlying asset of the reserve
   * @return borrowCap The borrow cap of the reserve
   * @return supplyCap The supply cap of the reserve
   */
  function getReserveCaps(
    address asset
  ) external view returns (uint256 borrowCap, uint256 supplyCap);

  /**
   * @notice Returns if the pool is paused
   * @param asset The address of the underlying asset of the reserve
   * @return isPaused True if the pool is paused, false otherwise
   */
  function getPaused(address asset) external view returns (bool isPaused);

  /**
   * @notice Returns the siloed borrowing flag
   * @param asset The address of the underlying asset of the reserve
   * @return True if the asset is siloed for borrowing
   */
  function getSiloedBorrowing(address asset) external view returns (bool);

  /**
   * @notice Returns the protocol fee on the liquidation bonus
   * @param asset The address of the underlying asset of the reserve
   * @return The protocol fee on liquidation
   */
  function getLiquidationProtocolFee(address asset) external view returns (uint256);

  /**
   * @notice Returns the unbacked mint cap of the reserve
   * @param asset The address of the underlying asset of the reserve
   * @return 0, DEPRECATED in v3.4.0
   */
  function getUnbackedMintCap(address asset) external view returns (uint256);

  /**
   * @notice Returns the debt ceiling of the reserve
   * @param asset The address of the underlying asset of the reserve
   * @return The debt ceiling of the reserve
   */
  function getDebtCeiling(address asset) external view returns (uint256);

  /**
   * @notice Returns the debt ceiling decimals
   * @return The debt ceiling decimals
   */
  function getDebtCeilingDecimals() external pure returns (uint256);

  /**
   * @notice Returns the reserve data
   * @param asset The address of the underlying asset of the reserve
   * @return unbacked The amount of unbacked tokens
   * @return accruedToTreasuryScaled The scaled amount of tokens accrued to treasury that is to be minted
   * @return totalAToken The total supply of the aToken
   * @return totalStableDebt The total stable debt of the reserve
   * @return totalVariableDebt The total variable debt of the reserve
   * @return liquidityRate The liquidity rate of the reserve
   * @return variableBorrowRate The variable borrow rate of the reserve
   * @return stableBorrowRate The stable borrow rate of the reserve
   * @return averageStableBorrowRate The average stable borrow rate of the reserve
   * @return liquidityIndex The liquidity index of the reserve
   * @return variableBorrowIndex The variable borrow index of the reserve
   * @return lastUpdateTimestamp The timestamp of the last update of the reserve
   */
  function getReserveData(
    address asset
  )
    external
    view
    returns (
      uint256 unbacked,
      uint256 accruedToTreasuryScaled,
      uint256 totalAToken,
      uint256 totalStableDebt,
      uint256 totalVariableDebt,
      uint256 liquidityRate,
      uint256 variableBorrowRate,
      uint256 stableBorrowRate,
      uint256 averageStableBorrowRate,
      uint256 liquidityIndex,
      uint256 variableBorrowIndex,
      uint40 lastUpdateTimestamp
    );

  /**
   * @notice Returns the total supply of aTokens for a given asset
   * @param asset The address of the underlying asset of the reserve
   * @return The total supply of the aToken
   */
  function getATokenTotalSupply(address asset) external view returns (uint256);

  /**
   * @notice Returns the total debt for a given asset
   * @param asset The address of the underlying asset of the reserve
   * @return The total debt for asset
   */
  function getTotalDebt(address asset) external view returns (uint256);

  /**
   * @notice Returns the user data in a reserve
   * @param asset The address of the underlying asset of the reserve
   * @param user The address of the user
   * @return currentATokenBalance The current AToken balance of the user
   * @return currentStableDebt The current stable debt of the user
   * @return currentVariableDebt The current variable debt of the user
   * @return principalStableDebt The principal stable debt of the user
   * @return scaledVariableDebt The scaled variable debt of the user
   * @return stableBorrowRate The stable borrow rate of the user
   * @return liquidityRate The liquidity rate of the reserve
   * @return stableRateLastUpdated The timestamp of the last update of the user stable rate
   * @return usageAsCollateralEnabled True if the user is using the asset as collateral, false
   *         otherwise
   */
  function getUserReserveData(
    address asset,
    address user
  )
    external
    view
    returns (
      uint256 currentATokenBalance,
      uint256 currentStableDebt,
      uint256 currentVariableDebt,
      uint256 principalStableDebt,
      uint256 scaledVariableDebt,
      uint256 stableBorrowRate,
      uint256 liquidityRate,
      uint40 stableRateLastUpdated,
      bool usageAsCollateralEnabled
    );

  /**
   * @notice Returns the token addresses of the reserve
   * @param asset The address of the underlying asset of the reserve
   * @return aTokenAddress The AToken address of the reserve
   * @return stableDebtTokenAddress DEPRECATED in v3.2.0
   * @return variableDebtTokenAddress The VariableDebtToken address of the reserve
   */
  function getReserveTokensAddresses(
    address asset
  )
    external
    view
    returns (
      address aTokenAddress,
      address stableDebtTokenAddress,
      address variableDebtTokenAddress
    );

  /**
   * @notice Returns the address of the Interest Rate strategy
   * @param asset The address of the underlying asset of the reserve
   * @return irStrategyAddress The address of the Interest Rate strategy
   */
  function getInterestRateStrategyAddress(
    address asset
  ) external view returns (address irStrategyAddress);

  /**
   * @notice Returns whether the reserve has FlashLoans enabled or disabled
   * @param asset The address of the underlying asset of the reserve
   * @return True if FlashLoans are enabled, false otherwise
   */
  function getFlashLoanEnabled(address asset) external view returns (bool);

  /**
   * @notice Returns whether virtual accounting is enabled/not for a reserve
   * @param asset The address of the underlying asset of the reserve
   * @return True, DEPRECATED in v3.4.0 as all reserves have virtual accounting set as active
   */
  function getIsVirtualAccActive(address asset) external view returns (bool);

  /**
   * @notice Returns the virtual underlying balance of the reserve
   * @param asset The address of the underlying asset of the reserve
   * @return The reserve virtual underlying balance
   */
  function getVirtualUnderlyingBalance(address asset) external view returns (uint256);

  /**
   * @notice Returns the deficit of the reserve
   * @param asset The address of the underlying asset of the reserve
   * @return The reserve deficit
   */
  function getReserveDeficit(address asset) external view returns (uint256);
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
import {DataTypes} from '../types/DataTypes.sol';
import {SafeCast} from 'openzeppelin-contracts/contracts/utils/math/SafeCast.sol';

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
    uint256 totalVariableDebt = reserveCache.nextScaledVariableDebt.rayMul(
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

    //calculate the total variable debt at moment of the last interaction
    uint256 prevTotalVariableDebt = reserveCache.currScaledVariableDebt.rayMul(
      reserveCache.currVariableBorrowIndex
    );

    //calculate the new total variable debt after accumulation of the interest on the index
    uint256 currTotalVariableDebt = reserveCache.currScaledVariableDebt.rayMul(
      reserveCache.nextVariableBorrowIndex
    );

    //debt accrued is the sum of the current debt minus the sum of the debt at the last update
    uint256 totalDebtAccrued = currTotalVariableDebt - prevTotalVariableDebt;

    uint256 amountToMint = totalDebtAccrued.percentMul(reserveCache.reserveFactor);

    if (amountToMint != 0) {
      reserve.accruedToTreasury += amountToMint.rayDiv(reserveCache.nextLiquidityIndex).toUint128();
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

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {IPool} from '../../../interfaces/IPool.sol';
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

    ValidationLogic.validateSupply(reserveCache, reserve, params.amount, params.onBehalfOf);

    reserve.updateInterestRatesAndVirtualBalance(
      reserveCache,
      params.asset,
      params.amount,
      0,
      params.interestRateStrategyAddress
    );

    IERC20(params.asset).safeTransferFrom(params.user, reserveCache.aTokenAddress, params.amount);

    bool isFirstSupply = IAToken(reserveCache.aTokenAddress).mint(
      params.user,
      params.onBehalfOf,
      params.amount,
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

    uint256 userBalance = IAToken(reserveCache.aTokenAddress).scaledBalanceOf(params.user).rayMul(
      reserveCache.nextLiquidityIndex
    );

    uint256 amountToWithdraw = params.amount;

    if (params.amount == type(uint256).max) {
      amountToWithdraw = userBalance;
    }

    ValidationLogic.validateWithdraw(reserveCache, amountToWithdraw, userBalance);

    reserve.updateInterestRatesAndVirtualBalance(
      reserveCache,
      params.asset,
      0,
      amountToWithdraw,
      params.interestRateStrategyAddress
    );

    bool isCollateral = userConfig.isUsingAsCollateral(reserve.id);

    if (isCollateral && amountToWithdraw == userBalance) {
      userConfig.setUsingAsCollateral(reserve.id, params.asset, params.user, false);
    }

    IAToken(reserveCache.aTokenAddress).burn(
      params.user,
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
        params.user,
        params.oracle,
        params.userEModeCategory
      );
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
    uint256 scaledAmount = params.amount.rayDiv(reserve.getNormalizedIncome());

    if (params.from != params.to && scaledAmount != 0) {
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
            params.oracle,
            params.fromEModeCategory
          );
        }
        if (params.balanceFromBefore == params.amount) {
          fromConfig.setUsingAsCollateral(reserveId, params.asset, params.from, false);
        }
      }

      if (params.balanceToBefore == 0) {
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
      ValidationLogic.validateHFAndLtv(
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

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {SafeCast} from 'openzeppelin-contracts/contracts/utils/math/SafeCast.sol';
import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {IVariableDebtToken} from '../../../interfaces/IVariableDebtToken.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {IPool} from '../../../interfaces/IPool.sol';
import {WadRayMath} from '../../libraries/math/WadRayMath.sol';
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
  using WadRayMath for uint256;
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
        oracle: params.oracle,
        userEModeCategory: params.userEModeCategory,
        priceOracleSentinel: params.priceOracleSentinel
      })
    );

    reserveCache.nextScaledVariableDebt = IVariableDebtToken(reserveCache.variableDebtTokenAddress)
      .mint(params.user, params.onBehalfOf, params.amount, reserveCache.nextVariableBorrowIndex);

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
    DataTypes.UserConfigurationMap storage onBehalfOfConfig,
    DataTypes.ExecuteRepayParams memory params
  ) external returns (uint256) {
    DataTypes.ReserveData storage reserve = reservesData[params.asset];
    DataTypes.ReserveCache memory reserveCache = reserve.cache();
    reserve.updateState(reserveCache);

    uint256 userDebt = IVariableDebtToken(reserveCache.variableDebtTokenAddress)
      .scaledBalanceOf(params.onBehalfOf)
      .rayMul(reserveCache.nextVariableBorrowIndex);

    ValidationLogic.validateRepay(
      params.user,
      reserveCache,
      params.amount,
      params.interestRateMode,
      params.onBehalfOf,
      userDebt
    );

    uint256 paybackAmount = params.amount;

    // Allows a user to repay with aTokens without leaving dust from interest.
    if (params.useATokens && paybackAmount == type(uint256).max) {
      paybackAmount = IAToken(reserveCache.aTokenAddress).balanceOf(params.user);
    }

    if (paybackAmount > userDebt) {
      paybackAmount = userDebt;
    }

    bool noMoreDebt;
    (noMoreDebt, reserveCache.nextScaledVariableDebt) = IVariableDebtToken(
      reserveCache.variableDebtTokenAddress
    ).burn(params.onBehalfOf, paybackAmount, reserveCache.nextVariableBorrowIndex);

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
      IAToken(reserveCache.aTokenAddress).burn(
        params.user,
        reserveCache.aTokenAddress,
        paybackAmount,
        reserveCache.nextLiquidityIndex
      );
      bool isCollateral = onBehalfOfConfig.isUsingAsCollateral(reserve.id);
      if (isCollateral && IAToken(reserveCache.aTokenAddress).scaledBalanceOf(params.user) == 0) {
        onBehalfOfConfig.setUsingAsCollateral(reserve.id, params.asset, params.user, false);
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

// AUTOGENERATED - MANUALLY CHANGES WILL BE REVERTED BY THE GENERATOR
// SPDX-License-Identifier: MIT
pragma solidity >=0.6.0;

import {IPoolAddressesProvider, IPool, IPoolConfigurator, IAaveOracle, IPoolDataProvider, IACLManager, ICollector} from './AaveV3.sol';
library AaveV3Ethereum {
  // https://etherscan.io/address/0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e
  IPoolAddressesProvider internal constant POOL_ADDRESSES_PROVIDER =
    IPoolAddressesProvider(0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e);

  // https://etherscan.io/address/0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2
  IPool internal constant POOL = IPool(0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2);

  // https://etherscan.io/address/0x64b761D848206f447Fe2dd461b0c635Ec39EbB27
  IPoolConfigurator internal constant POOL_CONFIGURATOR =
    IPoolConfigurator(0x64b761D848206f447Fe2dd461b0c635Ec39EbB27);

  // https://etherscan.io/address/0x54586bE62E3c3580375aE3723C145253060Ca0C2
  IAaveOracle internal constant ORACLE = IAaveOracle(0x54586bE62E3c3580375aE3723C145253060Ca0C2);

  // https://etherscan.io/address/0x5300A1a15135EA4dc7aD5a167152C01EFc9b192A
  address internal constant ACL_ADMIN = 0x5300A1a15135EA4dc7aD5a167152C01EFc9b192A;

  // https://etherscan.io/address/0xc2aaCf6553D20d1e9d78E365AAba8032af9c85b0
  IACLManager internal constant ACL_MANAGER =
    IACLManager(0xc2aaCf6553D20d1e9d78E365AAba8032af9c85b0);

  // https://etherscan.io/address/0x497a1994c46d4f6C864904A9f1fac6328Cb7C8a6
  IPoolDataProvider internal constant AAVE_PROTOCOL_DATA_PROVIDER =
    IPoolDataProvider(0x497a1994c46d4f6C864904A9f1fac6328Cb7C8a6);

  // https://etherscan.io/address/0x9aEb8aAA1cA38634Aa8C0c8933E7fB4D61091327
  address internal constant POOL_IMPL = 0x9aEb8aAA1cA38634Aa8C0c8933E7fB4D61091327;

  // https://etherscan.io/address/0xE5e48Ad1F9D1A894188b483DcF91f4FaD6AbA43b
  address internal constant POOL_CONFIGURATOR_IMPL = 0xE5e48Ad1F9D1A894188b483DcF91f4FaD6AbA43b;

  // https://etherscan.io/address/0x8164Cc65827dcFe994AB23944CBC90e0aa80bFcb
  address internal constant DEFAULT_INCENTIVES_CONTROLLER =
    0x8164Cc65827dcFe994AB23944CBC90e0aa80bFcb;

  // https://etherscan.io/address/0x223d844fc4B006D67c0cDbd39371A9F73f69d974
  address internal constant EMISSION_MANAGER = 0x223d844fc4B006D67c0cDbd39371A9F73f69d974;

  // https://etherscan.io/address/0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c
  ICollector internal constant COLLECTOR = ICollector(0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c);

  // https://etherscan.io/address/0x7EfFD7b47Bfd17e52fB7559d3f924201b9DbfF3d
  address internal constant DEFAULT_A_TOKEN_IMPL_REV_1 = 0x7EfFD7b47Bfd17e52fB7559d3f924201b9DbfF3d;

  // https://etherscan.io/address/0xaC725CB59D16C81061BDeA61041a8A5e73DA9EC6
  address internal constant DEFAULT_VARIABLE_DEBT_TOKEN_IMPL_REV_1 =
    0xaC725CB59D16C81061BDeA61041a8A5e73DA9EC6;

  // https://etherscan.io/address/0x82dcCF206Ae2Ab46E2099e663F70DeE77caE7778
  address internal constant CAPS_PLUS_RISK_STEWARD = 0x82dcCF206Ae2Ab46E2099e663F70DeE77caE7778;

  // https://etherscan.io/address/0x46Ab47bA01EF627ce47F2ED61C9482794a6109c4
  address internal constant RISK_STEWARD = 0x46Ab47bA01EF627ce47F2ED61C9482794a6109c4;

  // https://etherscan.io/address/0x2eE68ACb6A1319de1b49DC139894644E424fefD6
  address internal constant FREEZING_STEWARD = 0x2eE68ACb6A1319de1b49DC139894644E424fefD6;

  // https://etherscan.io/address/0xd7852E139a7097E119623de0751AE53a61efb442
  address internal constant DEBT_SWAP_ADAPTER = 0xd7852E139a7097E119623de0751AE53a61efb442;

  // https://etherscan.io/address/0x21714092D90c7265F52fdfDae068EC11a23C6248
  address internal constant DELEGATION_AWARE_A_TOKEN_IMPL_REV_1 =
    0x21714092D90c7265F52fdfDae068EC11a23C6248;

  // https://etherscan.io/address/0xA8e351C7Ab1b75A2134A418701919c462932DF79
  address internal constant CONFIG_ENGINE = 0xA8e351C7Ab1b75A2134A418701919c462932DF79;

  // https://etherscan.io/address/0xbaA999AC55EAce41CcAE355c77809e68Bb345170
  address internal constant POOL_ADDRESSES_PROVIDER_REGISTRY =
    0xbaA999AC55EAce41CcAE355c77809e68Bb345170;

  // https://etherscan.io/address/0x35bb522b102326ea3F1141661dF4626C87000e3E
  address internal constant REPAY_WITH_COLLATERAL_ADAPTER =
    0x35bb522b102326ea3F1141661dF4626C87000e3E;

  // https://etherscan.io/address/0x411D79b8cC43384FDE66CaBf9b6a17180c842511
  address internal constant LEGACY_STATIC_A_TOKEN_FACTORY =
    0x411D79b8cC43384FDE66CaBf9b6a17180c842511;

  // https://etherscan.io/address/0xADC0A53095A0af87F3aa29FE0715B5c28016364e
  address internal constant SWAP_COLLATERAL_ADAPTER = 0xADC0A53095A0af87F3aa29FE0715B5c28016364e;

  // https://etherscan.io/address/0x379c1EDD1A41218bdbFf960a9d5AD2818Bf61aE8
  address internal constant UI_GHO_DATA_PROVIDER = 0x379c1EDD1A41218bdbFf960a9d5AD2818Bf61aE8;

  // https://etherscan.io/address/0xe3dFf4052F0bF6134ACb73bEaE8fe2317d71F047
  address internal constant UI_INCENTIVE_DATA_PROVIDER = 0xe3dFf4052F0bF6134ACb73bEaE8fe2317d71F047;

  // https://etherscan.io/address/0x3F78BBD206e4D3c504Eb854232EdA7e47E9Fd8FC
  address internal constant UI_POOL_DATA_PROVIDER = 0x3F78BBD206e4D3c504Eb854232EdA7e47E9Fd8FC;

  // https://etherscan.io/address/0xC7be5307ba715ce89b152f3Df0658295b3dbA8E2
  address internal constant WALLET_BALANCE_PROVIDER = 0xC7be5307ba715ce89b152f3Df0658295b3dbA8E2;

  // https://etherscan.io/address/0xd01607c3C5eCABa394D8be377a08590149325722
  address internal constant WETH_GATEWAY = 0xd01607c3C5eCABa394D8be377a08590149325722;

  // https://etherscan.io/address/0x78F8Bd884C3D738B74B420540659c82f392820e0
  address internal constant WITHDRAW_SWAP_ADAPTER = 0x78F8Bd884C3D738B74B420540659c82f392820e0;

  // https://etherscan.io/address/0xE28E2c8d240dd5eBd0adcab86fbD79df7a052034
  address internal constant SAVINGS_DAI_TOKEN_WRAPPER = 0xE28E2c8d240dd5eBd0adcab86fbD79df7a052034;

  // https://etherscan.io/address/0xCb0b5cA20b6C5C02A9A3B2cE433650768eD2974F
  address internal constant STATA_FACTORY = 0xCb0b5cA20b6C5C02A9A3B2cE433650768eD2974F;

  // https://etherscan.io/address/0x31a0Ba3C2242a095dBF58A7C53751eCBd27dBA9b
  address internal constant DUST_BIN = 0x31a0Ba3C2242a095dBF58A7C53751eCBd27dBA9b;

  // https://etherscan.io/address/0xf00E2de0E78DFf055A92AD4719a179CE275b6Ef7
  address internal constant CLINIC_STEWARD = 0xf00E2de0E78DFf055A92AD4719a179CE275b6Ef7;

  // https://etherscan.io/address/0x8b493f416F5F7933cC146b1899c069F2361cad60
  address internal constant SVR_STEWARD = 0x8b493f416F5F7933cC146b1899c069F2361cad60;

  // https://etherscan.io/address/0x22aC12a6937BBBC0a301AF9154d08EaD95673122
  address internal constant POOL_EXPOSURE_STEWARD = 0x22aC12a6937BBBC0a301AF9154d08EaD95673122;

  // https://etherscan.io/address/0x83ab600cE8a61b43e1757b89C0589928f765c1C4
  address internal constant EDGE_INJECTOR_PENDLE_EMODE = 0x83ab600cE8a61b43e1757b89C0589928f765c1C4;
}
library AaveV3EthereumAssets {
  // https://etherscan.io/address/0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2
  address internal constant WETH_UNDERLYING = 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2;

  uint8 internal constant WETH_DECIMALS = 18;

  // https://etherscan.io/address/0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8
  address internal constant WETH_A_TOKEN = 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8;

  // https://etherscan.io/address/0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE
  address internal constant WETH_V_TOKEN = 0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE;

  // https://etherscan.io/address/0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419
  address internal constant WETH_ORACLE = 0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant WETH_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x252231882FB38481497f3C767469106297c8d93b
  address internal constant WETH_STATIC_A_TOKEN = 0x252231882FB38481497f3C767469106297c8d93b;

  // https://etherscan.io/address/0x0bfc9d54Fc184518A81162F8fB99c2eACa081202
  address internal constant WETH_STATA_TOKEN = 0x0bfc9d54Fc184518A81162F8fB99c2eACa081202;

  // https://etherscan.io/address/0x7f39C581F595B53c5cb19bD0b3f8dA6c935E2Ca0
  address internal constant wstETH_UNDERLYING = 0x7f39C581F595B53c5cb19bD0b3f8dA6c935E2Ca0;

  uint8 internal constant wstETH_DECIMALS = 18;

  // https://etherscan.io/address/0x0B925eD163218f6662a35e0f0371Ac234f9E9371
  address internal constant wstETH_A_TOKEN = 0x0B925eD163218f6662a35e0f0371Ac234f9E9371;

  // https://etherscan.io/address/0xC96113eED8cAB59cD8A66813bCB0cEb29F06D2e4
  address internal constant wstETH_V_TOKEN = 0xC96113eED8cAB59cD8A66813bCB0cEb29F06D2e4;

  // https://etherscan.io/address/0xB4aB0c94159bc2d8C133946E7241368fc2F2a010
  address internal constant wstETH_ORACLE = 0xB4aB0c94159bc2d8C133946E7241368fc2F2a010;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant wstETH_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x322AA5F5Be95644d6c36544B6c5061F072D16DF5
  address internal constant wstETH_STATIC_A_TOKEN = 0x322AA5F5Be95644d6c36544B6c5061F072D16DF5;

  // https://etherscan.io/address/0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599
  address internal constant WBTC_UNDERLYING = 0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599;

  uint8 internal constant WBTC_DECIMALS = 8;

  // https://etherscan.io/address/0x5Ee5bf7ae06D1Be5997A1A72006FE6C607eC6DE8
  address internal constant WBTC_A_TOKEN = 0x5Ee5bf7ae06D1Be5997A1A72006FE6C607eC6DE8;

  // https://etherscan.io/address/0x40aAbEf1aa8f0eEc637E0E7d92fbfFB2F26A8b7B
  address internal constant WBTC_V_TOKEN = 0x40aAbEf1aa8f0eEc637E0E7d92fbfFB2F26A8b7B;

  // https://etherscan.io/address/0xDaa4B74C6bAc4e25188e64ebc68DB5050b690cAc
  address internal constant WBTC_ORACLE = 0xDaa4B74C6bAc4e25188e64ebc68DB5050b690cAc;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant WBTC_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xB07E357cc262E92eee03D8B81464D596B258eA7a
  address internal constant WBTC_STATIC_A_TOKEN = 0xB07E357cc262E92eee03D8B81464D596B258eA7a;

  // https://etherscan.io/address/0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48
  address internal constant USDC_UNDERLYING = 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48;

  uint8 internal constant USDC_DECIMALS = 6;

  // https://etherscan.io/address/0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c
  address internal constant USDC_A_TOKEN = 0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c;

  // https://etherscan.io/address/0x72E95b8931767C79bA4EeE721354d6E99a61D004
  address internal constant USDC_V_TOKEN = 0x72E95b8931767C79bA4EeE721354d6E99a61D004;

  // https://etherscan.io/address/0xB6557F02F0a5dA7b9D3C2d979cc19e00e756F6dA
  address internal constant USDC_ORACLE = 0xB6557F02F0a5dA7b9D3C2d979cc19e00e756F6dA;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant USDC_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x73edDFa87C71ADdC275c2b9890f5c3a8480bC9E6
  address internal constant USDC_STATIC_A_TOKEN = 0x73edDFa87C71ADdC275c2b9890f5c3a8480bC9E6;

  // https://etherscan.io/address/0xD4fa2D31b7968E448877f69A96DE69f5de8cD23E
  address internal constant USDC_STATA_TOKEN = 0xD4fa2D31b7968E448877f69A96DE69f5de8cD23E;

  // https://etherscan.io/address/0x6B175474E89094C44Da98b954EedeAC495271d0F
  address internal constant DAI_UNDERLYING = 0x6B175474E89094C44Da98b954EedeAC495271d0F;

  uint8 internal constant DAI_DECIMALS = 18;

  // https://etherscan.io/address/0x018008bfb33d285247A21d44E50697654f754e63
  address internal constant DAI_A_TOKEN = 0x018008bfb33d285247A21d44E50697654f754e63;

  // https://etherscan.io/address/0xcF8d0c70c850859266f5C338b38F9D663181C314
  address internal constant DAI_V_TOKEN = 0xcF8d0c70c850859266f5C338b38F9D663181C314;

  // https://etherscan.io/address/0x5c66322CA59bB61e867B28195576DbD8dA4b08dE
  address internal constant DAI_ORACLE = 0x5c66322CA59bB61e867B28195576DbD8dA4b08dE;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant DAI_INTEREST_RATE_STRATEGY = 0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xaf270C38fF895EA3f95Ed488CEACe2386F038249
  address internal constant DAI_STATIC_A_TOKEN = 0xaf270C38fF895EA3f95Ed488CEACe2386F038249;

  // https://etherscan.io/address/0x5caF5a86f39073637Ac7c8A7b5290871de80cb9b
  address internal constant DAI_STATA_TOKEN = 0x5caF5a86f39073637Ac7c8A7b5290871de80cb9b;

  // https://etherscan.io/address/0x514910771AF9Ca656af840dff83E8264EcF986CA
  address internal constant LINK_UNDERLYING = 0x514910771AF9Ca656af840dff83E8264EcF986CA;

  uint8 internal constant LINK_DECIMALS = 18;

  // https://etherscan.io/address/0x5E8C8A7243651DB1384C0dDfDbE39761E8e7E51a
  address internal constant LINK_A_TOKEN = 0x5E8C8A7243651DB1384C0dDfDbE39761E8e7E51a;

  // https://etherscan.io/address/0x4228F8895C7dDA20227F6a5c6751b8Ebf19a6ba8
  address internal constant LINK_V_TOKEN = 0x4228F8895C7dDA20227F6a5c6751b8Ebf19a6ba8;

  // https://etherscan.io/address/0xC7e9b623ed51F033b32AE7f1282b1AD62C28C183
  address internal constant LINK_ORACLE = 0xC7e9b623ed51F033b32AE7f1282b1AD62C28C183;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant LINK_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x57bd8C73838d1781b4f6E0d5Cf89eb676488d3df
  address internal constant LINK_STATIC_A_TOKEN = 0x57bd8C73838d1781b4f6E0d5Cf89eb676488d3df;

  // https://etherscan.io/address/0x7Fc66500c84A76Ad7e9c93437bFc5Ac33E2DDaE9
  address internal constant AAVE_UNDERLYING = 0x7Fc66500c84A76Ad7e9c93437bFc5Ac33E2DDaE9;

  uint8 internal constant AAVE_DECIMALS = 18;

  // https://etherscan.io/address/0xA700b4eB416Be35b2911fd5Dee80678ff64fF6C9
  address internal constant AAVE_A_TOKEN = 0xA700b4eB416Be35b2911fd5Dee80678ff64fF6C9;

  // https://etherscan.io/address/0xBae535520Abd9f8C85E58929e0006A2c8B372F74
  address internal constant AAVE_V_TOKEN = 0xBae535520Abd9f8C85E58929e0006A2c8B372F74;

  // https://etherscan.io/address/0xF02C1e2A3B77c1cacC72f72B44f7d0a4c62e4a85
  address internal constant AAVE_ORACLE = 0xF02C1e2A3B77c1cacC72f72B44f7d0a4c62e4a85;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant AAVE_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xFEB859A50f92C6D5ad7C9eF7C2c060D164B3280f
  address internal constant AAVE_STATIC_A_TOKEN = 0xFEB859A50f92C6D5ad7C9eF7C2c060D164B3280f;

  // https://etherscan.io/address/0xBe9895146f7AF43049ca1c1AE358B0541Ea49704
  address internal constant cbETH_UNDERLYING = 0xBe9895146f7AF43049ca1c1AE358B0541Ea49704;

  uint8 internal constant cbETH_DECIMALS = 18;

  // https://etherscan.io/address/0x977b6fc5dE62598B08C85AC8Cf2b745874E8b78c
  address internal constant cbETH_A_TOKEN = 0x977b6fc5dE62598B08C85AC8Cf2b745874E8b78c;

  // https://etherscan.io/address/0x0c91bcA95b5FE69164cE583A2ec9429A569798Ed
  address internal constant cbETH_V_TOKEN = 0x0c91bcA95b5FE69164cE583A2ec9429A569798Ed;

  // https://etherscan.io/address/0x6243d2F41b4ec944F731f647589E28d9745a2674
  address internal constant cbETH_ORACLE = 0x6243d2F41b4ec944F731f647589E28d9745a2674;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant cbETH_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xe2a6863C8f043457B497667Ef3c43073e2D69089
  address internal constant cbETH_STATIC_A_TOKEN = 0xe2a6863C8f043457B497667Ef3c43073e2D69089;

  // https://etherscan.io/address/0xdAC17F958D2ee523a2206206994597C13D831ec7
  address internal constant USDT_UNDERLYING = 0xdAC17F958D2ee523a2206206994597C13D831ec7;

  uint8 internal constant USDT_DECIMALS = 6;

  // https://etherscan.io/address/0x23878914EFE38d27C4D67Ab83ed1b93A74D4086a
  address internal constant USDT_A_TOKEN = 0x23878914EFE38d27C4D67Ab83ed1b93A74D4086a;

  // https://etherscan.io/address/0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8
  address internal constant USDT_V_TOKEN = 0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8;

  // https://etherscan.io/address/0x260326c220E469358846b187eE53328303Efe19C
  address internal constant USDT_ORACLE = 0x260326c220E469358846b187eE53328303Efe19C;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant USDT_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x862c57d48becB45583AEbA3f489696D22466Ca1b
  address internal constant USDT_STATIC_A_TOKEN = 0x862c57d48becB45583AEbA3f489696D22466Ca1b;

  // https://etherscan.io/address/0x7Bc3485026Ac48b6cf9BaF0A377477Fff5703Af8
  address internal constant USDT_STATA_TOKEN = 0x7Bc3485026Ac48b6cf9BaF0A377477Fff5703Af8;

  // https://etherscan.io/address/0xae78736Cd615f374D3085123A210448E74Fc6393
  address internal constant rETH_UNDERLYING = 0xae78736Cd615f374D3085123A210448E74Fc6393;

  uint8 internal constant rETH_DECIMALS = 18;

  // https://etherscan.io/address/0xCc9EE9483f662091a1de4795249E24aC0aC2630f
  address internal constant rETH_A_TOKEN = 0xCc9EE9483f662091a1de4795249E24aC0aC2630f;

  // https://etherscan.io/address/0xae8593DD575FE29A9745056aA91C4b746eee62C8
  address internal constant rETH_V_TOKEN = 0xae8593DD575FE29A9745056aA91C4b746eee62C8;

  // https://etherscan.io/address/0x5AE8365D0a30D67145f0c55A08760C250559dB64
  address internal constant rETH_ORACLE = 0x5AE8365D0a30D67145f0c55A08760C250559dB64;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant rETH_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x867Cf025B5dA438c4e215c60B59bBB3aFe896Fda
  address internal constant rETH_STATIC_A_TOKEN = 0x867Cf025B5dA438c4e215c60B59bBB3aFe896Fda;

  // https://etherscan.io/address/0x5f98805A4E8be255a32880FDeC7F6728C6568bA0
  address internal constant LUSD_UNDERLYING = 0x5f98805A4E8be255a32880FDeC7F6728C6568bA0;

  uint8 internal constant LUSD_DECIMALS = 18;

  // https://etherscan.io/address/0x3Fe6a295459FAe07DF8A0ceCC36F37160FE86AA9
  address internal constant LUSD_A_TOKEN = 0x3Fe6a295459FAe07DF8A0ceCC36F37160FE86AA9;

  // https://etherscan.io/address/0x33652e48e4B74D18520f11BfE58Edd2ED2cEc5A2
  address internal constant LUSD_V_TOKEN = 0x33652e48e4B74D18520f11BfE58Edd2ED2cEc5A2;

  // https://etherscan.io/address/0xEbb721daf3DA9f1b3dcEc590cDf648137172d7CB
  address internal constant LUSD_ORACLE = 0xEbb721daf3DA9f1b3dcEc590cDf648137172d7CB;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant LUSD_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xDBf5E36569798D1E39eE9d7B1c61A7409a74F23A
  address internal constant LUSD_STATIC_A_TOKEN = 0xDBf5E36569798D1E39eE9d7B1c61A7409a74F23A;

  // https://etherscan.io/address/0xD533a949740bb3306d119CC777fa900bA034cd52
  address internal constant CRV_UNDERLYING = 0xD533a949740bb3306d119CC777fa900bA034cd52;

  uint8 internal constant CRV_DECIMALS = 18;

  // https://etherscan.io/address/0x7B95Ec873268a6BFC6427e7a28e396Db9D0ebc65
  address internal constant CRV_A_TOKEN = 0x7B95Ec873268a6BFC6427e7a28e396Db9D0ebc65;

  // https://etherscan.io/address/0x1b7D3F4b3c032a5AE656e30eeA4e8E1Ba376068F
  address internal constant CRV_V_TOKEN = 0x1b7D3F4b3c032a5AE656e30eeA4e8E1Ba376068F;

  // https://etherscan.io/address/0xCd627aA160A6fA45Eb793D19Ef54f5062F20f33f
  address internal constant CRV_ORACLE = 0xCd627aA160A6fA45Eb793D19Ef54f5062F20f33f;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant CRV_INTEREST_RATE_STRATEGY = 0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x149EE12310D499F701B6A5714eDAd2C832008fd2
  address internal constant CRV_STATIC_A_TOKEN = 0x149EE12310D499F701B6A5714eDAd2C832008fd2;

  // https://etherscan.io/address/0x9f8F72aA9304c8B593d555F12eF6589cC3A579A2
  address internal constant MKR_UNDERLYING = 0x9f8F72aA9304c8B593d555F12eF6589cC3A579A2;

  uint8 internal constant MKR_DECIMALS = 18;

  // https://etherscan.io/address/0x8A458A9dc9048e005d22849F470891b840296619
  address internal constant MKR_A_TOKEN = 0x8A458A9dc9048e005d22849F470891b840296619;

  // https://etherscan.io/address/0x6Efc73E54E41b27d2134fF9f98F15550f30DF9B1
  address internal constant MKR_V_TOKEN = 0x6Efc73E54E41b27d2134fF9f98F15550f30DF9B1;

  // https://etherscan.io/address/0xec1D1B3b0443256cc3860e24a46F108e699484Aa
  address internal constant MKR_ORACLE = 0xec1D1B3b0443256cc3860e24a46F108e699484Aa;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant MKR_INTEREST_RATE_STRATEGY = 0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xC011a73ee8576Fb46F5E1c5751cA3B9Fe0af2a6F
  address internal constant SNX_UNDERLYING = 0xC011a73ee8576Fb46F5E1c5751cA3B9Fe0af2a6F;

  uint8 internal constant SNX_DECIMALS = 18;

  // https://etherscan.io/address/0xC7B4c17861357B8ABB91F25581E7263E08DCB59c
  address internal constant SNX_A_TOKEN = 0xC7B4c17861357B8ABB91F25581E7263E08DCB59c;

  // https://etherscan.io/address/0x8d0de040e8aAd872eC3c33A3776dE9152D3c34ca
  address internal constant SNX_V_TOKEN = 0x8d0de040e8aAd872eC3c33A3776dE9152D3c34ca;

  // https://etherscan.io/address/0xDC3EA94CD0AC27d9A86C180091e7f78C683d3699
  address internal constant SNX_ORACLE = 0xDC3EA94CD0AC27d9A86C180091e7f78C683d3699;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant SNX_INTEREST_RATE_STRATEGY = 0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xaECEbdfE454d869A626cAb38226C52a1575D1866
  address internal constant SNX_STATIC_A_TOKEN = 0xaECEbdfE454d869A626cAb38226C52a1575D1866;

  // https://etherscan.io/address/0xba100000625a3754423978a60c9317c58a424e3D
  address internal constant BAL_UNDERLYING = 0xba100000625a3754423978a60c9317c58a424e3D;

  uint8 internal constant BAL_DECIMALS = 18;

  // https://etherscan.io/address/0x2516E7B3F76294e03C42AA4c5b5b4DCE9C436fB8
  address internal constant BAL_A_TOKEN = 0x2516E7B3F76294e03C42AA4c5b5b4DCE9C436fB8;

  // https://etherscan.io/address/0x3D3efceb4Ff0966D34d9545D3A2fa2dcdBf451f2
  address internal constant BAL_V_TOKEN = 0x3D3efceb4Ff0966D34d9545D3A2fa2dcdBf451f2;

  // https://etherscan.io/address/0xdF2917806E30300537aEB49A7663062F4d1F2b5F
  address internal constant BAL_ORACLE = 0xdF2917806E30300537aEB49A7663062F4d1F2b5F;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant BAL_INTEREST_RATE_STRATEGY = 0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984
  address internal constant UNI_UNDERLYING = 0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984;

  uint8 internal constant UNI_DECIMALS = 18;

  // https://etherscan.io/address/0xF6D2224916DDFbbab6e6bd0D1B7034f4Ae0CaB18
  address internal constant UNI_A_TOKEN = 0xF6D2224916DDFbbab6e6bd0D1B7034f4Ae0CaB18;

  // https://etherscan.io/address/0xF64178Ebd2E2719F2B1233bCb5Ef6DB4bCc4d09a
  address internal constant UNI_V_TOKEN = 0xF64178Ebd2E2719F2B1233bCb5Ef6DB4bCc4d09a;

  // https://etherscan.io/address/0x553303d460EE0afB37EdFf9bE42922D8FF63220e
  address internal constant UNI_ORACLE = 0x553303d460EE0afB37EdFf9bE42922D8FF63220e;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant UNI_INTEREST_RATE_STRATEGY = 0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x78fb5E79D5cb59729D0cd72bEA7879aD2683454D
  address internal constant UNI_STATIC_A_TOKEN = 0x78fb5E79D5cb59729D0cd72bEA7879aD2683454D;

  // https://etherscan.io/address/0x5A98FcBEA516Cf06857215779Fd812CA3beF1B32
  address internal constant LDO_UNDERLYING = 0x5A98FcBEA516Cf06857215779Fd812CA3beF1B32;

  uint8 internal constant LDO_DECIMALS = 18;

  // https://etherscan.io/address/0x9A44fd41566876A39655f74971a3A6eA0a17a454
  address internal constant LDO_A_TOKEN = 0x9A44fd41566876A39655f74971a3A6eA0a17a454;

  // https://etherscan.io/address/0xc30808705C01289A3D306ca9CAB081Ba9114eC82
  address internal constant LDO_V_TOKEN = 0xc30808705C01289A3D306ca9CAB081Ba9114eC82;

  // https://etherscan.io/address/0xb01e6C9af83879B8e06a092f0DD94309c0D497E4
  address internal constant LDO_ORACLE = 0xb01e6C9af83879B8e06a092f0DD94309c0D497E4;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant LDO_INTEREST_RATE_STRATEGY = 0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x1eA6E1ba21601258401d0B9DB24eA0a07948458e
  address internal constant LDO_STATIC_A_TOKEN = 0x1eA6E1ba21601258401d0B9DB24eA0a07948458e;

  // https://etherscan.io/address/0xC18360217D8F7Ab5e7c516566761Ea12Ce7F9D72
  address internal constant ENS_UNDERLYING = 0xC18360217D8F7Ab5e7c516566761Ea12Ce7F9D72;

  uint8 internal constant ENS_DECIMALS = 18;

  // https://etherscan.io/address/0x545bD6c032eFdde65A377A6719DEF2796C8E0f2e
  address internal constant ENS_A_TOKEN = 0x545bD6c032eFdde65A377A6719DEF2796C8E0f2e;

  // https://etherscan.io/address/0xd180D7fdD4092f07428eFE801E17BC03576b3192
  address internal constant ENS_V_TOKEN = 0xd180D7fdD4092f07428eFE801E17BC03576b3192;

  // https://etherscan.io/address/0x5C00128d4d1c2F4f652C267d7bcdD7aC99C16E16
  address internal constant ENS_ORACLE = 0x5C00128d4d1c2F4f652C267d7bcdD7aC99C16E16;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant ENS_INTEREST_RATE_STRATEGY = 0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x2767C27Eeaf3566082E74b963B6A0f5c9a46C8a1
  address internal constant ENS_STATIC_A_TOKEN = 0x2767C27Eeaf3566082E74b963B6A0f5c9a46C8a1;

  // https://etherscan.io/address/0x111111111117dC0aa78b770fA6A738034120C302
  address internal constant ONE_INCH_UNDERLYING = 0x111111111117dC0aa78b770fA6A738034120C302;

  uint8 internal constant ONE_INCH_DECIMALS = 18;

  // https://etherscan.io/address/0x71Aef7b30728b9BB371578f36c5A1f1502a5723e
  address internal constant ONE_INCH_A_TOKEN = 0x71Aef7b30728b9BB371578f36c5A1f1502a5723e;

  // https://etherscan.io/address/0xA38fCa8c6Bf9BdA52E76EB78f08CaA3BE7c5A970
  address internal constant ONE_INCH_V_TOKEN = 0xA38fCa8c6Bf9BdA52E76EB78f08CaA3BE7c5A970;

  // https://etherscan.io/address/0xc929ad75B72593967DE83E7F7Cda0493458261D9
  address internal constant ONE_INCH_ORACLE = 0xc929ad75B72593967DE83E7F7Cda0493458261D9;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant ONE_INCH_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xB490fF18e55b8881C9527FE7E358dd363780449F
  address internal constant ONE_INCH_STATIC_A_TOKEN = 0xB490fF18e55b8881C9527FE7E358dd363780449F;

  // https://etherscan.io/address/0x853d955aCEf822Db058eb8505911ED77F175b99e
  address internal constant FRAX_UNDERLYING = 0x853d955aCEf822Db058eb8505911ED77F175b99e;

  uint8 internal constant FRAX_DECIMALS = 18;

  // https://etherscan.io/address/0xd4e245848d6E1220DBE62e155d89fa327E43CB06
  address internal constant FRAX_A_TOKEN = 0xd4e245848d6E1220DBE62e155d89fa327E43CB06;

  // https://etherscan.io/address/0x88B8358F5BC87c2D7E116cCA5b65A9eEb2c5EA3F
  address internal constant FRAX_V_TOKEN = 0x88B8358F5BC87c2D7E116cCA5b65A9eEb2c5EA3F;

  // https://etherscan.io/address/0xeF50f8DC65402c3019586bc8725fCD0b99B8AAd7
  address internal constant FRAX_ORACLE = 0xeF50f8DC65402c3019586bc8725fCD0b99B8AAd7;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant FRAX_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xEE66abD4D0f9908A48E08AE354B0f425De3e237E
  address internal constant FRAX_STATIC_A_TOKEN = 0xEE66abD4D0f9908A48E08AE354B0f425De3e237E;

  // https://etherscan.io/address/0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f
  address internal constant GHO_UNDERLYING = 0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f;

  uint8 internal constant GHO_DECIMALS = 18;

  // https://etherscan.io/address/0x00907f9921424583e7ffBfEdf84F92B7B2Be4977
  address internal constant GHO_A_TOKEN = 0x00907f9921424583e7ffBfEdf84F92B7B2Be4977;

  // https://etherscan.io/address/0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B
  address internal constant GHO_V_TOKEN = 0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B;

  // https://etherscan.io/address/0xD110cac5d8682A3b045D5524a9903E031d70FCCd
  address internal constant GHO_ORACLE = 0xD110cac5d8682A3b045D5524a9903E031d70FCCd;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant GHO_INTEREST_RATE_STRATEGY = 0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x048459E4fb3402e58d8900aF7283Ad574B91d742
  address internal constant GHO_STATIC_A_TOKEN = 0x048459E4fb3402e58d8900aF7283Ad574B91d742;

  // https://etherscan.io/address/0xD33526068D116cE69F19A9ee46F0bd304F21A51f
  address internal constant RPL_UNDERLYING = 0xD33526068D116cE69F19A9ee46F0bd304F21A51f;

  uint8 internal constant RPL_DECIMALS = 18;

  // https://etherscan.io/address/0xB76CF92076adBF1D9C39294FA8e7A67579FDe357
  address internal constant RPL_A_TOKEN = 0xB76CF92076adBF1D9C39294FA8e7A67579FDe357;

  // https://etherscan.io/address/0x8988ECA19D502fd8b9CCd03fA3bD20a6f599bc2A
  address internal constant RPL_V_TOKEN = 0x8988ECA19D502fd8b9CCd03fA3bD20a6f599bc2A;

  // https://etherscan.io/address/0x4E155eD98aFE9034b7A5962f6C84c86d869daA9d
  address internal constant RPL_ORACLE = 0x4E155eD98aFE9034b7A5962f6C84c86d869daA9d;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant RPL_INTEREST_RATE_STRATEGY = 0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x95EF7cb3494e65dA4926bA330dBf540a13afFD17
  address internal constant RPL_STATIC_A_TOKEN = 0x95EF7cb3494e65dA4926bA330dBf540a13afFD17;

  // https://etherscan.io/address/0x91ad1f5443cF356010D2171D6D26B11C309c4b16
  address internal constant RPL_STATA_TOKEN = 0x91ad1f5443cF356010D2171D6D26B11C309c4b16;

  // https://etherscan.io/address/0x83F20F44975D03b1b09e64809B757c47f942BEeA
  address internal constant sDAI_UNDERLYING = 0x83F20F44975D03b1b09e64809B757c47f942BEeA;

  uint8 internal constant sDAI_DECIMALS = 18;

  // https://etherscan.io/address/0x4C612E3B15b96Ff9A6faED838F8d07d479a8dD4c
  address internal constant sDAI_A_TOKEN = 0x4C612E3B15b96Ff9A6faED838F8d07d479a8dD4c;

  // https://etherscan.io/address/0x8Db9D35e117d8b93C6Ca9b644b25BaD5d9908141
  address internal constant sDAI_V_TOKEN = 0x8Db9D35e117d8b93C6Ca9b644b25BaD5d9908141;

  // https://etherscan.io/address/0xf83B85205241c3BCCA0a09D32FaE65c16e0CF236
  address internal constant sDAI_ORACLE = 0xf83B85205241c3BCCA0a09D32FaE65c16e0CF236;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant sDAI_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xFa7E3571786CE9489bBC58d9Cb8ecE8aAe6B56F3
  address internal constant sDAI_STATIC_A_TOKEN = 0xFa7E3571786CE9489bBC58d9Cb8ecE8aAe6B56F3;

  // https://etherscan.io/address/0xAf5191B0De278C7286d6C7CC6ab6BB8A73bA2Cd6
  address internal constant STG_UNDERLYING = 0xAf5191B0De278C7286d6C7CC6ab6BB8A73bA2Cd6;

  uint8 internal constant STG_DECIMALS = 18;

  // https://etherscan.io/address/0x1bA9843bD4327c6c77011406dE5fA8749F7E3479
  address internal constant STG_A_TOKEN = 0x1bA9843bD4327c6c77011406dE5fA8749F7E3479;

  // https://etherscan.io/address/0x655568bDd6168325EC7e58Bf39b21A856F906Dc2
  address internal constant STG_V_TOKEN = 0x655568bDd6168325EC7e58Bf39b21A856F906Dc2;

  // https://etherscan.io/address/0x7A9f34a0Aa917D438e9b6E630067062B7F8f6f3d
  address internal constant STG_ORACLE = 0x7A9f34a0Aa917D438e9b6E630067062B7F8f6f3d;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant STG_INTEREST_RATE_STRATEGY = 0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xdeFA4e8a7bcBA345F687a2f1456F5Edd9CE97202
  address internal constant KNC_UNDERLYING = 0xdeFA4e8a7bcBA345F687a2f1456F5Edd9CE97202;

  uint8 internal constant KNC_DECIMALS = 18;

  // https://etherscan.io/address/0x5b502e3796385E1e9755d7043B9C945C3aCCeC9C
  address internal constant KNC_A_TOKEN = 0x5b502e3796385E1e9755d7043B9C945C3aCCeC9C;

  // https://etherscan.io/address/0x253127Ffc04981cEA8932F406710661c2f2c3fD2
  address internal constant KNC_V_TOKEN = 0x253127Ffc04981cEA8932F406710661c2f2c3fD2;

  // https://etherscan.io/address/0xf8fF43E991A81e6eC886a3D281A2C6cC19aE70Fc
  address internal constant KNC_ORACLE = 0xf8fF43E991A81e6eC886a3D281A2C6cC19aE70Fc;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant KNC_INTEREST_RATE_STRATEGY = 0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x3432B6A60D23Ca0dFCa7761B7ab56459D9C964D0
  address internal constant FXS_UNDERLYING = 0x3432B6A60D23Ca0dFCa7761B7ab56459D9C964D0;

  uint8 internal constant FXS_DECIMALS = 18;

  // https://etherscan.io/address/0x82F9c5ad306BBa1AD0De49bB5FA6F01bf61085ef
  address internal constant FXS_A_TOKEN = 0x82F9c5ad306BBa1AD0De49bB5FA6F01bf61085ef;

  // https://etherscan.io/address/0x68e9f0aD4e6f8F5DB70F6923d4d6d5b225B83b16
  address internal constant FXS_V_TOKEN = 0x68e9f0aD4e6f8F5DB70F6923d4d6d5b225B83b16;

  // https://etherscan.io/address/0x6Ebc52C8C1089be9eB3945C4350B68B8E4C2233f
  address internal constant FXS_ORACLE = 0x6Ebc52C8C1089be9eB3945C4350B68B8E4C2233f;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant FXS_INTEREST_RATE_STRATEGY = 0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xf939E0A03FB07F59A73314E73794Be0E57ac1b4E
  address internal constant crvUSD_UNDERLYING = 0xf939E0A03FB07F59A73314E73794Be0E57ac1b4E;

  uint8 internal constant crvUSD_DECIMALS = 18;

  // https://etherscan.io/address/0xb82fa9f31612989525992FCfBB09AB22Eff5c85A
  address internal constant crvUSD_A_TOKEN = 0xb82fa9f31612989525992FCfBB09AB22Eff5c85A;

  // https://etherscan.io/address/0x028f7886F3e937f8479efaD64f31B3fE1119857a
  address internal constant crvUSD_V_TOKEN = 0x028f7886F3e937f8479efaD64f31B3fE1119857a;

  // https://etherscan.io/address/0x9Dc30dc58c72f5B669aEa01d02A2e4da194eE893
  address internal constant crvUSD_ORACLE = 0x9Dc30dc58c72f5B669aEa01d02A2e4da194eE893;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant crvUSD_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x848107491E029AFDe0AC543779c7790382f15929
  address internal constant crvUSD_STATIC_A_TOKEN = 0x848107491E029AFDe0AC543779c7790382f15929;

  // https://etherscan.io/address/0x6c3ea9036406852006290770BEdFcAbA0e23A0e8
  address internal constant PYUSD_UNDERLYING = 0x6c3ea9036406852006290770BEdFcAbA0e23A0e8;

  uint8 internal constant PYUSD_DECIMALS = 6;

  // https://etherscan.io/address/0x0C0d01AbF3e6aDfcA0989eBbA9d6e85dD58EaB1E
  address internal constant PYUSD_A_TOKEN = 0x0C0d01AbF3e6aDfcA0989eBbA9d6e85dD58EaB1E;

  // https://etherscan.io/address/0x57B67e4DE077085Fd0AF2174e9c14871BE664546
  address internal constant PYUSD_V_TOKEN = 0x57B67e4DE077085Fd0AF2174e9c14871BE664546;

  // https://etherscan.io/address/0x36964C0579D02E0a5AaAb89E24Cf8d7CDF3549EE
  address internal constant PYUSD_ORACLE = 0x36964C0579D02E0a5AaAb89E24Cf8d7CDF3549EE;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant PYUSD_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x00F2a835758B33f3aC53516Ebd69f3dc77B0D152
  address internal constant PYUSD_STATIC_A_TOKEN = 0x00F2a835758B33f3aC53516Ebd69f3dc77B0D152;

  // https://etherscan.io/address/0xb51EDdDD8c47856D81C8681EA71404Cec93E92c6
  address internal constant PYUSD_STATA_TOKEN = 0xb51EDdDD8c47856D81C8681EA71404Cec93E92c6;

  // https://etherscan.io/address/0xCd5fE23C85820F7B72D0926FC9b05b43E359b7ee
  address internal constant weETH_UNDERLYING = 0xCd5fE23C85820F7B72D0926FC9b05b43E359b7ee;

  uint8 internal constant weETH_DECIMALS = 18;

  // https://etherscan.io/address/0xBdfa7b7893081B35Fb54027489e2Bc7A38275129
  address internal constant weETH_A_TOKEN = 0xBdfa7b7893081B35Fb54027489e2Bc7A38275129;

  // https://etherscan.io/address/0x77ad9BF13a52517AD698D65913e8D381300c8Bf3
  address internal constant weETH_V_TOKEN = 0x77ad9BF13a52517AD698D65913e8D381300c8Bf3;

  // https://etherscan.io/address/0xf112aF6F0A332B815fbEf3Ff932c057E570b62d3
  address internal constant weETH_ORACLE = 0xf112aF6F0A332B815fbEf3Ff932c057E570b62d3;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant weETH_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x867b0CDC4B39a19945E616c29639b0390b39db3B
  address internal constant weETH_STATIC_A_TOKEN = 0x867b0CDC4B39a19945E616c29639b0390b39db3B;

  // https://etherscan.io/address/0xf1C9acDc66974dFB6dEcB12aA385b9cD01190E38
  address internal constant osETH_UNDERLYING = 0xf1C9acDc66974dFB6dEcB12aA385b9cD01190E38;

  uint8 internal constant osETH_DECIMALS = 18;

  // https://etherscan.io/address/0x927709711794F3De5DdBF1D176bEE2D55Ba13c21
  address internal constant osETH_A_TOKEN = 0x927709711794F3De5DdBF1D176bEE2D55Ba13c21;

  // https://etherscan.io/address/0x8838eefF2af391863E1Bb8b1dF563F86743a8470
  address internal constant osETH_V_TOKEN = 0x8838eefF2af391863E1Bb8b1dF563F86743a8470;

  // https://etherscan.io/address/0x0A2AF898cEc35197e6944D9E0F525C2626393442
  address internal constant osETH_ORACLE = 0x0A2AF898cEc35197e6944D9E0F525C2626393442;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant osETH_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xE5248968166206d14ab57345971E32facD839aDA
  address internal constant osETH_STATIC_A_TOKEN = 0xE5248968166206d14ab57345971E32facD839aDA;

  // https://etherscan.io/address/0x4c9EDD5852cd905f086C759E8383e09bff1E68B3
  address internal constant USDe_UNDERLYING = 0x4c9EDD5852cd905f086C759E8383e09bff1E68B3;

  uint8 internal constant USDe_DECIMALS = 18;

  // https://etherscan.io/address/0x4F5923Fc5FD4a93352581b38B7cD26943012DECF
  address internal constant USDe_A_TOKEN = 0x4F5923Fc5FD4a93352581b38B7cD26943012DECF;

  // https://etherscan.io/address/0x015396E1F286289aE23a762088E863b3ec465145
  address internal constant USDe_V_TOKEN = 0x015396E1F286289aE23a762088E863b3ec465145;

  // https://etherscan.io/address/0xC26D4a1c46d884cfF6dE9800B6aE7A8Cf48B4Ff8
  address internal constant USDe_ORACLE = 0xC26D4a1c46d884cfF6dE9800B6aE7A8Cf48B4Ff8;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant USDe_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x46e5d6A33C8Bd8eD38F3c95991C78C9B2FF3bC99
  address internal constant USDe_STATIC_A_TOKEN = 0x46e5d6A33C8Bd8eD38F3c95991C78C9B2FF3bC99;

  // https://etherscan.io/address/0x5F9D59db355b4A60501544637b00e94082cA575b
  address internal constant USDe_STATA_TOKEN = 0x5F9D59db355b4A60501544637b00e94082cA575b;

  // https://etherscan.io/address/0xA35b1B31Ce002FBF2058D22F30f95D405200A15b
  address internal constant ETHx_UNDERLYING = 0xA35b1B31Ce002FBF2058D22F30f95D405200A15b;

  uint8 internal constant ETHx_DECIMALS = 18;

  // https://etherscan.io/address/0x1c0E06a0b1A4c160c17545FF2A951bfcA57C0002
  address internal constant ETHx_A_TOKEN = 0x1c0E06a0b1A4c160c17545FF2A951bfcA57C0002;

  // https://etherscan.io/address/0x08a8Dc81AeA67F84745623aC6c72CDA3967aab8b
  address internal constant ETHx_V_TOKEN = 0x08a8Dc81AeA67F84745623aC6c72CDA3967aab8b;

  // https://etherscan.io/address/0xD6270dAabFe4862306190298C2B48fed9e15C847
  address internal constant ETHx_ORACLE = 0xD6270dAabFe4862306190298C2B48fed9e15C847;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant ETHx_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x7CC6694CF75C18D488d16FB4bf3c71A3B31cc7FB
  address internal constant ETHx_STATIC_A_TOKEN = 0x7CC6694CF75C18D488d16FB4bf3c71A3B31cc7FB;

  // https://etherscan.io/address/0x9D39A5DE30e57443BfF2A8307A4256c8797A3497
  address internal constant sUSDe_UNDERLYING = 0x9D39A5DE30e57443BfF2A8307A4256c8797A3497;

  uint8 internal constant sUSDe_DECIMALS = 18;

  // https://etherscan.io/address/0x4579a27aF00A62C0EB156349f31B345c08386419
  address internal constant sUSDe_A_TOKEN = 0x4579a27aF00A62C0EB156349f31B345c08386419;

  // https://etherscan.io/address/0xeFFDE9BFA8EC77c14C364055a200746d6e12BeD6
  address internal constant sUSDe_V_TOKEN = 0xeFFDE9BFA8EC77c14C364055a200746d6e12BeD6;

  // https://etherscan.io/address/0x42bc86f2f08419280a99d8fbEa4672e7c30a86ec
  address internal constant sUSDe_ORACLE = 0x42bc86f2f08419280a99d8fbEa4672e7c30a86ec;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant sUSDe_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x54D612b000697bd8B0094889D7d6A92bA0Bf2DEa
  address internal constant sUSDe_STATIC_A_TOKEN = 0x54D612b000697bd8B0094889D7d6A92bA0Bf2DEa;

  // https://etherscan.io/address/0x18084fbA666a33d37592fA2633fD49a74DD93a88
  address internal constant tBTC_UNDERLYING = 0x18084fbA666a33d37592fA2633fD49a74DD93a88;

  uint8 internal constant tBTC_DECIMALS = 18;

  // https://etherscan.io/address/0x10Ac93971cdb1F5c778144084242374473c350Da
  address internal constant tBTC_A_TOKEN = 0x10Ac93971cdb1F5c778144084242374473c350Da;

  // https://etherscan.io/address/0xAC50890a80A2731eb1eA2e9B4F29569CeB06D960
  address internal constant tBTC_V_TOKEN = 0xAC50890a80A2731eb1eA2e9B4F29569CeB06D960;

  // https://etherscan.io/address/0xb41E773f507F7a7EA890b1afB7d2b660c30C8B0A
  address internal constant tBTC_ORACLE = 0xb41E773f507F7a7EA890b1afB7d2b660c30C8B0A;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant tBTC_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xcbB7C0000aB88B473b1f5aFd9ef808440eed33Bf
  address internal constant cbBTC_UNDERLYING = 0xcbB7C0000aB88B473b1f5aFd9ef808440eed33Bf;

  uint8 internal constant cbBTC_DECIMALS = 8;

  // https://etherscan.io/address/0x5c647cE0Ae10658ec44FA4E11A51c96e94efd1Dd
  address internal constant cbBTC_A_TOKEN = 0x5c647cE0Ae10658ec44FA4E11A51c96e94efd1Dd;

  // https://etherscan.io/address/0xeB284A70557EFe3591b9e6D9D720040E02c54a4d
  address internal constant cbBTC_V_TOKEN = 0xeB284A70557EFe3591b9e6D9D720040E02c54a4d;

  // https://etherscan.io/address/0xb41E773f507F7a7EA890b1afB7d2b660c30C8B0A
  address internal constant cbBTC_ORACLE = 0xb41E773f507F7a7EA890b1afB7d2b660c30C8B0A;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant cbBTC_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xdC035D45d973E3EC169d2276DDab16f1e407384F
  address internal constant USDS_UNDERLYING = 0xdC035D45d973E3EC169d2276DDab16f1e407384F;

  uint8 internal constant USDS_DECIMALS = 18;

  // https://etherscan.io/address/0x32a6268f9Ba3642Dda7892aDd74f1D34469A4259
  address internal constant USDS_A_TOKEN = 0x32a6268f9Ba3642Dda7892aDd74f1D34469A4259;

  // https://etherscan.io/address/0x490E0E6255bF65b43E2e02F7acB783c5e04572Ff
  address internal constant USDS_V_TOKEN = 0x490E0E6255bF65b43E2e02F7acB783c5e04572Ff;

  // https://etherscan.io/address/0x94C7FD62fd0506e71d8142E9D36687fC72A86B02
  address internal constant USDS_ORACLE = 0x94C7FD62fd0506e71d8142E9D36687fC72A86B02;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant USDS_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xb80B3215EA8183a064073f9892eb64236160a4dF
  address internal constant USDS_STATA_TOKEN = 0xb80B3215EA8183a064073f9892eb64236160a4dF;

  // https://etherscan.io/address/0xA1290d69c65A6Fe4DF752f95823fae25cB99e5A7
  address internal constant rsETH_UNDERLYING = 0xA1290d69c65A6Fe4DF752f95823fae25cB99e5A7;

  uint8 internal constant rsETH_DECIMALS = 18;

  // https://etherscan.io/address/0x2D62109243b87C4bA3EE7bA1D91B0dD0A074d7b1
  address internal constant rsETH_A_TOKEN = 0x2D62109243b87C4bA3EE7bA1D91B0dD0A074d7b1;

  // https://etherscan.io/address/0x6De3E52A1B7294A34e271a508082b1Ff4a37E30e
  address internal constant rsETH_V_TOKEN = 0x6De3E52A1B7294A34e271a508082b1Ff4a37E30e;

  // https://etherscan.io/address/0x47F52B2e43D0386cF161e001835b03Ad49889e3b
  address internal constant rsETH_ORACLE = 0x47F52B2e43D0386cF161e001835b03Ad49889e3b;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant rsETH_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x8236a87084f8B84306f72007F36F2618A5634494
  address internal constant LBTC_UNDERLYING = 0x8236a87084f8B84306f72007F36F2618A5634494;

  uint8 internal constant LBTC_DECIMALS = 8;

  // https://etherscan.io/address/0x65906988ADEe75306021C417a1A3458040239602
  address internal constant LBTC_A_TOKEN = 0x65906988ADEe75306021C417a1A3458040239602;

  // https://etherscan.io/address/0x68aeB290C7727D899B47c56d1c96AEAC475cD0dD
  address internal constant LBTC_V_TOKEN = 0x68aeB290C7727D899B47c56d1c96AEAC475cD0dD;

  // https://etherscan.io/address/0xb41E773f507F7a7EA890b1afB7d2b660c30C8B0A
  address internal constant LBTC_ORACLE = 0xb41E773f507F7a7EA890b1afB7d2b660c30C8B0A;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant LBTC_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x657e8C867D8B37dCC18fA4Caead9C45EB088C642
  address internal constant eBTC_UNDERLYING = 0x657e8C867D8B37dCC18fA4Caead9C45EB088C642;

  uint8 internal constant eBTC_DECIMALS = 8;

  // https://etherscan.io/address/0x5fefd7069a7D91d01f269DADE14526CCF3487810
  address internal constant eBTC_A_TOKEN = 0x5fefd7069a7D91d01f269DADE14526CCF3487810;

  // https://etherscan.io/address/0x47eD0509e64615c0d5C6d39AF1B38D02Bc9fE58f
  address internal constant eBTC_V_TOKEN = 0x47eD0509e64615c0d5C6d39AF1B38D02Bc9fE58f;

  // https://etherscan.io/address/0x577C217cB5b1691A500D48aA7F69346409cFd668
  address internal constant eBTC_ORACLE = 0x577C217cB5b1691A500D48aA7F69346409cFd668;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant eBTC_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x8292Bb45bf1Ee4d140127049757C2E0fF06317eD
  address internal constant RLUSD_UNDERLYING = 0x8292Bb45bf1Ee4d140127049757C2E0fF06317eD;

  uint8 internal constant RLUSD_DECIMALS = 18;

  // https://etherscan.io/address/0xFa82580c16A31D0c1bC632A36F82e83EfEF3Eec0
  address internal constant RLUSD_A_TOKEN = 0xFa82580c16A31D0c1bC632A36F82e83EfEF3Eec0;

  // https://etherscan.io/address/0xBdFe7aD7976d5d7E0965ea83a81Ca1bCfF7e84a9
  address internal constant RLUSD_V_TOKEN = 0xBdFe7aD7976d5d7E0965ea83a81Ca1bCfF7e84a9;

  // https://etherscan.io/address/0xf0eaC18E908B34770FDEe46d069c846bDa866759
  address internal constant RLUSD_ORACLE = 0xf0eaC18E908B34770FDEe46d069c846bDa866759;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant RLUSD_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x6A1792a91C08e9f0bFe7a990871B786643237f0F
  address internal constant RLUSD_STATA_TOKEN = 0x6A1792a91C08e9f0bFe7a990871B786643237f0F;

  // https://etherscan.io/address/0x50D2C7992b802Eef16c04FeADAB310f31866a545
  address internal constant PT_eUSDE_29MAY2025_UNDERLYING =
    0x50D2C7992b802Eef16c04FeADAB310f31866a545;

  uint8 internal constant PT_eUSDE_29MAY2025_DECIMALS = 18;

  // https://etherscan.io/address/0x4B0821e768Ed9039a70eD1E80E15E76a5bE5Df5F
  address internal constant PT_eUSDE_29MAY2025_A_TOKEN = 0x4B0821e768Ed9039a70eD1E80E15E76a5bE5Df5F;

  // https://etherscan.io/address/0x3c20fbFD32243Dd9899301C84bCe17413EeE0A0C
  address internal constant PT_eUSDE_29MAY2025_V_TOKEN = 0x3c20fbFD32243Dd9899301C84bCe17413EeE0A0C;

  // https://etherscan.io/address/0x5292AB3292D076271f853Ed8e05e61cc02F0A2C6
  address internal constant PT_eUSDE_29MAY2025_ORACLE = 0x5292AB3292D076271f853Ed8e05e61cc02F0A2C6;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant PT_eUSDE_29MAY2025_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x3b3fB9C57858EF816833dC91565EFcd85D96f634
  address internal constant PT_sUSDE_31JUL2025_UNDERLYING =
    0x3b3fB9C57858EF816833dC91565EFcd85D96f634;

  uint8 internal constant PT_sUSDE_31JUL2025_DECIMALS = 18;

  // https://etherscan.io/address/0xDE6eF6CB4aBd3A473ffC2942eEf5D84536F8E864
  address internal constant PT_sUSDE_31JUL2025_A_TOKEN = 0xDE6eF6CB4aBd3A473ffC2942eEf5D84536F8E864;

  // https://etherscan.io/address/0x8C6FeaF5d58BA1A6541F9c4aF685f62bFCBaC3b1
  address internal constant PT_sUSDE_31JUL2025_V_TOKEN = 0x8C6FeaF5d58BA1A6541F9c4aF685f62bFCBaC3b1;

  // https://etherscan.io/address/0x759B9B72700A129CD7AD8e53F9c99cb48Fd57105
  address internal constant PT_sUSDE_31JUL2025_ORACLE = 0x759B9B72700A129CD7AD8e53F9c99cb48Fd57105;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant PT_sUSDE_31JUL2025_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xC139190F447e929f090Edeb554D95AbB8b18aC1C
  address internal constant USDtb_UNDERLYING = 0xC139190F447e929f090Edeb554D95AbB8b18aC1C;

  uint8 internal constant USDtb_DECIMALS = 18;

  // https://etherscan.io/address/0xEc4ef66D4fCeEba34aBB4dE69dB391Bc5476ccc8
  address internal constant USDtb_A_TOKEN = 0xEc4ef66D4fCeEba34aBB4dE69dB391Bc5476ccc8;

  // https://etherscan.io/address/0xeA85a065F87FE28Aa8Fbf0D6C7deC472b106252C
  address internal constant USDtb_V_TOKEN = 0xeA85a065F87FE28Aa8Fbf0D6C7deC472b106252C;

  // https://etherscan.io/address/0x2FA6A78E3d617c1013a22938411602dc9Da98dBa
  address internal constant USDtb_ORACLE = 0x2FA6A78E3d617c1013a22938411602dc9Da98dBa;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant USDtb_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x917459337CaAC939D41d7493B3999f571D20D667
  address internal constant PT_USDe_31JUL2025_UNDERLYING =
    0x917459337CaAC939D41d7493B3999f571D20D667;

  uint8 internal constant PT_USDe_31JUL2025_DECIMALS = 18;

  // https://etherscan.io/address/0x312ffC57778CEfa11989733e6E08143E7E229c1c
  address internal constant PT_USDe_31JUL2025_A_TOKEN = 0x312ffC57778CEfa11989733e6E08143E7E229c1c;

  // https://etherscan.io/address/0xd90DA2Df915B87fE1621A7F2201FbF4ff2cCA031
  address internal constant PT_USDe_31JUL2025_V_TOKEN = 0xd90DA2Df915B87fE1621A7F2201FbF4ff2cCA031;

  // https://etherscan.io/address/0x6b99e86B48Fee533B7Bee602e7959f024051Eca0
  address internal constant PT_USDe_31JUL2025_ORACLE = 0x6b99e86B48Fee533B7Bee602e7959f024051Eca0;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant PT_USDe_31JUL2025_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x14Bdc3A3AE09f5518b923b69489CBcAfB238e617
  address internal constant PT_eUSDE_14AUG2025_UNDERLYING =
    0x14Bdc3A3AE09f5518b923b69489CBcAfB238e617;

  uint8 internal constant PT_eUSDE_14AUG2025_DECIMALS = 18;

  // https://etherscan.io/address/0x2eDff5AF94334fBd7C38ae318edf1c40e072b73B
  address internal constant PT_eUSDE_14AUG2025_A_TOKEN = 0x2eDff5AF94334fBd7C38ae318edf1c40e072b73B;

  // https://etherscan.io/address/0x22517fE16DEd08e52E7EA3423A2EA4995b1f1731
  address internal constant PT_eUSDE_14AUG2025_V_TOKEN = 0x22517fE16DEd08e52E7EA3423A2EA4995b1f1731;

  // https://etherscan.io/address/0x03f9bA9A897241985c1f12bCe97fAC1B0bd4a7A7
  address internal constant PT_eUSDE_14AUG2025_ORACLE = 0x03f9bA9A897241985c1f12bCe97fAC1B0bd4a7A7;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant PT_eUSDE_14AUG2025_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0x90D2af7d622ca3141efA4d8f1F24d86E5974Cc8F
  address internal constant eUSDe_UNDERLYING = 0x90D2af7d622ca3141efA4d8f1F24d86E5974Cc8F;

  uint8 internal constant eUSDe_DECIMALS = 18;

  // https://etherscan.io/address/0x5F9190496e0DFC831C3bd307978de4a245E2F5cD
  address internal constant eUSDe_A_TOKEN = 0x5F9190496e0DFC831C3bd307978de4a245E2F5cD;

  // https://etherscan.io/address/0x48351fCc9536dA440AE9471220F6dC921b0eB703
  address internal constant eUSDe_V_TOKEN = 0x48351fCc9536dA440AE9471220F6dC921b0eB703;

  // https://etherscan.io/address/0xc7Ad695ac0ae38Ae308640897E51468977A862a2
  address internal constant eUSDe_ORACLE = 0xc7Ad695ac0ae38Ae308640897E51468977A862a2;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant eUSDe_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;

  // https://etherscan.io/address/0xC96dE26018A54D51c097160568752c4E3BD6C364
  address internal constant FBTC_UNDERLYING = 0xC96dE26018A54D51c097160568752c4E3BD6C364;

  uint8 internal constant FBTC_DECIMALS = 8;

  // https://etherscan.io/address/0xcCA43ceF272c30415866914351fdfc3E881bb7c2
  address internal constant FBTC_A_TOKEN = 0xcCA43ceF272c30415866914351fdfc3E881bb7c2;

  // https://etherscan.io/address/0x4A35FD7F93324Cc48bc12190D3F37493437b1Eff
  address internal constant FBTC_V_TOKEN = 0x4A35FD7F93324Cc48bc12190D3F37493437b1Eff;

  // https://etherscan.io/address/0xb41E773f507F7a7EA890b1afB7d2b660c30C8B0A
  address internal constant FBTC_ORACLE = 0xb41E773f507F7a7EA890b1afB7d2b660c30C8B0A;

  // https://etherscan.io/address/0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB
  address internal constant FBTC_INTEREST_RATE_STRATEGY =
    0x9ec6F08190DeA04A54f8Afc53Db96134e5E3FdFB;
}
library AaveV3EthereumEModes {
  uint8 internal constant NONE = 0;

  uint8 internal constant ETH_CORRELATED = 1;

  uint8 internal constant SUSDE_STABLECOINS = 2;

  uint8 internal constant RSETH_LST_MAIN = 3;

  uint8 internal constant LBTC_WBTC = 4;

  uint8 internal constant LBTC_CBBTC = 5;

  uint8 internal constant LBTC_TBTC = 6;

  uint8 internal constant EBTC_WBTC = 7;

  uint8 internal constant PT_SUSDE_STABLECOINS_JUL_2025 = 8;

  uint8 internal constant PT_EUSDE_STABLECOINS_MAY_2025 = 9;

  uint8 internal constant PT_USDE_STABLECOINS_JULY_2025 = 10;

  uint8 internal constant USDE_STABLECOIN = 11;

  uint8 internal constant PT_USDE_USDE_JULY_2025 = 12;

  uint8 internal constant PT_EUSDE_STABLECOINS_AUGUST_2025 = 13;

  uint8 internal constant PT_EUSDE_USDE_AUGUST_2025 = 14;

  uint8 internal constant EUSDE_STABLECOIN = 15;

  uint8 internal constant FBTC_WBTC = 16;
}
library AaveV3EthereumExternalLibraries {
  // https://etherscan.io/address/0x34039100cc9584Ae5D741d322e16d0d18CEE8770
  address internal constant FLASHLOAN_LOGIC = 0x34039100cc9584Ae5D741d322e16d0d18CEE8770;

  // https://etherscan.io/address/0x62325c94E1c49dcDb5937726aB5D8A4c37bCAd36
  address internal constant BORROW_LOGIC = 0x62325c94E1c49dcDb5937726aB5D8A4c37bCAd36;

  // https://etherscan.io/address/0x621Ef86D8A5C693a06295BC288B95C12D4CE4994
  address internal constant BRIDGE_LOGIC = 0x621Ef86D8A5C693a06295BC288B95C12D4CE4994;

  // https://etherscan.io/address/0xC31d2362fAeD85dF79d0bec99693D0EB0Abd3f74
  address internal constant E_MODE_LOGIC = 0xC31d2362fAeD85dF79d0bec99693D0EB0Abd3f74;

  // https://etherscan.io/address/0x4731bF01583F991278692E8727d0700a00A1fBBf
  address internal constant LIQUIDATION_LOGIC = 0x4731bF01583F991278692E8727d0700a00A1fBBf;

  // https://etherscan.io/address/0xf8C97539934ee66a67C26010e8e027D77E821B0C
  address internal constant POOL_LOGIC = 0xf8C97539934ee66a67C26010e8e027D77E821B0C;

  // https://etherscan.io/address/0x185477906B46D9b8DE0DEB73A1bBfb87b5b51BC3
  address internal constant SUPPLY_LOGIC = 0x185477906B46D9b8DE0DEB73A1bBfb87b5b51BC3;
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
    uint256 amount;
    uint256 balanceFromBefore;
    uint256 balanceToBefore;
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
    uint256 amount;
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

// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.10;

import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {Address} from '../../../dependencies/openzeppelin/contracts/Address.sol';
import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {IAToken} from '../../../interfaces/IAToken.sol';
import {IPool} from '../../../interfaces/IPool.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {Errors} from '../helpers/Errors.sol';
import {WadRayMath} from '../math/WadRayMath.sol';
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
  using WadRayMath for uint256;
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
        uint256 amountToMint = accruedToTreasury.rayMul(normalizedIncome);
        IAToken(reserve.aTokenAddress).mintToTreasury(amountToMint, normalizedIncome);

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

import {GPv2SafeERC20} from '../../../dependencies/gnosis/contracts/GPv2SafeERC20.sol';
import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {IPool} from '../../../interfaces/IPool.sol';
import {UserConfiguration} from '../configuration/UserConfiguration.sol';
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

import {IERC20} from '../../../dependencies/openzeppelin/contracts/IERC20.sol';
import {IScaledBalanceToken} from '../../../interfaces/IScaledBalanceToken.sol';
import {IPriceOracleGetter} from '../../../interfaces/IPriceOracleGetter.sol';
import {ReserveConfiguration} from '../configuration/ReserveConfiguration.sol';
import {UserConfiguration} from '../configuration/UserConfiguration.sol';
import {EModeConfiguration} from '../configuration/EModeConfiguration.sol';
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
    // fetching variable debt
    uint256 userTotalDebt = IScaledBalanceToken(reserve.variableDebtTokenAddress).scaledBalanceOf(
      user
    );
    if (userTotalDebt == 0) {
      return 0;
    }

    userTotalDebt = userTotalDebt.rayMul(reserve.getNormalizedDebt()) * assetPrice;
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

enum DelegationMode {
  NO_DELEGATION,
  VOTING_DELEGATED,
  PROPOSITION_DELEGATED,
  FULL_POWER_DELEGATED
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IReserveInterestRateStrategy} from './IReserveInterestRateStrategy.sol';
import {IPoolAddressesProvider} from './IPoolAddressesProvider.sol';

/**
 * @title IDefaultInterestRateStrategyV2
 * @author BGD Labs
 * @notice Interface of the default interest rate strategy used by the Aave protocol
 */
interface IDefaultInterestRateStrategyV2 is IReserveInterestRateStrategy {
  /**
   * @notice Holds the interest rate data for a given reserve
   *
   * @dev Since values are in bps, they are multiplied by 1e23 in order to become rays with 27 decimals. This
   * in turn means that the maximum supported interest rate is 4294967295 (2**32-1) bps or 42949672.95%.
   *
   * @param optimalUsageRatio The optimal usage ratio, in bps
   * @param baseVariableBorrowRate The base variable borrow rate, in bps
   * @param variableRateSlope1 The slope of the variable interest curve, before hitting the optimal ratio, in bps
   * @param variableRateSlope2 The slope of the variable interest curve, after hitting the optimal ratio, in bps
   */
  struct InterestRateData {
    uint16 optimalUsageRatio;
    uint32 baseVariableBorrowRate;
    uint32 variableRateSlope1;
    uint32 variableRateSlope2;
  }

  /**
   * @notice The interest rate data, where all values are in ray (fixed-point 27 decimal numbers) for a given reserve,
   * used in in-memory calculations.
   *
   * @param optimalUsageRatio The optimal usage ratio
   * @param baseVariableBorrowRate The base variable borrow rate
   * @param variableRateSlope1 The slope of the variable interest curve, before hitting the optimal ratio
   * @param variableRateSlope2 The slope of the variable interest curve, after hitting the optimal ratio
   */
  struct InterestRateDataRay {
    uint256 optimalUsageRatio;
    uint256 baseVariableBorrowRate;
    uint256 variableRateSlope1;
    uint256 variableRateSlope2;
  }

  /**
   * @notice emitted when new interest rate data is set in a reserve
   *
   * @param reserve address of the reserve that has new interest rate data set
   * @param optimalUsageRatio The optimal usage ratio, in bps
   * @param baseVariableBorrowRate The base variable borrow rate, in bps
   * @param variableRateSlope1 The slope of the variable interest curve, before hitting the optimal ratio, in bps
   * @param variableRateSlope2 The slope of the variable interest curve, after hitting the optimal ratio, in bps
   */
  event RateDataUpdate(
    address indexed reserve,
    uint256 optimalUsageRatio,
    uint256 baseVariableBorrowRate,
    uint256 variableRateSlope1,
    uint256 variableRateSlope2
  );

  /**
   * @notice Returns the address of the PoolAddressesProvider
   * @return The address of the PoolAddressesProvider contract
   */
  function ADDRESSES_PROVIDER() external view returns (IPoolAddressesProvider);

  /**
   * @notice Returns the maximum value achievable for variable borrow rate, in bps
   * @return The maximum rate
   */
  function MAX_BORROW_RATE() external view returns (uint256);

  /**
   * @notice Returns the minimum optimal point, in bps
   * @return The optimal point
   */
  function MIN_OPTIMAL_POINT() external view returns (uint256);

  /**
   * @notice Returns the maximum optimal point, in bps
   * @return The optimal point
   */
  function MAX_OPTIMAL_POINT() external view returns (uint256);

  /**
   * notice Returns the full InterestRateData object for the given reserve, in ray
   *
   * @param reserve The reserve to get the data of
   *
   * @return The InterestRateDataRay object for the given reserve
   */
  function getInterestRateData(address reserve) external view returns (InterestRateDataRay memory);

  /**
   * notice Returns the full InterestRateDataRay object for the given reserve, in bps
   *
   * @param reserve The reserve to get the data of
   *
   * @return The InterestRateData object for the given reserve
   */
  function getInterestRateDataBps(address reserve) external view returns (InterestRateData memory);

  /**
   * @notice Returns the optimal usage rate for the given reserve in ray
   *
   * @param reserve The reserve to get the optimal usage rate of
   *
   * @return The optimal usage rate is the level of borrow / collateral at which the borrow rate
   */
  function getOptimalUsageRatio(address reserve) external view returns (uint256);

  /**
   * @notice Returns the variable rate slope below optimal usage ratio in ray
   * @dev It's the variable rate when usage ratio > 0 and <= OPTIMAL_USAGE_RATIO
   *
   * @param reserve The reserve to get the variable rate slope 1 of
   *
   * @return The variable rate slope
   */
  function getVariableRateSlope1(address reserve) external view returns (uint256);

  /**
   * @notice Returns the variable rate slope above optimal usage ratio in ray
   * @dev It's the variable rate when usage ratio > OPTIMAL_USAGE_RATIO
   *
   * @param reserve The reserve to get the variable rate slope 2 of
   *
   * @return The variable rate slope
   */
  function getVariableRateSlope2(address reserve) external view returns (uint256);

  /**
   * @notice Returns the base variable borrow rate, in ray
   *
   * @param reserve The reserve to get the base variable borrow rate of
   *
   * @return The base variable borrow rate
   */
  function getBaseVariableBorrowRate(address reserve) external view returns (uint256);

  /**
   * @notice Returns the maximum variable borrow rate, in ray
   *
   * @param reserve The reserve to get the maximum variable borrow rate of
   *
   * @return The maximum variable borrow rate
   */
  function getMaxVariableBorrowRate(address reserve) external view returns (uint256);

  /**
   * @notice Sets interest rate data for an Aave rate strategy
   * @param reserve The reserve to update
   * @param rateData The reserve interest rate data to apply to the given reserve
   *   Being specific to this custom implementation, with custom struct type,
   *   overloading the function on the generic interface
   */
  function setInterestRateParams(address reserve, InterestRateData calldata rateData) external;
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {IERC20} from 'openzeppelin-contracts/contracts/token/ERC20/IERC20.sol';

interface ICollector {
  struct Stream {
    uint256 deposit;
    uint256 ratePerSecond;
    uint256 remainingBalance;
    uint256 startTime;
    uint256 stopTime;
    address recipient;
    address sender;
    address tokenAddress;
    bool isEntity;
  }

  /**
   * @dev Withdraw amount exceeds available balance
   */
  error BalanceExceeded();

  /**
   * @dev Deposit smaller than time delta
   */
  error DepositSmallerTimeDelta();

  /**
   * @dev Deposit not multiple of time delta
   */
  error DepositNotMultipleTimeDelta();

  /**
   * @dev Recipient cannot be the contract itself or msg.sender
   */
  error InvalidRecipient();

  /**
   * @dev Start time cannot be before block.timestamp
   */
  error InvalidStartTime();

  /**
   * @dev Stop time must be greater than startTime
   */
  error InvalidStopTime();

  /**
   * @dev Provided address cannot be the zero-address
   */
  error InvalidZeroAddress();

  /**
   * @dev Amount cannot be zero
   */
  error InvalidZeroAmount();

  /**
   * @dev Only caller with FUNDS_ADMIN role can call
   */
  error OnlyFundsAdmin();

  /**
   * @dev Only caller with FUNDS_ADMIN role or stream recipient can call
   */
  error OnlyFundsAdminOrRecipient();

  /**
   * @dev The provided ID does not belong to an existing stream
   */
  error StreamDoesNotExist();

  /** @notice Emitted when the new stream is created
   * @param streamId The identifier of the stream.
   * @param sender The address of the collector.
   * @param recipient The address towards which the money is streamed.
   * @param deposit The amount of money to be streamed.
   * @param tokenAddress The ERC20 token to use as streaming currency.
   * @param startTime The unix timestamp for when the stream starts.
   * @param stopTime The unix timestamp for when the stream stops.
   **/
  event CreateStream(
    uint256 indexed streamId,
    address indexed sender,
    address indexed recipient,
    uint256 deposit,
    address tokenAddress,
    uint256 startTime,
    uint256 stopTime
  );

  /**
   * @notice Emmitted when withdraw happens from the contract to the recipient's account.
   * @param streamId The id of the stream to withdraw tokens from.
   * @param recipient The address towards which the money is streamed.
   * @param amount The amount of tokens to withdraw.
   */
  event WithdrawFromStream(uint256 indexed streamId, address indexed recipient, uint256 amount);

  /**
   * @notice Emmitted when the stream is canceled.
   * @param streamId The id of the stream to withdraw tokens from.
   * @param sender The address of the collector.
   * @param recipient The address towards which the money is streamed.
   * @param senderBalance The sender's balance at the moment of cancelling.
   * @param recipientBalance The recipient's balance at the moment of cancelling.
   */
  event CancelStream(
    uint256 indexed streamId,
    address indexed sender,
    address indexed recipient,
    uint256 senderBalance,
    uint256 recipientBalance
  );

  /**
   * @notice FUNDS_ADMIN role granted by ACL Manager
   **/
  function FUNDS_ADMIN_ROLE() external view returns (bytes32);

  /** @notice Returns the mock ETH reference address
   * @return address The address
   **/
  function ETH_MOCK_ADDRESS() external pure returns (address);

  /**
   * @notice Checks if address is funds admin
   * @return bool If the address has the funds admin role
   **/
  function isFundsAdmin(address admin) external view returns (bool);

  /**
   * @notice Returns the available funds for the given stream id and address.
   * @param streamId The id of the stream for which to query the balance.
   * @param who The address for which to query the balance.
   * @notice Returns the total funds allocated to `who` as uint256.
   **/
  function balanceOf(uint256 streamId, address who) external view returns (uint256 balance);

  /**
   * @dev Function for the funds admin to give ERC20 allowance to other parties
   * @param token The address of the token to give allowance from
   * @param recipient Allowance's recipient
   * @param amount Allowance to approve
   **/
  function approve(IERC20 token, address recipient, uint256 amount) external;

  /**
   * @notice Function for the funds admin to transfer ERC20 tokens to other parties
   * @param token The address of the token to transfer
   * @param recipient Transfer's recipient
   * @param amount Amount to transfer
   **/
  function transfer(IERC20 token, address recipient, uint256 amount) external;

  /**
   * @notice Creates a new stream funded by this contracts itself and paid towards `recipient`.
   * @param recipient The address towards which the money is streamed.
   * @param deposit The amount of money to be streamed.
   * @param tokenAddress The ERC20 token to use as streaming currency.
   * @param startTime The unix timestamp for when the stream starts.
   * @param stopTime The unix timestamp for when the stream stops.
   * @return streamId the uint256 id of the newly created stream.
   */
  function createStream(
    address recipient,
    uint256 deposit,
    address tokenAddress,
    uint256 startTime,
    uint256 stopTime
  ) external returns (uint256 streamId);

  /**
   * @notice Returns the stream with all its properties.
   * @dev Throws if the id does not point to a valid stream.
   * @param streamId The id of the stream to query.
   * @notice Returns the stream object.
   */
  function getStream(
    uint256 streamId
  )
    external
    view
    returns (
      address sender,
      address recipient,
      uint256 deposit,
      address tokenAddress,
      uint256 startTime,
      uint256 stopTime,
      uint256 remainingBalance,
      uint256 ratePerSecond
    );

  /**
   * @notice Withdraws from the contract to the recipient's account.
   * @param streamId The id of the stream to withdraw tokens from.
   * @param amount The amount of tokens to withdraw.
   * @return bool Returns true if successful.
   */
  function withdrawFromStream(uint256 streamId, uint256 amount) external returns (bool);

  /**
   * @notice Cancels the stream and transfers the tokens back on a pro rata basis.
   * @param streamId The id of the stream to cancel.
   * @return bool Returns true if successful.
   */
  function cancelStream(uint256 streamId) external returns (bool);

  /**
   * @notice Returns the next available stream id
   * @return nextStreamId Returns the stream id.
   */
  function getNextStreamId() external view returns (uint256);
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

// SPDX-License-Identifier: BUSL-1.1
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
pragma solidity ^0.8.0;

import {IPriceOracleGetter} from './IPriceOracleGetter.sol';
import {IPoolAddressesProvider} from './IPoolAddressesProvider.sol';

/**
 * @title IAaveOracle
 * @author Aave
 * @notice Defines the basic interface for the Aave Oracle
 */
interface IAaveOracle is IPriceOracleGetter {
  /**
   * @dev Emitted after the base currency is set
   * @param baseCurrency The base currency of used for price quotes
   * @param baseCurrencyUnit The unit of the base currency
   */
  event BaseCurrencySet(address indexed baseCurrency, uint256 baseCurrencyUnit);

  /**
   * @dev Emitted after the price source of an asset is updated
   * @param asset The address of the asset
   * @param source The price source of the asset
   */
  event AssetSourceUpdated(address indexed asset, address indexed source);

  /**
   * @dev Emitted after the address of fallback oracle is updated
   * @param fallbackOracle The address of the fallback oracle
   */
  event FallbackOracleUpdated(address indexed fallbackOracle);

  /**
   * @notice Returns the PoolAddressesProvider
   * @return The address of the PoolAddressesProvider contract
   */
  function ADDRESSES_PROVIDER() external view returns (IPoolAddressesProvider);

  /**
   * @notice Sets or replaces price sources of assets
   * @param assets The addresses of the assets
   * @param sources The addresses of the price sources
   */
  function setAssetSources(address[] calldata assets, address[] calldata sources) external;

  /**
   * @notice Sets the fallback oracle
   * @param fallbackOracle The address of the fallback oracle
   */
  function setFallbackOracle(address fallbackOracle) external;

  /**
   * @notice Returns a list of prices from a list of assets addresses
   * @param assets The list of assets addresses
   * @return The prices of the given assets
   */
  function getAssetsPrices(address[] calldata assets) external view returns (uint256[] memory);

  /**
   * @notice Returns the address of the source for an asset address
   * @param asset The address of the asset
   * @return The address of the source
   */
  function getSourceOfAsset(address asset) external view returns (address);

  /**
   * @notice Returns the address of the fallback oracle
   * @return The address of the fallback oracle
   */
  function getFallbackOracle() external view returns (address);
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
  error CollateralBalanceIsZero(); // 'The collateral balance is 0'
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
   * @param index The next liquidity index of the reserve
   */
  function transferOnLiquidation(address from, address to, uint256 value, uint256 index) external;

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
   * @return The scaled total debt of the reserve
   */
  function mint(
    address user,
    address onBehalfOf,
    uint256 amount,
    uint256 index
  ) external returns (uint256);

  /**
   * @notice Burns user variable debt
   * @dev In some instances, a burn transaction will emit a mint event
   * if the amount to burn is less than the interest that the user accrued
   * @param from The address from which the debt will be burned
   * @param amount The amount getting burned
   * @param index The variable debt index of the reserve
   * @return True if the new balance is zero
   * @return The scaled total debt of the reserve
   */
  function burn(address from, uint256 amount, uint256 index) external returns (bool, uint256);

  /**
   * @notice Returns the address of the underlying asset of this debtToken (E.g. WETH for variableDebtWETH)
   * @return The address of the underlying asset
   */
  function UNDERLYING_ASSET_ADDRESS() external view returns (address);
}

// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

library ConfiguratorInputTypes {
  struct InitReserveInput {
    address aTokenImpl;
    address variableDebtTokenImpl;
    address underlyingAsset;
    string aTokenName;
    string aTokenSymbol;
    string variableDebtTokenName;
    string variableDebtTokenSymbol;
    bytes params;
    bytes interestRateData;
  }

  struct UpdateATokenInput {
    address asset;
    string name;
    string symbol;
    address implementation;
    bytes params;
  }

  struct UpdateDebtTokenInput {
    address asset;
    string name;
    string symbol;
    address implementation;
    bytes params;
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
// Chainlink Contracts v0.8
pragma solidity ^0.8.0;

interface AggregatorInterface {
  function decimals() external view returns (uint8);

  function description() external view returns (string memory);

  function getRoundData(
    uint80 _roundId
  )
    external
    view
    returns (
      uint80 roundId,
      int256 answer,
      uint256 startedAt,
      uint256 updatedAt,
      uint80 answeredInRound
    );

  function latestRoundData()
    external
    view
    returns (
      uint80 roundId,
      int256 answer,
      uint256 startedAt,
      uint256 updatedAt,
      uint80 answeredInRound
    );

  function latestAnswer() external view returns (int256);

  function latestTimestamp() external view returns (uint256);

  function latestRound() external view returns (uint256);

  function getAnswer(uint256 roundId) external view returns (int256);

  function getTimestamp(uint256 roundId) external view returns (uint256);

  event AnswerUpdated(int256 indexed current, uint256 indexed roundId, uint256 updatedAt);

  event NewRound(uint256 indexed roundId, address indexed startedBy, uint256 startedAt);
}

// SPDX-License-Identifier: MIT
// OpenZeppelin Contracts (last updated v5.1.0) (token/ERC20/IERC20.sol)

pragma solidity ^0.8.20;

/**
 * @dev Interface of the ERC-20 standard as defined in the ERC.
 */
interface IERC20 {
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

    /**
     * @dev Returns the value of tokens in existence.
     */
    function totalSupply() external view returns (uint256);

    /**
     * @dev Returns the value of tokens owned by `account`.
     */
    function balanceOf(address account) external view returns (uint256);

    /**
     * @dev Moves a `value` amount of tokens from the caller's account to `to`.
     *
     * Returns a boolean value indicating whether the operation succeeded.
     *
     * Emits a {Transfer} event.
     */
    function transfer(address to, uint256 value) external returns (bool);

    /**
     * @dev Returns the remaining number of tokens that `spender` will be
     * allowed to spend on behalf of `owner` through {transferFrom}. This is
     * zero by default.
     *
     * This value changes when {approve} or {transferFrom} are called.
     */
    function allowance(address owner, address spender) external view returns (uint256);

    /**
     * @dev Sets a `value` amount of tokens as the allowance of `spender` over the
     * caller's tokens.
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
    function approve(address spender, uint256 value) external returns (bool);

    /**
     * @dev Moves a `value` amount of tokens from `from` to `to` using the
     * allowance mechanism. `value` is then deducted from the caller's
     * allowance.
     *
     * Returns a boolean value indicating whether the operation succeeded.
     *
     * Emits a {Transfer} event.
     */
    function transferFrom(address from, address to, uint256 value) external returns (bool);
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
}

