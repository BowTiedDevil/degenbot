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
# entry point (attach.sh) does `podman start`+`exec`, which does NOT
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
# eliminating the shared-venv poisoning between differing container/host paths.
# `--allow-existing` makes `uv venv` idempotent: a no-op if the venv is already
# there (warm venv survives a podman stop/start), creates it fresh otherwise
# (a --remove-existing-container rebuild wipes it). `uv sync` then installs
# packages (and builds the PyO3 extension) into whichever state it finds.
cd /workspaces/degenbot
uv venv --allow-existing "$UV_PROJECT_ENVIRONMENT"
uv sync

# Self-heal a poisoned editable install. uv's rebuild-on-`uv run` (driven by
# [tool.uv] cache-keys watching the .rs sources) only fires when the editable
# install's `.pth` points at the LIVE repo — it keys staleness off that path.
# A venv snapshotted on another machine (e.g. repo at /home/btd/code/degenbot
# there but /workspaces/degenbot here) carries a `.pth` whose path is dead in
# this container, so uv's "is it fresh?" check silently short-circuits and
# `uv run` loads whatever stale .so sits in the source tree. Detect that drift
# and force a clean reinstall so the .pth is re-pointed correctly.
LIVE_SRC="/workspaces/degenbot/src"
while read -r pth; do
	if ! grep -qxF "$LIVE_SRC" "$pth"; then
		echo ">>> editable .pth is stale/mispointed; repairing editable install"
		echo "    offending: $pth -> $(cat "$pth")"
		uv sync --reinstall-package degenbot
		break
	fi
done < <(find "$UV_PROJECT_ENVIRONMENT" -name degenbot.pth -type f 2>/dev/null)

just setup-git-hooks 