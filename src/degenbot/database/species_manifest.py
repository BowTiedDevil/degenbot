"""Typed Python reader for the Rust-owned species manifest (ADR-059 D3).

``rust/crates/foundation/degenbot-db/src/species.toml`` is the single source of fork /
manager-deployment identity: one row per species carries the DB ``kind``
discriminator, subclass ``table``, family, fee denominator, V3 storage layout,
and the per-chain factory / CREATE2 init-codehash (or V4 manager). The Rust
core parses + validates that file once per process (``species.rs``); this
module is the Python half of the same read so the driver's enumerations — the
``build_paths`` version-tag expansion and the model parity gate — cannot drift
from the file.

The SQLAlchemy classes themselves stay handwritten (a class is not data); what
rides the manifest here is their *identity*: :func:`model_subclass_tables`,
:func:`model_pool_kinds`, :func:`pool_version_map`, and
:func:`assert_manifest_model_parity` all project the manifest + the model
registry rather than a second hand-listed enumeration.

Validation mirrors the Rust invariants (``species.rs``): unknown keys are
rejected (``deny_unknown_fields`` has no ``tomllib`` equivalent), every
``kind`` is unique and non-empty, V4 names the ``managed`` table, non-V4 names
a table that a real V2/V3 subclass declares, V3 carries ``slot_layout`` +
``fee_denominator``, ``stable`` is V2-only, every fee denominator is positive,
and each chain carries exactly the identifiers its family uses.
"""

from __future__ import annotations

import tomllib
from dataclasses import dataclass
from enum import StrEnum
from functools import cache
from pathlib import Path
from typing import TYPE_CHECKING

from degenbot.database.models.pools import (
    LFJPoolTable,
    UniswapV2PoolTableBase,
    UniswapV3PoolTableBase,
    UniswapV4PoolTable,
)

if TYPE_CHECKING:
    from collections.abc import Collection, Mapping

#: The ``table`` sentinel a V4 species carries. V4 pools join the managed
#: polymorphic base, not a V2/V3 subclass table.
V4_MANAGED_TABLE = "managed"

#: The subclass table the LFJ binned family names.
LFJ_POOLS = "lfj_pools"

#: Persisted pool kinds whose family the taxonomy declares but no tier
#: supports yet (ADR-059 D8). Mirrors the Rust graph vocabulary's
#: ``DECLARED_UNSUPPORTED_KINDS``: the ``LFJPoolTable`` model exists so a
#: manifest LFJ row resolves, but the shipped manifest declares no LFJ species
#: (no deployment addresses are invented here).
DECLARED_UNSUPPORTED_KINDS: frozenset[str] = frozenset({"lfj_binned"})

#: The repo-relative location of the Rust-owned manifest.
_MANIFEST_RELATIVE = Path("rust") / "crates" / "foundation" / "degenbot-db" / "src" / "species.toml"

_MANIFEST_KEYS = frozenset({"species"})
_SPECIES_KEYS = frozenset(
    {
        "kind",
        "family",
        "table",
        "fee_denominator",
        "slot_layout",
        "stable",
        "chains",
    },
)
_CHAIN_KEYS = frozenset({"chain_id", "factory", "init_codehash", "manager"})


class Family(StrEnum):
    """The pool family a species belongs to.

    V2/V3/V4 name graph-vocabulary families; LFJ names the declared-but-
    unsupported binned-liquidity family (ADR-059 E3), which has no graph pool
    kind.
    """

    V2 = "v2"
    V3 = "v3"
    V4 = "v4"
    LFJ = "lfj"


class SlotLayout(StrEnum):
    """The EVM storage layout a V3 species uses."""

    UNISWAP_V3 = "uniswap_v3"
    PANCAKE_V3 = "pancake_v3"


class ManifestError(ValueError):
    """A malformed or internally inconsistent species manifest."""


class SpeciesModelMismatchError(ValueError):
    """The species manifest and the SQLAlchemy pool models disagree."""


@dataclass(frozen=True, slots=True)
class ChainIdentifiers:
    """The identifiers a species has on one chain."""

    chain_id: int
    factory: str | None = None
    init_codehash: str | None = None
    manager: str | None = None


@dataclass(frozen=True, slots=True)
class Species:
    """A validated species row."""

    kind: str
    family: Family
    table: str
    fee_denominator: int | None
    slot_layout: SlotLayout | None
    stable: bool
    chains: Mapping[int, ChainIdentifiers]


@dataclass(frozen=True, slots=True)
class Manifest:
    """The validated species manifest, in manifest order."""

    species: tuple[Species, ...]

    def get(self, kind: str) -> Species | None:
        """Return the species with ``kind``, or None.

        Returns:
            The matching species, or None.

        """
        for species in self.species:
            if species.kind == kind:
                return species
        return None

    def species_of(self, family: Family) -> tuple[Species, ...]:
        """Return the species of one family, in manifest order.

        Returns:
            The matching species, in manifest order.

        """
        return tuple(s for s in self.species if s.family is family)

    def subclass_table_for_kind(self, kind: str) -> str | None:
        """Return the V2/V3 subclass table for ``kind``, or None.

        None for a V4 kind (no V2/V3 subclass table) or an unknown kind.

        Returns:
            The subclass table name, or None.

        """
        species = self.get(kind)
        if species is None or species.family is Family.V4:
            return None
        return species.table


# ──────────────────────────────────────────────────────────────────
# Parsing + validation
# ──────────────────────────────────────────────────────────────────


def _expect_str(value: object, field: str) -> str:
    if not isinstance(value, str):
        msg = f"species manifest field {field!r} must be a string"
        raise ManifestError(msg)
    return value


def _check_hex(value: str, digits: int, *, kind: str, chain_id: int, field: str) -> None:
    valid = len(value) == digits + 2 and value.startswith("0x")
    if valid:
        try:
            int(value, 16)
        except ValueError:
            valid = False
    if not valid:
        msg = f"species {kind!r} chain {chain_id} has invalid {field} {value!r}"
        raise ManifestError(msg)


def _parse_chains(  # ruff:ignore[too-many-branches]
    kind: str,
    family: Family,
    entries: object,
) -> dict[int, ChainIdentifiers]:
    if not isinstance(entries, list):
        msg = f"species {kind!r} chains must be an array"
        raise ManifestError(msg)
    if not entries:
        msg = f"species {kind!r} declares no chain entries"
        raise ManifestError(msg)
    chains: dict[int, ChainIdentifiers] = {}
    for entry in entries:
        if not isinstance(entry, dict):
            msg = f"species {kind!r} chain entry must be a TOML table"
            raise ManifestError(msg)
        unknown = set(entry) - _CHAIN_KEYS
        if unknown:
            msg = f"species {kind!r} chain entry has unknown key(s): {sorted(unknown)}"
            raise ManifestError(msg)
        chain_id = entry.get("chain_id")
        if isinstance(chain_id, bool) or not isinstance(chain_id, int):
            msg = f"species {kind!r} chain entry is missing an integer chain_id"
            raise ManifestError(msg)
        if chain_id in chains:
            msg = f"species {kind!r} declares chain {chain_id} more than once"
            raise ManifestError(msg)

        raw_factory = entry.get("factory")
        raw_hash = entry.get("init_codehash")
        raw_manager = entry.get("manager")
        factory = None if raw_factory is None else _expect_str(raw_factory, "factory")
        init_codehash = None if raw_hash is None else _expect_str(raw_hash, "init_codehash")
        manager = None if raw_manager is None else _expect_str(raw_manager, "manager")
        if factory is not None:
            _check_hex(factory, 40, kind=kind, chain_id=chain_id, field="factory")
        if init_codehash is not None:
            _check_hex(init_codehash, 64, kind=kind, chain_id=chain_id, field="init_codehash")
        if manager is not None:
            _check_hex(manager, 40, kind=kind, chain_id=chain_id, field="manager")

        if family is Family.V4:
            if factory is not None or init_codehash is not None:
                msg = f"V4 species {kind!r} chain {chain_id} carries a CREATE2 identifier"
                raise ManifestError(msg)
            if manager is None:
                msg = f"species {kind!r} chain {chain_id} is missing its manager"
                raise ManifestError(msg)
        else:
            if manager is not None:
                msg = f"non-V4 species {kind!r} chain {chain_id} carries a V4 manager address"
                raise ManifestError(msg)
            if factory is None:
                msg = f"species {kind!r} chain {chain_id} is missing its factory"
                raise ManifestError(msg)

        chains[chain_id] = ChainIdentifiers(
            chain_id=chain_id,
            factory=factory,
            init_codehash=init_codehash,
            manager=manager,
        )
    return chains


def _parse_species(  # ruff:ignore[too-many-branches, too-many-statements]
    entry: object,
    allowed_tables: Collection[str],
) -> Species:
    if not isinstance(entry, dict):
        msg = "every species entry must be a TOML table"
        raise ManifestError(msg)
    unknown = set(entry) - _SPECIES_KEYS
    if unknown:
        msg = f"species entry has unknown key(s): {sorted(unknown)}"
        raise ManifestError(msg)

    kind = entry.get("kind")
    if not isinstance(kind, str) or not kind:
        msg = "species manifest declares an empty or non-string kind"
        raise ManifestError(msg)

    family_raw = entry.get("family")
    if not isinstance(family_raw, str):
        msg = f"species {kind!r} is missing a string family"
        raise ManifestError(msg)
    try:
        family = Family(family_raw)
    except ValueError:
        msg = f"species {kind!r} has unknown family {family_raw!r}"
        raise ManifestError(msg) from None

    table = entry.get("table")
    if not isinstance(table, str) or not table:
        msg = f"species {kind!r} is missing a non-empty table"
        raise ManifestError(msg)
    if family is Family.LFJ:
        if table != LFJ_POOLS:
            msg = f"LFJ species {kind!r} must name table {LFJ_POOLS!r}, got {table!r}"
            raise ManifestError(msg)
    elif family is Family.V4:
        if table != V4_MANAGED_TABLE:
            msg = f"V4 species {kind!r} must name table {V4_MANAGED_TABLE!r}, got {table!r}"
            raise ManifestError(msg)
    elif table not in allowed_tables:
        msg = f"species {kind!r} names unknown subclass table {table!r}"
        raise ManifestError(msg)

    raw_fee = entry.get("fee_denominator")
    if raw_fee is not None and (isinstance(raw_fee, bool) or not isinstance(raw_fee, int)):
        msg = f"species {kind!r} fee_denominator must be an integer"
        raise ManifestError(msg)
    fee_denominator: int | None = raw_fee

    raw_layout = entry.get("slot_layout")
    slot_layout: SlotLayout | None = None
    if raw_layout is not None:
        if not isinstance(raw_layout, str):
            msg = f"species {kind!r} slot_layout must be a string"
            raise ManifestError(msg)
        try:
            slot_layout = SlotLayout(raw_layout)
        except ValueError:
            msg = f"species {kind!r} has unknown slot_layout {raw_layout!r}"
            raise ManifestError(msg) from None

    if family is Family.V3:
        if slot_layout is None:
            msg = f"V3 species {kind!r} is missing its slot_layout"
            raise ManifestError(msg)
        if fee_denominator is None:
            msg = f"V3 species {kind!r} is missing its fee_denominator"
            raise ManifestError(msg)
    elif slot_layout is not None:
        msg = f"non-V3 species {kind!r} declares a slot_layout"
        raise ManifestError(msg)

    stable = entry.get("stable", False)
    if not isinstance(stable, bool):
        msg = f"species {kind!r} stable must be a boolean"
        raise ManifestError(msg)
    if stable and family is not Family.V2:
        msg = f"non-V2 species {kind!r} declares stable"
        raise ManifestError(msg)
    if fee_denominator is not None and fee_denominator <= 0:
        msg = f"species {kind!r} declares a non-positive fee_denominator"
        raise ManifestError(msg)

    chains = _parse_chains(kind, family, entry.get("chains", []))
    return Species(
        kind=kind,
        family=family,
        table=table,
        fee_denominator=fee_denominator,
        slot_layout=slot_layout,
        stable=stable,
        chains=chains,
    )


def parse_manifest(
    toml_str: str,
    *,
    allowed_tables: Collection[str] | None = None,
) -> Manifest:
    """Parse + validate a species manifest string.

    Args:
        toml_str: The TOML manifest body.
        allowed_tables: The V2/V3 subclass tables a non-V4 species may name.
            Defaults to the tables the SQLAlchemy V2/V3 model classes declare
            (:func:`model_subclass_tables`), mirroring the Rust
            ``V2_V3_SUBCLASS_TABLES`` check.

    Returns:
        The validated manifest, in manifest order.

    Raises:
        ManifestError: The manifest is invalid TOML or violates an invariant.

    """
    try:
        raw = tomllib.loads(toml_str)
    except tomllib.TOMLDecodeError as exc:
        msg = f"species manifest is not valid TOML: {exc}"
        raise ManifestError(msg) from exc
    if not isinstance(raw, dict):
        msg = "species manifest must be a TOML table"
        raise ManifestError(msg)
    unknown = set(raw) - _MANIFEST_KEYS
    if unknown:
        msg = f"species manifest has unknown key(s): {sorted(unknown)}"
        raise ManifestError(msg)
    entries = raw.get("species")
    if not isinstance(entries, list):
        msg = "species manifest must declare a 'species' array"
        raise ManifestError(msg)
    if not entries:
        msg = "species manifest declares no species"
        raise ManifestError(msg)
    if allowed_tables is None:
        allowed_tables = model_subclass_tables()

    species: list[Species] = []
    seen: set[str] = set()
    for entry in entries:
        parsed = _parse_species(entry, allowed_tables)
        if parsed.kind in seen:
            msg = f"species kind {parsed.kind!r} is declared more than once"
            raise ManifestError(msg)
        seen.add(parsed.kind)
        species.append(parsed)
    return Manifest(species=tuple(species))


def shipped_manifest_path() -> Path:
    """Locate the Rust-owned ``species.toml`` from the package or the cwd.

    Walks up from this module towards the repo root looking for the manifest,
    falling back to the current working directory (the gate runs from the repo
    root). Mirrors ``build_info.read_receipt``'s checkout-root resolution.

    Returns:
        The absolute path to the Rust species manifest.

    Raises:
        FileNotFoundError: The manifest cannot be located.

    """
    resolved = Path(__file__).resolve()
    for parent in resolved.parents:
        candidate = parent / _MANIFEST_RELATIVE
        if candidate.is_file():
            return candidate
    cwd_candidate = Path.cwd() / _MANIFEST_RELATIVE
    if cwd_candidate.is_file():
        return cwd_candidate
    msg = f"cannot locate species manifest {_MANIFEST_RELATIVE} from {resolved} or {Path.cwd()}"
    raise FileNotFoundError(msg)


@cache
def manifest() -> Manifest:
    """Return the validated shipped manifest, parsed once per process.

    Returns:
        The cached, validated manifest.

    """
    return parse_manifest(shipped_manifest_path().read_text(encoding="utf-8"))


def reset_manifest_cache() -> None:
    """Drop the cached manifest (tests that swap the shipped file)."""
    manifest.cache_clear()


# ──────────────────────────────────────────────────────────────────
# Model parity + build_paths expansion
# ──────────────────────────────────────────────────────────────────


def concrete_pool_types(base_type: type) -> list[type]:
    """Expand an abstract pool table base into its concrete subclasses.

    Returns:
        The concrete subclasses of ``base_type``, or ``[base_type]`` when it
        is already concrete / has no subclasses.

    """
    if not getattr(base_type, "__abstract__", False):
        return [base_type]
    subs = base_type.__subclasses__()
    if not subs:
        return [base_type]
    result: list[type] = []
    for sub in subs:
        result.extend(concrete_pool_types(sub))
    return result


def _v2_v3_models() -> list[type]:
    models: list[type] = []
    for base in (UniswapV2PoolTableBase, UniswapV3PoolTableBase):
        models.extend(concrete_pool_types(base))
    return models


def model_subclass_tables() -> frozenset[str]:
    """Return the V2/V3 subclass table names the SQLAlchemy models declare.

    Returns:
        The set of V2/V3 subclass ``__tablename__`` values.

    """
    return frozenset(
        model.__tablename__  # ty: ignore[unresolved-attribute]
        for model in _v2_v3_models()
    )


def _model_index() -> tuple[dict[str, type], dict[str, type]]:
    """Index the concrete pool models by kind identity and by table.

    Returns:
        ``(by_kind, by_table)`` — polymorphic identity → class and
        ``__tablename__`` → class. The V4 model is reachable under both its
        ``uniswap_v4`` identity and the manifest's ``managed`` table sentinel.

    """
    by_kind: dict[str, type] = {}
    by_table: dict[str, type] = {}
    for model in _v2_v3_models():
        identity = model.__mapper__.polymorphic_identity  # ty: ignore[unresolved-attribute]
        by_kind[identity] = model
        by_table[model.__tablename__] = model  # ty: ignore[unresolved-attribute]
    v4_identity = UniswapV4PoolTable.__mapper__.polymorphic_identity
    if isinstance(v4_identity, str):
        by_kind[v4_identity] = UniswapV4PoolTable
    by_table[V4_MANAGED_TABLE] = UniswapV4PoolTable
    # The declared-but-unsupported LFJ family's model: resolvable so a
    # fixture manifest row passes parity, excluded from the shipped-kind set.
    lfj_identity = LFJPoolTable.__mapper__.polymorphic_identity
    if isinstance(lfj_identity, str):
        by_kind[lfj_identity] = LFJPoolTable
    by_table[LFJ_POOLS] = LFJPoolTable
    return by_kind, by_table


def model_pool_kinds() -> frozenset[str]:
    """Return the SUPPORTED pool-kind discriminators the models declare.

    Returns:
        The set of supported polymorphic identities (V2/V3 subclasses + V4).
        The declared-but-unsupported LFJ kind is excluded so the shipped
        manifest and the shipped model set stay 1:1.

    """
    by_kind, _ = _model_index()
    return frozenset(by_kind) - DECLARED_UNSUPPORTED_KINDS


def _missing_model_message(species: Species) -> str:
    return (
        f"manifest species {species.kind!r} has no SQLAlchemy pool model "
        f"with polymorphic identity {species.kind!r}"
    )


def _table_mismatch_message(species: Species, model: type) -> str:
    table = model.__tablename__  # ty: ignore[unresolved-attribute]
    return (
        f"manifest species {species.kind!r} names table {species.table!r}, "
        f"but model {model.__name__} declares {table!r}"
    )


def pool_version_map(manifest_obj: Manifest) -> dict[str, list[type]]:
    """Expand the version tags to their concrete pool table classes.

    The ``V2``/``V3``/``V4`` tags are the public API surface; the species under
    each tag come from the manifest, so adding a species needs a manifest row
    (and a model class), not an edit to a second Python enumeration.

    Args:
        manifest_obj: The manifest whose species drive the expansion.

    Returns:
        ``{"V2": [...], "V3": [...], "V4": [...]}`` in manifest order.

    Raises:
        SpeciesModelMismatchError: A manifest species has no model class, or
            its table disagrees with the model's ``__tablename__``.

    """
    by_kind, _ = _model_index()
    families: dict[str, Family] = {"V2": Family.V2, "V3": Family.V3, "V4": Family.V4}
    result: dict[str, list[type]] = {}
    for tag, family in families.items():
        classes: list[type] = []
        for species in manifest_obj.species_of(family):
            model = by_kind.get(species.kind)
            if model is None:
                raise SpeciesModelMismatchError(_missing_model_message(species))
            if (
                species.family is not Family.V4 and model.__tablename__ != species.table  # ty: ignore[unresolved-attribute]
            ):
                raise SpeciesModelMismatchError(_table_mismatch_message(species, model))
            classes.append(model)
        result[tag] = classes
    return result


def assert_manifest_model_parity(manifest_obj: Manifest) -> None:
    """Fail unless manifest species ↔ model kinds ↔ subclass tables are 1:1.

    The drift gate: a species declared in the manifest without a matching
    SQLAlchemy model (or a model without a manifest row) refuses here, so the
    two enumerations cannot silently diverge.

    Args:
        manifest_obj: The manifest to check.

    Raises:
        SpeciesModelMismatchError: The model and manifest enumerations differ.

    """
    by_kind, _ = _model_index()
    manifest_kinds = {species.kind for species in manifest_obj.species}
    missing = sorted(manifest_kinds - set(by_kind))
    extra = sorted(set(by_kind) - manifest_kinds - DECLARED_UNSUPPORTED_KINDS)
    if missing or extra:
        parts: list[str] = []
        if missing:
            parts.append(f"manifest species without a model: {missing}")
        if extra:
            parts.append(f"model kinds absent from the manifest: {extra}")
        msg = "manifest/model pool-kind parity broken — " + "; ".join(parts)
        raise SpeciesModelMismatchError(msg)
    for species in manifest_obj.species:
        model = by_kind[species.kind]
        if (
            species.family is not Family.V4 and model.__tablename__ != species.table  # ty: ignore[unresolved-attribute]
        ):
            raise SpeciesModelMismatchError(_table_mismatch_message(species, model))
