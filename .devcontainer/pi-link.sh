# Auto-connect interactive `pi` sessions to pi-link on startup.
#
# pi-link is off by default; only `--link`/`--link-name`/`pi-link`/`/link-connect`
# opt in (see the pi-link README "Configuration" section). This wraps `pi` so a
# plain interactive launch (`pi`, `pi "do X"`, `pi --resume`, ...) gets `--link`
# prepended automatically, while pi's management/CLI subcommands — which print
# and exit rather than starting a session — are passed through untouched.
#
# Sourced from ~/.bashrc (which loops over ~/.bashrc.d/*.sh) and from ~/.zshrc
# where relevant. Bash functions are unavailable in non-interactive shells, so
# scripted/`podman exec` `pi` calls are unaffected. Baked into the image (not
# post-create.sh) for the same attach reason as the ~/.bashrc.d completions:
# attach.sh does `podman start`+`exec` and never runs postCreateCommand, and
# /home/dev is overlay (not bind-mounted), so a runtime write would not survive
# a rebuild. See `pi --help` for the subcommand list.

# Subcommands that print-and-exit (never start a session, no point connecting).
_pi_link_nop_list=(
  install
  remove
  uninstall
  update
  list
  config
)
# Flags handled by pi's own arg parser before extension startup (never connect).
_pi_link_nop_flags=(
  --help
  -h
  --version
  -v
)

pi() {
  if [ "$#" -gt 0 ]; then
    first="$1"
    for cmd in "${_pi_link_nop_list[@]}"; do
      [ "$first" = "$cmd" ] && { command pi "$@"; return $?; }
    done
    for flag in "${_pi_link_nop_flags[@]}"; do
      [ "$first" = "$flag" ] && { command pi "$@"; return $?; }
    done
  fi
  command pi --link "$@"
}