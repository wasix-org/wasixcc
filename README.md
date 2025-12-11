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

### GitHub Actions

The easiest way to use `wasixcc` in your CI/CD pipeline is via the GitHub Action:

```yaml
- name: Install wasixcc
  uses: wasix-org/wasixcc@main
```

### Local Installation

1. Install a recent version of [binaryen](https://github.com/WebAssembly/binaryen)
2. Install `wasixcc`:
   ```bash
   cargo install wasixcc -F bin
   ```
   - Alternatively, if you have [cargo-binstall](https://github.com/cargo-bins/cargo-binstall), you can install pre-built binaries:
     ```bash
     cargo binstall wasixcc
     ```
   - Or clone and build wasixcc from this repo:
     ```bash
     git clone https://github.com/wasix-org/wasixcc
     cd wasixcc
     cargo build -r -F bin --bin wasixcc
     ```
3. Install all executables (`wasix++`, `wasixar`, etc.) to your PATH:
   ```bash
   sudo wasixcc --install-executables /usr/local/bin
   ```
4. Optionally, download the latest LLVM toolchain and WASIX sysroot if you don't have them already:
   ```bash
   wasixcc --download-all
   ```

## Usage

Basic usage:

```bash
wasixcc [OPTIONS] -- [PASS-THROUGH OPTIONS]
```

Run `wasixcc --help` for comprehensive usage instructions.

### Common Options

| Option                         | Description                                                        |
| ------------------------------ | ------------------------------------------------------------------ |
| `-h`, `--help`                 | Print help message                                                 |
| `-v`, `--version`              | Print version information                                          |
| `--install-executables <PATH>` | Install executables to specified path                              |
| `--download-sysroot <TAG>`     | Download and install WASIX libc sysroot ('latest' or specific tag) |
| `--download-llvm <TAG>`        | Download and install LLVM toolchain ('latest' or specific tag)     |
| `--download-all`               | Download and install the latest sysroot and LLVM toolchain         |
| `--print-sysroot`              | Print current sysroot location                                     |
| `-s[CONFIG]=[VALUE]`           | Set configuration values (see below)                               |

### Configuration Options

Configuration can be set via command line (`-s` flag) or environment variables (`WASIXCC_` prefix):

| Option                      | Description                                                          |
| --------------------------- | -------------------------------------------------------------------- |
| `SYSROOT`                   | Set the sysroot location                                             |
| `SYSROOT_PREFIX`            | Set the sysroot prefix directory                                     |
| `LLVM_LOCATION`             | Set location of LLVM binaries                                        |
| `COMPILER_FLAGS`            | Extra compiler flags (colon-separated)                               |
| `COMPILER_POST_FLAGS`       | Extra compiler flags (after command line args)                       |
| `COMPILER_FLAGS_C`          | C-specific compiler flags                                            |
| `COMPILER_POST_FLAGS_C`     | C-specific post compiler flags                                       |
| `COMPILER_FLAGS_CXX`        | C++-specific compiler flags                                          |
| `COMPILER_POST_FLAGS_CXX`   | C++-specific post compiler flags                                     |
| `LINKER_FLAGS`              | Extra linker flags                                                   |
| `RUN_WASM_OPT`              | Whether to run wasm-opt                                              |
| `WASM_OPT_FLAGS`            | Extra wasm-opt flags                                                 |
| `WASM_OPT_SUPPRESS_DEFAULT` | Suppress default wasm-opt flags                                      |
| `MODULE_KIND`               | Module type (static-main, dynamic-main, shared-library, object-file) |
| `WASM_EXCEPTIONS`           | Enable WASM exception handling                                       |
| `PIC`                       | Enable position-independent code                                     |
| `LINK_SYMBOLIC`             | Enable -Bsymbolic linking (enabled by default)                       |

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

4. Compile with custom optimization flags:
   ```bash
   wasixcc -sCOMPILER_FLAGS="-O3" -sWASM_OPT_FLAGS="-O3" app.c -o app.wasm
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
  RANLIB=wasixranlib

# Disable wasm-opt during configuration...
WASIXCC_RUN_WASM_OPT=no \
  ./configure ...

# ... but make sure to enable it again during the build, as skipping
# wasm-opt produces broken binaries in all configurations.
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
export WASIX_SYSROOT=$(wasixcc --print-sysroot)

# Disable wasm-opt during configuration...
WASIXCC_RUN_WASM_OPT=no \
  cmake ... -DCMAKE_TOOLCHAIN_FILE=wasix-toolchain.cmake

# ... but make sure to enable it again during the build
cmake --build ...
```

## Contributing

Contributions are welcome! Please feel free to open a PR if there's something you feel can be improved.
