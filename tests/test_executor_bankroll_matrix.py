"""Tests for the one-command bankroll matrix runner.

The runner's whole job is orchestration, so every test drives a fake canonical
launcher and a fake matrix client: no bot is spawned, no socket is dialed, and
no network is touched. What is under test is the lifecycle — one start, one
injection, both probes on that same registration set, archived evidence, real
return codes, and a stop that always runs.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import signal
from pathlib import Path
from typing import TYPE_CHECKING, Any

import pytest

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "executor_bankroll_matrix.py"
SPEC = importlib.util.spec_from_file_location("executor_bankroll_matrix", MODULE_PATH)
assert SPEC is not None
assert SPEC.loader is not None
bankroll = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bankroll)

OWNER = "0x" + "11" * 20
INJECTED = "0x" + "22" * 20
MANIFEST = ROOT / "scripts" / "executor_three_hop_matrix.json"

#: The owner the tracked deployment baked into its runtime, in the mixed case the
#: deployment spells it; a run's ``--owner`` must be this address.
TRACKED_OWNER = "0x9C56a29c7231974c269E24F9FB3c29203039089E"

#: The runtime the bot injects: the deployed ``cmd_executor/2`` bytecode, code
#: section plus the seven immutable words its constructor wrote.
TRACKED_RUNTIME = (
    ROOT
    / "rust"
    / "crates"
    / "foundation"
    / "degenbot-executor"
    / "releases"
    / "v2"
    / "deployment"
    / "cmd_executor_runtime_bytecode.txt"
)

#: The fixture runtime's code section. The runner reads the immutable tail that
#: follows it, so no test depends on this being executable bytecode.
RUNTIME_CODE = "0x60006000"


def _owner_word(owner: str) -> str:
    """One address immutable word: twelve zero bytes, then the owner's twenty."""
    return "00" * 12 + owner[2:].lower()


def _runtime_hex(owner: str = OWNER) -> str:
    """One ``cmd_executor/2`` runtime: a minimal code section, then the seven-word tail.

    Word 0 carries OWNER_ADDR, as ``cmd_executor.immutables.json`` orders the
    tail; the other six words are immutables this runner never reads.
    """
    return RUNTIME_CODE + _owner_word(owner) + "ab" * (32 * 6)


def _summary(*, complete: bool = True, unexpected: int = 0, matched: int = 27) -> dict[str, Any]:
    """One report summary in the matrix's own shape."""
    return {
        "expect": None,
        "expected": 27,
        "completed": 27 if complete else 4,
        "missing": 0 if complete else 23,
        "matched": matched,
        "unexpected": unexpected,
        "complete": complete,
        "interrupted": False,
        "categories": {},
    }


class World:
    """A fake launcher, a fake matrix client, and a fake endpoint for one run."""

    def __init__(self, root: Path) -> None:
        """Build a run directory, a runtime artifact, and the launcher's own paths."""
        self.root = root
        self.out = root / "run"
        self.runtime = root / "runtime.txt"
        self.runtime.write_text(_runtime_hex(), encoding="utf-8")
        self.bot_log = root / "logs" / "bot_run.log"
        self.bot_log.parent.mkdir(parents=True, exist_ok=True)
        self.pidfile = root / "logs" / "bot_run.pid"
        self.calls: list[tuple[str, list[str], Mapping[str, str] | None]] = []
        self.rpc_calls: list[str] = []
        self.running = False
        self.start_code = 0
        self.inject_code = 0
        self.probe_codes: dict[str, int] = {"self_fund": 0, "zero_balance": 0}
        self.probe_summaries: dict[str, dict[str, Any]] = {}
        self.stop_code = 0
        self.leave_session = False
        self.lose_bot_log = False
        self.rpc_answers: dict[str, object] = {"eth_chainId": "0x1", "eth_blockNumber": "0x1400000"}
        self.rpc_error: str | None = None
        self.commit = "a" * 40
        self.dirty = True
        self.raise_on: str | None = None
        self.stop_request: Any = None
        self.stop_during_probe: str | None = None
        self.omit_report: set[str] = set()
        self.metadata_at_probe: dict[str, dict[str, Any] | None] = {}
        self.probe_requests: dict[str, list[str]] = {}

    # ── the injected transports ────────────────────────────────────────

    @property
    def socket(self) -> Path:
        """The socket this run would bind (created by ``start``)."""
        return self.out / "operator.sock"

    def deps(self) -> Any:
        """The runner's dependencies, all pointing at this fake world."""
        return bankroll.RunnerDeps(
            run=self.run,
            rpc=self.rpc,
            bot_log=self.bot_log,
            bot_pidfile=self.pidfile,
        )

    def run(self, argv: Sequence[str], *, env: Mapping[str, str] | None = None) -> Any:
        """Run one faked command."""
        args = [str(item) for item in argv]
        step = self._step(args)
        self.calls.append((step, args, env))
        if self.raise_on == step:
            msg = f"injected runner failure at {step}"
            raise RuntimeError(msg)
        if args[0] == "git":
            if args[1] == "rev-parse":
                return self._result(args, 0, f"{self.commit}\n")
            return self._result(args, 0, " M scripts/x.py\n" if self.dirty else "")
        if args[0].endswith("run_bot.sh"):
            return self._launcher(args)
        return self._matrix(args)

    def rpc(self, method: str) -> object:
        """Answer one JSON-RPC method."""
        self.rpc_calls.append(method)
        if self.rpc_error is not None:
            msg = self.rpc_error
            raise RuntimeError(msg)
        return self.rpc_answers[method]

    # ── the fakes behind those transports ──────────────────────────────

    def _step(self, argv: Sequence[str]) -> str:
        if argv[0] == "git":
            return "git"
        if argv[0].endswith("run_bot.sh"):
            return argv[1]
        if argv[2] == "inject":
            return "inject"
        return f"probe-{argv[argv.index('--capital') + 1]}"

    def _result(self, argv: Sequence[str], code: int, output: str) -> Any:
        return bankroll.CommandResult(argv=tuple(argv), returncode=code, output=output)

    def _launcher(self, argv: list[str]) -> Any:
        action = argv[1]
        if action == "status":
            answer = "[runner] running pid 42" if self.running else "[runner] not running"
            return self._result(argv, 0, answer)
        if action == "start":
            if self.start_code == 0:
                self.running = True
                self.pidfile.write_text("42\n", encoding="utf-8")
                self.socket.write_text("", encoding="utf-8")
                self.bot_log.write_text(
                    "[bot] rpc=http://user:token@rpc.example/v3/s3cret\n", encoding="utf-8"
                )
                return self._result(
                    argv,
                    0,
                    "[runner] started bot pid 42\n"
                    "[runner] resolved rpc http=http://user:token@rpc.example/v3/s3cret",
                )
            return self._result(argv, self.start_code, "[runner] bot already running (pid 42)")
        if action == "stop":
            if not self.leave_session:
                self.running = False
                self.pidfile.unlink(missing_ok=True)
                self.socket.unlink(missing_ok=True)
            # The launcher appends the driver's exit and its resolved endpoints
            # to its own log; the runner must archive this tail, redacted.
            with self.bot_log.open("a", encoding="utf-8") as handle:
                handle.write("[runner] bot exited rc=0 shutdown-complete\n")
                handle.write("[runner] resolved rpc ws=ws://user:token@rpc.example/ws/s3cret\n")
            if self.lose_bot_log:
                # A launcher that rotated its own log leaves nothing to archive.
                self.bot_log.unlink(missing_ok=True)
            return self._result(argv, self.stop_code, "[runner] stopped")
        msg = f"unexpected launcher action {action}"
        raise AssertionError(msg)

    def _matrix(self, argv: list[str]) -> Any:
        action = argv[2]
        if action == "inject":
            if self.inject_code == 0:
                Path(argv[argv.index("--output") + 1]).write_text("[]\n", encoding="utf-8")
            return self._result(argv, self.inject_code, "V2-V3-V4 forward path_id=1\n")
        capital = argv[argv.index("--capital") + 1]
        self.probe_requests[capital] = argv
        metadata_path = Path(argv[argv.index("--metadata-file") + 1])
        self.metadata_at_probe[capital] = (
            json.loads(metadata_path.read_text(encoding="utf-8"))
            if metadata_path.is_file()
            else None
        )
        code = self.probe_codes[capital]
        summary = self.probe_summaries.get(
            capital, _summary(complete=code == 0, unexpected=1 if code else 0)
        )
        if capital not in self.omit_report:
            Path(argv[argv.index("--output") + 1]).write_text(
                json.dumps({
                    "schema_version": 2,
                    "metadata": self.metadata_at_probe[capital],
                    "fixed_input": int(argv[argv.index("--fixed-input") + 1]),
                    "expect": argv[argv.index("--expect") + 1],
                    "capital": capital,
                    "paths": [],
                    "summary": summary,
                }),
                encoding="utf-8",
            )
        if self.stop_during_probe == capital and self.stop_request is not None:
            self.stop_request.request(signal.SIGTERM)
        return self._result(argv, code, f"probe {capital} exit={code}\n")

    # ── assertions helpers ─────────────────────────────────────────────

    def argv(self, *extra: str) -> list[str]:
        """The runner's own command line for this world."""
        return [
            "--owner",
            OWNER,
            "--injected-address",
            INJECTED,
            "--runtime",
            str(self.runtime),
            "--output-dir",
            str(self.out),
            *extra,
        ]

    def steps(self) -> list[str]:
        """The lifecycle steps this world saw, in order."""
        return [step for step, _argv, _env in self.calls]

    def argv_for(self, step: str) -> list[list[str]]:
        """Every argv recorded for one step."""
        return [argv for recorded, argv, _env in self.calls if recorded == step]

    def env_for(self, step: str) -> Mapping[str, str] | None:
        """The environment overrides recorded for one step (the first one)."""
        for recorded, _argv, env in self.calls:
            if recorded == step:
                return env
        return None

    def artifact(self, name: str) -> Path:
        """One archived artifact's path."""
        return self.out / name

    def metadata(self) -> dict[str, Any]:
        """The archived metadata document."""
        return json.loads(self.artifact("metadata.json").read_text(encoding="utf-8"))

    def run_metadata(self) -> dict[str, Any]:
        """The archived run-level record."""
        return json.loads(self.artifact("run_metadata.json").read_text(encoding="utf-8"))


@pytest.fixture
def world(tmp_path: Path) -> World:
    """One fake world per test."""
    return World(tmp_path)


def _run(world: World, *extra: str) -> Any:
    """Run the runner against the fake world, the way the CLI does."""
    return bankroll.run_matrix(
        bankroll.config_from_args(bankroll.build_parser().parse_args(world.argv(*extra))),
        world.deps(),
    )


# ── the complete run ───────────────────────────────────────────────────


def test_one_command_runs_both_postures_over_one_injection(world: World) -> None:
    """The canonical command: one fresh launch, one registration set, two probes."""
    record = _run(world)

    assert record.outcome == bankroll.OUTCOME_COMPLETE
    assert bankroll.exit_code(record) == bankroll.EXIT_COMPLETE

    assert world.steps().count("start") == 1
    assert world.steps().count("inject") == 1
    assert world.steps().count("probe-self_fund") == 1
    assert world.steps().count("probe-zero_balance") == 1
    assert world.steps().count("stop") == 1

    # Both probes used the ONE registration set the single injection wrote.
    registrations = str(world.artifact("registrations.json"))
    for capital in ("self_fund", "zero_balance"):
        (argv,) = world.argv_for(f"probe-{capital}")
        assert argv[argv.index("--registrations") + 1] == registrations
    assert world.metadata_at_probe["self_fund"] == world.metadata_at_probe["zero_balance"]

    # Each posture asked for its own expectation and its own capital.
    self_fund = world.probe_requests["self_fund"]
    zero_balance = world.probe_requests["zero_balance"]
    assert self_fund[self_fund.index("--expect") + 1] == "executed"
    assert zero_balance[zero_balance.index("--expect") + 1] == "insolvent"
    assert self_fund[self_fund.index("--capital") + 1] == "self_fund"
    assert zero_balance[zero_balance.index("--capital") + 1] == "zero_balance"

    # The fixed input is stated on every probe and in the report.
    for capital in ("self_fund", "zero_balance"):
        argv = world.probe_requests[capital]
        assert argv[argv.index("--fixed-input") + 1] == str(bankroll.DEFAULT_FIXED_INPUT)


def test_the_launch_is_a_fresh_dry_run_session(world: World) -> None:
    """The launcher is asked for one operator-only session on this run's socket."""
    _run(world)

    (start,) = world.argv_for("start")
    assert start[0].endswith("run_bot.sh")
    assert start[1] == "start"
    assert start[2] == "--"
    assert "--live" not in start, "a probe session never arms a submit lane"
    assert start[start.index("--operator-socket") + 1] == str(world.socket)
    assert "--skip-discovery" in start
    assert "--sparse-registration" in start
    assert world.socket.parent == world.out, "the socket lives inside the run directory"
    for _step, argv, _env in world.calls:
        assert "--live" not in argv, "--live never appears in any command this runner issues"


def test_the_launcher_gets_the_executor_injection_environment(world: World) -> None:
    """Injection is on, the runtime under test is named, and the floor is zero."""
    _run(world)

    env = world.env_for("start")
    assert env is not None
    assert set(env) == set(bankroll.LAUNCH_ENV_KEYS), "no other environment is imposed"
    assert env["DEGENBOT_INJECT_EXECUTOR_CODE"] == "1"
    assert env["EXECUTOR_OWNER_ADDRESS"] == OWNER
    assert env["INJECTED_EXECUTOR_ADDRESS"] == INJECTED
    assert env["EXECUTOR_RUNTIME"] == str(world.runtime)
    assert env["DEGENBOT_MIN_PROFIT_WEI"] == "0"
    assert env["DEGENBOT_MIN_PROFIT_MARGIN_BPS"] == "0"
    for name in env:
        lowered = name.lower()
        assert "key" not in lowered, "no key material may be passed through this environment"
        assert "secret" not in lowered
        assert "private" not in lowered


def test_every_artifact_is_archived_in_the_run_directory(world: World) -> None:
    """Logs, registrations, reports, and both metadata files are all in one place."""
    _run(world)

    for name in (
        "metadata.json",
        "run_metadata.json",
        "registrations.json",
        "report_self_fund.json",
        "report_zero_balance.json",
        "matrix_inject.log",
        "matrix_probe_self_fund.log",
        "matrix_probe_zero_balance.log",
        "launcher.log",
        "bot_run.log",
    ):
        path = world.artifact(name)
        assert path.is_file(), f"{name} was not archived"
        assert path.stat().st_size > 0, f"{name} is empty"

    report = json.loads(world.artifact("report_self_fund.json").read_text(encoding="utf-8"))
    assert report["metadata"]["chain_id"] == 1
    assert report["capital"] == "self_fund"


def test_the_metadata_records_what_a_reproduction_needs(world: World) -> None:
    """Chain, block, release, hashes, source state, posture, and artifact names."""
    _run(world, "--fixed-input", str(10**16))

    metadata = world.metadata()
    assert metadata["chain_id"] == 1
    assert metadata["block_number"] == 0x1400000
    assert metadata["release_id"] == "cmd_executor/2"
    assert metadata["runtime_sha256"] == hashlib.sha256(world.runtime.read_bytes()).hexdigest()
    assert metadata["matrix_manifest_sha256"] == hashlib.sha256(MANIFEST.read_bytes()).hexdigest()
    assert metadata["source_commit"] == world.commit
    assert metadata["source_dirty"] is True
    assert metadata["fixed_input"] == 10**16
    assert metadata["capitals"] == ["self_fund", "zero_balance"]
    assert metadata["capital_selection"] == "both"
    assert metadata["expectations"] == {"self_fund": "executed", "zero_balance": "insolvent"}
    assert metadata["owner"] == OWNER
    assert metadata["injected_address"] == INJECTED
    assert metadata["output_dir"] == str(world.out)
    assert metadata["artifacts"]["reports"] == {
        "self_fund": "report_self_fund.json",
        "zero_balance": "report_zero_balance.json",
    }
    assert metadata["artifacts"]["registrations"] == "registrations.json"
    assert metadata["artifacts"]["bot_log"] == "bot_run.log"


def test_a_clean_tree_is_recorded_as_clean(world: World) -> None:
    """The dirty flag is git's answer, not a constant."""
    world.dirty = False
    _run(world, "--capital", "self_fund")

    assert world.metadata()["source_dirty"] is False


def test_no_bot_pidfile_or_socket_remains_after_a_complete_run(world: World) -> None:
    """Completion means the session is gone, verified rather than assumed."""
    record = _run(world)

    assert record.cleanup["verified"] is True
    assert record.cleanup["bot_running"] is False
    assert record.cleanup["pidfile_present"] is False
    assert record.cleanup["socket_present"] is False
    assert record.cleanup["stop_returncode"] == 0
    assert not world.pidfile.exists()
    assert not world.socket.exists()
    assert world.steps()[-2:] == ["stop", "status"], "stop, then a status verification"


def test_the_run_metadata_records_real_exit_codes_and_cleanup(world: World) -> None:
    """Every command's own status is archived, not a synthesized verdict."""
    _run(world)

    document = world.run_metadata()
    assert document["outcome"] == "complete"
    assert document["exit_code"] == 0
    assert document["launched"] is True
    assert document["stop"] == {"requested": False, "signal": None}
    steps = [command["step"] for command in document["commands"]]
    assert steps == [
        "status-refusal-check",
        "start",
        "inject",
        "probe-self_fund",
        "probe-zero_balance",
        "stop",
        "status-cleanup-verify",
    ]
    assert all(command["returncode"] == 0 for command in document["commands"])
    assert document["cleanup"]["verified"] is True
    assert [mode["exit_code"] for mode in document["modes"]] == [0, 0]


def test_the_rpc_endpoint_never_reaches_a_log_or_a_record(world: World) -> None:
    """A URI can carry a provider token, so captured output is scrubbed."""
    _run(world)

    launcher_log = world.artifact("launcher.log").read_text(encoding="utf-8")
    assert "rpc.example" not in launcher_log, "the launcher log leaked the endpoint"
    assert "<rpc-uri>" in launcher_log, "the redacted line still says a URI was there"

    record = world.artifact("run_metadata.json").read_text(encoding="utf-8")
    assert "rpc.example" not in record

    bot_log = world.artifact("bot_run.log").read_text(encoding="utf-8")
    assert "rpc.example" not in bot_log, "the archived bot log leaked the endpoint"
    assert "s3cret" not in bot_log, "the archived bot log leaked the endpoint's credentials"
    assert "<rpc-uri>" in bot_log


def test_the_archived_bot_log_keeps_the_shutdown_tail(world: World) -> None:
    """The copy is taken after the stop, so the session's end is in the evidence."""
    _run(world)

    bot_log = world.artifact("bot_run.log").read_text(encoding="utf-8")
    assert "shutdown-complete" in bot_log, "the archived copy stops before the bot's exit"
    assert world.run_metadata()["cleanup"]["archived_bot_log"] is True


def test_a_launched_session_without_an_archived_bot_log_fails_the_run(world: World) -> None:
    """The bot's own log is part of the evidence envelope, not an optional extra."""
    world.lose_bot_log = True

    record = _run(world)

    assert record.cleanup["archived_bot_log"] is False
    assert not world.artifact("bot_run.log").exists(), "nothing was archived to inspect"
    assert record.cleanup["verified"] is False, "a clean stop does not stand in for the log"
    assert record.outcome == bankroll.OUTCOME_FAILED
    assert bankroll.exit_code(record) == bankroll.EXIT_FAILED
    assert "archived bot log" in (record.error or "")


def test_a_run_that_never_launched_owes_no_archived_bot_log(world: World) -> None:
    """A stop requested before the launch starts nothing, so no log is owed."""
    stop = bankroll.StopRequest()
    stop.request(signal.SIGTERM)

    record = bankroll.run_matrix(
        bankroll.config_from_args(bankroll.build_parser().parse_args(world.argv())),
        world.deps(),
        stop=stop,
    )

    assert record.launched is False
    assert "start" not in world.steps()
    assert record.cleanup["archived_bot_log"] is False
    assert record.cleanup["verified"] is True, "a run that never launched owes no log"
    assert record.outcome == bankroll.OUTCOME_STOPPED
    assert bankroll.exit_code(record) == bankroll.EXIT_STOPPED


# ── capital selection ──────────────────────────────────────────────────


@pytest.mark.parametrize(
    ("capital", "expect"),
    [("self_fund", "executed"), ("zero_balance", "insolvent")],
)
def test_a_selected_capital_runs_only_that_posture(
    world: World, *, capital: str, expect: str
) -> None:
    record = _run(world, "--capital", capital)

    assert record.outcome == bankroll.OUTCOME_COMPLETE
    assert world.steps().count("start") == 1
    assert world.steps().count("inject") == 1
    probes = [step for step in world.steps() if step.startswith("probe-")]
    assert probes == [f"probe-{capital}"]
    argv = world.probe_requests[capital]
    assert argv[argv.index("--expect") + 1] == expect
    assert world.metadata()["capitals"] == [capital]
    assert world.metadata()["capital_selection"] == capital


# ── failure propagation ───────────────────────────────────────────────


def test_a_startup_failure_stops_and_never_probes(world: World) -> None:
    """A launcher that refuses leaves no bot to probe and no probe to attempt."""
    world.start_code = 1

    record = _run(world)

    assert record.outcome == bankroll.OUTCOME_FAILED
    assert bankroll.exit_code(record) == bankroll.EXIT_FAILED
    assert "start" in world.steps()
    assert "inject" not in world.steps()
    assert not any(step.startswith("probe-") for step in world.steps())
    assert world.steps().count("stop") == 1, "the launcher is always told to stop"


def test_an_injection_failure_stops_and_never_probes(world: World) -> None:
    """A registration set that never landed cannot be probed."""
    world.inject_code = 1

    record = _run(world)

    assert record.outcome == bankroll.OUTCOME_FAILED
    assert not any(step.startswith("probe-") for step in world.steps())
    assert world.steps().count("stop") == 1
    assert world.run_metadata()["error"].startswith("registration injection exited")


def test_the_second_posture_is_attempted_after_an_unexpected_first_report(world: World) -> None:
    """Both real exit codes survive: the failure is kept, the second probe runs."""
    world.probe_codes["self_fund"] = 1
    world.probe_summaries["self_fund"] = _summary(complete=True, unexpected=27, matched=0)

    record = _run(world)

    assert [mode["exit_code"] for mode in record.modes] == [1, 0]
    assert [mode["outcome"] for mode in record.modes] == [
        bankroll.OUTCOME_MISMATCHED,
        bankroll.OUTCOME_COMPLETE,
    ]
    assert record.outcome == bankroll.OUTCOME_MISMATCHED
    assert bankroll.exit_code(record) == bankroll.EXIT_FAILED
    assert world.steps().count("stop") == 1


def test_an_incomplete_report_is_incomplete_not_complete(world: World) -> None:
    """A probe that stopped early cannot be reported as coverage."""
    world.probe_codes["zero_balance"] = 1
    world.probe_summaries["zero_balance"] = _summary(complete=False, matched=4)

    record = _run(world)

    assert [mode["outcome"] for mode in record.modes] == [
        bankroll.OUTCOME_COMPLETE,
        bankroll.OUTCOME_INCOMPLETE,
    ]
    assert record.outcome == bankroll.OUTCOME_INCOMPLETE
    assert bankroll.exit_code(record) == bankroll.EXIT_FAILED


def test_a_zero_exit_probe_without_a_report_is_a_failure(world: World) -> None:
    """No report means nothing was audited, whatever the command returned."""
    world.omit_report.add("self_fund")

    record = _run(world, "--capital", "self_fund")

    assert record.modes[0]["exit_code"] == 0
    assert record.modes[0]["summary"] is None
    assert record.modes[0]["outcome"] == bankroll.OUTCOME_FAILED
    assert record.outcome == bankroll.OUTCOME_FAILED
    assert bankroll.exit_code(record) == bankroll.EXIT_FAILED


@pytest.mark.parametrize(
    "summary",
    [
        {"complete": True},
        {"complete": True, "interrupted": False},
        {"complete": True, "expected": 27, "matched": 26, "missing": 0, "unexpected": 0},
        {"complete": True, "expected": 0, "matched": 0, "missing": 0, "unexpected": 0},
        {"complete": True, "expected": 27, "matched": 27, "missing": 1, "unexpected": 0},
        {"complete": "yes", "expected": 27, "matched": 27, "missing": 0, "unexpected": 0},
        {
            "complete": True,
            "expected": 27,
            "matched": 27,
            "missing": 0,
            "unexpected": True,
        },
        {
            "complete": True,
            "expected": 27,
            "matched": 27,
            "missing": 0,
            "unexpected": 0,
        },
        {
            "complete": True,
            "interrupted": None,
            "expected": 27,
            "matched": 27,
            "missing": 0,
            "unexpected": 0,
        },
        {
            "complete": True,
            "interrupted": "no",
            "expected": 27,
            "matched": 27,
            "missing": 0,
            "unexpected": 0,
        },
        {
            "complete": True,
            "interrupted": 0,
            "expected": 27,
            "matched": 27,
            "missing": 0,
            "unexpected": 0,
        },
        {
            "complete": True,
            "interrupted": True,
            "expected": 27,
            "matched": 27,
            "missing": 0,
            "unexpected": 0,
        },
    ],
)
def test_a_zero_exit_probe_with_a_thin_or_partial_summary_is_not_complete(
    world: World, *, summary: dict[str, Any]
) -> None:
    """Completion is arithmetic on the report, not the command's exit code."""
    world.probe_summaries["self_fund"] = summary

    record = _run(world, "--capital", "self_fund")

    assert record.modes[0]["outcome"] != bankroll.OUTCOME_COMPLETE
    assert record.outcome != bankroll.OUTCOME_COMPLETE
    assert bankroll.exit_code(record) == bankroll.EXIT_FAILED


def test_a_nonzero_probe_is_a_failure_even_when_its_report_reads_complete(
    world: World,
) -> None:
    """A nonzero exit is a real result and is retained, not overwritten by the report."""
    world.probe_codes["self_fund"] = 1
    world.probe_summaries["self_fund"] = _summary()

    record = _run(world, "--capital", "self_fund")

    assert record.modes[0]["exit_code"] == 1
    assert record.modes[0]["outcome"] == bankroll.OUTCOME_FAILED
    assert record.outcome == bankroll.OUTCOME_FAILED


def test_a_runner_exception_still_stops_and_reports(
    world: World, monkeypatch: pytest.MonkeyPatch
) -> None:
    """An exception inside the runner is a failed run with a clean session."""

    def exploding(_path: Path) -> dict[str, Any] | None:
        msg = "injected runner failure"
        raise RuntimeError(msg)

    monkeypatch.setattr(bankroll, "read_report_summary", exploding)

    record = _run(world)

    assert record.outcome == bankroll.OUTCOME_FAILED
    assert "injected runner failure" in (record.error or "")
    assert world.steps().count("stop") == 1
    assert world.run_metadata()["outcome"] == "failed"
    assert record.cleanup["verified"] is True


def test_a_command_that_cannot_run_is_a_recorded_failure(world: World) -> None:
    """A launcher that cannot even be executed stops the run, with no probe issued."""
    world.raise_on = "inject"

    record = _run(world)

    assert record.outcome == bankroll.OUTCOME_FAILED
    assert not any(step.startswith("probe-") for step in world.steps())
    inject = next(
        command for command in world.run_metadata()["commands"] if command["step"] == "inject"
    )
    assert inject["returncode"] is None, "a command that never ran records no exit code"


def test_a_session_that_does_not_shut_down_fails_the_run(world: World) -> None:
    """A leftover pidfile or socket is a failed cleanup, never a quiet success."""
    world.leave_session = True

    record = _run(world)

    assert record.cleanup["verified"] is False
    assert record.cleanup["pidfile_present"] is True, "a live pidfile is a failed cleanup"
    assert record.cleanup["socket_existed"] is True
    assert record.cleanup["socket_present"] is False, "the runner removes the socket itself"
    assert record.outcome == bankroll.OUTCOME_FAILED
    assert bankroll.exit_code(record) == bankroll.EXIT_FAILED


# ── the operator's own stop ───────────────────────────────────────────


def test_a_sigterm_during_a_probe_is_a_stopped_run_with_cleanup(world: World) -> None:
    """SIGTERM is the operator's outcome: stop, archive, and skip what is left."""
    stop = bankroll.StopRequest()
    world.stop_request = stop
    world.stop_during_probe = "self_fund"
    world.probe_codes["self_fund"] = 1
    world.probe_summaries["self_fund"] = _summary(complete=False, matched=4)

    record = bankroll.run_matrix(
        bankroll.config_from_args(bankroll.build_parser().parse_args(world.argv())),
        world.deps(),
        stop=stop,
    )

    assert record.outcome == bankroll.OUTCOME_STOPPED
    assert bankroll.exit_code(record) == bankroll.EXIT_STOPPED
    assert record.modes[1]["skipped"] is True, "the remaining posture is named as skipped"
    assert world.steps().count("stop") == 1
    assert record.cleanup["verified"] is True
    assert record.cleanup["stop_requested"] is True
    assert record.cleanup["stop_signal"] == "SIGTERM"
    document = world.run_metadata()
    assert document["outcome"] == "stopped"
    assert document["stop"] == {"requested": True, "signal": "SIGTERM"}


def test_a_stopped_run_whose_session_survives_is_a_failure(world: World) -> None:
    """The operator's stop never excuses a leftover pidfile or socket."""
    stop = bankroll.StopRequest()
    world.stop_request = stop
    world.stop_during_probe = "self_fund"
    world.probe_codes["self_fund"] = 1
    world.probe_summaries["self_fund"] = _summary(complete=False, matched=4)
    world.leave_session = True

    record = bankroll.run_matrix(
        bankroll.config_from_args(bankroll.build_parser().parse_args(world.argv())),
        world.deps(),
        stop=stop,
    )

    assert stop.requested is True
    assert record.cleanup["verified"] is False
    assert record.cleanup["pidfile_present"] is True
    assert record.outcome == bankroll.OUTCOME_FAILED, "a live session outranks the stop outcome"
    assert bankroll.exit_code(record) == bankroll.EXIT_FAILED


def test_a_failed_stop_command_is_a_failure_even_when_nothing_is_left(world: World) -> None:
    """A stop the launcher reports as failed is not a clean shutdown."""
    stop = bankroll.StopRequest()
    world.stop_request = stop
    world.stop_during_probe = "self_fund"
    world.probe_codes["self_fund"] = 1
    world.stop_code = 1

    record = bankroll.run_matrix(
        bankroll.config_from_args(bankroll.build_parser().parse_args(world.argv())),
        world.deps(),
        stop=stop,
    )

    assert record.cleanup["stop_returncode"] == 1
    assert record.cleanup["stop_failed"] is True
    assert record.cleanup["verified"] is False
    assert record.outcome == bankroll.OUTCOME_FAILED
    assert bankroll.exit_code(record) == bankroll.EXIT_FAILED


def test_the_stop_handler_records_the_signal_instead_of_raising() -> None:
    """The installed handler turns a stop signal into run state."""
    stop = bankroll.StopRequest()
    stop.install()
    try:
        signal.raise_signal(signal.SIGTERM)
        assert stop.requested is True
        assert stop.signal_name == "SIGTERM"
        stop.request(signal.SIGINT)
        assert stop.signal_name == "SIGTERM", "the first request is the one recorded"
    finally:
        stop.restore()


# ── refusals ──────────────────────────────────────────────────────────


def test_a_nonempty_output_directory_is_refused(world: World) -> None:
    """A used directory would mix one run's evidence with another's."""
    world.out.mkdir(parents=True)
    (world.out / "leftover.json").write_text("{}", encoding="utf-8")

    assert bankroll.main(world.argv(), deps=world.deps()) == bankroll.EXIT_REFUSED
    assert "start" not in world.steps(), "a refusal never launches"
    assert "stop" not in world.steps(), "a refusal never stops somebody else's session"
    assert (world.out / "leftover.json").is_file(), "the refusal left the directory alone"


def test_an_already_running_bot_is_refused(world: World) -> None:
    """Refusing is the whole point: the runner must not kill a live session."""
    world.running = True

    assert bankroll.main(world.argv(), deps=world.deps()) == bankroll.EXIT_REFUSED
    assert "start" not in world.steps()
    assert "stop" not in world.steps(), "an already-running bot is never stopped by this runner"


def test_an_unreadable_launcher_status_is_refused(world: World) -> None:
    """When the launcher cannot say, the runner does not guess."""

    def broken(argv: Sequence[str], *, env: Mapping[str, str] | None = None) -> Any:
        return bankroll.CommandResult(argv=tuple(str(item) for item in argv), returncode=3)

    deps = bankroll.RunnerDeps(
        run=broken, rpc=world.rpc, bot_log=world.bot_log, bot_pidfile=world.pidfile
    )
    assert bankroll.main(world.argv(), deps=deps) == bankroll.EXIT_REFUSED


@pytest.mark.parametrize(
    ("method", "answer"),
    [("eth_chainId", "0x2"), ("eth_chainId", "1"), ("eth_blockNumber", "0x0")],
)
def test_an_endpoint_that_cannot_state_the_run_facts_is_refused(
    world: World, *, method: str, answer: str
) -> None:
    """A wrong chain, an unreadable quantity, or no head block fails before launch."""
    world.rpc_answers[method] = answer

    assert bankroll.main(world.argv(), deps=world.deps()) == bankroll.EXIT_REFUSED
    assert "start" not in world.steps()
    assert not world.artifact("metadata.json").exists(), "no invented facts are archived"


def test_an_unreachable_endpoint_is_refused_before_launch(world: World) -> None:
    """A failed RPC never becomes a fabricated block number."""
    world.rpc_error = "connection refused"

    assert bankroll.main(world.argv(), deps=world.deps()) == bankroll.EXIT_REFUSED
    assert "start" not in world.steps()
    assert not world.out.exists(), "the refusal did not create a run directory"


def test_a_missing_runtime_artifact_is_refused(world: World) -> None:
    """The artifact under test must exist to be hashed and injected."""
    world.runtime.unlink()

    assert bankroll.main(world.argv(), deps=world.deps()) == bankroll.EXIT_REFUSED
    assert "start" not in world.steps()


# ── the runtime's own owner ───────────────────────────────────────────


def test_a_runtime_owned_by_another_address_is_refused(
    world: World, capsys: pytest.CaptureFixture[str]
) -> None:
    """The runtime admits its own OWNER_ADDR alone, so another --owner reverts every row."""
    world.runtime.write_text(_runtime_hex(TRACKED_OWNER), encoding="utf-8")

    assert bankroll.main(world.argv(), deps=world.deps()) == bankroll.EXIT_REFUSED

    assert world.calls == [], "the refusal precedes even the launcher's status check"
    assert world.rpc_calls == [], "the refusal precedes the endpoint read"
    assert not world.out.exists(), "the refusal did not create a run directory"
    refusal = capsys.readouterr().err
    assert TRACKED_OWNER.lower() in refusal, "the refusal names the owner the runtime declares"
    assert OWNER in refusal, "the refusal names the owner the run was given"


def test_the_owner_is_matched_through_another_hex_spelling(world: World) -> None:
    """Hex case, the optional 0x prefix, and whitespace carry no identity."""
    world.runtime.write_text(f"\n 0X{_runtime_hex(TRACKED_OWNER)[2:].upper()}\n", encoding="utf-8")

    record = _run(world, "--owner", TRACKED_OWNER.lower())

    assert record.outcome == bankroll.OUTCOME_COMPLETE
    assert bankroll.runtime_owner(world.runtime, release_id="cmd_executor/2") == (
        TRACKED_OWNER.lower()
    )


def test_an_unsupported_release_id_is_refused(
    world: World, capsys: pytest.CaptureFixture[str]
) -> None:
    """Only the release whose immutable layout this runner knows is accepted."""
    assert bankroll.main(world.argv("--release-id", "cmd_executor/7"), deps=world.deps()) == (
        bankroll.EXIT_REFUSED
    )

    assert world.calls == []
    assert world.rpc_calls == []
    assert not world.out.exists()
    refusal = capsys.readouterr().err
    assert "unsupported release id" in refusal
    assert "cmd_executor/2" in refusal, "the refusal names the layout it can read"


@pytest.mark.parametrize(
    "body",
    [
        "0x60006000,not-hex\n",
        "0x",
        "",
    ],
)
def test_a_runtime_that_is_not_hex_is_refused(world: World, *, body: str) -> None:
    """A body this runner cannot decode is no runtime at all."""
    world.runtime.write_text(body, encoding="utf-8")

    assert bankroll.main(world.argv(), deps=world.deps()) == bankroll.EXIT_REFUSED
    assert world.calls == []
    assert world.rpc_calls == []
    assert not world.out.exists()


def test_a_runtime_too_short_to_hold_its_tail_is_refused(world: World) -> None:
    """200 bytes cannot carry a seven-word tail, so there is no owner word to read."""
    world.runtime.write_text("0x" + "00" * 200, encoding="utf-8")

    assert bankroll.main(world.argv(), deps=world.deps()) == bankroll.EXIT_REFUSED
    assert world.calls == []
    assert world.rpc_calls == []
    assert not world.out.exists()


def test_a_runtime_whose_owner_word_has_nonzero_high_bytes_is_refused(world: World) -> None:
    """An address immutable is left-padded in its word; code bytes there are no address.

    The unlinked compiler artifact ends in its code section, so its last 20 bytes
    spell a plausible-looking owner — which is exactly why they are refused.
    """
    world.runtime.write_text("0x6000" + "ff" * 12 + OWNER[2:] + "ab" * (32 * 6), encoding="utf-8")

    assert bankroll.main(world.argv(), deps=world.deps()) == bankroll.EXIT_REFUSED
    assert world.calls == []
    assert world.rpc_calls == []
    assert not world.out.exists()


def test_the_tracked_runtime_states_its_deployed_owner() -> None:
    """The artifact the bot injects parses to the owner its deployment baked in."""
    assert bankroll.runtime_owner(TRACKED_RUNTIME, release_id="cmd_executor/2") == (
        TRACKED_OWNER.lower()
    )


def test_an_unusable_socket_path_is_refused(world: World) -> None:
    """A path the kernel cannot bind must fail before a bot is started on it."""
    deep = world.root.joinpath("d" * 60, "e" * 60)

    assert bankroll.main(world.argv("--output-dir", str(deep)), deps=world.deps()) == (
        bankroll.EXIT_REFUSED
    )
    assert "start" not in world.steps()


@pytest.mark.parametrize(
    "extra",
    [
        ("--owner", "0xnot-an-address"),
        ("--injected-address", "22" * 20),
        ("--fixed-input", "0"),
        ("--fixed-input", "-1"),
        ("--chain-id", "0"),
        ("--ready-timeout", "0"),
    ],
)
def test_an_unusable_argument_is_refused(world: World, *, extra: tuple[str, ...]) -> None:
    """Every argument that could not produce an auditable run is refused."""
    assert bankroll.main(world.argv(*extra), deps=world.deps()) == bankroll.EXIT_REFUSED
    assert "start" not in world.steps()


def test_an_unknown_capital_spelling_is_refused_by_the_parser(world: World) -> None:
    """The postures are a closed set: a near-miss spelling never reaches the bot."""
    with pytest.raises(SystemExit) as refusal:
        bankroll.main(world.argv("--capital", "self-funded"), deps=world.deps())
    assert refusal.value.code == bankroll.EXIT_REFUSED


def test_the_runner_prints_a_summary_and_returns_the_run_status(
    world: World, capsys: pytest.CaptureFixture[str]
) -> None:
    """The operator sees the outcome and the per-posture verdict in one screen."""
    assert bankroll.main(world.argv(), deps=world.deps()) == bankroll.EXIT_COMPLETE

    captured = capsys.readouterr()
    assert "outcome=complete" in captured.out
    assert "self_fund expect=executed exit=0 outcome=complete" in captured.out
    assert "zero_balance expect=insolvent exit=0 outcome=complete" in captured.out
    assert "cleanup verified=True" in captured.out
