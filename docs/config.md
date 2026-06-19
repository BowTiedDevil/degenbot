---
title: Configuration
category: cli
tags:
  - configuration
  - environment-variables
related_files:
  - src/degenbot/logging.py
  - src/degenbot/config.py
  - src/degenbot/cli/__init__.py
complexity: simple
---

# Configuration

## Usage with Bot

The `Bot` class is the primary consumer of configuration. It loads settings via `Bot.from_config_file()` or explicit `DegenbotConfig`:

```python
import degenbot

# Load from config file (creates default if not exists)
bot = degenbot.Bot.from_config_file()

# Or pass explicit config
from degenbot.config import DegenbotConfig

bot = degenbot.Bot(
    config=DegenbotConfig(
        default_chain_id=1,
        rpc={
            1: "https://eth-mainnet.example.com",
            8453: "https://base-mainnet.example.com",
        },
        database={"path": "~/.config/degenbot/degenbot.db"},
    )
)
```

## Environment Variables

### Debug Logging

| Variable | Values | Description |
|----------|--------|-------------|
| `DEGENBOT_DEBUG` | `1`, `true`, `yes` | Enable debug-level logging output globally |
| `DEGENBOT_DEBUG_FUNCTION_CALLS` | `1`, `true`, `yes` | Enable function call trace logging |
| `DEGENBOT_COVERAGE` | `1` | Enable CLI code coverage tracking (dev use) |
| `DEGENBOT_COVERAGE_OUTPUT` | directory path | Output directory for coverage report (default: `htmlcov`) |

Set `DEGENBOT_DEBUG` before importing degenbot to see all `logger.debug()` messages throughout the codebase. This is useful for troubleshooting and development.

Set `DEGENBOT_DEBUG_FUNCTION_CALLS` to trace all function calls decorated with `@log_function_call`.

```bash
DEGENBOT_DEBUG=1 python my_script.py
```

## Configuration File

Degenbot uses a TOML configuration file located at `~/.config/degenbot/config.toml`. It is created automatically on first use with default settings.

```toml
# The chain this Bot session targets (required). One Bot per chain — see ADR-006.
# Must match one of the chain IDs in the [rpc] table below; the connected RPC's
# eth_chainId is enforced to match this value at construction (fail-fast).
default_chain_id = 1

[rpc]
# Chain ID to RPC endpoint mapping (used by Bot.connections)
1 = "https://eth-mainnet.example.com"
8453 = "https://base-mainnet.example.com"

[database]
# SQLite database path (used by Bot.db, defaults to ~/.config/degenbot/degenbot.db)
path = "/path/to/degenbot.db"
```

### `default_chain_id`

The chain this `Bot` session targets. **Required** — `Bot.__init__()` raises
`DegenbotValueError` if it is unset. A `Bot` is single-chain (ADR-006): one
Bot instance per chain. The value must match a key in the `[rpc]` table, and
the connected RPC endpoint's `eth_chainId` is enforced to equal it at
construction (fail-fast on a misconfigured endpoint).

On a freshly-initialized config (created by `_init_config()` when no config
file exists yet), `default_chain_id` is left unset (`None`) — set it
manually after adding at least one RPC entry.

### Config Resolution Order

1. **Explicit config passed to `Bot.__init__()`** (highest priority)
2. **Path passed to `Bot.from_config_file(path)`**
3. **`~/.config/degenbot/config.toml`** (default location)
4. **Default empty config** with platform-specific defaults (lowest priority)