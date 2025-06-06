use super::*;

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
        "-mthread-model",
        "-current_version",
        "-I",
        "-l",
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

static WASM_LD_FLAGS_WITH_ARGS: LazyLock<HashSet<&str>> =
    LazyLock::new(|| ["-o", "-mllvm", "-L", "-l", "-m", "-O", "-y", "-z"].into());

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
    None,
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

    let (args, build_settings) = prepare_compiler_args(args, &mut user_settings)?;

    tracing::info!("Compiler settings: {user_settings:?}");

    if args.compiler_inputs.is_empty() && args.linker_inputs.is_empty() {
        // If there are no inputs, just pass everything through to clang.
        // This lets us support invocations such as `wasixcc -dumpmachine`.
        let mut command = Command::new(user_settings.llvm_location.get_tool_path(if run_cxx {
            "clang++"
        } else {
            "clang"
        }));
        command.args(original_args);
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

pub(crate) fn link_only(args: Vec<String>, mut user_settings: UserSettings) -> Result<()> {
    let args = prepare_linker_args(args, &mut user_settings)?;

    if !user_settings.module_kind().is_binary() {
        bail!(
            "Only binaries can be linked, current module kind is: {:?}",
            user_settings.module_kind()
        );
    }

    tracing::info!("Linker settings: {user_settings:?}");

    if args.linker_inputs.is_empty() {
        bail!("No input");
    }

    let build_settings = BuildSettings {
        opt_level: OptLevel::O0,
        debug_level: DebugLevel::G0,
        use_wasm_opt: user_settings.force_wasm_opt,
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

fn output_path(state: &State) -> &Path {
    if let Some(output) = &state.args.output {
        output.as_path()
    } else {
        match state.user_settings.module_kind() {
            ModuleKind::StaticMain | ModuleKind::DynamicMain | ModuleKind::SharedLibrary => {
                Path::new("a.out")
            }
            ModuleKind::ObjectFile => Path::new("a.o"),
        }
    }
}

fn compile_inputs(state: &mut State) -> Result<()> {
    let compiler_path = state
        .user_settings
        .llvm_location
        .get_tool_path(if state.cxx { "clang++" } else { "clang" });

    let mut command_args: Vec<&OsStr> = vec![
        OsStr::new("--sysroot"),
        state.user_settings.sysroot_location().as_os_str(),
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

    command_args.extend(
        state
            .user_settings
            .extra_compiler_flags
            .iter()
            .map(OsStr::new),
    );

    if state.user_settings.wasm_exceptions {
        command_args.push(OsStr::new("-fwasm-exceptions"));
    }

    if state.user_settings.module_kind().requires_pic() || state.user_settings.pic {
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
        command.arg("-o").arg(output_path(state));

        run_command(command)?;
    }

    Ok(())
}

fn link_inputs(state: &State) -> Result<()> {
    let linker_path = state.user_settings.llvm_location.get_tool_path("wasm-ld");

    let sysroot_lib_path = state.user_settings.sysroot_location().join("lib");
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

        if state.cxx {
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

fn prepare_compiler_args(
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
        if let Some(arg) = arg.strip_prefix("-Wl,") {
            match arg.split_once(',') {
                Some((x, y)) => {
                    result.linker_args.push(x.to_owned());
                    result.linker_args.push(y.to_owned());
                }
                None => {
                    result.linker_args.push(arg.to_owned());
                }
            }
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
    let mut result = PreparedArgs {
        compiler_args: Vec::new(),
        linker_args: Vec::new(),
        compiler_inputs: Vec::new(),
        linker_inputs: Vec::new(),
        output: None,
    };

    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        if arg == "-o" {
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
            let has_next_arg = WASM_LD_FLAGS_WITH_ARGS.contains(&arg[..]);
            result.linker_args.push(arg);
            if has_next_arg {
                if let Some(next_arg) = iter.next() {
                    result.linker_args.push(next_arg);
                }
            }
        } else {
            // Assume it's an input file
            result.linker_inputs.push(PathBuf::from(arg));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LlvmLocation, UserSettings};
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
            debug_level: DebugLevel::None,
            use_wasm_opt: true,
        };
        let mut us = UserSettings {
            sysroot_location: None,
            llvm_location: LlvmLocation::FromSystem(0),
            extra_compiler_flags: vec![],
            extra_linker_flags: vec![],
            force_wasm_opt: false,
            wasm_opt_flags: vec![],
            module_kind: None,
            wasm_exceptions: false,
            pic: false,
        };
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
        let mut us = UserSettings {
            sysroot_location: None,
            llvm_location: LlvmLocation::FromSystem(0),
            extra_compiler_flags: vec![],
            extra_linker_flags: vec![],
            force_wasm_opt: false,
            wasm_opt_flags: vec![],
            module_kind: None,
            wasm_exceptions: false,
            pic: false,
        };
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
        let (pa, bs) = prepare_compiler_args(args, &mut us).unwrap();
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
        let mut us = UserSettings {
            sysroot_location: None,
            llvm_location: LlvmLocation::FromSystem(0),
            extra_compiler_flags: vec![],
            extra_linker_flags: vec![],
            force_wasm_opt: false,
            wasm_opt_flags: vec![],
            module_kind: None,
            wasm_exceptions: false,
            pic: false,
        };
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
}
