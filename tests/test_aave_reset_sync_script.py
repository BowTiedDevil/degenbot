"""The Aave soak script (``scripts/aave_reset_sync.sh``).

Six contracts, each negative-probed against a recording stub whose cursor state
is truthful (a dry-run reports the current cursor and echoes an identifier
target as ``None``, as the real CLI does; a reset rewinds to the cold-boot
cursor; a sync moves it):

1. Resume is the default: a plain run never invokes ``aave reset``.
   ``--reset`` is the only thing that purges, and it runs before the sync.
2. A ``latest`` target (the default) is resolved to a concrete block BEFORE any
   write — through ``--node`` when given, else through the CLI's resolved
   ``nodes.http[<chain>]`` — and the plan and sync are both pinned to that
   number. The resolved endpoint is never echoed (no credential logging).
3. A numeric ``--to-block`` skips head resolution entirely.
4. A missing node or a failed head read refuses before any write.
5. A run whose cursor never reached the captured target fails; a
   ``--stop-after-one-chunk`` run may stop short but must advance, measured from
   the cursor a reset left.
6. The operator surface keeps its stated shape: the documented
   ``just aave-soak -- --reset`` form works, ``--max-chunks`` does not exist, and
   the final report states the cursor, event count, and duration.

The stub stands in for both ``degenbot`` (``DEGENBOT_CMD``) and ``cast``
(``CAST_CMD``), so no database and no node is touched.
"""

from __future__ import annotations

import json
import re
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "aave_reset_sync.sh"
JUSTFILE = REPO_ROOT / "justfile"

STUB = """
import json
import os
import sys

state_path = os.environ["SOAK_STUB_STATE"]
with open(state_path) as handle:
    state = json.load(handle)

argv = sys.argv[1:]
state["calls"].append(argv)

def save():
    with open(state_path, "w") as handle:
        json.dump(state, handle)

if argv[:1] == ["--cast"]:
    if state["cast_fails"]:
        save()
        sys.exit(1)
    save()
    print(state["head"])
    sys.exit(0)

def subcommand(words):
    for index, word in enumerate(words):
        if word == "aave":
            return words[index : index + 2]
    return []

if argv[:3] == ["config", "show", "--resolved"]:
    save()
    print("config = /tmp/soak-stub.toml")
    if state["config_has_node"]:
        print(f"nodes.http[1] = {state['config_http_url']} (env)")
    else:
        print("nodes.http = (unresolved)")
    sys.exit(0)

command = subcommand(argv)

if command == ["aave", "reset"]:
    state["cursor"] = state["cold_boot_cursor"]
    save()
    print("Reset chain 1 market 1 (Aave Ethereum Market).")
    sys.exit(0)

if command != ["aave", "update"]:
    save()
    print("stub: unknown command")
    sys.exit(9)

cursor = state["cursor"]
target = argv[argv.index("--to-block") + 1]

if "--dry-run" in argv:
    save()
    resolved = target if target.isdigit() else "None"
    print(
        f"Dry run: would advance chain 1 market 1 (Aave Ethereum Market) from "
        f"block {cursor} to {resolved} (no changes committed)."
    )
    sys.exit(0)

if not target.isdigit():
    save()
    print(f"stub: a sync needs a numeric --to-block, got {target}")
    sys.exit(9)

to_block = int(target)
if state["sync_to"] == "target":
    state["cursor"] = to_block
elif isinstance(state["sync_to"], int):
    state["cursor"] = state["sync_to"]
state["events"] += state["events_per_run"]
save()
print(
    f"Chain 1 market 1 (Aave Ethereum Market): advanced {cursor}-> "
    f"{state['cursor']} in 1 chunks ({state['events_per_run']} events applied)."
)
"""

CONFIGURED_NODE = "http://configured.example:8545"


def _bash() -> str:
    exe = shutil.which("bash")
    assert exe is not None, "bash is not installed on PATH"
    return exe


def _run(tmp_path: Path, *args: str, **state_overrides: object) -> subprocess.CompletedProcess[str]:
    stub = tmp_path / "degenbot_stub.py"
    stub.write_text(STUB)
    state = {
        "calls": [],
        "cursor": 100,
        "cold_boot_cursor": 100,
        "sync_to": "target",
        "head": 250,
        "events": 0,
        "events_per_run": 42,
        "config_has_node": True,
        "config_http_url": CONFIGURED_NODE,
        "cast_fails": False,
    }
    state.update(state_overrides)
    state_path = tmp_path / "state.json"
    state_path.write_text(json.dumps(state))
    env = {
        "PATH": "/usr/bin:/bin",
        # The script's two seams: fixed literal argv lists, never shell text.
        "DEGENBOT_CMD": f"{sys.executable} {stub}",
        "CAST_CMD": f"{sys.executable} {stub} --cast",
        "SOAK_STUB_STATE": str(state_path),
    }
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] -- fixed literal argv list
        [_bash(), str(SCRIPT), *args],
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )


def _without_duration(stdout: str) -> str:
    return re.sub(r"duration \d+s", "duration <n>s", stdout)


def _calls(tmp_path: Path) -> list[list[str]]:
    return json.loads((tmp_path / "state.json").read_text())["calls"]


def _aave_calls(tmp_path: Path) -> list[list[str]]:
    return [call for call in _calls(tmp_path) if "aave" in call]


def _syncs(tmp_path: Path) -> list[list[str]]:
    return [
        call
        for call in _aave_calls(tmp_path)
        if call[call.index("aave") + 1] == "update" and "--dry-run" not in call
    ]


def _cast_calls(tmp_path: Path) -> list[list[str]]:
    return [call for call in _calls(tmp_path) if call[:1] == ["--cast"]]


def _planned_targets(tmp_path: Path) -> list[str]:
    return [
        call[call.index("--to-block") + 1] for call in _aave_calls(tmp_path) if "--dry-run" in call
    ]


def test_the_operator_help_states_the_supported_surface(tmp_path: Path) -> None:
    result = _run(tmp_path, "--help")
    assert result.returncode == 0, result.stderr
    assert "--reset" in result.stdout
    assert "--to-block" in result.stdout
    assert "--stop-after-one-chunk" in result.stdout
    assert "--max-chunks" not in result.stdout
    assert "resume" in result.stdout.lower()
    assert "cast block-number" in result.stdout


def test_an_unknown_argument_is_refused(tmp_path: Path) -> None:
    result = _run(tmp_path, "--max-chunks", "5")
    assert result.returncode == 2
    assert "unknown argument" in result.stderr


def test_default_resume_resolves_latest_before_any_write(tmp_path: Path) -> None:
    # The regression: the CLI's dry-run echoes an identifier target as `None`, so
    # the script must resolve the head first and plan/sync against the number.
    result = _run(tmp_path)
    assert result.returncode == 0, result.stderr + result.stdout

    calls = _calls(tmp_path)
    assert calls[0][:3] == ["config", "show", "--resolved"], calls
    assert calls[1][:1] == ["--cast"], "the head is read before any CLI write"
    assert _cast_calls(tmp_path)[0][1:3] == ["block-number", "--rpc-url"], calls
    assert _cast_calls(tmp_path)[0][3] == CONFIGURED_NODE

    assert _planned_targets(tmp_path) == ["250", "250"], "both plans pin the number"
    assert [call[call.index("--to-block") + 1] for call in _syncs(tmp_path)] == ["250"]
    assert all(call != ["aave", "reset"] for call in _aave_calls(tmp_path)), "no purge"
    assert "resolved latest to block 250" in result.stdout
    assert "cursor 250 events 42 duration" in result.stdout
    assert CONFIGURED_NODE not in result.stdout + result.stderr, "no credential logging"


def test_an_explicit_node_serves_head_resolution_without_the_config(tmp_path: Path) -> None:
    node = "http://explicit.example:8545"
    result = _run(tmp_path, "--node", node)
    assert result.returncode == 0, result.stderr + result.stdout
    assert _cast_calls(tmp_path)[0][3] == node
    assert not any(call[:1] == ["config"] for call in _calls(tmp_path)), (
        "an explicit --node replaces the configured lookup"
    )
    assert node not in result.stdout + result.stderr, "no credential logging"


def test_a_numeric_target_skips_head_resolution(tmp_path: Path) -> None:
    result = _run(tmp_path, "--to-block", "250")
    assert result.returncode == 0, result.stderr + result.stdout
    assert not _cast_calls(tmp_path), "a number is its own answer"
    assert not any(call[:1] == ["config"] for call in _calls(tmp_path))
    assert _planned_targets(tmp_path) == ["250", "250"]
    assert [call[call.index("--to-block") + 1] for call in _syncs(tmp_path)] == ["250"]
    assert "resolved latest" not in result.stdout


def test_a_missing_configured_node_refuses_before_any_write(tmp_path: Path) -> None:
    result = _run(tmp_path, config_has_node=False)
    assert result.returncode == 1
    assert "declares no nodes.http[1]" in result.stderr
    assert not _aave_calls(tmp_path), "nothing ran against the CLI"


def test_a_failed_head_read_refuses_before_any_write(tmp_path: Path) -> None:
    result = _run(tmp_path, cast_fails=True)
    assert result.returncode == 1
    assert "chain-head resolution failed" in result.stderr
    assert not _aave_calls(tmp_path), "nothing ran against the CLI"


def test_a_leading_separator_carries_the_documented_just_form(tmp_path: Path) -> None:
    # `just aave-soak -- --reset --to-block latest` forwards one literal `--`;
    # the script consumes it and parses the rest as a direct invocation would.
    result = _run(tmp_path, "--", "--reset", "--to-block", "latest")
    assert result.returncode == 0, result.stderr + result.stdout

    calls = _calls(tmp_path)
    reset_at = next(
        index
        for index, call in enumerate(calls)
        if "aave" in call and call[call.index("aave") + 1] == "reset"
    )
    sync_at = next(
        index
        for index, call in enumerate(calls)
        if "aave" in call and "update" in call and "--dry-run" not in call
    )
    assert reset_at < sync_at, "the purge runs before the sync"
    assert len(_aave_calls(tmp_path)) == 5, calls  # 3 plans + reset + sync
    assert [call[call.index("--to-block") + 1] for call in _syncs(tmp_path)] == ["250"]
    assert "explicit --reset" in result.stdout
    assert "captured target block 250" in result.stdout
    assert "cursor 250 events 42" in result.stdout


def test_the_separator_form_runs_the_same_commands_as_the_direct_form(
    tmp_path: Path,
) -> None:
    direct = tmp_path / "direct"
    forwarded = tmp_path / "forwarded"
    direct.mkdir()
    forwarded.mkdir()

    without = _run(direct, "--reset", "--to-block", "latest")
    with_separator = _run(forwarded, "--", "--reset", "--to-block", "latest")

    assert without.returncode == 0, without.stderr
    assert with_separator.returncode == 0, with_separator.stderr
    assert _calls(direct) == _calls(forwarded), "one CLI command sequence"
    # The report is identical apart from the wall-clock figure, which the script
    # reads from the shell's SECONDS.
    assert _without_duration(without.stdout) == _without_duration(with_separator.stdout), (
        "one report"
    )


def test_reset_purges_before_the_sync_and_keeps_the_captured_target(tmp_path: Path) -> None:
    result = _run(tmp_path, "--reset", cursor=250)
    assert result.returncode == 0, result.stderr + result.stdout
    calls = _calls(tmp_path)
    reset_at = next(
        index
        for index, call in enumerate(calls)
        if "aave" in call and call[call.index("aave") + 1] == "reset"
    )
    sync_at = next(
        index
        for index, call in enumerate(calls)
        if "aave" in call and "update" in call and "--dry-run" not in call
    )
    assert reset_at < sync_at
    assert "explicit --reset" in result.stdout
    assert "reset left the market at 100" in result.stdout
    sync = _syncs(tmp_path)[0]
    assert sync[sync.index("--to-block") + 1] == "250", "the pre-reset target survives the purge"


def test_reset_then_one_chunk_measures_advancement_from_the_cold_boot_cursor(
    tmp_path: Path,
) -> None:
    # The warm market sits at its target (250); the reset rewinds it to 100; one
    # chunk lands at 150. The run advanced — from the cursor the reset left — and
    # stopped short of the target captured before any write.
    result = _run(
        tmp_path,
        "--reset",
        "--stop-after-one-chunk",
        cursor=250,
        cold_boot_cursor=100,
        sync_to=150,
    )
    assert result.returncode == 0, result.stderr + result.stdout
    assert "reset left the market at 100" in result.stdout
    assert "not reached by design" in result.stdout
    assert "cursor 150 events 42" in result.stdout
    sync = _syncs(tmp_path)[0]
    assert sync[sync.index("--to-block") + 1] == "250", "the sync target is the pre-capture"


def test_a_cursor_that_never_reaches_the_target_fails(tmp_path: Path) -> None:
    result = _run(tmp_path, sync_to=249)
    assert result.returncode == 1
    assert "never reached the captured target 250" in result.stderr


def test_one_chunk_may_stop_short_but_must_advance(tmp_path: Path) -> None:
    result = _run(tmp_path, "--stop-after-one-chunk", sync_to=150)
    assert result.returncode == 0, result.stderr
    sync = _syncs(tmp_path)
    assert "--one-chunk" in sync[0], sync
    assert "not reached by design" in result.stdout


def test_one_chunk_that_does_not_advance_fails(tmp_path: Path) -> None:
    result = _run(tmp_path, "--stop-after-one-chunk", sync_to=None)
    assert result.returncode == 1
    assert "did not advance" in result.stderr


def test_the_soak_is_an_on_demand_recipe_and_never_a_default_gate() -> None:
    recipe = JUSTFILE.read_text()
    assert "\naave-soak " in recipe, "the recipe exists"
    soak = recipe.split("\naave-soak ", 1)[1].split("\n\n", 1)[0]
    assert "aave_reset_sync.sh" in soak
    assert "test" not in soak.split("\n")[0], "no test recipe depends on the soak"
