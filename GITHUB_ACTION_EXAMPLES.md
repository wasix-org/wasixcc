# GitHub Action Examples

This file contains example workflows for using the wasixcc GitHub Action.

## Example 1: Basic C compilation

```yaml
name: Build with wasixcc

on: [push, pull_request]

permissions:
  contents: read

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout code
        uses: actions/checkout@v4
      
      - name: Setup wasixcc
        uses: wasix-org/wasixcc@v0.2.4
      
      - name: Compile C program
        run: |
          wasixcc src/main.c -o build/main.wasm
      
      - name: Upload artifact
        uses: actions/upload-artifact@v4
        with:
          name: wasm-binary
          path: build/main.wasm
```

## Example 2: Multi-platform compilation

```yaml
name: Build on multiple platforms

on: [push, pull_request]

permissions:
  contents: read

jobs:
  build:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    
    steps:
      - uses: actions/checkout@v4
      
      - name: Setup wasixcc
        uses: wasix-org/wasixcc@v0.2.4
        with:
          version: latest
          install-sysroot: true
          install-llvm: true
          install-binaryen: true
      
      - name: Build
        run: wasixcc main.c -o main.wasm
```

## Example 3: C++ compilation with custom versions

```yaml
name: Build C++ with specific toolchain versions

on: [push, pull_request]

permissions:
  contents: read

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      
      - name: Setup wasixcc with specific versions
        uses: wasix-org/wasixcc@v0.2.4
        with:
          version: v0.2.4
          sysroot-tag: latest
          llvm-tag: latest
          binaryen-tag: version_118
      
      - name: Compile C++ program
        run: |
          wasix++ src/app.cpp -o build/app.wasm
```

## Example 4: Minimal installation (no automatic toolchain)

If you already have the toolchain installed or want to manage it separately:

```yaml
name: Build with existing toolchain

on: [push, pull_request]

permissions:
  contents: read

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      
      - name: Setup wasixcc (minimal)
        uses: wasix-org/wasixcc@v0.2.4
        with:
          install-sysroot: false
          install-llvm: false
          install-binaryen: false
      
      - name: Install custom toolchain
        run: |
          # Your custom toolchain installation steps
          wasixcc --download-sysroot v2024-12-01.1
          wasixcc --download-llvm v2024-12-01.1
      
      - name: Build
        run: wasixcc main.c -o main.wasm
```

## Example 5: Build matrix with different configurations

```yaml
name: Build with different WASM configurations

on: [push, pull_request]

permissions:
  contents: read

jobs:
  build:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        config:
          - name: default
            flags: ""
          - name: eh-enabled
            flags: "-sWASM_EXCEPTIONS=yes"
          - name: eh-pic
            flags: "-sWASM_EXCEPTIONS=yes -sPIC=yes"
    
    steps:
      - uses: actions/checkout@v4
      
      - name: Setup wasixcc
        uses: wasix-org/wasixcc@v0.2.4
      
      - name: Build ${{ matrix.config.name }}
        run: |
          wasixcc ${{ matrix.config.flags }} main.c -o main-${{ matrix.config.name }}.wasm
      
      - name: Upload ${{ matrix.config.name }} artifact
        uses: actions/upload-artifact@v4
        with:
          name: wasm-${{ matrix.config.name }}
          path: main-${{ matrix.config.name }}.wasm
```

## Example 6: Using action outputs

```yaml
name: Use action outputs

on: [push, pull_request]

permissions:
  contents: read

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      
      - name: Setup wasixcc
        id: wasixcc-setup
        uses: wasix-org/wasixcc@v0.2.4
      
      - name: Display installation paths
        run: |
          echo "wasixcc path: ${{ steps.wasixcc-setup.outputs.wasixcc-path }}"
          echo "sysroot path: ${{ steps.wasixcc-setup.outputs.sysroot-path }}"
      
      - name: Build
        run: ${{ steps.wasixcc-setup.outputs.wasixcc-path }} main.c -o main.wasm
```
