import enum
import json
from fractions import Fraction

import pytest
from hexbytes import HexBytes

from degenbot.anvil_fork import AnvilFork
from degenbot.balancer.deployments import (
    BALANCER_V2_VAULT_ADDRESS,
    BALANCERQUERIES_CONTRACT_ADDRESS,
)
from degenbot.balancer.pools import BalancerV2Pool, detect_pow_version
from degenbot.checksum_cache import get_checksum_address
from degenbot.provider import ProviderAdapter
from tests.helpers.bot_factory import make_bot_with_provider

# ---------- Pool Addresses ----------

# WETH/BAL 80/20 WeightedPool2Tokens
# Both tokens are 18-decimal. Fee = 1%.
BALANCER_V2_WETH_BAL_POOL_ADDRESS = get_checksum_address(
    "0x5c6Ee304399DBdB9C8Ef030aB642B10820DB8F56"
)
BALANCER_V2_WETH_BAL_POOL_ID = HexBytes(
    "0x5c6ee304399dbdb9c8ef030ab642b10820db8f56000200000000000000000014"
)

# USDC/WETH 50/50 WeightedPool2Tokens
# USDC is 6-decimal, WETH is 18-decimal. Fee = 0.09%.
BALANCER_V2_USDC_WETH_POOL_ADDRESS = get_checksum_address(
    "0x3e5fa9518ea95c3e533eb377c001702a9aacaa32"
)
BALANCER_V2_USDC_WETH_POOL_ID = HexBytes(
    "0x3e5fa9518ea95c3e533eb377c001702a9aacaa32000200000000000000000052"
)

# WETH/RPL 80/20 WeightedPool2Tokens
# Both tokens are 18-decimal. Fee = 0.15%.
BALANCER_V2_WETH_RPL_POOL_ADDRESS = get_checksum_address(
    "0xff083f57a556bfb3bbe46ea1b4fa154b2b1fbe88"
)
BALANCER_V2_WETH_RPL_POOL_ID = HexBytes(
    "0xff083f57a556bfb3bbe46ea1b4fa154b2b1fbe88000200000000000000000030"
)

# ---------- Shared ABIs & Contract Addresses ----------

BALANCER_V2_WETH_BAL_POOL_ABI = json.loads(
    """
    [{"inputs":[{"components":[{"internalType":"contract IVault","name":"vault","type":"address"},{"internalType":"string","name":"name","type":"string"},{"internalType":"string","name":"symbol","type":"string"},{"internalType":"contract IERC20","name":"token0","type":"address"},{"internalType":"contract IERC20","name":"token1","type":"address"},{"internalType":"uint256","name":"normalizedWeight0","type":"uint256"},{"internalType":"uint256","name":"normalizedWeight1","type":"uint256"},{"internalType":"uint256","name":"swapFeePercentage","type":"uint256"},{"internalType":"uint256","name":"pauseWindowDuration","type":"uint256"},{"internalType":"uint256","name":"bufferPeriodDuration","type":"uint256"},{"internalType":"bool","name":"oracleEnabled","type":"bool"},{"internalType":"address","name":"owner","type":"address"}],"internalType":"struct WeightedPool2Tokens.NewPoolParams","name":"params","type":"tuple"}],"stateMutability":"nonpayable","type":"constructor"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"owner","type":"address"},{"indexed":true,"internalType":"address","name":"spender","type":"address"},{"indexed":false,"internalType":"uint256","name":"value","type":"uint256"}],"name":"Approval","type":"event"},{"anonymous":false,"inputs":[{"indexed":false,"internalType":"bool","name":"enabled","type":"bool"}],"name":"OracleEnabledChanged","type":"event"},{"anonymous":false,"inputs":[{"indexed":false,"internalType":"bool","name":"paused","type":"bool"}],"name":"PausedStateChanged","type":"event"},{"anonymous":false,"inputs":[{"indexed":false,"internalType":"uint256","name":"swapFeePercentage","type":"uint256"}],"name":"SwapFeePercentageChanged","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"from","type":"address"},{"indexed":true,"internalType":"address","name":"to","type":"address"},{"indexed":false,"internalType":"uint256","name":"value","type":"uint256"}],"name":"Transfer","type":"event"},{"inputs":[],"name":"DOMAIN_SEPARATOR","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"owner","type":"address"},{"internalType":"address","name":"spender","type":"address"}],"name":"allowance","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"spender","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"}],"name":"approve","outputs":[{"internalType":"bool","name":"","type":"bool"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"address","name":"account","type":"address"}],"name":"balanceOf","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"decimals","outputs":[{"internalType":"uint8","name":"","type":"uint8"}],"stateMutability":"pure","type":"function"},{"inputs":[{"internalType":"address","name":"spender","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"}],"name":"decreaseApproval","outputs":[{"internalType":"bool","name":"","type":"bool"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[],"name":"enableOracle","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes4","name":"selector","type":"bytes4"}],"name":"getActionId","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getAuthorizer","outputs":[{"internalType":"contract IAuthorizer","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getInvariant","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getLargestSafeQueryWindow","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"pure","type":"function"},{"inputs":[],"name":"getLastInvariant","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"enum IPriceOracle.Variable","name":"variable","type":"uint8"}],"name":"getLatest","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getMiscData","outputs":[{"internalType":"int256","name":"logInvariant","type":"int256"},{"internalType":"int256","name":"logTotalSupply","type":"int256"},{"internalType":"uint256","name":"oracleSampleCreationTimestamp","type":"uint256"},{"internalType":"uint256","name":"oracleIndex","type":"uint256"},{"internalType":"bool","name":"oracleEnabled","type":"bool"},{"internalType":"uint256","name":"swapFeePercentage","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getNormalizedWeights","outputs":[{"internalType":"uint256[]","name":"","type":"uint256[]"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getOwner","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"components":[{"internalType":"enum IPriceOracle.Variable","name":"variable","type":"uint8"},{"internalType":"uint256","name":"ago","type":"uint256"}],"internalType":"struct IPriceOracle.OracleAccumulatorQuery[]","name":"queries","type":"tuple[]"}],"name":"getPastAccumulators","outputs":[{"internalType":"int256[]","name":"results","type":"int256[]"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getPausedState","outputs":[{"internalType":"bool","name":"paused","type":"bool"},{"internalType":"uint256","name":"pauseWindowEndTime","type":"uint256"},{"internalType":"uint256","name":"bufferPeriodEndTime","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getPoolId","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getRate","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"uint256","name":"index","type":"uint256"}],"name":"getSample","outputs":[{"internalType":"int256","name":"logPairPrice","type":"int256"},{"internalType":"int256","name":"accLogPairPrice","type":"int256"},{"internalType":"int256","name":"logBptPrice","type":"int256"},{"internalType":"int256","name":"accLogBptPrice","type":"int256"},{"internalType":"int256","name":"logInvariant","type":"int256"},{"internalType":"int256","name":"accLogInvariant","type":"int256"},{"internalType":"uint256","name":"timestamp","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getSwapFeePercentage","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[{"components":[{"internalType":"enum IPriceOracle.Variable","name":"variable","type":"uint8"},{"internalType":"uint256","name":"secs","type":"uint256"},{"internalType":"uint256","name":"ago","type":"uint256"}],"internalType":"struct IPriceOracle.OracleAverageQuery[]","name":"queries","type":"tuple[]"}],"name":"getTimeWeightedAverage","outputs":[{"internalType":"uint256[]","name":"results","type":"uint256[]"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getTotalSamples","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"pure","type":"function"},{"inputs":[],"name":"getVault","outputs":[{"internalType":"contract IVault","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"spender","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"}],"name":"increaseApproval","outputs":[{"internalType":"bool","name":"","type":"bool"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[],"name":"name","outputs":[{"internalType":"string","name":"","type":"string"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"owner","type":"address"}],"name":"nonces","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"address","name":"sender","type":"address"},{"internalType":"address","name":"recipient","type":"address"},{"internalType":"uint256[]","name":"balances","type":"uint256[]"},{"internalType":"uint256","name":"lastChangeBlock","type":"uint256"},{"internalType":"uint256","name":"protocolSwapFeePercentage","type":"uint256"},{"internalType":"bytes","name":"userData","type":"bytes"}],"name":"onExitPool","outputs":[{"internalType":"uint256[]","name":"","type":"uint256[]"},{"internalType":"uint256[]","name":"","type":"uint256[]"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"address","name":"sender","type":"address"},{"internalType":"address","name":"recipient","type":"address"},{"internalType":"uint256[]","name":"balances","type":"uint256[]"},{"internalType":"uint256","name":"lastChangeBlock","type":"uint256"},{"internalType":"uint256","name":"protocolSwapFeePercentage","type":"uint256"},{"internalType":"bytes","name":"userData","type":"bytes"}],"name":"onJoinPool","outputs":[{"internalType":"uint256[]","name":"amountsIn","type":"uint256[]"},{"internalType":"uint256[]","name":"dueProtocolFeeAmounts","type":"uint256[]"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"internalType":"enum IVault.SwapKind","name":"kind","type":"uint8"},{"internalType":"contract IERC20","name":"tokenIn","type":"address"},{"internalType":"contract IERC20","name":"tokenOut","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"},{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"uint256","name":"lastChangeBlock","type":"uint256"},{"internalType":"address","name":"from","type":"address"},{"internalType":"address","name":"to","type":"address"},{"internalType":"bytes","name":"userData","type":"bytes"}],"internalType":"struct IPoolSwapStructs.SwapRequest","name":"request","type":"tuple"},{"internalType":"uint256","name":"balanceTokenIn","type":"uint256"},{"internalType":"uint256","name":"balanceTokenOut","type":"uint256"}],"name":"onSwap","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"address","name":"owner","type":"address"},{"internalType":"address","name":"spender","type":"address"},{"internalType":"uint256","name":"value","type":"uint256"},{"internalType":"uint256","name":"deadline","type":"uint256"},{"internalType":"uint8","name":"v","type":"uint8"},{"internalType":"bytes32","name":"r","type":"bytes32"},{"internalType":"bytes32","name":"s","type":"bytes32"}],"name":"permit","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"address","name":"sender","type":"address"},{"internalType":"address","name":"recipient","type":"address"},{"internalType":"uint256[]","name":"balances","type":"uint256[]"},{"internalType":"uint256","name":"lastChangeBlock","type":"uint256"},{"internalType":"uint256","name":"protocolSwapFeePercentage","type":"uint256"},{"internalType":"bytes","name":"userData","type":"bytes"}],"name":"queryExit","outputs":[{"internalType":"uint256","name":"bptIn","type":"uint256"},{"internalType":"uint256[]","name":"amountsOut","type":"uint256[]"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"address","name":"sender","type":"address"},{"internalType":"address","name":"recipient","type":"address"},{"internalType":"uint256[]","name":"balances","type":"uint256[]"},{"internalType":"uint256","name":"lastChangeBlock","type":"uint256"},{"internalType":"uint256","name":"protocolSwapFeePercentage","type":"uint256"},{"internalType":"bytes","name":"userData","type":"bytes"}],"name":"queryJoin","outputs":[{"internalType":"uint256","name":"bptOut","type":"uint256"},{"internalType":"uint256[]","name":"amountsIn","type":"uint256[]"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bool","name":"paused","type":"bool"}],"name":"setPaused","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"uint256","name":"swapFeePercentage","type":"uint256"}],"name":"setSwapFeePercentage","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[],"name":"symbol","outputs":[{"internalType":"string","name":"","type":"string"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"totalSupply","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"recipient","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"}],"name":"transfer","outputs":[{"internalType":"bool","name":"","type":"bool"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"address","name":"sender","type":"address"},{"internalType":"address","name":"recipient","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"}],"name":"transferFrom","outputs":[{"internalType":"bool","name":"","type":"bool"}],"stateMutability":"nonpayable","type":"function"}]
    """  # noqa: E501
)
BALANCER_V2_VAULT_ABI = json.loads(
    """
    [{"inputs":[{"internalType":"contract IAuthorizer","name":"authorizer","type":"address"},{"internalType":"contract IWETH","name":"weth","type":"address"},{"internalType":"uint256","name":"pauseWindowDuration","type":"uint256"},{"internalType":"uint256","name":"bufferPeriodDuration","type":"uint256"}],"stateMutability":"nonpayable","type":"constructor"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"contract IAuthorizer","name":"newAuthorizer","type":"address"}],"name":"AuthorizerChanged","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"contract IERC20","name":"token","type":"address"},{"indexed":true,"internalType":"address","name":"sender","type":"address"},{"indexed":false,"internalType":"address","name":"recipient","type":"address"},{"indexed":false,"internalType":"uint256","name":"amount","type":"uint256"}],"name":"ExternalBalanceTransfer","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"contract IFlashLoanRecipient","name":"recipient","type":"address"},{"indexed":true,"internalType":"contract IERC20","name":"token","type":"address"},{"indexed":false,"internalType":"uint256","name":"amount","type":"uint256"},{"indexed":false,"internalType":"uint256","name":"feeAmount","type":"uint256"}],"name":"FlashLoan","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"user","type":"address"},{"indexed":true,"internalType":"contract IERC20","name":"token","type":"address"},{"indexed":false,"internalType":"int256","name":"delta","type":"int256"}],"name":"InternalBalanceChanged","type":"event"},{"anonymous":false,"inputs":[{"indexed":false,"internalType":"bool","name":"paused","type":"bool"}],"name":"PausedStateChanged","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"bytes32","name":"poolId","type":"bytes32"},{"indexed":true,"internalType":"address","name":"liquidityProvider","type":"address"},{"indexed":false,"internalType":"contract IERC20[]","name":"tokens","type":"address[]"},{"indexed":false,"internalType":"int256[]","name":"deltas","type":"int256[]"},{"indexed":false,"internalType":"uint256[]","name":"protocolFeeAmounts","type":"uint256[]"}],"name":"PoolBalanceChanged","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"bytes32","name":"poolId","type":"bytes32"},{"indexed":true,"internalType":"address","name":"assetManager","type":"address"},{"indexed":true,"internalType":"contract IERC20","name":"token","type":"address"},{"indexed":false,"internalType":"int256","name":"cashDelta","type":"int256"},{"indexed":false,"internalType":"int256","name":"managedDelta","type":"int256"}],"name":"PoolBalanceManaged","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"bytes32","name":"poolId","type":"bytes32"},{"indexed":true,"internalType":"address","name":"poolAddress","type":"address"},{"indexed":false,"internalType":"enum IVault.PoolSpecialization","name":"specialization","type":"uint8"}],"name":"PoolRegistered","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"relayer","type":"address"},{"indexed":true,"internalType":"address","name":"sender","type":"address"},{"indexed":false,"internalType":"bool","name":"approved","type":"bool"}],"name":"RelayerApprovalChanged","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"bytes32","name":"poolId","type":"bytes32"},{"indexed":true,"internalType":"contract IERC20","name":"tokenIn","type":"address"},{"indexed":true,"internalType":"contract IERC20","name":"tokenOut","type":"address"},{"indexed":false,"internalType":"uint256","name":"amountIn","type":"uint256"},{"indexed":false,"internalType":"uint256","name":"amountOut","type":"uint256"}],"name":"Swap","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"bytes32","name":"poolId","type":"bytes32"},{"indexed":false,"internalType":"contract IERC20[]","name":"tokens","type":"address[]"}],"name":"TokensDeregistered","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"bytes32","name":"poolId","type":"bytes32"},{"indexed":false,"internalType":"contract IERC20[]","name":"tokens","type":"address[]"},{"indexed":false,"internalType":"address[]","name":"assetManagers","type":"address[]"}],"name":"TokensRegistered","type":"event"},{"inputs":[],"name":"WETH","outputs":[{"internalType":"contract IWETH","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"enum IVault.SwapKind","name":"kind","type":"uint8"},{"components":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"uint256","name":"assetInIndex","type":"uint256"},{"internalType":"uint256","name":"assetOutIndex","type":"uint256"},{"internalType":"uint256","name":"amount","type":"uint256"},{"internalType":"bytes","name":"userData","type":"bytes"}],"internalType":"struct IVault.BatchSwapStep[]","name":"swaps","type":"tuple[]"},{"internalType":"contract IAsset[]","name":"assets","type":"address[]"},{"components":[{"internalType":"address","name":"sender","type":"address"},{"internalType":"bool","name":"fromInternalBalance","type":"bool"},{"internalType":"address payable","name":"recipient","type":"address"},{"internalType":"bool","name":"toInternalBalance","type":"bool"}],"internalType":"struct IVault.FundManagement","name":"funds","type":"tuple"},{"internalType":"int256[]","name":"limits","type":"int256[]"},{"internalType":"uint256","name":"deadline","type":"uint256"}],"name":"batchSwap","outputs":[{"internalType":"int256[]","name":"assetDeltas","type":"int256[]"}],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"contract IERC20[]","name":"tokens","type":"address[]"}],"name":"deregisterTokens","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"address","name":"sender","type":"address"},{"internalType":"address payable","name":"recipient","type":"address"},{"components":[{"internalType":"contract IAsset[]","name":"assets","type":"address[]"},{"internalType":"uint256[]","name":"minAmountsOut","type":"uint256[]"},{"internalType":"bytes","name":"userData","type":"bytes"},{"internalType":"bool","name":"toInternalBalance","type":"bool"}],"internalType":"struct IVault.ExitPoolRequest","name":"request","type":"tuple"}],"name":"exitPool","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"contract IFlashLoanRecipient","name":"recipient","type":"address"},{"internalType":"contract IERC20[]","name":"tokens","type":"address[]"},{"internalType":"uint256[]","name":"amounts","type":"uint256[]"},{"internalType":"bytes","name":"userData","type":"bytes"}],"name":"flashLoan","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes4","name":"selector","type":"bytes4"}],"name":"getActionId","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getAuthorizer","outputs":[{"internalType":"contract IAuthorizer","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getDomainSeparator","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"user","type":"address"},{"internalType":"contract IERC20[]","name":"tokens","type":"address[]"}],"name":"getInternalBalance","outputs":[{"internalType":"uint256[]","name":"balances","type":"uint256[]"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"user","type":"address"}],"name":"getNextNonce","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getPausedState","outputs":[{"internalType":"bool","name":"paused","type":"bool"},{"internalType":"uint256","name":"pauseWindowEndTime","type":"uint256"},{"internalType":"uint256","name":"bufferPeriodEndTime","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"}],"name":"getPool","outputs":[{"internalType":"address","name":"","type":"address"},{"internalType":"enum IVault.PoolSpecialization","name":"","type":"uint8"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"contract IERC20","name":"token","type":"address"}],"name":"getPoolTokenInfo","outputs":[{"internalType":"uint256","name":"cash","type":"uint256"},{"internalType":"uint256","name":"managed","type":"uint256"},{"internalType":"uint256","name":"lastChangeBlock","type":"uint256"},{"internalType":"address","name":"assetManager","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"}],"name":"getPoolTokens","outputs":[{"internalType":"contract IERC20[]","name":"tokens","type":"address[]"},{"internalType":"uint256[]","name":"balances","type":"uint256[]"},{"internalType":"uint256","name":"lastChangeBlock","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getProtocolFeesCollector","outputs":[{"internalType":"contract ProtocolFeesCollector","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"user","type":"address"},{"internalType":"address","name":"relayer","type":"address"}],"name":"hasApprovedRelayer","outputs":[{"internalType":"bool","name":"","type":"bool"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"address","name":"sender","type":"address"},{"internalType":"address","name":"recipient","type":"address"},{"components":[{"internalType":"contract IAsset[]","name":"assets","type":"address[]"},{"internalType":"uint256[]","name":"maxAmountsIn","type":"uint256[]"},{"internalType":"bytes","name":"userData","type":"bytes"},{"internalType":"bool","name":"fromInternalBalance","type":"bool"}],"internalType":"struct IVault.JoinPoolRequest","name":"request","type":"tuple"}],"name":"joinPool","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"components":[{"internalType":"enum IVault.PoolBalanceOpKind","name":"kind","type":"uint8"},{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"contract IERC20","name":"token","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"}],"internalType":"struct IVault.PoolBalanceOp[]","name":"ops","type":"tuple[]"}],"name":"managePoolBalance","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"internalType":"enum IVault.UserBalanceOpKind","name":"kind","type":"uint8"},{"internalType":"contract IAsset","name":"asset","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"},{"internalType":"address","name":"sender","type":"address"},{"internalType":"address payable","name":"recipient","type":"address"}],"internalType":"struct IVault.UserBalanceOp[]","name":"ops","type":"tuple[]"}],"name":"manageUserBalance","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"enum IVault.SwapKind","name":"kind","type":"uint8"},{"components":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"uint256","name":"assetInIndex","type":"uint256"},{"internalType":"uint256","name":"assetOutIndex","type":"uint256"},{"internalType":"uint256","name":"amount","type":"uint256"},{"internalType":"bytes","name":"userData","type":"bytes"}],"internalType":"struct IVault.BatchSwapStep[]","name":"swaps","type":"tuple[]"},{"internalType":"contract IAsset[]","name":"assets","type":"address[]"},{"components":[{"internalType":"address","name":"sender","type":"address"},{"internalType":"bool","name":"fromInternalBalance","type":"bool"},{"internalType":"address payable","name":"recipient","type":"address"},{"internalType":"bool","name":"toInternalBalance","type":"bool"}],"internalType":"struct IVault.FundManagement","name":"funds","type":"tuple"}],"name":"queryBatchSwap","outputs":[{"internalType":"int256[]","name":"","type":"int256[]"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"enum IVault.PoolSpecialization","name":"specialization","type":"uint8"}],"name":"registerPool","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"contract IERC20[]","name":"tokens","type":"address[]"},{"internalType":"address[]","name":"assetManagers","type":"address[]"}],"name":"registerTokens","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"contract IAuthorizer","name":"newAuthorizer","type":"address"}],"name":"setAuthorizer","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bool","name":"paused","type":"bool"}],"name":"setPaused","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"address","name":"sender","type":"address"},{"internalType":"address","name":"relayer","type":"address"},{"internalType":"bool","name":"approved","type":"bool"}],"name":"setRelayerApproval","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"enum IVault.SwapKind","name":"kind","type":"uint8"},{"internalType":"contract IAsset","name":"assetIn","type":"address"},{"internalType":"contract IAsset","name":"assetOut","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"},{"internalType":"bytes","name":"userData","type":"bytes"}],"internalType":"struct IVault.SingleSwap","name":"singleSwap","type":"tuple"},{"components":[{"internalType":"address","name":"sender","type":"address"},{"internalType":"bool","name":"fromInternalBalance","type":"bool"},{"internalType":"address payable","name":"recipient","type":"address"},{"internalType":"bool","name":"toInternalBalance","type":"bool"}],"internalType":"struct IVault.FundManagement","name":"funds","type":"tuple"},{"internalType":"uint256","name":"limit","type":"uint256"},{"internalType":"uint256","name":"deadline","type":"uint256"}],"name":"swap","outputs":[{"internalType":"uint256","name":"amountCalculated","type":"uint256"}],"stateMutability":"payable","type":"function"},{"stateMutability":"payable","type":"receive"}]
    """  # noqa: E501
)

BALANCERQUERIES_CONTRACT_ABI = json.loads(
    """
    [{"inputs":[{"internalType":"contract IVault","name":"_vault","type":"address"}],"stateMutability":"nonpayable","type":"constructor"},{"inputs":[{"internalType":"enum IVault.SwapKind","name":"kind","type":"uint8"},{"components":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"uint256","name":"assetInIndex","type":"uint256"},{"internalType":"uint256","name":"assetOutIndex","type":"uint256"},{"internalType":"uint256","name":"amount","type":"uint256"},{"internalType":"bytes","name":"userData","type":"bytes"}],"internalType":"struct IVault.BatchSwapStep[]","name":"swaps","type":"tuple[]"},{"internalType":"contract IAsset[]","name":"assets","type":"address[]"},{"components":[{"internalType":"address","name":"sender","type":"address"},{"internalType":"bool","name":"fromInternalBalance","type":"bool"},{"internalType":"address payable","name":"recipient","type":"address"},{"internalType":"bool","name":"toInternalBalance","type":"bool"}],"internalType":"struct IVault.FundManagement","name":"funds","type":"tuple"}],"name":"queryBatchSwap","outputs":[{"internalType":"int256[]","name":"assetDeltas","type":"int256[]"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"address","name":"sender","type":"address"},{"internalType":"address","name":"recipient","type":"address"},{"components":[{"internalType":"contract IAsset[]","name":"assets","type":"address[]"},{"internalType":"uint256[]","name":"minAmountsOut","type":"uint256[]"},{"internalType":"bytes","name":"userData","type":"bytes"},{"internalType":"bool","name":"toInternalBalance","type":"bool"}],"internalType":"struct IVault.ExitPoolRequest","name":"request","type":"tuple"}],"name":"queryExit","outputs":[{"internalType":"uint256","name":"bptIn","type":"uint256"},{"internalType":"uint256[]","name":"amountsOut","type":"uint256[]"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"address","name":"sender","type":"address"},{"internalType":"address","name":"recipient","type":"address"},{"components":[{"internalType":"contract IAsset[]","name":"assets","type":"address[]"},{"internalType":"uint256[]","name":"maxAmountsIn","type":"uint256[]"},{"internalType":"bytes","name":"userData","type":"bytes"},{"internalType":"bool","name":"fromInternalBalance","type":"bool"}],"internalType":"struct IVault.JoinPoolRequest","name":"request","type":"tuple"}],"name":"queryJoin","outputs":[{"internalType":"uint256","name":"bptOut","type":"uint256"},{"internalType":"uint256[]","name":"amountsIn","type":"uint256[]"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"enum IVault.SwapKind","name":"kind","type":"uint8"},{"internalType":"contract IAsset","name":"assetIn","type":"address"},{"internalType":"contract IAsset","name":"assetOut","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"},{"internalType":"bytes","name":"userData","type":"bytes"}],"internalType":"struct IVault.SingleSwap","name":"singleSwap","type":"tuple"},{"components":[{"internalType":"address","name":"sender","type":"address"},{"internalType":"bool","name":"fromInternalBalance","type":"bool"},{"internalType":"address payable","name":"recipient","type":"address"},{"internalType":"bool","name":"toInternalBalance","type":"bool"}],"internalType":"struct IVault.FundManagement","name":"funds","type":"tuple"}],"name":"querySwap","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[],"name":"vault","outputs":[{"internalType":"contract IVault","name":"","type":"address"}],"stateMutability":"view","type":"function"}]
    """  # noqa: E501
)

VITALIK_ADDRESS = get_checksum_address("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")

# ---------- Shared Pool Construction Helper ----------


class SwapKind(enum.IntEnum):
    GIVEN_IN = 0
    GIVEN_OUT = 1


def _build_pool_from_chain(
    fork: AnvilFork,
    pool_address: str,
) -> BalancerV2Pool:
    """Build a BalancerV2Pool by fetching on-chain data from an anvil fork."""
    bot = make_bot_with_provider(ProviderAdapter.from_web3(fork.w3))

    pool_contract = fork.w3.eth.contract(
        address=get_checksum_address(pool_address),
        abi=BALANCER_V2_WETH_BAL_POOL_ABI,
    )
    vault_contract = fork.w3.eth.contract(
        address=BALANCER_V2_VAULT_ADDRESS,
        abi=BALANCER_V2_VAULT_ABI,
    )

    pool_id = pool_contract.functions.getPoolId().call()
    vault = pool_contract.functions.getVault().call()
    swap_fee = pool_contract.functions.getSwapFeePercentage().call()
    weights = pool_contract.functions.getNormalizedWeights().call()
    tokens_addresses, balances, _ = vault_contract.functions.getPoolTokens(pool_id).call()

    tokens = [bot.build_erc20token(addr) for addr in tokens_addresses]

    # Detect which FixedPoint pow implementation the deployed pool uses
    bytecode = fork.w3.eth.get_code(get_checksum_address(pool_address)).hex()
    pow_version = detect_pow_version(bytecode)

    return BalancerV2Pool(
        address=get_checksum_address(pool_address),
        pool_id=pool_id,
        vault=vault,
        tokens=tokens,
        balances=balances,
        fee=Fraction(swap_fee, 10**18),
        weights=weights,
        pow_version=pow_version,
        chain_id=1,
    )


# ---------- Fixtures ----------


@pytest.fixture
def ethereum_balancer_v2_weth_bal_pool(
    fork_mainnet_full: AnvilFork,
) -> BalancerV2Pool:
    return _build_pool_from_chain(fork_mainnet_full, BALANCER_V2_WETH_BAL_POOL_ADDRESS)


@pytest.fixture
def ethereum_balancer_v2_usdc_weth_pool(
    fork_mainnet_full: AnvilFork,
) -> BalancerV2Pool:
    return _build_pool_from_chain(fork_mainnet_full, BALANCER_V2_USDC_WETH_POOL_ADDRESS)


@pytest.fixture
def ethereum_balancer_v2_weth_rpl_pool(
    fork_mainnet_full: AnvilFork,
) -> BalancerV2Pool:
    return _build_pool_from_chain(fork_mainnet_full, BALANCER_V2_WETH_RPL_POOL_ADDRESS)


# ---------- Pool Creation Tests ----------


def test_create_weth_bal_pool(ethereum_balancer_v2_weth_bal_pool: BalancerV2Pool):
    lp = ethereum_balancer_v2_weth_bal_pool
    assert lp.address == BALANCER_V2_WETH_BAL_POOL_ADDRESS
    assert lp.pool_id == BALANCER_V2_WETH_BAL_POOL_ID
    assert lp.vault == BALANCER_V2_VAULT_ADDRESS
    assert lp.pool_specialization == 2
    assert lp.fee == Fraction(1, 100)
    assert len(lp.tokens) == 2
    assert lp.tokens[0] == "0xba100000625a3754423978a60c9317c58a424e3D"
    assert lp.tokens[1] == "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    assert lp.state.address == BALANCER_V2_WETH_BAL_POOL_ADDRESS
    assert len(lp.state.balances) == len(lp.tokens)
    assert lp.weights == (80 * 10**16, 20 * 10**16)


def test_create_usdc_weth_pool(ethereum_balancer_v2_usdc_weth_pool: BalancerV2Pool):
    lp = ethereum_balancer_v2_usdc_weth_pool
    assert lp.address == BALANCER_V2_USDC_WETH_POOL_ADDRESS
    assert lp.pool_id == BALANCER_V2_USDC_WETH_POOL_ID
    assert lp.vault == BALANCER_V2_VAULT_ADDRESS
    assert lp.pool_specialization == 2
    assert lp.fee == Fraction(9, 10000)
    assert len(lp.tokens) == 2
    assert lp.tokens[0] == "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    assert lp.tokens[1] == "0xdAC17F958D2ee523a2206206994597C13D831ec7"
    assert lp.state.address == BALANCER_V2_USDC_WETH_POOL_ADDRESS
    assert len(lp.state.balances) == len(lp.tokens)
    assert lp.weights == (50 * 10**16, 50 * 10**16)


def test_create_weth_rpl_pool(ethereum_balancer_v2_weth_rpl_pool: BalancerV2Pool):
    lp = ethereum_balancer_v2_weth_rpl_pool
    assert lp.address == BALANCER_V2_WETH_RPL_POOL_ADDRESS
    assert lp.pool_id == BALANCER_V2_WETH_RPL_POOL_ID
    assert lp.vault == BALANCER_V2_VAULT_ADDRESS
    assert lp.pool_specialization == 2
    assert lp.fee == Fraction(15, 10000)
    assert len(lp.tokens) == 2
    assert lp.tokens[0] == "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    assert lp.tokens[1] == "0xDe30da39c46104798bB5aA3fe8B9e0e1F348163F"
    assert lp.state.address == BALANCER_V2_WETH_RPL_POOL_ADDRESS
    assert len(lp.state.balances) == len(lp.tokens)
    assert lp.weights == (20 * 10**16, 80 * 10**16)


# ---------- Swap Calculation Tests ----------

# Amount multipliers as fractions of the pool's token reserves.
# Each multiplier determines a swap amount as (multiplier * max_reserve).
TOKEN_AMOUNT_MULTIPLIERS = [
    0.0000001,
    0.000001,
    0.00001,
    0.0001,
    0.001,
    0.01,
    0.1,
    0.125,
    0.25,
]


def _run_swap_calculations(
    fork: AnvilFork,
    lp: BalancerV2Pool,
    *,
    swap_directions: list[tuple[int, int]] | None = None,
):
    """Run GIVEN_IN swap calculations against the on-chain BalancerQueries contract
    and assert exact integer match at each amount.
    """
    if swap_directions is None:
        # Default: test both directions (0→1 and 1→0)
        swap_directions = [(0, 1), (1, 0)]

    query_contract = fork.w3.eth.contract(
        address=BALANCERQUERIES_CONTRACT_ADDRESS, abi=BALANCERQUERIES_CONTRACT_ABI
    )

    vault_contract = fork.w3.eth.contract(
        address=BALANCER_V2_VAULT_ADDRESS,
        abi=BALANCER_V2_VAULT_ABI,
    )

    assert lp.balances == tuple(vault_contract.functions.getPoolTokens(lp.pool_id).call()[1])

    for token_in_idx, token_out_idx in swap_directions:
        max_reserve = lp.balances[token_in_idx]

        for token_mult in TOKEN_AMOUNT_MULTIPLIERS:
            token_in_amount = int(token_mult * max_reserve)
            if token_in_amount == 0:
                continue

            helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
                token_in=lp.tokens[token_in_idx],
                token_in_quantity=token_in_amount,
                token_out=lp.tokens[token_out_idx],
            )
            contract_amount_out = query_contract.functions.querySwap(
                (
                    lp.pool_id,
                    SwapKind.GIVEN_IN,
                    lp.tokens[token_in_idx].address,
                    lp.tokens[token_out_idx].address,
                    token_in_amount,
                    b"",
                ),
                (
                    VITALIK_ADDRESS,
                    False,
                    VITALIK_ADDRESS,
                    False,
                ),
            ).call()

            assert helper_amount_out == contract_amount_out, (
                f"Pool {lp.address}, direction {token_in_idx}→{token_out_idx}, "
                f"mult={token_mult}: Python={helper_amount_out}, on-chain={contract_amount_out}"
            )


def test_calculations_weth_bal(
    fork_mainnet_full: AnvilFork,
    ethereum_balancer_v2_weth_bal_pool: BalancerV2Pool,
):
    """WETH/BAL 80/20 pool: both 18-decimal, 1% fee."""
    _run_swap_calculations(fork_mainnet_full, ethereum_balancer_v2_weth_bal_pool)


def test_calculations_usdc_weth(
    fork_mainnet_full: AnvilFork,
    ethereum_balancer_v2_usdc_weth_pool: BalancerV2Pool,
):
    """USDC/WETH 50/50 pool: mixed decimals (6/18), 0.09% fee."""
    lp = ethereum_balancer_v2_usdc_weth_pool
    _run_swap_calculations(fork_mainnet_full, lp)


def test_calculations_weth_rpl(
    fork_mainnet_full: AnvilFork,
    ethereum_balancer_v2_weth_rpl_pool: BalancerV2Pool,
):
    """WETH/RPL 80/20 pool: both 18-decimal, 0.15% fee."""
    lp = ethereum_balancer_v2_weth_rpl_pool
    _run_swap_calculations(fork_mainnet_full, lp)
