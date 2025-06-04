use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
};

use anyhow::{bail, Context, Result};

use crate::compiler::ModuleKind;

mod compiler;

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
    force_wasm_opt: bool,            // key name: FORCE_WASM_OPT
    wasm_opt_flags: Vec<String>,     // key name: WASM_OPT_FLAGS
    module_kind: Option<ModuleKind>, // key name: MODULE_KIND
    wasm_exceptions: bool,           // key name: WASM_EXCEPTIONS
}

impl UserSettings {
    pub fn module_kind(&self) -> ModuleKind {
        self.module_kind.unwrap_or(ModuleKind::StaticMain)
    }
}

fn get_args_and_user_settings() -> Result<(Vec<String>, UserSettings)> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (settings_args, args) = separate_user_settings_args(args);
    let user_settings = gather_user_settings(&settings_args)?;
    Ok((args, user_settings))
}

fn run_command(mut command: Command) -> Result<()> {
    tracing::info!("Executing build command: {command:?}");

    let status = command
        .status()
        .with_context(|| format!("Failed to run command: {command:?}"))?;
    if !status.success() {
        bail!("Command failed with status: {status}; the command was: {command:?}");
    }

    Ok(())
}

fn run_tool_with_passthrough_args(
    tool: &str,
    args: Vec<String>,
    user_settings: UserSettings,
) -> Result<()> {
    let tool_path = user_settings.llvm_location.get_tool_path(tool);
    let mut command = Command::new(tool_path);
    command.args(args);
    run_command(command)
}

pub fn run_compiler(run_cxx: bool) -> Result<()> {
    tracing::info!("Starting in compiler mode");

    let (args, user_settings) = get_args_and_user_settings()?;
    compiler::run(args, user_settings, run_cxx)
}

pub fn run_linker() -> Result<()> {
    tracing::info!("Starting in linker mode");

    let (args, user_settings) = get_args_and_user_settings()?;
    compiler::link_only(args, user_settings)
}

pub fn run_ar() -> Result<()> {
    tracing::info!("Starting in ar mode");

    let (args, user_settings) = get_args_and_user_settings()?;
    run_tool_with_passthrough_args("llvm-ar", args, user_settings)
}

pub fn run_nm() -> Result<()> {
    tracing::info!("Starting in nm mode");

    let (args, user_settings) = get_args_and_user_settings()?;
    run_tool_with_passthrough_args("llvm-nm", args, user_settings)
}

pub fn run_ranlib() -> Result<()> {
    tracing::info!("Starting in ranlib mode");

    let (args, user_settings) = get_args_and_user_settings()?;
    run_tool_with_passthrough_args("llvm-ranlib", args, user_settings)
}

fn separate_user_settings_args(args: Vec<String>) -> (Vec<String>, Vec<String>) {
    args.into_iter()
        .partition(|arg| arg.starts_with("-s") && arg.contains('='))
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

    let force_wasm_opt = match try_get_user_setting_value("FORCE_WASM_OPT", args)? {
        Some(value) => read_bool_user_setting(&value)
            .with_context(|| format!("Invalid value {value} for FORCE_WASM_OPT"))?,
        None => false,
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

    Ok(UserSettings {
        llvm_location,
        sysroot_location: sysroot_location.into(),
        force_wasm_opt,
        wasm_opt_flags,
        module_kind,
        wasm_exceptions,
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
