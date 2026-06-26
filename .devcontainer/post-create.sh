#!/usr/bin/env bash
# degenbot devcontainer post-create install script.
# Idempotent: safe to re-run (devcontainer rebuild / --force-rebuild).
#
# OS-level tools (python3, rustc/cargo/rustfmt/clippy, nodejs24/npm, just, uv,
# tmux, git) are baked into the Dockerfile via dnf (fedora:latest). This script
# installs only the tools with no dnf package, plus project wiring:
#   - foundry  (foundryup; no dnf path)
#   - pi       (npm i -g via the dnf npm; lands in ~/.local/bin via npm prefix)
#   - venv + uv sync (builds the degenbot_rs PyO3 extension)
set -euo pipefail

LOCAL_BIN="/home/dev/.local/bin"
mkdir -p "$LOCAL_BIN"

# PATH for this script: postCreateCommand does not inherit remoteEnv.
# ~/.local/bin holds pi; ~/.foundry/bin holds foundry. cargo/just/node/python/
# uv are in /usr/bin from dnf and already on PATH.
export PATH="$HOME/.foundry/bin:$LOCAL_BIN:$PATH"

echo ">>> installing foundry (foundryup latest; no dnf path)"
if ! command -v forge >/dev/null 2>&1; then
  curl -L https://foundry.paradigm.xyz | bash
  /home/dev/.foundry/bin/foundryup
fi

echo ">>> installing pi (matches host: @earendil-works/pi-coding-agent)"
if ! command -v pi >/dev/null 2>&1; then
  # npm prefix is set to ~/.local in the Dockerfile, so this lands the `pi`
  # binary in ~/.local/bin and needs no sudo.
  npm install -g @earendil-works/pi-coding-agent
fi

echo ">>> creating venv from dnf python and syncing project"
# dnf python (3.14 on fedora:latest) ships libpython.so via python3-devel, so
# the PyO3 extension links fine. Use the system python — no uv-managed python
# install step needed. Create the venv only if absent, so a warm one survives
# --force-rebuild cycles instead of being wiped each time.
cd /workspaces/degenbot
if [ ! -d .venv ]; then
  uv venv --python python3
fi
uv sync

echo ">>> enabling commitlint git hooks (optional; needs node)"
if [ -f justfile ]; then
  just setup-git-hooks || echo "  (setup-git-hooks skipped or failed — non-fatal)"
fi

echo ">>> post-create complete"
echo "    pi         : $(command -v pi || echo MISSING)"
echo "    just       : $(command -v just || echo MISSING)"
echo "    uv         : $(command -v uv || echo MISSING)"
echo "    forge      : $(command -v forge || echo MISSING)"
echo "    cargo      : $(command -v cargo || echo MISSING)"
echo "    rustc      : $(rustc --version 2>&1 || echo MISSING)"
echo "    clippy     : $(cargo clippy --version 2>&1 || echo MISSING)"
echo "    python     : $(command -v python3 || echo MISSING)"
# Import smoke-test for the PyO3 extension built by `uv sync`.
ext_status="$(cd /workspaces/degenbot && uv run python -c 'from degenbot import degenbot_rs' 2>&1 | tail -1)"
echo "    degenbot_rs : ${ext_status:-OK}"