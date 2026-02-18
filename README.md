# wasixcc - The C/C++ compiler for the WASIX platform

`wasixcc` is a clang wrapper designed to simplify compilation for the WASIX platform.
It provides a convenient interface to configure and invoke the LLVM toolchain with appropriate
flags for the WASIX platform.

## Features

- Easy configuration of WASIX compilation parameters
- Automatic sysroot management
- Support for both C and C++ compilation
- Flexible flag management for compiler and linker
- Integration with wasm-opt for optimization
- Support for various module types
  - Static, dynamic, shared libraries
  - Exception handling, asyncify

## Installation

```sh
curl -fsSL https://wasix.cc | sh
```

The installer will install wasixcc and its dependencies to `~/.wasixcc`

<details>
  <summary>Other installation options</summary>
  
  _If you don't use the installer (or the github action)_

- From [crates.io](https://crates.io/crates/wasixcc) (macOS, Linux)

  ```sh
  cargo install wasixcc
  # Install executables
  sudo wasixccenv install-executables /usr/local/bin
  # Download the latest LLVM toolchain, WASIX sysroot, and wasm-opt
  wasixccenv download-all
  ```

- [Cargo binstall](https://crates.io/crates/cargo-binstall)</a>

  ```sh
  cargo binstall wasixcc
  # Install executables
  sudo wasixccenv install-executables /usr/local/bin
  # Download the latest LLVM toolchain, WASIX sysroot, and wasm-opt
  wasixccenv download-all
  ```

- Directly from the [git repo](https://github.com/wasix-org/wasixcc)

  ```sh
  git clone https://github.com/wasix-org/wasixcc
  cd wasixcc
  cargo build -r
  # Install executables
  sudo ./target/release/wasixccenv install-executables /usr/local/bin
  # Download the latest LLVM toolchain, WASIX sysroot, and wasm-opt
  wasixccenv download-all
  ```

</details>

### GitHub Actions

To use `wasixcc` in your GitHub Action use the following snippet

```yaml
- name: Install wasixcc
  uses: wasix-org/wasixcc@main
```

This will setup wasixcc and all dependencies in less than 10 seconds.

## Usage

Environment management commands are available from the `wasixccenv` binary.
Run `wasixccenv --help` for comprehensive usage instructions.

To use the compiler toolchain, first run `wasixccenv install-executables` and
then run the correct tool (`wasixcc`, `wasixar`, etc.).

### Common `wasixccenv` Options

| Option                       | Description                                                        |
| ---------------------------- | ------------------------------------------------------------------ |
| `-h`, `--help`               | Print help message                                                 |
| `-v`, `--version`            | Print version information                                          |
| `install-executables <PATH>` | Install executables to specified path                              |
| `download-sysroot <TAG>`     | Download and install WASIX libc sysroot ('latest' or specific tag) |
| `download-llvm <TAG>`        | Download and install LLVM toolchain ('latest' or specific tag)     |
| `download-all`               | Download and install the latest sysroot and LLVM toolchain         |
| `print-sysroot`              | Print current sysroot location                                     |
| `-s[CONFIG]=[VALUE]`         | Set configuration values (see below)                               |

### `wasixcc` Configuration Options

Configuration can be set via command line (`-s` flag) or environment variables (`WASIXCC_` prefix).
You can run `wasixccenv help-config` to see detailed explanation of each flag and what it does. A
summary is provided here for reference:

| Option                          | Description                                                                                    |
| ------------------------------- | ---------------------------------------------------------------------------------------------- |
| `SYSROOT`                       | Set the sysroot location - not recommended, use SYSROOT_PREFIX instead where possible          |
| `SYSROOT_PREFIX`                | Set the sysroot prefix directory                                                               |
| `LLVM_LOCATION`                 | Set location of LLVM binaries                                                                  |
| `BINARYEN_LOCATION`             | Set location of wasm-opt                                                                       |
| `COMPILER_FLAGS`                | Extra compiler flags (colon-separated)                                                         |
| `COMPILER_POST_FLAGS`           | Extra compiler flags (after command line args)                                                 |
| `COMPILER_FLAGS_C`              | C-specific compiler flags                                                                      |
| `COMPILER_POST_FLAGS_C`         | C-specific post compiler flags                                                                 |
| `COMPILER_FLAGS_CXX`            | C++-specific compiler flags                                                                    |
| `COMPILER_POST_FLAGS_CXX`       | C++-specific post compiler flags                                                               |
| `LINKER_FLAGS`                  | Extra linker flags                                                                             |
| `INCLUDE_CPP_SYMBOLS`           | Whether to also link libc++, libc++abi and libunwind into C binaries                           |
| `RUN_WASM_OPT`                  | Whether to run wasm-opt                                                                        |
| `WASM_OPT_FLAGS`                | Extra wasm-opt flags                                                                           |
| `WASM_OPT_SUPPRESS_DEFAULT`     | Suppress default wasm-opt flags                                                                |
| `WASM_OPT_PRESERVE_UNOPTIMIZED` | Whether to preserve the unoptimized binary in a temp directory                                 |
| `MODULE_KIND`                   | Module type (static-main, dynamic-main, shared-library, object-file)                           |
| `WASM_EXCEPTIONS`               | Enable WASM exception handling.                                                                |
| `EXCEPTION_STYLE`               | The exception handling style to generate for WASM exception handling. (exnref, legacy)         |
| `PIC`                           | Enable position-independent code                                                               |
| `LINK_SYMBOLIC`                 | Enable -Bsymbolic linking (enabled by default)                                                 |
| `GENERATE_SHELL_SCRIPT`         | Generate a shell script for running the resulting WASM binary as if it was a native executable |
| `SHELL_SCRIPT_WASMER_ARGS`      | Specify wasmer args for running the WASM binary in the shell script                            |
| `DISCARD_UNSUPPORTED_FLAGS`     | Some flags that you would use with native tools don't work when targeting WASIX. Discard them. |
| `AUTOCONF_WORKAROUNDS`          | Attempt to detect autoconf tests and make them work as intended.                               |

### Environment Variables

All configuration options can be set via environment variables by prefixing them with `WASIXCC_`:

```bash
export WASIXCC_SYSROOT=/custom/sysroot
export WASIXCC_COMPILER_FLAGS="-O2"
wasixcc program.c -o program.wasm
```

This is useful when `wasixcc` is integrated into build systems where you don't control the CLI invocation
directly, such as when running through CMake.

## Examples

1. Compile a simple C program:

   ```bash
   wasixcc hello.c -o hello.wasm
   ```

2. Compile a simple C++ program:

   ```bash
   wasix++ hello.cpp -o program.wasm
   ```

3. Compile with custom sysroot:

   ```bash
   wasixcc -sSYSROOT=/path/to/sysroot program.c -o program.wasm
   ```

4. Compile with custom flags:
   ```bash
   export WASIXCC_COMPILER_FLAGS="-O3"
   # ... a lot of other code ...
   wasixcc app.c -o app.wasm
   ```

## Build configurations

`wasixcc` supports 3 primary build configurations. The configurations are mainly
differentiated based on where they can run and what language features they support,
and how `setjmp`/`longjmp` is handled.

- The default configuration; this configuration can run anywhere, but [relies on
  `asyncify`](https://github.com/WebAssembly/binaryen/blob/main/src/passes/Asyncify.cpp)
  for `setjmp`/`longjmp` support. `asyncify` has considerable performance
  implications, and should be avoided where possible.
  Support for C++ exceptions in this configuration has not been tested, and
  it is likely to be broken.

- The EH configuration uses the [WASM Exception Handling Proposal](https://github.com/WebAssembly/exception-handling/blob/main/proposals/exception-handling/Exceptions.md)
  to support `setjmp`/`longjmp`. This configuration can only run on EH-enabled
  WASM runtimes, including Wasmer's LLVM backend and browsers. However, it is
  considerably faster than the default configuration due to avoiding `asyncify`.
  C++ exceptions are also fully supported in this mode.

  To enable this mode, run `wasixcc` with `-sWASM_EXCEPTIIONS=yes`.

- The EH+PIC configuration uses the EH proposal similarly to the EH configuration,
  but also enables [Position-Independent Code](https://en.wikipedia.org/wiki/Position-independent_code).
  In the WASM world, PIC is only useful for dynamic linking scenarios, so you should
  avoid this configuration unless you require support for `dlopen`/`dlsym`.

  To enable this mode, run `wasixcc` with `-sWASM_EXCEPTIONS=yes -sPIC=yes`.

### Dynamic linking

If you need support for dynamic linking, you need to use the EH+PIC configuration
for the main module and all side modules. The usual clang flags work here; just
passing `-shared` will give you a DL side module, a.k.a. a
dynamically-linked library.

However, there is one caveat: native binaries generally link against libc
dynamically at runtime, with libc being provided by the OS. Since there is
no concept of an OS in wasm, the approach is slightly different; the main
module is expected to embed _all_ `libc`/`libc++` symbols and make them available
to side modules.

To enable this behavior in `wasixcc`, you may need to explicitly set the module
kind to dynamic-main by passing `-sMODULE_KIND=dynamic-main`.

## Integration with build systems

`wasixcc` can be integrated into different build systems to adapt existing
software to the WASIX platform.

### GNU Autotools

To use `wasixcc` with Autotools, simply replace the default LLVM tools with the
`wasixcc` equivalent.

`wasixcc` runs `wasm-opt` to generate working output modules by default, but this
can break compilation tests, so it is recommended to disable `wasm-opt` during
configuration:

```bash
# Set up wasixcc's settings
export WASIXCC_XXX=YYY

# Replace default tools with wasixcc equivalents
export \
  CC=wasixcc \
  CXX=wasix++ \
  LD=wasixld \
  AR=wasixar \
  NM=wasixnm \
  RANLIB=wasixranlib \

# The way configure detects functions is not compatible with wasm.
# Enable detection and automatic workarounds for that.
WASIXCC_AUTOCONF_WORKAROUNDS=yes ./configure ...

make ...
```

### CMake

To use `wasixcc` with CMake, you can use the
[toolchain file in this repository](./wasix-toolchain.cmake):

```bash
# First, set up wasixcc settings for the build. This is important
# because the build settings influence the sysroot location.
export WASIXCC_XXX=YYY

# wasix-toolchain.cmake references this variable
export WASIX_SYSROOT=$(wasixccenv print-sysroot)

# Disable wasm-opt during configuration...
WASIXCC_RUN_WASM_OPT=no \
  cmake ... -DCMAKE_TOOLCHAIN_FILE=wasix-toolchain.cmake

# ... but make sure to enable it again during the build
cmake --build ...
```

## Contributing

Contributions are welcome! Please feel free to open a PR if there's something you feel can be improved.
