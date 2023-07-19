from fractions import Fraction
from typing import Sequence, Tuple, Union

import web3

from degenbot import Erc20Token
from degenbot.arbitrage import UniswapLpCycle
from degenbot.uniswap.v2.liquidity_pool import (
    LiquidityPool,
    UniswapV2PoolState,
)
from degenbot.uniswap.v3.v3_liquidity_pool import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3PoolState,
    V3LiquidityPool,
)


class MockErc20Token(Erc20Token):
    def __init__(self):
        pass


class MockLiquidityPool(LiquidityPool):
    def __init__(self):
        pass


class MockV3LiquidityPool(V3LiquidityPool):
    def __init__(self):
        pass


wbtc = MockErc20Token()
wbtc.address = web3.Web3.toChecksumAddress(
    "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
)
wbtc.decimals = 8
wbtc.name = "Wrapped BTC"
wbtc.symbol = "WBTC"

weth = MockErc20Token()
weth.address = web3.Web3.toChecksumAddress(
    "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
)
weth.decimals = 18
weth.name = "Wrapped Ether"
weth.symbol = "WETH"


v2_lp = MockLiquidityPool()
v2_lp.name = "WBTC-WETH (V2, 0.30%)"
v2_lp.address = web3.Web3.toChecksumAddress(
    "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
)
v2_lp.factory = web3.Web3.toChecksumAddress(
    "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
)
v2_lp.fee = None
v2_lp.fee_token0 = Fraction(3, 1000)
v2_lp.fee_token1 = Fraction(3, 1000)
v2_lp.reserves_token0 = 16231137593
v2_lp.reserves_token1 = 2571336301536722443178
v2_lp.token0 = wbtc
v2_lp.token1 = weth
v2_lp._update_pool_state()

v3_lp = MockV3LiquidityPool()
v3_lp.name = "WBTC-WETH (V3, 0.30%)"
v3_lp.address = web3.Web3.toChecksumAddress(
    "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"
)
v3_lp.factory = web3.Web3.toChecksumAddress(
    "0x1F98431c8aD98523631AE4a59f267346ea31F984"
)
v3_lp.fee = 3000
v3_lp.token0 = wbtc
v3_lp.token1 = weth
v3_lp.liquidity = 1612978974357835825
v3_lp.liquidity_update_block = 1
v3_lp.sqrt_price_x96 = 31549217861118002279483878013792428
v3_lp.sparse_bitmap = False
v3_lp.tick = 257907
v3_lp.tick_spacing = 60
v3_lp.tick_bitmap = {
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
}
v3_lp.tick_data = {
    -887160: UniswapV3LiquidityAtTick(
        liquidityNet=80064092962998,
        liquidityGross=80064092962998,
    ),
    -887220: UniswapV3LiquidityAtTick(
        liquidityNet=82174936226787,
        liquidityGross=82174936226787,
    ),
    -92100: UniswapV3LiquidityAtTick(
        liquidityNet=456406095307,
        liquidityGross=456406095307,
    ),
    0: UniswapV3LiquidityAtTick(
        liquidityNet=10943161472679,
        liquidityGross=10943161472679,
    ),
    152640: UniswapV3LiquidityAtTick(
        liquidityNet=40166358066529,
        liquidityGross=40166358066529,
    ),
    152880: UniswapV3LiquidityAtTick(
        liquidityNet=-40166358066529,
        liquidityGross=40166358066529,
    ),
    153360: UniswapV3LiquidityAtTick(
        liquidityNet=155684471699734,
        liquidityGross=155684471699734,
    ),
    153420: UniswapV3LiquidityAtTick(
        liquidityNet=-155684471699734,
        liquidityGross=155684471699734,
    ),
    161220: UniswapV3LiquidityAtTick(
        liquidityNet=181470429131796,
        liquidityGross=181470429131796,
    ),
    176220: UniswapV3LiquidityAtTick(
        liquidityNet=358354299830,
        liquidityGross=358354299830,
    ),
    183180: UniswapV3LiquidityAtTick(
        liquidityNet=-358354299830,
        liquidityGross=358354299830,
    ),
    184200: UniswapV3LiquidityAtTick(
        liquidityNet=-79344120619876,
        liquidityGross=80784065306120,
    ),
    206580: UniswapV3LiquidityAtTick(
        liquidityNet=54402074932,
        liquidityGross=54402074932,
    ),
    206640: UniswapV3LiquidityAtTick(
        liquidityNet=-54402074932,
        liquidityGross=54402074932,
    ),
    214920: UniswapV3LiquidityAtTick(
        liquidityNet=31454785349939,
        liquidityGross=31454785349939,
    ),
    216420: UniswapV3LiquidityAtTick(
        liquidityNet=3151604679224,
        liquidityGross=3151604679224,
    ),
    217740: UniswapV3LiquidityAtTick(
        liquidityNet=-31454785349939,
        liquidityGross=31454785349939,
    ),
    223320: UniswapV3LiquidityAtTick(
        liquidityNet=340095002,
        liquidityGross=340095002,
    ),
    230100: UniswapV3LiquidityAtTick(
        liquidityNet=10499785476924,
        liquidityGross=10499785476924,
    ),
    230160: UniswapV3LiquidityAtTick(
        liquidityNet=1670921212240,
        liquidityGross=1670921212240,
    ),
    230220: UniswapV3LiquidityAtTick(
        liquidityNet=248504515915,
        liquidityGross=248504515915,
    ),
    230280: UniswapV3LiquidityAtTick(
        liquidityNet=11377831611211,
        liquidityGross=14720354225695,
    ),
    231180: UniswapV3LiquidityAtTick(
        liquidityNet=10325006026,
        liquidityGross=10325006026,
    ),
    231240: UniswapV3LiquidityAtTick(
        liquidityNet=9636087679,
        liquidityGross=9636087679,
    ),
    231300: UniswapV3LiquidityAtTick(
        liquidityNet=4067292562252,
        liquidityGross=4067292562252,
    ),
    231360: UniswapV3LiquidityAtTick(
        liquidityNet=10098305733,
        liquidityGross=10098305733,
    ),
    231420: UniswapV3LiquidityAtTick(
        liquidityNet=105440390087,
        liquidityGross=105440390087,
    ),
    231480: UniswapV3LiquidityAtTick(
        liquidityNet=389377464792,
        liquidityGross=389377464792,
    ),
    231540: UniswapV3LiquidityAtTick(
        liquidityNet=62725860270,
        liquidityGross=62725860270,
    ),
    234060: UniswapV3LiquidityAtTick(
        liquidityNet=24418298514298,
        liquidityGross=24418298514298,
    ),
    234300: UniswapV3LiquidityAtTick(
        liquidityNet=-379213263309,
        liquidityGross=379213263309,
    ),
    235620: UniswapV3LiquidityAtTick(
        liquidityNet=13143858905286,
        liquidityGross=13143858905286,
    ),
    237180: UniswapV3LiquidityAtTick(
        liquidityNet=4066391507115,
        liquidityGross=4218743418661,
    ),
    237300: UniswapV3LiquidityAtTick(
        liquidityNet=2512358234332724,
        liquidityGross=2512358234332724,
    ),
    237420: UniswapV3LiquidityAtTick(
        liquidityNet=788243782725,
        liquidityGross=788243782725,
    ),
    239460: UniswapV3LiquidityAtTick(
        liquidityNet=8584538262056,
        liquidityGross=8584538262056,
    ),
    239760: UniswapV3LiquidityAtTick(
        liquidityNet=3095863280482,
        liquidityGross=3095863280482,
    ),
    240420: UniswapV3LiquidityAtTick(
        liquidityNet=12999196246833,
        liquidityGross=12999196246833,
    ),
    240780: UniswapV3LiquidityAtTick(
        liquidityNet=7818819408261,
        liquidityGross=7818819408261,
    ),
    241260: UniswapV3LiquidityAtTick(
        liquidityNet=1856215464927,
        liquidityGross=1856215464927,
    ),
    241380: UniswapV3LiquidityAtTick(
        liquidityNet=7814421206784,
        liquidityGross=7814421206784,
    ),
    242340: UniswapV3LiquidityAtTick(
        liquidityNet=3373862096954,
        liquidityGross=3373862096954,
    ),
    242820: UniswapV3LiquidityAtTick(
        liquidityNet=2611786952298,
        liquidityGross=2611786952298,
    ),
    243360: UniswapV3LiquidityAtTick(
        liquidityNet=103000565770,
        liquidityGross=103000565770,
    ),
    244140: UniswapV3LiquidityAtTick(
        liquidityNet=217561198854734,
        liquidityGross=217561198854734,
    ),
    244860: UniswapV3LiquidityAtTick(
        liquidityNet=893004067318,
        liquidityGross=893004067318,
    ),
    245520: UniswapV3LiquidityAtTick(
        liquidityNet=266126101590362,
        liquidityGross=266126101590362,
    ),
    245700: UniswapV3LiquidityAtTick(
        liquidityNet=16758180163,
        liquidityGross=16758180163,
    ),
    245940: UniswapV3LiquidityAtTick(
        liquidityNet=76701235421656,
        liquidityGross=76701235421656,
    ),
    246180: UniswapV3LiquidityAtTick(
        liquidityNet=4890971540,
        liquidityGross=4890971540,
    ),
    246360: UniswapV3LiquidityAtTick(
        liquidityNet=1235247247938725,
        liquidityGross=1235247247938725,
    ),
    246420: UniswapV3LiquidityAtTick(
        liquidityNet=69925333162032,
        liquidityGross=69925333162032,
    ),
    246480: UniswapV3LiquidityAtTick(
        liquidityNet=83562433586,
        liquidityGross=83562433586,
    ),
    246540: UniswapV3LiquidityAtTick(
        liquidityNet=837767553892,
        liquidityGross=837767553892,
    ),
    246600: UniswapV3LiquidityAtTick(
        liquidityNet=4814170393189,
        liquidityGross=4814170393189,
    ),
    246660: UniswapV3LiquidityAtTick(
        liquidityNet=14690532940788,
        liquidityGross=14690532940788,
    ),
    246720: UniswapV3LiquidityAtTick(
        liquidityNet=243908811117210,
        liquidityGross=243908811117210,
    ),
    246780: UniswapV3LiquidityAtTick(
        liquidityNet=233944583115,
        liquidityGross=233944583115,
    ),
    246840: UniswapV3LiquidityAtTick(
        liquidityNet=1143995552,
        liquidityGross=1143995552,
    ),
    246900: UniswapV3LiquidityAtTick(
        liquidityNet=263365923015,
        liquidityGross=263365923015,
    ),
    246960: UniswapV3LiquidityAtTick(
        liquidityNet=14264473913293,
        liquidityGross=14264473913293,
    ),
    247320: UniswapV3LiquidityAtTick(
        liquidityNet=48519918747269,
        liquidityGross=48519918747269,
    ),
    247380: UniswapV3LiquidityAtTick(
        liquidityNet=117622952159219,
        liquidityGross=117622952159219,
    ),
    247440: UniswapV3LiquidityAtTick(
        liquidityNet=665770897850117,
        liquidityGross=665770897850117,
    ),
    247620: UniswapV3LiquidityAtTick(
        liquidityNet=487875820420,
        liquidityGross=487875820420,
    ),
    247740: UniswapV3LiquidityAtTick(
        liquidityNet=11523276594189,
        liquidityGross=11523276594189,
    ),
    247860: UniswapV3LiquidityAtTick(
        liquidityNet=1649567047148,
        liquidityGross=1649567047148,
    ),
    247920: UniswapV3LiquidityAtTick(
        liquidityNet=5342639837069495,
        liquidityGross=5342639837069495,
    ),
    247980: UniswapV3LiquidityAtTick(
        liquidityNet=23028334649368,
        liquidityGross=23028334649368,
    ),
    248040: UniswapV3LiquidityAtTick(
        liquidityNet=252942942781,
        liquidityGross=252942942781,
    ),
    248100: UniswapV3LiquidityAtTick(
        liquidityNet=24372131940667,
        liquidityGross=24372131940667,
    ),
    248160: UniswapV3LiquidityAtTick(
        liquidityNet=102373720892200,
        liquidityGross=102373720892200,
    ),
    248280: UniswapV3LiquidityAtTick(
        liquidityNet=5974932615038,
        liquidityGross=5974932615038,
    ),
    248340: UniswapV3LiquidityAtTick(
        liquidityNet=745663361773900,
        liquidityGross=745663361773900,
    ),
    248400: UniswapV3LiquidityAtTick(
        liquidityNet=106096220552937,
        liquidityGross=106096220552937,
    ),
    248460: UniswapV3LiquidityAtTick(
        liquidityNet=15823287912623,
        liquidityGross=15823287912623,
    ),
    248520: UniswapV3LiquidityAtTick(
        liquidityNet=339165199161295,
        liquidityGross=339165199161295,
    ),
    248580: UniswapV3LiquidityAtTick(
        liquidityNet=96340504853,
        liquidityGross=96340504853,
    ),
    248640: UniswapV3LiquidityAtTick(
        liquidityNet=34036669144137,
        liquidityGross=34036669144137,
    ),
    248700: UniswapV3LiquidityAtTick(
        liquidityNet=106728159577006,
        liquidityGross=106728159577006,
    ),
    248760: UniswapV3LiquidityAtTick(
        liquidityNet=130017625795223,
        liquidityGross=130017625795223,
    ),
    248820: UniswapV3LiquidityAtTick(
        liquidityNet=231267402130471,
        liquidityGross=231267402130471,
    ),
    248880: UniswapV3LiquidityAtTick(
        liquidityNet=1191600446578,
        liquidityGross=1191600446578,
    ),
    248940: UniswapV3LiquidityAtTick(
        liquidityNet=10262040820492,
        liquidityGross=10262040820492,
    ),
    249000: UniswapV3LiquidityAtTick(
        liquidityNet=375045998526336,
        liquidityGross=375045998526336,
    ),
    249060: UniswapV3LiquidityAtTick(
        liquidityNet=14220938232775,
        liquidityGross=14220938232775,
    ),
    249120: UniswapV3LiquidityAtTick(
        liquidityNet=8546121898214,
        liquidityGross=8546121898214,
    ),
    249180: UniswapV3LiquidityAtTick(
        liquidityNet=805435724180,
        liquidityGross=805435724180,
    ),
    249240: UniswapV3LiquidityAtTick(
        liquidityNet=2458914255421348,
        liquidityGross=2458914255421348,
    ),
    249300: UniswapV3LiquidityAtTick(
        liquidityNet=753689707471433,
        liquidityGross=753689707471433,
    ),
    249360: UniswapV3LiquidityAtTick(
        liquidityNet=250509876783271,
        liquidityGross=250509876783271,
    ),
    249420: UniswapV3LiquidityAtTick(
        liquidityNet=15459403677425,
        liquidityGross=15459403677425,
    ),
    249480: UniswapV3LiquidityAtTick(
        liquidityNet=204634341236947,
        liquidityGross=204634341236947,
    ),
    249540: UniswapV3LiquidityAtTick(
        liquidityNet=536139191751638,
        liquidityGross=536139191751638,
    ),
    249600: UniswapV3LiquidityAtTick(
        liquidityNet=1362751680139023,
        liquidityGross=1362751680139023,
    ),
    249660: UniswapV3LiquidityAtTick(
        liquidityNet=75072282227263,
        liquidityGross=75072282227263,
    ),
    249720: UniswapV3LiquidityAtTick(
        liquidityNet=128404454018236,
        liquidityGross=128404454018236,
    ),
    249780: UniswapV3LiquidityAtTick(
        liquidityNet=752941312872158,
        liquidityGross=752941312872158,
    ),
    249840: UniswapV3LiquidityAtTick(
        liquidityNet=150303272592282,
        liquidityGross=150303272592282,
    ),
    249900: UniswapV3LiquidityAtTick(
        liquidityNet=2999000185960519,
        liquidityGross=2999000185960519,
    ),
    249960: UniswapV3LiquidityAtTick(
        liquidityNet=1897487030889456,
        liquidityGross=1897487030889456,
    ),
    250020: UniswapV3LiquidityAtTick(
        liquidityNet=338541271525525,
        liquidityGross=338541271525525,
    ),
    250080: UniswapV3LiquidityAtTick(
        liquidityNet=1019910411632819,
        liquidityGross=1019910411632819,
    ),
    250140: UniswapV3LiquidityAtTick(
        liquidityNet=349559136168929,
        liquidityGross=349559136168929,
    ),
    250200: UniswapV3LiquidityAtTick(
        liquidityNet=357255210710225,
        liquidityGross=357255210710225,
    ),
    250260: UniswapV3LiquidityAtTick(
        liquidityNet=236399678077506,
        liquidityGross=236399678077506,
    ),
    250320: UniswapV3LiquidityAtTick(
        liquidityNet=149802042174914,
        liquidityGross=149802042174914,
    ),
    250380: UniswapV3LiquidityAtTick(
        liquidityNet=22684152997344,
        liquidityGross=22684152997344,
    ),
    250440: UniswapV3LiquidityAtTick(
        liquidityNet=162814016216893,
        liquidityGross=162814016216893,
    ),
    250500: UniswapV3LiquidityAtTick(
        liquidityNet=2873122593562261,
        liquidityGross=2873122593562261,
    ),
    250560: UniswapV3LiquidityAtTick(
        liquidityNet=721265962680257,
        liquidityGross=721265962680257,
    ),
    250620: UniswapV3LiquidityAtTick(
        liquidityNet=3321833231733709,
        liquidityGross=3321833231733709,
    ),
    250680: UniswapV3LiquidityAtTick(
        liquidityNet=379136360814294,
        liquidityGross=379136360814294,
    ),
    250740: UniswapV3LiquidityAtTick(
        liquidityNet=30685778627595,
        liquidityGross=30685778627595,
    ),
    250800: UniswapV3LiquidityAtTick(
        liquidityNet=486676008472444,
        liquidityGross=486676008472444,
    ),
    250860: UniswapV3LiquidityAtTick(
        liquidityNet=495251715841078,
        liquidityGross=495251715841078,
    ),
    250920: UniswapV3LiquidityAtTick(
        liquidityNet=546196098082915,
        liquidityGross=546196098082915,
    ),
    250980: UniswapV3LiquidityAtTick(
        liquidityNet=1874212917320,
        liquidityGross=1874212917320,
    ),
    251040: UniswapV3LiquidityAtTick(
        liquidityNet=18739094027110,
        liquidityGross=18739094027110,
    ),
    251100: UniswapV3LiquidityAtTick(
        liquidityNet=116746369690747,
        liquidityGross=116746369690747,
    ),
    251160: UniswapV3LiquidityAtTick(
        liquidityNet=26215123110965,
        liquidityGross=26215123110965,
    ),
    251220: UniswapV3LiquidityAtTick(
        liquidityNet=1978401442982954,
        liquidityGross=1978401442982954,
    ),
    251280: UniswapV3LiquidityAtTick(
        liquidityNet=1744914434361,
        liquidityGross=1744914434361,
    ),
    251340: UniswapV3LiquidityAtTick(
        liquidityNet=180353178844777,
        liquidityGross=180353178844777,
    ),
    251400: UniswapV3LiquidityAtTick(
        liquidityNet=128509974135464,
        liquidityGross=128509974135464,
    ),
    251460: UniswapV3LiquidityAtTick(
        liquidityNet=540981291328174,
        liquidityGross=540981291328174,
    ),
    251520: UniswapV3LiquidityAtTick(
        liquidityNet=5731476751822266,
        liquidityGross=5731476751822266,
    ),
    251580: UniswapV3LiquidityAtTick(
        liquidityNet=26703188780726,
        liquidityGross=26703188780726,
    ),
    251640: UniswapV3LiquidityAtTick(
        liquidityNet=506062280322839,
        liquidityGross=506062280322839,
    ),
    251700: UniswapV3LiquidityAtTick(
        liquidityNet=244084381320927,
        liquidityGross=244084381320927,
    ),
    251760: UniswapV3LiquidityAtTick(
        liquidityNet=472259833373720,
        liquidityGross=472259833373720,
    ),
    251820: UniswapV3LiquidityAtTick(
        liquidityNet=72486190536708,
        liquidityGross=72486190536708,
    ),
    251880: UniswapV3LiquidityAtTick(
        liquidityNet=931553309070,
        liquidityGross=931553309070,
    ),
    251940: UniswapV3LiquidityAtTick(
        liquidityNet=293375936683379,
        liquidityGross=293375936683379,
    ),
    252000: UniswapV3LiquidityAtTick(
        liquidityNet=103709076861484,
        liquidityGross=103709076861484,
    ),
    252060: UniswapV3LiquidityAtTick(
        liquidityNet=10396565017847,
        liquidityGross=10396565017847,
    ),
    252120: UniswapV3LiquidityAtTick(
        liquidityNet=219076530609482,
        liquidityGross=219076530609482,
    ),
    252180: UniswapV3LiquidityAtTick(
        liquidityNet=156092492819634,
        liquidityGross=156092492819634,
    ),
    252240: UniswapV3LiquidityAtTick(
        liquidityNet=1115420578481664,
        liquidityGross=1115420578481664,
    ),
    252300: UniswapV3LiquidityAtTick(
        liquidityNet=7031828617233349,
        liquidityGross=7031828617233349,
    ),
    252360: UniswapV3LiquidityAtTick(
        liquidityNet=733766399181670,
        liquidityGross=733766399181670,
    ),
    252420: UniswapV3LiquidityAtTick(
        liquidityNet=84805612483975,
        liquidityGross=84805612483975,
    ),
    252480: UniswapV3LiquidityAtTick(
        liquidityNet=9463874529391447,
        liquidityGross=9463874529391447,
    ),
    252540: UniswapV3LiquidityAtTick(
        liquidityNet=7619953195446359,
        liquidityGross=7619953195446359,
    ),
    252600: UniswapV3LiquidityAtTick(
        liquidityNet=16406775025846,
        liquidityGross=16406775025846,
    ),
    252660: UniswapV3LiquidityAtTick(
        liquidityNet=2212907952699581,
        liquidityGross=2212907952699581,
    ),
    252720: UniswapV3LiquidityAtTick(
        liquidityNet=30877568635642,
        liquidityGross=30877568635642,
    ),
    252780: UniswapV3LiquidityAtTick(
        liquidityNet=1127243612814164,
        liquidityGross=1127243612814164,
    ),
    252840: UniswapV3LiquidityAtTick(
        liquidityNet=121022273918801,
        liquidityGross=121022273918801,
    ),
    252900: UniswapV3LiquidityAtTick(
        liquidityNet=3444505408543,
        liquidityGross=3444505408543,
    ),
    252960: UniswapV3LiquidityAtTick(
        liquidityNet=2921179233331,
        liquidityGross=2921179233331,
    ),
    253020: UniswapV3LiquidityAtTick(
        liquidityNet=16328660267242,
        liquidityGross=16328660267242,
    ),
    253080: UniswapV3LiquidityAtTick(
        liquidityNet=280182070401033,
        liquidityGross=280182070401033,
    ),
    253140: UniswapV3LiquidityAtTick(
        liquidityNet=60418933087985,
        liquidityGross=60418933087985,
    ),
    253200: UniswapV3LiquidityAtTick(
        liquidityNet=2221329753927173,
        liquidityGross=2221329753927173,
    ),
    253260: UniswapV3LiquidityAtTick(
        liquidityNet=785975068283793,
        liquidityGross=785975068283793,
    ),
    253320: UniswapV3LiquidityAtTick(
        liquidityNet=11068031251918845,
        liquidityGross=11068031251918845,
    ),
    253380: UniswapV3LiquidityAtTick(
        liquidityNet=1045385900711870,
        liquidityGross=1045385900711870,
    ),
    253440: UniswapV3LiquidityAtTick(
        liquidityNet=52071127547319,
        liquidityGross=52071127547319,
    ),
    253500: UniswapV3LiquidityAtTick(
        liquidityNet=905430992202364,
        liquidityGross=905430992202364,
    ),
    253560: UniswapV3LiquidityAtTick(
        liquidityNet=743934772353060,
        liquidityGross=743934772353060,
    ),
    253620: UniswapV3LiquidityAtTick(
        liquidityNet=1720814520558933,
        liquidityGross=1720814520558933,
    ),
    253680: UniswapV3LiquidityAtTick(
        liquidityNet=1694339960854588,
        liquidityGross=1694339960854588,
    ),
    253740: UniswapV3LiquidityAtTick(
        liquidityNet=1265620344704839,
        liquidityGross=1265620344704839,
    ),
    253800: UniswapV3LiquidityAtTick(
        liquidityNet=6822448654094831,
        liquidityGross=6822448654094831,
    ),
    253860: UniswapV3LiquidityAtTick(
        liquidityNet=1693329952501742,
        liquidityGross=1693329952501742,
    ),
    253920: UniswapV3LiquidityAtTick(
        liquidityNet=125002317687877,
        liquidityGross=125002317687877,
    ),
    253980: UniswapV3LiquidityAtTick(
        liquidityNet=3223198388506118,
        liquidityGross=3223198388506118,
    ),
    254040: UniswapV3LiquidityAtTick(
        liquidityNet=7233669530042180,
        liquidityGross=7233669530042180,
    ),
    254100: UniswapV3LiquidityAtTick(
        liquidityNet=1354995309152257,
        liquidityGross=1354995309152257,
    ),
    254160: UniswapV3LiquidityAtTick(
        liquidityNet=232717215486581,
        liquidityGross=232717215486581,
    ),
    254220: UniswapV3LiquidityAtTick(
        liquidityNet=1251624436633624,
        liquidityGross=1251624436633624,
    ),
    254280: UniswapV3LiquidityAtTick(
        liquidityNet=2397734105813300,
        liquidityGross=2397734105813300,
    ),
    254340: UniswapV3LiquidityAtTick(
        liquidityNet=5944750269566964,
        liquidityGross=5944750269566964,
    ),
    254400: UniswapV3LiquidityAtTick(
        liquidityNet=293897761293656,
        liquidityGross=293897761293656,
    ),
    254460: UniswapV3LiquidityAtTick(
        liquidityNet=27750958550784078,
        liquidityGross=27750958550784078,
    ),
    254520: UniswapV3LiquidityAtTick(
        liquidityNet=8803885001420580,
        liquidityGross=8803885001420580,
    ),
    254580: UniswapV3LiquidityAtTick(
        liquidityNet=4832503942071766,
        liquidityGross=4832503942071766,
    ),
    254640: UniswapV3LiquidityAtTick(
        liquidityNet=18878251447853522,
        liquidityGross=18878251447853522,
    ),
    254700: UniswapV3LiquidityAtTick(
        liquidityNet=47155277686430759,
        liquidityGross=47155277686430759,
    ),
    254760: UniswapV3LiquidityAtTick(
        liquidityNet=2205915167240804,
        liquidityGross=2205915167240804,
    ),
    254820: UniswapV3LiquidityAtTick(
        liquidityNet=714569902194940,
        liquidityGross=714569902194940,
    ),
    254880: UniswapV3LiquidityAtTick(
        liquidityNet=79311941684663448,
        liquidityGross=79311941684663448,
    ),
    254940: UniswapV3LiquidityAtTick(
        liquidityNet=131611058716155639,
        liquidityGross=131611058716155639,
    ),
    255000: UniswapV3LiquidityAtTick(
        liquidityNet=1598723641314836,
        liquidityGross=1598723641314836,
    ),
    255060: UniswapV3LiquidityAtTick(
        liquidityNet=3570483212215165,
        liquidityGross=3570483212215165,
    ),
    255120: UniswapV3LiquidityAtTick(
        liquidityNet=-685298200223732,
        liquidityGross=17722528321831664,
    ),
    255180: UniswapV3LiquidityAtTick(
        liquidityNet=10072938869856001,
        liquidityGross=10094548691501753,
    ),
    255240: UniswapV3LiquidityAtTick(
        liquidityNet=21324359501748223,
        liquidityGross=21324359501748223,
    ),
    255300: UniswapV3LiquidityAtTick(
        liquidityNet=4364228887048905,
        liquidityGross=4364237096627033,
    ),
    255360: UniswapV3LiquidityAtTick(
        liquidityNet=3206055008276559,
        liquidityGross=3206055008276559,
    ),
    255420: UniswapV3LiquidityAtTick(
        liquidityNet=8337914674210692,
        liquidityGross=8337914674210692,
    ),
    255480: UniswapV3LiquidityAtTick(
        liquidityNet=7341369571915483,
        liquidityGross=7343598454325075,
    ),
    255540: UniswapV3LiquidityAtTick(
        liquidityNet=66767515757170606,
        liquidityGross=66845387101892372,
    ),
    255600: UniswapV3LiquidityAtTick(
        liquidityNet=162909027590248760,
        liquidityGross=162909027590248760,
    ),
    255660: UniswapV3LiquidityAtTick(
        liquidityNet=216964302409007549,
        liquidityGross=216964302409007549,
    ),
    255720: UniswapV3LiquidityAtTick(
        liquidityNet=385101416899725455,
        liquidityGross=385101416899725455,
    ),
    255780: UniswapV3LiquidityAtTick(
        liquidityNet=40303492606471480,
        liquidityGross=40303492606471480,
    ),
    255840: UniswapV3LiquidityAtTick(
        liquidityNet=3851053596342728,
        liquidityGross=3851053596342728,
    ),
    255900: UniswapV3LiquidityAtTick(
        liquidityNet=61739626823736178,
        liquidityGross=61761883437367390,
    ),
    255960: UniswapV3LiquidityAtTick(
        liquidityNet=7555831027318727,
        liquidityGross=8019972741275283,
    ),
    256020: UniswapV3LiquidityAtTick(
        liquidityNet=11360440169028523,
        liquidityGross=11827319419190403,
    ),
    256080: UniswapV3LiquidityAtTick(
        liquidityNet=60072349123153735,
        liquidityGross=60213912612514587,
    ),
    256140: UniswapV3LiquidityAtTick(
        liquidityNet=-74357472016628479,
        liquidityGross=170532390299599075,
    ),
    256200: UniswapV3LiquidityAtTick(
        liquidityNet=16239518064529848,
        liquidityGross=18577658901767840,
    ),
    256260: UniswapV3LiquidityAtTick(
        liquidityNet=438562623027249,
        liquidityGross=524155195785499,
    ),
    256320: UniswapV3LiquidityAtTick(
        liquidityNet=416244761851792821,
        liquidityGross=418360901573548409,
    ),
    256380: UniswapV3LiquidityAtTick(
        liquidityNet=35258940131745361,
        liquidityGross=35354281359858859,
    ),
    256440: UniswapV3LiquidityAtTick(
        liquidityNet=87429969783283206,
        liquidityGross=87498002263567942,
    ),
    256500: UniswapV3LiquidityAtTick(
        liquidityNet=36662462787136434,
        liquidityGross=37079399801719790,
    ),
    256560: UniswapV3LiquidityAtTick(
        liquidityNet=24690609809160596,
        liquidityGross=26861955491935480,
    ),
    256620: UniswapV3LiquidityAtTick(
        liquidityNet=1530693759002640,
        liquidityGross=2266872875510184,
    ),
    256680: UniswapV3LiquidityAtTick(
        liquidityNet=450725489682812642,
        liquidityGross=617845694976651704,
    ),
    256740: UniswapV3LiquidityAtTick(
        liquidityNet=7757717182115128,
        liquidityGross=9508460709542130,
    ),
    256800: UniswapV3LiquidityAtTick(
        liquidityNet=13959272670778670,
        liquidityGross=16092031679955394,
    ),
    256860: UniswapV3LiquidityAtTick(
        liquidityNet=4834815815433126,
        liquidityGross=5917950403250604,
    ),
    256920: UniswapV3LiquidityAtTick(
        liquidityNet=3833542223902956,
        liquidityGross=4512613781521666,
    ),
    256980: UniswapV3LiquidityAtTick(
        liquidityNet=-513770369320412373,
        liquidityGross=518977161709113805,
    ),
    257040: UniswapV3LiquidityAtTick(
        liquidityNet=-399680035107951409,
        liquidityGross=403620542995899807,
    ),
    257100: UniswapV3LiquidityAtTick(
        liquidityNet=944424023373490,
        liquidityGross=3694067897442130,
    ),
    257160: UniswapV3LiquidityAtTick(
        liquidityNet=-17670767329996145,
        liquidityGross=29594233102383831,
    ),
    257220: UniswapV3LiquidityAtTick(
        liquidityNet=11903192163319542,
        liquidityGross=39655175448077564,
    ),
    257280: UniswapV3LiquidityAtTick(
        liquidityNet=-1571540551183525,
        liquidityGross=13904101892598927,
    ),
    257340: UniswapV3LiquidityAtTick(
        liquidityNet=11998694428887359,
        liquidityGross=12966404404911551,
    ),
    257400: UniswapV3LiquidityAtTick(
        liquidityNet=-41005393479443210,
        liquidityGross=77763278732931152,
    ),
    257460: UniswapV3LiquidityAtTick(
        liquidityNet=-40685333587385153,
        liquidityGross=65686824302883035,
    ),
    257520: UniswapV3LiquidityAtTick(
        liquidityNet=-61225490611514652,
        liquidityGross=70184420136930512,
    ),
    257580: UniswapV3LiquidityAtTick(
        liquidityNet=-18439176989338244,
        liquidityGross=27257534142204174,
    ),
    257640: UniswapV3LiquidityAtTick(
        liquidityNet=-354819799885904976,
        liquidityGross=354826232508972794,
    ),
    257700: UniswapV3LiquidityAtTick(
        liquidityNet=555307873298135460,
        liquidityGross=580538043725177658,
    ),
    257760: UniswapV3LiquidityAtTick(
        liquidityNet=-25710738959484157,
        liquidityGross=36060625186049061,
    ),
    257820: UniswapV3LiquidityAtTick(
        liquidityNet=-41778268035859589,
        liquidityGross=41831402034343447,
    ),
    257880: UniswapV3LiquidityAtTick(
        liquidityNet=-2514493092005759,
        liquidityGross=12524194724929907,
    ),
    257940: UniswapV3LiquidityAtTick(
        liquidityNet=-3653336697694042,
        liquidityGross=3948963147827352,
    ),
    258000: UniswapV3LiquidityAtTick(
        liquidityNet=-17407959736997210,
        liquidityGross=18022766627074666,
    ),
    258060: UniswapV3LiquidityAtTick(
        liquidityNet=-90544091051378961,
        liquidityGross=90569735114912293,
    ),
    258120: UniswapV3LiquidityAtTick(
        liquidityNet=-27435537767419192,
        liquidityGross=27512736265079542,
    ),
    258180: UniswapV3LiquidityAtTick(
        liquidityNet=-70075898568894367,
        liquidityGross=71239752558266035,
    ),
    258240: UniswapV3LiquidityAtTick(
        liquidityNet=-77730101655027806,
        liquidityGross=77788860536221776,
    ),
    258300: UniswapV3LiquidityAtTick(
        liquidityNet=-480160670015383231,
        liquidityGross=480234010987500729,
    ),
    258360: UniswapV3LiquidityAtTick(
        liquidityNet=-7951267675408107,
        liquidityGross=7959900060977557,
    ),
    258420: UniswapV3LiquidityAtTick(
        liquidityNet=-53269633953047198,
        liquidityGross=53282183913353050,
    ),
    258480: UniswapV3LiquidityAtTick(
        liquidityNet=-44688876530963149,
        liquidityGross=44956172397163395,
    ),
    258540: UniswapV3LiquidityAtTick(
        liquidityNet=-8452946696940052,
        liquidityGross=8452946696940052,
    ),
    258600: UniswapV3LiquidityAtTick(
        liquidityNet=-7622516282077012,
        liquidityGross=7622516282077012,
    ),
    258660: UniswapV3LiquidityAtTick(
        liquidityNet=-31477812345055638,
        liquidityGross=31477812345055638,
    ),
    258720: UniswapV3LiquidityAtTick(
        liquidityNet=-10379774337647228,
        liquidityGross=10390946286213796,
    ),
    258780: UniswapV3LiquidityAtTick(
        liquidityNet=-3847257447300179,
        liquidityGross=3950604565611973,
    ),
    258840: UniswapV3LiquidityAtTick(
        liquidityNet=-97900646011974339,
        liquidityGross=97900646011974339,
    ),
    258900: UniswapV3LiquidityAtTick(
        liquidityNet=-9891688696501483,
        liquidityGross=10064179935032431,
    ),
    258960: UniswapV3LiquidityAtTick(
        liquidityNet=-25494043572351332,
        liquidityGross=25494043572351332,
    ),
    259020: UniswapV3LiquidityAtTick(
        liquidityNet=-1984614229062349,
        liquidityGross=1984614229062349,
    ),
    259080: UniswapV3LiquidityAtTick(
        liquidityNet=-60946434614754049,
        liquidityGross=60946434614754049,
    ),
    259140: UniswapV3LiquidityAtTick(
        liquidityNet=-1771930106734303,
        liquidityGross=1771930106734303,
    ),
    259200: UniswapV3LiquidityAtTick(
        liquidityNet=-58824567876657891,
        liquidityGross=58824567876657891,
    ),
    259260: UniswapV3LiquidityAtTick(
        liquidityNet=-68270577775079268,
        liquidityGross=68270577775079268,
    ),
    259320: UniswapV3LiquidityAtTick(
        liquidityNet=-2364480825489862,
        liquidityGross=2364480825489862,
    ),
    259380: UniswapV3LiquidityAtTick(
        liquidityNet=-1189170434129164,
        liquidityGross=3335648726627380,
    ),
    259440: UniswapV3LiquidityAtTick(
        liquidityNet=-2460236402350929,
        liquidityGross=2460236402350929,
    ),
    259500: UniswapV3LiquidityAtTick(
        liquidityNet=-535417280350002,
        liquidityGross=535417280350002,
    ),
    259560: UniswapV3LiquidityAtTick(
        liquidityNet=-70350768590105,
        liquidityGross=70350768590105,
    ),
    259620: UniswapV3LiquidityAtTick(
        liquidityNet=-1021778168444274,
        liquidityGross=1021778168444274,
    ),
    259680: UniswapV3LiquidityAtTick(
        liquidityNet=-276049909077726,
        liquidityGross=276049909077726,
    ),
    259740: UniswapV3LiquidityAtTick(
        liquidityNet=-2702260478188734,
        liquidityGross=2702260478188734,
    ),
    259800: UniswapV3LiquidityAtTick(
        liquidityNet=-1163565651941686,
        liquidityGross=1163565651941686,
    ),
    259860: UniswapV3LiquidityAtTick(
        liquidityNet=-710040743947057,
        liquidityGross=710040743947057,
    ),
    259920: UniswapV3LiquidityAtTick(
        liquidityNet=-830712491263343,
        liquidityGross=830712491263343,
    ),
    259980: UniswapV3LiquidityAtTick(
        liquidityNet=-4689302966707254,
        liquidityGross=4692619231168190,
    ),
    260040: UniswapV3LiquidityAtTick(
        liquidityNet=-14807473500577126,
        liquidityGross=14807473500577126,
    ),
    260100: UniswapV3LiquidityAtTick(
        liquidityNet=-3390547462651817,
        liquidityGross=3390547462651817,
    ),
    260160: UniswapV3LiquidityAtTick(
        liquidityNet=-2213051740643507,
        liquidityGross=2213051740643507,
    ),
    260220: UniswapV3LiquidityAtTick(
        liquidityNet=-16333264145033263,
        liquidityGross=16333264145033263,
    ),
    260280: UniswapV3LiquidityAtTick(
        liquidityNet=-202256047650577199,
        liquidityGross=202256047650577199,
    ),
    260340: UniswapV3LiquidityAtTick(
        liquidityNet=-2305739590349437,
        liquidityGross=2305739590349437,
    ),
    260400: UniswapV3LiquidityAtTick(
        liquidityNet=-482148621584562,
        liquidityGross=482148621584562,
    ),
    260460: UniswapV3LiquidityAtTick(
        liquidityNet=-5151763950535971,
        liquidityGross=5151763950535971,
    ),
    260520: UniswapV3LiquidityAtTick(
        liquidityNet=-4949460699961,
        liquidityGross=4949460699961,
    ),
    260580: UniswapV3LiquidityAtTick(
        liquidityNet=-792216572972459,
        liquidityGross=792216572972459,
    ),
    260640: UniswapV3LiquidityAtTick(
        liquidityNet=-1659069837896683,
        liquidityGross=1659069837896683,
    ),
    260700: UniswapV3LiquidityAtTick(
        liquidityNet=-471050672837794,
        liquidityGross=471050672837794,
    ),
    260760: UniswapV3LiquidityAtTick(
        liquidityNet=-1427210936238126,
        liquidityGross=1427210936238126,
    ),
    260820: UniswapV3LiquidityAtTick(
        liquidityNet=-160567446454122,
        liquidityGross=160567446454122,
    ),
    260880: UniswapV3LiquidityAtTick(
        liquidityNet=-4660514545814,
        liquidityGross=4660514545814,
    ),
    260940: UniswapV3LiquidityAtTick(
        liquidityNet=-949190291053485,
        liquidityGross=949190291053485,
    ),
    261000: UniswapV3LiquidityAtTick(
        liquidityNet=-701508382094315,
        liquidityGross=701508382094315,
    ),
    261060: UniswapV3LiquidityAtTick(
        liquidityNet=-192494897286115,
        liquidityGross=192494897286115,
    ),
    261120: UniswapV3LiquidityAtTick(
        liquidityNet=-97448428510165,
        liquidityGross=97448428510165,
    ),
    261180: UniswapV3LiquidityAtTick(
        liquidityNet=-768965647069,
        liquidityGross=768965647069,
    ),
    261240: UniswapV3LiquidityAtTick(
        liquidityNet=-4290736218537318,
        liquidityGross=4290736218537318,
    ),
    261300: UniswapV3LiquidityAtTick(
        liquidityNet=-5099128904602610,
        liquidityGross=5099128904602610,
    ),
    261360: UniswapV3LiquidityAtTick(
        liquidityNet=-1609462935217957,
        liquidityGross=1609462935217957,
    ),
    261420: UniswapV3LiquidityAtTick(
        liquidityNet=-1090436462449794,
        liquidityGross=1090436462449794,
    ),
    261480: UniswapV3LiquidityAtTick(
        liquidityNet=-427691948100201,
        liquidityGross=427691948100201,
    ),
    261540: UniswapV3LiquidityAtTick(
        liquidityNet=-25222997794671,
        liquidityGross=25222997794671,
    ),
    261600: UniswapV3LiquidityAtTick(
        liquidityNet=-14172746964296,
        liquidityGross=14172746964296,
    ),
    261660: UniswapV3LiquidityAtTick(
        liquidityNet=-33286914116176,
        liquidityGross=33286914116176,
    ),
    261720: UniswapV3LiquidityAtTick(
        liquidityNet=-390958960205477,
        liquidityGross=390958960205477,
    ),
    261780: UniswapV3LiquidityAtTick(
        liquidityNet=-2170475390497,
        liquidityGross=2170475390497,
    ),
    261840: UniswapV3LiquidityAtTick(
        liquidityNet=-40685506569662,
        liquidityGross=40685506569662,
    ),
    261900: UniswapV3LiquidityAtTick(
        liquidityNet=-768317113367493,
        liquidityGross=768317113367493,
    ),
    261960: UniswapV3LiquidityAtTick(
        liquidityNet=-349615904032981,
        liquidityGross=349615904032981,
    ),
    262020: UniswapV3LiquidityAtTick(
        liquidityNet=-91960406332603,
        liquidityGross=91960406332603,
    ),
    262080: UniswapV3LiquidityAtTick(
        liquidityNet=-443641166532945,
        liquidityGross=443641166532945,
    ),
    262140: UniswapV3LiquidityAtTick(
        liquidityNet=-38227973445101,
        liquidityGross=38227973445101,
    ),
    262200: UniswapV3LiquidityAtTick(
        liquidityNet=-903825331042290,
        liquidityGross=903825331042290,
    ),
    262260: UniswapV3LiquidityAtTick(
        liquidityNet=-13128065406663,
        liquidityGross=13128065406663,
    ),
    262320: UniswapV3LiquidityAtTick(
        liquidityNet=-121957304230580,
        liquidityGross=121957304230580,
    ),
    262380: UniswapV3LiquidityAtTick(
        liquidityNet=-424848915752761,
        liquidityGross=424848915752761,
    ),
    262440: UniswapV3LiquidityAtTick(
        liquidityNet=-14652087781394115,
        liquidityGross=14652087781394115,
    ),
    262500: UniswapV3LiquidityAtTick(
        liquidityNet=-1770941642899971,
        liquidityGross=1770941642899971,
    ),
    262560: UniswapV3LiquidityAtTick(
        liquidityNet=-1647348926749984,
        liquidityGross=1647348926749984,
    ),
    262620: UniswapV3LiquidityAtTick(
        liquidityNet=-142827709793555,
        liquidityGross=142827709793555,
    ),
    262680: UniswapV3LiquidityAtTick(
        liquidityNet=-236240897581658,
        liquidityGross=236240897581658,
    ),
    262740: UniswapV3LiquidityAtTick(
        liquidityNet=-3198063287248,
        liquidityGross=3198063287248,
    ),
    262800: UniswapV3LiquidityAtTick(
        liquidityNet=-28405605552220,
        liquidityGross=28405605552220,
    ),
    262860: UniswapV3LiquidityAtTick(
        liquidityNet=-127001888471234,
        liquidityGross=127001888471234,
    ),
    262920: UniswapV3LiquidityAtTick(
        liquidityNet=-1875443486130867,
        liquidityGross=1875443486130867,
    ),
    262980: UniswapV3LiquidityAtTick(
        liquidityNet=-338041427279495,
        liquidityGross=338041427279495,
    ),
    263040: UniswapV3LiquidityAtTick(
        liquidityNet=-678050751391,
        liquidityGross=678050751391,
    ),
    263100: UniswapV3LiquidityAtTick(
        liquidityNet=-2233936785039147,
        liquidityGross=2233936785039147,
    ),
    263160: UniswapV3LiquidityAtTick(
        liquidityNet=-784249058176776,
        liquidityGross=784249058176776,
    ),
    263220: UniswapV3LiquidityAtTick(
        liquidityNet=-184063967535124,
        liquidityGross=184063967535124,
    ),
    263280: UniswapV3LiquidityAtTick(
        liquidityNet=-88096801401735,
        liquidityGross=88096801401735,
    ),
    263340: UniswapV3LiquidityAtTick(
        liquidityNet=-198487481613624,
        liquidityGross=198487481613624,
    ),
    263400: UniswapV3LiquidityAtTick(
        liquidityNet=-452889034759715,
        liquidityGross=452889034759715,
    ),
    263460: UniswapV3LiquidityAtTick(
        liquidityNet=-606799883869495,
        liquidityGross=606799883869495,
    ),
    263520: UniswapV3LiquidityAtTick(
        liquidityNet=-7638454561936103,
        liquidityGross=7638454561936103,
    ),
    263580: UniswapV3LiquidityAtTick(
        liquidityNet=-126821769296502,
        liquidityGross=126821769296502,
    ),
    263640: UniswapV3LiquidityAtTick(
        liquidityNet=-6110861158994498,
        liquidityGross=6110861158994498,
    ),
    263700: UniswapV3LiquidityAtTick(
        liquidityNet=-1245903277890155,
        liquidityGross=1245903277890155,
    ),
    263760: UniswapV3LiquidityAtTick(
        liquidityNet=-2890954460186027,
        liquidityGross=2890954460186027,
    ),
    263820: UniswapV3LiquidityAtTick(
        liquidityNet=-2254895454670155,
        liquidityGross=2254895454670155,
    ),
    263880: UniswapV3LiquidityAtTick(
        liquidityNet=-331818644781597,
        liquidityGross=331818644781597,
    ),
    263940: UniswapV3LiquidityAtTick(
        liquidityNet=-1012237412969331,
        liquidityGross=1012237412969331,
    ),
    264000: UniswapV3LiquidityAtTick(
        liquidityNet=-355849039791761,
        liquidityGross=355849039791761,
    ),
    264060: UniswapV3LiquidityAtTick(
        liquidityNet=-830913905222007,
        liquidityGross=830913905222007,
    ),
    264120: UniswapV3LiquidityAtTick(
        liquidityNet=-232657278104839,
        liquidityGross=232657278104839,
    ),
    264180: UniswapV3LiquidityAtTick(
        liquidityNet=-223175670932954,
        liquidityGross=223175670932954,
    ),
    264240: UniswapV3LiquidityAtTick(
        liquidityNet=-45075145059172,
        liquidityGross=45075145059172,
    ),
    264300: UniswapV3LiquidityAtTick(
        liquidityNet=-514961937591143,
        liquidityGross=514961937591143,
    ),
    264360: UniswapV3LiquidityAtTick(
        liquidityNet=-1836640574139053,
        liquidityGross=1836640574139053,
    ),
    264420: UniswapV3LiquidityAtTick(
        liquidityNet=-399126992437212,
        liquidityGross=399126992437212,
    ),
    264480: UniswapV3LiquidityAtTick(
        liquidityNet=-3386196039666481,
        liquidityGross=3386196039666481,
    ),
    264540: UniswapV3LiquidityAtTick(
        liquidityNet=-169026268794532,
        liquidityGross=169026268794532,
    ),
    264600: UniswapV3LiquidityAtTick(
        liquidityNet=-38164370768122,
        liquidityGross=38164370768122,
    ),
    264660: UniswapV3LiquidityAtTick(
        liquidityNet=-6838638581919,
        liquidityGross=6838638581919,
    ),
    264720: UniswapV3LiquidityAtTick(
        liquidityNet=-6934819008835,
        liquidityGross=6934819008835,
    ),
    264780: UniswapV3LiquidityAtTick(
        liquidityNet=-634419296225677,
        liquidityGross=634419296225677,
    ),
    264840: UniswapV3LiquidityAtTick(
        liquidityNet=-1894461556126,
        liquidityGross=1894461556126,
    ),
    264900: UniswapV3LiquidityAtTick(
        liquidityNet=-1087328233426084,
        liquidityGross=1087328233426084,
    ),
    264960: UniswapV3LiquidityAtTick(
        liquidityNet=-9592653857021,
        liquidityGross=9592653857021,
    ),
    265020: UniswapV3LiquidityAtTick(
        liquidityNet=-964623108704,
        liquidityGross=964623108704,
    ),
    265080: UniswapV3LiquidityAtTick(
        liquidityNet=-63222276742849,
        liquidityGross=63222276742849,
    ),
    265140: UniswapV3LiquidityAtTick(
        liquidityNet=-417896448821485,
        liquidityGross=417896448821485,
    ),
    265200: UniswapV3LiquidityAtTick(
        liquidityNet=-115757424347917,
        liquidityGross=115757424347917,
    ),
    265260: UniswapV3LiquidityAtTick(
        liquidityNet=-247937698964119,
        liquidityGross=247937698964119,
    ),
    265320: UniswapV3LiquidityAtTick(
        liquidityNet=-549333109125871,
        liquidityGross=549333109125871,
    ),
    265380: UniswapV3LiquidityAtTick(
        liquidityNet=-682184643186092,
        liquidityGross=682184643186092,
    ),
    265440: UniswapV3LiquidityAtTick(
        liquidityNet=-80696202126881,
        liquidityGross=80696202126881,
    ),
    265500: UniswapV3LiquidityAtTick(
        liquidityNet=-646838003704937,
        liquidityGross=646838003704937,
    ),
    265560: UniswapV3LiquidityAtTick(
        liquidityNet=-422423521362347,
        liquidityGross=422423521362347,
    ),
    265620: UniswapV3LiquidityAtTick(
        liquidityNet=-11390495459371,
        liquidityGross=11390495459371,
    ),
    265680: UniswapV3LiquidityAtTick(
        liquidityNet=-14264473913293,
        liquidityGross=14264473913293,
    ),
    265740: UniswapV3LiquidityAtTick(
        liquidityNet=-809676720010,
        liquidityGross=809676720010,
    ),
    265800: UniswapV3LiquidityAtTick(
        liquidityNet=-228442660393752,
        liquidityGross=228442660393752,
    ),
    265980: UniswapV3LiquidityAtTick(
        liquidityNet=-1109158269877,
        liquidityGross=1109158269877,
    ),
    266220: UniswapV3LiquidityAtTick(
        liquidityNet=-40234494434,
        liquidityGross=40234494434,
    ),
    266520: UniswapV3LiquidityAtTick(
        liquidityNet=-95239241762338,
        liquidityGross=95239241762338,
    ),
    266580: UniswapV3LiquidityAtTick(
        liquidityNet=-7345787342181,
        liquidityGross=7345787342181,
    ),
    266640: UniswapV3LiquidityAtTick(
        liquidityNet=-893004067318,
        liquidityGross=893004067318,
    ),
    266700: UniswapV3LiquidityAtTick(
        liquidityNet=-238322377607001,
        liquidityGross=238322377607001,
    ),
    266940: UniswapV3LiquidityAtTick(
        liquidityNet=-3816659140777,
        liquidityGross=3816659140777,
    ),
    267000: UniswapV3LiquidityAtTick(
        liquidityNet=-14630998912172,
        liquidityGross=14630998912172,
    ),
    267060: UniswapV3LiquidityAtTick(
        liquidityNet=-2002323771185,
        liquidityGross=2002323771185,
    ),
    267120: UniswapV3LiquidityAtTick(
        liquidityNet=-4026522741505,
        liquidityGross=4026522741505,
    ),
    267180: UniswapV3LiquidityAtTick(
        liquidityNet=-878049993248372,
        liquidityGross=878049993248372,
    ),
    267720: UniswapV3LiquidityAtTick(
        liquidityNet=-1432820167102,
        liquidityGross=1432820167102,
    ),
    268260: UniswapV3LiquidityAtTick(
        liquidityNet=-41247994973279,
        liquidityGross=41247994973279,
    ),
    268320: UniswapV3LiquidityAtTick(
        liquidityNet=-3921753993726,
        liquidityGross=3921753993726,
    ),
    268620: UniswapV3LiquidityAtTick(
        liquidityNet=-7842385042332,
        liquidityGross=7842385042332,
    ),
    268920: UniswapV3LiquidityAtTick(
        liquidityNet=-48860694293,
        liquidityGross=48860694293,
    ),
    268980: UniswapV3LiquidityAtTick(
        liquidityNet=-233944583115,
        liquidityGross=233944583115,
    ),
    269160: UniswapV3LiquidityAtTick(
        liquidityNet=-2921179233331,
        liquidityGross=2921179233331,
    ),
    269220: UniswapV3LiquidityAtTick(
        liquidityNet=-116527715609,
        liquidityGross=116527715609,
    ),
    269280: UniswapV3LiquidityAtTick(
        liquidityNet=-4816040963852,
        liquidityGross=4816040963852,
    ),
    269400: UniswapV3LiquidityAtTick(
        liquidityNet=-515347242942616,
        liquidityGross=515347242942616,
    ),
    270420: UniswapV3LiquidityAtTick(
        liquidityNet=-120389476854562,
        liquidityGross=120389476854562,
    ),
    270540: UniswapV3LiquidityAtTick(
        liquidityNet=-34804953692,
        liquidityGross=34804953692,
    ),
    270840: UniswapV3LiquidityAtTick(
        liquidityNet=-1649567047148,
        liquidityGross=1649567047148,
    ),
    271080: UniswapV3LiquidityAtTick(
        liquidityNet=-103000565770,
        liquidityGross=103000565770,
    ),
    271500: UniswapV3LiquidityAtTick(
        liquidityNet=-7387510370926,
        liquidityGross=7387510370926,
    ),
    271680: UniswapV3LiquidityAtTick(
        liquidityNet=-6060018180622,
        liquidityGross=6060018180622,
    ),
    272280: UniswapV3LiquidityAtTick(
        liquidityNet=-4538579708481,
        liquidityGross=4538579708481,
    ),
    272520: UniswapV3LiquidityAtTick(
        liquidityNet=-202950841112641,
        liquidityGross=202950841112641,
    ),
    272580: UniswapV3LiquidityAtTick(
        liquidityNet=-1537745525855,
        liquidityGross=1537745525855,
    ),
    276300: UniswapV3LiquidityAtTick(
        liquidityNet=-2543662313063992,
        liquidityGross=2543662313063992,
    ),
    278940: UniswapV3LiquidityAtTick(
        liquidityNet=-5164088638751,
        liquidityGross=5164088638751,
    ),
    280080: UniswapV3LiquidityAtTick(
        liquidityNet=-24418298514298,
        liquidityGross=24418298514298,
    ),
    280500: UniswapV3LiquidityAtTick(
        liquidityNet=-6081508773089,
        liquidityGross=6081508773089,
    ),
    281220: UniswapV3LiquidityAtTick(
        liquidityNet=-33872560240,
        liquidityGross=33872560240,
    ),
    282180: UniswapV3LiquidityAtTick(
        liquidityNet=-2366835767353,
        liquidityGross=2366835767353,
    ),
    283260: UniswapV3LiquidityAtTick(
        liquidityNet=-12241814398321,
        liquidityGross=12241814398321,
    ),
    283800: UniswapV3LiquidityAtTick(
        liquidityNet=-1410203189999,
        liquidityGross=1410203189999,
    ),
    329460: UniswapV3LiquidityAtTick(
        liquidityNet=-643796387349,
        liquidityGross=643796387349,
    ),
    368460: UniswapV3LiquidityAtTick(
        liquidityNet=-486522234414488,
        liquidityGross=486522234414488,
    ),
    391440: UniswapV3LiquidityAtTick(
        liquidityNet=-456406095307,
        liquidityGross=456406095307,
    ),
    437520: UniswapV3LiquidityAtTick(
        liquidityNet=-10943161472679,
        liquidityGross=10943161472679,
    ),
    887220: UniswapV3LiquidityAtTick(
        liquidityNet=-97132329311971,
        liquidityGross=97132329311971,
    ),
    92040: UniswapV3LiquidityAtTick(
        liquidityNet=1620915153,
        liquidityGross=1620915153,
    ),
    92100: UniswapV3LiquidityAtTick(
        liquidityNet=3974353998848,
        liquidityGross=3974353998848,
    ),
}
v3_lp._update_pool_state()

arb = UniswapLpCycle(
    id="test_arb",
    input_token=weth,
    swap_pools=[v2_lp, v3_lp],
    max_input=100 * 10**18,
)


def test_type_checks():
    # Need to ensure that the mocked helpers will pass the type checks
    # inside various methods
    assert isinstance(v2_lp, LiquidityPool)
    assert isinstance(v3_lp, V3LiquidityPool)
    assert isinstance(weth, Erc20Token)
    assert isinstance(wbtc, Erc20Token)


def test_arbitrage():
    assert arb.calculate_arbitrage() == (False, (9216006286314, -177578711481))


def test_arbitrage_with_overrides():
    v2_pool_state_override = UniswapV2PoolState(
        pool=v2_lp,
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
    )

    v3_pool_state_override = UniswapV3PoolState(
        pool=v3_lp,
        liquidity=1533143241938066251,
        sqrt_price_x96=31881290961944305252140777263703426,
        tick=258116,
    )

    overrides: Sequence[
        Union[
            Tuple[LiquidityPool, UniswapV2PoolState],
            Tuple[V3LiquidityPool, UniswapV3PoolState],
        ]
    ]

    # Override both pools
    overrides = [
        (v2_lp, v2_pool_state_override),
        (v3_lp, v3_pool_state_override),
    ]

    assert arb.calculate_arbitrage(override_state=overrides) == (
        False,
        (20522764010327, -282198455271),
    )

    # Override V2 pool only
    overrides = [
        (v2_lp, v2_pool_state_override),
    ]

    assert arb.calculate_arbitrage(override_state=overrides) == (
        False,
        (15636391570906, -572345612992),
    )

    # Override V3 pool only
    overrides = [
        (v3_lp, v3_pool_state_override),
    ]

    assert arb.calculate_arbitrage(override_state=overrides) == (
        True,
        (20454968409226055680, 163028226755627520),
    )

    # Irrelevant V2 and V3 mocked pools, only the address is changed.
    irrelevant_v2_pool = MockLiquidityPool()
    irrelevant_v2_pool.address = web3.Web3.toChecksumAddress(
        "0x0000000000000000000000000000000000000069"
    )
    irrelevant_v2_pool.name = "WBTC-WETH (V2, 0.30%)"
    irrelevant_v2_pool.factory = web3.Web3.toChecksumAddress(
        "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
    )
    irrelevant_v2_pool.fee = None
    irrelevant_v2_pool.fee_token0 = Fraction(3, 1000)
    irrelevant_v2_pool.fee_token1 = Fraction(3, 1000)
    irrelevant_v2_pool.reserves_token0 = 16231137593
    irrelevant_v2_pool.reserves_token1 = 2571336301536722443178
    irrelevant_v2_pool.token0 = wbtc
    irrelevant_v2_pool.token1 = weth

    irrelevant_v3_pool = MockV3LiquidityPool()
    irrelevant_v3_pool.address = web3.Web3.toChecksumAddress(
        "0x0000000000000000000000000000000000000420"
    )
    irrelevant_v3_pool.name = "WBTC-WETH (V3, 0.30%)"
    irrelevant_v3_pool.factory = web3.Web3.toChecksumAddress(
        "0x1F98431c8aD98523631AE4a59f267346ea31F984"
    )
    irrelevant_v3_pool.fee = 3000
    irrelevant_v3_pool.token0 = wbtc
    irrelevant_v3_pool.token1 = weth
    irrelevant_v3_pool.liquidity = 1612978974357835825
    irrelevant_v3_pool.liquidity_update_block = 1
    irrelevant_v3_pool.sqrt_price_x96 = 31549217861118002279483878013792428
    irrelevant_v3_pool.sparse_bitmap = False
    irrelevant_v3_pool.tick = 257907
    irrelevant_v3_pool.tick_spacing = 60

    # Include overrides for the irrelevant pools.
    # The freshly-built mock pools will be ignored by the arb calculation
    # method since their addresses will not match
    overrides = [
        (irrelevant_v2_pool, v2_pool_state_override),
        (irrelevant_v3_pool, v3_pool_state_override),
    ]

    # This should equal the result from the original test (no overriddes)
    assert arb.calculate_arbitrage(override_state=overrides) == (
        False,
        (9216006286314, -177578711481),
    )

    overrides = [
        (v2_lp, v2_pool_state_override),
        (irrelevant_v3_pool, v3_pool_state_override),  # Should be ignored
    ]

    # This should equal the result from the test with the V2 override only
    assert arb.calculate_arbitrage(override_state=overrides) == (
        False,
        (15636391570906, -572345612992),
    )

    overrides = [
        (irrelevant_v2_pool, v2_pool_state_override),  # Should be ignored
        (v3_lp, v3_pool_state_override),
    ]

    # This should equal the result from the test with the V3 override only
    assert arb.calculate_arbitrage(override_state=overrides) == (
        True,
        (20454968409226055680, 163028226755627520),
    )
