# Overview
Degenbot is a set of Python classes that abstract many of the implementation details of Uniswap liquidity pools and their underlying ERC-20 tokens. It is an abstraction that uses web3.py for communication with an EVM blockchain.

These classes serve as a building blocks for the lessons published by [BowTiedDevil](https://twitter.com/BowTiedDevil) on [Degen Code](https://www.degencode.com/).

The classes originally relied on [Brownie](https://github.com/eth-brownie/), but have evolved to use [web3.py](https://github.com/ethereum/web3.py/) more generally following Brownie's transition to "maintenance mode". The degenbot classes still operate when loaded within a Brownie console or when connected to a Python script with a connected `chain` object.

## Prerequisites
Python version 3.10 or newer.

## Installation
There are two ways to install degenbot, both require `pip`.

### From PyPI
`pip install degenbot` will fetch the latest version from [PyPI](https://pypi.org/project/degenbot/) with dependencies.

### From Source
Use `git clone` to create a local copy of this repo, then install with `pip install -e /path/to/repo`. This creates an editable installation that can be imported into a script or Python REPL using `import degenbot`.

## Examples
The following snippets assume a connected `Web3` instance with a working provider on Ethereum mainnet (chain ID #1), and the classes imported under the `degenbot` namespace.

### Uniswap V2 Liquidity Pools

```
# Create `LiquidityPool` object from on-chain data at the given address and current chain height
>>> lp = degenbot.LiquidityPool('0xBb2b8038a1640196FbE3e38816F3e67Cba72D940')
• WBTC (Wrapped BTC)
• WETH (Wrapped Ether)
WBTC-WETH (V2, 0.30%)
• Token 0: WBTC - Reserves: 10732489743
• Token 1: WETH - Reserves: 2056834999904002274711

# Inspect the tokens held by the pool
>>> lp.token0
Erc20Token(address=0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599, symbol='WBTC', name='Wrapped BTC', decimals=8)

>>> lp.token1
Erc20Token(address=0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2, symbol='WETH', name='Wrapped Ether', decimals=18)

>>> lp.fee_token0
Fraction(3, 1000)

>>> lp.fee_token1
Fraction(3, 1000)

# Predict the input and output values for swaps through the pool, accounting for fees
>>> lp.calculate_tokens_out_from_tokens_in(
    token_in=lp.token1, 
    token_in_quantity=1*10**18
)
5199789

>>> lp.calculate_tokens_in_from_tokens_out(
    token_out=lp.token0, 
    token_out_quantity=5199789
)
999999992817074189

# Update the current reserves from the live blockchain (`True` if updated reserves found, `False` otherwise)
>>> lp.update_reserves(silent=False)
[WBTC-WETH (V2, 0.30%)]
WBTC: 10732455184
WETH: 2056841643098872755548
WBTC/WETH: 0.05217929741946638
WETH/WBTC: 19.164688860431703
True
       
>>> lp.reserves_token0
10732455184

>>> lp.reserves_token1
2056841643098872755548
```

### Uniswap V3 Liquidity Pools
```
>>> lp = degenbot.V3LiquidityPool('0xCBCdF9626bC03E24f779434178A73a0B4bad62eD')
WBTC-WETH (V3, 0.30%)
• Token 0: WBTC
• Token 1: WETH
• Fee: 3000
• Liquidity: 544425151051415575
• SqrtPrice: 34048891009198980752047510166697902
• Tick: 259432

# Calculate inputs and outputs
>>> lp.calculate_tokens_out_from_tokens_in(
    token_in=lp.token1, 
    token_in_quantity=1*10**18
)
5398169

>>> lp.calculate_tokens_in_from_tokens_out(
    token_out=lp.token0, 
    token_out_quantity=5398169
)
999999871563434214

# Inspect the known liquidity positions
>>> lp.tick_bitmap
{
    0: UniswapV3BitmapAtWord(
        bitmap=1,
        block=18517665
        ),
    16: UniswapV3BitmapAtWord(
        bitmap=115792089237316195423570985008687907853268655437644779123584680198630541352072,block=18517670
        )
}

# NOTE: the V3 liquidity pool helper is optimized for fast instantiation, and will 
lazy-load liquidity data for positions outside of the current range as needed.

>>> lp.tick_data
{
    0: UniswapV3LiquidityAtTick(
        liquidityNet=10943161472679, liquidityGross=10943161472679, block=18517665
    ),
    261060: UniswapV3LiquidityAtTick(
        liquidityNet=-910396189679465, liquidityGross=910396189679465, block=18517670
    ),
    261000: UniswapV3LiquidityAtTick(
        liquidityNet=-3774266260841234, liquidityGross=3774266260841234, block=18517670
    ),
    260940: UniswapV3LiquidityAtTick(
        liquidityNet=-46192636206556, liquidityGross=46192636206556, block=18517670
    ),
    260880: UniswapV3LiquidityAtTick(
        liquidityNet=-116021224930444, liquidityGross=116021224930444, block=18517670
    ),
    260820: UniswapV3LiquidityAtTick(
        liquidityNet=-22305257591348143, liquidityGross=22305257591348143, block=18517670
    ),
    260760: UniswapV3LiquidityAtTick(
        liquidityNet=-21011503402422046, liquidityGross=21011503402422046, block=18517670
    ),
    260700: UniswapV3LiquidityAtTick(
        liquidityNet=-712562901475206, liquidityGross=712562901475206, block=18517670
    ),
    260640: UniswapV3LiquidityAtTick(
        liquidityNet=-2824973601322102, liquidityGross=2824973601322102, block=18517670
    ),
    260580: UniswapV3LiquidityAtTick(
        liquidityNet=-1720757279804405, liquidityGross=1720757279804405, block=18517670
    ),
    260520: UniswapV3LiquidityAtTick(
        liquidityNet=-672260671043476, liquidityGross=672260671043476, block=18517670
    ),
    260460: UniswapV3LiquidityAtTick(
        liquidityNet=-15504359343636169, liquidityGross=15504359343636169, block=18517670
    ),
    260400: UniswapV3LiquidityAtTick(
        liquidityNet=-4414624347774090, liquidityGross=4414624347774090, block=18517670
    ),
    260340: UniswapV3LiquidityAtTick(
        liquidityNet=-6535507987682952, liquidityGross=6535507987682952, block=18517670
    ),
    260280: UniswapV3LiquidityAtTick(
        liquidityNet=-203426775793467733, liquidityGross=203426775793467733, block=18517670
    ),
    260220: UniswapV3LiquidityAtTick(
        liquidityNet=-15295798443780658, liquidityGross=15295798443780658, block=18517670
    ),
    260160: UniswapV3LiquidityAtTick(
        liquidityNet=-2160712979864375, liquidityGross=2160712979864375, block=18517670
    ),
    260100: UniswapV3LiquidityAtTick(
        liquidityNet=-9064067384996110, liquidityGross=9064067384996110, block=18517670
    ),
    260040: UniswapV3LiquidityAtTick(
        liquidityNet=-15054878819287389, liquidityGross=15054878819287389, block=18517670
    ),
    259980: UniswapV3LiquidityAtTick(
        liquidityNet=-4860601626896509, liquidityGross=4863917891357445, block=18517670
    ),
    259920: UniswapV3LiquidityAtTick(
        liquidityNet=-3824390175347386, liquidityGross=3824390175347386, block=18517670
    ),
    259860: UniswapV3LiquidityAtTick(
        liquidityNet=-65407651539408578, liquidityGross=65407651539408578, block=18517670
    ),
    259800: UniswapV3LiquidityAtTick(
        liquidityNet=-3855187764510684, liquidityGross=4967766430709060, block=18517670
    ),
    259740: UniswapV3LiquidityAtTick(
        liquidityNet=-5509690868388578, liquidityGross=7723957237041816, block=18517670
    ),
    259680: UniswapV3LiquidityAtTick(
        liquidityNet=-1136219849672495, liquidityGross=1136219849672495, block=18517670
    ),
    259620: UniswapV3LiquidityAtTick(
        liquidityNet=-2393279497150341, liquidityGross=2393279497150341, block=18517670
    ),
    259560: UniswapV3LiquidityAtTick(
        liquidityNet=398865936118031, liquidityGross=508994624225907, block=18517670
    ),
    259500: UniswapV3LiquidityAtTick(
        liquidityNet=15522465756110847, liquidityGross=16125721890069227, block=18517670
    ),
    259440: UniswapV3LiquidityAtTick(
        liquidityNet=-1891527898798928, liquidityGross=1891527898798928, block=18517670
    ),
    259380: UniswapV3LiquidityAtTick(
        liquidityNet=8044068772191, liquidityGross=2138434223726025, block=18517670
    ),
    259320: UniswapV3LiquidityAtTick(
        liquidityNet=-2832095947421521, liquidityGross=3878976749252607, block=18517670
    ),
    259260: UniswapV3LiquidityAtTick(
        liquidityNet=-9861460805976385, liquidityGross=10286980247580505, block=18517670
    ),
    259200: UniswapV3LiquidityAtTick(
        liquidityNet=-37586853874962884, liquidityGross=75585150815154956, block=18517670
    ),
    259140: UniswapV3LiquidityAtTick(
        liquidityNet=7463492399952407, liquidityGross=8891903842079407, block=18517670
    ),
    259080: UniswapV3LiquidityAtTick(
        liquidityNet=-56614546716155240, liquidityGross=65350152772739822, block=18517670
    ),
    259020: UniswapV3LiquidityAtTick(
        liquidityNet=-1327341509605616, liquidityGross=2105008157295342, block=18517670
    ),
    258960: UniswapV3LiquidityAtTick(
        liquidityNet=-14244474437746000, liquidityGross=17444952752908754, block=18517670
    ),
    258900: UniswapV3LiquidityAtTick(
        liquidityNet=-46343627695797256, liquidityGross=71051996162057522, block=18517670
    ),
    258840: UniswapV3LiquidityAtTick(
        liquidityNet=-101739259548283798, liquidityGross=101745019411002912, block=18517670
    ),
    258780: UniswapV3LiquidityAtTick(
        liquidityNet=13621217177168585, liquidityGross=41671812563446815, block=18517670
    ),
    258720: UniswapV3LiquidityAtTick(
        liquidityNet=-5152780236002260, liquidityGross=8880794898061486, block=18517670
    ),
    258660: UniswapV3LiquidityAtTick(
        liquidityNet=-3235861820705505, liquidityGross=5499474441534555, block=18517670
    ),
    258600: UniswapV3LiquidityAtTick(
        liquidityNet=-5630124373999292, liquidityGross=12223522949857822, block=18517670
    ),
    258540: UniswapV3LiquidityAtTick(
        liquidityNet=-734560692701879, liquidityGross=8173859228106765, block=18517670
    ),
    258480: UniswapV3LiquidityAtTick(
        liquidityNet=36210639414045186, liquidityGross=41428946111707628, block=18517670
    ),
    258420: UniswapV3LiquidityAtTick(
        liquidityNet=-16368395034512681, liquidityGross=16577757244543031, block=18517670
    ),
    258360: UniswapV3LiquidityAtTick(
        liquidityNet=11676566600396389, liquidityGross=27621347058872833, block=18517670
    ),
    258300: UniswapV3LiquidityAtTick(
        liquidityNet=-6236683989611472, liquidityGross=11783257379970642, block=18517670
    ),
    258240: UniswapV3LiquidityAtTick(
        liquidityNet=-26111698072989509, liquidityGross=35967234961353755, block=18517670
    ),
    258180: UniswapV3LiquidityAtTick(
        liquidityNet=-43752203528125, liquidityGross=2503948569387081, block=18517670
    ),
    258120: UniswapV3LiquidityAtTick(
        liquidityNet=-14019784298647212, liquidityGross=16358266211081848, block=18517670
    ),
    258060: UniswapV3LiquidityAtTick(
        liquidityNet=-50566067420839356, liquidityGross=52492146639742206, block=18517670
    ),
    258000: UniswapV3LiquidityAtTick(
        liquidityNet=-3551067669525354, liquidityGross=8290366955921038, block=18517670
    ),
    257940: UniswapV3LiquidityAtTick(
        liquidityNet=1758152882730366, liquidityGross=2534651163736588, block=18517670
    ),
    257880: UniswapV3LiquidityAtTick(
        liquidityNet=-56656978588286, liquidityGross=16223864920185876, block=18517670
    ),
    257820: UniswapV3LiquidityAtTick(
        liquidityNet=-33937486561329081, liquidityGross=34102009440943215, block=18517670
    ),
    257760: UniswapV3LiquidityAtTick(
        liquidityNet=-10127829585462420, liquidityGross=17262129678917226, block=18517670
    ),
    257700: UniswapV3LiquidityAtTick(
        liquidityNet=96604547308016661, liquidityGross=102791355376716069, block=18517670
    ),
    257640: UniswapV3LiquidityAtTick(
        liquidityNet=-338409326835535874, liquidityGross=338697615812129298, block=18517670
    ),
    257580: UniswapV3LiquidityAtTick(
        liquidityNet=-5094471825546761, liquidityGross=10509748147908773, block=18517670
    ),
    257520: UniswapV3LiquidityAtTick(
        liquidityNet=-3767576344830388, liquidityGross=8396258114838746, block=18517670
    ),
    257460: UniswapV3LiquidityAtTick(
        liquidityNet=-38749335282109174, liquidityGross=39770426890911852, block=18517670
    ),
    257400: UniswapV3LiquidityAtTick(
        liquidityNet=3200284058124153, liquidityGross=33332647227589839, block=18517670
    ),
    257340: UniswapV3LiquidityAtTick(
        liquidityNet=18646241377654652, liquidityGross=19016689046037238, block=18517670
    ),
    257280: UniswapV3LiquidityAtTick(
        liquidityNet=2615520413213675, liquidityGross=9828580804361195, block=18517670
    ),
    257220: UniswapV3LiquidityAtTick(
        liquidityNet=-11229068506159916, liquidityGross=12089429304336690, block=18517670
    ),
    257160: UniswapV3LiquidityAtTick(
        liquidityNet=-11662718150322376, liquidityGross=19676762675093858, block=18517670
    ),
    257100: UniswapV3LiquidityAtTick(
        liquidityNet=786913226889493, liquidityGross=3536557100958133, block=18517670
    ),
    257040: UniswapV3LiquidityAtTick(
        liquidityNet=-5261396896828650, liquidityGross=15328615451424658, block=18517670
    ),
    256980: UniswapV3LiquidityAtTick(
        liquidityNet=43093016764219195, liquidityGross=49358692286301881, block=18517670
    ),
    256920: UniswapV3LiquidityAtTick(
        liquidityNet=14677487677575707, liquidityGross=15356559235194417, block=18517670
    ),
    256860: UniswapV3LiquidityAtTick(
        liquidityNet=40020160591048548, liquidityGross=40020160591048746, block=18517670
    ),
    256800: UniswapV3LiquidityAtTick(
        liquidityNet=11634067886260478, liquidityGross=11681957782482436, block=18517670
    ),
    256740: UniswapV3LiquidityAtTick(
        liquidityNet=5624574287944241, liquidityGross=7375315796345001, block=18517670
    ),
    256680: UniswapV3LiquidityAtTick(
        liquidityNet=12045265224341150, liquidityGross=13820079944701738, block=18517670
    ),
    256620: UniswapV3LiquidityAtTick(
        liquidityNet=3535316951135905, liquidityGross=3536350157626185, block=18517670
    ),
    256560: UniswapV3LiquidityAtTick(
        liquidityNet=17525403052151745, liquidityGross=17584348656064121, block=18517670
    ),
    256500: UniswapV3LiquidityAtTick(
        liquidityNet=1025345449735081, liquidityGross=1063945770181331, block=18517670
    ),
    256440: UniswapV3LiquidityAtTick(
        liquidityNet=727223719311025, liquidityGross=753784782008073, block=18517670
    ),
    256380: UniswapV3LiquidityAtTick(
        liquidityNet=46047691842438496, liquidityGross=46057884167356466, block=18517670
    ),
    256320: UniswapV3LiquidityAtTick(
        liquidityNet=4932200352232408, liquidityGross=4952765854091110, block=18517670
    ),
    256260: UniswapV3LiquidityAtTick(
        liquidityNet=4355883710424067, liquidityGross=4412191561983625, block=18517670
    ),
    256200: UniswapV3LiquidityAtTick(
        liquidityNet=4545562662872915, liquidityGross=4596380565785711, block=18517670
    ),
    256140: UniswapV3LiquidityAtTick(
        liquidityNet=-87872955733423537, liquidityGross=157016906582804017, block=18517670
    ),
    256080: UniswapV3LiquidityAtTick(
        liquidityNet=4682515786536304, liquidityGross=4824079275897156, block=18517670
    ),
    256020: UniswapV3LiquidityAtTick(
        liquidityNet=6209748904086778, liquidityGross=6676628154248658, block=18517670
    ),
    255960: UniswapV3LiquidityAtTick(
        liquidityNet=242338928509014, liquidityGross=706480642465570, block=18517670
    ),
    255900: UniswapV3LiquidityAtTick(
        liquidityNet=57788164122819335, liquidityGross=57788164122819335, block=18517670
    ),
    255840: UniswapV3LiquidityAtTick(
        liquidityNet=2681485933451509, liquidityGross=2681485933451509, block=18517670
    ),
    255780: UniswapV3LiquidityAtTick(
        liquidityNet=32497203800057722, liquidityGross=32497203800057722, block=18517670
    ),
    255720: UniswapV3LiquidityAtTick(
        liquidityNet=345519840372546515, liquidityGross=345519840372546515, block=18517670
    ),
    255660: UniswapV3LiquidityAtTick(
        liquidityNet=206108059657808865, liquidityGross=206108059657808865, block=18517670
    ),
    255600: UniswapV3LiquidityAtTick(
        liquidityNet=69066567049687306, liquidityGross=69066567049687306, block=18517670
    ),
    255540: UniswapV3LiquidityAtTick(
        liquidityNet=12573464478524678, liquidityGross=12651335823246444, block=18517670
    ),
    255480: UniswapV3LiquidityAtTick(
        liquidityNet=5429910519664093, liquidityGross=5432139402073685, block=18517670
    ),
    255420: UniswapV3LiquidityAtTick(
        liquidityNet=5420702164488620, liquidityGross=5420702164488620, block=18517670
    ),
    255360: UniswapV3LiquidityAtTick(
        liquidityNet=2970687973019887, liquidityGross=2970687973019887, block=18517670
    ),
    255300: UniswapV3LiquidityAtTick(
        liquidityNet=3118876516992539, liquidityGross=3118884726570667, block=18517670
    ),
    255240: UniswapV3LiquidityAtTick(
        liquidityNet=15938154594511104, liquidityGross=15938154594511104, block=18517670
    ),
    255180: UniswapV3LiquidityAtTick(
        liquidityNet=3362327716952194, liquidityGross=3383937538597946, block=18517670
    ),
    255120: UniswapV3LiquidityAtTick(
        liquidityNet=-5518315615400599, liquidityGross=12889510906654797, block=18517670
    ),
    255060: UniswapV3LiquidityAtTick(
        liquidityNet=3321720625770939, liquidityGross=3321720625770939, block=18517670
    ),
    255000: UniswapV3LiquidityAtTick(
        liquidityNet=1207883340618470, liquidityGross=1207883340618470, block=18517670
    ),
    254940: UniswapV3LiquidityAtTick(
        liquidityNet=129120170481656356, liquidityGross=129120170481656356, block=18517670
    ),
    254880: UniswapV3LiquidityAtTick(
        liquidityNet=15809890986662671, liquidityGross=15809890986662671, block=18517670
    ),
    254820: UniswapV3LiquidityAtTick(
        liquidityNet=607424827334213, liquidityGross=607424827334213, block=18517670
    ),
    254760: UniswapV3LiquidityAtTick(
        liquidityNet=2331060946123729, liquidityGross=2331060946123729, block=18517670
    ),
    254700: UniswapV3LiquidityAtTick(
        liquidityNet=42064542430490573, liquidityGross=42064542430490573, block=18517670
    ),
    254640: UniswapV3LiquidityAtTick(
        liquidityNet=2358693316849393, liquidityGross=2358693316849393, block=18517670
    ),
    254580: UniswapV3LiquidityAtTick(
        liquidityNet=2810417552742125, liquidityGross=2810417552742125, block=18517670
    ),
    254520: UniswapV3LiquidityAtTick(
        liquidityNet=8805497809819716, liquidityGross=8805497809819716, block=18517670
    ),
    254460: UniswapV3LiquidityAtTick(
        liquidityNet=11237252833638609, liquidityGross=11237252833638609, block=18517670
    ),
    254400: UniswapV3LiquidityAtTick(
        liquidityNet=268093616139520, liquidityGross=268093616139520, block=18517670
    ),
    254340: UniswapV3LiquidityAtTick(
        liquidityNet=4227451208229240, liquidityGross=4227451208229240, block=18517670
    ),
    254280: UniswapV3LiquidityAtTick(
        liquidityNet=3322493141433915, liquidityGross=3322493141433915, block=18517670
    ),
    254220: UniswapV3LiquidityAtTick(
        liquidityNet=490781592320560, liquidityGross=490781592320560, block=18517670
    ),
    254160: UniswapV3LiquidityAtTick(
        liquidityNet=232717215486581, liquidityGross=232717215486581, block=18517670
    ),
    254100: UniswapV3LiquidityAtTick(
        liquidityNet=565480887552709, liquidityGross=565480887552709, block=18517670
    ),
    254040: UniswapV3LiquidityAtTick(
        liquidityNet=7101739089830234, liquidityGross=7101739089830234, block=18517670
    ),
    253980: UniswapV3LiquidityAtTick(
        liquidityNet=3223198388506118, liquidityGross=3223198388506118, block=18517670
    ),
    253920: UniswapV3LiquidityAtTick(
        liquidityNet=79405717454959, liquidityGross=79405717454959, block=18517670
    ),
    253860: UniswapV3LiquidityAtTick(
        liquidityNet=1374759776269187, liquidityGross=1374759776269187, block=18517670
    ),
    253800: UniswapV3LiquidityAtTick(
        liquidityNet=6822448654094831, liquidityGross=6822448654094831, block=18517670
    ),
    253740: UniswapV3LiquidityAtTick(
        liquidityNet=1350933422913769, liquidityGross=1350933422913769, block=18517670
    ),
    253680: UniswapV3LiquidityAtTick(
        liquidityNet=1663577753416874, liquidityGross=1663577753416874, block=18517670
    ),
    253620: UniswapV3LiquidityAtTick(
        liquidityNet=1656277444079970, liquidityGross=1656277444079970, block=18517670
    ),
    253560: UniswapV3LiquidityAtTick(
        liquidityNet=701038218856846, liquidityGross=701038218856846, block=18517670
    ),
    253500: UniswapV3LiquidityAtTick(
        liquidityNet=74614842066107, liquidityGross=74614842066107, block=18517670
    ),
    253440: UniswapV3LiquidityAtTick(
        liquidityNet=7771107673834, liquidityGross=7771107673834, block=18517670
    ),
    253380: UniswapV3LiquidityAtTick(
        liquidityNet=850009840735196, liquidityGross=850009840735196, block=18517670
    ),
    253320: UniswapV3LiquidityAtTick(
        liquidityNet=9578098617804610, liquidityGross=9578098617804610, block=18517670
    ),
    253260: UniswapV3LiquidityAtTick(
        liquidityNet=882253846522151, liquidityGross=882253846522151, block=18517670
    ),
    253200: UniswapV3LiquidityAtTick(
        liquidityNet=2217589294645485, liquidityGross=2217589294645485, block=18517670
    ),
    253140: UniswapV3LiquidityAtTick(
        liquidityNet=22132152381748, liquidityGross=22132152381748, block=18517670
    ),
    253080: UniswapV3LiquidityAtTick(
        liquidityNet=249085331552036, liquidityGross=249085331552036, block=18517670
    ),
    253020: UniswapV3LiquidityAtTick(
        liquidityNet=6721662344261, liquidityGross=6721662344261, block=18517670
    ),
    252900: UniswapV3LiquidityAtTick(
        liquidityNet=314014747876014, liquidityGross=314014747876014, block=18517670
    ),
    252840: UniswapV3LiquidityAtTick(
        liquidityNet=167515635276874, liquidityGross=167515635276874, block=18517670
    ),
    252780: UniswapV3LiquidityAtTick(
        liquidityNet=1159990627245887, liquidityGross=1159990627245887, block=18517670
    ),
    252720: UniswapV3LiquidityAtTick(
        liquidityNet=5047008107017, liquidityGross=5047008107017, block=18517670
    ),
    252660: UniswapV3LiquidityAtTick(
        liquidityNet=512188446270779, liquidityGross=512188446270779, block=18517670
    ),
    252600: UniswapV3LiquidityAtTick(
        liquidityNet=273279821521550, liquidityGross=273279821521550, block=18517670
    ),
    252540: UniswapV3LiquidityAtTick(
        liquidityNet=20966457573470, liquidityGross=20966457573470, block=18517670
    ),
    252480: UniswapV3LiquidityAtTick(
        liquidityNet=9441405421903372, liquidityGross=9441405421903372, block=18517670
    ),
    252420: UniswapV3LiquidityAtTick(
        liquidityNet=73664242866262, liquidityGross=73664242866262, block=18517670
    ),
    252360: UniswapV3LiquidityAtTick(
        liquidityNet=733766399181670, liquidityGross=733766399181670, block=18517670
    ),
    252300: UniswapV3LiquidityAtTick(
        liquidityNet=5145007763816834, liquidityGross=5145007763816834, block=18517670
    ),
    252240: UniswapV3LiquidityAtTick(
        liquidityNet=1196025218095912, liquidityGross=1196025218095912, block=18517670
    ),
    252180: UniswapV3LiquidityAtTick(
        liquidityNet=137301056084937, liquidityGross=137301056084937, block=18517670
    ),
    252120: UniswapV3LiquidityAtTick(
        liquidityNet=218428093170490, liquidityGross=218428093170490, block=18517670
    ),
    252060: UniswapV3LiquidityAtTick(
        liquidityNet=10396565017847, liquidityGross=10396565017847, block=18517670
    ),
    252000: UniswapV3LiquidityAtTick(
        liquidityNet=103709076861484, liquidityGross=103709076861484, block=18517670
    ),
    251940: UniswapV3LiquidityAtTick(
        liquidityNet=303412206496584, liquidityGross=303412206496584, block=18517670
    ),
    251880: UniswapV3LiquidityAtTick(
        liquidityNet=931553309070, liquidityGross=931553309070, block=18517670
    ),
    251820: UniswapV3LiquidityAtTick(
        liquidityNet=238927643400594, liquidityGross=238927643400594, block=18517670
    ),
    251760: UniswapV3LiquidityAtTick(
        liquidityNet=71151341935989, liquidityGross=71151341935989, block=18517670
    ),
    251700: UniswapV3LiquidityAtTick(
        liquidityNet=254940811724985, liquidityGross=254940811724985, block=18517670
    ),
    251640: UniswapV3LiquidityAtTick(
        liquidityNet=506070575264535, liquidityGross=506070575264535, block=18517670
    ),
    251580: UniswapV3LiquidityAtTick(
        liquidityNet=920580097962985, liquidityGross=920580097962985, block=18517670
    ),
    251520: UniswapV3LiquidityAtTick(
        liquidityNet=285298765806058, liquidityGross=285298765806058, block=18517670
    ),
    251460: UniswapV3LiquidityAtTick(
        liquidityNet=228858017740757, liquidityGross=228858017740757, block=18517670
    ),
    251400: UniswapV3LiquidityAtTick(
        liquidityNet=51646374915687, liquidityGross=51646374915687, block=18517670
    ),
    251340: UniswapV3LiquidityAtTick(
        liquidityNet=102153632391551, liquidityGross=102153632391551, block=18517670
    ),
    251280: UniswapV3LiquidityAtTick(
        liquidityNet=2508101241354, liquidityGross=2508101241354, block=18517670
    ),
    251220: UniswapV3LiquidityAtTick(
        liquidityNet=2067006751200965, liquidityGross=2067006751200965, block=18517670
    ),
    251160: UniswapV3LiquidityAtTick(
        liquidityNet=1433371311279, liquidityGross=1433371311279, block=18517670
    ),
    251100: UniswapV3LiquidityAtTick(
        liquidityNet=112091084363005, liquidityGross=112091084363005, block=18517670
    ),
    251040: UniswapV3LiquidityAtTick(
        liquidityNet=212536642944127, liquidityGross=212536642944127, block=18517670
    ),
    250980: UniswapV3LiquidityAtTick(
        liquidityNet=6183529048927, liquidityGross=6183529048927, block=18517670
    ),
    250920: UniswapV3LiquidityAtTick(
        liquidityNet=478673355767477, liquidityGross=478673355767477, block=18517670
    ),
    250860: UniswapV3LiquidityAtTick(
        liquidityNet=307105541326301, liquidityGross=307105541326301, block=18517670
    ),
    250800: UniswapV3LiquidityAtTick(
        liquidityNet=486676008472444, liquidityGross=486676008472444, block=18517670
    ),
    250740: UniswapV3LiquidityAtTick(
        liquidityNet=30685778627595, liquidityGross=30685778627595, block=18517670
    ),
    250680: UniswapV3LiquidityAtTick(
        liquidityNet=343632980290124, liquidityGross=343632980290124, block=18517670
    ),
    250620: UniswapV3LiquidityAtTick(
        liquidityNet=532471927087429, liquidityGross=532471927087429, block=18517670
    ),
    250560: UniswapV3LiquidityAtTick(
        liquidityNet=477427362162841, liquidityGross=477427362162841, block=18517670
    ),
    250500: UniswapV3LiquidityAtTick(
        liquidityNet=2954112272092633, liquidityGross=2954112272092633, block=18517670
    ),
    250440: UniswapV3LiquidityAtTick(
        liquidityNet=75870743740124, liquidityGross=75870743740124, block=18517670
    ),
    250380: UniswapV3LiquidityAtTick(
        liquidityNet=17046815729350, liquidityGross=17046815729350, block=18517670
    ),
    250320: UniswapV3LiquidityAtTick(
        liquidityNet=145060936845339, liquidityGross=145060936845339, block=18517670
    ),
    250260: UniswapV3LiquidityAtTick(
        liquidityNet=196910001717446, liquidityGross=196910001717446, block=18517670
    ),
    250200: UniswapV3LiquidityAtTick(
        liquidityNet=153903268977929, liquidityGross=153903268977929, block=18517670
    ),
    250140: UniswapV3LiquidityAtTick(
        liquidityNet=304551192169091, liquidityGross=304551192169091, block=18517670
    ),
    250080: UniswapV3LiquidityAtTick(
        liquidityNet=885983345157125, liquidityGross=885983345157125, block=18517670
    ),
    250020: UniswapV3LiquidityAtTick(
        liquidityNet=112201242447706, liquidityGross=112201242447706, block=18517670
    ),
    249960: UniswapV3LiquidityAtTick(
        liquidityNet=1877538880511015, liquidityGross=1877538880511015, block=18517670
    ),
    249900: UniswapV3LiquidityAtTick(
        liquidityNet=3018585079042067, liquidityGross=3018585079042067, block=18517670
    ),
    249840: UniswapV3LiquidityAtTick(
        liquidityNet=150302362314233, liquidityGross=150302362314233, block=18517670
    ),
    249780: UniswapV3LiquidityAtTick(
        liquidityNet=76115806188819, liquidityGross=76115806188819, block=18517670
    ),
    249720: UniswapV3LiquidityAtTick(
        liquidityNet=128404454018236, liquidityGross=128404454018236, block=18517670
    ),
    249660: UniswapV3LiquidityAtTick(
        liquidityNet=46357218604509, liquidityGross=46357218604509, block=18517670
    ),
    249600: UniswapV3LiquidityAtTick(
        liquidityNet=36621642612612, liquidityGross=36621642612612, block=18517670
    ),
    249540: UniswapV3LiquidityAtTick(
        liquidityNet=488612000165439, liquidityGross=488612000165439, block=18517670
    ),
    249480: UniswapV3LiquidityAtTick(
        liquidityNet=203698672050888, liquidityGross=203698672050888, block=18517670
    ),
    249420: UniswapV3LiquidityAtTick(
        liquidityNet=9128851335772, liquidityGross=9128851335772, block=18517670
    ),
    249360: UniswapV3LiquidityAtTick(
        liquidityNet=233194264562651, liquidityGross=233194264562651, block=18517670
    ),
    249300: UniswapV3LiquidityAtTick(
        liquidityNet=688394752753546, liquidityGross=688394752753546, block=18517670
    ),
    249240: UniswapV3LiquidityAtTick(
        liquidityNet=2400965140222275, liquidityGross=2400965140222275, block=18517670
    ),
    249180: UniswapV3LiquidityAtTick(
        liquidityNet=805435724180, liquidityGross=805435724180, block=18517670
    ),
    249120: UniswapV3LiquidityAtTick(
        liquidityNet=8546121898214, liquidityGross=8546121898214, block=18517670
    ),
    249060: UniswapV3LiquidityAtTick(
        liquidityNet=9471041602366, liquidityGross=9471041602366, block=18517670
    ),
    249000: UniswapV3LiquidityAtTick(
        liquidityNet=372745130402934, liquidityGross=372745130402934, block=18517670
    ),
    248940: UniswapV3LiquidityAtTick(
        liquidityNet=10262040820492, liquidityGross=10262040820492, block=18517670
    ),
    248880: UniswapV3LiquidityAtTick(
        liquidityNet=1191600446578, liquidityGross=1191600446578, block=18517670
    ),
    248820: UniswapV3LiquidityAtTick(
        liquidityNet=231267402130471, liquidityGross=231267402130471, block=18517670
    ),
    248760: UniswapV3LiquidityAtTick(
        liquidityNet=130017625795223, liquidityGross=130017625795223, block=18517670
    ),
    248700: UniswapV3LiquidityAtTick(
        liquidityNet=107658938294395, liquidityGross=107658938294395, block=18517670
    ),
    248640: UniswapV3LiquidityAtTick(
        liquidityNet=34036669144137, liquidityGross=34036669144137, block=18517670
    ),
    248580: UniswapV3LiquidityAtTick(
        liquidityNet=96340504853, liquidityGross=96340504853, block=18517670
    ),
    248520: UniswapV3LiquidityAtTick(
        liquidityNet=339165199161295, liquidityGross=339165199161295, block=18517670
    ),
    248460: UniswapV3LiquidityAtTick(
        liquidityNet=15823287912623, liquidityGross=15823287912623, block=18517670
    ),
    248400: UniswapV3LiquidityAtTick(
        liquidityNet=106096220552937, liquidityGross=106096220552937, block=18517670
    ),
    248340: UniswapV3LiquidityAtTick(
        liquidityNet=745663361773900, liquidityGross=745663361773900, block=18517670
    ),
    248280: UniswapV3LiquidityAtTick(
        liquidityNet=5974932615038, liquidityGross=5974932615038, block=18517670
    ),
    248160: UniswapV3LiquidityAtTick(
        liquidityNet=102395958293758, liquidityGross=102395958293758, block=18517670
    ),
    248100: UniswapV3LiquidityAtTick(
        liquidityNet=25900754677899, liquidityGross=25900754677899, block=18517670
    ),
    248040: UniswapV3LiquidityAtTick(
        liquidityNet=252942942781, liquidityGross=252942942781, block=18517670
    ),
    247980: UniswapV3LiquidityAtTick(
        liquidityNet=23028334649368, liquidityGross=23028334649368, block=18517670
    ),
    247920: UniswapV3LiquidityAtTick(
        liquidityNet=5342639837069495, liquidityGross=5342639837069495, block=18517670
    ),
    247860: UniswapV3LiquidityAtTick(
        liquidityNet=1649567047148, liquidityGross=1649567047148, block=18517670
    ),
    247740: UniswapV3LiquidityAtTick(
        liquidityNet=11523276594189, liquidityGross=11523276594189, block=18517670
    ),
    247620: UniswapV3LiquidityAtTick(
        liquidityNet=487875820420, liquidityGross=487875820420, block=18517670
    ),
    247440: UniswapV3LiquidityAtTick(
        liquidityNet=724237872589606, liquidityGross=724237872589606, block=18517670
    ),
    247320: UniswapV3LiquidityAtTick(
        liquidityNet=48519918747269, liquidityGross=48519918747269, block=18517670
    ),
    246960: UniswapV3LiquidityAtTick(
        liquidityNet=14264473913293, liquidityGross=14264473913293, block=18517670
    ),
    246900: UniswapV3LiquidityAtTick(
        liquidityNet=263365923015, liquidityGross=263365923015, block=18517670
    ),
    246840: UniswapV3LiquidityAtTick(
        liquidityNet=1143995552, liquidityGross=1143995552, block=18517670
    ),
    246780: UniswapV3LiquidityAtTick(
        liquidityNet=233944583115, liquidityGross=233944583115, block=18517670
    ),
    246720: UniswapV3LiquidityAtTick(
        liquidityNet=9703483785504, liquidityGross=9703483785504, block=18517670
    ),
    246660: UniswapV3LiquidityAtTick(
        liquidityNet=14690532940788, liquidityGross=14690532940788, block=18517670
    ),
    246600: UniswapV3LiquidityAtTick(
        liquidityNet=4814170393189, liquidityGross=4814170393189, block=18517670
    ),
    246480: UniswapV3LiquidityAtTick(
        liquidityNet=83562433586, liquidityGross=83562433586, block=18517670
    ),
    246420: UniswapV3LiquidityAtTick(
        liquidityNet=69925333162032, liquidityGross=69925333162032, block=18517670
    ),
    246360: UniswapV3LiquidityAtTick(
        liquidityNet=1235001955603188, liquidityGross=1235001955603188, block=18517670
    ),
    246180: UniswapV3LiquidityAtTick(
        liquidityNet=4890971540, liquidityGross=4890971540, block=18517670
    ),
    245940: UniswapV3LiquidityAtTick(
        liquidityNet=76701235421656, liquidityGross=76701235421656, block=18517670
    ),
}
```

### Uniswap Arbitrage
TBD

### Local Forking With Anvil
TBD