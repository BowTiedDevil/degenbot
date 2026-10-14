# degenbot devcontainer base.

FROM fedora:latest

RUN dnf install -y \
        gcc gcc-c++ make pkgconf-pkg-config \
        uv python3 python3-devel python3-pip \
        gh \
        golang \
        nodejs24 \
        sqlite \
        git \
        just \
        ripgrep \
        # Faster linkers for cargo (dev-only perf; analysis lived in rust/PERF_RESULTS.md (dropped in `f082db7a7`)).
        # mold is the default via the user-level ~/.cargo/config.toml below;
        # lld is installed as a fallback (switch via that config).
        mold lld \
        sudo tmux vim htop procps-ng psmisc \
        glibc-langpack-en \
        hostname \
        bash-completion \
        direnv \
        xdg-utils \
    && dnf clean all

# Create the dev user (uid 1000 to match host uid — avoids updateRemoteUserUID
# dances on bind mounts). The fedora:latest image ships only root, so we make
# the user ourselves (replaces the vscode user from the ms base image).
ARG USERNAME=dev
ARG USER_UID=1000
ARG USER_GID=1000
RUN groupadd --gid "${USER_GID}" "${USERNAME}" \
    && useradd --uid "${USER_UID}" --gid "${USER_GID}" --create-home \
        --shell /bin/bash "${USERNAME}" \
    && echo "${USERNAME} ALL=(root) NOPASSWD:ALL" \
        > /etc/sudoers.d/${USERNAME} \
    && chmod 0440 /etc/sudoers.d/${USERNAME}

# UTF-8 locale so Python/tz behave. The ``C.UTF-8`` ENV defaults assume only
# ``C.utf8`` exists; but VSCode's git extension runs on the host with
# ``LC_ALL=en_US.UTF-8`` and forwards that env into the container when it
# spawns git. Without the matching locale data installed (above:
# ``glibc-langpack-en``) glibc's ``setlocale`` fails and every git invocation
# prints ``cannot change locale (en_US.UTF-8): No such file or directory``.
# Keeping both the langpack AND the ``C.UTF-8`` default means the container's
# own processes stay on ``C.UTF-8`` while an injected ``en_US.UTF-8`` resolves
# instead of warning.
ENV LANG=C.UTF-8
ENV LC_ALL=C.UTF-8

# Bake tmux config into the image so pi's 24-bit RGB theme renders correctly
# when launched inside tmux via attach.sh (truecolor pass-through +
# extended-keys). Applies to all users; a personal ~/.tmux.conf overrides it.
COPY --chown=root:root tmux.conf /etc/tmux.conf

# Drop the hostname from the bash prompt. Podman (under VSCode's devcontainer
# path) strips CAP_SYS_ADMIN, so the container ID is used as the hostname and
# \h renders as e.g. `e92c9419732a` — unfriendly. /etc/bashrc sets the
# default `PS1='[\u@\h \W]\\$ '`; this appended line strips the `@\h` segment
# so the prompt reads `[dev degenbot]$` instead. Runs for both login and
# non-login interactive shells (tmux sessions, `podman exec`).
RUN printf '\nPS1="${PS1//@\\\\h/}"\n' >> /etc/bashrc

# Hook direnv into every interactive shell (all users). Loading the repo
# .envrc is a deliberate no-op inside the container (it is guarded on
# /run/.containerenv and containerEnv in devcontainer.json owns those vars
# here); the hook exists so the bind-mounted .envrc is safe to load from
# either side of the boundary.
RUN printf '\neval "$(direnv hook bash)"\n' >> /etc/bashrc

# Silence direnv's loading/unload/export chatter (direnv.toml(5)): log_filter
# is a REGEX ALLOWLIST on status lines — a never-matching pattern drops them;
# logError bypasses the filter, so real errors still print.
# Whitelist the bind-mounted repo so a container rebuild (which wipes the dev
# user's home, including direnv's allow records) does not re-prompt: the .envrc
# is a no-op in-container (guarded on /run/.containerenv) and first-party.
#
# chowns the WHOLE ~/.config (not just direnv/): this step created it as root
# and it must be dev-owned before the USER switch below — otherwise the dev
# user cannot write sibling dotfile dirs under ~/.config. It also pre-empts
# podman auto-creating the devcontainer.json bind-mount parent
# (~/.config/degenbot) as root:root at container-create time.
RUN mkdir -p /home/dev/.config/direnv && \
    printf 'hide_env_diff = true\nlog_filter = "error"\n\n[whitelist]\nprefix = [ "/workspaces/degenbot" ]\n' \
        > /home/dev/.config/direnv/direnv.toml && \
    chown -R dev:dev /home/dev/.config

USER ${USERNAME}
ENV HOME=/home/${USERNAME}

# Put foundry (~/.foundry/bin), pi (~/.local/bin), and rustup's proxies +
# cargo subcommands (~/.cargo/bin) on PATH for EVERY
# process spawned in the image — both curl-installed below — not
# just interactive shells. devcontainer.json's remoteEnv only reaches
# VSCode-launched shells; non-interactive subshells (e.g. `just`/`cargo`
# invoked by the agent) inherit image ENV instead, so bake it here.
ENV PATH="/home/${USERNAME}/go/bin:/home/${USERNAME}/.foundry/bin:/home/${USERNAME}/.local/bin:/home/${USERNAME}/.cargo/bin:${PATH}"

# Configure npm's global prefix to a user-writable dir so `npm i -g`
# (agent-browser, below) needs no sudo, and the bins land in ~/.local/bin
# (already on PATH via ENV above).
# NOTE: ~/.config is pre-created dev-owned in the root step above (a chown
# here as the dev user would fail with EPERM against the root-owned dir that
# step created). This step only needs the user-local dirs + npm prefix.
RUN mkdir -p "${HOME}/.config" "${HOME}/.local/bin" \
    && npm config set prefix "${HOME}/.local"

# Foundry (forge/cast/anvil/chisel). No dnf package, so curl-install foundryup
# + run it at BUILD time. Baked into the image (not post-create.sh) because the
# primary entry point (attach.sh) does `podman start` + `podman exec`, which
# does NOT run postCreateCommand — so anything left to post-create would be
# missing after a container recreate + re-attach. foundryup lands in
# ~/.foundry/bin, already on PATH via ENV above. Rebuild the image to pick up
# newer Foundry; pin with `foundryup -v <tag>` if reproducibility matters.
RUN curl -L https://foundry.paradigm.xyz | bash \
    && /home/${USERNAME}/.foundry/bin/foundryup

# pi (curl-installed via pi.dev's install script). Baked into the image for
# the same attach reason as foundry. Matches host version era; rebuild the
# image to bump. The managed launcher lands in ~/.local/bin (already on PATH
# via ENV above, pre-created in the npm-prefix step); the trailing
# `command -v pi && pi --version` fails the build loudly if it didn't.
#
# PI_CODING_AGENT_DIR deliberately relocates the managed install OUT of
# ~/.pi: devcontainer.json bind-mounts the host ~/.pi over /home/dev/.pi at
# RUN time (shared extensions/auth/state), which would shadow the image's
# ~/.pi/agent/install tree and leave ~/.local/bin/pi a dangling symlink —
# "pi: not found" despite correct PATH (this build-time guard cannot see
# runtime mounts). Relocation keeps the pinned install/releases tree (the
# reason this installer is used over bare `npm i -g`, which floats
# transitive deps) outside the mount, while runtime STATE still resolves to
# ~/.pi/agent by default (pi dist/config.js) — extensions/auth keep flowing
# through the mount. The env is scoped to this RUN via export, NOT baked as
# ENV: the runtime reads the same variable and would move state too.
# `export VAR && curl | sh` (not `VAR=x curl | sh`) is deliberate — a prefix
# assignment binds to curl only and the installer's sh never sees it.
RUN export PI_CODING_AGENT_DIR="/home/${USERNAME}/.local/share/pi-agent" \
    && curl -fsSL https://pi.dev/install.sh | sh \
    && command -v pi \
    && pi --version
RUN npm install -g agent-browser
RUN agent-browser install --with-deps
RUN npx skills add vercel-labs/agent-browser -g --agent pi --yes

RUN go install github.com/sandover/ergo/v6/cmd/ergo@latest

# prek (git hook runner) as a global uv tool. Lands in ~/.local/bin via the
# uv tool bin dir (already on PATH via ENV above). Baked into the image for
# the same attach reason as foundry/pi/ergo: attach.sh does `podman start` +
# `podman exec`, which does NOT run postCreateCommand — so `just
# setup-git-hooks` (run by post-create.sh) needs prek already on PATH. Using
# the global uv tool (not the project venv) keeps the hooks prek generates
# from pinning a throwaway .venv path; they fall back to `prek` on PATH.
RUN uv tool install prek

# rustup is the toolchain manager (replacing the dnf `rust`/`cargo` stack).
# WHY: distro cargo ignores rust-toolchain.toml, so the channel pin was inert
# here and the toolchain moved underneath it with a routine image rebuild (
# this README documented 1.96 while `rustc --version` had drifted to Fedora's
# 1.98.1 rebuild) -- and a distro rebuild is a different binary than CI's
# upstream toolchain, one more local/CI codegen divergence (the 2026-10-14
# ADR-071 D3 identity-gate failure was exactly a codegen-layout sensitivity).
# rustup enforces the pin, provides the MSRV toolchain behind `just check-msrv`
# and gives local builds the same upstream toolchain binary CI installs.
# Versions track rust-toolchain.toml (channel) and rust/Cargo.toml's
# `rust-version` (MSRV) -- bump them together. Components cover the lint/format
# gates, rust-src (rust-analyzer), and llvm-tools-preview (cargo-llvm-cov finds
# it by itself under a rustup toolchain; the coverage recipe says as much).
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --profile minimal --default-toolchain 1.98.1 \
    && rustup component add --toolchain 1.98.1 clippy rustfmt rust-src llvm-tools-preview \
    && rustup toolchain install 1.97.0 --profile minimal

# `cargo-edit` provides `cargo upgrade` (rewrites Cargo.toml version requirements
# to latest published, across semver majors — `cargo update` alone can't cross
# a major boundary), used by `just update-deps`.
RUN cargo install --locked cargo-edit cargo-nextest

# User-level cargo config: use `mold` as the linker for all cargo builds
# inside the container (dev-only perf; see rust/PERF_RESULTS.md (dropped in `f082db7a7`) lever #1).
# Lives at ~/.cargo/config.toml (cargo's home config, read in ADDITION to
# any repo-local .cargo/config.toml) so it travels with the image and
# applies to every cargo invocation — including rust-analyzer, just recipes,
# and the uv/maturin PyO3 extension rebuild — regardless of entry path
# (VSCode 'Reopen in Container' OR attach.sh's `podman start`+`exec`).
#
# This is INTENTIONALLY NOT committed as rust/.cargo/config.toml: that would
# force mold on CI (runs on ubuntu-latest without mold → link failures) and
# on non-devcontainer cloners. Scoping mold to the devcontainer image keeps
# the repo buildable anywhere with the default system linker. mold was
# measured to cut the clean build 59→51s, the test-binary build 64→48s, and
# the degenbot_rs cdylib link 27→19.5s. To try lld instead, swap
# `-fuse-ld=mold` for `-fuse-ld=lld` (lld is installed above as a fallback).
RUN mkdir -p "${HOME}/.cargo" \
    && printf '%s\n' \
        '# Devcontainer-only: use mold linker for faster cargo links.' \
        '# Perf analysis lived in rust/PERF_RESULTS.md (dropped in f082db7a7). Remove or swap to lld if mold breaks.' \
        '[target.x86_64-unknown-linux-gnu]' \
        'rustflags = ["-C", "link-arg=-fuse-ld=mold"]' \
        > "${HOME}/.cargo/config.toml"

# Generate shell completions for the four curl/npm/go-installed CLIs into
# ~/.bashrc.d/, which ~/.bashrc already loops over and sources at shell
# startup. Baked into the image (not post-create.sh) for the same attach
# reason as foundry/pi: the attach entry point does `podman start`+`exec` and
# never runs postCreateCommand, so anything left to post-create would be
# missing after a recreate + re-attach. /home/dev is on the overlay layer
# (not bind-mounted), so regenerating here at build time is what makes them
# survive rebuilds. `bash-completion` (in the dnf layer above) separately
# sources /etc/bash_completion.d/{cargo,fzf,...} for git/dnf/systemctl/etc.
RUN mkdir -p "${HOME}/.bashrc.d" \
    && JUST_COMPLETE=bash just > "${HOME}/.bashrc.d/just-completion.sh" \
    && uv generate-shell-completion bash > "${HOME}/.bashrc.d/uv-completion.sh" \
    && ergo completion bash > "${HOME}/.bashrc.d/ergo-completion.sh" \
    && cast completions bash > "${HOME}/.bashrc.d/cast-completion.sh"

WORKDIR /workspaces/degenbot
