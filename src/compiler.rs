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
    "--enable-wide-arithmetic",
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
    openmp: bool,
    relocatable: bool,
}

/// A single user-supplied token destined for the link stage. Flags (`-Wl`,
/// `-Xlinker`, forwarded `-L`/`-l`, ...) and file inputs (`.o`/`.a`/...) share
/// one ordered stream so their original left-to-right order survives: e.g.
/// `--whole-archive foo.a --no-whole-archive` must stay contiguous, otherwise
/// the bracket wraps nothing and `foo.a` is linked lazily outside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkToken {
    Flag(String),
    Input(PathBuf),
}

#[derive(Debug)]
pub(crate) struct PreparedArgs {
    compiler_args: Vec<String>,
    // User link-stage tokens in original order. wasixcc's own injected flags and
    // sysroot libs are added separately in `link_inputs`.
    link_args: Vec<LinkToken>,
    compiler_inputs: Vec<PathBuf>,
    output: Option<PathBuf>,
}

impl PreparedArgs {
    fn has_linker_inputs(&self) -> bool {
        self.link_args
            .iter()
            .any(|token| matches!(token, LinkToken::Input(_)))
    }

    #[cfg(test)]
    fn linker_flags(&self) -> Vec<String> {
        self.link_args
            .iter()
            .filter_map(|token| match token {
                LinkToken::Flag(flag) => Some(flag.clone()),
                LinkToken::Input(_) => None,
            })
            .collect()
    }

    #[cfg(test)]
    fn linker_inputs(&self) -> Vec<PathBuf> {
        self.link_args
            .iter()
            .filter_map(|token| match token {
                LinkToken::Input(input) => Some(input.clone()),
                LinkToken::Flag(_) => None,
            })
            .collect()
    }

    // The ordered user link stream as `link_inputs` emits it to wasm-ld (flags
    // and inputs interleaved), for asserting relative order in tests.
    #[cfg(test)]
    fn link_stream(&self) -> Vec<String> {
        self.link_args
            .iter()
            .map(|token| match token {
                LinkToken::Flag(flag) => flag.clone(),
                LinkToken::Input(input) => input.to_string_lossy().into_owned(),
            })
            .collect()
    }
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

    let relocatable = build_settings.relocatable;

    if relocatable || (args.compiler_inputs.is_empty() && !args.has_linker_inputs()) {
        // If there are no inputs, just pass everything through to clang.
        // This lets us support invocations such as `wasixcc -dumpmachine`.
        //
        // A relocatable link (`-r`) goes the same way: it merges objects into
        // one object, so none of the module setup below applies, as there is
        // no crt, sysroot libs, entry point, or wasm-opt.
        let mut command = Command::new(user_settings.llvm_location.get_tool_path(if run_cxx {
            "clang++"
        } else {
            "clang"
        }));
        let user_set_linker = original_args.iter().any(|a| a.starts_with("-fuse-ld"));
        command.args(original_args);
        command.args([OsStr::new("--target=wasm32-wasi")]);
        if !user_set_linker {
            let mut fuse_ld = OsString::from("-fuse-ld=");
            fuse_ld.push(user_settings.llvm_location.get_tool_path("wasm-ld"));
            command.arg(fuse_ld);
        }
        if relocatable {
            // clang's wasm driver doesn't infer -nostdlib from -r, so it still
            // adds crt1.o and the default libs and the merge fails on the
            // undefined `main` they bring in.
            command.arg("-nostdlib");
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

    if !args.has_linker_inputs() {
        // If there are no inputs, just pass everything through to wasm-ld.
        let mut command = Command::new(user_settings.llvm_location.get_tool_path("wasm-ld"));
        command.args(original_args);
        return run_command(command);
    }

    let build_settings = BuildSettings {
        opt_level: OptLevel::O0,
        debug_level: DebugLevel::G0,
        use_wasm_opt: user_settings.run_wasm_opt.unwrap_or(true),
        openmp: false,
        relocatable: false,
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
            // Compiled objects follow the user link tokens, matching the prior
            // behaviour where they were appended after user inputs.
            state.args.link_args.push(LinkToken::Input(output_path));

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

    let args = build_link_args(state, &sysroot_lib_path, &sysroot_lib_wasm32_path)?;

    let mut command = Command::new(linker_path);
    command.args(args);

    run_command(command)
}

// Assemble the full wasm-ld argument vector (everything after the tool name).
// Kept separate from spawning so the ordering can be unit-tested.
fn build_link_args(
    state: &State,
    sysroot_lib_path: &Path,
    sysroot_lib_wasm32_path: &Path,
) -> Result<Vec<OsString>> {
    let module_kind = state.user_settings.module_kind();
    let no_default_libs = state
        .args
        .link_args
        .iter()
        .any(|token| matches!(token, LinkToken::Flag(flag) if flag == "-nodefaultlibs"));
    let no_start_files = state
        .args
        .link_args
        .iter()
        .any(|token| matches!(token, LinkToken::Flag(flag) if flag == "-nostartfiles"));
    let mut args: Vec<OsString> = Vec::new();

    let mut push = |arg: &str| args.push(OsString::from(arg));

    push("--extra-features=atomics");
    push("--extra-features=bulk-memory");
    push("--extra-features=mutable-globals");
    push("--shared-memory");
    push("--max-memory=4294967296"); // TODO: make configurable
    push("--import-memory");
    push("--export-dynamic");
    push("--export=__wasm_call_ctors");
    // Do not demangle the function names (happens by default)
    push("--no-demangle");

    if state.user_settings.wasm_exceptions.is_enabled() {
        push("-mllvm");
        push("--wasm-enable-eh");
        push("-mllvm");
        push("--wasm-enable-sjlj");

        if state.user_settings.wasm_exceptions.is_legacy() {
            push("-mllvm");
            push("--wasm-use-legacy-eh=true");
        } else {
            push("-mllvm");
            push("--wasm-use-legacy-eh=false");
        }

        // Use native wasm exceptions
        push("-mllvm");
        push("--exception-model=wasm");
    }

    push("--export=__wasm_init_tls");
    push("--export=__wasm_signal");
    push("--export=__tls_size");
    push("--export=__tls_align");
    push("--export=__tls_base");
    push("--export-if-defined=__indirect_function_table"); // needed for reflection and call_dynamic

    if module_kind.is_executable() {
        push("--export-if-defined=__stack_pointer");
        push("--export-if-defined=__heap_base");
        push("--export-if-defined=__data_end");
    }

    if matches!(module_kind, ModuleKind::DynamicMain) {
        push("--whole-archive");
        push("--export-all");
    }

    // Make sysroots libs available to all modules so they can optionally
    // link against them if needed, even when we don't.
    let mut lib_arg = OsString::new();
    lib_arg.push("-L");
    lib_arg.push(sysroot_lib_path);
    args.push(lib_arg);

    let mut lib_arg = OsString::new();
    lib_arg.push("-L");
    lib_arg.push(sysroot_lib_wasm32_path);
    args.push(lib_arg);

    let mut push = |arg: &str| args.push(OsString::from(arg));

    if module_kind.is_executable() && !no_default_libs {
        push("-lwasi-emulated-getpid");
        push("-lwasi-emulated-mman");
        push("-lwasi-emulated-process-clocks");
        push("-lc");
        push("-lresolv");
        push("-lrt");
        push("-lm");
        push("-lpthread");
        push("-lutil");
    }

    if matches!(module_kind, ModuleKind::DynamicMain) {
        push("--no-whole-archive");
    }

    // -fopenmp is a compiler-driver flag and cannot be forwarded to wasm-ld.
    // The caller supplies libomp's search path just like any other -L path.
    if state.build_settings.openmp {
        push("-lomp");
    }

    // The WASIX libomp archive references the C++ runtime. Shared modules do
    // not otherwise receive it, and executables should receive only one copy.
    if state.build_settings.openmp
        || (module_kind.is_executable() && (state.cxx || state.user_settings.include_cpp_symbols))
    {
        push("-lc++");
        push("-lc++abi");
        if state.user_settings.wasm_exceptions.is_enabled() {
            push("-lunwind");
        }
    }

    // Link as much as needed out of libclang_rt.builtins regardless of module kind.
    push("-lclang_rt.builtins-wasm32");

    if module_kind.requires_pic() {
        push("--experimental-pic");
        push("--export-if-defined=__wasm_apply_data_relocs");
        push("--export-if-defined=__wasm_apply_tls_relocs");
    }

    match module_kind {
        ModuleKind::StaticMain => {
            // TODO: make configurable
            push("-z");
            push("stack-size=10");
        }

        ModuleKind::DynamicMain => {
            push("-pie");
            push("-lcommon-tag-stubs");
        }

        ModuleKind::SharedLibrary => {
            push("-shared");
            push("--no-entry");
            push("--unresolved-symbols=import-dynamic");
            if state.user_settings.link_symbolic {
                push("-Bsymbolic");
            }
        }

        ModuleKind::ObjectFile => panic!("Internal error: object files can't be linked"),
    }

    // User link tokens as one ordered stream, so --whole-archive/--no-whole-archive
    // pairs keep their archive bracketed. Placed after the DynamicMain whole-archive
    // block: a trailing user -l for a lib that block already force-included would
    // otherwise re-pull it and produce a duplicate-symbol error. Lazy resolution
    // still binds user objects to the sysroot -lc above.
    for token in &state.args.link_args {
        match token {
            LinkToken::Flag(flag) if flag == "-nodefaultlibs" => {}
            LinkToken::Flag(flag) if flag == "-nostartfiles" => {}
            // -lstdc++/-lsupc++ are the GNU C++ runtime. We only have Clang's
            // libc++. Auto-replace references to the GNU one and pull in
            // Clang's instead, so consumers that hardcode GNU work automatically.
            LinkToken::Flag(flag) if flag == "-lstdc++" || flag == "-lsupc++" => {
                args.push(OsString::from("-lc++"));
                args.push(OsString::from("-lc++abi"));
            }
            LinkToken::Flag(flag) => args.push(OsString::from(flag)),
            LinkToken::Input(input) => args.push(input.clone().into_os_string()),
        }
    }

    if !no_start_files {
        if module_kind.is_executable() {
            args.push(sysroot_lib_wasm32_path.join("crt1.o").into_os_string());
        } else {
            args.push(sysroot_lib_wasm32_path.join("scrt1.o").into_os_string());
        }
    }

    args.push(OsString::from("-o"));
    args.push(output_path(state)?.into_os_string());

    Ok(args)
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
                openmp: false,
                relocatable: false,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                link_args: Vec::new(),
                compiler_inputs: Vec::new(),
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
                openmp: false,
                relocatable: false,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                link_args: Vec::new(),
                compiler_inputs: Vec::new(),
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
                openmp: false,
                relocatable: false,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                link_args: Vec::new(),
                compiler_inputs: Vec::new(),
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
                openmp: false,
                relocatable: false,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                link_args: Vec::new(),
                compiler_inputs: Vec::new(),
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
                openmp: false,
                relocatable: false,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                link_args: Vec::new(),
                compiler_inputs: Vec::new(),
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
                openmp: false,
                relocatable: false,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                link_args: Vec::new(),
                compiler_inputs: Vec::new(),
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
                openmp: false,
                relocatable: false,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                link_args: Vec::new(),
                compiler_inputs: Vec::new(),
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
                openmp: false,
                relocatable: false,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                link_args: Vec::new(),
                compiler_inputs: Vec::new(),
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
                openmp: false,
                relocatable: false,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                link_args: Vec::new(),
                compiler_inputs: Vec::new(),
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

    // Needs host executables on PATH, which a WASIX test module has no way to
    // reach — and a failed spawn there aborts the process instead of returning
    // an error, so this can't be turned into a negative test either.
    #[cfg(not(target_family = "wasm"))]
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
            prepared.linker_flags(),
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

    fn link_args_state(module_kind: ModuleKind, link_args: Vec<LinkToken>) -> State {
        let us = UserSettings {
            module_kind: Some(module_kind),
            ..Default::default()
        };
        State {
            user_settings: us,
            build_settings: BuildSettings {
                opt_level: OptLevel::O0,
                debug_level: DebugLevel::G0,
                use_wasm_opt: false,
                openmp: false,
                relocatable: false,
            },
            args: PreparedArgs {
                compiler_args: Vec::new(),
                link_args,
                compiler_inputs: Vec::new(),
                output: Some(PathBuf::from("out.wasm")),
            },
            cxx: false,
            temp_dir: PathBuf::from("/tmp"),
        }
    }

    fn rendered_link_args(state: &State) -> Vec<String> {
        // Arbitrary sysroot paths; build_link_args does not touch the filesystem.
        build_link_args(
            state,
            Path::new("/sysroot/lib"),
            Path::new("/sysroot/lib/wasm32-wasi"),
        )
        .unwrap()
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
    }

    fn compiler_args_state(args: &[&str], cxx: bool) -> State {
        let mut user_settings = UserSettings::default();
        let (args, build_settings) = prepare_compiler_args(
            args.iter().map(|arg| (*arg).to_owned()),
            &mut user_settings,
            cxx,
        )
        .unwrap();
        State {
            user_settings,
            build_settings,
            args,
            cxx,
            temp_dir: PathBuf::from("/tmp"),
        }
    }

    #[test]
    fn test_dynamic_main_user_lib_emitted_after_whole_archive_block() {
        // Regression: for DynamicMain, wasixcc force-includes the sysroot libs via
        // its own --whole-archive block. A user -l naming the same archive must be
        // emitted AFTER --no-whole-archive, otherwise (with a referencing object
        // ahead of it) it lazily pulls a member the block also force-pulls, and
        // wasm-ld reports a duplicate symbol.
        let state = link_args_state(
            ModuleKind::DynamicMain,
            vec![
                LinkToken::Input(PathBuf::from("main.o")),
                LinkToken::Flag("-lwasi-emulated-process-clocks".to_string()),
            ],
        );
        let args = rendered_link_args(&state);

        let nwa = args
            .iter()
            .position(|a| a == "--no-whole-archive")
            .expect("DynamicMain must close its whole-archive block");
        // wasixcc still injects its own copy inside the block.
        let injected = args
            .iter()
            .position(|a| a == "-lwasi-emulated-process-clocks")
            .unwrap();
        let user = args
            .iter()
            .rposition(|a| a == "-lwasi-emulated-process-clocks")
            .unwrap();
        assert!(injected < nwa, "injected lib should sit inside the block");
        assert!(
            user > nwa,
            "user -l must follow --no-whole-archive, got {args:?}"
        );
    }

    #[test]
    fn test_openmp_links_runtime_for_shared_module() {
        let state = compiler_args_state(&["-fopenmp", "-shared", "ext.o", "-o", "out.wasm"], false);
        assert!(state.build_settings.openmp);
        let args = rendered_link_args(&state);

        for library in ["-lomp", "-lc++", "-lc++abi", "-lunwind"] {
            assert_eq!(
                args.iter().filter(|arg| arg.as_str() == library).count(),
                1,
                "OpenMP should link {library} once, got {args:?}"
            );
        }
    }

    #[test]
    fn test_openmp_cxx_executable_does_not_duplicate_cpp_runtime() {
        let mut state = link_args_state(
            ModuleKind::StaticMain,
            vec![LinkToken::Input(PathBuf::from("main.o"))],
        );
        state.build_settings.openmp = true;
        state.cxx = true;
        let args = rendered_link_args(&state);

        for library in ["-lomp", "-lc++", "-lc++abi", "-lunwind"] {
            assert_eq!(
                args.iter().filter(|arg| arg.as_str() == library).count(),
                1,
                "OpenMP C++ executables should link {library} once, got {args:?}"
            );
        }
    }

    #[test]
    fn test_no_openmp_leaves_runtime_out() {
        let state = link_args_state(
            ModuleKind::SharedLibrary,
            vec![LinkToken::Input(PathBuf::from("ext.o"))],
        );
        let args = rendered_link_args(&state);

        assert!(
            !args.iter().any(|arg| arg == "-lomp"),
            "libomp must not be linked without OpenMP, got {args:?}"
        );
    }

    #[test]
    fn test_user_lstdcxx_is_translated_to_libcxx() {
        // The sysroot has no libstdc++. A build system that links a vendored C++
        // dep itself passes the GNU name, and for a shared module wasixcc adds no
        // C++ runtime of its own, so an untranslated -lstdc++ fails the link.
        let state = link_args_state(
            ModuleKind::SharedLibrary,
            vec![
                LinkToken::Input(PathBuf::from("ext.o")),
                LinkToken::Flag("-lstdc++".to_string()),
            ],
        );
        let args = rendered_link_args(&state);

        assert!(
            !args.iter().any(|a| a == "-lstdc++"),
            "-lstdc++ must not reach wasm-ld, got {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "-lc++") && args.iter().any(|a| a == "-lc++abi"),
            "-lstdc++ should become -lc++ -lc++abi, got {args:?}"
        );
    }

    #[test]
    fn test_nodefaultlibs_suppresses_injected_system_libraries() {
        let state = link_args_state(
            ModuleKind::DynamicMain,
            vec![
                LinkToken::Flag("-nodefaultlibs".to_string()),
                LinkToken::Flag("--whole-archive".to_string()),
                LinkToken::Flag("-lc".to_string()),
                LinkToken::Flag("--no-whole-archive".to_string()),
            ],
        );
        let args = rendered_link_args(&state);

        assert!(!args.iter().any(|arg| arg == "-nodefaultlibs"));
        assert_eq!(
            args.iter().filter(|arg| *arg == "-lc").count(),
            1,
            "only the user-supplied libc should remain: {args:?}"
        );
        assert!(!args.iter().any(|arg| arg == "-lwasi-emulated-getpid"));
    }

    #[test]
    fn test_nostartfiles_suppresses_injected_crt() {
        let state = link_args_state(
            ModuleKind::DynamicMain,
            vec![
                LinkToken::Flag("-nostartfiles".to_string()),
                LinkToken::Input(PathBuf::from("crt1-command.o")),
            ],
        );
        let args = rendered_link_args(&state);

        assert!(!args.iter().any(|arg| arg == "-nostartfiles"));
        assert!(args.iter().any(|arg| arg == "crt1-command.o"));
        assert!(
            !args
                .iter()
                .any(|arg| arg == "/sysroot/lib/wasm32-wasi/crt1.o"),
            "wasixcc must not add its crt when the caller supplied one: {args:?}"
        );
    }

    #[test]
    fn test_user_whole_archive_bracket_contiguous_in_link_args() {
        // A user --whole-archive <archive> --no-whole-archive must reach wasm-ld
        // contiguous so the archive is bracketed. wasixcc injects no whole-archive
        // block for SharedLibrary, so nothing splits the stream.
        let state = link_args_state(
            ModuleKind::SharedLibrary,
            vec![
                LinkToken::Flag("--whole-archive".to_string()),
                LinkToken::Input(PathBuf::from("libtest.a")),
                LinkToken::Flag("--no-whole-archive".to_string()),
            ],
        );
        let args = rendered_link_args(&state);

        let wa = args.iter().position(|a| a == "--whole-archive").unwrap();
        assert_eq!(
            &args[wa..wa + 3],
            &["--whole-archive", "libtest.a", "--no-whole-archive"]
        );
    }
}
