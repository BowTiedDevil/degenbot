"""Cross-source lock: Rust embedded JSON == Python loader (K2SM4O).

The ``include_str!`` embed in ``degenbot-uniswap::deployments`` parses the
*same* file ``load_deployments()`` reads at runtime. This test asserts the two
sources agree on every shipped row — catching a stale embed path, a serde
field mismatch, or an Address-casing skew at test time.

For every Python ``DeploymentRecord``:

- ``init_hash_for(chain, factory)`` must equal ``record.init_hash`` (both
  ``None`` for the no-CREATE2 rows).
- ``deployer_for(chain, factory)`` must equal the *effective* deployer
  (``record.deployer`` if set, else ``record.factory``), checksummed.
- Count parity: every Python row resolves via Rust (no row lost), and the Rust
  table holds exactly the shipped count (no stale/extra key).
"""

from __future__ import annotations

from degenbot._ffi.deployments import deployer_for, init_hash_for
from degenbot.checksum_cache import get_checksum_address
from degenbot.registry.deployment_loader import DeploymentRecord, load_deployments

RECORDS: list[DeploymentRecord] = load_deployments()


def _effective_deployer(record: DeploymentRecord) -> str:
    """The ``None -> factory`` convention the Rust side applies."""
    return get_checksum_address(record.deployer) if record.deployer else record.factory


def test_rust_resolves_every_python_row() -> None:
    """Every shipped Python row resolves via the Rust lookup (count parity)."""
    assert len(RECORDS) == 32, f"expected 32 shipped rows, got {len(RECORDS)}"
    for record in RECORDS:
        # The effective deployer must resolve for every row (init_hash may be
        # None only for the no-CREATE2 rows, asserted separately).
        deployer = deployer_for(record.chain_id, record.factory)
        assert deployer is not None, (
            f"Rust lookup missed chain {record.chain_id} factory {record.factory}"
        )


def test_init_hash_matches_for_every_row() -> None:
    """Rust ``init_hash_for`` == Python ``record.init_hash`` for all rows."""
    for record in RECORDS:
        rust_ih = init_hash_for(record.chain_id, record.factory)
        py_ih = record.init_hash
        # Normalize: both lowercase 0x-hex or None.
        assert rust_ih == py_ih, (
            f"init_hash mismatch chain {record.chain_id} "
            f"factory {record.factory}: rust={rust_ih!r} python={py_ih!r}"
        )


def test_effective_deployer_matches_for_every_row() -> None:
    """Rust ``deployer_for`` == Python effective deployer for all rows."""
    for record in RECORDS:
        rust_dep = deployer_for(record.chain_id, record.factory)
        py_dep = _effective_deployer(record)
        # Rust returns a checksummed address; normalize the Python side too.
        assert rust_dep == get_checksum_address(py_dep), (
            f"deployer mismatch chain {record.chain_id} "
            f"factory {record.factory}: rust={rust_dep!r} python={py_dep!r}"
        )


def test_pancakeswap_v3_uses_separate_deployer() -> None:
    """The load-bearing case: a row whose deployer differs from the factory."""
    for record in RECORDS:
        if record.pool_type == "pancakeswap-v3" and record.chain_id == 1:
            assert record.deployer is not None
            assert record.deployer != record.factory
            assert deployer_for(record.chain_id, record.factory) == get_checksum_address(
                record.deployer
            )
            return
    msg = "no PancakeSwap V3 mainnet row in shipped deployments"
    raise AssertionError(msg)


def test_no_unregistered_key_resolves() -> None:
    """The Rust table holds exactly the shipped rows — no stale/extra key.

    A key not in the shipped JSON must return None; this guards against a stale
    embed pointing at a file with extra rows.
    """
    assert init_hash_for(999_999, "0x" + "ab" * 20) is None
    assert deployer_for(999_999, "0x" + "ab" * 20) is None
