"""Observe the configuration cascade through the door the claim belongs to.

The seam rule lives here because this module owns the suite's only fresh
interpreter:

    A fresh interpreter is for claims that only exist at process level: import
    cost, exit codes, argv, transport. Resolution claims go through
    ``resolve_hypothetical``; installed-verdict claims through
    ``resolved_config``.

The cascade installs ONCE, at FFI module init, so a declared key is decided by
whatever the environment held when the interpreter started. Two doors answer
two different questions, and using the wrong one is a tautology:

* :func:`run` -- a fresh interpreter whose environment is fixed BEFORE the
  import. The door for a claim about the process that booted: an exit code, an
  import timing, a raw-table reader closing over the installed file.
* :func:`hypothetical_values` / :func:`resolved_value` / :func:`build_config` /
  :func:`config_values` -- ``degenbot._ffi.resolve_hypothetical``, a pure
  function of a captured environment + file that installs nothing. The door
  for a resolution claim, asked in-process without spawning.

:func:`operator_file` writes the one layer ``resolve_hypothetical`` cannot take
as an environment dict.
"""

from __future__ import annotations

import dataclasses
import os
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
import tempfile
from contextlib import contextmanager
from pathlib import Path
from typing import TYPE_CHECKING, Any

from degenbot import _ffi
from degenbot.runner.config import ArbitrageConfig, RpcCascadeOverrides
from tests.helpers.identity_env import identity_env

if TYPE_CHECKING:
    from collections.abc import Iterator, Mapping, Sequence

_REPO_ROOT = Path(__file__).resolve().parents[2]

#: The operator file the Python suite pins for its own boot. A hypothetical
#: resolution uses it by default so it inherits the suite's determinism, not a
#: developer's shell; a test that wants a different file layer writes one with
#: :func:`operator_file` and passes it.
AMBIENT_OPERATOR_FILE = _REPO_ROOT / "tests/ambient_config.toml"

#: The explicit-override endpoint every config build resolves through, so no
#: build in this suite depends on whichever RPC the machine happens to carry.
_OVERRIDE_NODE = "wss://probe.example"

#: Names a child must not inherit: every declared key's env name except
#: `DEGENBOT_CONFIG` (which this module sets), the pre-prefix verification-retry
#: family, and the retired bare injection name — so "absent" means absent.
_SCRUBBED_PREFIXES = ("DEGENBOT_", "VERIFICATION_RETRY_")
_SCRUBBED_NAMES = frozenset({"DEGENBOT_CONFIG", "INJECT_EXECUTOR_CODE"})

def run(
    code: str,
    *,
    env: Mapping[str, str] | None = None,
    operator_file: Path | None = AMBIENT_OPERATOR_FILE,
) -> subprocess.CompletedProcess[str]:
    """Run ``code`` in a fresh interpreter whose only config layers are declared.

    Use this ONLY for a process-level claim -- an exit code, an import timing, a
    reader that closes over the installed file. A resolution claim belongs in
    :func:`hypothetical_values`, which installs nothing.

    Args:
        code: The child program. It may import degenbot; its exit code and
            streams are the assertion.
        env: The environment variables the child is given, on top of a scrubbed
            copy of this process's environment.
        operator_file: The operator file to pin, or ``None`` for no file layer
            (the child then resolves against env and declared defaults only).

    Returns:
        The completed process. A boot refusal exits 2 with its reason on
        stderr -- that is an assertion, not a crash, so nothing is raised here.

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

    The file layer is the one layer a test cannot hand to
    ``resolve_hypothetical`` as an environment dict, so a test that needs an
    operator file writes one here and passes it.

    Args:
        body: The file's TOML text.

    Yields:
        The path to write and pass.

    """
    with tempfile.TemporaryDirectory() as scratch:
        path = Path(scratch) / "operator.toml"
        path.write_text(body, encoding="utf-8")
        yield path

def hypothetical_values(
    env: Mapping[str, str] | None = None,
    *,
    operator_file: Path | None = AMBIENT_OPERATOR_FILE,
) -> dict[str, Any]:
    """Every declared key's resolved value for ``env`` + ``operator_file``.

    A pure resolution: ``resolve_hypothetical`` reads the captured environment
    and the named file, never this process's installed verdict, so the answer
    is HOW the cascade resolves rather than WHAT this interpreter installed.

    Args:
        env: The declared env layer, exactly the names the caller wants
            visible. An inherited ``DEGENBOT_*`` name cannot leak in.
        operator_file: The file layer to resolve through, or ``None`` for none.

    Returns:
        The declared values keyed by dotted TOML path.

    """
    hypothetical = _ffi.resolve_hypothetical(
        dict(env or {}),
        None if operator_file is None else str(operator_file),
    )
    return hypothetical.values

def resolved_value(
    path: str,
    *,
    env: Mapping[str, str] | None = None,
    operator_file: Path | None = AMBIENT_OPERATOR_FILE,
) -> dict[str, object]:
    """One declared key's resolved value and winning layer.

    Args:
        path: The declared key's dotted TOML path.
        env: The declared env layer.
        operator_file: The file layer to resolve through, or ``None`` for none.

    Returns:
        ``{"value": ..., "source": ...}``. ``source`` is absent when no layer
        supplied the key -- an unrecorded provenance map is the answer, not a
        gap to paper over with the floor.

    """
    hypothetical = _ffi.resolve_hypothetical(
        dict(env or {}),
        None if operator_file is None else str(operator_file),
    )
    payload: dict[str, object] = {"value": hypothetical.values[path]}
    source = hypothetical.provenance.get(path)
    if source is not None:
        payload["source"] = source
    return payload

def build_config(
    *,
    env: Mapping[str, str] | None = None,
    identity: Mapping[str, str] | None = None,
    live: bool = False,
    operator_file: Path | None = AMBIENT_OPERATOR_FILE,
) -> ArbitrageConfig:
    """Build an :class:`ArbitrageConfig` from a hypothetical resolution.

    The declared keys come from :func:`hypothetical_values`, so the build reads
    the cascade's answer for ``env`` + ``operator_file`` and installs nothing.
    Operator/executor identity is read from the process environment, so
    ``identity`` is installed for the build and blanked otherwise.

    Args:
        env: The declared env layer.
        identity: The operator/executor identity installed for the build.
        live: Whether to build in live mode.
        operator_file: The file layer to resolve through, or ``None`` for none.

    Returns:
        The built config, carrying the hypothetical verdict's values.

    """
    values = hypothetical_values(env, operator_file=operator_file)
    with identity_env(identity):
        return ArbitrageConfig.build(
            live=live,
            permutation=None,
            rpc=RpcCascadeOverrides(chain_id=1, node=_OVERRIDE_NODE),
            values=values,
        )

def config_values(
    fields: Sequence[str],
    *,
    env: Mapping[str, str] | None = None,
    identity: Mapping[str, str] | None = None,
    live: bool = False,
    operator_file: Path | None = AMBIENT_OPERATOR_FILE,
) -> dict[str, object]:
    """The fields a config built from a hypothetical resolution carries.

    Args:
        fields: Dotted paths off the built config, e.g. ``diag.tracemalloc_secs``
            or ``verification_retry_policy.max_attempts``.
        env: The declared env layer.
        identity: The operator/executor identity installed for the build.
        live: Whether to build the config in live mode.
        operator_file: The file layer to resolve through, or ``None`` for none.

    Returns:
        The requested field values, keyed by the path they were asked for.

    """
    cfg = build_config(env=env, identity=identity, live=live, operator_file=operator_file)
    out: dict[str, object] = {}
    for path in fields:
        value: object = cfg
        for part in path.split("."):
            value = getattr(value, part)
        out[path] = dataclasses.asdict(value) if dataclasses.is_dataclass(value) else value
    return out
