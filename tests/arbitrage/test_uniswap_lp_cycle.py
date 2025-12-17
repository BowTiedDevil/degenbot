import asyncio
import concurrent.futures
import contextlib
import multiprocessing
import pickle
import time
from fractions import Fraction
from threading import Lock
from typing import TYPE_CHECKING
from weakref import WeakSet

import pytest

from degenbot.anvil_fork import AnvilFork
from degenbot.arbitrage import UniswapLpCycle
from degenbot.arbitrage.types import (
    ArbitrageCalculationResult,
    UniswapV2PoolSwapAmounts,
    UniswapV3PoolSwapAmounts,
)
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import set_web3
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.arbitrage import ArbitrageError, RateOfExchangeBelowMinimum
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import (
    UniswapV2PoolExternalUpdate,
    UniswapV2PoolState,
    UniswapV2PoolStateUpdated,
)
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3PoolExternalUpdate,
    UniswapV3PoolState,
    UniswapV3PoolStateUpdated,
)
from tests.conftest import FakeSubscriber

if TYPE_CHECKING:
    from degenbot.arbitrage.uniswap_lp_cycle import Pool, PoolState

WBTC_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
WBTC_WETH_V2_POOL_ADDRESS = "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
WBTC_WETH_V3_POOL_ADDRESS = "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"


@pytest.fixture
def wbtc_token(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)
    return Erc20Token(WBTC_ADDRESS)


@pytest.fixture
def weth_token(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)
    return Erc20Token(WETH_ADDRESS)


@pytest.fixture
def wbtc_weth_v2_lp(
    fork_mainnet_full: AnvilFork,
    wbtc_token,  # noqa:ARG001
    weth_token,  # noqa:ARG001
) -> UniswapV2Pool:
    set_web3(fork_mainnet_full.w3)
    pool = UniswapV2Pool(WBTC_WETH_V2_POOL_ADDRESS)
    pool._state = UniswapV2PoolState(
        address=pool.address,
        reserves_token0=16231137593,
        reserves_token1=2571336301536722443178,
        block=pool.update_block,
    )

    return pool


@pytest.fixture
def wbtc_weth_v3_lp(fork_mainnet_full: AnvilFork) -> UniswapV3Pool:
    set_web3(fork_mainnet_full.w3)
    pool = UniswapV3Pool(
        WBTC_WETH_V3_POOL_ADDRESS,
        tick_bitmap={
            -1: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -10: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -11: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -12: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -13: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -14: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -15: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -16: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -17: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -18: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -19: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -2: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -20: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -21: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -22: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -23: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -24: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -25: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -26: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -27: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -28: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -29: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -3: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -30: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -31: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -32: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -33: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -34: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -35: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -36: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -37: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -38: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -39: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -4: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -40: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -41: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -42: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -43: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -44: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -45: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -46: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -47: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -48: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -49: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -5: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -50: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -51: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -52: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -53: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -54: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -55: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -56: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -57: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -58: UniswapV3BitmapAtWord(
                bitmap=6917529027641081856,
            ),
            -6: UniswapV3BitmapAtWord(
                bitmap=2,
            ),
            -7: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -8: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            -9: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            0: UniswapV3BitmapAtWord(
                bitmap=1,
            ),
            1: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            10: UniswapV3BitmapAtWord(
                bitmap=170141183460469231731687303715884105728,
            ),
            11: UniswapV3BitmapAtWord(
                bitmap=28948243165212146153933944164359569828134633057888820293979968816874004676608,
            ),
            12: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            13: UniswapV3BitmapAtWord(
                bitmap=28948022309329048855892746252171976963317620781534745845727480733890183692288,
            ),
            14: UniswapV3BitmapAtWord(
                bitmap=54277541829991966604798899222822457154669449039060000979991340549024308527104,
            ),
            15: UniswapV3BitmapAtWord(
                bitmap=65136584752943728826460900673877721788351413523865880600493789704421574172672,
            ),
            16: UniswapV3BitmapAtWord(
                bitmap=115792089237316195423570985008687907853269984665640564039457584005690955922568,
            ),
            17: UniswapV3BitmapAtWord(
                bitmap=14474011154664524432752386995444604548638792651980126970205750076693337341951,
            ),
            18: UniswapV3BitmapAtWord(
                bitmap=5327296616471419015641141638445662208,
            ),
            19: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            2: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            20: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            21: UniswapV3BitmapAtWord(
                bitmap=41538374868278621028243970633760768,
            ),
            22: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            23: UniswapV3BitmapAtWord(
                bitmap=14474011154664524427946373126085988481658748083205070504932198000989141204992,
            ),
            24: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            25: UniswapV3BitmapAtWord(
                bitmap=21267647932558653966460912964485513216,
            ),
            26: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            27: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            28: UniswapV3BitmapAtWord(
                bitmap=21267647932558653966460912964485513216,
            ),
            29: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            3: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            30: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            31: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            32: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            33: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            34: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            35: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            36: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            37: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            38: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            39: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            4: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            40: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            41: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            42: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            43: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            44: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            45: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            46: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            47: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            48: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            49: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            5: UniswapV3BitmapAtWord(
                bitmap=86844066927987146567678238756515930889952488499230423029593188005934847229952,
            ),
            50: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            51: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            52: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            53: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            54: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            55: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            56: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            57: UniswapV3BitmapAtWord(
                bitmap=50216813883093446110686315385661331328818843555712276103168,
            ),
            6: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            7: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            8: UniswapV3BitmapAtWord(
                bitmap=0,
            ),
            9: UniswapV3BitmapAtWord(
                bitmap=21741053132098019175522475746641612337257189351054491279686364306905686343680,
            ),
        },
        tick_data={
            -887160: UniswapV3LiquidityAtTick(
                liquidity_net=80064092962998,
                liquidity_gross=80064092962998,
            ),
            -887220: UniswapV3LiquidityAtTick(
                liquidity_net=82174936226787,
                liquidity_gross=82174936226787,
            ),
            -92100: UniswapV3LiquidityAtTick(
                liquidity_net=456406095307,
                liquidity_gross=456406095307,
            ),
            0: UniswapV3LiquidityAtTick(
                liquidity_net=10943161472679,
                liquidity_gross=10943161472679,
            ),
            152640: UniswapV3LiquidityAtTick(
                liquidity_net=40166358066529,
                liquidity_gross=40166358066529,
            ),
            152880: UniswapV3LiquidityAtTick(
                liquidity_net=-40166358066529,
                liquidity_gross=40166358066529,
            ),
            153360: UniswapV3LiquidityAtTick(
                liquidity_net=155684471699734,
                liquidity_gross=155684471699734,
            ),
            153420: UniswapV3LiquidityAtTick(
                liquidity_net=-155684471699734,
                liquidity_gross=155684471699734,
            ),
            161220: UniswapV3LiquidityAtTick(
                liquidity_net=181470429131796,
                liquidity_gross=181470429131796,
            ),
            176220: UniswapV3LiquidityAtTick(
                liquidity_net=358354299830,
                liquidity_gross=358354299830,
            ),
            183180: UniswapV3LiquidityAtTick(
                liquidity_net=-358354299830,
                liquidity_gross=358354299830,
            ),
            184200: UniswapV3LiquidityAtTick(
                liquidity_net=-79344120619876,
                liquidity_gross=80784065306120,
            ),
            206580: UniswapV3LiquidityAtTick(
                liquidity_net=54402074932,
                liquidity_gross=54402074932,
            ),
            206640: UniswapV3LiquidityAtTick(
                liquidity_net=-54402074932,
                liquidity_gross=54402074932,
            ),
            214920: UniswapV3LiquidityAtTick(
                liquidity_net=31454785349939,
                liquidity_gross=31454785349939,
            ),
            216420: UniswapV3LiquidityAtTick(
                liquidity_net=3151604679224,
                liquidity_gross=3151604679224,
            ),
            217740: UniswapV3LiquidityAtTick(
                liquidity_net=-31454785349939,
                liquidity_gross=31454785349939,
            ),
            223320: UniswapV3LiquidityAtTick(
                liquidity_net=340095002,
                liquidity_gross=340095002,
            ),
            230100: UniswapV3LiquidityAtTick(
                liquidity_net=10499785476924,
                liquidity_gross=10499785476924,
            ),
            230160: UniswapV3LiquidityAtTick(
                liquidity_net=1670921212240,
                liquidity_gross=1670921212240,
            ),
            230220: UniswapV3LiquidityAtTick(
                liquidity_net=248504515915,
                liquidity_gross=248504515915,
            ),
            230280: UniswapV3LiquidityAtTick(
                liquidity_net=11377831611211,
                liquidity_gross=14720354225695,
            ),
            231180: UniswapV3LiquidityAtTick(
                liquidity_net=10325006026,
                liquidity_gross=10325006026,
            ),
            231240: UniswapV3LiquidityAtTick(
                liquidity_net=9636087679,
                liquidity_gross=9636087679,
            ),
            231300: UniswapV3LiquidityAtTick(
                liquidity_net=4067292562252,
                liquidity_gross=4067292562252,
            ),
            231360: UniswapV3LiquidityAtTick(
                liquidity_net=10098305733,
                liquidity_gross=10098305733,
            ),
            231420: UniswapV3LiquidityAtTick(
                liquidity_net=105440390087,
                liquidity_gross=105440390087,
            ),
            231480: UniswapV3LiquidityAtTick(
                liquidity_net=389377464792,
                liquidity_gross=389377464792,
            ),
            231540: UniswapV3LiquidityAtTick(
                liquidity_net=62725860270,
                liquidity_gross=62725860270,
            ),
            234060: UniswapV3LiquidityAtTick(
                liquidity_net=24418298514298,
                liquidity_gross=24418298514298,
            ),
            234300: UniswapV3LiquidityAtTick(
                liquidity_net=-379213263309,
                liquidity_gross=379213263309,
            ),
            235620: UniswapV3LiquidityAtTick(
                liquidity_net=13143858905286,
                liquidity_gross=13143858905286,
            ),
            237180: UniswapV3LiquidityAtTick(
                liquidity_net=4066391507115,
                liquidity_gross=4218743418661,
            ),
            237300: UniswapV3LiquidityAtTick(
                liquidity_net=2512358234332724,
                liquidity_gross=2512358234332724,
            ),
            237420: UniswapV3LiquidityAtTick(
                liquidity_net=788243782725,
                liquidity_gross=788243782725,
            ),
            239460: UniswapV3LiquidityAtTick(
                liquidity_net=8584538262056,
                liquidity_gross=8584538262056,
            ),
            239760: UniswapV3LiquidityAtTick(
                liquidity_net=3095863280482,
                liquidity_gross=3095863280482,
            ),
            240420: UniswapV3LiquidityAtTick(
                liquidity_net=12999196246833,
                liquidity_gross=12999196246833,
            ),
            240780: UniswapV3LiquidityAtTick(
                liquidity_net=7818819408261,
                liquidity_gross=7818819408261,
            ),
            241260: UniswapV3LiquidityAtTick(
                liquidity_net=1856215464927,
                liquidity_gross=1856215464927,
            ),
            241380: UniswapV3LiquidityAtTick(
                liquidity_net=7814421206784,
                liquidity_gross=7814421206784,
            ),
            242340: UniswapV3LiquidityAtTick(
                liquidity_net=3373862096954,
                liquidity_gross=3373862096954,
            ),
            242820: UniswapV3LiquidityAtTick(
                liquidity_net=2611786952298,
                liquidity_gross=2611786952298,
            ),
            243360: UniswapV3LiquidityAtTick(
                liquidity_net=103000565770,
                liquidity_gross=103000565770,
            ),
            244140: UniswapV3LiquidityAtTick(
                liquidity_net=217561198854734,
                liquidity_gross=217561198854734,
            ),
            244860: UniswapV3LiquidityAtTick(
                liquidity_net=893004067318,
                liquidity_gross=893004067318,
            ),
            245520: UniswapV3LiquidityAtTick(
                liquidity_net=266126101590362,
                liquidity_gross=266126101590362,
            ),
            245700: UniswapV3LiquidityAtTick(
                liquidity_net=16758180163,
                liquidity_gross=16758180163,
            ),
            245940: UniswapV3LiquidityAtTick(
                liquidity_net=76701235421656,
                liquidity_gross=76701235421656,
            ),
            246180: UniswapV3LiquidityAtTick(
                liquidity_net=4890971540,
                liquidity_gross=4890971540,
            ),
            246360: UniswapV3LiquidityAtTick(
                liquidity_net=1235247247938725,
                liquidity_gross=1235247247938725,
            ),
            246420: UniswapV3LiquidityAtTick(
                liquidity_net=69925333162032,
                liquidity_gross=69925333162032,
            ),
            246480: UniswapV3LiquidityAtTick(
                liquidity_net=83562433586,
                liquidity_gross=83562433586,
            ),
            246540: UniswapV3LiquidityAtTick(
                liquidity_net=837767553892,
                liquidity_gross=837767553892,
            ),
            246600: UniswapV3LiquidityAtTick(
                liquidity_net=4814170393189,
                liquidity_gross=4814170393189,
            ),
            246660: UniswapV3LiquidityAtTick(
                liquidity_net=14690532940788,
                liquidity_gross=14690532940788,
            ),
            246720: UniswapV3LiquidityAtTick(
                liquidity_net=243908811117210,
                liquidity_gross=243908811117210,
            ),
            246780: UniswapV3LiquidityAtTick(
                liquidity_net=233944583115,
                liquidity_gross=233944583115,
            ),
            246840: UniswapV3LiquidityAtTick(
                liquidity_net=1143995552,
                liquidity_gross=1143995552,
            ),
            246900: UniswapV3LiquidityAtTick(
                liquidity_net=263365923015,
                liquidity_gross=263365923015,
            ),
            246960: UniswapV3LiquidityAtTick(
                liquidity_net=14264473913293,
                liquidity_gross=14264473913293,
            ),
            247320: UniswapV3LiquidityAtTick(
                liquidity_net=48519918747269,
                liquidity_gross=48519918747269,
            ),
            247380: UniswapV3LiquidityAtTick(
                liquidity_net=117622952159219,
                liquidity_gross=117622952159219,
            ),
            247440: UniswapV3LiquidityAtTick(
                liquidity_net=665770897850117,
                liquidity_gross=665770897850117,
            ),
            247620: UniswapV3LiquidityAtTick(
                liquidity_net=487875820420,
                liquidity_gross=487875820420,
            ),
            247740: UniswapV3LiquidityAtTick(
                liquidity_net=11523276594189,
                liquidity_gross=11523276594189,
            ),
            247860: UniswapV3LiquidityAtTick(
                liquidity_net=1649567047148,
                liquidity_gross=1649567047148,
            ),
            247920: UniswapV3LiquidityAtTick(
                liquidity_net=5342639837069495,
                liquidity_gross=5342639837069495,
            ),
            247980: UniswapV3LiquidityAtTick(
                liquidity_net=23028334649368,
                liquidity_gross=23028334649368,
            ),
            248040: UniswapV3LiquidityAtTick(
                liquidity_net=252942942781,
                liquidity_gross=252942942781,
            ),
            248100: UniswapV3LiquidityAtTick(
                liquidity_net=24372131940667,
                liquidity_gross=24372131940667,
            ),
            248160: UniswapV3LiquidityAtTick(
                liquidity_net=102373720892200,
                liquidity_gross=102373720892200,
            ),
            248280: UniswapV3LiquidityAtTick(
                liquidity_net=5974932615038,
                liquidity_gross=5974932615038,
            ),
            248340: UniswapV3LiquidityAtTick(
                liquidity_net=745663361773900,
                liquidity_gross=745663361773900,
            ),
            248400: UniswapV3LiquidityAtTick(
                liquidity_net=106096220552937,
                liquidity_gross=106096220552937,
            ),
            248460: UniswapV3LiquidityAtTick(
                liquidity_net=15823287912623,
                liquidity_gross=15823287912623,
            ),
            248520: UniswapV3LiquidityAtTick(
                liquidity_net=339165199161295,
                liquidity_gross=339165199161295,
            ),
            248580: UniswapV3LiquidityAtTick(
                liquidity_net=96340504853,
                liquidity_gross=96340504853,
            ),
            248640: UniswapV3LiquidityAtTick(
                liquidity_net=34036669144137,
                liquidity_gross=34036669144137,
            ),
            248700: UniswapV3LiquidityAtTick(
                liquidity_net=106728159577006,
                liquidity_gross=106728159577006,
            ),
            248760: UniswapV3LiquidityAtTick(
                liquidity_net=130017625795223,
                liquidity_gross=130017625795223,
            ),
            248820: UniswapV3LiquidityAtTick(
                liquidity_net=231267402130471,
                liquidity_gross=231267402130471,
            ),
            248880: UniswapV3LiquidityAtTick(
                liquidity_net=1191600446578,
                liquidity_gross=1191600446578,
            ),
            248940: UniswapV3LiquidityAtTick(
                liquidity_net=10262040820492,
                liquidity_gross=10262040820492,
            ),
            249000: UniswapV3LiquidityAtTick(
                liquidity_net=375045998526336,
                liquidity_gross=375045998526336,
            ),
            249060: UniswapV3LiquidityAtTick(
                liquidity_net=14220938232775,
                liquidity_gross=14220938232775,
            ),
            249120: UniswapV3LiquidityAtTick(
                liquidity_net=8546121898214,
                liquidity_gross=8546121898214,
            ),
            249180: UniswapV3LiquidityAtTick(
                liquidity_net=805435724180,
                liquidity_gross=805435724180,
            ),
            249240: UniswapV3LiquidityAtTick(
                liquidity_net=2458914255421348,
                liquidity_gross=2458914255421348,
            ),
            249300: UniswapV3LiquidityAtTick(
                liquidity_net=753689707471433,
                liquidity_gross=753689707471433,
            ),
            249360: UniswapV3LiquidityAtTick(
                liquidity_net=250509876783271,
                liquidity_gross=250509876783271,
            ),
            249420: UniswapV3LiquidityAtTick(
                liquidity_net=15459403677425,
                liquidity_gross=15459403677425,
            ),
            249480: UniswapV3LiquidityAtTick(
                liquidity_net=204634341236947,
                liquidity_gross=204634341236947,
            ),
            249540: UniswapV3LiquidityAtTick(
                liquidity_net=536139191751638,
                liquidity_gross=536139191751638,
            ),
            249600: UniswapV3LiquidityAtTick(
                liquidity_net=1362751680139023,
                liquidity_gross=1362751680139023,
            ),
            249660: UniswapV3LiquidityAtTick(
                liquidity_net=75072282227263,
                liquidity_gross=75072282227263,
            ),
            249720: UniswapV3LiquidityAtTick(
                liquidity_net=128404454018236,
                liquidity_gross=128404454018236,
            ),
            249780: UniswapV3LiquidityAtTick(
                liquidity_net=752941312872158,
                liquidity_gross=752941312872158,
            ),
            249840: UniswapV3LiquidityAtTick(
                liquidity_net=150303272592282,
                liquidity_gross=150303272592282,
            ),
            249900: UniswapV3LiquidityAtTick(
                liquidity_net=2999000185960519,
                liquidity_gross=2999000185960519,
            ),
            249960: UniswapV3LiquidityAtTick(
                liquidity_net=1897487030889456,
                liquidity_gross=1897487030889456,
            ),
            250020: UniswapV3LiquidityAtTick(
                liquidity_net=338541271525525,
                liquidity_gross=338541271525525,
            ),
            250080: UniswapV3LiquidityAtTick(
                liquidity_net=1019910411632819,
                liquidity_gross=1019910411632819,
            ),
            250140: UniswapV3LiquidityAtTick(
                liquidity_net=349559136168929,
                liquidity_gross=349559136168929,
            ),
            250200: UniswapV3LiquidityAtTick(
                liquidity_net=357255210710225,
                liquidity_gross=357255210710225,
            ),
            250260: UniswapV3LiquidityAtTick(
                liquidity_net=236399678077506,
                liquidity_gross=236399678077506,
            ),
            250320: UniswapV3LiquidityAtTick(
                liquidity_net=149802042174914,
                liquidity_gross=149802042174914,
            ),
            250380: UniswapV3LiquidityAtTick(
                liquidity_net=22684152997344,
                liquidity_gross=22684152997344,
            ),
            250440: UniswapV3LiquidityAtTick(
                liquidity_net=162814016216893,
                liquidity_gross=162814016216893,
            ),
            250500: UniswapV3LiquidityAtTick(
                liquidity_net=2873122593562261,
                liquidity_gross=2873122593562261,
            ),
            250560: UniswapV3LiquidityAtTick(
                liquidity_net=721265962680257,
                liquidity_gross=721265962680257,
            ),
            250620: UniswapV3LiquidityAtTick(
                liquidity_net=3321833231733709,
                liquidity_gross=3321833231733709,
            ),
            250680: UniswapV3LiquidityAtTick(
                liquidity_net=379136360814294,
                liquidity_gross=379136360814294,
            ),
            250740: UniswapV3LiquidityAtTick(
                liquidity_net=30685778627595,
                liquidity_gross=30685778627595,
            ),
            250800: UniswapV3LiquidityAtTick(
                liquidity_net=486676008472444,
                liquidity_gross=486676008472444,
            ),
            250860: UniswapV3LiquidityAtTick(
                liquidity_net=495251715841078,
                liquidity_gross=495251715841078,
            ),
            250920: UniswapV3LiquidityAtTick(
                liquidity_net=546196098082915,
                liquidity_gross=546196098082915,
            ),
            250980: UniswapV3LiquidityAtTick(
                liquidity_net=1874212917320,
                liquidity_gross=1874212917320,
            ),
            251040: UniswapV3LiquidityAtTick(
                liquidity_net=18739094027110,
                liquidity_gross=18739094027110,
            ),
            251100: UniswapV3LiquidityAtTick(
                liquidity_net=116746369690747,
                liquidity_gross=116746369690747,
            ),
            251160: UniswapV3LiquidityAtTick(
                liquidity_net=26215123110965,
                liquidity_gross=26215123110965,
            ),
            251220: UniswapV3LiquidityAtTick(
                liquidity_net=1978401442982954,
                liquidity_gross=1978401442982954,
            ),
            251280: UniswapV3LiquidityAtTick(
                liquidity_net=1744914434361,
                liquidity_gross=1744914434361,
            ),
            251340: UniswapV3LiquidityAtTick(
                liquidity_net=180353178844777,
                liquidity_gross=180353178844777,
            ),
            251400: UniswapV3LiquidityAtTick(
                liquidity_net=128509974135464,
                liquidity_gross=128509974135464,
            ),
            251460: UniswapV3LiquidityAtTick(
                liquidity_net=540981291328174,
                liquidity_gross=540981291328174,
            ),
            251520: UniswapV3LiquidityAtTick(
                liquidity_net=5731476751822266,
                liquidity_gross=5731476751822266,
            ),
            251580: UniswapV3LiquidityAtTick(
                liquidity_net=26703188780726,
                liquidity_gross=26703188780726,
            ),
            251640: UniswapV3LiquidityAtTick(
                liquidity_net=506062280322839,
                liquidity_gross=506062280322839,
            ),
            251700: UniswapV3LiquidityAtTick(
                liquidity_net=244084381320927,
                liquidity_gross=244084381320927,
            ),
            251760: UniswapV3LiquidityAtTick(
                liquidity_net=472259833373720,
                liquidity_gross=472259833373720,
            ),
            251820: UniswapV3LiquidityAtTick(
                liquidity_net=72486190536708,
                liquidity_gross=72486190536708,
            ),
            251880: UniswapV3LiquidityAtTick(
                liquidity_net=931553309070,
                liquidity_gross=931553309070,
            ),
            251940: UniswapV3LiquidityAtTick(
                liquidity_net=293375936683379,
                liquidity_gross=293375936683379,
            ),
            252000: UniswapV3LiquidityAtTick(
                liquidity_net=103709076861484,
                liquidity_gross=103709076861484,
            ),
            252060: UniswapV3LiquidityAtTick(
                liquidity_net=10396565017847,
                liquidity_gross=10396565017847,
            ),
            252120: UniswapV3LiquidityAtTick(
                liquidity_net=219076530609482,
                liquidity_gross=219076530609482,
            ),
            252180: UniswapV3LiquidityAtTick(
                liquidity_net=156092492819634,
                liquidity_gross=156092492819634,
            ),
            252240: UniswapV3LiquidityAtTick(
                liquidity_net=1115420578481664,
                liquidity_gross=1115420578481664,
            ),
            252300: UniswapV3LiquidityAtTick(
                liquidity_net=7031828617233349,
                liquidity_gross=7031828617233349,
            ),
            252360: UniswapV3LiquidityAtTick(
                liquidity_net=733766399181670,
                liquidity_gross=733766399181670,
            ),
            252420: UniswapV3LiquidityAtTick(
                liquidity_net=84805612483975,
                liquidity_gross=84805612483975,
            ),
            252480: UniswapV3LiquidityAtTick(
                liquidity_net=9463874529391447,
                liquidity_gross=9463874529391447,
            ),
            252540: UniswapV3LiquidityAtTick(
                liquidity_net=7619953195446359,
                liquidity_gross=7619953195446359,
            ),
            252600: UniswapV3LiquidityAtTick(
                liquidity_net=16406775025846,
                liquidity_gross=16406775025846,
            ),
            252660: UniswapV3LiquidityAtTick(
                liquidity_net=2212907952699581,
                liquidity_gross=2212907952699581,
            ),
            252720: UniswapV3LiquidityAtTick(
                liquidity_net=30877568635642,
                liquidity_gross=30877568635642,
            ),
            252780: UniswapV3LiquidityAtTick(
                liquidity_net=1127243612814164,
                liquidity_gross=1127243612814164,
            ),
            252840: UniswapV3LiquidityAtTick(
                liquidity_net=121022273918801,
                liquidity_gross=121022273918801,
            ),
            252900: UniswapV3LiquidityAtTick(
                liquidity_net=3444505408543,
                liquidity_gross=3444505408543,
            ),
            252960: UniswapV3LiquidityAtTick(
                liquidity_net=2921179233331,
                liquidity_gross=2921179233331,
            ),
            253020: UniswapV3LiquidityAtTick(
                liquidity_net=16328660267242,
                liquidity_gross=16328660267242,
            ),
            253080: UniswapV3LiquidityAtTick(
                liquidity_net=280182070401033,
                liquidity_gross=280182070401033,
            ),
            253140: UniswapV3LiquidityAtTick(
                liquidity_net=60418933087985,
                liquidity_gross=60418933087985,
            ),
            253200: UniswapV3LiquidityAtTick(
                liquidity_net=2221329753927173,
                liquidity_gross=2221329753927173,
            ),
            253260: UniswapV3LiquidityAtTick(
                liquidity_net=785975068283793,
                liquidity_gross=785975068283793,
            ),
            253320: UniswapV3LiquidityAtTick(
                liquidity_net=11068031251918845,
                liquidity_gross=11068031251918845,
            ),
            253380: UniswapV3LiquidityAtTick(
                liquidity_net=1045385900711870,
                liquidity_gross=1045385900711870,
            ),
            253440: UniswapV3LiquidityAtTick(
                liquidity_net=52071127547319,
                liquidity_gross=52071127547319,
            ),
            253500: UniswapV3LiquidityAtTick(
                liquidity_net=905430992202364,
                liquidity_gross=905430992202364,
            ),
            253560: UniswapV3LiquidityAtTick(
                liquidity_net=743934772353060,
                liquidity_gross=743934772353060,
            ),
            253620: UniswapV3LiquidityAtTick(
                liquidity_net=1720814520558933,
                liquidity_gross=1720814520558933,
            ),
            253680: UniswapV3LiquidityAtTick(
                liquidity_net=1694339960854588,
                liquidity_gross=1694339960854588,
            ),
            253740: UniswapV3LiquidityAtTick(
                liquidity_net=1265620344704839,
                liquidity_gross=1265620344704839,
            ),
            253800: UniswapV3LiquidityAtTick(
                liquidity_net=6822448654094831,
                liquidity_gross=6822448654094831,
            ),
            253860: UniswapV3LiquidityAtTick(
                liquidity_net=1693329952501742,
                liquidity_gross=1693329952501742,
            ),
            253920: UniswapV3LiquidityAtTick(
                liquidity_net=125002317687877,
                liquidity_gross=125002317687877,
            ),
            253980: UniswapV3LiquidityAtTick(
                liquidity_net=3223198388506118,
                liquidity_gross=3223198388506118,
            ),
            254040: UniswapV3LiquidityAtTick(
                liquidity_net=7233669530042180,
                liquidity_gross=7233669530042180,
            ),
            254100: UniswapV3LiquidityAtTick(
                liquidity_net=1354995309152257,
                liquidity_gross=1354995309152257,
            ),
            254160: UniswapV3LiquidityAtTick(
                liquidity_net=232717215486581,
                liquidity_gross=232717215486581,
            ),
            254220: UniswapV3LiquidityAtTick(
                liquidity_net=1251624436633624,
                liquidity_gross=1251624436633624,
            ),
            254280: UniswapV3LiquidityAtTick(
                liquidity_net=2397734105813300,
                liquidity_gross=2397734105813300,
            ),
            254340: UniswapV3LiquidityAtTick(
                liquidity_net=5944750269566964,
                liquidity_gross=5944750269566964,
            ),
            254400: UniswapV3LiquidityAtTick(
                liquidity_net=293897761293656,
                liquidity_gross=293897761293656,
            ),
            254460: UniswapV3LiquidityAtTick(
                liquidity_net=27750958550784078,
                liquidity_gross=27750958550784078,
            ),
            254520: UniswapV3LiquidityAtTick(
                liquidity_net=8803885001420580,
                liquidity_gross=8803885001420580,
            ),
            254580: UniswapV3LiquidityAtTick(
                liquidity_net=4832503942071766,
                liquidity_gross=4832503942071766,
            ),
            254640: UniswapV3LiquidityAtTick(
                liquidity_net=18878251447853522,
                liquidity_gross=18878251447853522,
            ),
            254700: UniswapV3LiquidityAtTick(
                liquidity_net=47155277686430759,
                liquidity_gross=47155277686430759,
            ),
            254760: UniswapV3LiquidityAtTick(
                liquidity_net=2205915167240804,
                liquidity_gross=2205915167240804,
            ),
            254820: UniswapV3LiquidityAtTick(
                liquidity_net=714569902194940,
                liquidity_gross=714569902194940,
            ),
            254880: UniswapV3LiquidityAtTick(
                liquidity_net=79311941684663448,
                liquidity_gross=79311941684663448,
            ),
            254940: UniswapV3LiquidityAtTick(
                liquidity_net=131611058716155639,
                liquidity_gross=131611058716155639,
            ),
            255000: UniswapV3LiquidityAtTick(
                liquidity_net=1598723641314836,
                liquidity_gross=1598723641314836,
            ),
            255060: UniswapV3LiquidityAtTick(
                liquidity_net=3570483212215165,
                liquidity_gross=3570483212215165,
            ),
            255120: UniswapV3LiquidityAtTick(
                liquidity_net=-685298200223732,
                liquidity_gross=17722528321831664,
            ),
            255180: UniswapV3LiquidityAtTick(
                liquidity_net=10072938869856001,
                liquidity_gross=10094548691501753,
            ),
            255240: UniswapV3LiquidityAtTick(
                liquidity_net=21324359501748223,
                liquidity_gross=21324359501748223,
            ),
            255300: UniswapV3LiquidityAtTick(
                liquidity_net=4364228887048905,
                liquidity_gross=4364237096627033,
            ),
            255360: UniswapV3LiquidityAtTick(
                liquidity_net=3206055008276559,
                liquidity_gross=3206055008276559,
            ),
            255420: UniswapV3LiquidityAtTick(
                liquidity_net=8337914674210692,
                liquidity_gross=8337914674210692,
            ),
            255480: UniswapV3LiquidityAtTick(
                liquidity_net=7341369571915483,
                liquidity_gross=7343598454325075,
            ),
            255540: UniswapV3LiquidityAtTick(
                liquidity_net=66767515757170606,
                liquidity_gross=66845387101892372,
            ),
            255600: UniswapV3LiquidityAtTick(
                liquidity_net=162909027590248760,
                liquidity_gross=162909027590248760,
            ),
            255660: UniswapV3LiquidityAtTick(
                liquidity_net=216964302409007549,
                liquidity_gross=216964302409007549,
            ),
            255720: UniswapV3LiquidityAtTick(
                liquidity_net=385101416899725455,
                liquidity_gross=385101416899725455,
            ),
            255780: UniswapV3LiquidityAtTick(
                liquidity_net=40303492606471480,
                liquidity_gross=40303492606471480,
            ),
            255840: UniswapV3LiquidityAtTick(
                liquidity_net=3851053596342728,
                liquidity_gross=3851053596342728,
            ),
            255900: UniswapV3LiquidityAtTick(
                liquidity_net=61739626823736178,
                liquidity_gross=61761883437367390,
            ),
            255960: UniswapV3LiquidityAtTick(
                liquidity_net=7555831027318727,
                liquidity_gross=8019972741275283,
            ),
            256020: UniswapV3LiquidityAtTick(
                liquidity_net=11360440169028523,
                liquidity_gross=11827319419190403,
            ),
            256080: UniswapV3LiquidityAtTick(
                liquidity_net=60072349123153735,
                liquidity_gross=60213912612514587,
            ),
            256140: UniswapV3LiquidityAtTick(
                liquidity_net=-74357472016628479,
                liquidity_gross=170532390299599075,
            ),
            256200: UniswapV3LiquidityAtTick(
                liquidity_net=16239518064529848,
                liquidity_gross=18577658901767840,
            ),
            256260: UniswapV3LiquidityAtTick(
                liquidity_net=438562623027249,
                liquidity_gross=524155195785499,
            ),
            256320: UniswapV3LiquidityAtTick(
                liquidity_net=416244761851792821,
                liquidity_gross=418360901573548409,
            ),
            256380: UniswapV3LiquidityAtTick(
                liquidity_net=35258940131745361,
                liquidity_gross=35354281359858859,
            ),
            256440: UniswapV3LiquidityAtTick(
                liquidity_net=87429969783283206,
                liquidity_gross=87498002263567942,
            ),
            256500: UniswapV3LiquidityAtTick(
                liquidity_net=36662462787136434,
                liquidity_gross=37079399801719790,
            ),
            256560: UniswapV3LiquidityAtTick(
                liquidity_net=24690609809160596,
                liquidity_gross=26861955491935480,
            ),
            256620: UniswapV3LiquidityAtTick(
                liquidity_net=1530693759002640,
                liquidity_gross=2266872875510184,
            ),
            256680: UniswapV3LiquidityAtTick(
                liquidity_net=450725489682812642,
                liquidity_gross=617845694976651704,
            ),
            256740: UniswapV3LiquidityAtTick(
                liquidity_net=7757717182115128,
                liquidity_gross=9508460709542130,
            ),
            256800: UniswapV3LiquidityAtTick(
                liquidity_net=13959272670778670,
                liquidity_gross=16092031679955394,
            ),
            256860: UniswapV3LiquidityAtTick(
                liquidity_net=4834815815433126,
                liquidity_gross=5917950403250604,
            ),
            256920: UniswapV3LiquidityAtTick(
                liquidity_net=3833542223902956,
                liquidity_gross=4512613781521666,
            ),
            256980: UniswapV3LiquidityAtTick(
                liquidity_net=-513770369320412373,
                liquidity_gross=518977161709113805,
            ),
            257040: UniswapV3LiquidityAtTick(
                liquidity_net=-399680035107951409,
                liquidity_gross=403620542995899807,
            ),
            257100: UniswapV3LiquidityAtTick(
                liquidity_net=944424023373490,
                liquidity_gross=3694067897442130,
            ),
            257160: UniswapV3LiquidityAtTick(
                liquidity_net=-17670767329996145,
                liquidity_gross=29594233102383831,
            ),
            257220: UniswapV3LiquidityAtTick(
                liquidity_net=11903192163319542,
                liquidity_gross=39655175448077564,
            ),
            257280: UniswapV3LiquidityAtTick(
                liquidity_net=-1571540551183525,
                liquidity_gross=13904101892598927,
            ),
            257340: UniswapV3LiquidityAtTick(
                liquidity_net=11998694428887359,
                liquidity_gross=12966404404911551,
            ),
            257400: UniswapV3LiquidityAtTick(
                liquidity_net=-41005393479443210,
                liquidity_gross=77763278732931152,
            ),
            257460: UniswapV3LiquidityAtTick(
                liquidity_net=-40685333587385153,
                liquidity_gross=65686824302883035,
            ),
            257520: UniswapV3LiquidityAtTick(
                liquidity_net=-61225490611514652,
                liquidity_gross=70184420136930512,
            ),
            257580: UniswapV3LiquidityAtTick(
                liquidity_net=-18439176989338244,
                liquidity_gross=27257534142204174,
            ),
            257640: UniswapV3LiquidityAtTick(
                liquidity_net=-354819799885904976,
                liquidity_gross=354826232508972794,
            ),
            257700: UniswapV3LiquidityAtTick(
                liquidity_net=555307873298135460,
                liquidity_gross=580538043725177658,
            ),
            257760: UniswapV3LiquidityAtTick(
                liquidity_net=-25710738959484157,
                liquidity_gross=36060625186049061,
            ),
            257820: UniswapV3LiquidityAtTick(
                liquidity_net=-41778268035859589,
                liquidity_gross=41831402034343447,
            ),
            257880: UniswapV3LiquidityAtTick(
                liquidity_net=-2514493092005759,
                liquidity_gross=12524194724929907,
            ),
            257940: UniswapV3LiquidityAtTick(
                liquidity_net=-3653336697694042,
                liquidity_gross=3948963147827352,
            ),
            258000: UniswapV3LiquidityAtTick(
                liquidity_net=-17407959736997210,
                liquidity_gross=18022766627074666,
            ),
            258060: UniswapV3LiquidityAtTick(
                liquidity_net=-90544091051378961,
                liquidity_gross=90569735114912293,
            ),
            258120: UniswapV3LiquidityAtTick(
                liquidity_net=-27435537767419192,
                liquidity_gross=27512736265079542,
            ),
            258180: UniswapV3LiquidityAtTick(
                liquidity_net=-70075898568894367,
                liquidity_gross=71239752558266035,
            ),
            258240: UniswapV3LiquidityAtTick(
                liquidity_net=-77730101655027806,
                liquidity_gross=77788860536221776,
            ),
            258300: UniswapV3LiquidityAtTick(
                liquidity_net=-480160670015383231,
                liquidity_gross=480234010987500729,
            ),
            258360: UniswapV3LiquidityAtTick(
                liquidity_net=-7951267675408107,
                liquidity_gross=7959900060977557,
            ),
            258420: UniswapV3LiquidityAtTick(
                liquidity_net=-53269633953047198,
                liquidity_gross=53282183913353050,
            ),
            258480: UniswapV3LiquidityAtTick(
                liquidity_net=-44688876530963149,
                liquidity_gross=44956172397163395,
            ),
            258540: UniswapV3LiquidityAtTick(
                liquidity_net=-8452946696940052,
                liquidity_gross=8452946696940052,
            ),
            258600: UniswapV3LiquidityAtTick(
                liquidity_net=-7622516282077012,
                liquidity_gross=7622516282077012,
            ),
            258660: UniswapV3LiquidityAtTick(
                liquidity_net=-31477812345055638,
                liquidity_gross=31477812345055638,
            ),
            258720: UniswapV3LiquidityAtTick(
                liquidity_net=-10379774337647228,
                liquidity_gross=10390946286213796,
            ),
            258780: UniswapV3LiquidityAtTick(
                liquidity_net=-3847257447300179,
                liquidity_gross=3950604565611973,
            ),
            258840: UniswapV3LiquidityAtTick(
                liquidity_net=-97900646011974339,
                liquidity_gross=97900646011974339,
            ),
            258900: UniswapV3LiquidityAtTick(
                liquidity_net=-9891688696501483,
                liquidity_gross=10064179935032431,
            ),
            258960: UniswapV3LiquidityAtTick(
                liquidity_net=-25494043572351332,
                liquidity_gross=25494043572351332,
            ),
            259020: UniswapV3LiquidityAtTick(
                liquidity_net=-1984614229062349,
                liquidity_gross=1984614229062349,
            ),
            259080: UniswapV3LiquidityAtTick(
                liquidity_net=-60946434614754049,
                liquidity_gross=60946434614754049,
            ),
            259140: UniswapV3LiquidityAtTick(
                liquidity_net=-1771930106734303,
                liquidity_gross=1771930106734303,
            ),
            259200: UniswapV3LiquidityAtTick(
                liquidity_net=-58824567876657891,
                liquidity_gross=58824567876657891,
            ),
            259260: UniswapV3LiquidityAtTick(
                liquidity_net=-68270577775079268,
                liquidity_gross=68270577775079268,
            ),
            259320: UniswapV3LiquidityAtTick(
                liquidity_net=-2364480825489862,
                liquidity_gross=2364480825489862,
            ),
            259380: UniswapV3LiquidityAtTick(
                liquidity_net=-1189170434129164,
                liquidity_gross=3335648726627380,
            ),
            259440: UniswapV3LiquidityAtTick(
                liquidity_net=-2460236402350929,
                liquidity_gross=2460236402350929,
            ),
            259500: UniswapV3LiquidityAtTick(
                liquidity_net=-535417280350002,
                liquidity_gross=535417280350002,
            ),
            259560: UniswapV3LiquidityAtTick(
                liquidity_net=-70350768590105,
                liquidity_gross=70350768590105,
            ),
            259620: UniswapV3LiquidityAtTick(
                liquidity_net=-1021778168444274,
                liquidity_gross=1021778168444274,
            ),
            259680: UniswapV3LiquidityAtTick(
                liquidity_net=-276049909077726,
                liquidity_gross=276049909077726,
            ),
            259740: UniswapV3LiquidityAtTick(
                liquidity_net=-2702260478188734,
                liquidity_gross=2702260478188734,
            ),
            259800: UniswapV3LiquidityAtTick(
                liquidity_net=-1163565651941686,
                liquidity_gross=1163565651941686,
            ),
            259860: UniswapV3LiquidityAtTick(
                liquidity_net=-710040743947057,
                liquidity_gross=710040743947057,
            ),
            259920: UniswapV3LiquidityAtTick(
                liquidity_net=-830712491263343,
                liquidity_gross=830712491263343,
            ),
            259980: UniswapV3LiquidityAtTick(
                liquidity_net=-4689302966707254,
                liquidity_gross=4692619231168190,
            ),
            260040: UniswapV3LiquidityAtTick(
                liquidity_net=-14807473500577126,
                liquidity_gross=14807473500577126,
            ),
            260100: UniswapV3LiquidityAtTick(
                liquidity_net=-3390547462651817,
                liquidity_gross=3390547462651817,
            ),
            260160: UniswapV3LiquidityAtTick(
                liquidity_net=-2213051740643507,
                liquidity_gross=2213051740643507,
            ),
            260220: UniswapV3LiquidityAtTick(
                liquidity_net=-16333264145033263,
                liquidity_gross=16333264145033263,
            ),
            260280: UniswapV3LiquidityAtTick(
                liquidity_net=-202256047650577199,
                liquidity_gross=202256047650577199,
            ),
            260340: UniswapV3LiquidityAtTick(
                liquidity_net=-2305739590349437,
                liquidity_gross=2305739590349437,
            ),
            260400: UniswapV3LiquidityAtTick(
                liquidity_net=-482148621584562,
                liquidity_gross=482148621584562,
            ),
            260460: UniswapV3LiquidityAtTick(
                liquidity_net=-5151763950535971,
                liquidity_gross=5151763950535971,
            ),
            260520: UniswapV3LiquidityAtTick(
                liquidity_net=-4949460699961,
                liquidity_gross=4949460699961,
            ),
            260580: UniswapV3LiquidityAtTick(
                liquidity_net=-792216572972459,
                liquidity_gross=792216572972459,
            ),
            260640: UniswapV3LiquidityAtTick(
                liquidity_net=-1659069837896683,
                liquidity_gross=1659069837896683,
            ),
            260700: UniswapV3LiquidityAtTick(
                liquidity_net=-471050672837794,
                liquidity_gross=471050672837794,
            ),
            260760: UniswapV3LiquidityAtTick(
                liquidity_net=-1427210936238126,
                liquidity_gross=1427210936238126,
            ),
            260820: UniswapV3LiquidityAtTick(
                liquidity_net=-160567446454122,
                liquidity_gross=160567446454122,
            ),
            260880: UniswapV3LiquidityAtTick(
                liquidity_net=-4660514545814,
                liquidity_gross=4660514545814,
            ),
            260940: UniswapV3LiquidityAtTick(
                liquidity_net=-949190291053485,
                liquidity_gross=949190291053485,
            ),
            261000: UniswapV3LiquidityAtTick(
                liquidity_net=-701508382094315,
                liquidity_gross=701508382094315,
            ),
            261060: UniswapV3LiquidityAtTick(
                liquidity_net=-192494897286115,
                liquidity_gross=192494897286115,
            ),
            261120: UniswapV3LiquidityAtTick(
                liquidity_net=-97448428510165,
                liquidity_gross=97448428510165,
            ),
            261180: UniswapV3LiquidityAtTick(
                liquidity_net=-768965647069,
                liquidity_gross=768965647069,
            ),
            261240: UniswapV3LiquidityAtTick(
                liquidity_net=-4290736218537318,
                liquidity_gross=4290736218537318,
            ),
            261300: UniswapV3LiquidityAtTick(
                liquidity_net=-5099128904602610,
                liquidity_gross=5099128904602610,
            ),
            261360: UniswapV3LiquidityAtTick(
                liquidity_net=-1609462935217957,
                liquidity_gross=1609462935217957,
            ),
            261420: UniswapV3LiquidityAtTick(
                liquidity_net=-1090436462449794,
                liquidity_gross=1090436462449794,
            ),
            261480: UniswapV3LiquidityAtTick(
                liquidity_net=-427691948100201,
                liquidity_gross=427691948100201,
            ),
            261540: UniswapV3LiquidityAtTick(
                liquidity_net=-25222997794671,
                liquidity_gross=25222997794671,
            ),
            261600: UniswapV3LiquidityAtTick(
                liquidity_net=-14172746964296,
                liquidity_gross=14172746964296,
            ),
            261660: UniswapV3LiquidityAtTick(
                liquidity_net=-33286914116176,
                liquidity_gross=33286914116176,
            ),
            261720: UniswapV3LiquidityAtTick(
                liquidity_net=-390958960205477,
                liquidity_gross=390958960205477,
            ),
            261780: UniswapV3LiquidityAtTick(
                liquidity_net=-2170475390497,
                liquidity_gross=2170475390497,
            ),
            261840: UniswapV3LiquidityAtTick(
                liquidity_net=-40685506569662,
                liquidity_gross=40685506569662,
            ),
            261900: UniswapV3LiquidityAtTick(
                liquidity_net=-768317113367493,
                liquidity_gross=768317113367493,
            ),
            261960: UniswapV3LiquidityAtTick(
                liquidity_net=-349615904032981,
                liquidity_gross=349615904032981,
            ),
            262020: UniswapV3LiquidityAtTick(
                liquidity_net=-91960406332603,
                liquidity_gross=91960406332603,
            ),
            262080: UniswapV3LiquidityAtTick(
                liquidity_net=-443641166532945,
                liquidity_gross=443641166532945,
            ),
            262140: UniswapV3LiquidityAtTick(
                liquidity_net=-38227973445101,
                liquidity_gross=38227973445101,
            ),
            262200: UniswapV3LiquidityAtTick(
                liquidity_net=-903825331042290,
                liquidity_gross=903825331042290,
            ),
            262260: UniswapV3LiquidityAtTick(
                liquidity_net=-13128065406663,
                liquidity_gross=13128065406663,
            ),
            262320: UniswapV3LiquidityAtTick(
                liquidity_net=-121957304230580,
                liquidity_gross=121957304230580,
            ),
            262380: UniswapV3LiquidityAtTick(
                liquidity_net=-424848915752761,
                liquidity_gross=424848915752761,
            ),
            262440: UniswapV3LiquidityAtTick(
                liquidity_net=-14652087781394115,
                liquidity_gross=14652087781394115,
            ),
            262500: UniswapV3LiquidityAtTick(
                liquidity_net=-1770941642899971,
                liquidity_gross=1770941642899971,
            ),
            262560: UniswapV3LiquidityAtTick(
                liquidity_net=-1647348926749984,
                liquidity_gross=1647348926749984,
            ),
            262620: UniswapV3LiquidityAtTick(
                liquidity_net=-142827709793555,
                liquidity_gross=142827709793555,
            ),
            262680: UniswapV3LiquidityAtTick(
                liquidity_net=-236240897581658,
                liquidity_gross=236240897581658,
            ),
            262740: UniswapV3LiquidityAtTick(
                liquidity_net=-3198063287248,
                liquidity_gross=3198063287248,
            ),
            262800: UniswapV3LiquidityAtTick(
                liquidity_net=-28405605552220,
                liquidity_gross=28405605552220,
            ),
            262860: UniswapV3LiquidityAtTick(
                liquidity_net=-127001888471234,
                liquidity_gross=127001888471234,
            ),
            262920: UniswapV3LiquidityAtTick(
                liquidity_net=-1875443486130867,
                liquidity_gross=1875443486130867,
            ),
            262980: UniswapV3LiquidityAtTick(
                liquidity_net=-338041427279495,
                liquidity_gross=338041427279495,
            ),
            263040: UniswapV3LiquidityAtTick(
                liquidity_net=-678050751391,
                liquidity_gross=678050751391,
            ),
            263100: UniswapV3LiquidityAtTick(
                liquidity_net=-2233936785039147,
                liquidity_gross=2233936785039147,
            ),
            263160: UniswapV3LiquidityAtTick(
                liquidity_net=-784249058176776,
                liquidity_gross=784249058176776,
            ),
            263220: UniswapV3LiquidityAtTick(
                liquidity_net=-184063967535124,
                liquidity_gross=184063967535124,
            ),
            263280: UniswapV3LiquidityAtTick(
                liquidity_net=-88096801401735,
                liquidity_gross=88096801401735,
            ),
            263340: UniswapV3LiquidityAtTick(
                liquidity_net=-198487481613624,
                liquidity_gross=198487481613624,
            ),
            263400: UniswapV3LiquidityAtTick(
                liquidity_net=-452889034759715,
                liquidity_gross=452889034759715,
            ),
            263460: UniswapV3LiquidityAtTick(
                liquidity_net=-606799883869495,
                liquidity_gross=606799883869495,
            ),
            263520: UniswapV3LiquidityAtTick(
                liquidity_net=-7638454561936103,
                liquidity_gross=7638454561936103,
            ),
            263580: UniswapV3LiquidityAtTick(
                liquidity_net=-126821769296502,
                liquidity_gross=126821769296502,
            ),
            263640: UniswapV3LiquidityAtTick(
                liquidity_net=-6110861158994498,
                liquidity_gross=6110861158994498,
            ),
            263700: UniswapV3LiquidityAtTick(
                liquidity_net=-1245903277890155,
                liquidity_gross=1245903277890155,
            ),
            263760: UniswapV3LiquidityAtTick(
                liquidity_net=-2890954460186027,
                liquidity_gross=2890954460186027,
            ),
            263820: UniswapV3LiquidityAtTick(
                liquidity_net=-2254895454670155,
                liquidity_gross=2254895454670155,
            ),
            263880: UniswapV3LiquidityAtTick(
                liquidity_net=-331818644781597,
                liquidity_gross=331818644781597,
            ),
            263940: UniswapV3LiquidityAtTick(
                liquidity_net=-1012237412969331,
                liquidity_gross=1012237412969331,
            ),
            264000: UniswapV3LiquidityAtTick(
                liquidity_net=-355849039791761,
                liquidity_gross=355849039791761,
            ),
            264060: UniswapV3LiquidityAtTick(
                liquidity_net=-830913905222007,
                liquidity_gross=830913905222007,
            ),
            264120: UniswapV3LiquidityAtTick(
                liquidity_net=-232657278104839,
                liquidity_gross=232657278104839,
            ),
            264180: UniswapV3LiquidityAtTick(
                liquidity_net=-223175670932954,
                liquidity_gross=223175670932954,
            ),
            264240: UniswapV3LiquidityAtTick(
                liquidity_net=-45075145059172,
                liquidity_gross=45075145059172,
            ),
            264300: UniswapV3LiquidityAtTick(
                liquidity_net=-514961937591143,
                liquidity_gross=514961937591143,
            ),
            264360: UniswapV3LiquidityAtTick(
                liquidity_net=-1836640574139053,
                liquidity_gross=1836640574139053,
            ),
            264420: UniswapV3LiquidityAtTick(
                liquidity_net=-399126992437212,
                liquidity_gross=399126992437212,
            ),
            264480: UniswapV3LiquidityAtTick(
                liquidity_net=-3386196039666481,
                liquidity_gross=3386196039666481,
            ),
            264540: UniswapV3LiquidityAtTick(
                liquidity_net=-169026268794532,
                liquidity_gross=169026268794532,
            ),
            264600: UniswapV3LiquidityAtTick(
                liquidity_net=-38164370768122,
                liquidity_gross=38164370768122,
            ),
            264660: UniswapV3LiquidityAtTick(
                liquidity_net=-6838638581919,
                liquidity_gross=6838638581919,
            ),
            264720: UniswapV3LiquidityAtTick(
                liquidity_net=-6934819008835,
                liquidity_gross=6934819008835,
            ),
            264780: UniswapV3LiquidityAtTick(
                liquidity_net=-634419296225677,
                liquidity_gross=634419296225677,
            ),
            264840: UniswapV3LiquidityAtTick(
                liquidity_net=-1894461556126,
                liquidity_gross=1894461556126,
            ),
            264900: UniswapV3LiquidityAtTick(
                liquidity_net=-1087328233426084,
                liquidity_gross=1087328233426084,
            ),
            264960: UniswapV3LiquidityAtTick(
                liquidity_net=-9592653857021,
                liquidity_gross=9592653857021,
            ),
            265020: UniswapV3LiquidityAtTick(
                liquidity_net=-964623108704,
                liquidity_gross=964623108704,
            ),
            265080: UniswapV3LiquidityAtTick(
                liquidity_net=-63222276742849,
                liquidity_gross=63222276742849,
            ),
            265140: UniswapV3LiquidityAtTick(
                liquidity_net=-417896448821485,
                liquidity_gross=417896448821485,
            ),
            265200: UniswapV3LiquidityAtTick(
                liquidity_net=-115757424347917,
                liquidity_gross=115757424347917,
            ),
            265260: UniswapV3LiquidityAtTick(
                liquidity_net=-247937698964119,
                liquidity_gross=247937698964119,
            ),
            265320: UniswapV3LiquidityAtTick(
                liquidity_net=-549333109125871,
                liquidity_gross=549333109125871,
            ),
            265380: UniswapV3LiquidityAtTick(
                liquidity_net=-682184643186092,
                liquidity_gross=682184643186092,
            ),
            265440: UniswapV3LiquidityAtTick(
                liquidity_net=-80696202126881,
                liquidity_gross=80696202126881,
            ),
            265500: UniswapV3LiquidityAtTick(
                liquidity_net=-646838003704937,
                liquidity_gross=646838003704937,
            ),
            265560: UniswapV3LiquidityAtTick(
                liquidity_net=-422423521362347,
                liquidity_gross=422423521362347,
            ),
            265620: UniswapV3LiquidityAtTick(
                liquidity_net=-11390495459371,
                liquidity_gross=11390495459371,
            ),
            265680: UniswapV3LiquidityAtTick(
                liquidity_net=-14264473913293,
                liquidity_gross=14264473913293,
            ),
            265740: UniswapV3LiquidityAtTick(
                liquidity_net=-809676720010,
                liquidity_gross=809676720010,
            ),
            265800: UniswapV3LiquidityAtTick(
                liquidity_net=-228442660393752,
                liquidity_gross=228442660393752,
            ),
            265980: UniswapV3LiquidityAtTick(
                liquidity_net=-1109158269877,
                liquidity_gross=1109158269877,
            ),
            266220: UniswapV3LiquidityAtTick(
                liquidity_net=-40234494434,
                liquidity_gross=40234494434,
            ),
            266520: UniswapV3LiquidityAtTick(
                liquidity_net=-95239241762338,
                liquidity_gross=95239241762338,
            ),
            266580: UniswapV3LiquidityAtTick(
                liquidity_net=-7345787342181,
                liquidity_gross=7345787342181,
            ),
            266640: UniswapV3LiquidityAtTick(
                liquidity_net=-893004067318,
                liquidity_gross=893004067318,
            ),
            266700: UniswapV3LiquidityAtTick(
                liquidity_net=-238322377607001,
                liquidity_gross=238322377607001,
            ),
            266940: UniswapV3LiquidityAtTick(
                liquidity_net=-3816659140777,
                liquidity_gross=3816659140777,
            ),
            267000: UniswapV3LiquidityAtTick(
                liquidity_net=-14630998912172,
                liquidity_gross=14630998912172,
            ),
            267060: UniswapV3LiquidityAtTick(
                liquidity_net=-2002323771185,
                liquidity_gross=2002323771185,
            ),
            267120: UniswapV3LiquidityAtTick(
                liquidity_net=-4026522741505,
                liquidity_gross=4026522741505,
            ),
            267180: UniswapV3LiquidityAtTick(
                liquidity_net=-878049993248372,
                liquidity_gross=878049993248372,
            ),
            267720: UniswapV3LiquidityAtTick(
                liquidity_net=-1432820167102,
                liquidity_gross=1432820167102,
            ),
            268260: UniswapV3LiquidityAtTick(
                liquidity_net=-41247994973279,
                liquidity_gross=41247994973279,
            ),
            268320: UniswapV3LiquidityAtTick(
                liquidity_net=-3921753993726,
                liquidity_gross=3921753993726,
            ),
            268620: UniswapV3LiquidityAtTick(
                liquidity_net=-7842385042332,
                liquidity_gross=7842385042332,
            ),
            268920: UniswapV3LiquidityAtTick(
                liquidity_net=-48860694293,
                liquidity_gross=48860694293,
            ),
            268980: UniswapV3LiquidityAtTick(
                liquidity_net=-233944583115,
                liquidity_gross=233944583115,
            ),
            269160: UniswapV3LiquidityAtTick(
                liquidity_net=-2921179233331,
                liquidity_gross=2921179233331,
            ),
            269220: UniswapV3LiquidityAtTick(
                liquidity_net=-116527715609,
                liquidity_gross=116527715609,
            ),
            269280: UniswapV3LiquidityAtTick(
                liquidity_net=-4816040963852,
                liquidity_gross=4816040963852,
            ),
            269400: UniswapV3LiquidityAtTick(
                liquidity_net=-515347242942616,
                liquidity_gross=515347242942616,
            ),
            270420: UniswapV3LiquidityAtTick(
                liquidity_net=-120389476854562,
                liquidity_gross=120389476854562,
            ),
            270540: UniswapV3LiquidityAtTick(
                liquidity_net=-34804953692,
                liquidity_gross=34804953692,
            ),
            270840: UniswapV3LiquidityAtTick(
                liquidity_net=-1649567047148,
                liquidity_gross=1649567047148,
            ),
            271080: UniswapV3LiquidityAtTick(
                liquidity_net=-103000565770,
                liquidity_gross=103000565770,
            ),
            271500: UniswapV3LiquidityAtTick(
                liquidity_net=-7387510370926,
                liquidity_gross=7387510370926,
            ),
            271680: UniswapV3LiquidityAtTick(
                liquidity_net=-6060018180622,
                liquidity_gross=6060018180622,
            ),
            272280: UniswapV3LiquidityAtTick(
                liquidity_net=-4538579708481,
                liquidity_gross=4538579708481,
            ),
            272520: UniswapV3LiquidityAtTick(
                liquidity_net=-202950841112641,
                liquidity_gross=202950841112641,
            ),
            272580: UniswapV3LiquidityAtTick(
                liquidity_net=-1537745525855,
                liquidity_gross=1537745525855,
            ),
            276300: UniswapV3LiquidityAtTick(
                liquidity_net=-2543662313063992,
                liquidity_gross=2543662313063992,
            ),
            278940: UniswapV3LiquidityAtTick(
                liquidity_net=-5164088638751,
                liquidity_gross=5164088638751,
            ),
            280080: UniswapV3LiquidityAtTick(
                liquidity_net=-24418298514298,
                liquidity_gross=24418298514298,
            ),
            280500: UniswapV3LiquidityAtTick(
                liquidity_net=-6081508773089,
                liquidity_gross=6081508773089,
            ),
            281220: UniswapV3LiquidityAtTick(
                liquidity_net=-33872560240,
                liquidity_gross=33872560240,
            ),
            282180: UniswapV3LiquidityAtTick(
                liquidity_net=-2366835767353,
                liquidity_gross=2366835767353,
            ),
            283260: UniswapV3LiquidityAtTick(
                liquidity_net=-12241814398321,
                liquidity_gross=12241814398321,
            ),
            283800: UniswapV3LiquidityAtTick(
                liquidity_net=-1410203189999,
                liquidity_gross=1410203189999,
            ),
            329460: UniswapV3LiquidityAtTick(
                liquidity_net=-643796387349,
                liquidity_gross=643796387349,
            ),
            368460: UniswapV3LiquidityAtTick(
                liquidity_net=-486522234414488,
                liquidity_gross=486522234414488,
            ),
            391440: UniswapV3LiquidityAtTick(
                liquidity_net=-456406095307,
                liquidity_gross=456406095307,
            ),
            437520: UniswapV3LiquidityAtTick(
                liquidity_net=-10943161472679,
                liquidity_gross=10943161472679,
            ),
            887220: UniswapV3LiquidityAtTick(
                liquidity_net=-97132329311971,
                liquidity_gross=97132329311971,
            ),
            92040: UniswapV3LiquidityAtTick(
                liquidity_net=1620915153,
                liquidity_gross=1620915153,
            ),
            92100: UniswapV3LiquidityAtTick(
                liquidity_net=3974353998848,
                liquidity_gross=3974353998848,
            ),
        },
    )

    pool._state = UniswapV3PoolState(
        address=pool.address,
        liquidity=1612978974357835825,
        sqrt_price_x96=31549217861118002279483878013792428,
        tick=257907,
        tick_bitmap=pool.tick_bitmap,
        tick_data=pool.tick_data,
        block=pool.update_block,
    )
    pool._initial_state_block = 0

    return pool


@pytest.fixture
def wbtc_weth_arb(
    wbtc_weth_v2_lp: UniswapV2Pool,
    wbtc_weth_v3_lp: UniswapV3Pool,
    weth_token: Erc20Token,
):
    return UniswapLpCycle(
        id="test_arb",
        input_token=weth_token,
        swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
        max_input=100 * 10**18,
    )


class MockLiquidityPool(UniswapV2Pool):
    def __init__(self) -> None:
        self._state = UniswapV2PoolState(
            address=ZERO_ADDRESS,
            reserves_token0=0,
            reserves_token1=0,
            block=None,
        )
        self._state_lock = Lock()
        self._subscribers = WeakSet()


class MockV3LiquidityPool(UniswapV3Pool):
    def __init__(self) -> None:
        self._state_lock = Lock()
        self._subscribers = WeakSet()


def test_create_with_either_token_input(
    wbtc_weth_v2_lp: UniswapV2Pool,
    wbtc_weth_v3_lp: UniswapV3Pool,
    weth_token: Erc20Token,
    wbtc_token: Erc20Token,
):
    UniswapLpCycle(
        id="test_arb",
        input_token=weth_token,
        swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
        max_input=100 * 10**18,
    )
    UniswapLpCycle(
        id="test_arb",
        input_token=wbtc_token,
        swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
        max_input=100 * 10**18,
    )


def test_arbitrage_with_overrides(
    wbtc_weth_arb: UniswapLpCycle,
    wbtc_weth_v2_lp: UniswapV2Pool,
    wbtc_weth_v3_lp: UniswapV3Pool,
    weth_token: Erc20Token,
    wbtc_token: Erc20Token,
):
    v2_pool_state_override = UniswapV2PoolState(
        address=wbtc_weth_v2_lp.address,
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
        block=None,
    )

    v3_pool_state_override = UniswapV3PoolState(
        address=wbtc_weth_v3_lp.address,
        block=None,
        liquidity=1533143241938066251,
        sqrt_price_x96=31881290961944305252140777263703426,
        tick=258116,
        tick_bitmap=wbtc_weth_v3_lp.tick_bitmap,
        tick_data=wbtc_weth_v3_lp.tick_data,
    )

    overrides: dict[Pool, PoolState]

    # Override both pools
    overrides = {
        wbtc_weth_v2_lp: v2_pool_state_override,
        wbtc_weth_v3_lp: v3_pool_state_override,
    }

    with pytest.raises(ArbitrageError):
        wbtc_weth_arb.calculate(state_overrides=overrides)

    # Override V2 pool only
    overrides = {
        wbtc_weth_v2_lp: v2_pool_state_override,
    }

    with pytest.raises(ArbitrageError):
        wbtc_weth_arb.calculate(state_overrides=overrides)

    # Override V3 pool only
    overrides = {
        wbtc_weth_v3_lp: v3_pool_state_override,
    }

    result = wbtc_weth_arb.calculate(state_overrides=overrides)
    assert result.input_amount == 20454968409226055680
    assert result.profit_amount == 163028226755627521

    # Irrelevant V2 and V3 mocked pools, only the address is changed.
    irrelevant_v2_pool = MockLiquidityPool()
    irrelevant_v2_pool.address = get_checksum_address("0x0000000000000000000000000000000000000069")
    irrelevant_v2_pool.name = "WBTC-WETH (V2, 0.30%)"
    irrelevant_v2_pool.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    irrelevant_v2_pool.fee_token0 = Fraction(3, 1000)
    irrelevant_v2_pool.fee_token1 = Fraction(3, 1000)
    irrelevant_v2_pool._state = UniswapV2PoolState(
        address=irrelevant_v2_pool.address,
        reserves_token0=16231137593,
        reserves_token1=2571336301536722443178,
        block=None,
    )
    irrelevant_v2_pool.token0 = wbtc_token
    irrelevant_v2_pool.token1 = weth_token

    irrelevant_v3_pool = MockV3LiquidityPool()
    irrelevant_v3_pool.address = get_checksum_address("0x0000000000000000000000000000000000000420")
    irrelevant_v3_pool._state = UniswapV3PoolState(
        address=irrelevant_v3_pool.address,
        block=None,
        liquidity=1612978974357835825,
        sqrt_price_x96=31549217861118002279483878013792428,
        tick=257907,
        tick_bitmap={},
        tick_data={},
    )
    irrelevant_v3_pool.name = "WBTC-WETH (V3, 0.30%)"
    irrelevant_v3_pool.factory = get_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984")
    irrelevant_v3_pool.fee = 3000
    irrelevant_v3_pool.token0 = wbtc_token
    irrelevant_v3_pool.token1 = weth_token
    irrelevant_v3_pool.sparse_liquidity_map = False
    irrelevant_v3_pool.tick_spacing = 60

    overrides = {
        irrelevant_v2_pool: v2_pool_state_override,  # <--- entry should be ignored
        wbtc_weth_v3_lp: v3_pool_state_override,
    }

    # This should equal the result from the test with the V3 override only
    result = wbtc_weth_arb.calculate(state_overrides=overrides)
    assert result.input_amount == 20454968409226055680
    assert result.profit_amount == 163028226755627521


async def test_pickle_uniswap_lp_cycle_with_camelot_pool(fork_arbitrum_full: AnvilFork):
    # Arbitrum-specific token addresses
    weth_address = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"

    camelot_weth_wbtc_pool_address = "0x96059759C6492fb4e8a9777b65f307F2C811a34F"
    sushi_v2_weth_wbtc_pool_address = "0x515e252b2b5c22b4b2b6Df66c2eBeeA871AA4d69"

    set_web3(fork_arbitrum_full.w3)

    _weth = Erc20Token(weth_address)

    camelot_lp = CamelotLiquidityPool(address=camelot_weth_wbtc_pool_address)
    sushi_lp = UniswapV2Pool(address=sushi_v2_weth_wbtc_pool_address)

    arb = UniswapLpCycle(
        id="test_arb",
        input_token=_weth,
        swap_pools=[camelot_lp, sushi_lp],
        max_input=100 * 10**18,
    )
    pickle.dumps(arb)

    loop = asyncio.get_running_loop()

    with concurrent.futures.ProcessPoolExecutor(
        mp_context=multiprocessing.get_context("spawn"),
    ) as executor:
        _tasks = [
            loop.run_in_executor(
                executor,
                arb.calculate,
            )
            for _ in range(8)
        ]

        for task in asyncio.as_completed(_tasks):
            with contextlib.suppress(RateOfExchangeBelowMinimum):
                await task

    with contextlib.suppress(RateOfExchangeBelowMinimum):
        arb.calculate()


async def test_process_pool_calculation(
    wbtc_weth_arb: UniswapLpCycle, wbtc_weth_v3_lp: UniswapV3Pool, weth_token: Erc20Token
):
    start = time.perf_counter()

    v3_pool_state_override = UniswapV3PoolState(
        address=wbtc_weth_v3_lp.address,
        block=None,
        liquidity=1533143241938066251,
        sqrt_price_x96=31881290961944305252140777263703426,
        tick=258116,
        tick_bitmap=wbtc_weth_v3_lp.tick_bitmap,
        tick_data=wbtc_weth_v3_lp.tick_data,
    )

    overrides: dict[Pool, PoolState] = {
        wbtc_weth_v3_lp: v3_pool_state_override,
    }

    with concurrent.futures.ProcessPoolExecutor(
        mp_context=multiprocessing.get_context("spawn"),
    ) as executor:
        with pytest.raises(ArbitrageError):
            await wbtc_weth_arb.calculate_with_pool(executor=executor)

        future = await wbtc_weth_arb.calculate_with_pool(
            executor=executor,
            state_overrides=overrides,
        )
        result = await future
        assert result == ArbitrageCalculationResult(
            id="test_arb",
            input_token=weth_token,
            profit_token=weth_token,
            input_amount=20454968409226055680,
            profit_amount=163028226755627521,
            swap_amounts=(
                UniswapV2PoolSwapAmounts(
                    pool=wbtc_weth_arb.swap_pools[0].address,
                    amounts_in=(0, 20454968409226055680),
                    amounts_out=(127718318, 0),
                ),
                UniswapV3PoolSwapAmounts(
                    pool=wbtc_weth_arb.swap_pools[1].address,
                    amount_in=127718318,
                    amount_out=20617996635981683201,
                    amount_specified=127718318,
                    zero_for_one=True,
                    sqrt_price_limit_x96=4295128740,
                ),
            ),
            state_block=None,
        )

        # Saturate the process pool executor with multiple calculations.
        # Should reveal cases of excessive latency.
        num_futures = 64
        calculation_futures = [
            await wbtc_weth_arb.calculate_with_pool(
                executor=executor,
                state_overrides=overrides,
            )
            for _ in range(num_futures)
        ]

        assert len(calculation_futures) == num_futures
        for i, task in enumerate(asyncio.as_completed(calculation_futures)):
            await task
            print(
                f"Completed process_pool calc #{i}, {time.perf_counter() - start:.2f}s since start"
            )
        print(f"Completed {num_futures} calculations in {time.perf_counter() - start:.1f}s")

        assert isinstance(wbtc_weth_arb.swap_pools[1], UniswapV3Pool)
        wbtc_weth_arb.swap_pools[1].sparse_liquidity_map = True
        with pytest.raises(DegenbotValueError, match=r"One or more V3 pools has a sparse bitmap."):
            await wbtc_weth_arb.calculate_with_pool(
                executor=executor,
                state_overrides=overrides,
            )


def test_pre_calc_check(weth_token: Erc20Token, wbtc_token: Erc20Token):
    lp_1 = MockLiquidityPool()
    lp_1.name = "WBTC-WETH (V2, 0.30%)"
    lp_1.address = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
    lp_1.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    lp_1.fee_token0 = Fraction(3, 1000)
    lp_1.fee_token1 = Fraction(3, 1000)
    lp_1._state = UniswapV2PoolState(
        address=lp_1.address,
        reserves_token0=16000000000,
        reserves_token1=2500000000000000000000,
        block=None,
    )
    lp_1.token0 = wbtc_token
    lp_1.token1 = weth_token

    lp_2 = MockLiquidityPool()
    lp_2.name = "WBTC-WETH (V2, 0.30%)"
    lp_2.address = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D941")
    lp_2.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    lp_2.fee_token0 = Fraction(3, 1000)
    lp_2.fee_token1 = Fraction(3, 1000)
    lp_2._state = UniswapV2PoolState(
        address=lp_2.address,
        reserves_token0=15000000000,
        reserves_token1=2500000000000000000000,
        block=None,
    )
    lp_2.token0 = wbtc_token
    lp_2.token1 = weth_token

    # lp_1 price = 2500000000000000000000/16000000000 ~= 156250000000.00
    # lp_2 price = 2500000000000000000000/15000000000 ~= 166666666666.67

    # This arb path should result in a profitable calculation, since token1
    # price is higher in the second pool.
    # i.e. sell overpriced token0 (WETH) in pool0 for token1 (WBTC),
    # buy underpriced token0 (WETH) in pool1 with token1 (WBTC)
    arb = UniswapLpCycle(
        id="test_arb",
        input_token=weth_token,
        swap_pools=[lp_1, lp_2],
        max_input=100 * 10**18,
    )
    result = arb.calculate()
    arb.generate_payloads(
        from_address=ZERO_ADDRESS,
        pool_swap_amounts=result.swap_amounts,
        swap_amount=result.input_amount,
    )

    # This arb path should result in an unprofitable calculation, since token1
    # price is lower in the second pool.
    # i.e. sell underpriced token0 (WETH) in pool0 for token1 (WBTC),
    # buy overpriced token0 (WETH) in pool1 with token1 (WBTC)
    arb = UniswapLpCycle(
        id="test_arb", input_token=weth_token, swap_pools=[lp_2, lp_1], max_input=100 * 10**18
    )
    with pytest.raises(RateOfExchangeBelowMinimum):
        arb.calculate()


def test_bad_pool_in_constructor(
    wbtc_weth_v2_lp: UniswapV2Pool, wbtc_weth_v3_lp: UniswapV3Pool, weth_token: Erc20Token
):
    with pytest.raises(
        DegenbotValueError, match=f"Incompatible pool type \\({type(None)}\\) provided."
    ):
        UniswapLpCycle(
            id="test_arb",
            input_token=weth_token,
            swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp, None],  # type: ignore[list-item]
            max_input=100 * 10**18,
        )


def test_no_max_input(
    wbtc_weth_v2_lp: UniswapV2Pool, wbtc_weth_v3_lp: UniswapV3Pool, weth_token: Erc20Token
):
    arb = UniswapLpCycle(
        id="test_arb",
        input_token=weth_token,
        swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
    )
    assert arb.max_input == 100 * 10**18


def test_zero_max_input(
    wbtc_weth_v2_lp: UniswapV2Pool, wbtc_weth_v3_lp: UniswapV3Pool, weth_token: Erc20Token
):
    with pytest.raises(DegenbotValueError, match=r"Maximum input must be positive."):
        UniswapLpCycle(
            id="test_arb",
            input_token=weth_token,
            swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
            max_input=0,
        )


def test_arbitrage_helper_subscriptions(
    wbtc_weth_arb: UniswapLpCycle, wbtc_weth_v2_lp: UniswapV2Pool, wbtc_weth_v3_lp: UniswapV3Pool
):
    assert wbtc_weth_arb in wbtc_weth_v2_lp._subscribers
    assert wbtc_weth_arb in wbtc_weth_v3_lp._subscribers
    assert wbtc_weth_arb in wbtc_weth_v2_lp.get_arbitrage_helpers()
    assert wbtc_weth_arb in wbtc_weth_v3_lp.get_arbitrage_helpers()

    pool_subscriber = FakeSubscriber()
    pool_subscriber.subscribe(publisher=wbtc_weth_v2_lp)
    pool_subscriber.subscribe(publisher=wbtc_weth_v3_lp)

    assert len(pool_subscriber.inbox) == 0

    # Trigger pool state updates
    wbtc_weth_v2_lp.external_update(
        update=UniswapV2PoolExternalUpdate(
            block_number=wbtc_weth_v2_lp.update_block,
            reserves_token0=69,
            reserves_token1=420,
        )
    )

    wbtc_weth_v3_lp.external_update(
        update=UniswapV3PoolExternalUpdate(
            block_number=wbtc_weth_v3_lp.update_block,
            liquidity=69_420,
            sqrt_price_x96=1,
            tick=-1,
        )
    )

    # Verify the subscribers have received state update notifications
    assert len(pool_subscriber.inbox) == 2
    assert pool_subscriber.inbox[0]["from"] == wbtc_weth_v2_lp
    assert pool_subscriber.inbox[1]["from"] == wbtc_weth_v3_lp
    assert isinstance(pool_subscriber.inbox[0]["message"], UniswapV2PoolStateUpdated)
    assert isinstance(pool_subscriber.inbox[1]["message"], UniswapV3PoolStateUpdated)

    pool_subscriber.unsubscribe(wbtc_weth_v2_lp)
    pool_subscriber.unsubscribe(wbtc_weth_v3_lp)


def test_pool_helper_unsubscriptions(
    wbtc_weth_arb: UniswapLpCycle, wbtc_weth_v2_lp: UniswapV2Pool, wbtc_weth_v3_lp: UniswapV3Pool
):
    assert wbtc_weth_arb in wbtc_weth_v2_lp._subscribers
    assert wbtc_weth_arb in wbtc_weth_v3_lp._subscribers

    wbtc_weth_v2_lp.unsubscribe(wbtc_weth_arb)
    wbtc_weth_v3_lp.unsubscribe(wbtc_weth_arb)

    assert wbtc_weth_arb not in wbtc_weth_v2_lp._subscribers
    assert wbtc_weth_arb not in wbtc_weth_v3_lp._subscribers
