"""Observe a cascade layer in a process whose environment declared it.

``degenbot-config`` installs the process verdict ONCE, at FFI module init, so
a declared key is decided by whatever the environment held when the interpreter
started. A test that mutates ``os.environ`` in-process and then reads a
declared key is reading a verdict settled before the mutation, which makes the
probe agree with itself no matter what the layer does.

So every test that needs a layer observed — an env export, a bad value, a file
layer — runs Python in a child interpreter whose only layers are the ones it
declares. The child also gets its own XDG config/state home, so neither the
developer's shell nor their operator file decides the layer under test.

Two questions are asked here, and they are different:

* :func:`resolved_value` — what did the cascade settle for one declared key,
  and which layer won. (No config object involved.)
* :func:`config_values` — what did :meth:`ArbitrageConfig.from_env` build from
  that verdict, which is the surface a driver consumes.

A value is declared as a dotted field path (``diag.tracemalloc_secs``) or a
dotted TOML path (``diagnostics.tracemalloc_secs``) — same shape, different
questions, and the tests that need both assert they agree.
"""

from __future__ import annotations

import json
import os
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
import tempfile
from contextlib import contextmanager
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Iterator, Mapping, Sequence


_REPO_ROOT = Path(__file__).resolve().parents[2]

#: The operator file the Python suite pins for its own boot. A child that wants
#: a different file layer writes one with :func:`operator_file`; this is the
#: default so a probe inherits the suite's determinism, not a developer's shell.
AMBIENT_OPERATOR_FILE = _REPO_ROOT / "tests/ambient_config.toml"

#: Names a child must not inherit: every declared key's env name except
#: `DEGENBOT_CONFIG` (which this module sets), the pre-prefix verification-retry
#: family, and the retired bare injection name — so "absent" means absent.
_SCRUBBED_PREFIXES = ("DEGENBOT_", "VERIFICATION_RETRY_")
_SCRUBBED_NAMES = frozenset({"DEGENBOT_CONFIG", "INJECT_EXECUTOR_CODE"})

#: The chain-1 endpoint every probe resolves through, so no probe is about RPC.
_OVERRIDE_NODE = "wss://probe.example"

#: The separator for the child programs assembled below.
_NL = chr(10)


def run(
    code: str,
    *,
    env: Mapping[str, str] | None = None,
    operator_file: Path | None = AMBIENT_OPERATOR_FILE,
) -> subprocess.CompletedProcess[str]:
    """Run ``code`` in a fresh interpreter whose only config layers are declared.

    Args:
        code: The child program. It may import degenbot; its exit code and
            streams are the assertion.
        env: The environment variables the child is given, on top of a scrubbed
            copy of this process's environment.
        operator_file: The operator file to pin, or ``None`` for no file layer
            (the child then resolves against env and declared defaults only).

    Returns:
        The completed process. A boot refusal exits 2 with its reason on
        stderr — that is an assertion, not a crash, so nothing is raised here.

    """
    with tempfile.TemporaryDirectory() as scratch:
        child_env = {
            name: value
            for name, value in os.environ.items()
            if name not in _SCRUBBED_NAMES and not name.startswith(_SCRUBBED_PREFIXES)
        }
        child_env["XDG_CONFIG_HOME"] = str(Path(scratch) / "config")
        child_env["XDG_STATE_HOME"] = str(Path(scratch) / "state")
        if operator_file is not None:
            child_env["DEGENBOT_CONFIG"] = str(operator_file)
        child_env.update(env or {})

        return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] — fixed argv, no shell
            [sys.executable, "-X", "utf8", "-c", code],
            capture_output=True,
            text=True,
            cwd=_REPO_ROOT,
            env=child_env,
            timeout=120,
            check=False,
        )


@contextmanager
def operator_file(body: str) -> Iterator[Path]:
    """A temporary operator file carrying ``body``, removed on exit.

    The file layer is the one layer a test cannot hand a child as an
    environment variable, so a test that needs an operator file writes one
    here and pins it.

    Args:
        body: The file's TOML text.

    Yields:
        The path to write and pin.

    """
    with tempfile.TemporaryDirectory() as scratch:
        path = Path(scratch) / "operator.toml"
        path.write_text(body, encoding="utf-8")
        yield path


#: Print one declared key's resolved value and the layer that supplied it.
_VALUE_PROBE = """\
import json

from degenbot.config import resolved_config


verdict = resolved_config()
print(json.dumps({"value": verdict.values[PATH], "source": verdict.provenance.get(PATH)}))
"""


def resolved_value(
    path: str,
    *,
    env: Mapping[str, str] | None = None,
    operator_file: Path | None = AMBIENT_OPERATOR_FILE,
) -> dict[str, object]:
    """One declared key's resolved value and winning layer, from a child process.

    Args:
        path: The declared key's dotted TOML path.
        env: Extra environment for the child.
        operator_file: The operator file to pin, or ``None`` for none.

    Returns:
        ``{"value": ..., "source": ...}``. ``source`` is absent when no layer
        supplied the key — an unrecorded provenance map is the answer, not a
        gap to paper over with the floor.

    """
    completed = run(f"PATH = {path!r}\n" + _VALUE_PROBE, env=env, operator_file=operator_file)
    assert completed.returncode == 0, (
        f"{path} did not resolve: stdout={completed.stdout!r} stderr={completed.stderr!r}"
    )
    payload: dict[str, object] = json.loads(completed.stdout)
    return payload


#: Build a config in the child and print the requested fields by dotted path.
_FIELD_PROBE = """\
import dataclasses
import json

from degenbot.runner.config import ArbitrageConfig, RpcCascadeOverrides


cfg = ArbitrageConfig.from_env(
    json.loads(DOTENV),
    live=LIVE,
    permutation=None,
    rpc=RpcCascadeOverrides(chain_id=1, node=NODE),
)

out = {}
for path in FIELDS:
    value = cfg
    for part in path.split("."):
        value = getattr(value, part)
    out[path] = dataclasses.asdict(value) if dataclasses.is_dataclass(value) else value
print(json.dumps(out, default=str))
"""


def build_config_code(
    fields: Sequence[str],
    *,
    dotenv: Mapping[str, str] | None = None,
    live: bool = False,
) -> str:
    """The child program that builds a config and prints `fields` by dotted path.

    Args:
        fields: Dotted paths off the built config.
        dotenv: The example dotenv mapping handed to ``from_env``.
        live: Whether the child builds the config in live mode.

    Returns:
        A program to hand to :func:`run`. Use it directly when the test is
        about a refusal, which is a child exit code and a message rather than
        a printed value.

    """
    return _NL.join([
        f"DOTENV = {json.dumps(dict(dotenv or {}))!r}",
        f"FIELDS = {list(fields)!r}",
        f"LIVE = {live!r}",
        f"NODE = {_OVERRIDE_NODE!r}",
        _FIELD_PROBE,
    ])


def config_values(
    fields: Sequence[str],
    *,
    env: Mapping[str, str] | None = None,
    dotenv: Mapping[str, str] | None = None,
    live: bool = False,
    operator_file: Path | None = AMBIENT_OPERATOR_FILE,
) -> dict[str, object]:
    """The fields a config built in a child process carries, by dotted path.

    Args:
        fields: Dotted paths off the built config, e.g. ``diag.tracemalloc_secs``
            or ``verification_retry_policy.max_attempts``.
        env: Extra environment for the child (the declared env layer).
        dotenv: The example dotenv mapping handed to ``from_env``.
        live: Whether the child builds the config in live mode.
        operator_file: The operator file to pin, or ``None`` for none.

    Returns:
        The requested field values, keyed by the path they were asked for.

    """
    completed = run(
        build_config_code(fields, dotenv=dotenv, live=live),
        env=env,
        operator_file=operator_file,
    )
    assert completed.returncode == 0, (
        f"config build failed: stdout={completed.stdout!r} stderr={completed.stderr!r}"
    )
    payload: dict[str, object] = json.loads(completed.stdout)
    return payload
