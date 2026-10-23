#!/usr/bin/env bash
set -euo pipefail

BINARY="${1:-./rustdrome}"
INSTALL_DIR="$HOME/.local/bin"
BINARY_NAME="rustdrome"

if [[ ! -f "$BINARY" ]]; then
    echo "Error: Binary not found: $BINARY" >&2
    exit 1
fi

mkdir -p "$INSTALL_DIR"
install -m 755 "$BINARY" "$INSTALL_DIR/$BINARY_NAME"

echo "Installed rustdrome to $INSTALL_DIR/$BINARY_NAME"

case ":$PATH:" in
    *":$INSTALL_DIR:"*)
        echo "$INSTALL_DIR is already in PATH."
        ;;
    *)
        echo "Warning: $INSTALL_DIR is not in PATH."
        echo 'Add this line to ~/.bashrc or ~/.zshrc:'
        echo 'export PATH="$HOME/.local/bin:$PATH"'
        ;;
esac

