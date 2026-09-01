#!/usr/bin/env sh
set -e

WASIXCC_DIR="$HOME/.wasixcc"
WASIXCC_BIN="$WASIXCC_DIR/bin"
WASIXCC_EXECUTABLE="$WASIXCC_DIR/wasixccenv"

# If set to latest => latest, if empty => do not install, else use as is
WASIXCC_SYSROOT_TAG="latest"
WASIXCC_LLVM_TAG="latest"
WASIXCC_BINARYEN_TAG="latest"

VERSION="0.4.6"
TARGET= # detected in detect_target()

log() {
    echo -- "$1" >&2
}

fail() {
    echo "Error: $1" >&2
    exit 1
}

append_block_to_env_file() {
  file="$1"
  body="$2"

  # Only touch existing files
  [ -f "$file" ] || return 0

  # Already installed?
  grep -Fqs "# >>> wasixcc initialize >>>" "$file" && return 0

  {
    echo ""
    echo "# >>> wasixcc initialize >>>"
    echo "$body"
    echo "# <<< wasixcc initialize <<<"
  } >>"$file"
}

check_command() {
    command -v "$1" > /dev/null
}

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

download_wasixcc() {
    if test -z "$TARGET" ; then
        fail "Error: Could not detect target platform."
    fi

    log "Fetching the latest wasixcc executable"

    URL="https://github.com/wasix-org/wasixcc/releases/download/v$VERSION/wasixcc-$TARGET.tar.gz"
    ARCHIVE="wasixcc-$TARGET.tar.gz"

    mkdir -p "$WASIXCC_DIR"
    cd "$WASIXCC_DIR"

    # Download to a file rather than piping into tar. On an HTTP error the body
    # ("Not Found") would otherwise be fed to tar, which reports the misleading
    # "Unrecognized archive format" and buries the real cause (WAX-605).
    rm -f "$ARCHIVE"
    if test -n "$CURL" ; then
        if test -n "$GITHUB_TOKEN" ; then
            "$CURL" -fL -H "authorization: Bearer $GITHUB_TOKEN" "$URL" --output "$ARCHIVE" \
                || fail "Failed to download $URL"
        else
            "$CURL" -fL "$URL" --output "$ARCHIVE" \
                || fail "Failed to download $URL"
        fi
    else
        if test -n "$GITHUB_TOKEN" ; then
            "$WGET" --header "authorization: Bearer $GITHUB_TOKEN" -q "$URL" -O "$ARCHIVE" \
                || fail "Failed to download $URL"
        else
            "$WGET" -q "$URL" -O "$ARCHIVE" \
                || fail "Failed to download $URL"
        fi
    fi

    "$TAR" -xzf "$ARCHIVE" || fail "Failed to extract $ARCHIVE"
    rm -f "$ARCHIVE"

    cd - > /dev/null 2>&1

    if test ! -f "$WASIXCC_EXECUTABLE" ; then
        fail "Error: Failed to download wasixcc executable."
    fi
    if ! "$WASIXCC_EXECUTABLE" --version >/dev/null ; then
        fail "Error: $WASIXCC_EXECUTABLE is not working."
    fi

    log "Downloaded wasixcc"
}

# Install symlinks at ~/.wasixcc/bin
install_executables() {
    mkdir -p "$WASIXCC_BIN"
    "$WASIXCC_EXECUTABLE" install-executables "$WASIXCC_BIN"
}

# Download sysroot, LLVM, and Binaryen
download_dependencies() {
    case "$WASIXCC_SYSROOT_TAG" in
        latest) "$WASIXCC_EXECUTABLE" download-sysroot ;;
        "") : ;;
        *) "$WASIXCC_EXECUTABLE" download-sysroot "$WASIXCC_SYSROOT_TAG" ;;
    esac

    case "$WASIXCC_LLVM_TAG" in
        latest) "$WASIXCC_EXECUTABLE" download-llvm ;;
        "") : ;;
        *) "$WASIXCC_EXECUTABLE" download-llvm "$WASIXCC_LLVM_TAG" ;;
    esac

    case "$WASIXCC_BINARYEN_TAG" in
        latest) "$WASIXCC_EXECUTABLE" download-binaryen ;;
        "") : ;;
        *) "$WASIXCC_EXECUTABLE" download-binaryen "$WASIXCC_BINARYEN_TAG" ;;
    esac
}

# Create env files for various shells
create_env_files() {
    cat <<'EOF' > "$WASIXCC_DIR/env"
echo "$PATH" | grep "$HOME/.wasixcc/bin" 2>/dev/null >/dev/null || export PATH="$HOME/.wasixcc/bin:$PATH" 2>/dev/null >/dev/null
true
EOF
    chmod +x "$WASIXCC_DIR/env"

    cat <<'EOF' > "$WASIXCC_DIR/env.nu"
let bin = $"($env.HOME)/.wasixcc/bin"
if not ($env.PATH | any {|p| $p == $bin }) {
  $env.PATH = ([$bin] ++ $env.PATH)
}
EOF
    chmod +x "$WASIXCC_DIR/env.nu"

    cat <<'EOF' > "$WASIXCC_DIR/env.xsh"
import os
home = os.path.expanduser("~")
bin = home + "/.wasixcc/bin"
if bin not in $PATH:
    $PATH.insert(0, bin)
EOF
    chmod +x "$WASIXCC_DIR/env.xsh"
}

add_env_files_to_rcs() {
    # Add env files to various shell rc files
    # shellcheck disable=SC2016
    POSIX_BODY='test -f "$HOME/.wasixcc/env" && . "$HOME/.wasixcc/env"'
    # shellcheck disable=SC2016
    FISH_BODY='test -f "$HOME/.wasixcc/env" && source "$HOME/.wasixcc/env"'
    XONSH_BODY="import os
    p = os.path.expanduser('~/.wasixcc/env.xsh')
    if os.path.isfile(p):
        execx(open(p).read(), 'exec', locals(), globals())"
    NU_BODY="let p = (\$env.HOME | path join '.wasixcc' 'env.nu')
    if (\$p | path exists) { source \$p }"

    append_block_to_env_file "$HOME/.bashrc" "$POSIX_BODY"
    append_block_to_env_file "$HOME/.zshrc" "$POSIX_BODY"
    append_block_to_env_file "$HOME/.kshrc" "$POSIX_BODY"
    append_block_to_env_file "$HOME/.mkshrc" "$POSIX_BODY"
    append_block_to_env_file "$HOME/.profile" "$POSIX_BODY"
    append_block_to_env_file "$HOME/.config/fish/config.fish" "$FISH_BODY"
    append_block_to_env_file "$HOME/.xonshrc" "$XONSH_BODY"
    append_block_to_env_file "$HOME/.config/nushell/config.nu" "$NU_BODY"
}

# Actual script execution starts here

if test -z "$HOME" ; then
    fail "HOME environment variable is not set."
fi

find_deps
detect_target

download_wasixcc
install_executables
download_dependencies

create_env_files
add_env_files_to_rcs

# shellcheck disable=SC2016
echo 'wasixcc installed successfully!

To start using wasixcc, please restart your terminal or
run the appropriate command to source the environment file for your shell:

. ~/.wasixcc/env
source ~/.wasixcc/env  # For fish
source $"($nu.home-path)/.wasixcc/env.nu"  # For nushell"
'
