#!/usr/bin/env bash
# degenbot devcontainer post-create install script.
# Idempotent: safe to re-run (devcontainer rebuild / --force-rebuild).
#
# OS-level tools (python3, rustc/cargo/rustfmt/clippy, nodejs24/npm, just, uv,
# tmux, git) AND the curl/npm-installed tools (foundry, pi) are all baked into
# the Dockerfile image. This script handles only the workspace-dependent wiring
# that CANNOT be baked into the image (it needs the bind-mounted repo):
#   - venv + uv sync (builds the degenbot_rs PyO3 extension)
#   - commitlint git hooks
# foundry/pi used to live here but were moved into the Dockerfile because the
# SSH entry point (ssh-attach.sh) does `podman start`+`exec`, which does NOT
# run postCreateCommand — image-baking them guarantees availability.
set -euo pipefail

# PATH for this script: postCreateCommand does not inherit remoteEnv.
# foundry/pi are in the image under ~/.foundry/bin and ~/.local/bin. cargo/
# just/node/python/uv are in /usr/bin from dnf.
export PATH="$HOME/.foundry/bin:$HOME/.local/bin:$PATH"

echo ">>> creating container-local venv (UV_PROJECT_ENVIRONMENT) and syncing project"
# dnf python (3.14 on fedora:latest) ships libpython.so via python3-devel, so
# the PyO3 extension links fine. Use the system python — no uv-managed python
# install step needed. The venv lives at $UV_PROJECT_ENVIRONMENT (container-
# local, NOT under the bind-mounted /workspaces) so its pyvenv.cfg and script
# shebangs bake /home/dev/.venvs/... paths that are never read by the host —
# eliminating the shared-venv poisoning between /workspaces and /home/btd.
# `--allow-existing` makes `uv venv` idempotent: a no-op if the venv is already
# there (warm venv survives a podman stop/start), creates it fresh otherwise
# (a --remove-existing-container rebuild wipes it). `uv sync` then installs
# packages (and builds the PyO3 extension) into whichever state it finds.
cd /workspaces/degenbot
uv venv --allow-existing "$UV_PROJECT_ENVIRONMENT"
uv sync

echo ">>> enabling commitlint git hooks (optional; needs node)"
echo ">>> foundry+pi are baked into the image; if either is MISSING in the summary below, the image is stale — rebuild it."
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