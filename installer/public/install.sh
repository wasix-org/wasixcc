#!/usr/bin/env sh
set -e

# Settings
WASIXCC_SYSROOT_TAG="latest"
WASIXCC_LLVM_TAG="latest"
WASIXCC_BINARYEN_TAG="latest"
VERSION="0.4.2"

TARGET="" # detected in detect_target()

TEMP_DIR="" # set in download_wasixccenv()
WASIXCCENV="" # set in download_wasixccenv()
cleanup() {
    if test -n "$TEMP_DIR" && test -d "$TEMP_DIR"; then
        rm -r "$TEMP_DIR"
    fi
}
trap cleanup EXIT
trap cleanup INT

log() {
    echo -- "$1" >&2
}

fail() {
    echo "Error: $1" >&2
    exit 1
}

check_command() {
    command -v "$1" > /dev/null
}

# Assert all required dependencies are available
find_deps() {
    if check_command curl ; then
        CURL="curl"
    else
        if check_command wget ; then
            WGET="wget"
        else
            fail "Could not find curl or wget. Please install one of these tools."
        fi
    fi

    if check_command tar ; then
        TAR="tar"
    else
        fail "Could not find tar. Please install it."
    fi
}

# Set the TARGET variable to the current platform.
# If it is set to anything other than an empty string then we should have a prebuilt binary for that in our releases.
detect_target() {
    OS=$(uname -s | tr '[:upper:]' '[:lower:]')
    ARCH=$(uname -m)
    
    # Normalize OS
    case "$OS" in linux*) OS="linux";; darwin*) OS="apple";; mingw*|msys*|cygwin*|windows*) OS="windows";; *) fail "Unsupported OS: $OS" ;; esac
    
    # Normalize architecture
    case "$ARCH" in x86_64|amd64) ARCH="x86_64";; aarch64|arm64) ARCH="aarch64";; *) fail "Unsupported architecture: $ARCH" ;; esac
    
    # Construct target triple
    if [ "$OS" = "linux" ]; then
      if [ -f /lib/ld-musl-x86_64.so.1 ] || [ -f /lib/ld-musl-aarch64.so.1 ] || (ldd --version 2>&1 | grep -qi musl); then
        TARGET="${ARCH}-unknown-linux-musl"
      else
        TARGET="${ARCH}-unknown-linux-gnu"
      fi
    elif [ "$OS" = "apple" ]; then
      TARGET="${ARCH}-apple-darwin"
    fi

    case "$TARGET" in
        x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl|aarch64-unknown-linux-gnu|aarch64-unknown-linux-musl|x86_64-apple-darwin|aarch64-apple-darwin)
            ;;
        *)
            echo "Error: No binary release available for $OS on $ARCH" >&2
            return 1
            ;;
    esac
}

# Download wasixccenv from the GitHub releases, extract it, and set the WASIXCCENV variable to its path.
download_wasixccenv() {
    if test -z "$TARGET" ; then
        fail "Error: Could not detect target platform."
    fi

    log "Fetching wasixcc $VERSION"

    TEMP_DIR="$(mktemp -d)"
    cd "$TEMP_DIR"
    if test -n "$CURL" ; then
        if test -n "$GITHUB_TOKEN" ; then
            "$CURL" -H "authorization: Bearer $GITHUB_TOKEN" -L "https://github.com/wasix-org/wasixcc/releases/download/v$VERSION/wasixcc-$TARGET.tar.gz" --output - | "$TAR" -xz
        else
            "$CURL" -L "https://github.com/wasix-org/wasixcc/releases/download/v$VERSION/wasixcc-$TARGET.tar.gz" --output - | "$TAR" -xz
        fi
    else
        if test -n "$GITHUB_TOKEN" ; then
            "$WGET" --header "authorization: Bearer $GITHUB_TOKEN" -q -c "https://github.com/wasix-org/wasixcc/releases/download/v$VERSION/wasixcc-$TARGET.tar.gz" -O - | "$TAR" -xz
        else
            "$WGET" -q -c "https://github.com/wasix-org/wasixcc/releases/download/v$VERSION/wasixcc-$TARGET.tar.gz" -O - | "$TAR" -xz
        fi
    fi
    WASIXCCENV="$TEMP_DIR/wasixccenv"

    cd - > /dev/null 2>&1

    if test ! -f "$WASIXCCENV" ; then
        fail "Error: Failed to download wasixcc executable."
    fi
    if ! "$WASIXCCENV" --version >/dev/null ; then
        fail "Error: $WASIXCCENV is not working."
    fi

    log "Downloaded wasixcc"
}

# Actual script execution starts here

find_deps
detect_target
download_wasixccenv

"$WASIXCCENV" aio-install --sysroot-tag "${WASIXCC_SYSROOT_TAG:-latest}" --llvm-tag "${WASIXCC_LLVM_TAG:-latest}" --binaryen-tag "${WASIXCC_BINARYEN_TAG:-latest}"
