"""Deployment and identity material for the settlement-arbitrage ``BotRunner``.

The Ethereum mainnet deployment addresses the driver's leaves consume, the
dry-run operator placeholders, the private keys the repository
publishes, and the address checksum helper. These are properties of the chain
and of the operator's deployment, not of the config schema, so they live apart
from the config value object and its factory.

The pure-Rust parity example (``rust/examples/settlement_bot/src/main.rs``)
carries its own copy of the deployment identity below. The core deliberately
does not own one deployment's executor/operator identity — a ``cargo add
degenbot`` consumer deploys its own — so the example's copy is an independent
parity mirror rather than a re-export, and a default changed here must change
there to keep the parity twin faithful. If the example stops being a parity
mirror, delete its copy; do not grow a second home.
"""

from __future__ import annotations

from pathlib import Path

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import WRAPPED_NATIVE_TOKENS
from degenbot.types.chain import ChainId

WETH_ADDRESS = WRAPPED_NATIVE_TOKENS[ChainId.ETH]
MULTICALL3_ADDRESS = "0xcA11bde05977b3631167028862bE2a173976CA11"

# V3 factories (Ethereum mainnet).
UNISWAP_V3_MAINNET_FACTORY = "0x33128a8fC17869897dcE68Ed026d694621f6FDfD"
SUSHISWAP_V3_MAINNET_FACTORY = "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"
PANCAKESWAP_V3_MAINNET_FACTORY = "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"

# V4 PoolManager (Ethereum mainnet).
UNISWAP_V4_POOL_MANAGER_ADDRESS = get_checksum_address("0x000000000004444c5dc75cB358380D2e3De08A90")

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
    """Read the lowercased, 0x-prefixed keys the repository publishes.

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
