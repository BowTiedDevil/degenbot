#!/usr/bin/env bash
# degenbot devcontainer post-create install script.
# Idempotent: safe to re-run (devcontainer rebuild / --force-rebuild).
set -euo pipefail

LOCAL_BIN="/home/vscode/.local/bin"
mkdir -p "$LOCAL_BIN"

echo ">>> installing tmux (the devcontainers base image lacks it)"
if ! command -v tmux >/dev/null 2>&1; then
  sudo apt-get update -qq && sudo apt-get install -y -qq tmux
fi

echo ">>> installing uv"
if ! command -v uv >/dev/null 2>&1; then
  curl -LsSf https://astral.sh/uv/install.sh | sh
fi
# Add all tool install dirs to PATH so the script's own checks and later
# steps find what they just installed. remoteEnv in devcontainer.json only
# applies to VSCode terminals, not to postCreateCommand — so set it here.
export PATH="$HOME/.cargo/bin:$HOME/.foundry/bin:$LOCAL_BIN:$PATH"

echo ">>> installing/updating rust toolchain"
# Base-image-agnostic: devcontainers/base:ubuntu ships no Rust at all, while
# devcontainers/rust:... pins a specific (stale) rustc. Install rustup if
# absent, then switch to the rolling stable channel and keep it current.
if ! command -v rustup >/dev/null 2>&1; then
  echo "    rustup not found — installing via sh.rustup.rs"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal
  # rustup installer writes ~/.cargo/env; source it so cargo/rustc are on PATH
  # for the rest of this script.
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi
rustup default stable
rustup update stable

echo ">>> installing just (prebuilt static binary, latest release)"
# Prebuilt binary, not `cargo install`: avoids coupling to rustc's MSRV
# entirely. The musl static binary has no glibc dependency and no rustc
# requirement. Version is resolved dynamically from GitHub releases for
# "up-to-date tools" — re-running post-create always gets the newest just.
if ! command -v just >/dev/null 2>&1; then
  arch="$(uname -m)"
  case "$arch" in
    x86_64)  just_arch="x86_64-unknown-linux-musl" ;;
    aarch64) just_arch="aarch64-unknown-linux-musl" ;;
    *) echo "ERROR: unsupported arch $arch for just prebuilt binary" >&2; exit 1 ;;
  esac
  just_version="$(curl -fsSL https://api.github.com/repos/casey/just/releases/latest \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)"
  if [ -z "$just_version" ]; then
    echo "ERROR: could not resolve latest just release tag" >&2; exit 1
  fi
  echo "    resolved just $just_version"
  curl -fsSL "https://github.com/casey/just/releases/download/${just_version}/just-${just_version}-${just_arch}.tar.gz" \
    | tar -xz -C "$LOCAL_BIN" just
fi

echo ">>> installing foundry (foundryup latest)"
if ! command -v forge >/dev/null 2>&1; then
  curl -L https://foundry.paradigm.xyz | bash
  # foundryup installs to ~/.foundry/bin and writes ~/.bashrc. Run it now so
  # the binaries land in /home/vscode/.foundry/bin during postCreate.
  /home/vscode/.foundry/bin/foundryup
fi

echo ">>> installing pi (matches host: @earendil-works/pi-coding-agent)"
npm install -g @earendil-works/pi-coding-agent

echo ">>> installing python 3.12 via uv (ships libpython.so — required by PyO3)"
# The devcontainers python feature (if present) installs a STATIC-only build
# (no libpython.so), which breaks PyO3 extension linking. uv's own managed
# cpython-3.12 ships the shared library. Create the venv explicitly with the
# uv-managed python so the venv's LIBDIR has libpython3.12.so — `uv sync` then
# builds the degenbot_rs extension via PEP 517 (maturin).
uv python install 3.12
cd /workspaces/degenbot
# Create venv explicitly (else uv may pick a static-only interpreter on PATH).
uv venv --python 3.12
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
echo "    python     : $(command -v python3 || echo MISSING)"
echo "    degenbot_rs : $(cd /workspaces/degenbot && uv run python -c 'from degenbot import degenbot_rs; print("OK")' 2>&1 | tail -1)"