use crate::{
    args::UserSettings,
    compiler::{
        BuildSettings, DebugLevel, ModuleKind, OptLevel, PreparedArgs, WasmExceptionStyle,
        response_file,
    },
};
use anyhow::Result;
use lexer::{Flag, lex_args};
use std::{collections::HashSet, path::PathBuf, sync::LazyLock};
mod lexer;

static CLANG_FLAGS_WITH_ARGS: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    [
        "-MT",
        "-MF",
        "-MJ",
        "-MQ",
        "-D",
        "-U",
        "-o",
        "-x",
        "-Xpreprocessor",
        "-include",
        "-imacros",
        "-idirafter",
        "-iprefix",
        "-iwithprefix",
        "-iwithprefixbefore",
        "-isysroot",
        "-iwithsysroot",
        "-imultilib",
        "-A",
        "-isystem",
        "-iquote",
        "-install_name",
        "-compatibility_version",
        "-mllvm",
        "-mthread-model",
        "-current_version",
        "-I",
        "-l",
        "-L",
        "-include-pch",
        "--param",
        "-target",
        "--sysroot",
        "-u",
        "-undefined",
        "-Xlinker",
        "-Xclang",
        "-z",
    ]
    .into()
});

static CLANG_FLAGS_WITH_OPTIONAL_ARGS: LazyLock<HashSet<&str>> =
    LazyLock::new(|| ["-ftls-model", "--target"].into());

static CLANG_FLAGS_TO_FORWARD_TO_WASM_LD: LazyLock<HashSet<&str>> =
    LazyLock::new(|| ["-L", "-l"].into());

// We always specify values for these flags according to the build configuration, so
// they must be discarded even if they're provided externally
static CLANG_FLAGS_TO_DISCARD: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    [
        "-ftls-model",
        "--sysroot",
        "--target",
        "-target",
        "-mthread-model",
        "-fwasm-exceptions",
        "--no-wasm-opt",
        "--wasm-opt",
    ]
    .into()
});

static WASM_LD_FLAGS_WITH_ARGS: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    [
        "--export",
        "-flavor",
        "-o",
        "-mllvm",
        "-L",
        "-l",
        "-m",
        "-O",
        "-y",
        "-z",
        "--version-script",
        "--wrap",
        "--unresolved-symbols",
        "--undefined",
        "--trace-symbol",
        "--threads",
        "--thinlto-jobs",
        "--thinlto-cache-policy",
        "--thinlto-cache-dir",
        "--soname",
        "--rsp-quoting",
        "--reproduce",
        "--Map",
        "--keep-section",
        "--export",
        "--export-if-defined",
        "--error-limit",
        "--entry",
        "--version-script",
        "-rpath",
        "--rpath",
    ]
    .into()
});

static WASM_LD_FLAGS_WITH_OPTIONAL_ARGS: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    [
        "--why-extract",
        "--table-base", // Not optional, but there is no form with a freestanding argument
        "--max-memory", // Not optional, but there is no form with a freestanding argument
        "--lto-partitions", // Not optional, but there is no form with a freestanding argument
        "--initial-memory", // Not optional, but there is no form with a freestanding argument
        "--initial-heap", // Not optional, but there is no form with a freestanding argument
        "--import-memory", // Not optional, but there is no form with a freestanding argument
        "--global-base",
        "--features",
        "--extra-features",
        "--export-memory",
        "--color-diagnostics",
        "--build-id",
        "--allow-undefined-file",
    ]
    .into()
});

// Some common linker flags are unsupported by wasm-ld
static WASM_LD_FLAGS_TO_DISCARD: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    [
        "--end-group",
        "--start-group",
        "--as-needed",
        "--no-as-needed",
        "--allow-shlib-undefined",
        "--enable-new-dtags",
        "--version-script",
        "--stats",
        "--no-stats",
    ]
    .into()
});

// Update the build settings based on a standalone compiler argument
fn update_build_settings_from_compiler_flag(
    compiler_arg: Flag,
    build_settings: &mut BuildSettings,
    user_settings: &mut UserSettings,
) {
    match compiler_arg {
        Flag::Simple("-O0") => build_settings.opt_level = OptLevel::O0,
        Flag::Simple("-O1" | "-O") => build_settings.opt_level = OptLevel::O1,
        Flag::Simple("-O2") => build_settings.opt_level = OptLevel::O2,
        Flag::Simple("-O3") => build_settings.opt_level = OptLevel::O3,
        Flag::Simple("-O4") => build_settings.opt_level = OptLevel::O4,
        Flag::Simple("-Os") => build_settings.opt_level = OptLevel::Os,
        Flag::Simple("-Oz") => build_settings.opt_level = OptLevel::Oz,
        Flag::Simple("-g0") => build_settings.debug_level = DebugLevel::G0,
        Flag::Simple("-g1") => build_settings.debug_level = DebugLevel::G1,
        Flag::Simple("-g2" | "-g") => build_settings.debug_level = DebugLevel::G2,
        Flag::Simple("-g3") => build_settings.debug_level = DebugLevel::G3,
        Flag::Simple("-fwasm-exceptions") => {
            user_settings.wasm_exceptions = WasmExceptionStyle::Exnref
        }
        Flag::Simple("-fno-exceptions") => user_settings.wasm_exceptions = WasmExceptionStyle::Off,
        Flag::Simple("-fPIC") => user_settings.pic = true,
        Flag::Simple("-fno-PIC") => user_settings.pic = false,
        Flag::Simple("--wasm-opt") => build_settings.use_wasm_opt = true,
        Flag::Simple("--no-wasm-opt") => build_settings.use_wasm_opt = false,
        Flag::Simple("-c" | "-S" | "-E") => {
            user_settings.module_kind = Some(ModuleKind::ObjectFile);
        }
        Flag::Simple("-shared") => {
            if user_settings.module_kind.is_none() {
                user_settings.module_kind = Some(ModuleKind::SharedLibrary);
            }
        }
        Flag::Simple("-pie") => {
            if user_settings.module_kind.is_none() {
                user_settings.module_kind = Some(ModuleKind::DynamicMain);
            }
        }
        _ => {}
    }
}

// Update the build settings based on a standalone linker argument
fn update_build_settings_from_linker_flag(linker_flag: &Flag, user_settings: &mut UserSettings) {
    match linker_flag {
        Flag::Simple("-shared") => {
            if user_settings.module_kind.is_none() {
                user_settings.module_kind = Some(ModuleKind::SharedLibrary);
            }
        }
        Flag::Simple("-pie") => {
            if user_settings.module_kind.is_none() {
                user_settings.module_kind = Some(ModuleKind::DynamicMain);
            }
        }
        _ => {}
    }
}

fn lex_compiler_args<'a>(
    args: impl IntoIterator<Item = &'a (impl AsRef<str> + 'a)>,
) -> Result<Vec<Flag<'a>>> {
    lex_args(
        args,
        &CLANG_FLAGS_WITH_ARGS,
        &CLANG_FLAGS_WITH_OPTIONAL_ARGS,
    )
}

fn lex_linker_args<'a>(
    args: impl IntoIterator<Item = &'a (impl AsRef<str> + 'a)>,
) -> Result<Vec<Flag<'a>>> {
    lex_args(
        args,
        &WASM_LD_FLAGS_WITH_ARGS,
        &WASM_LD_FLAGS_WITH_OPTIONAL_ARGS,
    )
}

/// Process the clang arguments into PreparedArgs, separating compiler args and linker args, and also extracting build settings from the args.
fn process_compiler_flags<'a>(
    flags: impl IntoIterator<Item = Flag<'a>>,
    build_settings: &mut BuildSettings,
    user_settings: &mut UserSettings,
) -> Result<PreparedArgs> {
    let mut compiler_flags = Vec::new();
    let mut linker_args = Vec::new();
    let mut inputs = Vec::new();
    let mut output = None;

    for compiler_flag in flags {
        match compiler_flag {
            Flag::Simple(flag) | Flag::WithValue(flag, _, _)
                if CLANG_FLAGS_TO_FORWARD_TO_WASM_LD.contains(flag) =>
            {
                linker_args.extend(compiler_flag.to_args());
            }
            Flag::Simple(flag) if flag.starts_with("-Wl,") => {
                for split in flag["-Wl,".len()..].split(',') {
                    linker_args.push(split.to_owned());
                }
            }
            Flag::Simple(arg) | Flag::WithValue(arg, _, _)
                if CLANG_FLAGS_TO_DISCARD.contains(arg) =>
            {
                update_build_settings_from_compiler_flag(
                    compiler_flag,
                    build_settings,
                    user_settings,
                );
                tracing::debug!("Discarding flag '{}'", &compiler_flag);
            }
            Flag::WithValue("-Xlinker", value, _) => {
                linker_args.push(value.to_owned());
            }
            Flag::WithValue("-z", value, _) => {
                linker_args.push("-z".to_owned());
                linker_args.push(value.to_owned());
            }
            Flag::WithValue("-o", value, _) => {
                output = Some(PathBuf::from(value));
            }
            Flag::Positional(value) => {
                inputs.push(PathBuf::from(value));
            }
            Flag::Terminator() => {
                compiler_flags.push(compiler_flag);
            }
            Flag::Simple(_) | Flag::WithValue(_, _, _) => {
                update_build_settings_from_compiler_flag(
                    compiler_flag,
                    build_settings,
                    user_settings,
                );
                compiler_flags.push(compiler_flag);
            }
        }
    }

    let (linker_inputs, compiler_inputs) = inputs.into_iter().partition(|input| {
        matches!(
            input.extension().and_then(|ext| ext.to_str()),
            Some("o") | Some("obj") | Some("a") | Some("so") | Some("dll") | Some("dylib")
        )
    });

    let compiler_args = compiler_flags
        .into_iter()
        .flat_map(|flag| flag.to_args().into_iter())
        .collect::<Vec<_>>();

    Ok(PreparedArgs {
        compiler_args,
        linker_args,
        compiler_inputs,
        linker_inputs,
        output,
    })
}

/// Process the wasm-ld arguments into PreparedArgs, extracting build settings from the args.
fn process_linker_flags<'a>(
    flags: impl IntoIterator<Item = Flag<'a>>,
    user_settings: &mut UserSettings,
) -> Result<PreparedArgs> {
    let mut linker_flags = Vec::new();
    let mut inputs = Vec::new();
    let mut output = None;

    for linker_flag in flags {
        match linker_flag {
            Flag::Simple(arg) | Flag::WithValue(arg, _, _)
                if user_settings.discard_unsupported_flags
                    && WASM_LD_FLAGS_TO_DISCARD.contains(arg) =>
            {
                update_build_settings_from_linker_flag(&linker_flag, user_settings);
                tracing::debug!("Discarding flag '{}'", &linker_flag);
            }
            Flag::WithValue("-o", value, _) => {
                if user_settings.autoconf_workarounds && value == "conftest" {
                    user_settings.run_wasm_opt = Some(false);
                    linker_flags.push(Flag::Simple("--no-shlib-sigcheck"));
                }
                output = Some(PathBuf::from(value));
            }
            Flag::Positional(value) => {
                inputs.push(PathBuf::from(value));
            }
            Flag::Terminator() => {
                linker_flags.push(linker_flag);
            }
            Flag::Simple(_) | Flag::WithValue(_, _, _) => {
                update_build_settings_from_linker_flag(&linker_flag, user_settings);
                linker_flags.push(linker_flag);
            }
        }
    }

    let linker_args = linker_flags
        .into_iter()
        .flat_map(|flag| flag.to_args().into_iter())
        .collect::<Vec<_>>();

    Ok(PreparedArgs {
        compiler_args: Vec::new(),
        linker_args,
        compiler_inputs: Vec::new(),
        linker_inputs: inputs,
        output,
    })
}

// Combine the compiler args with the extra args from user settings
fn add_extra_compiler_flags(
    args: impl IntoIterator<Item = impl Into<String>>,
    user_settings: &mut UserSettings,
    run_cxx: bool,
) -> impl IntoIterator<Item = String> {
    let extra_flags = std::mem::take(&mut user_settings.extra_compiler_flags);
    let extra_flags2 = if run_cxx {
        std::mem::take(&mut user_settings.extra_compiler_flags_cxx)
    } else {
        std::mem::take(&mut user_settings.extra_compiler_flags_c)
    };
    let extra_post_flags = std::mem::take(&mut user_settings.extra_compiler_post_flags);
    let extra_post_flags2 = if run_cxx {
        std::mem::take(&mut user_settings.extra_compiler_post_flags_cxx)
    } else {
        std::mem::take(&mut user_settings.extra_compiler_post_flags_c)
    };

    extra_flags
        .into_iter()
        .chain(extra_flags2)
        .chain(args.into_iter().map(|s| s.into()))
        .chain(extra_post_flags)
        .chain(extra_post_flags2)
}

// Combine the compiler args with the extra args from user settings
fn add_extra_linker_flags(
    linker_args: impl IntoIterator<Item = impl Into<String>>,
    user_settings: &mut UserSettings,
) -> impl IntoIterator<Item = String> {
    let extra_flags = std::mem::take(&mut user_settings.extra_linker_flags);
    extra_flags
        .into_iter()
        .chain(linker_args.into_iter().map(|s| s.into()))
}

// Resolve all response files in the args and return a flat list of args with no response files.
fn resolve_response_files(args: impl IntoIterator<Item = String>) -> Result<Vec<String>> {
    let mut arg_stack = response_file::ArgumentsStack::new(args.into_iter().collect::<Vec<_>>());
    let mut args = Vec::new();
    while let Some(arg) = arg_stack.next()? {
        args.push(arg);
    }
    Ok(args)
}

pub(super) fn prepare_compiler_args(
    args: impl IntoIterator<Item = String>,
    user_settings: &mut UserSettings,
    run_cxx: bool,
) -> Result<(PreparedArgs, BuildSettings)> {
    let mut build_settings = BuildSettings {
        opt_level: OptLevel::O0,
        debug_level: DebugLevel::G0,
        use_wasm_opt: true,
    };

    let args = add_extra_compiler_flags(args, user_settings, run_cxx);
    let args = resolve_response_files(args)?;

    let flags = lex_compiler_args(args.iter())?;

    let mut result = process_compiler_flags(flags, &mut build_settings, user_settings)?;

    if user_settings.autoconf_workarounds
        && (result.compiler_inputs.contains(&"conftest.c".into())
            || result.compiler_inputs.contains(&"conftest.cpp".into())
            || result.output == Some("conftest".into()))
    {
        // wasm opt fails if signature mismatches produce an invalid module
        user_settings.run_wasm_opt = Some(false);
        // Pass the flag to the linker to avoid shlib signature checks
        result.linker_args.push("--no-shlib-sigcheck".to_owned());
    }

    let linker_result = prepare_linker_args(result.linker_args, user_settings)?;

    result.linker_args = linker_result.linker_args;
    result.linker_inputs.extend(linker_result.linker_inputs);
    if result.output.is_none() {
        result.output = linker_result.output;
    }

    if user_settings.module_kind().requires_pic() {
        user_settings.pic = true;
    }

    Ok((result, build_settings))
}

pub(super) fn prepare_linker_args(
    args: impl IntoIterator<Item = String>,
    user_settings: &mut UserSettings,
) -> Result<PreparedArgs> {
    let args = add_extra_linker_flags(args, user_settings);
    let args = resolve_response_files(args)?;
    let flags = lex_linker_args(&args)?;

    let result = process_linker_flags(flags, user_settings)?;

    if user_settings.module_kind().requires_pic() {
        user_settings.pic = true;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UserSettings, compiler::flags::lexer::Separator};
    use std::path::PathBuf;

    #[test]
    fn test_update_build_settings_from_arg() {
        let mut bs = BuildSettings {
            opt_level: OptLevel::O0,
            debug_level: DebugLevel::G0,
            use_wasm_opt: true,
        };
        let mut us = UserSettings::default();
        update_build_settings_from_compiler_flag(Flag::Simple("-O3"), &mut bs, &mut us);
        assert_eq!(bs.opt_level, OptLevel::O3);
        update_build_settings_from_compiler_flag(Flag::Simple("-g1"), &mut bs, &mut us);
        assert_eq!(bs.debug_level, DebugLevel::G1);
        update_build_settings_from_compiler_flag(Flag::Simple("--no-wasm-opt"), &mut bs, &mut us);
        update_build_settings_from_compiler_flag(
            Flag::Simple("-fwasm-exceptions"),
            &mut bs,
            &mut us,
        );
        assert_eq!(us.wasm_exceptions, WasmExceptionStyle::Exnref);
        update_build_settings_from_compiler_flag(Flag::Simple("-fno-exceptions"), &mut bs, &mut us);
        assert_eq!(us.wasm_exceptions, WasmExceptionStyle::Off);
        us = UserSettings::default();
        update_build_settings_from_compiler_flag(
            Flag::Simple("-fwasm-exceptions"),
            &mut bs,
            &mut us,
        );
        assert_eq!(us.wasm_exceptions, WasmExceptionStyle::Exnref);

        // update_build_settings_from_compiler_flag should NOT override the exception style based on a linker setting.
        update_build_settings_from_compiler_flag(
            Flag::Simple("-Wl,-mllvm,--wasm-use-legacy-eh=true"),
            &mut bs,
            &mut us,
        );
        assert_eq!(us.wasm_exceptions, WasmExceptionStyle::Exnref);

        // update_build_settings_from_compiler_flag should override the exception style based on a compiler setting.
        update_build_settings_from_compiler_flag(
            Flag::WithValue("-mllvm", "--wasm-use-legacy-eh=true", Separator::Space),
            &mut bs,
            &mut us,
        );
        assert_eq!(us.wasm_exceptions, WasmExceptionStyle::Legacy);

        // Verify toggling eh kind is still saved after disabling EH in general
        update_build_settings_from_compiler_flag(Flag::Simple("-fno-exceptions"), &mut bs, &mut us);
        assert_eq!(us.wasm_exceptions, WasmExceptionStyle::Off);
        update_build_settings_from_compiler_flag(
            Flag::WithValue("-mllvm", "--wasm-use-legacy-eh=false", Separator::Space),
            &mut bs,
            &mut us,
        );
        assert_eq!(us.wasm_exceptions, WasmExceptionStyle::Exnref);
    }

    #[test]
    fn test_discarding_compiler_flags_works() {
        // Assert that prepare_compiler_args discards the flags it is supposed to discard
        let mut us = UserSettings::default();
        let args = vec![
            // Standalone flags that should be discarded
            "-fwasm-exceptions".to_string(),
            "--no-wasm-opt".to_string(),
            "--wasm-opt".to_string(),
            "--sysroot".to_string(),
            "/some/sysroot".to_string(),
            "-target".to_string(),
            "wasm32-wasi".to_string(),
            "-mthread-model".to_string(),
            "single".to_string(),
            // Flags-with-value that should be discarded (joined with =)
            "-ftls-model=local-exec".to_string(),
            // Flags with optional args should not cause the next arg to be treated as their value
            "-ftls-model".to_string(),
            "not_discarded.c".to_string(),
            "--sysroot=/other/sysroot".to_string(),
            "--target=wasm32-wasi".to_string(),
            // A normal flag that must NOT be discarded
            "-O2".to_string(),
            "in.c".to_string(),
        ];
        let (pa, bs) = prepare_compiler_args(args, &mut us, false).unwrap();

        // Only -O2 should survive in compiler_args
        assert_eq!(pa.compiler_args, vec!["-O2".to_string()]);
        assert_eq!(bs.opt_level, OptLevel::O2);
        // -fwasm-exceptions side-effect should still take effect even though the flag is discarded
        assert_eq!(us.wasm_exceptions, WasmExceptionStyle::Exnref);
        assert!(pa.linker_args.is_empty());
        assert_eq!(
            pa.compiler_inputs,
            vec![PathBuf::from("not_discarded.c"), PathBuf::from("in.c")]
        );
        assert!(pa.linker_inputs.is_empty());
    }

    #[test]
    fn test_prepare_compiler_args_lib_flag() {
        let mut us = UserSettings::default();
        let args = vec![
            "-lmylib".to_string(),
            "-l".to_string(),
            "mylib".to_string(),
            "in.c".to_string(),
        ];
        let (pa, _) = prepare_compiler_args(args, &mut us, false).unwrap();
        assert_eq!(
            pa.linker_args,
            vec!["-lmylib".to_string(), "-l".to_string(), "mylib".to_string(),]
        );
        assert_eq!(pa.compiler_inputs, vec![PathBuf::from("in.c")]);
        assert!(pa.linker_inputs.is_empty());
    }

    #[test]
    fn test_prepare_linker_args_lib_flag() {
        let mut us = UserSettings::default();
        let args = vec![
            "-lmylib".to_string(),
            "-l".to_string(),
            "mylib".to_string(),
            "input.o".to_string(),
        ];
        let pa = prepare_linker_args(args, &mut us).unwrap();
        assert_eq!(
            pa.linker_args,
            vec!["-lmylib".to_string(), "-l".to_string(), "mylib".to_string(),]
        );
        assert_eq!(pa.linker_inputs, vec![PathBuf::from("input.o")]);
    }

    #[test]
    fn test_prepare_compiler_args_and_build_settings() {
        let mut us = UserSettings::default();
        let args = vec![
            "-O2".to_string(),
            "-g0".to_string(),
            "-fwasm-exceptions".to_string(),
            "--no-wasm-opt".to_string(),
            "-Wl,-foo,bar".to_string(),
            "-Xlinker".to_string(),
            "baz".to_string(),
            "-z".to_string(),
            "zo".to_string(),
            "-o".to_string(),
            "out".to_string(),
            "in.c".to_string(),
            "lib.o".to_string(),
        ];
        let (pa, bs) = prepare_compiler_args(args, &mut us, false).unwrap();
        assert_eq!(bs.opt_level, OptLevel::O2);
        assert_eq!(bs.debug_level, DebugLevel::G0);
        assert!(!bs.use_wasm_opt);
        assert_eq!(us.wasm_exceptions, WasmExceptionStyle::Exnref);
        assert_eq!(pa.compiler_args, vec!["-O2".to_string(), "-g0".to_string()]);
        assert_eq!(
            pa.linker_args,
            vec!["-foo".to_string(), "-z".to_string(), "zo".to_string()]
        );
        assert_eq!(pa.output, Some(PathBuf::from("out")));
        assert_eq!(pa.compiler_inputs, vec![PathBuf::from("in.c")]);
        assert_eq!(
            pa.linker_inputs,
            vec![
                PathBuf::from("lib.o"),
                PathBuf::from("bar"),
                PathBuf::from("baz"),
            ]
        );
    }

    #[test]
    fn test_prepare_linker_args() {
        let mut us = UserSettings::default();
        let args = vec![
            "-o".to_string(),
            "out.wasm".to_string(),
            "-shared".to_string(),
            "-m".to_string(),
            "module".to_string(),
            "mod.wasm".to_string(),
        ];
        let pa = prepare_linker_args(args, &mut us).unwrap();
        assert_eq!(pa.output, Some(PathBuf::from("out.wasm")));
        assert_eq!(
            pa.linker_args,
            vec![
                "-shared".to_string(),
                "-m".to_string(),
                "module".to_string()
            ]
        );
        assert_eq!(pa.linker_inputs, vec![PathBuf::from("mod.wasm")]);
        assert_eq!(us.module_kind, Some(ModuleKind::SharedLibrary));
    }

    #[test]
    fn test_autoconf_workaround() {
        let mut us = UserSettings::default();
        us.autoconf_workarounds = true;
        let args = vec![
            "-o".to_string(),
            "conftest".to_string(),
            "conftest.c".to_string(),
        ];
        let (pa, _) = prepare_compiler_args(args, &mut us, false).unwrap();

        // Should have disabled wasm-opt
        assert_eq!(us.run_wasm_opt, Some(false));
        // Should have added --no-shlib-sigcheck
        assert_eq!(pa.linker_args, vec!["--no-shlib-sigcheck".to_string()]);
    }

    #[test]
    fn test_prepare_compiler_args_discard_linker_flags_via_wl() {
        let mut us = UserSettings::default();
        us.discard_unsupported_flags = true;
        let args = vec![
            "-Wl,--start-group".to_string(),
            "-Wl,--end-group".to_string(),
            "-Wl,--as-needed".to_string(),
            "-Wl,-L/some/path".to_string(),
            "-Wl,--version-script=/path/to/script".to_string(),
            "-Wl,--version-script,/path/to/script2".to_string(),
            "-Wl,--stats".to_string(),
            "test.c".to_string(),
        ];
        let (pa, _) = prepare_compiler_args(args, &mut us, false).unwrap();

        // Only -L should be forwarded, all discard flags should be filtered out
        assert_eq!(pa.linker_args, vec!["-L/some/path".to_string()]);
    }

    #[test]
    fn test_prepare_compiler_args_discard_linker_flags_multiple_via_wl() {
        let mut us = UserSettings::default();
        us.discard_unsupported_flags = true;
        let args = vec![
            "-Wl,--end-group".to_string(),
            "-Wl,--end-group,-L/some/path/a,--end-group".to_string(),
            "-Wl,--end-group,--version-script=/path/to/script,-L/some/path/b,--end-group"
                .to_string(),
            "-Wl,--end-group,--version-script,/path/to/script,-L/some/path/c,--end-group"
                .to_string(),
            "test.c".to_string(),
        ];
        let (pa, _) = prepare_compiler_args(args, &mut us, false).unwrap();

        // Only -L should be forwarded, all discard flags should be filtered out
        assert_eq!(
            pa.linker_args,
            vec![
                "-L/some/path/a".to_string(),
                "-L/some/path/b".to_string(),
                "-L/some/path/c".to_string()
            ]
        );
    }

    #[test]
    fn test_prepare_compiler_args_does_not_discard_linker_flags_by_default() {
        let mut us = UserSettings::default();
        let args = vec![
            "-Xlinker".to_string(),
            "--start-group".to_string(),
            "-Xlinker".to_string(),
            "-L/some/path".to_string(),
            "test.c".to_string(),
        ];
        let (pa, _) = prepare_compiler_args(args, &mut us, false).unwrap();

        assert_eq!(
            pa.linker_args,
            vec!["--start-group".to_string(), "-L/some/path".to_string()]
        );
    }

    #[test]
    fn test_prepare_compiler_args_discard_linker_flags_via_xlinker() {
        let mut us = UserSettings::default();
        us.discard_unsupported_flags = true;
        let args = vec![
            "-Xlinker".to_string(),
            "--start-group".to_string(),
            "-Xlinker".to_string(),
            "--as-needed".to_string(),
            "-Xlinker".to_string(),
            "-L/some/path".to_string(),
            "-Xlinker".to_string(),
            "--version-script=path/to/script".to_string(),
            "test.c".to_string(),
        ];
        let (pa, _) = prepare_compiler_args(args, &mut us, false).unwrap();

        // Only -L should be forwarded, all discard flags should be filtered out
        assert_eq!(pa.linker_args, vec!["-L/some/path".to_string()]);
    }

    #[test]
    fn test_prepare_compiler_args_discard_linker_flags_via_xlinker_two_arg() {
        let mut us = UserSettings::default();
        us.discard_unsupported_flags = true;
        let args = vec![
            "-Xlinker".to_string(),
            "--start-group".to_string(),
            "-Xlinker".to_string(),
            "--version-script".to_string(),
            "-Xlinker".to_string(),
            "/path/to/script".to_string(),
            "-Xlinker".to_string(),
            "-L/some/path".to_string(),
            "-Xlinker".to_string(),
            "--version-script".to_string(),
            "-Xlinker".to_string(),
            "/path/to/script2".to_string(),
            "test.c".to_string(),
        ];
        let (pa, _) = prepare_compiler_args(args, &mut us, false).unwrap();

        // Only -L should be forwarded, all discard flags (including their arguments) should be filtered out
        assert_eq!(pa.linker_args, vec!["-L/some/path".to_string()]);
    }

    #[test]
    fn test_prepare_compiler_args_discard_linker_flags_via_xlinker_incomplete() {
        let mut us = UserSettings::default();
        // Missing the argument after the second -Xlinker
        let args = vec![
            "-Xlinker".to_string(),
            "--version-script".to_string(),
            "-Xlinker".to_string(),
        ];
        let result = prepare_compiler_args(args, &mut us, false);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Expected argument after -Xlinker")
        );
    }

    #[test]
    fn test_prepare_linker_args_discard_flags() {
        let mut us = UserSettings::default();
        us.discard_unsupported_flags = true;
        let args = vec![
            "--start-group".to_string(),
            "--end-group".to_string(),
            "--as-needed".to_string(),
            "-L/some/path".to_string(),
            "--version-script=/path/to/script".to_string(),
            "--version-script".to_string(),
            "another.txt".to_string(),
            "--stats".to_string(),
            "-o".to_string(),
            "output.wasm".to_string(),
            "input.o".to_string(),
        ];
        let pa = prepare_linker_args(args, &mut us).unwrap();

        assert_eq!(pa.linker_args, vec!["-L/some/path".to_string()]);
        assert_eq!(pa.output, Some(PathBuf::from("output.wasm")));
        assert_eq!(pa.linker_inputs, vec![PathBuf::from("input.o")]);
    }
}
