use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
};

use anyhow::{bail, Context, Result};

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
        "-imultilib",
        "-A",
        "-isystem",
        "-iquote",
        "-install_name",
        "-compatibility_version",
        "-mllvm",
        "-current_version",
        "-I",
        "-L",
        "-include-pch",
        "-u",
        "-undefined",
        "-target",
        "-Xlinker",
        "-Xclang",
        "-z",
    ]
    .into()
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModuleKind {
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
enum OptLevel {
    O0,
    O1,
    O2,
    O3,
    O4,
    Os,
    Oz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebugLevel {
    None,
    G0,
    G1,
    G2,
    G3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LlvmLocation {
    FromPath(PathBuf),
    FromSystem(u32), // The u32 is the version suffix, e.g. clang-20
}

impl LlvmLocation {
    pub fn get_tool_path(&self, tool: &str) -> PathBuf {
        match self {
            LlvmLocation::FromPath(path) => path.join(tool),
            LlvmLocation::FromSystem(version_suffix) => {
                let tool_path = format!("{}-{}", tool, version_suffix);
                PathBuf::from(tool_path)
            }
        }
    }
}

/// Settings provided by user through env vars or -s flags. Some can be overridden by
/// compiler flags; e.g. `-fno-wasm-exceptions` takes priority over `-sWASM_EXCEPTIONS=1`.
#[derive(Debug)]
struct UserSettings {
    llvm_location: LlvmLocation, // key name: LLVM_LOCATION
    // TODO: implement automatic detection of sysroot kind, e.g. eh+pic vs eh
    sysroot_location: PathBuf,       // key name: SYSROOT
    wasm_opt_flags: Vec<String>,     // key name: WASM_OPT_FLAGS
    module_kind: Option<ModuleKind>, // key name: MODULE_KIND
    wasm_exceptions: bool,           // key name: WASM_EXCEPTIONS
    include_cpp_std: bool,           // key name: CPPSTD
}

impl UserSettings {
    pub fn module_kind(&self) -> ModuleKind {
        self.module_kind.unwrap_or(ModuleKind::StaticMain)
    }
}

/// Settings derived strictly from compiler flags.
#[derive(Debug)]
struct BuildSettings {
    opt_level: OptLevel,
    debug_level: DebugLevel,
    use_wasm_opt: bool,
}

#[derive(Debug)]
struct PreparedArgs {
    compiler_args: Vec<String>,
    linker_args: Vec<String>,
    compiler_inputs: Vec<PathBuf>,
    linker_inputs: Vec<PathBuf>,
    output: Option<PathBuf>,
}

#[derive(Debug)]
struct State {
    user_settings: UserSettings,
    build_settings: BuildSettings,
    args: PreparedArgs,
    cxx: bool,
    temp_dir: PathBuf,
}

pub fn run_clang(run_cxx: bool) -> Result<()> {
    tracing::info!("Starting in compiler mode");

    let args: Vec<String> = std::env::args().skip(1).collect();
    let (settings_args, args) = separate_user_settings_args(args);
    let mut user_settings = gather_user_settings(&settings_args)?;
    let (args, build_settings) = prepare_args(args, &mut user_settings)?;

    if args.compiler_inputs.is_empty() && args.linker_inputs.is_empty() {
        bail!("No input");
    }

    let temp_dir = tempfile::TempDir::new().context("Failed to create temporary directory")?;

    let mut state = State {
        user_settings,
        build_settings,
        args,
        cxx: run_cxx,
        temp_dir: temp_dir.path().to_owned(),
    };

    compile_inputs(&mut state)?;

    if state.user_settings.module_kind().is_binary() {
        link_inputs(&state)?;
    }

    if state.user_settings.module_kind().is_binary() && state.build_settings.use_wasm_opt {
        run_wasm_opt(&state)?;
    }

    tracing::info!("Done");
    Ok(())
}

fn output_path(state: &State) -> &Path {
    if let Some(output) = &state.args.output {
        output.as_path()
    } else {
        match state.user_settings.module_kind() {
            ModuleKind::StaticMain => Path::new("a.wasm"),
            ModuleKind::DynamicMain => Path::new("a.wasm"),
            ModuleKind::SharedLibrary => Path::new("liba.so"),
            ModuleKind::ObjectFile => Path::new("a.o"),
        }
    }
}

fn run_command(mut command: Command) -> Result<()> {
    tracing::info!("Executing build command: {command:?}");

    let status = command.status().context("Failed to compile inputs")?;
    if !status.success() {
        bail!("Command failed with status: {}", status);
    }

    Ok(())
}

fn compile_inputs(state: &mut State) -> Result<()> {
    let compiler_path = state
        .user_settings
        .llvm_location
        .get_tool_path(if state.cxx { "clang++" } else { "clang" });

    let mut command_args: Vec<&OsStr> = vec![
        OsStr::new("--sysroot"),
        state.user_settings.sysroot_location.as_os_str(),
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
    }

    if state.user_settings.module_kind().requires_pic() {
        command_args.push(OsStr::new("-fPIC"));
        command_args.push(OsStr::new("-ftls-model=global-dynamic"));
        command_args.push(OsStr::new("-fvisibility=default"));
    } else {
        command_args.push(OsStr::new("-ftls-model=local-exec"));
    }

    if state.cxx {
        // C++ exceptions aren't supported in WASIX yet
        command_args.push(OsStr::new("-fno-exceptions"));
    }

    if state.build_settings.debug_level != DebugLevel::None {
        command_args.push(OsStr::new("-g"));
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

        run_command(command)?;
    }

    Ok(())
}

fn link_inputs(state: &State) -> Result<()> {
    let linker_path = state.user_settings.llvm_location.get_tool_path("wasm-ld");

    let sysroot_lib_path = state.user_settings.sysroot_location.join("lib");
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

    if state.user_settings.wasm_exceptions {
        command.args(["-mllvm", "--wasm-enable-sjlj"]);
    }

    let module_kind = state.user_settings.module_kind();

    command.args([
        "--export=__wasm_init_tls",
        "--export=__wasm_signal",
        "--export=__tls_size",
        "--export=__tls_align",
        "--export=__tls_base",
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

    if module_kind.is_executable() {
        let mut lib_arg = OsString::new();
        lib_arg.push("-L");
        lib_arg.push(&sysroot_lib_path);
        command.arg(lib_arg);

        let mut lib_arg = OsString::new();
        lib_arg.push("-L");
        lib_arg.push(&sysroot_lib_wasm32_path);
        command.arg(lib_arg);

        // Hack: we're linking libclang_rt into libc, so no need to link that here
        command.args([
            "-lwasi-emulated-mman",
            "-lc",
            "-lresolv",
            "-lrt",
            "-lm",
            "-lpthread",
            "-lutil",
        ]);

        if state.user_settings.include_cpp_std {
            command.args(["-lc++", "-lc++abi"]);
        }
    }

    if matches!(module_kind, ModuleKind::DynamicMain) {
        command.args(["--no-whole-archive"]);
    }

    if state.user_settings.module_kind().requires_pic() {
        command.args([
            "--experimental-pic",
            "--export-if-defined=__wasm_apply_data_relocs",
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
    command.arg(output_path(state));

    run_command(command)
}

fn run_wasm_opt(state: &State) -> Result<()> {
    let mut command = Command::new("wasm-opt");

    if state.user_settings.wasm_exceptions {
        command.arg("--experimental-new-eh");
    }

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

    command.args(&state.user_settings.wasm_opt_flags);

    if command.get_args().next().is_none() {
        tracing::info!("Skipping wasm-opt as no passes were specified or needed");
        return Ok(());
    }

    match state.build_settings.debug_level {
        DebugLevel::None | DebugLevel::G0 => (),
        DebugLevel::G1 | DebugLevel::G2 | DebugLevel::G3 => {
            command.arg("-g");
        }
    }

    let output_path = output_path(state);
    command.arg(output_path);
    command.arg("-o");
    command.arg(output_path);

    run_command(command)
}

fn separate_user_settings_args(args: Vec<String>) -> (Vec<String>, Vec<String>) {
    args.into_iter()
        .partition(|arg| arg.starts_with("-s") && arg.contains('='))
}

fn prepare_args(
    args: Vec<String>,
    user_settings: &mut UserSettings,
) -> Result<(PreparedArgs, BuildSettings)> {
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

    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        if arg.starts_with("-Wl,") {
            result
                .linker_args
                .push(arg.strip_prefix("-Wl,").unwrap().to_owned());
        } else if arg == "-Xlinker" {
            let Some(next_arg) = iter.next() else {
                bail!("Expected argument after -Xlinker");
            };
            result.linker_args.push(next_arg);
        } else if arg == "-z" {
            let Some(next_arg) = iter.next() else {
                bail!("Expected argument after -z");
            };
            result.linker_args.push("-z".to_owned());
            result.linker_args.push(next_arg);
        } else if arg == "-o" {
            let Some(next_arg) = iter.next() else {
                bail!("Expected argument after -o");
            };
            let output = PathBuf::from(next_arg);
            if user_settings.module_kind.is_none() {
                if let Some(module_kind) = output.extension().and_then(deduce_module_kind) {
                    user_settings.module_kind = Some(module_kind);
                }
            }
            result.output = Some(output);
        } else if arg.starts_with('-') {
            if update_build_settings_from_arg(&arg, &mut build_settings, user_settings)? {
                let has_next_arg = CLANG_FLAGS_WITH_ARGS.contains(&arg[..]);
                result.compiler_args.push(arg);
                if has_next_arg {
                    if let Some(next_arg) = iter.next() {
                        result.compiler_args.push(next_arg);
                    }
                }
            }
        } else {
            // Assume it's an input file
            if arg.ends_with(".o") || arg.ends_with(".a") {
                result.linker_inputs.push(PathBuf::from(arg));
            } else {
                result.compiler_inputs.push(PathBuf::from(arg));
            }
        }
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

    Ok((result, build_settings))
}

// The returned bool indicated whether the argument should be kept in the
// compiler args.
fn update_build_settings_from_arg(
    arg: &str,
    build_settings: &mut BuildSettings,
    user_settings: &mut UserSettings,
) -> Result<bool> {
    if let Some(opt_level) = arg.strip_prefix("-O") {
        build_settings.opt_level = match opt_level {
            "0" => OptLevel::O0,
            "1" => OptLevel::O1,
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
            x => bail!("Invalid argument: -g{x}"),
        };
        Ok(true)
    } else if arg == "-fwasm-exceptions" {
        user_settings.wasm_exceptions = true;
        Ok(false)
    } else if arg == "-fno-wasm-exceptions" {
        user_settings.wasm_exceptions = false;
        Ok(true)
    } else if arg == "--no-wasm-opt" {
        build_settings.use_wasm_opt = false;
        Ok(false)
    } else {
        Ok(true)
    }
}

fn deduce_module_kind(extension: &OsStr) -> Option<ModuleKind> {
    match extension.to_str() {
        Some("o") => Some(ModuleKind::ObjectFile),
        Some("so") => Some(ModuleKind::SharedLibrary),
        _ => None, // Default to static main if no extension matches
    }
}

fn gather_user_settings(args: &[String]) -> Result<UserSettings> {
    let llvm_location = match try_get_user_setting_value("LLVM_LOCATION", args)? {
        Some(path) => LlvmLocation::FromPath(path.into()),
        None => LlvmLocation::FromSystem(20),
    };

    let Some(sysroot_location) = try_get_user_setting_value("SYSROOT", args)? else {
        bail!(
            "wasixcc currently requires a user-provided sysroot to run. \
            Please set it using -sSYSROOT=path or WASIXCC_SYSROOT environment variable."
        );
    };

    let wasm_opt_flags = match try_get_user_setting_value("WASM_OPT_FLAGS", args)? {
        Some(flags) => flags.split(',').map(|f| f.trim().to_owned()).collect(),
        None => vec![],
    };

    let module_kind = match try_get_user_setting_value("MODULE_KIND", args)? {
        Some(kind) => Some(match kind.as_str() {
            "static-main" => ModuleKind::StaticMain,
            "dynamic-main" => ModuleKind::DynamicMain,
            "shared-library" => ModuleKind::SharedLibrary,
            "object-file" => ModuleKind::ObjectFile,
            _ => bail!("Unknown module kind: {}", kind),
        }),
        None => None, // Default to static main
    };

    let wasm_exceptions = match try_get_user_setting_value("WASM_EXCEPTIONS", args)? {
        Some(value) => read_bool_user_setting(&value)
            .with_context(|| format!("Invalid value {value} for WASM_EXCEPTIONS"))?,
        None => false,
    };

    let include_cpp_std = match try_get_user_setting_value("CPPSTD", args)? {
        Some(value) => read_bool_user_setting(&value)
            .with_context(|| format!("Invalid value {value} for CPPSTD"))?,
        None => true,
    };

    Ok(UserSettings {
        llvm_location,
        sysroot_location: sysroot_location.into(),
        wasm_opt_flags,
        module_kind,
        wasm_exceptions,
        include_cpp_std,
    })
}

fn read_bool_user_setting(value: &str) -> Option<bool> {
    match value.to_lowercase().as_str() {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

fn try_get_user_setting_value(name: &str, args: &[String]) -> Result<Option<String>> {
    for arg in args {
        if arg.starts_with(&format!("-s{}=", name)) {
            let value = arg.split('=').nth(1).unwrap();
            return Ok(Some(value.to_owned()));
        }
    }

    let env_name = format!("WASIXCC_{}", name);
    if let Ok(env_value) = std::env::var(&env_name) {
        return Ok(Some(env_value));
    }

    Ok(None)
}
