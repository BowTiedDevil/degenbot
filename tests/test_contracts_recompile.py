"""Tests for the executor runtime bake's owner and destination selection.

``contracts/recompile.py`` is the one path from the committed tier-3 artifacts to
the runtime the bot injects, so its destination rule is what decides which sender
a baked runtime admits. Every byte assertion below comes from a real bake: the
script runs into a temp directory and its output is compared with the tracked
release artifact. No test re-implements the immutable layout.
"""

from __future__ import annotations

import hashlib
import importlib.util
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
RECOMPILE = ROOT / "contracts" / "recompile.py"
RELEASE_DEPLOYMENT = ROOT / "rust/crates/foundation/degenbot-executor/releases/v2/deployment"
TRACKED_RUNTIME = RELEASE_DEPLOYMENT / "cmd_executor_runtime_bytecode.txt"
TRACKED_CREATION = RELEASE_DEPLOYMENT / "cmd_executor_bytecode.txt"
TRACKED_ABI = RELEASE_DEPLOYMENT / "cmd_executor_abi.json"

RUNTIME_FILE = "cmd_executor_runtime_bytecode.txt"
CREATION_FILE = "cmd_executor_bytecode.txt"
ABI_FILE = "cmd_executor_abi.json"

#: The owner the tracked release runtime bakes in.
RELEASE_OWNER = "0x9C56a29c7231974c269E24F9FB3c29203039089E"

#: A dry-run session's own signer: an owner the release artifact does not admit,
#: so its runtime is a probe artifact that must never become the release one.
SESSION_SIGNER = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"

#: ``cmd_executor/2``'s immutable tail: seven 32-byte words after the CBOR.
TAIL_HEX = 7 * 32 * 2
WORD_HEX = 32 * 2


def _load(name: str, path: Path):
    """Load one repository script as a module, without running its entry point."""
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


bankroll = _load("executor_bankroll_matrix", ROOT / "scripts" / "executor_bankroll_matrix.py")
recompile = _load("contracts_recompile", RECOMPILE)


def _run(*args: str) -> subprocess.CompletedProcess[str]:
    """Run the real bake script with the given arguments."""
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [sys.executable, str(RECOMPILE), *args],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
        timeout=180,
    )


def _body(path: Path) -> str:
    """One artifact as bare lowercase hex (no ``0x``, no whitespace)."""
    body = "".join(path.read_text(encoding="utf-8").split())
    assert body.startswith("0x")
    return body[2:].lower()


def _tail(body: str) -> str:
    """The immutable tail of one runtime body, 448 hex digits."""
    return body[-TAIL_HEX:]


def _code(body: str) -> str:
    """The code section plus CBOR metadata of one runtime body."""
    return body[:-TAIL_HEX]


def _owner_word(owner: str) -> str:
    """One address immutable word: twelve zero bytes, then the owner's twenty."""
    return "00" * 12 + owner[2:].lower()


def test_a_default_bake_into_an_explicit_directory_reproduces_the_tracked_deployment(
    tmp_path: Path,
) -> None:
    """The default owner and the release destination stay one artifact."""
    result = _run("--output-dir", str(tmp_path))

    assert result.returncode == 0, result.stderr
    for name, tracked in (
        (RUNTIME_FILE, TRACKED_RUNTIME),
        (CREATION_FILE, TRACKED_CREATION),
        (ABI_FILE, TRACKED_ABI),
    ):
        assert (tmp_path / name).read_bytes() == tracked.read_bytes(), name


def test_a_custom_owner_changes_immutable_word_zero_alone(tmp_path: Path) -> None:
    """A probe bake keeps the accepted code and six words, and rebinds the owner."""
    result = _run("--owner", SESSION_SIGNER, "--output-dir", str(tmp_path))

    assert result.returncode == 0, result.stderr
    baked = _body(tmp_path / RUNTIME_FILE)
    tracked = _body(TRACKED_RUNTIME)
    assert len(baked) == len(tracked), "the bake substitutes a word, never resizes"
    assert _code(baked) == _code(tracked), "code section and CBOR are the accepted bytes"
    assert _tail(baked)[:WORD_HEX] == _owner_word(SESSION_SIGNER)
    assert _tail(tracked)[:WORD_HEX] == _owner_word(RELEASE_OWNER)
    assert _tail(baked)[WORD_HEX:] == _tail(tracked)[WORD_HEX:], "the other six words"
    assert (tmp_path / CREATION_FILE).read_bytes() == TRACKED_CREATION.read_bytes()
    assert (tmp_path / ABI_FILE).read_bytes() == TRACKED_ABI.read_bytes()
    assert SESSION_SIGNER[2:].lower() in result.stdout, "the report names the owner baked"


def test_the_matrix_runner_reads_the_probe_runtime_owner(tmp_path: Path) -> None:
    """The matrix's own parser accepts a probe bake as a cmd_executor/2 runtime."""
    result = _run("--owner", SESSION_SIGNER, "--output-dir", str(tmp_path))

    assert result.returncode == 0, result.stderr
    assert (
        bankroll.runtime_owner(tmp_path / RUNTIME_FILE, release_id="cmd_executor/2")
        == SESSION_SIGNER.lower()
    )


def test_a_custom_owner_without_a_destination_is_refused(tmp_path: Path) -> None:
    """An unnamed destination is the release deployment, so the bake refuses it."""
    before = hashlib.sha256(TRACKED_RUNTIME.read_bytes()).hexdigest()

    result = _run("--owner", SESSION_SIGNER)

    assert result.returncode != 0
    assert "needs an explicit --output-dir" in result.stderr
    assert hashlib.sha256(TRACKED_RUNTIME.read_bytes()).hexdigest() == before
    assert list(tmp_path.iterdir()) == []
    assert (
        bankroll.runtime_owner(TRACKED_RUNTIME, release_id="cmd_executor/2")
        == RELEASE_OWNER.lower()
    )


@pytest.mark.parametrize(
    ("args", "refusal"),
    [
        (("--owner", SESSION_SIGNER), "is not this release's owner"),
        (("--owner", "0x1234"), "is not an address"),
        (("--owner", SESSION_SIGNER[2:]), "is not an address"),
        (("--owner", "0x" + "zz" * 20), "is not an address"),
        (("--bogus",), "unrecognized arguments"),
    ],
)
def test_an_unusable_command_is_refused_with_the_release_runtime_untouched(
    args: tuple[str, ...],
    refusal: str,
) -> None:
    """Whatever the argument, a refused bake writes nothing at all."""
    before = hashlib.sha256(TRACKED_RUNTIME.read_bytes()).hexdigest()

    result = _run(*args)

    assert result.returncode != 0
    assert refusal in result.stderr
    assert hashlib.sha256(TRACKED_RUNTIME.read_bytes()).hexdigest() == before


def test_the_release_deployment_is_refused_as_a_probe_destination() -> None:
    """A probe destination that resolves to the release deployment never bakes."""
    args = recompile._parser().parse_args(
        ["--owner", SESSION_SIGNER, "--output-dir", str(RELEASE_DEPLOYMENT)],
    )

    with pytest.raises(SystemExit) as refusal:
        recompile._resolve_request(args)

    assert refusal.value.code != 0


def test_a_default_resolution_selects_the_release_owner_and_deployment() -> None:
    """No arguments must keep the release owner and the tracked destination."""
    request = recompile._resolve_request(recompile._parser().parse_args([]))

    assert request.owner.lower() == RELEASE_OWNER.lower()
    assert request.output_dir == RELEASE_DEPLOYMENT
