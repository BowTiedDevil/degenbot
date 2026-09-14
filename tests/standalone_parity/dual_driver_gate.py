"""RSP-8 running dual-driver decision gate (ergo 23DLCY).

The executable successor to the parity ledger: it diffs the Python driver's
and the Rust driver's decision streams modulo a documented
permitted-divergence list.

Two modes:

* --recorded (default, CI-safe, offline): replays the checked-in recorded
  decision fixture (fixtures/dual_driver_decisions.json) and asserts the two
  drivers agree on every shared decision except the explicitly permitted
  divergences. This is what the pytest gate
  (test_settlement_bot_dual_driver_gate.py) runs.
* --live (gated behind DEGENBOT_DUAL_DRIVER_GATE=1 and DEGENBOT_FORK_RPC):
  starts anvil at a pinned fork block, runs the Python example and the Rust
  example in dry-run, and diffs the captured decision streams. Outbound RPC
  may be unavailable in CI, so this mode is a no-op unless explicitly
  enabled. The per-batch decision stream is read from the files named by
  DEGENBOT_DECISION_STREAM (one JSON object per line, schema
  {block, path_id, decision}); a driver that does not emit the stream is
  reported loudly rather than silently passing.

--record regenerates the recorded fixture from the offline probes (no RPC):
the Python consumer probe and the shared boot oracle (which the Rust gate
pins to the real offline binary output).

Invocation:
    uv run python tests/standalone_parity/dual_driver_gate.py --recorded
    DEGENBOT_DUAL_DRIVER_GATE=1 DEGENBOT_FORK_RPC=... \
        uv run python tests/standalone_parity/dual_driver_gate.py --live
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_FIXTURE_DIR = _HERE / "fixtures"
_DECISIONS_FIXTURE = _FIXTURE_DIR / "dual_driver_decisions.json"
_BOOT_ORACLE = _FIXTURE_DIR / "settlement_bot_boot.json"
_DB_PATH = _HERE.parent.parent / "rust/crates/degenbot-db/tests/fixtures/parity.db"

GATE_ENV = "DEGENBOT_DUAL_DRIVER_GATE"
FORK_ENV = "DEGENBOT_FORK_RPC"
BLOCK_ENV = "DEGENBOT_FORK_BLOCK"
STREAM_ENV = "DEGENBOT_DECISION_STREAM"
LIVE_TIMEOUT_ENV = "DEGENBOT_DUAL_DRIVER_TIMEOUT_SECS"
DISCOVERY_CHAIN_ID = 8453
_REPO = _HERE.parent.parent
_ANVIL = Path("/home/dev/.foundry/bin/anvil")


def gate_enabled() -> bool:
    """Whether the live dual-driver gate is explicitly opted into."""
    return os.environ.get(GATE_ENV, "") == "1"


def load_decisions_fixture() -> dict:
    """Load the recorded dual-driver decision fixture."""
    with _DECISIONS_FIXTURE.open() as handle:
        return json.load(handle)


def load_boot_oracle() -> dict:
    """Load the shared fixture-boot oracle."""
    with _BOOT_ORACLE.open() as handle:
        return json.load(handle)


def diff_decisions(
    python_decisions: list[dict],
    rust_decisions: list[dict],
    permitted_divergence: list[dict],
) -> list[str]:
    """Diff two decision streams, ignoring the permitted-divergence keys.

    Returns one human-readable message per unpermitted divergence (an empty
    list means the drivers agree).
    """
    permitted = {entry["key"] for entry in permitted_divergence}
    python_by_key = {entry["key"]: entry["value"] for entry in python_decisions}
    rust_by_key = {entry["key"]: entry["value"] for entry in rust_decisions}
    divergences: list[str] = []
    for key in sorted(set(python_by_key) | set(rust_by_key)):
        if key in permitted:
            continue
        if key not in python_by_key or key not in rust_by_key:
            divergences.append(
                f"{key}: python={python_by_key.get(key)!r} rust={rust_by_key.get(key)!r}"
            )
        elif python_by_key[key] != rust_by_key[key]:
            divergences.append(
                f"{key}: python={python_by_key[key]!r} rust={rust_by_key[key]!r}"
            )
    return divergences


def python_offline_decisions() -> list[dict]:
    """The Python consumer probe's decisions against the parity.db fixture."""
    from degenbot._ffi import Bot, build_path_graph

    bot = Bot(1)
    bot.load_snapshot_from_db(str(_DB_PATH), 1)
    graph = build_path_graph(
        database_path=str(_DB_PATH),
        chain_id=DISCOVERY_CHAIN_ID,
        pool_kinds={0, 1, 2},
        allowed_intermediate_token_ids=None,
    )
    return [
        {"key": "snapshot_seed_block", "value": bot.snapshot_seed_block},
        {"key": "discovery_count", "value": len(graph["pool_id_to_kind"])},
        {"key": "graph.nodes", "value": len(graph["pool_id_to_kind"])},
        {"key": "graph.candidate_tokens", "value": sorted(graph["candidate_tokens"])},
    ]


def rust_offline_decisions() -> list[dict]:
    """The Rust example's offline boot decisions (from the pinned oracle).

    The boot oracle is asserted byte-for-byte against the real offline binary
    by rust/examples/settlement_bot/tests/boot_gate.rs, so this is the
    recorded Rust decision stream, not a second opinion.
    """
    expected = load_boot_oracle()["expected"]
    return [
        {"key": "snapshot_seed_block", "value": expected["snapshot_seed_block"]},
        {"key": "discovery_count", "value": expected["discovery_count"]},
        {"key": "graph.nodes", "value": expected["graph"]["nodes"]},
        {"key": "graph.candidate_tokens", "value": expected["graph"]["candidate_tokens"]},
    ]


def record_offline() -> dict:
    """Rebuild the recorded decision fixture from the offline probes."""
    fixture = {
        "schema": 1,
        "provenance": load_decisions_fixture()["provenance"],
        "permitted_divergence": load_decisions_fixture()["permitted_divergence"],
        "python": python_offline_decisions(),
        "rust": rust_offline_decisions(),
    }
    _DECISIONS_FIXTURE.write_text(json.dumps(fixture, indent=2) + "\n")
    return fixture


def run_recorded() -> int:
    """Replay the recorded fixture and report divergences (CI-safe)."""
    fixture = load_decisions_fixture()
    divergences = diff_decisions(
        fixture["python"], fixture["rust"], fixture["permitted_divergence"]
    )
    if divergences:
        print("dual-driver gate FAILED:")
        for divergence in divergences:
            print(f"  {divergence}")
        return 1
    print(
        "dual-driver gate ok: {} python / {} rust decisions agree "
        "(permitted: {})".format(
            len(fixture["python"]),
            len(fixture["rust"]),
            ", ".join(entry["key"] for entry in fixture["permitted_divergence"]),
        )
    )
    return 0


def _wait_for_rpc(port: int, timeout: float = 30.0) -> None:
    """Poll anvil's HTTP endpoint until it answers."""
    import urllib.request

    payload = json.dumps(
        {"jsonrpc": "2.0", "id": 1, "method": "eth_blockNumber", "params": []}
    ).encode()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            request = urllib.request.Request(
                f"http://127.0.0.1:{port}", data=payload, headers={"Content-Type": "application/json"}
            )
            with urllib.request.urlopen(request, timeout=1):  # noqa: S310
                return
        except OSError:
            time.sleep(0.25)
    msg = f"anvil on port {port} did not start within {timeout}s"
    raise RuntimeError(msg)


def _read_decision_stream(path: Path) -> list[dict]:
    """Read a JSONL per-batch decision stream."""
    if not path.exists():
        msg = (
            f"decision stream {path} was not produced; the driver did not emit "
            f"per-batch decisions (set {STREAM_ENV} in the driver)"
        )
        raise RuntimeError(msg)
    decisions: list[dict] = []
    for line in path.read_text().splitlines():
        line = line.strip()
        if line:
            decisions.append(json.loads(line))
    return decisions


def _run_driver(command: list[str], env: dict[str, str]) -> None:
    """Run one driver bounded by LIVE_TIMEOUT_ENV (dry-run loops until stopped)."""
    timeout = float(os.environ.get(LIVE_TIMEOUT_ENV, "180"))
    try:
        subprocess.run(command, env=env, check=False, timeout=timeout, capture_output=True)  # noqa: S603
    except subprocess.TimeoutExpired:
        # A dry-run driver may keep pumping until stopped; the timeout is the
        # bounded end of the fork-window run.
        pass


def run_live(fork_rpc: str, fork_block: int) -> int:
    """Run both drivers against an anvil fork and diff their decision streams.

    Requires DEGENBOT_DUAL_DRIVER_GATE=1 and DEGENBOT_FORK_RPC. Per-batch
    decision streams are read from DEGENBOT_DECISION_STREAM.{python,rust}.
    """
    if not gate_enabled():
        msg = f"{GATE_ENV}=1 is required to run the live dual-driver gate"
        raise RuntimeError(msg)
    if not _ANVIL.exists():
        msg = f"anvil not found at {_ANVIL}"
        raise RuntimeError(msg)

    import socket

    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    anvil = subprocess.Popen(  # noqa: S603
        [
            str(_ANVIL),
            "--fork-url",
            fork_rpc,
            "--fork-block-number",
            str(fork_block),
            "--port",
            str(port),
            "--silent",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    stream_base = os.environ.get(STREAM_ENV, "/tmp/parity-decisions")
    python_stream = Path(f"{stream_base}.python.jsonl")
    rust_stream = Path(f"{stream_base}.rust.jsonl")
    for stream in (python_stream, rust_stream):
        stream.unlink(missing_ok=True)
    runtime_env = {
        "DEGENBOT_RPC_HTTP_CHAINID_1": f"http://127.0.0.1:{port}",
        "DEGENBOT_RPC_WS_CHAINID_1": f"ws://127.0.0.1:{port}",
        "SMOKE_RPC_URL": f"ws://127.0.0.1:{port}",
        "DEGENBOT_FIXTURE_DB": str(_DB_PATH),
        "DEGENBOT_DISCOVERY_CHAIN_ID": str(DISCOVERY_CHAIN_ID),
    }
    try:
        _wait_for_rpc(port)
        # Dry-run both drivers against the pinned fork for a bounded window.
        _run_driver(
            [
                "cargo",
                "run",
                "--manifest-path",
                str(_REPO / "rust/Cargo.toml"),
                "-p",
                "degenbot-settlement-bot-example",
            ],
            {**os.environ, **runtime_env, STREAM_ENV: str(rust_stream)},
        )
        _run_driver(
            ["uv", "run", "python", str(_REPO / "examples/eth_settlement_arbitrage_v2_v3_v4_rust.py")],
            {**os.environ, **runtime_env, STREAM_ENV: str(python_stream)},
        )
        python_decisions = _read_decision_stream(python_stream)
        rust_decisions = _read_decision_stream(rust_stream)
    finally:
        anvil.terminate()
        anvil.wait(timeout=10)

    fixture = load_decisions_fixture()
    divergences = diff_decisions(
        python_decisions, rust_decisions, fixture["permitted_divergence"]
    )
    if divergences:
        print("live dual-driver gate FAILED:")
        for divergence in divergences:
            print(f"  {divergence}")
        return 1
    print(f"live dual-driver gate ok: {len(python_decisions)} / {len(rust_decisions)} decisions agree")
    return 0


def main(argv: list[str] | None = None) -> int:
    """CLI entrypoint."""
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--recorded", action="store_true", help="replay the recorded fixture (default)")
    mode.add_argument("--record", action="store_true", help="regenerate the recorded fixture")
    mode.add_argument("--live", action="store_true", help="run both drivers against an anvil fork")
    parser.add_argument("--fork-rpc", default=os.environ.get(FORK_ENV, ""))
    parser.add_argument("--fork-block", type=int, default=int(os.environ.get(BLOCK_ENV, "0")))
    args = parser.parse_args(argv)

    if args.record:
        fixture = record_offline()
        print(f"recorded {len(fixture['python'])} python / {len(fixture['rust'])} rust decisions")
        return 0
    if args.live:
        try:
            return run_live(args.fork_rpc, args.fork_block)
        except RuntimeError as error:
            print(f"dual-driver live gate: {error}", file=sys.stderr)
            return 2
    return run_recorded()


if __name__ == "__main__":
    sys.exit(main())
