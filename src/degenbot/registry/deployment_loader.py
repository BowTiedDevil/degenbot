"""Load DEX deployment data from a shipped JSON file + user overlay.

The shipped ``deployments.json`` is the single source of deployment data
(``chain_id``, ``factory`` → ``deployer`` / ``init_hash`` / ``variant`` /
``dex_identity`` preset). A user overlay path may be declared in
``config.toml`` under ``[deployments]`` → ``overlay``; entries there
override shipped entries on ``(chain_id, factory)`` conflict (user wins).

This module reads the data only — it does not touch
``pool_type_registry``. ``PoolTypeRegistry.register_from_deployments`` (a
separate wiring step) resolves ``pool_type`` → Python class +
``dex_variant`` → ``DexIdentity`` preset and calls the existing
``register()``.

Schema (``deployments.json``):

.. code-block:: json

    {
      "deployments": [
        {
          "name": "Uniswap V2",
          "chain_id": 1,
          "pool_type": "uniswap-v2",
          "variant": null,
          "dex_variant": "uniswap-v2",
          "family": null,
          "factory": "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
          "deployer": null,
          "init_hash": "0x96e8ac4277..."
        }
      ]
    }

Field semantics (mirror what the inline ``_register_*_deployments()``
tuples currently pass to ``pool_type_registry.register()``):

- ``pool_type``: string key into :data:`POOL_TYPE_MAP` (the Python companion
  class). This is a companion-layer concern (ADR-005) — the JSON carries a
  string, the loader resolves it to a class.
- ``variant``: the DB-kind variant. ``null`` means "use the class's
  ``variant`` ClassVar" (``register()`` calls ``getattr(cls, "variant",
  None)``). A string is an explicit override (e.g. ``"pancakeswap"`` on
  ``UniswapV2Pool``, whose own ``variant`` is ``None``).
- ``dex_variant``: the ``DexIdentity`` preset string (e.g.
  ``"camelot-v2-volatile"``). ``null`` means no ``dex_identity`` (V3,
  Aerodrome V2, Balancer).
- ``family``: override for ``_derive_family`` (Balancer needs this — its
  classes have ``tokens`` without ``fee_token0`` and would misclassify).
  ``null`` means auto-derive. One of ``"weighted"`` / ``"stableswap"``.
- ``factory``: EIP-55 checksummed factory address.
- ``deployer``: CREATE2 deployer. ``null`` → ``factory`` (the
  ``register()`` default).
- ``init_hash``: CREATE2 init code hash. ``null`` / ``""`` → no CREATE2
  (Aerodrome, Balancer).
"""

from __future__ import annotations

import json
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

from degenbot.checksum_cache import get_checksum_address
from degenbot.config import CONFIG_FILE
from degenbot.logging import logger
from degenbot.types import dex_identity
from degenbot.types.pool_type import PoolFamily

if TYPE_CHECKING:
    from degenbot.registry.pool_type import PoolTypeRegistry
    from degenbot.types import DexIdentity
    from degenbot.types.abstract import AbstractLiquidityPool

_SHIPPED_JSON = Path(__file__).with_name("deployments.json")


@dataclass(frozen=True)
class DeploymentRecord:
    """A single deployment row loaded from JSON."""

    name: str
    chain_id: int
    pool_type: str
    variant: str | None
    dex_variant: str | None
    family: str | None
    factory: str
    deployer: str | None
    init_hash: str | None


# ``pool_type`` string → Python companion class. This map lives in the loader
# (companion layer, ADR-005) — the JSON carries only the string key, so the
# data file stays free of Python-class coupling and a standalone-Rust consumer
# reading the same JSON would carry its own (Rust-side) pool_type → enum map.
#
# Lazy: pool classes trigger heavy import chains (aerodrome → uniswap →
# trackers → deployments → this loader) which would circular-import if
# constructed at module level. Built once on first access.
_POOL_TYPE_MAP: dict[str, type[AbstractLiquidityPool]] | None = None


def _pool_type_map() -> dict[str, type[AbstractLiquidityPool]]:
    """Return the pool_type → class map, building it lazily on first call.

    Returns:
        A mapping of pool type string to its Python pool class.

    """
    global _POOL_TYPE_MAP  # noqa: PLW0603
    if _POOL_TYPE_MAP is None:
        from degenbot.aerodrome.pools import AerodromeV2Pool, AerodromeV3Pool
        from degenbot.balancer.pools import BalancerV2Pool
        from degenbot.balancer.stable_pools import BalancerV2StablePool
        from degenbot.pancakeswap.pools import PancakeswapV3Pool
        from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
        from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

        _POOL_TYPE_MAP = {
            "uniswap-v2": UniswapV2Pool,
            "uniswap-v3": UniswapV3Pool,
            "pancakeswap-v3": PancakeswapV3Pool,
            "sushiswap-v3": UniswapV3Pool,
            "aerodrome-v2": AerodromeV2Pool,
            "aerodrome-v3": AerodromeV3Pool,
            "balancer-weighted": BalancerV2Pool,
            "balancer-stable": BalancerV2StablePool,
        }
    return _POOL_TYPE_MAP


def _parse_record(raw: dict[str, object]) -> DeploymentRecord:
    """Parse one JSON object into a :class:`DeploymentRecord`.

    Returns:
        The parsed deployment record.

    Raises:
        ValueError: If ``pool_type`` is unknown.

    """
    pool_type = _require_str(raw, "pool_type")
    if pool_type not in _pool_type_map():
        msg = f"Unknown pool_type {pool_type!r} in deployments JSON"
        raise ValueError(msg)
    init_hash_raw = raw.get("init_hash")
    init_hash = init_hash_raw if isinstance(init_hash_raw, str) and init_hash_raw else None
    return DeploymentRecord(
        name=_require_str(raw, "name"),
        chain_id=_require_int(raw, "chain_id"),
        pool_type=pool_type,
        variant=_optional_str(raw, "variant"),
        dex_variant=_optional_str(raw, "dex_variant"),
        family=_optional_str(raw, "family"),
        factory=get_checksum_address(_require_str(raw, "factory")),
        deployer=_optional_str(raw, "deployer"),
        init_hash=init_hash,
    )


def _require_str(raw: dict[str, object], key: str) -> str:
    value = raw.get(key)
    if not isinstance(value, str):
        msg = f"deployments JSON entry missing required string field {key!r}"
        raise TypeError(msg)
    return value


def _require_int(raw: dict[str, object], key: str) -> int:
    value = raw.get(key)
    if not isinstance(value, int) or isinstance(value, bool):
        msg = f"deployments JSON entry missing required integer field {key!r}"
        raise TypeError(msg)
    return value


def _optional_str(raw: dict[str, object], key: str) -> str | None:
    value = raw.get(key)
    if value is None:
        return None
    if isinstance(value, str):
        return value
    msg = f"deployments JSON field {key!r} must be a string or null"
    raise TypeError(msg)


def _read_json(path: Path) -> list[DeploymentRecord]:
    """Read and parse a deployments JSON file.

    Returns:
        The parsed deployment records.

    Raises:
        ValueError: If the JSON structure is invalid (no top-level ``deployments`` list).

    """
    text = path.read_text(encoding="utf-8")
    data = json.loads(text)
    deployments = data.get("deployments") if isinstance(data, dict) else None
    if not isinstance(deployments, list):
        msg = f"deployments JSON at {path} must have a top-level 'deployments' list"
        raise ValueError(msg)  # noqa: TRY004 — structural error, not a type error
    return [_parse_record(entry) for entry in deployments]


def _overlay_path_from_config() -> Path | None:
    """Read the ``[deployments] overlay`` path from ``config.toml``, if set.

    Returns:
        The overlay path (expanded + absolute), or ``None`` when no config
        exists or the section/key is absent.

    Expands ``~`` and resolves to an absolute path (mirrors
    :func:`~degenbot.config.load_config_from_file`'s path handling for the
    database setting).

    """
    if not CONFIG_FILE.exists():
        return None
    try:
        with CONFIG_FILE.open("rb") as fh:
            data = tomllib.load(fh)
    except (OSError, tomllib.TOMLDecodeError):
        return None
    section = data.get("deployments")
    if not isinstance(section, dict):
        return None
    overlay = section.get("overlay")
    if not isinstance(overlay, str) or not overlay:
        return None
    return Path(overlay).expanduser().absolute()


def load_deployments(*, overlay_path: Path | str | None = None) -> list[DeploymentRecord]:
    """Load deployment records from the shipped JSON, merged with an optional overlay.

    The overlay is resolved in this order:

    1. The ``overlay_path`` argument (programmatic — used by tests).
    2. The ``[deployments] overlay`` setting in ``~/.config/degenbot/config.toml``.

    Shipped defaults load first; overlay entries override on
    ``(chain_id, factory)`` conflict (overlay wins). The merged list preserves
    insertion order (shipped order, with overlays replacing in-place).

    Returns:
        The merged deployment records.

    """
    records = _read_json(_SHIPPED_JSON)
    overlay = Path(overlay_path) if overlay_path is not None else _overlay_path_from_config()
    if overlay is not None and overlay.exists():
        overlay_records = _read_json(overlay)
        records = _merge_overlay(records, overlay_records, overlay)
    return records


def _merge_overlay(
    base: list[DeploymentRecord],
    overlay: list[DeploymentRecord],
    overlay_path: Path,
) -> list[DeploymentRecord]:
    """Merge overlay entries into base, keyed by ``(chain_id, factory)``.

    Overlay wins on conflict. The returned list preserves base order, with
    overlay entries replacing in-place; overlay-only entries (new
    ``(chain_id, factory)`` keys not in base) append at the end.

    Returns:
        The merged list (base mutated in-place + returned).

    """
    index: dict[tuple[int, str], int] = {(r.chain_id, r.factory): i for i, r in enumerate(base)}
    for record in overlay:
        key = (record.chain_id, record.factory)
        if key in index:
            base[index[key]] = record
        else:
            base.append(record)
    logger.debug(
        f"Merged {len(overlay)} overlay deployment(s) from {overlay_path} "
        f"({sum(1 for r in overlay if (r.chain_id, r.factory) in index)} overrides, "
        f"{sum(1 for r in overlay if (r.chain_id, r.factory) not in index)} additions)."
    )
    return base


def load_json_deployments(path: Path | str) -> list[DeploymentRecord]:
    """Load deployment records from an explicit JSON file path.

    Convenience wrapper around the internal JSON reader — exposed for tests
    and programmatic callers that want to load a specific file without the
    shipped-defaults merge.

    Returns:
        The parsed deployment records.

    """
    return _read_json(Path(path))


def _resolve_family(family_str: str | None) -> PoolFamily | None:
    """Resolve a JSON family string to a :class:`PoolFamily` enum value.

    Returns:
        The matching ``PoolFamily``, or ``None`` when ``family_str`` is
        ``None`` (auto-derive at register time via ``_derive_family``).
        The string must match a ``PoolFamily`` enum value (``"weighted"``,
        ``"stableswap"``, …).

    """
    if family_str is None:
        return None
    return PoolFamily(family_str)


def register_from_deployments(records: list[DeploymentRecord], registry: PoolTypeRegistry) -> None:
    """Register deployment records into a :class:`PoolTypeRegistry`.

    Companion-layer orchestration (ADR-005): resolves the JSON string keys
    (``pool_type`` → Python class, ``dex_variant`` → ``DexIdentity`` preset,
    ``family`` string → ``PoolFamily`` enum) and calls the registry's
    low-level ``register()`` primitive for each record.

    The ``dex_variant`` preset is resolved via
    :func:`~degenbot._ffi.dex_identity` and asserted non-None — a
    preset string in the JSON must resolve, otherwise the deployment data is
    inconsistent with the compiled ``DexIdentity`` presets (Rust-side).

    Args:
        records: The deployment records (typically from :func:`load_deployments`).
        registry: The target registry (e.g. the ``pool_type_registry`` singleton).

    """
    for record in records:
        pool_class = _pool_type_map()[record.pool_type]
        family = _resolve_family(record.family)
        if record.dex_variant is not None:
            identity: DexIdentity | None = dex_identity(record.dex_variant)
            assert identity is not None, (
                f"dex_variant {record.dex_variant!r} for "
                f"{record.name} (chain {record.chain_id}, {record.factory}) "
                f"did not resolve to a DexIdentity preset"
            )
        else:
            identity = None
        init_hash = record.init_hash or None
        registry.register(
            pool_class,
            chain_id=record.chain_id,
            factory_address=record.factory,
            pool_init_hash=init_hash,
            deployer=record.deployer,
            family=family,
            variant=record.variant,
            dex_identity=identity,
        )
