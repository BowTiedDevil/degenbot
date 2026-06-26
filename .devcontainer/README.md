# degenbot devcontainer

VSCode auto-discovers `devcontainer.json` in this directory.

**Container runtime: Podman** (not Docker). Mounts are plain `type=bind` (no
`consistency=cached`, which is Docker-Desktop-only). VSCode and the devcontainer
CLI both need to be pointed at `podman` (see setup below).

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

3. Build the container once (chose one):
   - **VSCode:** open `/home/ralph/code/degenbot` → Command Palette →
     `Dev Containers: Reopen in Container`.
   - **SSH / CLI:** `devcontainer up --workspace-folder /home/ralph/code/degenbot --docker-path podman`
     (requires `npm i -g @devcontainers/cli`).

   First build is slow (image pull + features + `post-create.sh`). Both paths
   produce a container named `vsc-degenbot-<hash>` and are interchangeable — a
   container built by one is found and reused by the other.

## Entering the container

### A. SSH terminal (your default)

```bash
.devcontainer/ssh-attach.sh
```

This script finds the `vsc-degenbot-*` container, starts it if stopped, and
drops into a tmux session named `pi`. No flags to remember.

Equivalent manual commands:

```bash
podman ps -a --filter name=vsc-degenbot --format '{{.Names}}'   # find container name
podman start <name>
podman exec -it <name> tmux new -As pi
```

To rebuild from scratch from SSH:
```bash
.devcontainer/ssh-attach.sh rebuild    # needs devcontainer CLI
```

### B. VSCode

Open the repo → `Dev Containers: Reopen in Container`, or if the container is
already running (from either path), `Dev Containers: Attach to Running
Container...` → pick `vsc-degenbot-*`. Same filesystem as the tmux/pi session —
no state fork.

## What's inside

| Tool       | Source                                   | Notes                                               |
|------------|------------------------------------------|-----------------------------------------------------|
| Rust       | `rustup` from sh.rustup.rs in post-create | `rustup default stable` tracks latest stable      |
| Python 3.12 | uv-managed (NOT the devcontainers feature) | devcontainers python feature installs a STATIC-only build (no `libpython.so`) which breaks PyO3. uv's own cpython-3.12 ships the shared library, so the PyO3 extension builds. System python on the base image (3.14) stays on PATH for general use. |
| `uv`       | astral.sh install script                  | latest, in `~/.local/bin/uv`                        |
| `just`     | prebuilt musl static binary               | latest release, in `~/.local/bin/just`              |
| Foundry    | `foundryup` (latest)                     | `forge`, `cast`, `anvil` in `~/.foundry/bin`        |
| Node LTS   | devcontainers/features/node              | for markdownlint + commitlint hooks                 |
| `pi`       | `npm i -g @earendil-works/pi-coding-agent`| matches host version era                            |

## Bind mounts (host → container)

| Host                          | Container                | Purpose                                             |
|-------------------------------|--------------------------|-----------------------------------------------------|
| `${HOME}/.pi`                 | `/home/vscode/.pi`       | pi settings, auth, sessions, skill caches, agents    |
| `${HOME}/.agents`             | `/home/vscode/.agents`    | external skills (`cast` etc.)                       |
| `${HOME}/code/shared`         | `/shared`               | read-write scratch for copying files across boundary |
| (repo)                        | `/workspaces/degenbot`    | the repo (default workspace mount)                  |

`${localEnv:HOME}` substitution makes the mounts portable across machines, not
ralph-hardcoded. Host uid (1000) matches the `vscode` container user, so
ownership is consistent on all bind mounts.

## Cross-boundary file copy

`/shared` (host: `~/code/shared`) is read-write from both sides. Drop artifacts
there from the container, pick them up on the host, and vice versa. Useful for
exporting coverage HTML, wheel builds, etc.

## Files in this directory

| File                     | Purpose                                                |
|--------------------------|--------------------------------------------------------|
| `devcontainer.json`      | image, features, mounts, env vars, `postCreateCommand`  |
| `post-create.sh`         | installs tmux/uv/rust/just/foundry/pi, `uv sync`, hooks |
| `ssh-attach.sh`          | SSH/CLI helper: find + start container, tmux attach    |
| `devcontainer-lock.json` | pinned feature digests for reproducibility — commit it |
| `README.md`              | this file                                              |

## Notes / caveats

- **pi sessions are shared**: the bind-mounted `~/.pi` means in-container pi
  and host pi see the same sessions/auth. Don't run both against the same
  session simultaneously — they'd race on the VCC state. Typical workflow:
  host pi retreats while container pi works, or vice versa.
- **Container-native `.venv`**: the venv is created inside the container with
  uv-managed python 3.12 (symlinks point to `/home/vscode/...` paths). These
  don't exist on the host, so a `.venv` built in the container won't work for
  host-side development, and vice versa. If you switch environments, remove
  `.venv` and let the active one recreate it (`uv sync` on host; `uv venv
  --python 3.12 && uv sync` in container).
- **PyO3 needs `libpython.so`**: the devcontainers python feature installs a
  static-only python (no shared library), which breaks the PyO3 extension
  build. `post-create.sh` installs python 3.12 via `uv` (ships the shared
  library) and creates the venv explicitly with it. Don't re-add the
  `ghcr.io/devcontainers/features/python` feature without also ensuring a
  shared libpython is on the link path.
- **Rust toolchain auto-updates**: `post-create.sh` installs rustup from
  sh.rustup.rs and sets `rustup default stable`, so rustc tracks current
  stable (1.96.0 at time of writing) regardless of the base image.
- **`maturin develop` not run on create**: `uv sync` builds the extension via
  PEP 517 (maturin backend) as the editable install. Run `just dev` only for a
  one-shot rebuild after changing Rust sources without wanting a full sync.
- **Foundry is "latest"**: `foundryup` runs without a pin, so rebuilds may
  pick up newer Foundry releases. Re-pin in `post-create.sh`
  (`foundryup -v <tag>`) if reproducibility matters.
- **Node version drift**: `pi` is installed against the container's Node LTS.
  If you bump the host's pi major version, reinstall in-container to match.
- **Podman, not Docker**: mounts are plain `type=bind` (no `consistency=cached`,
  which is Docker-Desktop-only). VSCode's `dev.containers.dockerPath: "podman"`
  setting is required because the devcontainer runtime defaults to Docker.