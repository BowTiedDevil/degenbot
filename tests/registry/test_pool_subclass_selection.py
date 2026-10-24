# This fork-gated test targets the removed per-DEX V2 subclasses and the
# pool-tracker family. The canonical ``UniswapV2Pool`` + ``dex.variant`` model
# replaced the subclasses, and ``Bot.build_pool`` is the only construction
# door. The module stays skipped pending a rewrite under anvil.
import pytest

pytest.skip(
    "fork test pending rewrite against the UniswapV2Pool + dex.variant model",
    allow_module_level=True,
)
