"""The published-key refusal list has one declared home.

A private key the repository publishes must never sign a live transaction.
Which keys those are is a property of the repository — the dry-run throwaway
it chose plus the all-zero scalar it once used — not of the config schema or
the engine. So one declared manifest holds the classification, and both
consumers read it: the Python driver parses it at runtime, the Rust parity
example embeds it at compile time.
"""

from __future__ import annotations

import re
from pathlib import Path

from degenbot.runner import identity as identity_module

_REPO_ROOT = Path(__file__).resolve().parents[2]
_MANIFEST = _REPO_ROOT / "src/degenbot/runner/published_operator_private_keys.txt"
_RUST_EXAMPLE = _REPO_ROOT / "rust/examples/settlement_bot/src/main.rs"

_DRY_RUN_KEY = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
_ALL_ZERO_KEY = "0x" + "0" * 64


def _manifest_keys() -> set[str]:
    """The manifest's keys, lowercased, with comments and blank lines dropped."""
    return {
        stripped.lower()
        for raw in _MANIFEST.read_text(encoding="utf-8").splitlines()
        if (stripped := raw.strip()) and not stripped.startswith("#")
    }


def test_manifest_declares_the_published_keys() -> None:
    assert _manifest_keys() == {_ALL_ZERO_KEY, _DRY_RUN_KEY}


def test_python_refusal_list_is_the_manifest() -> None:
    assert identity_module._PLACEHOLDER_OPERATOR_PRIVATE_KEYS == _manifest_keys()


def test_rust_example_embeds_the_manifest_instead_of_a_second_list() -> None:
    source = _RUST_EXAMPLE.read_text(encoding="utf-8")
    assert re.search(
        r'include_str!\(\s*"\.\./\.\./\.\./\.\./src/degenbot/runner/'
        r'published_operator_private_keys\.txt"\s*\)',
        source,
    ), "the example must embed the Python driver's declared manifest"
    assert "PLACEHOLDER_OPERATOR_PRIVATE_KEYS: [&str; 2]" not in source, (
        "a hand-maintained key array in the Rust example is the second home this test forbids"
    )


def test_rust_example_cites_the_identity_home() -> None:
    """The example's stale ``_driver_constants`` citations name a deleted module."""
    source = _RUST_EXAMPLE.read_text(encoding="utf-8")
    assert "driver_constants" not in source, (
        "the retired _driver_constants module must not be cited; the identity "
        "home is src/degenbot/runner/identity.py"
    )
    assert "runner/identity.py" in source, (
        "the example must cite the identity home it mirrors"
    )


def test_rust_example_mirrors_the_python_identity_literals() -> None:
    """The example's deployment defaults are a contingent parity mirror.

    The core must not own one deployment's executor/operator identity, so the
    example carries its own copy to reproduce the Python driver byte-for-byte.
    This test is the revisit trigger pinned at the Python side: if the example
    stops being a parity mirror, delete the mirror rather than let it drift.
    """
    source = _RUST_EXAMPLE.read_text(encoding="utf-8")
    mirrors = (
        ("DEFAULT_EXECUTOR_ADDRESS", identity_module._DEFAULT_EXECUTOR_ADDRESS),
        ("DEFAULT_INJECTED_ADDRESS", identity_module._DEFAULT_INJECTED_ADDRESS),
        ("DEFAULT_EXECUTOR_OWNER", identity_module._DEFAULT_EXECUTOR_OWNER),
        ("DRY_RUN_OPERATOR_ADDRESS", identity_module._DRY_RUN_OPERATOR_ADDRESS),
        ("DRY_RUN_OPERATOR_PRIVATE_KEY", identity_module._DRY_RUN_OPERATOR_PRIVATE_KEY),
        ("WETH_ADDRESS", identity_module.WETH_ADDRESS),
    )
    for name, value in mirrors:
        assert re.search(
            rf'{name}:\s*&str\s*=\s*"{re.escape(value)}"', source
        ), f"the example must mirror identity.{name} = {value}"
