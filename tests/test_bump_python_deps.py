"""Tests for the PyPI requirement bumper behind `just update-deps`.

The bumper's whole job is a text rewrite of ``pyproject.toml`` requirement
clauses against the newest stable version it finds on PyPI, so every test
drives ``rewrite_requirement`` directly: no network, no file writes. What is
under test is which clauses are the bumper's to rewrite — a prerelease version
is an operator's deliberate choice, and no stable release may replace it.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from tests.repo_script import load_script

ROOT = Path(__file__).resolve().parents[1]
bump = load_script("bump_python_deps", ROOT / "scripts" / "bump_python_deps.py")

#: The newest stable release the bumper would pick, older than every
#: prerelease pin below — so an unguarded rewrite of one is a downgrade.
LATEST_STABLE = "0.4.3"

#: Pins whose prerelease version is the operator's choice, in the clause shapes
#: the bumper knows how to rewrite (exact, floor, compatible, compound, marked).
PRERELEASE_PINS = [
    "vyper == 0.5.0b2",
    "vyper >= 0.5.0b2",
    "vyper ~= 0.5.0b2",
    "vyper >= 0.4.3, == 0.5.0b2",
    'vyper >= 0.5.0b2; python_version >= "3.12"',
]


@pytest.mark.parametrize("requirement", PRERELEASE_PINS)
def test_prerelease_pin_survives_the_bump(requirement: str) -> None:
    """A prerelease version in a clause is deliberate: keep the requirement verbatim."""
    rewritten, note = bump.rewrite_requirement(requirement, LATEST_STABLE)

    assert rewritten == requirement
    assert "prerelease" in note


@pytest.mark.parametrize(
    ("requirement", "expected"),
    [
        ("mypy == 1.18.0", "mypy == 1.19.0"),
        ("hypothesis >= 6.100.0", "hypothesis >= 6.140.2"),
        ("pydantic ~= 2.13", "pydantic ~= 2.14"),
        ("tomlkit == 0.13.*", "tomlkit == 0.13.*"),
    ],
)
def test_stable_requirements_still_bump(requirement: str, expected: str) -> None:
    """The prerelease guard must not freeze the packages it is not about."""
    latest = {
        "mypy": "1.19.0",
        "hypothesis": "6.140.2",
        "pydantic": "2.14.1",
        "tomlkit": "0.13.2",
    }
    name = bump.requirement_parts(requirement)[0]

    rewritten, _note = bump.rewrite_requirement(requirement, latest[name])

    assert rewritten == expected


def test_stable_pin_equal_to_latest_is_unchanged() -> None:
    """No clause in a requirement already at the latest stable is rewritten."""
    requirement = "pytest-timeout == 2.4.0"

    rewritten, _note = bump.rewrite_requirement(requirement, "2.4.0")

    assert rewritten == requirement
