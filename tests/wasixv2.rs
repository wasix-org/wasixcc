//! End-to-end test for `TARGET=wasm32-wasixv2`: compiles and links a C program
//! with the real toolchain and checks the module's imports.
//!
//! It needs the v2 LLVM fork and the libc2 sysroot, found the same way wasixcc
//! finds them (`WASIXCC_LLVM_LOCATION` or `~/.wasixcc/llvm-wasixv2`,
//! `WASIXCC_SYSROOT` or `~/.wasixcc/sysroot/sysroot-wasixv2`). Without them the
//! test is skipped, unless `WASIXCC_TEST_WASIXV2_REQUIRED=1`. With
//! `WASIXCC_TEST_WASIXV2_HOST=<libc2>/wasix2/test/host.mjs` the programs are
//! also run under node.

// Spawns host processes, which a WASIX test module can't do.
#![cfg(all(unix, not(target_family = "wasm")))]

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

const PROGRAM: &str = r#"
#include <setjmp.h>
#include <stdio.h>

static jmp_buf env;

static void jump(int value) { longjmp(env, value); }

int main(int argc, char **argv) {
    (void)argv;
    int value = setjmp(env);
    if (value < 3)
        jump(value + 1);
    /* binary128 long double arithmetic and printf need compiler-rt builtins */
    long double third = (long double)argc / 3;
    printf("hello from wasixv2: setjmp %d, %.3Lf\n", value, third);
    return 0;
}
"#;

const EXPECTED_OUTPUT: &str = "hello from wasixv2: setjmp 3, 0.333\n";

fn home() -> PathBuf {
    PathBuf::from(env::var_os("HOME").expect("HOME is not set"))
}

fn llvm_location() -> PathBuf {
    env::var_os("WASIXCC_LLVM_LOCATION")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".wasixcc/llvm-wasixv2"))
}

fn sysroot_location() -> PathBuf {
    if let Some(sysroot) = env::var_os("WASIXCC_SYSROOT") {
        return sysroot.into();
    }
    env::var_os("WASIXCC_SYSROOT_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".wasixcc/sysroot"))
        .join("sysroot-wasixv2")
}

fn toolchain_available() -> bool {
    let missing: Vec<PathBuf> = [
        llvm_location().join("bin/clang"),
        sysroot_location().join("lib/wasm32-wasixv2/libc.a"),
    ]
    .into_iter()
    .filter(|path| !path.exists())
    .collect();

    if missing.is_empty() {
        return true;
    }
    if env::var("WASIXCC_TEST_WASIXV2_REQUIRED").is_ok_and(|v| v == "1") {
        panic!("wasm32-wasixv2 toolchain not found: {missing:?}");
    }
    eprintln!("skipping: wasm32-wasixv2 toolchain not found: {missing:?}");
    false
}

// wasixcc and wasix++ are the wasixccenv binary run under another name.
fn tool(dir: &Path, name: &str) -> Command {
    let link = dir.join(name);
    if !link.exists() {
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_wasixccenv"), &link).unwrap();
    }
    let mut command = Command::new(link);
    command.current_dir(dir);
    command
}

fn wasixcc(dir: &Path) -> Command {
    tool(dir, "wasixcc")
}

fn run(mut command: Command) {
    let status = command.status().unwrap();
    assert!(status.success(), "{command:?} failed with {status}");
}

#[test]
fn compile_and_link_c_for_wasixv2() {
    if !toolchain_available() {
        return;
    }

    let dir = tempfile::TempDir::new().unwrap();
    let dir = dir.path();
    std::fs::write(dir.join("hello.c"), PROGRAM).unwrap();

    // One step, selected by the TARGET setting.
    let mut command = wasixcc(dir);
    command.args(["-sTARGET=wasm32-wasixv2", "-O1", "hello.c", "-o", "a.wasm"]);
    run(command);

    // Separate compile and link, selected by --target as CMake passes it, and
    // with -fno-exceptions, which must keep setjmp working.
    let mut command = wasixcc(dir);
    command.args([
        "--target=wasm32-wasixv2",
        "-fno-exceptions",
        "-c",
        "hello.c",
        "-o",
        "hello.o",
    ]);
    run(command);
    let mut command = wasixcc(dir);
    command.args(["--target=wasm32-wasixv2", "hello.o", "-o", "b.wasm"]);
    run(command);

    for wasm in ["a.wasm", "b.wasm"] {
        let bytes = std::fs::read(dir.join(wasm)).unwrap();
        let imports = imports(&bytes);
        assert!(
            imports.contains(&("env".to_string(), "memory".to_string())),
            "{wasm} does not import env.memory: {imports:?}"
        );
        for (module, name) in &imports {
            assert!(
                (module == "env" && name == "memory") || module == "wasixv2",
                "{wasm} has an unexpected import {module}.{name}"
            );
        }

        if let Some(host) = env::var_os("WASIXCC_TEST_WASIXV2_HOST") {
            assert_eq!(
                run_in_node(Path::new(&host), &dir.join(wasm)),
                EXPECTED_OUTPUT
            );
        }
    }
}

#[test]
fn cxx_link_without_libcxx_fails_clearly() {
    if !toolchain_available() {
        return;
    }
    if sysroot_location()
        .join("lib/wasm32-wasixv2/libc++.a")
        .exists()
    {
        eprintln!("skipping: the sysroot has libc++");
        return;
    }

    let dir = tempfile::TempDir::new().unwrap();
    let dir = dir.path();
    std::fs::write(
        dir.join("plain.cpp"),
        "extern \"C\" int plain(int x) { return x + 1; }\n",
    )
    .unwrap();

    // Compiling C++ that needs no C++ library is left to clang...
    let mut command = tool(dir, "wasix++");
    command.args([
        "--target=wasm32-wasixv2",
        "-c",
        "plain.cpp",
        "-o",
        "plain.o",
    ]);
    run(command);

    // ...but linking C++ needs libc++, which the sysroot does not have yet.
    let mut command = tool(dir, "wasix++");
    command.args(["--target=wasm32-wasixv2", "plain.cpp", "-o", "plain.wasm"]);
    let output = command.output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("C++ is not supported"), "{stderr}");
}

fn run_in_node(host: &Path, wasm: &Path) -> String {
    // Node < 24 needs a flag for exnref exception handling.
    let mut command = Command::new("node");
    if Command::new("node")
        .args(["--experimental-wasm-exnref", "-e", "0"])
        .output()
        .is_ok_and(|output| output.status.success())
    {
        command.arg("--experimental-wasm-exnref");
    }
    let output = command.arg(host).arg(wasm).output().unwrap();
    assert!(
        output.status.success(),
        "{wasm:?} failed under node with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// The (module, name) pairs of a wasm module's imports.
fn imports(bytes: &[u8]) -> Vec<(String, String)> {
    struct Reader<'a>(&'a [u8]);

    impl<'a> Reader<'a> {
        fn byte(&mut self) -> u8 {
            let (first, rest) = self.0.split_first().expect("truncated module");
            self.0 = rest;
            *first
        }

        fn bytes(&mut self, len: usize) -> &'a [u8] {
            let (head, rest) = self.0.split_at(len);
            self.0 = rest;
            head
        }

        fn leb(&mut self) -> u64 {
            let (mut value, mut shift) = (0u64, 0);
            loop {
                let byte = self.byte();
                value |= u64::from(byte & 0x7f) << shift;
                if byte & 0x80 == 0 {
                    return value;
                }
                shift += 7;
            }
        }

        fn name(&mut self) -> String {
            let len = self.leb() as usize;
            String::from_utf8(self.bytes(len).to_vec()).unwrap()
        }

        fn limits(&mut self) {
            let flags = self.byte();
            self.leb();
            if flags & 1 != 0 {
                self.leb();
            }
        }
    }

    let mut reader = Reader(bytes);
    assert_eq!(reader.bytes(8), b"\0asm\x01\0\0\0", "not a wasm module");

    while !reader.0.is_empty() {
        let id = reader.byte();
        let len = reader.leb() as usize;
        let mut section = Reader(reader.bytes(len));
        if id != 2 {
            continue;
        }

        let mut imports = Vec::new();
        for _ in 0..section.leb() {
            let module = section.name();
            let name = section.name();
            match section.byte() {
                // function: type index
                0 => {
                    section.leb();
                }
                // table: reference type, limits
                1 => {
                    section.byte();
                    section.limits();
                }
                // memory: limits
                2 => section.limits(),
                // global: value type, mutability
                3 => {
                    section.byte();
                    section.byte();
                }
                // tag: attribute, type index
                4 => {
                    section.byte();
                    section.leb();
                }
                kind => panic!("unknown import kind {kind}"),
            }
            imports.push((module, name));
        }
        return imports;
    }

    Vec::new()
}
