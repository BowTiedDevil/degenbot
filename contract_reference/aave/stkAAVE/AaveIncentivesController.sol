// SPDX-License-Identifier: agpl-3.0
pragma solidity 0.6.12;
pragma experimental ABIEncoderV2;

import {DistributionTypes} from '../lib/DistributionTypes.sol';

import {IERC20} from '../interfaces/IERC20.sol';
import {IAToken} from '../interfaces/IAToken.sol';
import {IAaveIncentivesController} from '../interfaces/IAaveIncentivesController.sol';
import {IStakedAave} from '../interfaces/IStakedAave.sol';
import {VersionedInitializable} from '../utils/VersionedInitializable.sol';
import {AaveDistributionManager} from './AaveDistributionManager.sol';

/**
 * @title AaveIncentivesController
 * @notice Distributor contract for rewards to the Aave protocol
 * @author Aave
 **/
contract AaveIncentivesController is
  IAaveIncentivesController,
  VersionedInitializable,
  AaveDistributionManager
{
  uint256 public constant REVISION = 1;

  IStakedAave public immutable PSM;

  IERC20 public immutable REWARD_TOKEN;
  address public immutable REWARDS_VAULT;
  uint256 public immutable EXTRA_PSM_REWARD;

  mapping(address => uint256) internal _usersUnclaimedRewards;

  event RewardsAccrued(address indexed user, uint256 amount);
  event RewardsClaimed(address indexed user, address indexed to, uint256 amount);

  constructor(
    IERC20 rewardToken,
    address rewardsVault,
    IStakedAave psm,
    uint256 extraPsmReward,
    address emissionManager,
    uint128 distributionDuration
  ) public AaveDistributionManager(emissionManager, distributionDuration) {
    REWARD_TOKEN = rewardToken;
    REWARDS_VAULT = rewardsVault;
    PSM = psm;
    EXTRA_PSM_REWARD = extraPsmReward;
  }

  /**
   * @dev Called by the proxy contract. Not used at the moment, but for the future
   **/
  function initialize() external initializer {
    // to unlock possibility to stake on behalf of the user
    REWARD_TOKEN.approve(address(PSM), type(uint256).max);
  }

  /**
   * @dev Called by the corresponding asset on any update that affects the rewards distribution
   * @param user The address of the user
   * @param userBalance The balance of the user of the asset in the lending pool
   * @param totalSupply The total supply of the asset in the lending pool
   **/
  function handleAction(
    address user,
    uint256 userBalance,
    uint256 totalSupply
  ) external override {
    uint256 accruedRewards = _updateUserAssetInternal(user, msg.sender, userBalance, totalSupply);
    if (accruedRewards != 0) {
      _usersUnclaimedRewards[user] = _usersUnclaimedRewards[user].add(accruedRewards);
      emit RewardsAccrued(user, accruedRewards);
    }
  }

  /**
   * @dev Returns the total of rewards of an user, already accrued + not yet accrued
   * @param user The address of the user
   * @return The rewards
   **/
  function getRewardsBalance(address[] calldata assets, address user)
    external
    override
    view
    returns (uint256)
  {
    uint256 unclaimedRewards = _usersUnclaimedRewards[user];

    DistributionTypes.UserStakeInput[] memory userState = new DistributionTypes.UserStakeInput[](
      assets.length
    );
    for (uint256 i = 0; i < assets.length; i++) {
      userState[i].underlyingAsset = assets[i];
      (userState[i].stakedByUser, userState[i].totalStaked) = IAToken(assets[i])
        .getScaledUserBalanceAndSupply(user);
    }
    unclaimedRewards = unclaimedRewards.add(_getUnclaimedRewards(user, userState));
    return unclaimedRewards;
  }

  /**
   * @dev Claims reward for an user, on all the assets of the lending pool, accumulating the pending rewards
   * @param amount Amount of rewards to claim
   * @param to Address that will be receiving the rewards
   * @param stake Boolean flag to determined if the claimed rewards should be staked in the Safety Module or not
   * @return Rewards claimed
   **/
  function claimRewards(
    address[] calldata assets,
    uint256 amount,
    address to,
    bool stake
  ) external override returns (uint256) {
    if (amount == 0) {
      return 0;
    }
    address user = msg.sender;
    uint256 unclaimedRewards = _usersUnclaimedRewards[user];

    DistributionTypes.UserStakeInput[] memory userState = new DistributionTypes.UserStakeInput[](
      assets.length
    );
    for (uint256 i = 0; i < assets.length; i++) {
      userState[i].underlyingAsset = assets[i];
      (userState[i].stakedByUser, userState[i].totalStaked) = IAToken(assets[i])
        .getScaledUserBalanceAndSupply(user);
    }

    uint256 accruedRewards = _claimRewards(user, userState);
    if (accruedRewards != 0) {
      unclaimedRewards = unclaimedRewards.add(accruedRewards);
      emit RewardsAccrued(user, accruedRewards);
    }

    if (unclaimedRewards == 0) {
      return 0;
    }

    uint256 amountToClaim = amount > unclaimedRewards ? unclaimedRewards : amount;
    _usersUnclaimedRewards[user] = unclaimedRewards - amountToClaim; // Safe due to the previous line

    if (stake) {
      amountToClaim = amountToClaim.add(amountToClaim.mul(EXTRA_PSM_REWARD).div(100));
      REWARD_TOKEN.transferFrom(REWARDS_VAULT, address(this), amountToClaim);
      PSM.stake(to, amountToClaim);
    } else {
      REWARD_TOKEN.transferFrom(REWARDS_VAULT, to, amountToClaim);
    }
    emit RewardsClaimed(msg.sender, to, amountToClaim);

    return amountToClaim;
  }

  /**
   * @dev returns the unclaimed rewards of the user
   * @param _user the address of the user
   * @return the unclaimed user rewards
   */
  function getUserUnclaimedRewards(address _user) external view returns (uint256) {
    return _usersUnclaimedRewards[_user];
  }

  /**
   * @dev returns the revision of the implementation contract
   */
  function getRevision() internal override pure returns (uint256) {
    return REVISION;
  }
}
