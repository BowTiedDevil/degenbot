# ADR-005 slice 7 step 4b + P1 tracker-family removal: this fork-gated test
# imported the deleted hollow V2 DEX subclasses (Sushi/Pancake/Swapbased/
# Camelot) and the pool-tracker family (both now removed). The canonical
# ``UniswapV2Pool`` + ``dex.variant`` model replaced the subclasses, and
# ``Bot.build_pool`` is the only construction door. The module stays skipped
# pending a rewrite under anvil.
import pytest

pytest.skip(
    "ADR-005 slice 7 step 4b + tracker-family removal: fork test pending rewrite",
    allow_module_level=True,
)
