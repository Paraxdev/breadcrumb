#!/bin/sh
# Installs or updates breadcrumb on macOS and Linux and sets it up for Claude Code.
#   curl -fsSL https://raw.githubusercontent.com/Paraxdev/breadcrumb/main/install.sh | sh
# Run it again any time to update. BREADCRUMB_REPO picks a fork (owner/name).
set -eu

repo="${BREADCRUMB_REPO:-Paraxdev/breadcrumb}"
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) asset=breadcrumb-linux-x64.tar.gz ;;
  Darwin-arm64) asset=breadcrumb-macos-arm64.tar.gz ;;
  Darwin-x86_64) asset=breadcrumb-macos-x64.tar.gz ;;
  *) asset="" ;;
esac

if command -v breadcrumb >/dev/null 2>&1; then
  dir="$(dirname "$(command -v breadcrumb)")"
elif [ -d "$HOME/.cargo/bin" ]; then
  dir="$HOME/.cargo/bin"
else
  dir="$HOME/.local/bin"
fi
mkdir -p "$dir"

installed=""
if [ -n "$asset" ]; then
  tmp="$(mktemp -d)"
  if curl -fsSL -o "$tmp/$asset" "https://github.com/$repo/releases/latest/download/$asset" 2>/dev/null; then
    tar -xzf "$tmp/$asset" -C "$tmp"
    # Replacing by rename keeps a running MCP server on its old inode.
    for b in breadcrumb crumb; do
      cp "$tmp/$b" "$dir/.$b.new" && chmod +x "$dir/.$b.new" && mv -f "$dir/.$b.new" "$dir/$b"
    done
    installed=1
    echo "installed the latest breadcrumb release into $dir"
  else
    echo "no release download for $asset, building from source"
  fi
  rm -rf "$tmp"
fi

if [ -z "$installed" ]; then
  command -v cargo >/dev/null 2>&1 || { echo "no release to download and no cargo to build with, install Rust from https://rustup.rs and run this again" >&2; exit 1; }
  cargo install --git "https://github.com/$repo" breadcrumb --force --root "$(dirname "$dir")"
fi

case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "add $dir to your PATH, for example: echo 'export PATH=\"$dir:\$PATH\"' >> ~/.profile"; PATH="$dir:$PATH" ;;
esac
exe="$dir/breadcrumb"

"$exe" init
if command -v claude >/dev/null 2>&1; then
  claude mcp get breadcrumb >/dev/null 2>&1 || claude mcp add --scope user breadcrumb -- "$exe" mcp
else
  echo "Claude Code not found, register later with: $("$exe" mcp-config)"
fi
"$exe" hooks install
[ -f .breadcrumb/project.toml ] && "$exe" share pull || true
echo "$("$exe" --version) ready. Restart Claude Code so its MCP server and hooks use the new version."
