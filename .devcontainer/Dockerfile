# degenbot devcontainer base.

FROM fedora:latest

RUN dnf install -y \
        gcc gcc-c++ make pkgconf-pkg-config \
        uv python3 python3-devel python3-pip \
        rust rust-src cargo rustfmt clippy \
        golang \
        nodejs24 \
        sqlite \
        git \
        just \
        ripgrep \
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
# primary entry point (attach.sh) does `podman start` + `podman exec`, which
# does NOT run postCreateCommand — so anything left to post-create would be
# missing after a container recreate + re-attach. foundryup lands in
# ~/.foundry/bin, already on PATH via ENV above. Rebuild the image to pick up
# newer Foundry; pin with `foundryup -v <tag>` if reproducibility matters.
RUN curl -L https://foundry.paradigm.xyz | bash \
    && /home/${USERNAME}/.foundry/bin/foundryup

# pi (npm global). Lands in ~/.local/bin via the prefix set above. Baked into
# the image for the same attach reason as foundry. Matches host version era;
# rebuild the image to bump.
RUN npm install -g @earendil-works/pi-coding-agent
RUN npm install -g agent-browser
RUN agent-browser install
RUN npx skills add vercel-labs/agent-browser -g --agent pi --yes

RUN go install github.com/sandover/ergo/cmd/ergo@latest

# prek (git hook runner) as a global uv tool. Lands in ~/.local/bin via the
# uv tool bin dir (already on PATH via ENV above). Baked into the image for
# the same attach reason as foundry/pi/ergo: attach.sh does `podman start` +
# `podman exec`, which does NOT run postCreateCommand — so `just
# setup-git-hooks` (run by post-create.sh) needs prek already on PATH. Using
# the global uv tool (not the project venv) keeps the hooks prek generates
# from pinning a throwaway .venv path; they fall back to `prek` on PATH.
RUN uv tool install prek

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
