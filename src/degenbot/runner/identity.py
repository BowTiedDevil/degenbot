"""Deployment and identity material for the settlement-arbitrage ``BotRunner``.

The Ethereum mainnet deployment addresses and token sets the driver's leaves
consume, the dry-run operator placeholders, the private keys the repository
publishes, and the address checksum helper. These are properties of the chain
and of the operator's deployment, not of the config schema, so they live apart
from the config value object and its factory.

The pure-Rust parity example (``rust/examples/settlement_bot/src/main.rs``)
mirrors the defaults and the checksum helper declared here; a default changed
here has a Rust twin there.
"""

from __future__ import annotations

from pathlib import Path

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import WRAPPED_NATIVE_TOKENS
from degenbot.types.chain import ChainId

WETH_ADDRESS = WRAPPED_NATIVE_TOKENS[ChainId.ETH]
MULTICALL3_ADDRESS = "0xcA11bde05977b3631167028862bE2a173976CA11"

# Verified standard ERC-20 intermediates for Ethereum mainnet.
# Every token here is confirmed to have NO transfer fees and NO rebase.
ETH_MAINNET_ALLOWED_TOKENS: set[str] = {
    "0x163f8C2467924be0ae7B5347228CABF260318753",  # WLD
    "0x6c3ea9036406852006290770BEdFcAbA0e23A0e8",  # PyUSD
    "0xB8c77482e45F1F44dE1745F52C74426C631bDD52",  # BNB
    "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",  # WETH
    "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
    "0xdAC17F958D2ee523a2206206994597C13D831ec7",  # USDT
    "0x6B175474E89094C44Da98b954EedeAC495271d0F",  # DAI
    "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",  # WBTC
    "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984",  # UNI
    "0x514910771AF9Ca656af840dff83E8264EcF986CA",  # LINK
    "0x6B3595068778DD592e39A122f4f5a5cF09C90fE2",  # SUSHI
    "0xD533a949740bb3306d119CC777fa900bA034cd52",  # CRV
    "0xc00e94Cb662C3520282E6f5717214004A7f26888",  # COMP
    "0x0bc529c00C6401aEF6D220BE8C6Ea1667F6Ad93e",  # YFI
    "0x7D1AfA7B718fb893dB30A3aBc0Cfc608AaCfeBB0",  # MATIC/POL
    # Curated second-tier widening (this run's scope decision): established
    # no-tax tokens with deep pools, so long-tail arbitrage candidates are
    # offered to the solver instead of being filtered at crawl time.
    "0x95aD61b0a150d79219dCF64E1E6Cc01f0B64C4cE",  # SHIB
    "0x6982508145454Ce325dDbE47a25d4ec3d2311933",  # PEPE
    "0x5A98FcBEA516Cf06857215779Fd812CA3beF1B32",  # LDO
    "0x7Fc66500c84A76Ad7e9c93437bFc5Ac33E2DDaE9",  # AAVE
    "0xC011a73ee8576Fb46F5E1c5751cA3B9Fe0af2a6F",  # SNX
    "0x9f8F72aA9304c8B593d555F12eF6589cC3A579A2",  # MKR
    "0x912CE59144191C1204E64559FE8253a0e49E6548",  # ARB
    "0x7f39C581F595B53c5cb19bD0b3f8dA6c935E2Ca0",  # wstETH (non-rebasing wrap)
    "0xae78736Cd615f374D3085123A210448E74Fc6393",  # rETH
    "0xf939E0A03FB07F59A73314E73794Be0E57ac1b4E",  # crvUSD
    "0x853d955aCEf822Db058eb1555913474ccaD85CA7",  # FRAX
    "0x4c9EDD5852cd905f086C759E8383e09bff1E68B3",  # USDe
    "0x18084fbA666a33d37592fA2633fD49a74DD93a88",  # tBTC
    "0xcbB7C0000aB88B473b1f5aFd9ef808440eed33Bf",  # cbBTC
    "0x2416092f143aa786967e909fda3cb38aac16d7e4",  # ezETH
    "0x111111111117dC0aa78b770fA6A738034120C302",  # 1INCH
    "0x4d224452801ACEd8B2F0aebE155379bb5D594381",  # APE
}

# Only build paths where intermediate hops use these tokens. Set to None to
# allow all tokens. Pools connecting a non-whitelisted token are excluded,
# eliminating tax/fee-on-transfer tokens that waste sim gas and always revert.
ALLOWED_INTERMEDIATE_TOKENS: set[str] | None = ETH_MAINNET_ALLOWED_TOKENS

# V3 factories (Ethereum mainnet).
UNISWAP_V3_MAINNET_FACTORY = "0x33128a8fC17869897dcE68Ed026d694621f6FDfD"
SUSHISWAP_V3_MAINNET_FACTORY = "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"
PANCAKESWAP_V3_MAINNET_FACTORY = "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"

# V4 PoolManager (Ethereum mainnet).
UNISWAP_V4_POOL_MANAGER_ADDRESS = get_checksum_address("0x000000000004444c5dc75cB358380D2e3De08A90")

# Ethereum mainnet default allowed intermediate tokens — mirrors the example's
# ETH_MAINNET_ALLOWED_TOKENS set.
_ALLOWED_INTERMEDIATE_TOKENS = frozenset({
    "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
    "0xdAC17F958D2ee523a2206206994597C13D831ec7",  # USDT
    "0x6B175474E89094C44Da98b954EedeAC495271d0F",  # DAI
    "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",  # WBTC
    "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984",  # UNI
    "0x514910771AF9Ca656af840dff83E8264EcF986CA",  # LINK
    "0x6B3595068778DD592e39A122f4f5a5cF09C90fE2",  # SUSHI
    "0xD533a949740bb3306d119CC777fa900bA034cd52",  # CRV
    "0xc00e94Cb662C3520282E6f5717214004A7f26888",  # COMP
    "0x0bc529c00C6401aEF6D220BE8C6Ea1667F6Ad93e",  # YFI
    "0x7D1AfA7B718fb893dB30A3aBc0Cfc608AaCfeBB0",  # MATIC/POL
})

# Default executor deployment constants — mirror the example's env defaults.
_DEFAULT_EXECUTOR_ADDRESS = "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5"
_DEFAULT_INJECTED_ADDRESS = "0x0D6d4c3cF3BD3b769De1821f2BE0d7d99913E4F1"
_DEFAULT_EXECUTOR_OWNER = "0x9C56a29c7231974c269E24F9FB3c29203039089E"

# Dry-run operator placeholder: a VALID secp256k1 private key + its derived
# address, used when the process environment omits `OPERATOR_*` in non-live mode.
# The (now-eager) `TxSigner(key=operator_private_key, chain_id=1)` site
# rejects the former all-zero placeholder (zero is not a valid scalar) and
# raised `ValueError: signature error`. The Anvil account-0 key is a
# well-known valid throwaway that never signs in dry-run: the Rust submit
# leaf's `dry_run` guard skips `sign_eip1559` for every candidate.
_DRY_RUN_OPERATOR_PRIVATE_KEY = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
_DRY_RUN_OPERATOR_ADDRESS = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"

# Private keys the repository publishes, from their one declared home. The
# Python driver and the Rust parity example both read this manifest rather
# than carry a copy that can drift; a live run refuses what it names, because
# signing with a published key would submit real transactions under a key
# anyone can read.
_PUBLISHED_OPERATOR_PRIVATE_KEYS_PATH = Path(__file__).with_name(
    "published_operator_private_keys.txt"
)


def _published_operator_private_keys() -> frozenset[str]:
    """The lowercased, 0x-prefixed keys the repository publishes.

    Blank lines and ``#`` comments are ignored, so the manifest can carry the
    classification's rationale beside the keys.

    Returns:
        The published keys, compared case-insensitively by the live refusal.

    """
    text = _PUBLISHED_OPERATOR_PRIVATE_KEYS_PATH.read_text(encoding="utf-8")
    return frozenset(
        stripped.lower()
        for raw in text.splitlines()
        if (stripped := raw.strip()) and not stripped.startswith("#")
    )


_PLACEHOLDER_OPERATOR_PRIVATE_KEYS = _published_operator_private_keys()


def _checksum_or_empty(addr: str | None) -> str:
    """Checksum an address, returning "" for empty input.

    Mirrors ``main()``'s ``get_checksum_address`` handling of an unset field.

    Returns:
        The checksummed address, or ``""`` for empty input.

    """
    if not addr:
        return ""
    return get_checksum_address(addr)
