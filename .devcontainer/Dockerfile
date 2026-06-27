# degenbot devcontainer base.
#
# Replaces the prior mcr.microsoft.com/devcontainers/base:ubuntu + features
# approach. Everything dnf-installable is baked in here at build time, so
# container starts are fast and post-create.sh stays tiny.
#
# On Fedora 44, TWO tools have no dnf package and are curl/npm-installed here
# at BUILD time (baked into the image, not post-create.sh — see below):
#   - foundry  (foundryup; blockchain toolchain, no dnf path)
#   - pi       (npm i -g @earendil-works/pi-coding-agent)
#
# Why bake into the image rather than post-create.sh: the primary entry point
# (ssh-attach.sh) does `podman start` + `podman exec`, which does NOT run
# postCreateCommand. Anything left to post-create would vanish after a
# container recreate (foundry/pi live in /home/dev, which is NOT bind-mounted)
# and never come back via the SSH path. Image-baked tools are always present.
#
# uv IS packaged by Fedora (unlike Ubuntu) — installed via dnf here.
#
# Base tag is intentionally floating (fedora:latest) per project preference
# for up-to-date toolchains. Pin to a digest (`fedora:latest@sha256:...`) if
# reproducibility ever matters more than freshness.
FROM fedora:latest

# One dnf layer with every OS-level prereq the project needs.
#   - gcc, gcc-c++, make, pkgconf-pkg-config, python3-devel : PyO3/maturin
#       extension linking. Fedora's python3-devel ships libpython3.1x.so in
#       /usr/lib64 — required by PyO3. (The devcontainers python *feature*
#       installs a STATIC-only build that breaks PyO3; dnf's python3-devel is
#       a normal shared-lib build, so it's fine.)
#   - python3, python3-pip            : runtime (3.14 on fedora:latest)
#   - rust, cargo, rustfmt, clippy    : Rust toolchain for the extension.
#       No rustup — Fedora's 1.96.x matches stable. Tradeoff: no toolchain
#       switching / `rustup target add`.
#   - nodejs24                        : provides node + npm for pi (>=22.19)
#   - just, uv, tmux, git, curl, ca-certificates, openssh-clients, less, sudo
#   - bash-completion               : sources /etc/bash_completion.d/{cargo,fzf}
#                                      for git/dnf/systemctl/etc. Without it the
#                                      cargo/fzf completion scripts dnf drops
#                                      there are never loaded.
RUN dnf install -y \
        gcc gcc-c++ make pkgconf-pkg-config \
        uv python3 python3-devel python3-pip \
        rust rust-src cargo rustfmt clippy \
        golang \
        nodejs24 \
        sqlite \
        git \
        just \
        sudo tmux vim htop procps-ng psmisc \
        glibc-langpack-en \
        bash-completion \
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
# when launched inside tmux via ssh-attach.sh (truecolor pass-through +
# extended-keys). Applies to all users; a personal ~/.tmux.conf overrides it.
COPY --chown=root:root tmux.conf /etc/tmux.conf

# Drop the hostname from the bash prompt. Podman (under VSCode's devcontainer
# path) strips CAP_SYS_ADMIN, so the container ID is used as the hostname and
# \h renders as e.g. `e92c9419732a` — unfriendly. /etc/bashrc sets the
# default `PS1='[\u@\h \W]\\$ '`; this appended line strips the `@\h` segment
# so the prompt reads `[dev degenbot]$` instead. Runs for both login and
# non-login interactive shells (tmux sessions, `podman exec`).
RUN printf '\nPS1="${PS1//@\\\\h/}"\n' >> /etc/bashrc

USER ${USERNAME}
ENV HOME=/home/${USERNAME}

# Put foundry (~/.foundry/bin, curl-installed below) and the npm-global pi
# binary (~/.local/bin) on PATH for EVERY process spawned in the image — not
# just interactive shells. devcontainer.json's remoteEnv only reaches
# VSCode-launched shells; non-interactive subshells (e.g. `just`/`cargo`
# invoked by the agent) inherit image ENV instead, so bake it here.
ENV PATH="/home/${USERNAME}/go/bin:/home/${USERNAME}/.foundry/bin:/home/${USERNAME}/.local/bin:${PATH}"

# Configure npm's global prefix to a user-writable dir so `npm i -g` needs no
# sudo, and the bins land in ~/.local/bin (already on PATH via ENV above).
# Pre-create ~/.config as dev-owned so the devcontainer.json bind mount of
# ~/.config/degenbot finds its parent already present. Otherwise podman
# auto-creates missing bind-target parents as root:root, which then blocks
# the dev user from writing sibling dotfile dirs under ~/.config (and leaves
# the dir as root-owned with a sticky bit).
RUN mkdir -p "${HOME}/.config" "${HOME}/.local/bin" \
    && npm config set prefix "${HOME}/.local"

# Foundry (forge/cast/anvil/chisel). No dnf package, so curl-install foundryup
# + run it at BUILD time. Baked into the image (not post-create.sh) because the
# primary entry point (ssh-attach.sh) does `podman start` + `podman exec`, which
# does NOT run postCreateCommand — so anything left to post-create would be
# missing after a container recreate + SSH re-attach. foundryup lands in
# ~/.foundry/bin, already on PATH via ENV above. Rebuild the image to pick up
# newer Foundry; pin with `foundryup -v <tag>` if reproducibility matters.
RUN curl -L https://foundry.paradigm.xyz | bash \
    && /home/${USERNAME}/.foundry/bin/foundryup

# pi (npm global). Lands in ~/.local/bin via the prefix set above. Baked into
# the image for the same ssh-attach reason as foundry. Matches host version era;
# rebuild the image to bump.
RUN npm install -g @earendil-works/pi-coding-agent
RUN go install github.com/sandover/ergo/cmd/ergo@latest

# Generate shell completions for the four curl/npm/go-installed CLIs into
# ~/.bashrc.d/, which ~/.bashrc already loops over and sources at shell
# startup. Baked into the image (not post-create.sh) for the same ssh-attach
# reason as foundry/pi: the SSH entry point does `podman start`+`exec` and
# never runs postCreateCommand, so anything left to post-create would be
# missing after a recreate + SSH re-attach. /home/dev is on the overlay layer
# (not bind-mounted), so regenerating here at build time is what makes them
# survive rebuilds. `bash-completion` (in the dnf layer above) separately
# sources /etc/bash_completion.d/{cargo,fzf,...} for git/dnf/systemctl/etc.
RUN mkdir -p "${HOME}/.bashrc.d" \
    && JUST_COMPLETE=bash just > "${HOME}/.bashrc.d/just-completion.sh" \
    && uv generate-shell-completion bash > "${HOME}/.bashrc.d/uv-completion.sh" \
    && ergo completion bash > "${HOME}/.bashrc.d/ergo-completion.sh" \
    && cast completions bash > "${HOME}/.bashrc.d/cast-completion.sh"

WORKDIR /workspaces/degenbot
