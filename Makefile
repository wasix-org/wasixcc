CARGO        ?= cargo
WASIX_CONFIG := $(CURDIR)/wasix/registry.toml
WASIX_OUT    := target/wasm32-wasmer-wasi/release
LOCK_BACKUP  := .Cargo.lock.native.bak

.PHONY: all build test fmt wasix wasix-update wasix-package guard-no-cargo-config

all: build

build:
	$(CARGO) build --release --locked

test:
	$(CARGO) test --locked

fmt:
	$(CARGO) fmt --check

# The WASIX overlay registry must stay opt-in. If it is auto-discovered at
# .cargo/config.toml it applies to native builds too and rewrites Cargo.lock
# with `+wasix.N` versions that don't exist on crates.io, which breaks
# `cargo install --locked wasixcc` and `cargo vendor`. See issue #72.
guard-no-cargo-config:
	@if [ -e .cargo/config.toml ] || [ -e .cargo/config ]; then \
	  echo "error: .cargo/config.toml exists and would apply the WASIX overlay to"; \
	  echo "       every build. Delete it; this Makefile passes wasix/registry.toml"; \
	  echo "       explicitly to the WASIX build only."; \
	  exit 1; \
	fi

# Build the wasm32-wasmer-wasi module against the overlay registry, resolved
# from Cargo.wasix.lock. The crates.io Cargo.lock is put back afterwards, no
# matter how the build ends. CARGO_WASIX_NO_REGISTRY_CONFIG stops cargo-wasix
# from writing .cargo/config.toml behind our back.
#
# Each recipe below is a single shell invocation on purpose: `trap` only
# covers the shell it runs in, and .ONESHELL needs GNU Make 3.82+ (macOS
# ships 3.81).
#
# TODO: collapse the lockfile dance into `--lockfile-path Cargo.wasix.lock`
# once that flag is stable (unstable as of the pinned cargo 1.90).
wasix: guard-no-cargo-config
	@cp Cargo.lock $(LOCK_BACKUP); \
	 trap 'mv -f $(LOCK_BACKUP) Cargo.lock' EXIT INT TERM; \
	 cp Cargo.wasix.lock Cargo.lock; \
	 CARGO_WASIX_NO_REGISTRY_CONFIG=1 $(CARGO) wasix build --release --locked \
	   --no-default-features --config "$(WASIX_CONFIG)"

# Re-resolve the WASIX dependency graph and refresh Cargo.wasix.lock.
wasix-update: guard-no-cargo-config
	@cp Cargo.lock $(LOCK_BACKUP); \
	 trap 'mv -f $(LOCK_BACKUP) Cargo.lock' EXIT INT TERM; \
	 rm -f Cargo.lock; \
	 CARGO_WASIX_NO_REGISTRY_CONFIG=1 $(CARGO) wasix build --release \
	   --no-default-features --config "$(WASIX_CONFIG)" && \
	 cp Cargo.lock Cargo.wasix.lock

# wasmer.toml's [[module]] source is $(WASIX_OUT)/wasixcc.wasm, but the only
# [[bin]] is wasixccenv, so cargo emits wasixccenv.wasm. Copy rather than
# rename the bin: command dispatch (src/main.rs, get_command_name) keys off
# the webc command name, and a second [[bin]] would double the native build.
wasix-package: wasix
	cp $(WASIX_OUT)/wasixccenv.wasm $(WASIX_OUT)/wasixcc.wasm
	@echo "Module ready at $(WASIX_OUT)/wasixcc.wasm; now run: wasmer publish"
