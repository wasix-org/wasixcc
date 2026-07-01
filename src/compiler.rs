use crate::{
    args::UserSettings,
    compiler::flags::{prepare_compiler_args, prepare_linker_args},
};
use anyhow::{Context, Result, bail};
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

mod flags;
mod response_file;

static WASM_OPT_ENABLED_FEATURES: &[&str] = &[
    "--enable-threads",
    "--enable-mutable-globals",
    "--enable-bulk-memory",
    "--enable-bulk-memory-opt",
    "--enable-exception-handling",
    "--enable-simd",
    "--enable-relaxed-simd",
    "--enable-extended-const",
    // Unsupported by wasm-opt right now:
    // https://github.com/WebAssembly/binaryen/issues/8544
    // "--enable-wide-arithmetic",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModuleKind {
    StaticMain,
    DynamicMain,
    SharedLibrary,
    ObjectFile,
}

impl ModuleKind {
    pub fn requires_pic(&self) -> bool {
        matches!(self, ModuleKind::DynamicMain | ModuleKind::SharedLibrary)
    }

    pub fn is_binary(&self) -> bool {
        matches!(
            self,
            ModuleKind::StaticMain | ModuleKind::DynamicMain | ModuleKind::SharedLibrary
        )
    }

    pub fn is_executable(&self) -> bool {
        matches!(self, ModuleKind::StaticMain | ModuleKind::DynamicMain)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WasmExceptionStyle {
    Off,
    Legacy,
    Exnref,
}

impl WasmExceptionStyle {
    pub fn is_enabled(&self) -> bool {
        !matches!(self, WasmExceptionStyle::Off)
    }

    pub fn is_legacy(&self) -> bool {
        matches!(self, WasmExceptionStyle::Legacy)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OptLevel {
    O0,
    O1,
    O2,
    O3,
    O4,
    Os,
    Oz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DebugLevel {
    G0,
    G1,
    G2,
    G3,
}

/// Settings derived strictly from compiler flags.
#[derive(Debug)]
pub(crate) struct BuildSettings {
    opt_level: OptLevel,
    debug_level: DebugLevel,
    use_wasm_opt: bool,
}

#[derive(Debug)]
pub(crate) struct PreparedArgs {
    compiler_args: Vec<String>,
    linker_args: Vec<String>,
    compiler_inputs: Vec<PathBuf>,
    linker_inputs: Vec<PathBuf>,
    output: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct State {
    user_settings: UserSettings,
    build_settings: BuildSettings,
    args: PreparedArgs,
    cxx: bool,
    temp_dir: PathBuf,
}

pub(crate) fn run(args: Vec<String>, mut user_settings: UserSettings, run_cxx: bool) -> Result<()> {
    let original_args = args.clone();

    let (args, build_settings) = prepare_compiler_args(args, &mut user_settings, run_cxx)?;

    tracing::debug!("User settings: {user_settings:?}");
    tracing::debug!("Build settings: {build_settings:?}");
    tracing::debug!("Compiler/linker args: {args:?}");

    if args.compiler_inputs.is_empty() && args.linker_inputs.is_empty() {
        // If there are no inputs, just pass everything through to clang.
        // This lets us support invocations such as `wasixcc -dumpmachine`.
        let mut command = Command::new(user_settings.llvm_location.get_tool_path(if run_cxx {
            "clang++"
        } else {
            "clang"
        }));
        command.args(original_args);
        command.args([OsStr::new("--target=wasm32-wasi")]);
        return run_command(command);
    }

    let temp_dir = tempfile::TempDir::new().context("Failed to create temporary directory")?;

    let mut state = State {
        user_settings,
        build_settings,
        args,
        cxx: run_cxx,
        temp_dir: temp_dir.path().to_owned(),
    };

    if !state.args.compiler_inputs.is_empty() {
        compile_inputs(&mut state)?;
    }

    if state.user_settings.module_kind().is_binary() {
        link_inputs(&state)?;

        // Run wasm-opt if:
        //  * Explicitly enabled in the user settings, or
        //  * It wasn't disabled in the compiler flags AND it wasn't explicitly disabled in the user settings
        if matches!(
            (
                state.build_settings.use_wasm_opt,
                state.user_settings.run_wasm_opt,
            ),
            (_, Some(true)) | (true, None)
        ) {
            run_wasm_opt(&state)?;
        }
    }

    if state.user_settings.module_kind().is_executable()
        && state.user_settings.generate_shell_script
    {
        generate_shell_script(&state)?;
    }

    tracing::info!("Done");
    Ok(())
}

pub(crate) fn link_only(args: Vec<String>, mut user_settings: UserSettings) -> Result<()> {
    let original_args = args.clone();

    let args = prepare_linker_args(args, &mut user_settings)?;

    if !user_settings.module_kind().is_binary() {
        bail!(
            "Only binaries can be linked, current module kind is: {:?}",
            user_settings.module_kind()
        );
    }

    tracing::debug!("User settings: {user_settings:?}");
    tracing::debug!("Linker args: {args:?}");

    if args.linker_inputs.is_empty() {
        // If there are no inputs, just pass everything through to wasm-ld.
        let mut command = Command::new(user_settings.llvm_location.get_tool_path("wasm-ld"));
        command.args(original_args);
        return run_command(command);
    }

    let build_settings = BuildSettings {
        opt_level: OptLevel::O0,
        debug_level: DebugLevel::G0,
        use_wasm_opt: user_settings.run_wasm_opt.unwrap_or(true),
    };

    let state = State {
        user_settings,
        build_settings,
        args,
        // TODO: is there a way to figure this out automatically?
        cxx: false,
        // Not used for linking
        temp_dir: PathBuf::from("."),
    };

    link_inputs(&state)?;

    if state.build_settings.use_wasm_opt {
        run_wasm_opt(&state)?;
    }

    tracing::info!("Done");
    Ok(())
}

fn output_path(state: &State) -> Result<PathBuf> {
    let output_path = if let Some(output) = &state.args.output {
        output.as_path()
    } else {
        match state.user_settings.module_kind() {
            ModuleKind::StaticMain | ModuleKind::DynamicMain | ModuleKind::SharedLibrary => {
                Path::new("a.out")
            }
            ModuleKind::ObjectFile => Path::new("a.o"),
        }
    };

    if state.user_settings.generate_shell_script {
        match output_path.extension() {
            Some(ext) if ext == OsStr::new("wasm") => Ok(output_path.to_owned()),
            _ => {
                let Some(file_name) = output_path.file_name() else {
                    bail!(
                        "Cannot deduce output file name from path: {}",
                        output_path.display()
                    )
                };
                let mut file_name = file_name.to_os_string();
                file_name.push(".wasm");
                Ok(output_path.with_file_name(file_name))
            }
        }
    } else {
        Ok(output_path.to_owned())
    }
}

fn shell_script_path(state: &State) -> Result<(PathBuf, OsString)> {
    assert!(
        state.user_settings.generate_shell_script,
        "Should only call this function when shell scripts are requested"
    );

    let output_path = output_path(state)?;
    assert!(
        matches!(output_path.extension(), Some(ext) if ext == OsStr::new("wasm")),
        "Output file name must have a .wasm extension"
    );

    let output_file_name = output_path
        .file_name()
        .expect("Output path should have a file name")
        .to_owned();

    let mut script_path = output_path;
    assert!(script_path.set_extension(""));
    Ok((script_path, output_file_name))
}

fn compile_inputs(state: &mut State) -> Result<()> {
    let compiler_path = state
        .user_settings
        .llvm_location
        .get_tool_path(if state.cxx { "clang++" } else { "clang" });

    let sysroot_path = state.user_settings.ensure_sysroot_location()?;

    let mut command_args: Vec<&OsStr> = vec![
        OsStr::new("--sysroot"),
        sysroot_path.as_os_str(),
        OsStr::new("--target=wasm32-wasi"),
        OsStr::new("-c"),
        OsStr::new("-matomics"),
        OsStr::new("-mbulk-memory"),
        OsStr::new("-mmutable-globals"),
        OsStr::new("-pthread"),
        OsStr::new("-mthread-model"),
        OsStr::new("posix"),
        OsStr::new("-fno-trapping-math"),
        OsStr::new("-D_WASI_EMULATED_MMAN"),
        OsStr::new("-D_WASI_EMULATED_SIGNAL"),
        OsStr::new("-D_WASI_EMULATED_PROCESS_CLOCKS"),
    ];

    if state.user_settings.wasm_exceptions.is_enabled() {
        command_args.push(OsStr::new("-fwasm-exceptions"));

        command_args.push(OsStr::new("-mllvm"));
        command_args.push(OsStr::new("--wasm-enable-eh"));

        command_args.push(OsStr::new("-mllvm"));
        command_args.push(OsStr::new("--wasm-enable-sjlj"));

        if state.user_settings.wasm_exceptions.is_legacy() {
            command_args.push(OsStr::new("-mllvm"));
            command_args.push(OsStr::new("--wasm-use-legacy-eh=true"));
        } else {
            command_args.push(OsStr::new("-mllvm"));
            command_args.push(OsStr::new("--wasm-use-legacy-eh=false"));
        }
    } else {
        command_args.push(OsStr::new("-fno-exceptions"));
    }

    if state.user_settings.module_kind().requires_pic() || state.user_settings.pic {
        command_args.push(OsStr::new("-fPIC"));
        command_args.push(OsStr::new("-ftls-model=global-dynamic"));
        command_args.push(OsStr::new("-fvisibility=default"));
    } else {
        command_args.push(OsStr::new("-ftls-model=local-exec"));
    }

    match state.build_settings.debug_level {
        DebugLevel::G0 => (),
        DebugLevel::G1 => command_args.push(OsStr::new("-g1")),
        DebugLevel::G2 => command_args.push(OsStr::new("-g2")),
        DebugLevel::G3 => command_args.push(OsStr::new("-g3")),
    }

    for arg in &state.args.compiler_args {
        command_args.push(OsStr::new(arg.as_str()));
    }

    if state.user_settings.module_kind().is_binary() {
        // If we're linking later, we should compile each input separately

        let mut filename_counter = HashMap::new();

        for input in &state.args.compiler_inputs {
            let mut command = Command::new(&compiler_path);

            command.args(&command_args);

            command.arg(input);

            let output_path = {
                let input_name = input.file_name().unwrap_or_else(|| OsStr::new("output"));
                let counter = filename_counter.entry(input_name.to_owned()).or_insert(0);
                let mut output_name = input_name.to_owned();
                output_name.push(format!(".{}.o", counter));
                *counter += 1;
                state.temp_dir.join(output_name)
            };

            command.arg("-o").arg(&output_path);
            state.args.linker_inputs.push(output_path);

            run_command(command)?;
        }
    } else {
        // If we're not linking, just push all inputs to clang to get one output

        let mut command = Command::new(&compiler_path);

        command.args(&command_args);
        command.args(&state.args.compiler_inputs);
        if state.user_settings.module_kind().is_binary() {
            command.arg("--no-wasm-opt");
        }
        if let Some(output_path) = state.args.output.as_ref() {
            command.arg("-o").arg(output_path);
        }

        run_command(command)?;
    }

    Ok(())
}

fn link_inputs(state: &State) -> Result<()> {
    let linker_path = state.user_settings.llvm_location.get_tool_path("wasm-ld");

    let sysroot_path = state.user_settings.ensure_sysroot_location()?;
    let sysroot_lib_path = sysroot_path.join("lib");
    let sysroot_lib_wasm32_path = sysroot_lib_path.join("wasm32-wasi");

    let mut command = Command::new(linker_path);

    command.args(&state.args.linker_args);

    command.args([
        "--extra-features=atomics",
        "--extra-features=bulk-memory",
        "--extra-features=mutable-globals",
        "--shared-memory",
        "--max-memory=4294967296", // TODO: make configurable
        "--import-memory",
        "--export-dynamic",
        "--export=__wasm_call_ctors",
        // Do not demangle the function names (happens by default)
        "--no-demangle",
    ]);

    if state.user_settings.wasm_exceptions.is_enabled() {
        command.args(["-mllvm", "--wasm-enable-eh"]);
        command.args(["-mllvm", "--wasm-enable-sjlj"]);

        if state.user_settings.wasm_exceptions.is_legacy() {
            command.args(["-mllvm", "--wasm-use-legacy-eh=true"]);
        } else {
            command.args(["-mllvm", "--wasm-use-legacy-eh=false"]);
        }

        // Use native wasm exceptions
        command.args(["-mllvm", "--exception-model=wasm"]);
    }

    let module_kind = state.user_settings.module_kind();

    command.args([
        "--export=__wasm_init_tls",
        "--export=__wasm_signal",
        "--export=__tls_size",
        "--export=__tls_align",
        "--export=__tls_base",
        "--export-if-defined=__indirect_function_table", // needed for reflection and call_dynamic
    ]);

    if module_kind.is_executable() {
        command.args([
            "--export-if-defined=__stack_pointer",
            "--export-if-defined=__heap_base",
            "--export-if-defined=__data_end",
        ]);
    }

    if matches!(module_kind, ModuleKind::DynamicMain) {
        command.args(["--whole-archive", "--export-all"]);
    }

    // Make sysroots libs available to all modules so they can optionally
    // link against them if needed, even when we don't.
    let mut lib_arg = OsString::new();
    lib_arg.push("-L");
    lib_arg.push(&sysroot_lib_path);
    command.arg(lib_arg);

    let mut lib_arg = OsString::new();
    lib_arg.push("-L");
    lib_arg.push(&sysroot_lib_wasm32_path);
    command.arg(lib_arg);

    if module_kind.is_executable() {
        command.args([
            "-lwasi-emulated-getpid",
            "-lwasi-emulated-mman",
            "-lwasi-emulated-process-clocks",
            "-lc",
            "-lresolv",
            "-lrt",
            "-lm",
            "-lpthread",
            "-lutil",
        ]);

        if state.cxx || state.user_settings.include_cpp_symbols {
            command.args(["-lc++", "-lc++abi"]);
            if state.user_settings.wasm_exceptions.is_enabled() {
                command.arg("-lunwind");
            }
        }
    }

    // A C++ shared library (e.g. a CPython extension module) may be loaded by a host
    // that doesn't export the C++ runtime -- the CPython interpreter is plain C -- so
    // its libc++/libc++abi references would otherwise be left as unresolved dynamic
    // imports (SharedLibrary sets --unresolved-symbols=import-dynamic below) and fail
    // at load. Link the runtime into the module instead. Only SharedLibrary reaches
    // here; executables get the C++ runtime in the block above. wasm-ld extracts
    // archive members lazily regardless of command-line order, so only the referenced
    // members are pulled in.
    if !module_kind.is_executable() && (state.cxx || state.user_settings.include_cpp_symbols) {
        command.args(["-lc++", "-lc++abi"]);
        if state.user_settings.wasm_exceptions.is_enabled() {
            command.arg("-lunwind");
        }
    }

    if matches!(module_kind, ModuleKind::DynamicMain) {
        command.args(["--no-whole-archive"]);
    }

    // Link as much as needed out of libclang_rt.builtins regardless of module kind.
    command.arg("-lclang_rt.builtins-wasm32");

    if state.user_settings.module_kind().requires_pic() {
        command.args([
            "--experimental-pic",
            "--export-if-defined=__wasm_apply_data_relocs",
            "--export-if-defined=__wasm_apply_tls_relocs",
        ]);
    }

    match module_kind {
        ModuleKind::StaticMain => {
            // TODO: make configurable
            command.args(["-z", "stack-size=8388608"]);
        }

        ModuleKind::DynamicMain => {
            command.args(["-pie", "-lcommon-tag-stubs"]);
        }

        ModuleKind::SharedLibrary => {
            command.args([
                "-shared",
                "--no-entry",
                "--unresolved-symbols=import-dynamic",
            ]);
            if state.user_settings.link_symbolic {
                command.arg("-Bsymbolic");
            }
        }

        ModuleKind::ObjectFile => panic!("Internal error: object files can't be linked"),
    }

    command.args(&state.args.linker_inputs);

    if module_kind.is_executable() {
        command.arg(sysroot_lib_wasm32_path.join("crt1.o"));
    } else {
        command.arg(sysroot_lib_wasm32_path.join("scrt1.o"));
    }

    command.arg("-o");
    command.arg(output_path(state)?);

    run_command(command)
}

fn run_wasm_opt(state: &State) -> Result<()> {
    let mut command = Command::new(
        state
            .user_settings
            .binaryen_location
            .get_tool_path("wasm-opt"),
    );

    if !state.user_settings.wasm_opt_suppress_default {
        match state.user_settings.wasm_exceptions {
            WasmExceptionStyle::Exnref => {
                // No conversion to exnref needed
            }
            WasmExceptionStyle::Legacy => {
                // Convert old eh to new exnref
                command.arg("--emit-exnref");
            }
            WasmExceptionStyle::Off => {
                command.arg("--asyncify");
            }
        }

        if !state
            .user_settings
            .wasm_opt_flags
            .iter()
            .any(|o| o.starts_with("-O"))
        {
            match state.build_settings.opt_level {
                // -O0 does nothing, no need to specify it
                OptLevel::O0 => (),
                OptLevel::O1 => {
                    command.arg("-O1");
                }
                OptLevel::O2 => {
                    command.arg("-O2");
                }
                OptLevel::O3 => {
                    command.arg("-O3");
                }
                OptLevel::O4 => {
                    command.arg("-O4");
                }
                OptLevel::Os => {
                    command.arg("-Os");
                }
                OptLevel::Oz => {
                    command.arg("-Oz");
                }
            }
        }
    }

    command.args(&state.user_settings.wasm_opt_flags);

    if command.get_args().next().is_none() {
        tracing::info!("Skipping wasm-opt as no passes were specified or needed");
        return Ok(());
    }

    match state.build_settings.debug_level {
        DebugLevel::G0 => (),
        DebugLevel::G1 | DebugLevel::G2 | DebugLevel::G3 => {
            command.arg("-g");
        }
    }

    command.arg("--no-validation");

    command.args(WASM_OPT_ENABLED_FEATURES);

    let output_path = output_path(state)?;

    command.arg("-o");
    command.arg(&output_path);

    if state.user_settings.wasm_opt_preserve_unoptimized {
        let tempdir = tempfile::TempDir::new()
            .context("Failed to create temporary directory for wasm-opt")?;
        let unoptimized_path = tempdir.path().join("unoptimized.wasm");
        std::fs::copy(output_path, &unoptimized_path)
            .context("Failed to create copy of unoptimized artifact before running wasm-opt")?;
        command.arg(&unoptimized_path);
        match run_command(command) {
            Ok(()) => Ok(()),
            Err(e) => {
                let kept_path = tempdir.keep();
                eprintln!(
                    "failed to run wasm-opt, preserving unoptimized artifact at {}",
                    kept_path.display()
                );
                Err(e)
            }
        }
    } else {
        command.arg(&output_path);
        run_command(command)
    }
}

fn generate_shell_script(state: &State) -> Result<()> {
    fn write_file(
        file: &mut std::fs::File,
        output_file_name: &OsStr,
        wasmer_args: &[impl AsRef<str>],
    ) -> std::io::Result<()> {
        writeln!(file, "#! /bin/sh")?;
        writeln!(
            file,
            r#"SCRIPT_DIR=$(cd -- "$(dirname -- "$(realpath "$0" )" )" && pwd)"#
        )?;
        write!(file, "wasmer run ")?;
        for arg in wasmer_args {
            write!(file, r#""{}" "#, arg.as_ref())?;
        }
        // TODO: this fails for non-UTF8 paths
        writeln!(
            file,
            r#""$SCRIPT_DIR/{}" -- "$@""#,
            output_file_name.display()
        )?;

        Ok(())
    }

    let (script_path, output_file_name) = shell_script_path(state)?;

    tracing::info!("Generating shell script at {}", script_path.display());

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&script_path)
        .context("Failed to open shell script file")?;

    if state.user_settings.shell_script_wasmer_args.is_empty() {
        write_file(
            &mut file,
            &output_file_name,
            &[
                "--forward-host-env",
                "--net",
                "--dir",
                "$SCRIPT_DIR",
                "--cwd",
                "$SCRIPT_DIR",
            ],
        )
    } else {
        write_file(
            &mut file,
            &output_file_name,
            &state.user_settings.shell_script_wasmer_args,
        )
    }
    .context("Failed to write to shell script")?;

    file.flush().context("Failed to flush script file")?;
    drop(file);

    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&script_path)
            .context("Failed to stat script file")?
            .permissions();
        perms.set_mode(perms.mode() | 0o110);
        std::fs::set_permissions(&script_path, perms)
            .context("Failed to set executable permissions for script file")?;
    }

    Ok(())
}

pub fn run_command(mut command: Command) -> Result<()> {
    tracing::debug!("Executing build command: {command:?}");

    let status = command
        .status()
        .with_context(|| format!("Failed to run command: {command:?}"))?;
    if !status.success() {
        bail!("Command failed with status: {status}; the command was: {command:?}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UserSettings;
    use std::{fs, path::PathBuf};
    use tempfile::TempDir;

    #[test]
    fn test_sysroot_prefix() {
        let mut us = UserSettings {
            sysroot_prefix: PathBuf::from("/xxx"),
            ..Default::default()
        };
        assert_eq!(us.wasm_exceptions, WasmExceptionStyle::Exnref);
        assert_eq!(
            us.sysroot_location().unwrap(),
            PathBuf::from("/xxx/sysroot-exnref-eh")
        );

        us.wasm_exceptions = WasmExceptionStyle::Legacy;

        assert_eq!(
            us.sysroot_location().unwrap(),
            PathBuf::from("/xxx/sysroot-eh")
        );

        us.wasm_exceptions = WasmExceptionStyle::Off;
        assert_eq!(
            us.sysroot_location().unwrap(),
            PathBuf::from("/xxx/sysroot")
        );

        us.pic = true;
        us.wasm_exceptions = WasmExceptionStyle::Exnref;
        assert_eq!(
            us.sysroot_location().unwrap(),
            PathBuf::from("/xxx/sysroot-exnref-ehpic")
        );

        us.pic = true;
        us.wasm_exceptions = WasmExceptionStyle::Legacy;
        assert_eq!(
            us.sysroot_location().unwrap(),
            PathBuf::from("/xxx/sysroot-ehpic")
        );

        us.pic = true;
        us.wasm_exceptions = WasmExceptionStyle::Off;
        assert!(us.sysroot_location().is_err());

        us.sysroot_location = Some(PathBuf::from("/yyy"));
        assert_eq!(us.sysroot_location().unwrap(), PathBuf::from("/yyy"));

        // Hopefully, you don't have a /yyy folder on your system...
        assert!(us.ensure_sysroot_location().is_err());
    }

    #[test]
    fn test_output_path_without_shell_script() {
        let us = UserSettings {
            generate_shell_script: false,
            ..Default::default()
        };
        let state = State {
            user_settings: us,
            build_settings: BuildSettings {
                opt_level: OptLevel::O0,
                debug_level: DebugLevel::G0,
                use_wasm_opt: true,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                linker_args: Vec::new(),
                compiler_inputs: Vec::new(),
                linker_inputs: Vec::new(),
                output: Some(PathBuf::from("test_output")),
            },
            cxx: false,
            temp_dir: PathBuf::from("/tmp"),
        };

        let result = output_path(&state).unwrap();
        assert_eq!(result, PathBuf::from("test_output"));
    }

    #[test]
    fn test_output_path_with_shell_script_no_wasm_extension() {
        let us = UserSettings {
            generate_shell_script: true,
            ..Default::default()
        };
        let state = State {
            user_settings: us,
            build_settings: BuildSettings {
                opt_level: OptLevel::O0,
                debug_level: DebugLevel::G0,
                use_wasm_opt: true,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                linker_args: Vec::new(),
                compiler_inputs: Vec::new(),
                linker_inputs: Vec::new(),
                output: Some(PathBuf::from("myprogram")),
            },
            cxx: false,
            temp_dir: PathBuf::from("/tmp"),
        };

        let result = output_path(&state).unwrap();
        assert_eq!(result, PathBuf::from("myprogram.wasm"));
    }

    #[test]
    fn test_output_path_with_shell_script_existing_wasm_extension() {
        let us = UserSettings {
            generate_shell_script: true,
            ..Default::default()
        };
        let state = State {
            user_settings: us,
            build_settings: BuildSettings {
                opt_level: OptLevel::O0,
                debug_level: DebugLevel::G0,
                use_wasm_opt: true,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                linker_args: Vec::new(),
                compiler_inputs: Vec::new(),
                linker_inputs: Vec::new(),
                output: Some(PathBuf::from("myprogram.wasm")),
            },
            cxx: false,
            temp_dir: PathBuf::from("/tmp"),
        };

        let result = output_path(&state).unwrap();
        assert_eq!(result, PathBuf::from("myprogram.wasm"));
    }

    #[test]
    fn test_output_path_with_shell_script_default_output() {
        let mut us = UserSettings {
            generate_shell_script: true,
            ..Default::default()
        };
        us.module_kind = Some(ModuleKind::StaticMain);
        let state = State {
            user_settings: us,
            build_settings: BuildSettings {
                opt_level: OptLevel::O0,
                debug_level: DebugLevel::G0,
                use_wasm_opt: true,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                linker_args: Vec::new(),
                compiler_inputs: Vec::new(),
                linker_inputs: Vec::new(),
                output: None,
            },
            cxx: false,
            temp_dir: PathBuf::from("/tmp"),
        };

        let result = output_path(&state).unwrap();
        assert_eq!(result, PathBuf::from("a.out.wasm"));
    }

    #[test]
    fn test_shell_script_path_simple() {
        let us = UserSettings {
            generate_shell_script: true,
            ..Default::default()
        };
        let state = State {
            user_settings: us,
            build_settings: BuildSettings {
                opt_level: OptLevel::O0,
                debug_level: DebugLevel::G0,
                use_wasm_opt: true,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                linker_args: Vec::new(),
                compiler_inputs: Vec::new(),
                linker_inputs: Vec::new(),
                output: Some(PathBuf::from("myprogram")),
            },
            cxx: false,
            temp_dir: PathBuf::from("/tmp"),
        };

        let (script_path, output_file_name) = shell_script_path(&state).unwrap();
        assert_eq!(script_path, PathBuf::from("myprogram"));
        assert_eq!(output_file_name, OsString::from("myprogram.wasm"));
    }

    #[test]
    fn test_shell_script_path_with_directory() {
        let us = UserSettings {
            generate_shell_script: true,
            ..Default::default()
        };
        let state = State {
            user_settings: us,
            build_settings: BuildSettings {
                opt_level: OptLevel::O0,
                debug_level: DebugLevel::G0,
                use_wasm_opt: true,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                linker_args: Vec::new(),
                compiler_inputs: Vec::new(),
                linker_inputs: Vec::new(),
                output: Some(PathBuf::from("/path/to/output")),
            },
            cxx: false,
            temp_dir: PathBuf::from("/tmp"),
        };

        let (script_path, output_file_name) = shell_script_path(&state).unwrap();
        assert_eq!(script_path, PathBuf::from("/path/to/output"));
        assert_eq!(output_file_name, OsString::from("output.wasm"));
    }

    #[test]
    fn test_shell_script_path_with_wasm_extension() {
        let us = UserSettings {
            generate_shell_script: true,
            ..Default::default()
        };
        let state = State {
            user_settings: us,
            build_settings: BuildSettings {
                opt_level: OptLevel::O0,
                debug_level: DebugLevel::G0,
                use_wasm_opt: true,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                linker_args: Vec::new(),
                compiler_inputs: Vec::new(),
                linker_inputs: Vec::new(),
                output: Some(PathBuf::from("myprogram.wasm")),
            },
            cxx: false,
            temp_dir: PathBuf::from("/tmp"),
        };

        let (script_path, output_file_name) = shell_script_path(&state).unwrap();
        assert_eq!(script_path, PathBuf::from("myprogram"));
        assert_eq!(output_file_name, OsString::from("myprogram.wasm"));
    }

    #[test]
    fn test_generate_shell_script_default_args() {
        let temp_dir = TempDir::new().unwrap();
        let output_path = temp_dir.path().join("testprog");

        let us = UserSettings {
            generate_shell_script: true,
            ..Default::default()
        };
        let state = State {
            user_settings: us,
            build_settings: BuildSettings {
                opt_level: OptLevel::O0,
                debug_level: DebugLevel::G0,
                use_wasm_opt: true,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                linker_args: Vec::new(),
                compiler_inputs: Vec::new(),
                linker_inputs: Vec::new(),
                output: Some(output_path.clone()),
            },
            cxx: false,
            temp_dir: PathBuf::from("/tmp"),
        };

        generate_shell_script(&state).unwrap();

        // Verify the script file was created
        assert!(output_path.exists());

        // Read and verify the script contents
        let script_content = fs::read_to_string(&output_path).unwrap();
        assert!(script_content.starts_with("#! /bin/sh"));
        assert!(
            script_content
                .contains("SCRIPT_DIR=$(cd -- \"$(dirname -- \"$(realpath \"$0\" )\" )\" && pwd)")
        );
        assert!(script_content.contains("wasmer run"));
        assert!(script_content.contains("--forward-host-env"));
        assert!(script_content.contains("--net"));
        assert!(script_content.contains("--dir"));
        assert!(script_content.contains("--cwd"));
        assert!(script_content.contains("\"$SCRIPT_DIR/testprog.wasm\" -- \"$@\""));

        #[cfg(unix)]
        {
            // Verify the script is executable
            let metadata = fs::metadata(&output_path).unwrap();
            let permissions = metadata.permissions();
            assert!(
                permissions.mode() & 0o111 != 0,
                "Script should be executable"
            );
        }
    }

    #[test]
    fn test_generate_shell_script_custom_args() {
        let temp_dir = TempDir::new().unwrap();
        let output_path = temp_dir.path().join("testprog");

        let us = UserSettings {
            generate_shell_script: true,
            shell_script_wasmer_args: vec![
                "--custom-arg1".to_string(),
                "--custom-arg2".to_string(),
            ],
            ..Default::default()
        };
        let state = State {
            user_settings: us,
            build_settings: BuildSettings {
                opt_level: OptLevel::O0,
                debug_level: DebugLevel::G0,
                use_wasm_opt: true,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                linker_args: Vec::new(),
                compiler_inputs: Vec::new(),
                linker_inputs: Vec::new(),
                output: Some(output_path.clone()),
            },
            cxx: false,
            temp_dir: PathBuf::from("/tmp"),
        };

        generate_shell_script(&state).unwrap();

        // Verify the script file was created
        assert!(output_path.exists());

        // Read and verify the script contents
        let script_content = fs::read_to_string(&output_path).unwrap();
        assert!(script_content.starts_with("#! /bin/sh"));
        assert!(script_content.contains("wasmer run"));
        assert!(script_content.contains("--custom-arg1"));
        assert!(script_content.contains("--custom-arg2"));
        assert!(script_content.contains("\"$SCRIPT_DIR/testprog.wasm\" -- \"$@\""));

        // Should NOT contain default args
        assert!(!script_content.contains("--forward-host-env"));
        assert!(!script_content.contains("--net"));
    }

    #[test]
    fn test_run_command_success_and_failure() {
        // assume 'true' and 'false' are available on PATH
        run_command(Command::new("true")).unwrap();
        let err = run_command(Command::new("false")).unwrap_err();
        let msg = format!("{:?}", err);
        assert!(msg.contains("Command failed"));
    }

    #[test]
    fn test_should_discard_linker_flag() {
        let mut us = UserSettings {
            discard_unsupported_flags: true,
            ..Default::default()
        };
        let prepared = prepare_linker_args(
            vec![
                "--end-group".to_string(),
                "--start-group".to_string(),
                "--as-needed".to_string(),
                "--no-as-needed".to_string(),
                "-lmylib".to_string(),
                "--allow-shlib-undefined".to_string(),
                "--enable-new-dtags".to_string(),
                "--stats".to_string(),
                "--no-stats".to_string(),
                "--version-script=/path/to/script".to_string(),
                "--version-script".to_string(),
                "/path/to/script".to_string(),
                "--version-script,/path/to/script".to_string(),
                "--version-script=foo.txt".to_string(),
                "--export-dynamic".to_string(),
                "-o".to_string(),
                "--version-script=foo.txt".to_string(),
                "--end-group".to_string(),
            ],
            &mut us,
        )
        .unwrap();

        assert_eq!(
            prepared.linker_args,
            vec![
                "-lmylib".to_string(),
                "--version-script,/path/to/script".to_string(),
                "--export-dynamic".to_string(),
            ]
        );
        assert_eq!(
            prepared.output,
            Some(PathBuf::from("--version-script=foo.txt"))
        );
    }
}
