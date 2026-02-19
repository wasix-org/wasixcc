use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use anyhow::{Context, Result, bail};

use crate::args::UserSettings;

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

static CLANG_FLAGS_TO_FORWARD_TO_WASM_LD: LazyLock<HashSet<&str>> =
    LazyLock::new(|| ["-L", "-l"].into());

// We always specify values for these flags according to the build configuration, so
// they must be discarded even if they're provided externally
static CLANG_FLAGS_TO_DISCARD: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    [
        "-ftls-model",
        "--sysroot",
        "--target",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ExceptionStyle {
    // Generate object files with the standardized exnref EH proposal
    #[default]
    Exnref,
    // Generate object files with the legacy EH proposal
    Legacy,
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

    match (
        state.user_settings.wasm_exceptions,
        state.user_settings.exception_style,
    ) {
        (true, ExceptionStyle::Exnref) => {
            command_args.push(OsStr::new("-fwasm-exceptions"));

            command_args.push(OsStr::new("-mllvm"));
            command_args.push(OsStr::new("--wasm-enable-eh"));

            command_args.push(OsStr::new("-mllvm"));
            command_args.push(OsStr::new("--wasm-enable-sjlj"));

            command_args.push(OsStr::new("-mllvm"));
            command_args.push(OsStr::new("--wasm-use-legacy-eh=false"));
        }
        (true, ExceptionStyle::Legacy) => {
            command_args.push(OsStr::new("-fwasm-exceptions"));

            command_args.push(OsStr::new("-mllvm"));
            command_args.push(OsStr::new("--wasm-enable-sjlj"));

            command_args.push(OsStr::new("-mllvm"));
            command_args.push(OsStr::new("--wasm-use-legacy-eh=true"));

            if state.cxx {
                // Enable C++ exceptions as well
                command_args.push(OsStr::new("-mllvm"));
                command_args.push(OsStr::new("--wasm-enable-eh"));
            }
        }
        (false, _) => {
            command_args.push(OsStr::new("-fno-wasm-exceptions"));
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
    ]);

    command.args(&state.user_settings.extra_linker_flags);

    match (
        state.user_settings.wasm_exceptions,
        state.user_settings.exception_style,
    ) {
        (true, ExceptionStyle::Exnref) => {
            command.args(["-mllvm", "--wasm-enable-eh"]);
            command.args(["-mllvm", "--wasm-enable-sjlj"]);
            // Don't use legacy EH
            command.args(["-mllvm", "--wasm-use-legacy-eh=false"]);
            // Use native wasm exceptions
            command.args(["-mllvm", "--exception-model=wasm"]);
        }
        (true, ExceptionStyle::Legacy) => {
            command.args(["-mllvm", "--wasm-enable-sjlj"]);
            // Use legacy EH
            command.args(["-mllvm", "--wasm-use-legacy-eh=true"]);

            if state.cxx {
                command.args(["-mllvm", "--wasm-enable-eh"]);
            }
        }
        (false, _) => {}
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
        match (
            state.user_settings.wasm_exceptions,
            state.user_settings.exception_style,
        ) {
            (true, ExceptionStyle::Exnref) => {
                // No conversion to exnref needed
            }
            (true, ExceptionStyle::Legacy) => {
                // Convert old eh to new exnref
                command.arg("--emit-exnref");
            }
            (false, _) => {
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

/// Process the clang arguments into PreparedArgs, separating compiler args and linker args, and also extracting build settings from the args.
fn process_compiler_args<'a>(
    args: impl IntoIterator<Item = &'a String>,
    build_settings: &mut BuildSettings,
    user_settings: &mut UserSettings,
) -> Result<PreparedArgs> {
    let mut compiler_args = Vec::new();
    let mut linker_args = Vec::new();
    let mut inputs = Vec::new();
    let mut output = None;

    enum State {
        Normal,
        ArgWithValue(String),
        Terminated,
    }
    let mut state = State::Normal;

    for arg in args {
        state = match state {
            State::Normal => match arg.as_str() {
                "--" => {
                    compiler_args.push("--".to_owned());
                    State::Terminated
                }
                arg if CLANG_FLAGS_TO_FORWARD_TO_WASM_LD
                    .iter()
                    .any(|flag| arg.starts_with(flag)) =>
                {
                    linker_args.push(arg.to_owned());
                    State::Normal
                }
                arg if CLANG_FLAGS_WITH_ARGS.contains(arg) => State::ArgWithValue(arg.to_owned()),
                arg if arg.starts_with("-Wl,") => {
                    for split in arg["-Wl,".len()..].split(',') {
                        linker_args.push(split.to_owned());
                    }
                    State::Normal
                }
                arg if CLANG_FLAGS_TO_DISCARD.iter().any(|flag| {
                    arg.strip_prefix(flag)
                        .is_some_and(|value| value.is_empty() || value.starts_with('='))
                }) =>
                {
                    update_build_settings_from_compiler_arg(arg, build_settings, user_settings);
                    tracing::debug!("Discarding flag '{}'", arg);
                    State::Normal
                }
                arg if arg.starts_with("-") => {
                    update_build_settings_from_compiler_arg(arg, build_settings, user_settings);
                    compiler_args.push(arg.to_owned());
                    State::Normal
                }
                arg => {
                    inputs.push(PathBuf::from(arg));
                    State::Normal
                }
            },
            State::ArgWithValue(flag) => match flag.as_str() {
                "-mllvm" => {
                    update_build_settings_from_llvm_arg(arg, user_settings);
                    compiler_args.push(flag.to_owned());
                    compiler_args.push(arg.to_owned());
                    State::Normal
                }
                "-o" => {
                    update_build_settings_from_output_arg(arg, build_settings, user_settings);
                    output = Some(PathBuf::from(arg));
                    State::Normal
                }
                "-Xlinker" => {
                    linker_args.push(arg.to_owned());
                    State::Normal
                }
                "-z" => {
                    linker_args.push("-z".to_owned());
                    linker_args.push(arg.to_owned());
                    State::Normal
                }
                flag if CLANG_FLAGS_TO_DISCARD.contains(flag) => {
                    tracing::debug!("Discarding value '{}' for flag '{}'", arg, flag);
                    State::Normal
                }
                flag => {
                    compiler_args.push(flag.to_owned());
                    compiler_args.push(arg.to_owned());
                    State::Normal
                }
            },
            State::Terminated => {
                inputs.push(PathBuf::from(arg));
                State::Terminated
            }
        }
    }

    let (linker_inputs, compiler_inputs) = inputs.into_iter().partition(|input| {
        matches!(
            input.extension().and_then(|ext| ext.to_str()),
            Some("o") | Some("a") | Some("so") | Some("dll") | Some("dylib")
        )
    });

    match state {
        State::ArgWithValue(flag) => {
            bail!("Expected argument after {}", flag);
        }
        State::Normal | State::Terminated => Ok(PreparedArgs {
            compiler_args,
            linker_args,
            compiler_inputs,
            linker_inputs,
            output,
        }),
    }
}

/// Process the wasm-ld arguments into PreparedArgs, extracting build settings from the args.
fn process_linker_args<'a>(
    args: impl IntoIterator<Item = &'a String>,
    user_settings: &mut UserSettings,
) -> Result<PreparedArgs> {
    let mut linker_args = Vec::new();
    let mut inputs = Vec::new();
    let mut output = None;

    enum State {
        Normal,
        ArgWithValue(String),
        Terminated,
    }
    let mut state = State::Normal;

    for arg in args {
        state = match state {
            State::Normal => match arg.as_str() {
                "--" => {
                    linker_args.push("--".to_owned());
                    State::Terminated
                }
                arg if WASM_LD_FLAGS_WITH_ARGS.contains(arg) => State::ArgWithValue(arg.to_owned()),
                arg if user_settings.discard_unsupported_flags
                    && WASM_LD_FLAGS_TO_DISCARD.iter().any(|flag| {
                        arg.strip_prefix(flag)
                            .is_some_and(|value| value.is_empty() || value.starts_with('='))
                    }) =>
                {
                    update_build_settings_from_linker_arg(arg, user_settings);
                    tracing::debug!("Discarding flag '{}'", arg);
                    State::Normal
                }
                arg if arg.starts_with("-") => {
                    update_build_settings_from_linker_arg(arg, user_settings);
                    linker_args.push(arg.to_owned());
                    State::Normal
                }
                arg => {
                    inputs.push(PathBuf::from(arg));
                    State::Normal
                }
            },
            State::ArgWithValue(flag) => match flag.as_str() {
                "-mllvm" => {
                    update_build_settings_from_llvm_arg(arg, user_settings);
                    linker_args.push(flag.to_owned());
                    linker_args.push(arg.to_owned());
                    State::Normal
                }
                "-o" => {
                    output = Some(PathBuf::from(arg));
                    State::Normal
                }
                flag if user_settings.discard_unsupported_flags
                    && WASM_LD_FLAGS_TO_DISCARD.contains(flag) =>
                {
                    tracing::debug!("Discarding value '{}' for flag '{}'", arg, flag);
                    State::Normal
                }
                flag => {
                    linker_args.push(flag.to_owned());
                    linker_args.push(arg.to_owned());
                    State::Normal
                }
            },
            State::Terminated => {
                inputs.push(PathBuf::from(arg));
                State::Terminated
            }
        }
    }

    match state {
        State::ArgWithValue(flag) => {
            bail!("Expected argument after {}", flag);
        }
        State::Normal | State::Terminated => Ok(PreparedArgs {
            compiler_args: Vec::new(),
            linker_args,
            compiler_inputs: Vec::new(),
            linker_inputs: inputs,
            output,
        }),
    }
}

// Combine the compiler args with the extra args from user settings
fn add_extra_compiler_flags(
    args: impl IntoIterator<Item = String>,
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
        .chain(args)
        .chain(extra_post_flags)
        .chain(extra_post_flags2)
}

// Combine the compiler args with the extra args from user settings
fn add_extra_linker_flags(
    linker_args: impl IntoIterator<Item = String>,
    user_settings: &mut UserSettings,
) -> impl IntoIterator<Item = String> {
    let extra_flags = std::mem::take(&mut user_settings.extra_linker_flags);
    extra_flags.into_iter().chain(linker_args)
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

// Update the build settings based on a standalone compiler argument
fn update_build_settings_from_compiler_arg(
    compiler_arg: &str,
    build_settings: &mut BuildSettings,
    user_settings: &mut UserSettings,
) {
    match compiler_arg {
        "-O0" => build_settings.opt_level = OptLevel::O0,
        "-O1" | "-O" => build_settings.opt_level = OptLevel::O1,
        "-O2" => build_settings.opt_level = OptLevel::O2,
        "-O3" => build_settings.opt_level = OptLevel::O3,
        "-O4" => build_settings.opt_level = OptLevel::O4,
        "-Os" => build_settings.opt_level = OptLevel::Os,
        "-Oz" => build_settings.opt_level = OptLevel::Oz,
        "-g0" => build_settings.debug_level = DebugLevel::G0,
        "-g1" => build_settings.debug_level = DebugLevel::G1,
        "-g2" | "-g" => build_settings.debug_level = DebugLevel::G2,
        "-g3" => build_settings.debug_level = DebugLevel::G3,
        "-fwasm-exceptions" => user_settings.wasm_exceptions = true,
        "-fno-wasm-exceptions" => user_settings.wasm_exceptions = false,
        "-fPIC" => user_settings.pic = true,
        "-fno-PIC" => user_settings.pic = false,
        "--wasm-opt" => build_settings.use_wasm_opt = true,
        "--no-wasm-opt" => build_settings.use_wasm_opt = false,
        "-c" | "-S" | "-E" => {
            user_settings.module_kind = Some(ModuleKind::ObjectFile);
        }
        "-shared" => {
            if user_settings.module_kind.is_none() {
                user_settings.module_kind = Some(ModuleKind::SharedLibrary);
            }
        }
        "-pie" => {
            if user_settings.module_kind.is_none() {
                user_settings.module_kind = Some(ModuleKind::DynamicMain);
            }
        }
        _ => {}
    }
}

// Update the build settings based on a standalone linker argument
fn update_build_settings_from_linker_arg(compiler_arg: &str, user_settings: &mut UserSettings) {
    match compiler_arg {
        "-shared" => {
            if user_settings.module_kind.is_none() {
                user_settings.module_kind = Some(ModuleKind::SharedLibrary);
            }
        }
        "-pie" => {
            if user_settings.module_kind.is_none() {
                user_settings.module_kind = Some(ModuleKind::DynamicMain);
            }
        }
        _ => {}
    }
}

// Update the build settings based on a standalone llvm argument
fn update_build_settings_from_llvm_arg(arg: &str, user_settings: &mut UserSettings) {
    match arg {
        "--wasm-use-legacy-eh" | "--wasm-use-legacy-eh=true" => {
            user_settings.exception_style = ExceptionStyle::Legacy;
        }
        "--wasm-use-legacy-eh=false" => {
            user_settings.exception_style = ExceptionStyle::Exnref;
        }
        _ => {}
    }
}

fn update_build_settings_from_output_arg(
    _arg: &str,
    _build_settings: &mut BuildSettings,
    _user_settings: &mut UserSettings,
) {
    // Uncommented because I am pretty sure this is wrong. clang should not look at the output filename for determining the output type
    // Especially not in a way that will prevent a -c specified afterwards from working correctly

    // if user_settings.module_kind.is_some() {
    //     return;
    // }

    // let Some(module_kind) = PathBuf::from(arg).extension().and_then(deduce_module_kind) else {
    //     return;
    // };

    // user_settings.module_kind = Some(module_kind);
}

fn prepare_compiler_args(
    args: Vec<String>,
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

    let mut result = process_compiler_args(args.iter(), &mut build_settings, user_settings)?;

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

    // TODO: Add extra linker args somewhere here

    let linker_result = process_linker_args(result.linker_args.iter(), user_settings)?;

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

fn prepare_linker_args(
    args: Vec<String>,
    user_settings: &mut UserSettings,
) -> Result<PreparedArgs> {
    let args = add_extra_linker_flags(args, user_settings);
    let args = resolve_response_files(args)?;

    let result = process_linker_args(args.iter(), user_settings)?;

    if user_settings.module_kind().requires_pic() {
        user_settings.pic = true;
    }

    Ok(result)
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

    // #[test]
    // fn test_update_build_settings_from_arg() {
    //     let mut bs = BuildSettings {
    //         opt_level: OptLevel::O0,
    //         debug_level: DebugLevel::G0,
    //         use_wasm_opt: true,
    //     };
    //     let mut us = UserSettings::default();
    //     assert!(update_build_settings_from_arg("-O3", &mut bs, &mut us).unwrap());
    //     assert_eq!(bs.opt_level, OptLevel::O3);
    //     assert!(update_build_settings_from_arg("-g1", &mut bs, &mut us).unwrap());
    //     assert_eq!(bs.debug_level, DebugLevel::G1);
    //     assert!(!update_build_settings_from_arg("--no-wasm-opt", &mut bs, &mut us).unwrap());
    //     assert!(!update_build_settings_from_arg("-fwasm-exceptions", &mut bs, &mut us).unwrap());
    //     assert_eq!(us.wasm_exceptions, true);
    //     assert!(update_build_settings_from_arg("-fno-wasm-exceptions", &mut bs, &mut us).unwrap());
    //     assert_eq!(us.wasm_exceptions, false);
    //     us = UserSettings::default();
    //     assert!(!update_build_settings_from_arg("-fwasm-exceptions", &mut bs, &mut us).unwrap());
    //     assert_eq!(us.exception_style, ExceptionStyle::Exnref);
    //     assert_eq!(us.wasm_exceptions, true);

    //     assert!(
    //         update_build_settings_from_arg(
    //             "-Wl,-mllvm,--wasm-use-legacy-eh=true",
    //             &mut bs,
    //             &mut us
    //         )
    //         .unwrap()
    //     );
    //     assert_eq!(us.exception_style, ExceptionStyle::Legacy);
    //     // Verify toggling eh kind is still saved after disabling EH in general
    //     assert!(update_build_settings_from_arg("-fno-wasm-exceptions", &mut bs, &mut us).unwrap());
    //     assert!(
    //         update_build_settings_from_arg(
    //             "-Wl,-mllvm,--wasm-use-legacy-eh=false",
    //             &mut bs,
    //             &mut us
    //         )
    //         .unwrap()
    //     );
    //     assert_eq!(us.exception_style, ExceptionStyle::Exnref);
    //     assert!(
    //         update_build_settings_from_arg("-Wl,-mllvm,--wasm-use-legacy-eh", &mut bs, &mut us)
    //             .unwrap()
    //     );
    //     assert_eq!(us.exception_style, ExceptionStyle::Legacy);
    // }

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
        assert_eq!(us.exception_style, ExceptionStyle::Exnref);
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
    fn test_sysroot_prefix() {
        let mut us = UserSettings {
            sysroot_prefix: PathBuf::from("/xxx"),
            ..Default::default()
        };
        assert_eq!(us.wasm_exceptions, true);
        assert_eq!(us.exception_style, ExceptionStyle::Exnref);
        assert_eq!(
            us.sysroot_location().unwrap(),
            PathBuf::from("/xxx/sysroot-exnref-eh")
        );

        us.wasm_exceptions = true;
        us.exception_style = ExceptionStyle::Legacy;
        assert_eq!(
            us.sysroot_location().unwrap(),
            PathBuf::from("/xxx/sysroot-eh")
        );

        us.wasm_exceptions = false;
        us.exception_style = ExceptionStyle::Exnref;
        assert_eq!(
            us.sysroot_location().unwrap(),
            PathBuf::from("/xxx/sysroot")
        );

        us.wasm_exceptions = false;
        us.exception_style = ExceptionStyle::Legacy;
        assert_eq!(
            us.sysroot_location().unwrap(),
            PathBuf::from("/xxx/sysroot")
        );

        us.pic = true;
        us.wasm_exceptions = true;
        us.exception_style = ExceptionStyle::Exnref;
        assert_eq!(
            us.sysroot_location().unwrap(),
            PathBuf::from("/xxx/sysroot-exnref-ehpic")
        );

        us.pic = true;
        us.wasm_exceptions = true;
        us.exception_style = ExceptionStyle::Legacy;
        assert_eq!(
            us.sysroot_location().unwrap(),
            PathBuf::from("/xxx/sysroot-ehpic")
        );

        us.pic = true;
        us.wasm_exceptions = false;
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
