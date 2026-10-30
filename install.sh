#!/usr/bin/env bash
set -euo pipefail

BINARY="./bin/rustdrome"
INSTALL_DIR="$HOME/.local/bin"

if [[ ! -f "$BINARY" ]]; then
    echo "Error: Binary not found at $BINARY" >&2
    exit 1
fi

mkdir -p "$INSTALL_DIR"
install -m 755 "$BINARY" "$INSTALL_DIR/rustdrome"

echo "Installed/updated Rustdrome successfully ."

case ":$PATH:" in
    *":$INSTALL_DIR:"*)
        echo "$INSTALL_DIR is in PATH."
        ;;
    *)
        echo "Warning: $INSTALL_DIR is not in PATH."
        echo 'Add this to ~/.bashrc or ~/.zshrc:'
        echo 'export PATH="$HOME/.local/bin:$PATH"'
        ;;
esac

