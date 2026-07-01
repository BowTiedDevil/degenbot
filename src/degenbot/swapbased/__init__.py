"""SwapBased DEX pool type registration.

ADR-005 slice 7 step 4b: the hollow ``SwapbasedV2Pool`` subclass is deleted
— the canonical ``UniswapV2Pool`` is registered for the SwapBased factory,
keyed on the ``swapbased-v2`` DexIdentity preset (variant tag, reserves ABI
shape, default fees) + the ``variant="swapbased"`` override (preserves the
``swapbased_v2`` DB kind for backward compatibility).

Deployment data (chain_id, factory → deployer / init_hash / variant /
dex_identity) is loaded from the shipped ``deployments.json`` by the
top-level ``degenbot`` package init via
``register_from_deployments(load_deployments())`` (ADR-005).
"""
