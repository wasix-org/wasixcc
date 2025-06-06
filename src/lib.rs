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
    // TODO: implement automatic detection of sysroot kind, e.g. eh+pic vs eh
    sysroot_location: Option<PathBuf>, // key name: SYSROOT
    llvm_location: LlvmLocation,       // key name: LLVM_LOCATION
    extra_compiler_flags: Vec<String>, // key name: COMPILER_FLAGS
    extra_linker_flags: Vec<String>,   // key name: LINKER_FLAGS
    force_wasm_opt: bool,              // key name: FORCE_WASM_OPT
    wasm_opt_flags: Vec<String>,       // key name: WASM_OPT_FLAGS
    module_kind: Option<ModuleKind>,   // key name: MODULE_KIND
    wasm_exceptions: bool,             // key name: WASM_EXCEPTIONS
    pic: bool,                         // key name: PIC
}

impl UserSettings {
    pub fn sysroot_location(&self) -> &Path {
        self.sysroot_location.as_deref().expect(
            "wasixcc currently requires a user-provided sysroot to run. \
            Please set it using -sSYSROOT=path or WASIXCC_SYSROOT environment variable.",
        )
    }

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

    let sysroot_location = try_get_user_setting_value("SYSROOT", args)?;

    let extra_compiler_flags = match try_get_user_setting_value("COMPILER_FLAGS", args)? {
        Some(flags) => read_string_list_user_setting(&flags),
        None => vec![],
    };

    let extra_linker_flags = match try_get_user_setting_value("LINKER_FLAGS", args)? {
        Some(flags) => read_string_list_user_setting(&flags),
        None => vec![],
    };

    let force_wasm_opt = match try_get_user_setting_value("FORCE_WASM_OPT", args)? {
        Some(value) => read_bool_user_setting(&value)
            .with_context(|| format!("Invalid value {value} for FORCE_WASM_OPT"))?,
        None => false,
    };

    let wasm_opt_flags = match try_get_user_setting_value("WASM_OPT_FLAGS", args)? {
        Some(flags) => read_string_list_user_setting(&flags),
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

    let pic = match try_get_user_setting_value("PIC", args)? {
        Some(value) => read_bool_user_setting(&value)
            .with_context(|| format!("Invalid value {value} for PIC"))?,
        None => false,
    };

    Ok(UserSettings {
        sysroot_location: sysroot_location.map(Into::into),
        llvm_location,
        extra_compiler_flags,
        extra_linker_flags,
        force_wasm_opt,
        wasm_opt_flags,
        module_kind,
        wasm_exceptions,
        pic,
    })
}

fn read_string_list_user_setting(value: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut chars = value.chars();

    let mut push_current = |current: &mut String| {
        let trimmed = current.trim().to_owned();
        if !trimmed.is_empty() {
            result.push(current.trim().to_owned())
        }
        current.clear();
    };

    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some(':') => current.push(':'),
                Some(ch) => {
                    current.push('\\');
                    current.push(ch);
                }
                None => current.push('\\'),
            },

            ':' => push_current(&mut current),

            ch => current.push(ch),
        }
    }

    push_current(&mut current);

    result
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::ModuleKind;
    use std::{env, fs, path::PathBuf, process::Command};
    use tempfile::TempDir;

    #[test]
    fn test_read_string_list_user_setting() {
        let value = "a:b\\:c:d";
        let list = read_string_list_user_setting(value);
        assert_eq!(list, vec!["a", "b:c", "d"]);
    }

    #[test]
    fn test_read_bool_user_setting() {
        assert_eq!(read_bool_user_setting("1"), Some(true));
        assert_eq!(read_bool_user_setting("true"), Some(true));
        assert_eq!(read_bool_user_setting("Yes"), Some(true));
        assert_eq!(read_bool_user_setting("0"), Some(false));
        assert_eq!(read_bool_user_setting("false"), Some(false));
        assert_eq!(read_bool_user_setting("No"), Some(false));
        assert_eq!(read_bool_user_setting("invalid"), None);
    }

    #[test]
    fn test_separate_user_settings_args() {
        let args = vec![
            "-sA=1".to_string(),
            "-c".to_string(),
            "-sB=2".to_string(),
            "file.c".to_string(),
        ];
        let (settings, rest) = separate_user_settings_args(args.clone());
        assert_eq!(settings, vec!["-sA=1".to_string(), "-sB=2".to_string()]);
        assert_eq!(rest, vec!["-c".to_string(), "file.c".to_string()]);
    }

    #[test]
    fn test_try_get_user_setting_value_arg_and_env() {
        let args = vec!["-sFOO=bar".to_string()];
        env::remove_var("WASIXCC_FOO");
        let got = try_get_user_setting_value("FOO", &args).unwrap();
        assert_eq!(got, Some("bar".to_string()));
        // fallback to env
        let args2: Vec<String> = Vec::new();
        env::set_var("WASIXCC_FOO", "baz");
        let got2 = try_get_user_setting_value("FOO", &args2).unwrap();
        assert_eq!(got2, Some("baz".to_string()));
    }

    #[test]
    fn test_gather_user_settings() {
        let args = vec![
            "-sSYSROOT=/sys".to_string(),
            "-sCOMPILER_FLAGS=a:b".to_string(),
            "-sLINKER_FLAGS=x:y".to_string(),
            "-sFORCE_WASM_OPT=1".to_string(),
            "-sWASM_OPT_FLAGS=m:n".to_string(),
            "-sMODULE_KIND=shared-library".to_string(),
            "-sWASM_EXCEPTIONS=yes".to_string(),
            "-sPIC=false".to_string(),
        ];
        env::remove_var("WASIXCC_LINKER_FLAGS");
        let settings = gather_user_settings(&args).unwrap();
        assert_eq!(settings.sysroot_location, Some(PathBuf::from("/sys")));
        assert_eq!(
            settings.extra_compiler_flags,
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(
            settings.extra_linker_flags,
            vec!["x".to_string(), "y".to_string()]
        );
        assert!(settings.force_wasm_opt);
        assert_eq!(
            settings.wasm_opt_flags,
            vec!["m".to_string(), "n".to_string()]
        );
        assert_eq!(settings.module_kind, Some(ModuleKind::SharedLibrary));
        assert!(settings.wasm_exceptions);
        assert!(!settings.pic);
    }

    #[test]
    fn test_run_command_success_and_failure() {
        // assume 'true' and 'false' are available on PATH
        run_command(Command::new("true")).unwrap();
        let err = run_command(Command::new("false")).unwrap_err();
        let msg = format!("{:?}", err);
        assert!(msg.contains("Command failed"));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_tool_with_passthrough_args() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let tool_path = bin.join("dummytool");
        fs::write(&tool_path, "#!/bin/sh\nexit 0").unwrap();
        let mut perm = fs::metadata(&tool_path).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&tool_path, perm).unwrap();
        let user_settings = UserSettings {
            sysroot_location: None,
            llvm_location: LlvmLocation::FromPath(bin.clone()),
            extra_compiler_flags: vec![],
            extra_linker_flags: vec![],
            force_wasm_opt: false,
            wasm_opt_flags: vec![],
            module_kind: None,
            wasm_exceptions: false,
            pic: false,
        };
        run_tool_with_passthrough_args("dummytool", vec!["X".into(), "Y".into()], user_settings)
            .unwrap();
    }
}
