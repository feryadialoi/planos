#!/usr/bin/env bash
# Install or remove PlanOS: the `planos` binary and the Claude Code plugin.
#
#   ./install.sh               from a clone: build this checkout
#   curl -fsSL https://raw.githubusercontent.com/feryadialoi/planos/main/install.sh | bash
#                              without a clone: build from GitHub
#   ./install.sh --uninstall   remove the plugin, the marketplace and the binary
set -euo pipefail

REPO="feryadialoi/planos"
PLUGIN="planos@planos"
MARKETPLACE="planos"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "$1 not found. $2"; }

# The checkout this script lives in, or empty when piped from curl.
here=""
if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
  dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  [ -f "$dir/Cargo.toml" ] && [ -d "$dir/.claude-plugin" ] && here="$dir"
fi

has_marketplace() { claude plugin marketplace list 2>/dev/null | grep -Eq "[[:space:]]${MARKETPLACE}[[:space:]]*$"; }

uninstall() {
  need claude "Install Claude Code first: https://claude.com/claude-code"
  say "Removing the plugin"
  claude plugin uninstall "$PLUGIN" || true
  if has_marketplace; then
    say "Removing the marketplace"
    claude plugin marketplace remove "$MARKETPLACE" || true
  fi
  if command -v planos >/dev/null 2>&1; then
    say "Stopping the service"
    planos stop || true
  fi
  if command -v cargo >/dev/null 2>&1; then
    say "Removing the binary"
    cargo uninstall planos || true
  fi
  say "Done. Saved plans stay in ~/.planos; delete that folder to remove them."
}

install() {
  need cargo "Install Rust first: https://rustup.rs"
  need claude "Install Claude Code first: https://claude.com/claude-code"

  if [ -n "$here" ]; then
    say "Building planos from $here"
    cargo install --locked --path "$here"
    source="$here"
  else
    say "Building planos from github.com/$REPO"
    cargo install --locked --git "https://github.com/$REPO"
    source="$REPO"
  fi

  if has_marketplace; then
    say "Updating the marketplace"
    claude plugin marketplace update "$MARKETPLACE"
  else
    say "Adding the marketplace ($source)"
    claude plugin marketplace add "$source"
  fi

  say "Installing the plugin"
  claude plugin install "$PLUGIN" || claude plugin update "$PLUGIN"

  # A running service from an older build is replaced on the next hook call,
  # but stopping it now makes the new version take effect right away.
  "$(command -v planos)" stop >/dev/null 2>&1 || true

  say "Installed $(planos --version)."
  echo "Restart Claude Code (or run /reload-plugins), enter plan mode with shift+tab and ask for a plan."
}

case "${1:-}" in
  "") install ;;
  --uninstall) uninstall ;;
  -h|--help) sed -n '2,7p' "${BASH_SOURCE[0]:-/dev/null}" | sed 's/^# \{0,1\}//' ;;
  *) die "unknown option: $1 (use --uninstall or --help)" ;;
esac
