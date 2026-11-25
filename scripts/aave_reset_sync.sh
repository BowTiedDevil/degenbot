#!/usr/bin/env bash
set -euo pipefail

uv run --no-sync degenbot aave reset
uv run --no-sync degenbot aave update
