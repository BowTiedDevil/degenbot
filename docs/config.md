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
[rpc]
# Chain ID to RPC endpoint mapping
1 = "https://eth-mainnet.example.com"
8453 = "https://base-mainnet.example.com"

[database]
# SQLite database path (defaults to ~/.config/degenbot/degenbot.db)
path = "/path/to/degenbot.db"
```