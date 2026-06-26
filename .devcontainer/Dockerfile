# degenbot devcontainer base.
#
# Replaces the prior mcr.microsoft.com/devcontainers/base:ubuntu + features
# approach. Everything dnf-installable is baked in here at build time, so
# container starts are fast and post-create.sh stays tiny.
#
# On Fedora 44, ONE tool still has no package and is curl-installed in
# post-create.sh:
#   - foundry  (foundryup; blockchain toolchain, no dnf path)
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
RUN dnf install -y \
        gcc gcc-c++ make pkgconf-pkg-config \
        python3 python3-devel python3-pip \
        rust cargo rustfmt clippy \
        nodejs24 \
        just uv tmux git curl ca-certificates openssh-clients less sudo \
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

# UTF-8 locale so Python/tz behave.
ENV LANG=C.UTF-8
ENV LC_ALL=C.UTF-8

# Bake tmux config into the image so pi's 24-bit RGB theme renders correctly
# when launched inside tmux via ssh-attach.sh (truecolor pass-through +
# extended-keys). Applies to all users; a personal ~/.tmux.conf overrides it.
COPY --chown=root:root tmux.conf /etc/tmux.conf

USER ${USERNAME}
ENV HOME=/home/${USERNAME}

# Configure npm's global prefix to a user-writable dir so `npm i -g` (used for
# pi in post-create.sh) needs no sudo, and the bins land in ~/.local/bin —
# already on PATH via devcontainer.json's remoteEnv.
RUN mkdir -p "${HOME}/.local/bin" \
    && npm config set prefix "${HOME}/.local"

WORKDIR /workspaces/degenbot