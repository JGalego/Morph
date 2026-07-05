#!/bin/sh
# Installs the latest morph release binary for your platform.
#
#   curl -fsSL https://raw.githubusercontent.com/JGalego/Morph/main/install.sh | sh
#
# Set MORPH_INSTALL_DIR to change where the binary is placed (default:
# /usr/local/bin, falling back to ~/.local/bin if that isn't writable).
set -eu

REPO="JGalego/Morph"
INSTALL_DIR="${MORPH_INSTALL_DIR:-/usr/local/bin}"

os() {
  case "$(uname -s)" in
    Linux) echo "linux" ;;
    Darwin) echo "macos" ;;
    *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
  esac
}

arch() {
  case "$(uname -m)" in
    x86_64|amd64) echo "x86_64" ;;
    arm64|aarch64) echo "aarch64" ;;
    *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
  esac
}

main() {
  platform_os="$(os)"
  platform_arch="$(arch)"
  archive="morph-${platform_os}-${platform_arch}.tar.gz"
  url="https://github.com/${REPO}/releases/latest/download/${archive}"

  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "$tmp_dir"' EXIT

  echo "Downloading ${url} ..."
  if ! curl -fsSL "$url" -o "$tmp_dir/$archive"; then
    echo "Download failed. If this is a fresh clone with no published release yet," >&2
    echo "build from source instead: cargo build --release" >&2
    exit 1
  fi

  tar -xzf "$tmp_dir/$archive" -C "$tmp_dir"

  if [ ! -w "$INSTALL_DIR" ] 2>/dev/null; then
    INSTALL_DIR="$HOME/.local/bin"
    mkdir -p "$INSTALL_DIR"
  fi

  install -m 755 "$tmp_dir/morph" "$INSTALL_DIR/morph"
  echo "Installed morph to $INSTALL_DIR/morph"

  case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo "Note: $INSTALL_DIR is not on your PATH. Add it, e.g.:"
       echo "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
  esac

  echo
  echo "Next steps:"
  echo "  export OPENAI_API_KEY=sk-..."
  echo "  morph"
}

main "$@"
