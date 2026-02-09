---
title: Configuration
category: cli
tags:
  - configuration
  - environment-variables
related_files:
  - ../src/degenbot/logging.py
complexity: simple
---

# Configuration

## Environment Variables

### Debug Logging

| Variable | Values | Description |
|----------|--------|-------------|
| `DEGENBOT_DEBUG` | `1`, `true`, `yes` | Enable debug-level logging output globally |

Set this environment variable before importing degenbot to see all `logger.debug()` messages throughout the codebase. This is useful for troubleshooting and development.

```bash
DEGENBOT_DEBUG=1 python my_script.py
```