use std::{env, io::Write, path::absolute};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::*;

mod response_file;

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

static CLANG_FLAGS_TO_FORWARD_TO_WASM_LD: LazyLock<HashSet<&str>> =
    LazyLock::new(|| ["-L", "-l"].into());

// We always specify values for these flags according to the build configuration, so
// they must be discarded even if they're provided externally
static CLANG_FLAGS_TO_DISCARD: LazyLock<HashSet<&str>> =
    LazyLock::new(|| ["-ftls-model", "--sysroot", "--target", "-mthread-model"].into());

static WASM_LD_FLAGS_WITH_ARGS: LazyLock<HashSet<&str>> =
    LazyLock::new(|| ["-o", "-mllvm", "-L", "-l", "-m", "-O", "-y", "-z"].into());

static WASM_OPT_ENABLED_FEATURES: &[&str] = &[
    "--enable-threads",
    "--enable-mutable-globals",
    "--enable-bulk-memory",
    "--enable-bulk-memory-opt",
    "--enable-exception-handling",
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

        let binaryen_bin_path = user_settings.binaryen_location.get_bin_path();
        if let Some(binaryen_bin_path) = binaryen_bin_path {
            command.env(
                "PATH",
                format!(
                    "{}:{}",
                    absolute(binaryen_bin_path).unwrap().display(),
                    env::var("PATH").unwrap_or_default()
                ),
            );
        }
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
    let binaryen_bin_path = state.user_settings.binaryen_location.get_bin_path();
    let path_env = if let Some(binaryen_bin_path) = &binaryen_bin_path {
        format!(
            "{}:{}",
            absolute(binaryen_bin_path).unwrap().display(),
            env::var("PATH").unwrap_or_default()
        )
    } else {
        env::var("PATH").unwrap_or_default()
    };

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

    if state.user_settings.wasm_exceptions {
        command_args.push(OsStr::new("-fwasm-exceptions"));
        command_args.push(OsStr::new("-mllvm"));
        command_args.push(OsStr::new("--wasm-enable-sjlj"));
        if state.cxx {
            // Enable C++ exceptions as well
            command_args.push(OsStr::new("-mllvm"));
            command_args.push(OsStr::new("--wasm-enable-eh"));
        }
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
            command.env("PATH", &path_env);

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
        command.env("PATH", &path_env);

        command.args(&command_args);
        command.args(&state.args.compiler_inputs);
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
    ]);

    command.args(&state.user_settings.extra_linker_flags);

    if state.user_settings.wasm_exceptions {
        command.args(["-mllvm", "--wasm-enable-sjlj"]);
        if state.cxx {
            command.args(["-mllvm", "--wasm-enable-eh"]);
        }
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
            if state.user_settings.wasm_exceptions {
                command.arg("-lunwind");
            }
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
        if state.user_settings.wasm_exceptions {
            command.arg("--emit-exnref");
        } else {
            command.arg("--asyncify");
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
            r#"if [ -n "${{0%%/*}}" ]; then
  SCRIPT_PATH=$(command -v "$0")
else
  SCRIPT_PATH=$0
fi
SCRIPT_DIR=$(cd -- "$(dirname -- "$SCRIPT_PATH")" && pwd)"#
        )?;
        write!(file, "wasmer run ")?;
        for arg in wasmer_args {
            write!(file, r#""{}" "#, arg.as_ref())?;
        }
        // TODO: this fails for non-UTF8 paths
        write!(
            file,
            r#""$SCRIPT_DIR/{}" -- $@"#,
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
        std::fs::set_permissions(&output_file_name, perms)
            .context("Failed to set executable permissions for script file")?;
    }

    Ok(())
}

fn prepare_compiler_args(
    args: Vec<String>,
    user_settings: &mut UserSettings,
    run_cxx: bool,
) -> Result<(PreparedArgs, BuildSettings)> {
    fn process_one_arg(
        arg: String,
        next_arg: &mut Option<String>,
        user_settings: &mut UserSettings,
        build_settings: &mut BuildSettings,
        result: &mut PreparedArgs,
    ) -> Result<()> {
        if let Some(arg) = arg.strip_prefix("-Wl,") {
            for split in arg.split(',') {
                result.linker_args.push(split.to_owned());
            }
        } else if arg == "-Xlinker" {
            let Some(next_arg) = next_arg.take() else {
                bail!("Expected argument after -Xlinker");
            };
            result.linker_args.push(next_arg);
        } else if arg == "-z" {
            let Some(next_arg) = next_arg.take() else {
                bail!("Expected argument after -z");
            };
            result.linker_args.push("-z".to_owned());
            result.linker_args.push(next_arg);
        } else if arg == "-o" {
            let Some(next_arg) = next_arg.take() else {
                bail!("Expected argument after -o");
            };
            let output = PathBuf::from(next_arg);
            if user_settings.module_kind.is_none()
                && let Some(module_kind) = output.extension().and_then(deduce_module_kind)
            {
                user_settings.module_kind = Some(module_kind);
            }
            result.output = Some(output);
        } else if arg.starts_with('-') {
            if update_build_settings_from_arg(&arg, build_settings, user_settings)? {
                // Read the value early so it's also discarded if we discard the flag
                let next_arg = if CLANG_FLAGS_WITH_ARGS.contains(arg.as_str()) {
                    next_arg.take()
                } else {
                    None
                };

                if CLANG_FLAGS_TO_DISCARD.iter().any(|flag| {
                    arg.strip_prefix(flag)
                        .is_some_and(|value| value.is_empty() || value.starts_with('='))
                }) {
                    return Ok(());
                }

                let args_list = if CLANG_FLAGS_TO_FORWARD_TO_WASM_LD
                    .iter()
                    .any(|flag| arg.starts_with(flag))
                {
                    &mut result.linker_args
                } else {
                    &mut result.compiler_args
                };

                args_list.push(arg);
                if let Some(next_arg) = next_arg {
                    args_list.push(next_arg);
                }
            }
        } else {
            // Assume it's an input file
            let input = PathBuf::from(&arg);
            match input.extension().and_then(|ext| ext.to_str()) {
                Some("a") | Some("o") | Some("obj") | Some("so") => {
                    result.linker_inputs.push(PathBuf::from(arg));
                }
                _ => {
                    result.compiler_inputs.push(PathBuf::from(arg));
                }
            }
        }

        Ok(())
    }

    let mut result = PreparedArgs {
        compiler_args: Vec::new(),
        linker_args: Vec::new(),
        compiler_inputs: Vec::new(),
        linker_inputs: Vec::new(),
        output: None,
    };
    let mut build_settings = BuildSettings {
        opt_level: OptLevel::O0,
        debug_level: DebugLevel::G0,
        use_wasm_opt: true,
    };

    let mut extra_flags = vec![];
    std::mem::swap(&mut extra_flags, &mut user_settings.extra_compiler_flags);
    let mut extra_flags2 = vec![];
    std::mem::swap(
        &mut extra_flags2,
        if run_cxx {
            &mut user_settings.extra_compiler_flags_cxx
        } else {
            &mut user_settings.extra_compiler_flags_c
        },
    );
    let mut extra_post_flags = vec![];
    std::mem::swap(
        &mut extra_post_flags,
        &mut user_settings.extra_compiler_post_flags,
    );
    let mut extra_post_flags2 = vec![];
    std::mem::swap(
        &mut extra_post_flags2,
        if run_cxx {
            &mut user_settings.extra_compiler_post_flags_cxx
        } else {
            &mut user_settings.extra_compiler_post_flags_c
        },
    );

    extra_flags.extend(extra_flags2);
    extra_flags.extend(args);
    extra_flags.extend(extra_post_flags);
    extra_flags.extend(extra_post_flags2);
    let mut args = response_file::ArgumentsStack::new(extra_flags);
    let mut next_arg = None;
    loop {
        let arg = if let Some(arg) = next_arg.take() {
            arg
        } else if let Some(arg) = args.next()? {
            arg
        } else {
            break;
        };

        next_arg = args.next()?;
        process_one_arg(
            arg,
            &mut next_arg,
            user_settings,
            &mut build_settings,
            &mut result,
        )?;
    }

    if user_settings.module_kind.is_none() {
        for arg in &result.compiler_args {
            if arg == "-shared" {
                user_settings.module_kind = Some(ModuleKind::SharedLibrary);
                break;
            } else if arg == "-c" || arg == "-S" || arg == "-E" {
                user_settings.module_kind = Some(ModuleKind::ObjectFile);
                break;
            }
        }
    }

    if user_settings.module_kind.is_none() {
        for arg in &result.linker_args {
            if arg == "-shared" {
                user_settings.module_kind = Some(ModuleKind::SharedLibrary);
                break;
            } else if arg == "-pie" {
                user_settings.module_kind = Some(ModuleKind::DynamicMain);
                break;
            }
        }
    }

    Ok((result, build_settings))
}

fn prepare_linker_args(
    args: Vec<String>,
    user_settings: &mut UserSettings,
) -> Result<PreparedArgs> {
    fn process_one_arg(
        arg: String,
        next_arg: &mut Option<String>,
        user_settings: &mut UserSettings,
        result: &mut PreparedArgs,
    ) -> Result<()> {
        if arg == "-o" {
            let Some(next_arg) = next_arg.take() else {
                bail!("Expected argument after -o");
            };
            let output = PathBuf::from(next_arg);
            if user_settings.module_kind.is_none()
                && let Some(module_kind) = output.extension().and_then(deduce_module_kind)
            {
                user_settings.module_kind = Some(module_kind);
            }
            result.output = Some(output);
        } else if arg.starts_with('-') {
            let has_next_arg = WASM_LD_FLAGS_WITH_ARGS.contains(&arg[..]);
            result.linker_args.push(arg);
            if has_next_arg && let Some(next_arg) = next_arg.take() {
                result.linker_args.push(next_arg);
            }
        } else {
            // Assume it's an input file
            result.linker_inputs.push(PathBuf::from(arg));
        }

        Ok(())
    }

    let mut result = PreparedArgs {
        compiler_args: Vec::new(),
        linker_args: Vec::new(),
        compiler_inputs: Vec::new(),
        linker_inputs: Vec::new(),
        output: None,
    };

    let mut args = response_file::ArgumentsStack::new(args);
    let mut next_arg = None;
    loop {
        let arg = if let Some(arg) = next_arg.take() {
            arg
        } else if let Some(arg) = args.next()? {
            arg
        } else {
            break;
        };

        next_arg = args.next()?;
        process_one_arg(arg, &mut next_arg, user_settings, &mut result)?;
    }

    if user_settings.module_kind.is_none() {
        for arg in &result.linker_args {
            if arg == "-shared" {
                user_settings.module_kind = Some(ModuleKind::SharedLibrary);
                break;
            } else if arg == "-pie" {
                user_settings.module_kind = Some(ModuleKind::DynamicMain);
                break;
            }
        }
    }

    if user_settings.module_kind().requires_pic() {
        user_settings.pic = true;
    }

    Ok(result)
}

// The returned bool indicated whether the argument should be kept in the
// compiler args.
// TODO: update build settings from UserSettings::extra_compiler_flags as well
fn update_build_settings_from_arg(
    arg: &str,
    build_settings: &mut BuildSettings,
    user_settings: &mut UserSettings,
) -> Result<bool> {
    if let Some(opt_level) = arg.strip_prefix("-O") {
        build_settings.opt_level = match opt_level {
            "0" => OptLevel::O0,
            "" | "1" => OptLevel::O1,
            "2" => OptLevel::O2,
            "3" => OptLevel::O3,
            "4" => OptLevel::O4,
            "s" => OptLevel::Os,
            "z" => OptLevel::Oz,
            x => bail!("Invalid argument: -O{x}"),
        };
        Ok(true)
    } else if let Some(debug_level) = arg.strip_prefix("-g") {
        build_settings.debug_level = match debug_level {
            "" => DebugLevel::G2,
            "0" => DebugLevel::G0,
            "1" => DebugLevel::G1,
            "2" => DebugLevel::G2,
            "3" => DebugLevel::G3,

            // There are other -g options such as `-gdwarf`, we pass those through
            // without touching them.
            _ => return Ok(false),
        };
        Ok(true)
    } else if arg == "-fwasm-exceptions" {
        user_settings.wasm_exceptions = true;
        Ok(false)
    } else if arg == "-fno-wasm-exceptions" {
        user_settings.wasm_exceptions = false;
        Ok(true)
    } else if arg == "-fPIC" {
        user_settings.pic = true;
        Ok(true)
    } else if arg == "-fno-PIC" {
        user_settings.pic = false;
        Ok(true)
    } else if arg == "--wasm-opt" {
        build_settings.use_wasm_opt = true;
        Ok(false)
    } else if arg == "--no-wasm-opt" {
        build_settings.use_wasm_opt = false;
        Ok(false)
    } else {
        Ok(true)
    }
}

fn deduce_module_kind(extension: &OsStr) -> Option<ModuleKind> {
    match extension.to_str() {
        Some("o") | Some("obj") => Some(ModuleKind::ObjectFile),
        Some("so") => Some(ModuleKind::SharedLibrary),
        _ => None, // Default to static main if no extension matches
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UserSettings;
    use std::{ffi::OsStr, path::PathBuf};

    #[test]
    fn test_deduce_module_kind() {
        assert_eq!(
            deduce_module_kind(OsStr::new("o")),
            Some(ModuleKind::ObjectFile)
        );
        assert_eq!(
            deduce_module_kind(OsStr::new("so")),
            Some(ModuleKind::SharedLibrary)
        );
        assert_eq!(deduce_module_kind(OsStr::new("unknown")), None);
    }

    #[test]
    fn test_update_build_settings_from_arg() {
        let mut bs = BuildSettings {
            opt_level: OptLevel::O0,
            debug_level: DebugLevel::G0,
            use_wasm_opt: true,
        };
        let mut us = UserSettings::default();
        assert!(update_build_settings_from_arg("-O3", &mut bs, &mut us).unwrap());
        assert_eq!(bs.opt_level, OptLevel::O3);
        assert!(update_build_settings_from_arg("-g1", &mut bs, &mut us).unwrap());
        assert_eq!(bs.debug_level, DebugLevel::G1);
        assert!(!update_build_settings_from_arg("--no-wasm-opt", &mut bs, &mut us).unwrap());
        assert!(!update_build_settings_from_arg("-fwasm-exceptions", &mut bs, &mut us).unwrap());
        assert!(us.wasm_exceptions);
        assert!(update_build_settings_from_arg("-fno-wasm-exceptions", &mut bs, &mut us).unwrap());
        assert!(!us.wasm_exceptions);
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
        assert!(us.wasm_exceptions);
        assert_eq!(pa.compiler_args, vec!["-O2".to_string(), "-g0".to_string()]);
        assert_eq!(
            pa.linker_args,
            vec![
                "-foo".to_string(),
                "bar".to_string(),
                "baz".to_string(),
                "-z".to_string(),
                "zo".to_string()
            ]
        );
        assert_eq!(pa.output, Some(PathBuf::from("out")));
        assert_eq!(pa.compiler_inputs, vec![PathBuf::from("in.c")]);
        assert_eq!(pa.linker_inputs, vec![PathBuf::from("lib.o")]);
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
    fn test_sysroot_prefix() {
        let mut us = UserSettings {
            sysroot_prefix: PathBuf::from("/xxx"),
            ..Default::default()
        };
        assert_eq!(
            us.sysroot_location().unwrap(),
            PathBuf::from("/xxx/sysroot")
        );

        us.wasm_exceptions = true;
        assert_eq!(
            us.sysroot_location().unwrap(),
            PathBuf::from("/xxx/sysroot-eh")
        );

        us.pic = true;
        assert_eq!(
            us.sysroot_location().unwrap(),
            PathBuf::from("/xxx/sysroot-ehpic")
        );

        us.wasm_exceptions = false;
        assert!(us.sysroot_location().is_err());

        us.sysroot_location = Some(PathBuf::from("/yyy"));
        assert_eq!(us.sysroot_location().unwrap(), PathBuf::from("/yyy"));

        // Hopefully, you don't have a /yyy folder on your system...
        assert!(us.ensure_sysroot_location().is_err());
    }
}
