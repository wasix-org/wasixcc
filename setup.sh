#!/usr/bin/env sh

set -xe

MISSING_DEPS=""

fail() {
    echo "Error: $1" >&2
    exit 1
}

check_command() {
    if command -v "$1" > /dev/null ; then
        return 0
    else
        return 1
    fi
}

if test -z "$HOME" ; then
    fail "HOME environment variable is not set."
fi

add_to_missing_deps() {
    pkg="$1"
    if test -n "$pkg" ; then
        if test -n "$MISSING_DEPS" ; then
            MISSING_DEPS="$MISSING_DEPS $pkg"
        else
            MISSING_DEPS="$pkg"
        fi
    fi
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
        add_to_missing_deps "tar"
    fi

    if check_command uname ; then
        UNAME="uname"
    else
        add_to_missing_deps "uname"
    fi

    assert_commands
}

assert_commands() {
    if test -n "$MISSING_DEPS" ; then
        echo "Error: Some dependencies are missing \"$MISSING_DEPS\". Please install them and try again." >&2
        echo "You can install them with your package manager, e.g.:" >&2
        echo "  ${SUDO:-sudo} apt install $MISSING_DEPS" >&2
        if command -v apt >/dev/null 2>&1 ; then
            printf "Do you want to execute that command now? [y/N]:" >/dev/tty
            if read -r REPLY </dev/tty ; then
                case "$REPLY" in
                    [Yy])
                        ${SUDO:-sudo} apt install "$MISSING_DEPS" || exit 1
                        return 0
                        ;;
                esac
            fi
        fi
        exit 1
    fi
}

# Set the TARGET variable to the current platform.
# If it is set to anything other than an empty string then we should have a prebuilt binary for that in our releases.
detect_target() {
    OS=$("$UNAME" -s | tr '[:upper:]' '[:lower:]')
    ARCH=$("$UNAME" -m)
    
    # Normalize OS
    case "$OS" in linux*) OS="linux";; darwin*) OS="apple";; mingw*|msys*|cygwin*|windows*) OS="windows";; *) echo "Unsupported OS: $OS"; return 1;; esac
    
    # Normalize architecture
    case "$ARCH" in x86_64|amd64) ARCH="x86_64";; aarch64|arm64) ARCH="aarch64";; *) echo "Unsupported architecture: $ARCH"; return 1;; esac
    
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

find_deps

detect_target

WASIXCC_DIR="$HOME/.wasixcc"
WASIXCC_BIN="$WASIXCC_DIR/bin"
WASIXCC_EXECUTABLE="$WASIXCC_DIR/wasixcc"

# If set to latest => latest, if empty => do not install, else use as is
WASIXCC_SYSROOT_TAG="latest"
WASIXCC_LLVM_TAG="latest"
WASIXCC_BINARYEN_TAG="latest"

VERSION="0.2.4"
# TARGET= detected above

# Maybe add a confirmation prompt here

set -x

mkdir -p "$WASIXCC_DIR"

if test -z "$TARGET" ; then
    echo "Error: Could not detect target platform." >&2
    exit 1
fi

# Fetch wasixcc
fetch_wasixcc() {
    echo "Fetching the latest wasixcc executable" >&2
    mkdir -p "$WASIXCC_DIR"
    cd "$WASIXCC_DIR"
    if test -n "$CURL" ; then
        "$CURL" -L "https://github.com/wasix-org/wasixcc/releases/download/v$VERSION/wasixcc-$TARGET.tar.gz" --output - | "$TAR" -xz
    else
        "$WGET" -q -c "https://github.com/wasix-org/wasixcc/releases/download/v$VERSION/wasixcc-$TARGET.tar.gz" -O - | "$TAR" -xz
    fi
    cd -
}

# Fetch wasixcc executable
fetch_wasixcc

# Install the executables
mkdir -p "$WASIXCC_BIN"
"$WASIXCC_EXECUTABLE" --install-executables "$WASIXCC_BIN"

# Download dependencies
download_dependencies() {
    case "$WASIXCC_SYSROOT_TAG" in
        latest) "$WASIXCC_EXECUTABLE" --download-sysroot ;;
        "") : ;;
        *) "$WASIXCC_EXECUTABLE" --download-sysroot "$WASIXCC_SYSROOT_TAG" ;;
    esac

    case "$WASIXCC_LLVM_TAG" in
        latest) "$WASIXCC_EXECUTABLE" --download-llvm ;;
        "") : ;;
        *) "$WASIXCC_EXECUTABLE" --download-llvm "$WASIXCC_LLVM_TAG" ;;
    esac

    case "$WASIXCC_BINARYEN_TAG" in
        latest) "$WASIXCC_EXECUTABLE" --download-binaryen ;;
        "") : ;;
        *) "$WASIXCC_EXECUTABLE" --download-binaryen "$WASIXCC_BINARYEN_TAG" ;;
    esac
}
download_dependencies

# Add to the path of relevant shells config files

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