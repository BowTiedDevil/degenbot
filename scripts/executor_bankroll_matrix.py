#!/usr/bin/env python3
"""Run the three-hop execution matrix under every capital posture, in one command.

One command owns the whole session: it refuses to work on top of a live or
half-used run, launches the bot through the canonical ``run_bot.sh`` launcher,
injects ONE registration set, probes that same set under both capital postures
through ``scripts/executor_three_hop_matrix.py``, archives every artifact, and
stops the bot through the launcher's own ``stop`` — always, on every exit path.

The command writes only inside ``--output-dir`` (plus the launcher's own
``logs/bot_run.log``, which it archives) so a run is one directory:

    metadata.json            the reproducibility facts, written BEFORE the first probe
    run_metadata.json        command return codes, per-mode outcomes, cleanup verdict
    registrations.json       the ONE injected registration set both probes used
    report_<capital>.json    one fail-closed matrix report per selected posture
    matrix_inject.log        the inject command's own output
    matrix_probe_<capital>.log
    launcher.log             every ``run_bot.sh`` invocation and its return code
    bot_run.log              the bot's own launcher log, archived after stop

The capital postures and their expected outcomes are fixed by the postures
themselves: ``self_fund`` must execute every row (``--expect executed``) and
``zero_balance`` expects every row to be a typed capital-policy refusal
(``--expect insolvent``). That expectation is about which rows the run may see,
not a promise about any one market — a zero-capital row that does execute is an
execution, and therefore a policy mismatch for that posture. A selection of one
posture runs only that posture, with its own expectation.

The runtime artifact under test must state the owner named by ``--owner``: the
``cmd_executor/2`` immutable tail is decoded and its OWNER_ADDR word compared,
so a run the runtime would refuse fails here instead of reverting every row. A
``--release-id`` this runner does not know the immutable layout of is refused
rather than guessed.

No key material is read, accepted, or printed: the launcher inherits the normal
environment, and the only environment this runner sets is the executor-injection
set the matrix probe needs. Captured command output is scrubbed of RPC URIs
before it is written to a log or a record, and no command's environment is ever
dumped.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import signal
import subprocess  # ruff: ignore[suspicious-subprocess-import] - one explicit argv, never a shell
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence

ROOT = Path(__file__).resolve().parents[1]

#: The canonical launcher and the matrix client this runner drives. Both are
#: consumed as-is; the runner never reimplements their process model.
LAUNCHER_PATH = ROOT / "run_bot.sh"
MATRIX_SCRIPT_PATH = ROOT / "scripts" / "executor_three_hop_matrix.py"
MANIFEST_PATH = ROOT / "scripts" / "executor_three_hop_matrix.json"

#: The launcher's own log and pidfile, relative to the repository root (the
#: launcher owns both paths). The log is archived; the pidfile is only read to
#: verify the session really ended.
BOT_LOG_PATH = ROOT / "logs" / "bot_run.log"
BOT_PIDFILE_PATH = ROOT / "logs" / "bot_run.pid"

#: The operator socket name inside the run's output directory. A fresh
#: directory per run makes the socket fresh, so a stale socket from somebody
#: else's session can never be dialed.
SOCKET_NAME = "operator.sock"

#: ``sun_path`` holds 108 bytes including the terminating NUL.
UNIX_SOCKET_PATH_MAX = 107

METADATA_NAME = "metadata.json"
RUN_METADATA_NAME = "run_metadata.json"
REGISTRATIONS_NAME = "registrations.json"
INJECT_LOG_NAME = "matrix_inject.log"
LAUNCHER_LOG_NAME = "launcher.log"
BOT_LOG_NAME = "bot_run.log"

CAPITAL_SELF_FUND = "self_fund"
CAPITAL_ZERO_BALANCE = "zero_balance"
CAPITAL_BOTH = "both"
CAPITAL_SELECTIONS = (CAPITAL_BOTH, CAPITAL_SELF_FUND, CAPITAL_ZERO_BALANCE)

#: One posture, one expected outcome. ``self_fund`` must execute every row;
#: ``zero_balance`` strips the executor's own capital, so a row that cannot get
#: in without capital is expected to be a typed capital-policy refusal. The
#: expectation describes which rows the run may see, not a promise about any one
#: market: a zero-capital row that does execute is an execution, and therefore a
#: policy mismatch for that posture rather than a success.
EXPECTATION_BY_CAPITAL = {
    CAPITAL_SELF_FUND: "executed",
    CAPITAL_ZERO_BALANCE: "insolvent",
}

#: Default probe amount (wei): the value the operator wire has always fallen
#: back to, stated explicitly here so a report names the vector it probed.
DEFAULT_FIXED_INPUT = 10**15
DEFAULT_CHAIN_ID = 1
DEFAULT_RELEASE_ID = "cmd_executor/2"
DEFAULT_READY_TIMEOUT = 120.0

#: A 32-byte immutable word, and the 20 bytes of an ``address`` inside one:
#: Vyper appends the immutables after the runtime, one word per immutable, and
#: an address occupies its word's LOW bytes.
WORD_BYTES = 32
ADDRESS_BYTES = 20

#: The immutable words each supported release appends, by ``--release-id``.
#: ``cmd_executor.immutables.json`` orders OWNER_ADDR first, then WETH_ADDR,
#: POOL_MANAGER_ADDR, USDC_ADDR, USDT_ADDR, WETH_DELTA_SLOT,
#: NATIVE_DELTA_SLOT. A release with no entry here is refused: reading an
#: unlisted layout would validate whatever bytes happened to land there.
IMMUTABLE_WORDS_BY_RELEASE = {DEFAULT_RELEASE_ID: 7}

METADATA_SCHEMA_VERSION = 1
RUN_SCHEMA_VERSION = 1

#: Run outcomes. ``stopped`` is the operator's own request (SIGINT/SIGTERM),
#: never a misclassified probe failure.
OUTCOME_COMPLETE = "complete"
OUTCOME_MISMATCHED = "mismatched"
OUTCOME_INCOMPLETE = "incomplete"
OUTCOME_STOPPED = "stopped"
OUTCOME_FAILED = "failed"
OUTCOME_REFUSED = "refused"

EXIT_COMPLETE = 0
EXIT_FAILED = 1
EXIT_REFUSED = 2
EXIT_STOPPED = 130

#: The executor-injection environment the matrix probe needs: the bot must
#: inject the runtime under test as code at the operator's own address, and the
#: probe must see every row instead of a profit floor filtering it out.
LAUNCH_ENV_KEYS = (
    "DEGENBOT_INJECT_EXECUTOR_CODE",
    "EXECUTOR_OWNER_ADDRESS",
    "INJECTED_EXECUTOR_ADDRESS",
    "EXECUTOR_RUNTIME",
    "DEGENBOT_MIN_PROFIT_WEI",
    "DEGENBOT_MIN_PROFIT_MARGIN_BPS",
)

#: The bot's own argv for an operator-only dry run: no discovery sweep (only
#: the operator registers paths) and no tracked snapshot (pools come from
#: current chain state). ``--live`` is never passed — the probe executor has no
#: submit lane, and this runner must not reach one.
LAUNCH_ARGS = ("--skip-discovery", "--sparse-registration")

#: A URI is the one value in captured output that must never land in a log.
_URI_PATTERN = re.compile(r"\b(?:https?|wss?|ipc)://\S+")

_ADDRESS_PATTERN = re.compile(r"0x[0-9a-fA-F]{40}")

#: A JSON-RPC quantity: ``0x`` and hex digits only (no sign, no separators).
_QUANTITY_PATTERN = re.compile(r"0x[0-9a-fA-F]+")

#: A runtime artifact's body once its whitespace and optional ``0x`` prefix are
#: gone: hex digits only, so a stray byte or a comment word is a refusal.
_HEX_BODY_PATTERN = re.compile(r"[0-9a-fA-F]+")


class RefusalError(Exception):
    """The run was refused before the bot was launched."""


class CommandFailed(Exception):
    """A lifecycle command failed; the caller stops the session and reports."""


@dataclass(frozen=True)
class CommandResult:
    """One finished command: its argv, its real return code, its scrubbed output.

    Attributes:
        argv: The exact argv that ran.
        returncode: The command's own exit status.
        output: Its combined stdout/stderr, scrubbed of RPC URIs.

    """

    argv: tuple[str, ...]
    returncode: int
    output: str = ""


@dataclass(frozen=True)
class CommandStep:
    """One lifecycle command to run and archive.

    Attributes:
        label: The step's name, as recorded in the run's command list.
        argv: The command to run.
        env: Environment overrides merged over the inherited environment.
        log_path: Where the command's own output is archived, when it is.

    """

    label: str
    argv: Sequence[str]
    env: Mapping[str, str] | None = None
    log_path: Path | None = None


@dataclass(frozen=True)
class MatrixConfig:
    """Everything one matrix run is parameterized by.

    Attributes:
        owner: The executor owner address (gas funding, injected-code owner).
        injected_address: The address the runtime is injected at.
        runtime: The runtime bytecode artifact under test.
        output_dir: The run's own directory; refused unless absent or empty.
        capital: ``both``, ``self_fund``, or ``zero_balance``.
        fixed_input: The probe's fixed swap input (wei), positive.
        chain_id: The chain the run must be on.
        release_id: The executor release identity under test.
        ready_timeout: How long the registration crawl may take to become ready.

    """

    owner: str
    injected_address: str
    runtime: Path
    output_dir: Path
    capital: str = CAPITAL_BOTH
    fixed_input: int = DEFAULT_FIXED_INPUT
    chain_id: int = DEFAULT_CHAIN_ID
    release_id: str = DEFAULT_RELEASE_ID
    ready_timeout: float = DEFAULT_READY_TIMEOUT

    @property
    def socket_path(self) -> Path:
        """The operator socket this run uses.

        Returns:
            The socket path inside the run's output directory.

        """
        return self.output_dir / SOCKET_NAME

    @property
    def capitals(self) -> tuple[str, ...]:
        """The capital postures this run selects, in probe order.

        Returns:
            The selected postures: both, or the one that was named.

        """
        if self.capital == CAPITAL_BOTH:
            return (CAPITAL_SELF_FUND, CAPITAL_ZERO_BALANCE)
        return (self.capital,)

    def artifact(self, name: str) -> Path:
        """Return the path of one archived artifact.

        Args:
            name: The artifact's file name.

        Returns:
            That artifact's path inside the run's output directory.

        """
        return self.output_dir / name

    def report_path(self, capital: str) -> Path:
        """Return one posture's report path.

        Args:
            capital: The posture whose report is named.

        Returns:
            The report path for that posture.

        """
        return self.artifact(report_name(capital))

    def probe_log_path(self, capital: str) -> Path:
        """Return one posture's probe log path.

        Args:
            capital: The posture whose probe log is named.

        Returns:
            The probe log path for that posture.

        """
        return self.artifact(probe_log_name(capital))


def report_name(capital: str) -> str:
    """Return the report file name for one capital posture.

    Args:
        capital: The posture whose report is named.

    Returns:
        The report's file name.

    """
    return f"report_{capital}.json"


def probe_log_name(capital: str) -> str:
    """Return the probe log file name for one capital posture.

    Args:
        capital: The posture whose probe log is named.

    Returns:
        The log's file name.

    """
    return f"matrix_probe_{capital}.log"


@dataclass(frozen=True)
class RunnerDeps:
    """The outside world this runner touches, injected so tests need none of it.

    Attributes:
        run: Runs one command (``env`` values are overrides merged over the
            inherited environment) and returns its :class:`CommandResult`.
        rpc: Sends one JSON-RPC method name and returns its ``result``.
        bot_log: The launcher's own log, archived after stop.
        bot_pidfile: The launcher's pidfile, read only to verify the stop.
        launcher: The canonical ``run_bot.sh`` path.
        matrix_script: The matrix client path.
        manifest: The matrix manifest whose hash the metadata records.
        python: The interpreter that runs the matrix client.
        sleep: The pacing function (unused by the default flow, kept injectable).

    """

    run: Callable[..., CommandResult]
    rpc: Callable[[str], object]
    bot_log: Path = BOT_LOG_PATH
    bot_pidfile: Path = BOT_PIDFILE_PATH
    launcher: Path = LAUNCHER_PATH
    matrix_script: Path = MATRIX_SCRIPT_PATH
    manifest: Path = MANIFEST_PATH
    python: str = field(default_factory=lambda: sys.executable)
    sleep: Callable[[float], None] = time.sleep


@dataclass
class StopRequest:
    """An operator stop (SIGINT/SIGTERM) turned into run state, never a traceback.

    The handler only records the request: the runner finishes the command it is
    in, then takes the ``stopped`` path so cleanup runs unconditionally.
    """

    signal_name: str | None = None
    previous: dict[int, Any] = field(default_factory=dict)

    @property
    def requested(self) -> bool:
        """Whether a stop was requested.

        Returns:
            True once a signal or an explicit request was recorded.

        """
        return self.signal_name is not None

    def request(self, signum: int | None = None) -> None:
        """Record a stop request, keeping the first one.

        Args:
            signum: The signal that asked for the stop, or ``None`` for an
                explicit request.

        """
        if self.signal_name is not None:
            return
        self.signal_name = "stop-request" if signum is None else signal.Signals(signum).name

    def install(self) -> StopRequest:
        """Install the handlers for the two operator stop signals.

        Returns:
            This request, so it can be installed where it is created.

        """
        for signum in (signal.SIGINT, signal.SIGTERM):
            self.previous[int(signum)] = signal.signal(signum, self._handle)
        return self

    def restore(self) -> None:
        """Put the previous handlers back, so a caller keeps its own signals."""
        for signum, handler in self.previous.items():
            signal.signal(signum, handler)
        self.previous.clear()

    def _handle(self, signum: int, _frame: object) -> None:
        """Record one stop signal (the handler ``signal.signal`` calls).

        Args:
            signum: The delivered signal number.
            _frame: The interrupted frame (unused).

        """
        self.request(signum)


@dataclass
class RunRecord:
    """What a run did, accumulated as it happens and written at the end.

    Attributes:
        config: The run's configuration.
        metadata: The reproducibility facts, written before the first probe.
        metadata_written: Whether ``metadata.json`` was written.
        launched: Whether the launcher's ``start`` was invoked (so ``stop`` is ours).
        modes: One record per selected capital posture.
        commands: One record per lifecycle command, in order.
        cleanup: The stop/archive/verify verdict.
        outcome: The final run outcome (``OUTCOME_*``).
        error: The first failure's message, if any.

    """

    config: MatrixConfig
    metadata: dict[str, Any] | None = None
    metadata_written: bool = False
    launched: bool = False
    modes: list[dict[str, Any]] = field(default_factory=list)
    commands: list[dict[str, Any]] = field(default_factory=list)
    cleanup: dict[str, Any] = field(default_factory=dict)
    outcome: str | None = None
    error: str | None = None


def scrub(text: str) -> str:
    """Redact every URI in captured text.

    An RPC endpoint is the one value here that must never reach a log or a
    record: a URL can carry a provider token.

    Args:
        text: The captured output.

    Returns:
        The same text with every URI replaced by ``<rpc-uri>``.

    """
    return _URI_PATTERN.sub("<rpc-uri>", text)


def validate_address(value: str, *, label: str) -> str:
    """Refuse anything that is not one 20-byte hex address.

    Args:
        value: The address as given on the command line.
        label: The flag name to name in the refusal.

    Returns:
        The address, unchanged.

    Raises:
        RefusalError: If the value is not ``0x`` plus 40 hex digits.

    """
    if _ADDRESS_PATTERN.fullmatch(value) is None:
        msg = f"{label} must be a 0x-prefixed 20-byte hex address"
        raise RefusalError(msg)
    return value


def validate_fixed_input(value: object) -> int:
    """Refuse anything that is not a positive, non-boolean integer amount.

    Args:
        value: The fixed probe input as given.

    Returns:
        The amount.

    Raises:
        RefusalError: If the value is not a positive integer.

    """
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        msg = f"fixed input must be a positive integer amount of wei, got {value!r}"
        raise RefusalError(msg)
    return value


def validate_socket_path(path: Path) -> Path:
    """Refuse an operator socket whose path cannot be bound.

    A Unix socket address is 108 bytes including its NUL, so a deeper output
    directory makes the launch fail only after the bot is already running.

    Args:
        path: The socket path the run would bind.

    Returns:
        The socket path.

    Raises:
        RefusalError: If the encoded path is longer than ``sun_path`` allows.

    """
    encoded = os.fsencode(str(path))
    if len(encoded) > UNIX_SOCKET_PATH_MAX:
        msg = (
            f"operator socket path is {len(encoded)} bytes, longer than the "
            f"{UNIX_SOCKET_PATH_MAX}-byte Unix limit: choose a shorter --output-dir"
        )
        raise RefusalError(msg)
    return path


def decode_runtime(path: Path) -> bytes:
    """Decode a runtime artifact's bytecode.

    The accepted body is hex digits with an optional ``0x`` prefix and any
    whitespace between them — the shape ``contracts/recompile.py`` writes and
    the bot loads.

    Args:
        path: The runtime artifact.

    Returns:
        The decoded bytecode, immutable tail included.

    Raises:
        RefusalError: If the artifact is missing, unreadable, or not hex.

    """
    if not path.is_file():
        msg = f"runtime artifact is missing: {path}"
        raise RefusalError(msg)
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        msg = f"runtime artifact {path} could not be read: {type(exc).__name__}"
        raise RefusalError(msg) from None
    body = "".join(text.split())
    if body[:2].lower() == "0x":
        body = body[2:]
    if _HEX_BODY_PATTERN.fullmatch(body) is None:
        msg = f"runtime artifact {path} is not hex bytecode (hex digits, optional 0x prefix)"
        raise RefusalError(msg)
    try:
        return bytes.fromhex(body)
    except ValueError:
        msg = f"runtime artifact {path} is not hex bytecode (an odd number of hex digits)"
        raise RefusalError(msg) from None


def runtime_owner(path: Path, *, release_id: str) -> str:
    """Read the owner baked into a runtime artifact's immutable tail.

    The deployed runtime appends one 32-byte word per immutable AFTER the code
    section and its CBOR metadata, and ``OWNER_ADDR`` is word 0 of that tail, an
    ``address`` left-padded inside the word. So the owner is read from the LAST
    tail-sized slice of the artifact and nowhere else: an address-shaped run of
    bytes in the code section is a coincidence, and matching one would validate
    a runtime that admits a different sender.

    ``execute()`` admits only the runtime's own ``OWNER_ADDR``, which makes this
    the one fact a matrix launch cannot get wrong.

    Args:
        path: The runtime artifact under test.
        release_id: The release whose immutable layout applies.

    Returns:
        The artifact's owner address, lowercase (``0x`` plus 40 hex digits).

    Raises:
        RefusalError: If the release's layout is unknown, the artifact is
            unreadable, too short to hold its tail, or word 0's high bytes are
            not the zero padding an address requires.

    """
    words = IMMUTABLE_WORDS_BY_RELEASE.get(release_id)
    if words is None:
        supported = ", ".join(sorted(IMMUTABLE_WORDS_BY_RELEASE))
        msg = (
            f"unsupported release id {release_id!r}: this runner reads the immutable "
            f"tail of {supported} only, and does not guess another layout"
        )
        raise RefusalError(msg)
    tail_bytes = words * WORD_BYTES
    data = decode_runtime(path)
    if len(data) < tail_bytes:
        msg = (
            f"runtime artifact {path} decodes to {len(data)} bytes, shorter than the "
            f"{tail_bytes}-byte {release_id} immutable tail"
        )
        raise RefusalError(msg)
    padding = WORD_BYTES - ADDRESS_BYTES
    word = data[-tail_bytes:][:WORD_BYTES]
    if any(word[:padding]):
        msg = (
            f"runtime artifact {path} immutable word 0 is {word.hex()}, whose first "
            f"{padding} bytes are not zero: {release_id} carries no address there"
        )
        raise RefusalError(msg)
    return "0x" + word[padding:].hex()


def validate_runtime_owner(config: MatrixConfig) -> str:
    """Refuse a runtime whose own immutable owner is not ``--owner``.

    Args:
        config: The run's configuration.

    Returns:
        The owner the runtime admits, lowercase.

    Raises:
        RefusalError: If the release layout is unsupported, the artifact cannot
            be read as the release's runtime, or its owner is another address.

    """
    owner = runtime_owner(config.runtime, release_id=config.release_id)
    if owner != config.owner.lower():
        msg = (
            f"runtime owner {owner} does not match --owner {config.owner}: "
            f"the runtime admits its OWNER_ADDR alone, so no row of this run "
            f"could execute"
        )
        raise RefusalError(msg)
    return owner


def validate_config(config: MatrixConfig) -> MatrixConfig:
    """Refuse a configuration that could not produce an auditable run.

    Args:
        config: The run's configuration as given.

    Returns:
        The validated configuration.

    Raises:
        RefusalError: If an address, the fixed input, a chain id, the ready
            timeout, the capital selection, the release/runtime owner, or the
            socket path is unusable.

    """
    validate_address(config.owner, label="--owner")
    validate_address(config.injected_address, label="--injected-address")
    validate_fixed_input(config.fixed_input)
    if config.capital not in CAPITAL_SELECTIONS:
        expected = "|".join(CAPITAL_SELECTIONS)
        msg = f"unknown capital selection {config.capital!r} (expected {expected})"
        raise RefusalError(msg)
    chain_id = config.chain_id
    if not isinstance(chain_id, int) or isinstance(chain_id, bool) or chain_id <= 0:
        msg = f"chain id must be a positive integer, got {chain_id!r}"
        raise RefusalError(msg)
    if not config.ready_timeout > 0:
        msg = f"ready timeout must be positive, got {config.ready_timeout!r}"
        raise RefusalError(msg)
    validate_runtime_owner(config)
    validate_socket_path(config.socket_path)
    return config


def prepare_output_dir(output_dir: Path) -> Path:
    """Refuse a used output directory, then create an empty one.

    A shared directory would mix one run's artifacts with another's, and a
    report read later would have no reproducible home.

    Args:
        output_dir: The run's directory.

    Returns:
        The empty directory.

    Raises:
        RefusalError: If the path is not a directory, or is a non-empty one.

    """
    if output_dir.exists():
        if not output_dir.is_dir():
            msg = f"output path {output_dir} is not a directory"
            raise RefusalError(msg)
        if any(output_dir.iterdir()):
            msg = f"output directory {output_dir} is not empty; refusing to mix runs"
            raise RefusalError(msg)
        return output_dir
    output_dir.mkdir(parents=True)
    return output_dir


def write_json(path: Path, document: object) -> None:
    """Write one JSON document by atomic replace.

    Args:
        path: Where the document belongs.
        document: The JSON-serializable document.

    """
    temporary = path.with_name(f"{path.name}.tmp")
    temporary.write_text(json.dumps(document, indent=2, sort_keys=False) + "\n", encoding="utf-8")
    temporary.replace(path)


def sha256_of(path: Path) -> str:
    """Return a file's SHA-256 digest.

    Args:
        path: The file to hash.

    Returns:
        The lowercase hex digest.

    Raises:
        RefusalError: If the file is absent or empty — an empty artifact would
            hash to the digest of nothing.

    """
    if not path.is_file():
        msg = f"artifact is missing: {path}"
        raise RefusalError(msg)
    data = path.read_bytes()
    if not data:
        msg = f"artifact is empty: {path}"
        raise RefusalError(msg)
    return hashlib.sha256(data).hexdigest()


def json_rpc_quantity(rpc: Callable[[str], object], method: str) -> int:
    """Read one JSON-RPC quantity (a ``0x``-prefixed hex string) as an integer.

    Args:
        rpc: The transport: one method name in, the response's ``result`` out.
        method: The JSON-RPC method to call.

    Returns:
        The quantity.

    Raises:
        RefusalError: If the transport fails, or the result is not a
            non-negative hex quantity.

    """
    try:
        result = rpc(method)
    except Exception as exc:  # ruff: ignore[blind-except] - an unreachable endpoint is a refusal, never a guess
        msg = f"{method} failed: {scrub(f'{type(exc).__name__}: {exc}')}"
        raise RefusalError(msg) from None
    if not isinstance(result, str) or _QUANTITY_PATTERN.fullmatch(result) is None:
        msg = f"{method} returned no hex quantity"
        raise RefusalError(msg)
    return int(result, 16)


def read_chain_head(rpc: Callable[[str], object], *, chain_id: int) -> tuple[int, int]:
    """Read the pre-launch chain id and head block, refusing any mismatch.

    Args:
        rpc: The JSON-RPC transport.
        chain_id: The chain the run must be on.

    Returns:
        The ``(chain_id, block_number)`` pair the endpoint reported.

    Raises:
        RefusalError: If the endpoint reports another chain, or no usable block.

    """
    reported = json_rpc_quantity(rpc, "eth_chainId")
    if reported != chain_id:
        msg = f"endpoint serves chain {reported}, but the run declared chain {chain_id}"
        raise RefusalError(msg)
    block = json_rpc_quantity(rpc, "eth_blockNumber")
    if block <= 0:
        msg = "endpoint reported no head block"
        raise RefusalError(msg)
    return reported, block


def read_source_state(run: Callable[..., CommandResult]) -> tuple[str, bool]:
    """Read the source commit and whether the tree is dirty.

    Args:
        run: The command runner.

    Returns:
        The ``(commit, dirty)`` pair from ``git`` itself.

    Raises:
        RefusalError: If ``git`` cannot report the tree.

    """
    head = run(["git", "rev-parse", "HEAD"])
    commit = head.output.strip().splitlines()[-1] if head.output.strip() else ""
    if head.returncode != 0 or not commit:
        msg = f"git rev-parse HEAD failed (exit {head.returncode})"
        raise RefusalError(msg)
    status = run(["git", "status", "--porcelain"])
    if status.returncode != 0:
        msg = f"git status --porcelain failed (exit {status.returncode})"
        raise RefusalError(msg)
    return commit, bool(status.output.strip())


def launcher_is_running(result: CommandResult) -> bool | None:
    """Read the launcher's ``status`` answer.

    ``run_bot.sh status`` exits 0 whether or not a bot is live, so the answer
    is its text, and an unreadable answer is never read as "nobody is running".

    Args:
        result: The ``status`` invocation.

    Returns:
        True when a bot is live, False when the launcher says none is, and
        ``None`` when the answer cannot be read.

    """
    if result.returncode != 0:
        return None
    if "not running" in result.output:
        return False
    return "running" in result.output


def launch_environment(config: MatrixConfig) -> dict[str, str]:
    """Build the executor-injection environment for the bot.

    Args:
        config: The run's configuration.

    Returns:
        The environment overrides: injection on, the owner and injected
        addresses, the runtime under test, and a zero profit floor so the probe
        sees every row. Nothing here is a secret, and no endpoint is set — the
        launcher resolves its own.

    """
    return {
        "DEGENBOT_INJECT_EXECUTOR_CODE": "1",
        "EXECUTOR_OWNER_ADDRESS": config.owner,
        "INJECTED_EXECUTOR_ADDRESS": config.injected_address,
        "EXECUTOR_RUNTIME": str(config.runtime),
        "DEGENBOT_MIN_PROFIT_WEI": "0",
        "DEGENBOT_MIN_PROFIT_MARGIN_BPS": "0",
    }


def launch_argv(deps: RunnerDeps, config: MatrixConfig) -> list[str]:
    """Build the launcher's fresh-session argv.

    Args:
        deps: The injected dependencies (the launcher path).
        config: The run's configuration.

    Returns:
        The argv: ``run_bot.sh start``, ``--``, then the bot's own dry-run args
        including this run's own socket. ``--live`` is never among them.

    """
    return [
        str(deps.launcher),
        "start",
        "--",
        *LAUNCH_ARGS,
        "--operator-socket",
        str(config.socket_path),
    ]


def inject_argv(deps: RunnerDeps, config: MatrixConfig) -> list[str]:
    """Build the one registration-set injection's argv.

    Args:
        deps: The injected dependencies.
        config: The run's configuration.

    Returns:
        The argv for ``executor_three_hop_matrix.py inject``.

    """
    return [
        deps.python,
        str(deps.matrix_script),
        "inject",
        "--socket",
        str(config.socket_path),
        "--output",
        str(config.artifact(REGISTRATIONS_NAME)),
        "--ready-timeout",
        str(config.ready_timeout),
    ]


def probe_argv(deps: RunnerDeps, config: MatrixConfig, capital: str) -> list[str]:
    """Build one posture's probe argv.

    Args:
        deps: The injected dependencies.
        config: The run's configuration.
        capital: The posture to probe.

    Returns:
        The argv for ``executor_three_hop_matrix.py probe``, carrying this
        run's own registrations, expectation, fixed input, and metadata file.

    """
    return [
        deps.python,
        str(deps.matrix_script),
        "probe",
        "--socket",
        str(config.socket_path),
        "--registrations",
        str(config.artifact(REGISTRATIONS_NAME)),
        "--output",
        str(config.report_path(capital)),
        "--expect",
        EXPECTATION_BY_CAPITAL[capital],
        "--capital",
        capital,
        "--fixed-input",
        str(config.fixed_input),
        "--metadata-file",
        str(config.artifact(METADATA_NAME)),
    ]


def artifact_names(config: MatrixConfig) -> dict[str, Any]:
    """Name every artifact this run will write.

    Args:
        config: The run's configuration.

    Returns:
        The artifact names, by role, so the metadata names the evidence before
        any of it exists.

    """
    return {
        "metadata": METADATA_NAME,
        "run_metadata": RUN_METADATA_NAME,
        "registrations": REGISTRATIONS_NAME,
        "reports": {capital: report_name(capital) for capital in config.capitals},
        "matrix_logs": {
            "inject": INJECT_LOG_NAME,
            **{capital: probe_log_name(capital) for capital in config.capitals},
        },
        "launcher_log": LAUNCHER_LOG_NAME,
        "bot_log": BOT_LOG_NAME,
    }


def build_metadata(
    config: MatrixConfig, deps: RunnerDeps, facts: Mapping[str, Any]
) -> dict[str, Any]:
    """State everything a later reader needs to reproduce this run.

    Args:
        config: The run's configuration.
        deps: The injected dependencies (the manifest to hash).
        facts: What the preflight established: the pre-launch ``block_number``,
            the ``source_commit``, and whether the tree is ``source_dirty``.

    Returns:
        The metadata document handed to every probe and archived as
        ``metadata.json``.

    """
    return {
        "schema_version": METADATA_SCHEMA_VERSION,
        "chain_id": config.chain_id,
        "block_number": facts["block_number"],
        "release_id": config.release_id,
        "runtime": str(config.runtime),
        "runtime_sha256": sha256_of(config.runtime),
        "matrix_manifest": str(deps.manifest),
        "matrix_manifest_sha256": sha256_of(deps.manifest),
        "source_commit": facts["source_commit"],
        "source_dirty": facts["source_dirty"],
        "fixed_input": config.fixed_input,
        "capital_selection": config.capital,
        "capitals": list(config.capitals),
        "expectations": {capital: EXPECTATION_BY_CAPITAL[capital] for capital in config.capitals},
        "owner": config.owner,
        "injected_address": config.injected_address,
        "output_dir": str(config.output_dir),
        "socket": str(config.socket_path),
        "artifacts": artifact_names(config),
    }


def record_command(record: RunRecord, deps: RunnerDeps, step: CommandStep) -> CommandResult:
    """Run one lifecycle command, archive its output, and record its real code.

    Args:
        record: The run record to append to.
        deps: The injected dependencies (the command runner).
        step: The command to run, its name, its environment, and its log.

    Returns:
        The finished command.

    """
    argv = [str(item) for item in step.argv]
    code: int | None
    try:
        result = deps.run(argv, env=step.env)
        code = result.returncode
        output = scrub(result.output or "")
    except Exception as exc:  # ruff: ignore[blind-except] - a command that could not run is an outcome
        code = None
        output = scrub(f"{type(exc).__name__}: {exc}")
    record.commands.append({
        "step": step.label,
        "argv": argv,
        "returncode": code,
        "log": step.log_path.name if step.log_path is not None else None,
    })
    if step.log_path is not None:
        _append_log(step.log_path, step.label, argv, output, code)
    return CommandResult(
        argv=tuple(argv),
        returncode=-1 if code is None else code,
        output=output,
    )


def _append_log(
    log_path: Path,
    label: str,
    argv: Sequence[str],
    output: str,
    code: int | None,
) -> None:
    """Append one command's argv, output, and real exit code to its log.

    Args:
        log_path: The log to append to (already inside the run's directory).
        label: The step's name.
        argv: The command that ran.
        output: Its scrubbed output.
        code: Its exit code, or ``None`` when the command never ran.

    """
    header = f"$ {' '.join(str(item) for item in argv)}\n"
    body = output if output.endswith("\n") or not output else f"{output}\n"
    with log_path.open("a", encoding="utf-8") as handle:
        handle.write(f"{header}{body}\n[bankroll] {label} exit={code}\n")


def read_report_summary(path: Path) -> dict[str, Any] | None:
    """Read one report's summary, if the report is there and parseable.

    Args:
        path: The report path.

    Returns:
        The summary object, or ``None`` when the report cannot be read.

    """
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    summary = document.get("summary") if isinstance(document, dict) else None
    return summary if isinstance(summary, dict) else None


def is_count(value: object) -> bool:
    """Whether a report counter is a JSON integer.

    Args:
        value: The decoded counter.

    Returns:
        True for an ``int`` that is not a ``bool`` — a JSON ``true`` is not a
        count, and neither is an absent key.

    """
    return isinstance(value, int) and not isinstance(value, bool)


def summary_counts(summary: dict[str, Any]) -> tuple[int, int, int, int] | None:
    """Read a report summary's four counters, refusing a thin one.

    A summary is evidence only when it states its arithmetic: a document that
    says ``complete: true`` without the expected/completed/matched numbers
    cannot distinguish full coverage from an empty report.

    Args:
        summary: The report's summary object.

    Returns:
        ``(expected, matched, missing, unexpected)``, or ``None`` when any
        counter is absent or is not an integer.

    """
    counters = (
        summary.get("expected"),
        summary.get("matched"),
        summary.get("missing"),
        summary.get("unexpected"),
    )
    if not all(is_count(counter) for counter in counters):
        return None
    expected, matched, missing, unexpected = counters
    return expected, matched, missing, unexpected


def mode_outcome(returncode: int, summary: dict[str, Any] | None) -> str:
    """Classify one posture's probe from its real exit code and its report.

    ``interrupted`` and ``complete`` are read as exact booleans: a summary that
    omits ``interrupted``, spells it a string, or says ``interrupted: true`` has
    not stated a finished run, whatever its counters add up to.

    Args:
        returncode: The probe command's exit code.
        summary: The report's summary, or ``None`` when no report could be read.

    Returns:
        ``complete`` only for a zero-exit probe whose report states full,
        policy-matched coverage; otherwise the specific failure: no readable
        report at all, an incomplete one, or a complete one that did not match
        its policy.

    """
    counts = None if summary is None else summary_counts(summary)
    if summary is None or counts is None:
        return OUTCOME_FAILED
    expected, matched, missing, unexpected = counts
    if summary.get("interrupted") is not False or summary.get("complete") is not True or missing:
        return OUTCOME_INCOMPLETE
    if unexpected or matched != expected or expected <= 0:
        return OUTCOME_MISMATCHED
    return OUTCOME_COMPLETE if returncode == 0 else OUTCOME_FAILED


def preflight(config: MatrixConfig, deps: RunnerDeps, record: RunRecord) -> dict[str, Any]:
    """Refuse a run that cannot be reproduced, and state its facts.

    Everything here happens BEFORE the launcher is touched: a refusal leaves no
    bot to stop and nobody else's session at risk.

    Args:
        config: The run's configuration.
        deps: The injected dependencies.
        record: The run record, for the recorded commands.

    Returns:
        The metadata document.

    Raises:
        RefusalError: If a bot is already live, the output directory is used,
            the endpoint is unreachable or on another chain, or the artifacts
            under test cannot be hashed.

    """
    status = record_command(
        record,
        deps,
        CommandStep(label="status-refusal-check", argv=[str(deps.launcher), "status"]),
    )
    running = launcher_is_running(status)
    if running is None:
        msg = "cannot determine whether a bot is already running; refusing to launch over it"
        raise RefusalError(msg)
    if running:
        msg = "a bot is already running; refusing to take over somebody else's session"
        raise RefusalError(msg)
    _chain_id, block = read_chain_head(deps.rpc, chain_id=config.chain_id)
    commit, dirty = read_source_state(deps.run)
    # Every fact is gathered (and every artifact hashed) before the run
    # directory exists, so a refusal leaves no directory pretending a run began.
    metadata = build_metadata(
        config, deps, {"block_number": block, "source_commit": commit, "source_dirty": dirty}
    )
    prepare_output_dir(config.output_dir)
    return metadata


def launch(config: MatrixConfig, deps: RunnerDeps, record: RunRecord, stop: StopRequest) -> None:
    """Start one fresh bot session through the canonical launcher.

    Args:
        config: The run's configuration.
        deps: The injected dependencies.
        record: The run record.
        stop: The stop request, checked before starting.

    Raises:
        CommandFailed: If the launcher refuses or fails to start the bot.

    """
    if stop.requested:
        msg = "stop requested before launch"
        raise CommandFailed(msg)
    result = record_command(
        record,
        deps,
        CommandStep(
            label="start",
            argv=launch_argv(deps, config),
            env=launch_environment(config),
            log_path=config.artifact(LAUNCHER_LOG_NAME),
        ),
    )
    record.launched = True
    if result.returncode != 0:
        msg = f"launcher start exited {result.returncode}"
        raise CommandFailed(msg)


def inject(config: MatrixConfig, deps: RunnerDeps, record: RunRecord, stop: StopRequest) -> None:
    """Register the ONE path set every selected probe will use.

    Args:
        config: The run's configuration.
        deps: The injected dependencies.
        record: The run record.
        stop: The stop request, checked before injecting.

    Raises:
        CommandFailed: If the injection fails, is refused, or writes no
            registration set.

    """
    if stop.requested:
        msg = "stop requested before injection"
        raise CommandFailed(msg)
    result = record_command(
        record,
        deps,
        CommandStep(
            label="inject",
            argv=inject_argv(deps, config),
            log_path=config.artifact(INJECT_LOG_NAME),
        ),
    )
    if result.returncode != 0:
        msg = f"registration injection exited {result.returncode}"
        raise CommandFailed(msg)
    if not config.artifact(REGISTRATIONS_NAME).is_file():
        msg = "registration injection wrote no registration set"
        raise CommandFailed(msg)


def probe_modes(
    config: MatrixConfig, deps: RunnerDeps, record: RunRecord, stop: StopRequest
) -> None:
    """Probe the injected set under every selected posture.

    Each posture is attempted even when an earlier one completed with an
    unexpected outcome: both exit codes are real evidence, and the run's own
    status keeps the failure.

    Args:
        config: The run's configuration.
        deps: The injected dependencies.
        record: The run record.
        stop: The stop request; a requested stop skips the remaining postures.

    """
    for capital in config.capitals:
        expect = EXPECTATION_BY_CAPITAL[capital]
        if stop.requested:
            record.modes.append({
                "capital": capital,
                "expect": expect,
                "report": report_name(capital),
                "log": probe_log_name(capital),
                "exit_code": None,
                "outcome": None,
                "skipped": True,
                "summary": None,
            })
            continue
        result = record_command(
            record,
            deps,
            CommandStep(
                label=f"probe-{capital}",
                argv=probe_argv(deps, config, capital),
                log_path=config.probe_log_path(capital),
            ),
        )
        summary = read_report_summary(config.report_path(capital))
        record.modes.append({
            "capital": capital,
            "expect": expect,
            "report": report_name(capital),
            "log": probe_log_name(capital),
            "exit_code": result.returncode,
            "outcome": mode_outcome(result.returncode, summary),
            "skipped": False,
            "summary": summary,
        })


def archive_bot_log(deps: RunnerDeps, config: MatrixConfig, record: RunRecord) -> bool:
    """Archive the launcher's own bot log into the run directory.

    The copy is taken AFTER the stop, so it carries the shutdown tail, and it is
    written as text with every URI redacted: the launcher logs its resolved
    endpoints, and an endpoint can carry a provider token. Only the bot's own
    log is copied — never a command environment.

    Args:
        deps: The injected dependencies (the bot log's path).
        config: The run's configuration (the destination).
        record: The run record, for the cleanup verdict.

    Returns:
        Whether a bot log was archived.

    """
    if not deps.bot_log.is_file():
        return False
    destination = config.artifact(BOT_LOG_NAME)
    try:
        text = deps.bot_log.read_text(encoding="utf-8", errors="replace")
        destination.write_text(scrub(text), encoding="utf-8")
    except OSError as exc:
        record.cleanup["bot_log_error"] = scrub(f"{type(exc).__name__}: {exc}")
        return False
    return True


def cleanup(config: MatrixConfig, deps: RunnerDeps, record: RunRecord, stop: StopRequest) -> None:
    """Stop the bot this run started and verify nothing of it remains.

    Runs on every exit path, including a refusing argument, an injected
    failure, and an operator signal. A run that never launched stops nothing:
    the launcher's ``stop`` would kill whatever session it found. A launched
    session also owes its archived bot log: a clean stop with nothing archived
    leaves no evidence of what the session did.

    Args:
        config: The run's configuration.
        deps: The injected dependencies.
        record: The run record.
        stop: The stop request being finalized.

    """
    record.cleanup.setdefault("stop_returncode", None)
    # A refused run never created the run directory, so only a run that reached
    # its metadata write has a launcher log to append to.
    launcher_log = config.artifact(LAUNCHER_LOG_NAME) if record.metadata_written else None
    if record.launched:
        stop_result = record_command(
            record,
            deps,
            CommandStep(
                label="stop",
                argv=[str(deps.launcher), "stop"],
                log_path=launcher_log,
            ),
        )
        record.cleanup["stop_returncode"] = stop_result.returncode
        record.cleanup["stop_failed"] = stop_result.returncode != 0
    # After the stop, so the archived copy keeps the shutdown tail. Only a
    # session this run launched has a bot log to archive: a refused run must not
    # write into a directory it refused to use, and a previous run's log is not
    # this run's evidence.
    record.cleanup["archived_bot_log"] = (
        archive_bot_log(deps, config, record) if record.launched else False
    )
    socket_path = config.socket_path
    record.cleanup["socket_existed"] = socket_path.exists()
    if socket_path.exists():
        try:
            socket_path.unlink()
        except OSError as exc:
            record.cleanup["socket_error"] = scrub(f"{type(exc).__name__}: {exc}")
    status = record_command(
        record,
        deps,
        CommandStep(
            label="status-cleanup-verify",
            argv=[str(deps.launcher), "status"],
            log_path=launcher_log,
        ),
    )
    running = launcher_is_running(status)
    record.cleanup["bot_running"] = running
    record.cleanup["pidfile_present"] = deps.bot_pidfile.exists()
    record.cleanup["socket_present"] = socket_path.exists()
    record.cleanup["stop_requested"] = stop.requested
    record.cleanup["stop_signal"] = stop.signal_name
    record.cleanup["verified"] = (
        running is False
        and not record.cleanup["pidfile_present"]
        and not record.cleanup["socket_present"]
        and not record.cleanup.get("stop_failed", False)
        and (record.cleanup["archived_bot_log"] or not record.launched)
    )


def finalize_outcome(record: RunRecord, stop: StopRequest) -> str:
    """Decide the run's outcome from what actually happened.

    Args:
        record: The run record.
        stop: The stop request.

    Returns:
        The run's ``OUTCOME_*``: a cleanup that left a bot (or a pidfile, or a
        socket) behind, archived no bot log, or whose stop did not succeed, is
        a failure even when the operator asked for the stop — nothing may
        outlive the run. A clean stop is the operator's own outcome, and
        completion requires every selected report to be complete and
        policy-matched.

    """
    if record.outcome == OUTCOME_REFUSED:
        return OUTCOME_REFUSED
    if record.launched and not record.cleanup.get("verified"):
        record.error = record.error or (
            "the session did not shut down cleanly or left no archived bot log"
        )
        return OUTCOME_FAILED
    if stop.requested:
        return OUTCOME_STOPPED
    if record.outcome is not None:
        return record.outcome
    return coverage_outcome(record)


def coverage_outcome(record: RunRecord) -> str:
    """Decide completion from the selected postures' reports.

    Args:
        record: The run record.

    Returns:
        ``complete`` only when every selected posture ran and matched its
        policy; otherwise the failure the reports show, in order of severity.

    """
    outcomes = {mode["outcome"] for mode in record.modes if not mode["skipped"]}
    stopped_early = any(mode["skipped"] for mode in record.modes)
    if not stopped_early and outcomes == {OUTCOME_COMPLETE}:
        return OUTCOME_COMPLETE
    for candidate in (OUTCOME_MISMATCHED, OUTCOME_INCOMPLETE):
        if candidate in outcomes:
            return candidate
    return OUTCOME_FAILED


def run_metadata_document(record: RunRecord, stop: StopRequest) -> dict[str, Any]:
    """Render the run-level record.

    Args:
        record: The run record.
        stop: The stop request.

    Returns:
        The document archived as ``run_metadata.json``: the outcome, the real
        return codes of every command, each posture's result, and the cleanup
        verdict.

    """
    return {
        "schema_version": RUN_SCHEMA_VERSION,
        "metadata": METADATA_NAME,
        "outcome": record.outcome,
        "exit_code": exit_code(record),
        "error": record.error,
        "launched": record.launched,
        "stop": {"requested": stop.requested, "signal": stop.signal_name},
        "modes": record.modes,
        "commands": record.commands,
        "cleanup": record.cleanup,
    }


def execute(config: MatrixConfig, deps: RunnerDeps, record: RunRecord, stop: StopRequest) -> None:
    """Walk the session lifecycle in order, recording each step.

    Args:
        config: The run's configuration.
        deps: The injected dependencies.
        record: The run record to fill.
        stop: The stop request, honored between steps.

    """
    record.metadata = preflight(config, deps, record)
    write_json(config.artifact(METADATA_NAME), record.metadata)
    record.metadata_written = True
    launch(config, deps, record, stop)
    inject(config, deps, record, stop)
    probe_modes(config, deps, record, stop)


def run_matrix(
    config: MatrixConfig, deps: RunnerDeps, *, stop: StopRequest | None = None
) -> RunRecord:
    """Run one complete matrix session and return its record.

    Args:
        config: The run's configuration.
        deps: The injected dependencies.
        stop: The stop request to honor (a fresh one is created when absent).

    Returns:
        The finished run record, with its outcome and cleanup verdict.

    Raises:
        RefusalError: If the run is refused before the bot is launched.

    """
    stopper = stop if stop is not None else StopRequest()
    record = RunRecord(config=config)
    try:
        execute(config, deps, record, stopper)
    except RefusalError as exc:
        record.outcome = OUTCOME_REFUSED
        record.error = str(exc)
        raise
    except CommandFailed as exc:
        record.outcome = OUTCOME_STOPPED if stopper.requested else OUTCOME_FAILED
        record.error = str(exc)
    except KeyboardInterrupt:
        record.outcome = OUTCOME_STOPPED
        record.error = "interrupted"
        stopper.request()
    except Exception as exc:  # ruff: ignore[blind-except] - every failure still cleans up
        record.outcome = OUTCOME_STOPPED if stopper.requested else OUTCOME_FAILED
        record.error = scrub(f"{type(exc).__name__}: {exc}")
    finally:
        cleanup(config, deps, record, stopper)
        if record.metadata_written:
            record.outcome = finalize_outcome(record, stopper)
            write_json(config.artifact(RUN_METADATA_NAME), run_metadata_document(record, stopper))
    return record


def exit_code(record: RunRecord) -> int:
    """Map a run's outcome onto the runner's exit status.

    Args:
        record: The finished run record.

    Returns:
        ``0`` for a complete run, ``2`` for a refused one, ``130`` for an
        operator stop, and ``1`` for every failure — which is why a correct run
        needs no ``|| true``.

    """
    if record.outcome == OUTCOME_COMPLETE:
        return EXIT_COMPLETE
    if record.outcome == OUTCOME_REFUSED:
        return EXIT_REFUSED
    if record.outcome == OUTCOME_STOPPED:
        return EXIT_STOPPED
    return EXIT_FAILED


def render_summary(record: RunRecord) -> str:
    """Render the one-screen human summary of a run.

    Args:
        record: The finished run record.

    Returns:
        The summary text printed to the operator.

    """
    headline = (
        f"[bankroll] outcome={record.outcome} exit={exit_code(record)} "
        f"output_dir={record.config.output_dir}"
    )
    lines = [headline]
    if record.error:
        lines.append(f"[bankroll] error: {record.error}")
    for mode in record.modes:
        if mode["skipped"]:
            lines.append(f"[bankroll] {mode['capital']}: skipped (stop requested)")
            continue
        summary = mode["summary"] or {}
        lines.append(
            f"[bankroll] {mode['capital']} expect={mode['expect']} exit={mode['exit_code']} "
            f"outcome={mode['outcome']} complete={summary.get('complete')} "
            f"matched={summary.get('matched')} unexpected={summary.get('unexpected')}"
        )
    cleanup = record.cleanup
    lines.append(
        f"[bankroll] cleanup verified={cleanup.get('verified')} "
        f"bot_running={cleanup.get('bot_running')} pidfile={cleanup.get('pidfile_present')} "
        f"socket={cleanup.get('socket_present')}"
    )
    return "\n".join(lines) + "\n"


def default_run(argv: Sequence[str], *, env: Mapping[str, str] | None = None) -> CommandResult:
    """Run one command with the inherited environment plus explicit overrides.

    Args:
        argv: The argv to run.
        env: Overrides merged over this process's own environment, so the
            launcher keeps the operator's normal configuration.

    Returns:
        The finished command, its stdout and stderr merged.

    """
    merged = None if env is None else {**os.environ, **env}
    completed = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] - one explicit argv, never a shell
        [str(item) for item in argv],
        cwd=str(ROOT),
        env=merged,
        capture_output=True,
        text=True,
        check=False,
    )
    return CommandResult(
        argv=tuple(str(item) for item in argv),
        returncode=completed.returncode,
        output=f"{completed.stdout or ''}{completed.stderr or ''}",
    )


def resolve_rpc_transport(chain_id: int) -> Callable[[str], object]:
    """Build a minimal JSON-RPC transport over the project's resolved chain endpoint.

    The endpoint and its transport come from the project's own resolution — the
    same one the launcher's bot will use — so this preflight cannot read a
    different endpoint than the run will. The chain-bound provider the project
    constructs verifies the endpoint's chain before it answers, and nothing here
    ever prints or records the URI.

    Each call opens and closes its own provider: two reads are all this
    preflight needs, and a short-lived client cannot leak a connection into the
    session that follows.

    Args:
        chain_id: The chain whose request endpoint is resolved.

    Returns:
        A callable that takes one method name and returns its JSON-RPC result.

    """

    def call(method: str) -> object:
        try:
            # Imported here so `--help` and the argument refusals need no
            # configured extension.
            from degenbot.provider.factory import (  # ruff: ignore[import-outside-top-level]
                get_provider_from_config,
            )

            provider = get_provider_from_config(chain_id=chain_id)
        except Exception as exc:  # ruff: ignore[blind-except] - an unreachable endpoint is a refusal
            detail = scrub(f"{type(exc).__name__}: {exc}")
            msg = f"no usable RPC endpoint for chain {chain_id}: {detail}"
            raise RuntimeError(msg) from None
        try:
            return provider.make_request(method, [])
        finally:
            provider.close()

    return call


def build_deps(config: MatrixConfig) -> RunnerDeps:
    """Build the production dependencies for one run.

    Args:
        config: The run's configuration (its chain selects the endpoint).

    Returns:
        The dependencies the runner drives.

    """
    return RunnerDeps(run=default_run, rpc=resolve_rpc_transport(config.chain_id))


def build_parser() -> argparse.ArgumentParser:
    """Build the command line.

    Returns:
        The configured parser.

    """
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--owner", required=True, help="Owner address (0x + 40 hex).")
    parser.add_argument(
        "--injected-address", required=True, help="The address the runtime is injected at."
    )
    parser.add_argument(
        "--runtime", required=True, type=Path, help="The runtime bytecode artifact under test."
    )
    parser.add_argument(
        "--output-dir", required=True, type=Path, help="The run's directory (absent or empty)."
    )
    parser.add_argument("--capital", choices=CAPITAL_SELECTIONS, default=CAPITAL_BOTH)
    parser.add_argument("--fixed-input", type=int, default=DEFAULT_FIXED_INPUT)
    parser.add_argument("--chain-id", type=int, default=DEFAULT_CHAIN_ID)
    parser.add_argument("--release-id", default=DEFAULT_RELEASE_ID)
    parser.add_argument("--ready-timeout", type=float, default=DEFAULT_READY_TIMEOUT)
    return parser


def config_from_args(args: argparse.Namespace) -> MatrixConfig:
    """Turn the parsed command line into a validated configuration.

    Args:
        args: The parsed arguments.

    Returns:
        The validated configuration.

    """
    # `validate_config` owns the refusal: one place decides what an auditable
    # run needs, whichever entry point reached it.
    return validate_config(
        MatrixConfig(
            owner=args.owner,
            injected_address=args.injected_address,
            runtime=Path(args.runtime),
            output_dir=Path(args.output_dir),
            capital=args.capital,
            fixed_input=args.fixed_input,
            chain_id=args.chain_id,
            release_id=args.release_id,
            ready_timeout=args.ready_timeout,
        )
    )


def main(argv: Sequence[str] | None = None, *, deps: RunnerDeps | None = None) -> int:
    """Run the matrix for the requested capital selection.

    Args:
        argv: The argv to parse (defaults to the process argv).
        deps: The dependencies to use (built from the configuration when absent).

    Returns:
        The run's exit status.

    """
    args = build_parser().parse_args(argv)
    stop = StopRequest()
    try:
        config = config_from_args(args)
        resolved = deps if deps is not None else build_deps(config)
        record = run_matrix(config, resolved, stop=stop.install())
    except RefusalError as exc:
        sys.stderr.write(f"[bankroll] refused: {exc}\n")
        return EXIT_REFUSED
    finally:
        stop.restore()
    sys.stdout.write(render_summary(record))
    sys.stdout.flush()
    return exit_code(record)


if __name__ == "__main__":
    raise SystemExit(main())
