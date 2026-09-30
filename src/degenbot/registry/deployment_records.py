"""Deployment data records: read + merge the shipped JSON and operator overlay.

Data layer only (ADR-005): this module knows the JSON schema and the valid
``pool_type`` keys, and resolves nothing to Python classes — the companion
class map and the registry registration live in
:mod:`degenbot.registry.deployment_loader`. It is a leaf module: importing it
must not pull in any pool implementation, which is what lets
:mod:`degenbot.uniswap.deployments` bind its constants at import time and
lets the loader import the pool classes eagerly.

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

Field semantics:

- ``pool_type``: string key into the loader's companion class map. The JSON
  carries only the string; the loader resolves it to a class.
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

from degenbot.checksum_cache import get_checksum_address
from degenbot.config import config_file_path
from degenbot.exceptions.base import DegenbotValueError
from degenbot.logging import logger

_SHIPPED_JSON = Path(__file__).with_name("deployments.json")

# The pool_type strings the JSON may carry. The loader's companion class map
# (deployment_loader.POOL_TYPE_MAP) must cover exactly these keys; the
# alignment is pinned by tests/registry/test_deployment_loader.py.
KNOWN_POOL_TYPES = frozenset({
    "uniswap-v2",
    "uniswap-v3",
    "pancakeswap-v3",
    "sushiswap-v3",
    "aerodrome-v2",
    "aerodrome-v3",
    "balancer-weighted",
    "balancer-stable",
})


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
    implementation_address: str | None = None


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


def _parse_record(raw: dict[str, object]) -> DeploymentRecord:
    """Parse one JSON object into a :class:`DeploymentRecord`.

    Returns:
        The parsed deployment record.

    Raises:
        ValueError: If ``pool_type`` is unknown.

    """
    pool_type = _require_str(raw, "pool_type")
    if pool_type not in KNOWN_POOL_TYPES:
        msg = f"Unknown pool_type {pool_type!r} in deployments JSON"
        raise ValueError(msg)
    init_hash_raw = raw.get("init_hash")
    init_hash = init_hash_raw if isinstance(init_hash_raw, str) and init_hash_raw else None
    implementation_raw = raw.get("implementation_address")
    implementation_address = (
        get_checksum_address(implementation_raw)
        if isinstance(implementation_raw, str) and implementation_raw
        else None
    )
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
        implementation_address=implementation_address,
    )


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
        raise ValueError(msg)  # ruff:ignore[type-check-without-type-error] — structural error, not a type error
    return [_parse_record(entry) for entry in deployments]


def _overlay_path_from_config() -> Path | None:
    """Read the ``[deployments] overlay`` path from the operator file, if set.

    ``[deployments]`` is free-form: the typed schema does not declare it, so it
    is read as a raw table from the file the loader selected — the same file
    the typed load read, not a re-derived path.

    Absent means none: no file layer, no ``[deployments]`` section, or no
    ``overlay`` key all return ``None`` quietly, because an operator who did
    not write an overlay is not making a mistake. A file that exists but
    cannot serve the read — unreadable, unparseable, a non-table
    ``[deployments]``, or a non-string/empty ``overlay`` — raises instead:
    the bot must never silently drop the operator's overlay deployments.

    Returns:
        The overlay path (expanded + absolute), or ``None`` when the process
        has no file layer or the section/key is absent.

    Raises:
        DegenbotValueError: When the selected config file exists but cannot
            be read as ``[deployments] overlay``, naming the file and the
            problem.

    """
    selected = config_file_path()
    if selected is None:
        return None
    path = Path(selected)
    try:
        with path.open("rb") as fh:
            data = tomllib.load(fh)
    except OSError as exc:
        msg = (
            f"Operator config file {path} could not be read to resolve [deployments] overlay: {exc}"
        )
        raise DegenbotValueError(message=msg) from exc
    except tomllib.TOMLDecodeError as exc:
        msg = (
            f"Operator config file {path} is not valid TOML (needed to read "
            f"[deployments] overlay): {exc}"
        )
        raise DegenbotValueError(message=msg) from exc
    section = data.get("deployments")
    if section is None:
        return None
    if not isinstance(section, dict):
        msg = (
            f"Operator config file {path}: [deployments] must be a table, "
            f"got {type(section).__name__}"
        )
        raise DegenbotValueError(message=msg)
    overlay = section.get("overlay")
    if overlay is None:
        return None
    if not isinstance(overlay, str) or not overlay:
        msg = (
            f"Operator config file {path}: [deployments] overlay must be a "
            f"non-empty string, got {overlay!r}"
        )
        raise DegenbotValueError(message=msg)
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
