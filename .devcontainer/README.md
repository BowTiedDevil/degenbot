# degenbot devcontainer

VSCode auto-discovers `devcontainer.json` in this directory.

**Container runtime: Podman** (not Docker). Mounts are plain `type=bind` (no
`consistency=cached`, which is Docker-Desktop-only). VSCode and the devcontainer
CLI both need to be pointed at `podman` (see setup below).

**Base image: `fedora:latest`** (currently resolves to Fedora 44). Everything
dnf-installable is baked into a minimal `Dockerfile` — there are **zero
devcontainer features**. Only one tool is still curl-installed, because it has
no Fedora package: `foundry`. (uv, unlike on Ubuntu, IS packaged by Fedora.)

## Why Fedora

`fedora:latest` over the prior `mcr.microsoft.com/devcontainers/base:ubuntu` for
three reasons:

1. **More tools packaged, newer versions** — Fedora 44 ships rust 1.96 (vs
   Ubuntu's 1.93), uv directly (Ubuntu has no package), and nodejs24 Active LTS
   as a clean metapackage.
2. **Host parity** — the development host runs Fedora, so the container mirrors
   the host's toolchain versions and behavior.
3. **No features needed** — on the prior Ubuntu base we relied on the `node`
   devcontainer feature; on Fedora, `dnf install nodejs24` gives node + npm in
   one package, so all features disappear.

## First-time setup

1. Tell VSCode to use Podman (one-time, in `~/.config/Code/User/settings.json`):

   ```jsonc
   "dev.containers.dockerPath": "podman"
   ```

2. Ensure the host directories pi bind-mounts exist (create empty if absent —
   Podman refuses bind mounts whose source is missing):

   ```bash
   mkdir -p ~/.pi ~/.agents ~/code/shared
   ```

3. Build the container once (choose one):
  - **VSCode:** open this repo → Command Palette → `Dev Containers: Reopen in Container`.
  - **CLI:** `devcontainer up --workspace-folder <path-to-degenbot> --docker-path podman` (requires `npm i -g @devcontainers/cli`).

  First build is slow (dnf layer in the Dockerfile + `post-create.sh`). Both
  paths produce a container named `vsc-degenbot-<hash>` and are interchangeable —
  a container built by one is found and reused by the other.

## Entering the container

### A. terminal (your default)

```bash
.devcontainer/attach.sh
```

This script finds the `vsc-degenbot-*` container, starts it if stopped, and
drops into a tmux session named `pi`. No flags to remember.

Equivalent manual commands:

```bash
podman ps -a --filter name=vsc-degenbot --format '{{.Names}}'   # find container name
podman start <name>
podman exec -it <name> tmux new -As pi
```

To rebuild from scratch:
```bash
.devcontainer/rebuild.sh    # needs devcontainer CLI
```

### B. VSCode

Open the repo → `Dev Containers: Reopen in Container`, or if the container is
already running (from either path), `Dev Containers: Attach to Running
Container...` → pick `vsc-degenbot-*`. Same filesystem as the tmux/pi session —
no state fork.

## What's inside

All of the following come from Fedora's dnf repos via the Dockerfile (so they
are `dnf upgrade`-able and pinned to whatever `fedora:latest` ships at build
time):

| Tool        | dnf package(s)                            | Notes                                            |
|-------------|-------------------------------------------|--------------------------------------------------|
| Python 3.14 | `python3`, `python3-devel`, `python3-pip` | `python3-devel` ships `libpython3.14.so` in `/usr/lib64` — required by PyO3. |
| Rust 1.96   | `rust`, `cargo`, `rustfmt`, `clippy`      | No rustup — Fedora's packaged rustc matches stable. Tradeoff: no toolchain switching / `rustup target add`. |
| Node 24 LTS | `nodejs24`                                | Active LTS (EOL 2028-04). Provides node + npm in one package; satisfies pi's `>=22.19.0` floor. |
| `just`      | `just`                                    |                                                  |
| `uv`        | `uv`                                      | Fedora packages uv directly (unlike Ubuntu).     |
| mold, lld   | `mold`, `lld`                             | Faster cargo linkers (dev-only perf; see `rust/PERF_RESULTS.md`). mold is the default via the user-level `~/.cargo/config.toml` baked in the Dockerfile; lld is installed as a fallback. Scoped to the devcontainer (NOT committed as `rust/.cargo/config.toml`) so CI and non-devcontainer cloners keep using the default system linker. |
| `tmux`      | `tmux`                                    | baked in (was runtime-installed before)          |
| git / curl  | `git`, `curl`, `ca-certificates`, ...     |                                                  |

Two tools have no dnf package and are curl/npm-installed **in the Dockerfile**
(baked into the image, NOT post-create.sh — the entry point `attach.sh`
does `podman start`+`exec`, which does not run `postCreateCommand`, so
post-create-installed tools went missing after any container recreate):

| Tool    | Source               | Notes                                                                                                          |
|---------|----------------------|----------------------------------------------------------------------------------------------------------------|
| Foundry | `foundryup` (latest) | Blockchain toolchain; no dnf path. `forge`, `cast`, `anvil` in `~/.foundry/bin`. Rebuilds pick up newer Foundry — pin (`foundryup -v <tag>`) if reproducibility matters. |
| `pi`    | `npm i -g @earendil-works/pi-coding-agent` | npm prefix is set to `~/.local` in the Dockerfile, so the binary lands in `~/.local/bin` with no sudo. Matches host version era. |
| `cargo-edit` | `cargo install --locked cargo-edit` | Provides `cargo upgrade`, used by `just update-deps` to bump Cargo.toml version requirements across semver-major boundaries (e.g. revm 41 -> 42), which `cargo update` cannot do alone. Lands in `~/.cargo/bin`; dev-only. |

## Bind mounts (host → container)

| Host                          | Container            | Purpose                                              |
|-------------------------------|----------------------|------------------------------------------------------|
| `${HOME}/.pi`                 | `/home/dev/.pi`      | pi settings, auth, sessions, skill caches, agents    |
| `${HOME}/.agents`             | `/home/dev/.agents`  | external skills (`cast` etc.)                        |
| `${HOME}/code/shared`         | `/shared`            | read-write scratch for copying files across boundary |
| (repo)                        | `/workspaces/degenbot` | the repo (default workspace mount)                 |

`${localEnv:HOME}` substitution makes the mounts portable across machines. Host
uid (1000) matches the `dev` container user, so ownership is consistent on all
bind mounts.

## Cross-boundary file copy

`/shared` (host: `~/code/shared`) is read-write from both sides. Drop artifacts
there from the container, pick them up on the host, and vice versa. Useful for
exporting coverage HTML, wheel builds, etc.

## Files in this directory

| File                | Purpose                                                |
|---------------------|--------------------------------------------------------|
| `Dockerfile`        | `fedora:latest` + dnf layer + `dev` user (uid 1000)  |
| `devcontainer.json` | build/dockerfile, mounts, env vars, `postCreateCommand` |
| `post-create.sh`    | `uv sync` (venv + PyO3 extension build), git hooks            |
| `attach.sh`         | CLI helper: find + start container, tmux attach  |
| `tmux.conf`         | truecolor pass-through + extended-keys for pi in tmux |
| `README.md`         | this file                                            |

(There is no `devcontainer-lock.json` — it only exists to pin devcontainer
*features*, and this setup uses none.)

## Terminal colors (grey-text / red-accent fix)

If pi launched inside tmux via `attach.sh` looks dim (grey text nearly
invisible, highlights collapsed to red), the cause is a dropped color
capability across the container/tmux boundary — not pi's theme itself. Three
things have to line up, all handled by this setup:

1. **`COLORTERM` propagation**: pi reads `$COLORTERM` to decide whether to
   emit 24-bit RGB (see pi's themes doc). `podman exec` only forwards env vars
   you name explicitly, so `attach.sh` passes `--env COLORTERM` (and
   `TERM`) through from your host terminal. Without it pi falls back to a
   16-color palette.
2. **Truecolor pass-through in tmux**: even with COLORTERM set, tmux defaults
   to quantizing RGB escapes to 256-color. `tmux.conf` (baked to `/etc/tmux.conf`
   by the Dockerfile) sets `terminal-overrides ",*:Tc"` so tmux forwards 24-bit
   RGB unmodified to the outer terminal.
3. **Inner TERM**: tmux already defaults `default-terminal` to `tmux-256color`
   (256-color), so the inner shell pi runs in advertises at least 256-color.

The `tmux.conf` also enables `extended-keys`/`extended-keys-format csi-u`
(recommended by pi's `tmux.md`) so `Shift+Enter` / `Ctrl+Enter` / `Alt+Enter`
forward distinctly instead of collapsing to plain Enter.

If colors are still off, verify the chain inside the container tmux session:

```bash
echo "TERM=$TERM COLORTERM=$COLORTERM"   # expect tmux-256color + truecolor
tmux show -gv terminal-overrides         # expect *:Tc present
```

## Notes / caveats

- **No devcontainer features**: python, rust, node, just, uv, and tmux are all in
  the Dockerfile via dnf. If you want to re-add the `python` feature, do NOT
  do so without also ensuring a shared `libpython.so` is on the link path —
  the feature installs a static-only build that breaks PyO3. dnf's
  `python3-devel` is the safe shared-lib variant.
- **Rust is dnf-managed, not rustup**: you get `rustc`/`cargo`/`rustfmt`/`clippy`
  but no `rustup`, so `rustup target add ...` / toolchain pinning don't work. If
  you need cross-compilation targets or pinned toolchains, re-introduce rustup
  (drop the dnf rust packages) in the Dockerfile.
- **mold linker is the devcontainer default** via the user-level
  `~/.cargo/config.toml` baked into the image (NOT a committed
  `rust/.cargo/config.toml`). This is deliberate: a repo-local config would
  force mold on CI (ubuntu-latest, no mold → link failures) and on
  non-devcontainer cloners. Scoping it to the image keeps the repo buildable
  anywhere with the default linker while giving the devcontainer the
  measured build/link speedups (see `rust/PERF_RESULTS.md` lever #1). To
  disable mold locally, delete or edit `~/.cargo/config.toml`; to try lld
  instead, swap `-fuse-ld=mold` for `-fuse-ld=lld` (lld is also installed).
- **Python follows `fedora:latest`** (currently 3.14). The project declares
  `requires-python >= 3.12`, so this is in-spec. `tool.ty.environment
  python-version = "3.12"` is the type-checker's analysis target only — it
  does not constrain the runtime.
- **Node follows `fedora:latest`** via `nodejs24` (Active LTS, EOL 2028-04). pi's
  `engines.node` floor is `>=22.19.0`; `nodejs24` satisfies it with ~2 years of
  runway. If pi ever pins a max node, re-check before bumping `fedora:latest`.
- **pi sessions are shared**: the bind-mounted `~/.pi` means in-container pi and
  host pi see the same sessions/auth. Don't run both against the same session
  simultaneously — they'd race on the VCC state. Typical workflow: host pi
  retreats while container pi works, or vice versa.
- **Container-local venv (`UV_PROJECT_ENVIRONMENT`)**: the container's venv
  lives at `/home/dev/.venvs/degenbot` (container writable layer, NOT the
  bind-mounted repo), so its `pyvenv.cfg` and script shebangs bake
  `/home/dev/...` paths that are never read by the host. The host keeps its own
  in-repo `.venv` with `/home/btd/...` paths. The two venvs never poison each
  other. A `--remove-existing-container` rebuild wipes the container venv;
  `post-create.sh` recreates it via `uv sync`.
- **PyO3 needs `libpython.so`**: provided by dnf's `python3-devel`. Don't remove
  `python3-devel` from the Dockerfile or the extension build will fail.
- **`maturin develop` not run on create**: `uv sync` builds the extension via
  PEP 517 (maturin backend) as the editable install. Run `just dev` only for a
  one-shot rebuild after changing Rust sources without wanting a full sync.
- **Foundry is "latest"**: `foundryup` runs without a pin, so rebuilds may pick
  up newer Foundry releases.
- **Podman, not Docker**: mounts are plain `type=bind` (no `consistency=cached`,
  which is Docker-Desktop-only). VSCode's `dev.containers.dockerPath: "podman"`
  setting is required because the devcontainer runtime defaults to Docker.