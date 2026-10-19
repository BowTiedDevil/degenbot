"""The suite's declared environment, pinned before any ``degenbot`` import.

The FFI module init resolves the typed ``BotConfig`` from the PROCESS layers
at the first ``degenbot`` import, and the engine boot re-resolves those same
process layers at EVERY construction (``backrun_boot_resources`` ->
``load_process_config``, ADR-062 D7: the boot must see what a fresh process
would resolve). The config-holder test seam cannot re-govern that boot path,
so whatever the process layers held at first import governs every
construction's node dial and database read.

This module must therefore be imported BEFORE the first ``degenbot`` import —
the root ``conftest.py`` does exactly that. Importing it pins the two
boot-pinning names onto ``os.environ`` so the first-wins snapshot IS the
suite's declaration: constructions dial this suite's hermetic config instead
of whatever a developer's ``~/.config/degenbot/config.toml`` resolves (a live
provider here also made the offline suite slow: every engine construction
synchronously dialled the node join and loaded the connector index).

Force-set, not ``setdefault``: the suite's declaration governs regardless of
what the developer's shell exports — the same ambient-shape race the suite
already lost twice before the captured-env design. Operator-facing
``DEGENBOT_*`` overrides for on-demand runs (e.g.
``DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_ENDPOINTS``) are different names and
still ride the env layer above the file.

The declared database path is an absent per-session file so the connector
lane takes its discovery-shut branch: booting never reads the developer's
real ``~/.local/state/degenbot/db/degenbot.db`` (hundreds of MB). The
directory is removed at exit so repeated runs leave no state behind.
"""

import atexit
import os
import tempfile
from pathlib import Path

_STATE_DIR_CTX = tempfile.TemporaryDirectory(prefix="degenbot-pytest-state-")
atexit.register(_STATE_DIR_CTX.cleanup)
_STATE_DIR = _STATE_DIR_CTX.name

SUITE_ENV: dict[str, str] = {
    "DEGENBOT_CONFIG": str(Path(__file__).resolve().parent / "ambient_config.toml"),
    "DEGENBOT_DB_PATH": str(Path(_STATE_DIR) / "degenbot.db"),
}

os.environ.update(SUITE_ENV)
