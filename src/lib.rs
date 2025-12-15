#![cfg_attr(target_vendor = "wasmer", allow(unexpected_cfgs))]

use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
};

use anyhow::{bail, Context, Result};

use crate::{compiler::ModuleKind, download::TagSpec};

mod compiler;
pub mod download;

#[derive(Debug, Clone, PartialEq, Eq)]
enum LlvmLocation {
    UserProvided(PathBuf),
    DefaultPath(PathBuf),
}

impl LlvmLocation {
    pub fn get_tool_path(&self, tool: &str) -> PathBuf {
        match self {
            // Never override a user-provided path...
            Self::UserProvided(path) => path.join("bin").join(tool),

            // ... but a default path with fallbacks is generally acceptable.
            Self::DefaultPath(path) => {
                if path.join("bin").exists() {
                    path.join("bin").join(tool)
                } else {
                    // Default to running LLVM 21 binaries if the custom toolchain is not
                    // installed.
                    tracing::warn!(
                        default_path = ?path.display(),
                        "No LLVM location specified and no LLVM installation found in \
                        default path. Using system LLVM version 21. Output may be broken.\
                        Use `wasixcc --download-llvm` to download a compatible version."
                    );
                    let tool_path = format!("{}-{}", tool, 21);
                    PathBuf::from(tool_path)
                }
            }
        }
    }
}

#[cfg(test)]
impl Default for LlvmLocation {
    fn default() -> Self {
        LlvmLocation::DefaultPath(PathBuf::new())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BinaryenLocation {
    UserProvided(PathBuf),
    DefaultPath(PathBuf),
}

impl BinaryenLocation {
    pub fn get_tool_path(&self, tool: &str) -> PathBuf {
        match self {
            // Never override a user-provided path...
            Self::UserProvided(path) => path.join("bin").join(tool),

            // ... but a default path with fallbacks is generally acceptable.
            Self::DefaultPath(path) => {
                if path.join("bin").exists() {
                    path.join("bin").join(tool)
                } else {
                    // Default to running system binaryen if the custom toolchain is not
                    // installed.
                    tracing::warn!(
                        default_path = ?path.display(),
                        "No binaryen location specified and no binaryen installation found in \
                        default path. Using system binaryen. Output may be broken.\
                        Use `wasixcc --download-binaryen` to download a compatible version."
                    );
                    PathBuf::from(tool)
                }
            }
        }
    }
    pub fn get_bin_path(&self) -> Option<PathBuf> {
        match self {
            Self::UserProvided(path) => Some(path.join("bin")),
            Self::DefaultPath(path) => {
                if path.join("bin").exists() {
                    Some(path.join("bin"))
                } else {
                    // Default to running system binaryen if the custom toolchain is not
                    // installed.
                    None
                }
            }
        }
    }
}

#[cfg(test)]
impl Default for BinaryenLocation {
    fn default() -> Self {
        BinaryenLocation::DefaultPath(PathBuf::new())
    }
}

/// Settings provided by user through env vars or -s flags. Some can be overridden by
/// compiler flags; e.g. `-fno-wasm-exceptions` takes priority over `-sWASM_EXCEPTIONS=1`.
#[derive(Debug)]
#[cfg_attr(test, derive(Default))]
struct UserSettings {
    sysroot_location: Option<PathBuf>,          // key name: SYSROOT
    sysroot_prefix: PathBuf,                    // key name: SYSROOT_PREFIX
    llvm_location: LlvmLocation,                // key name: LLVM_LOCATION
    binaryen_location: BinaryenLocation,        // key name: BINARYEN_LOCATION
    extra_compiler_flags: Vec<String>,          // key name: COMPILER_FLAGS
    extra_compiler_post_flags: Vec<String>,     // key name: COMPILER_POST_FLAGS
    extra_compiler_flags_c: Vec<String>,        // key name: COMPILER_FLAGS_C
    extra_compiler_post_flags_c: Vec<String>,   // key name: COMPILER_POST_FLAGS_C
    extra_compiler_flags_cxx: Vec<String>,      // key name: COMPILER_FLAGS_CXX
    extra_compiler_post_flags_cxx: Vec<String>, // key name: COMPILER_POST_FLAGS_CXX
    extra_linker_flags: Vec<String>,            // key name: LINKER_FLAGS
    include_cpp_symbols: bool,                  // key name: INCLUDE_CPP_SYMBOLS
    run_wasm_opt: Option<bool>,                 // key name: RUN_WASM_OPT
    wasm_opt_flags: Vec<String>,                // key name: WASM_OPT_FLAGS
    wasm_opt_suppress_default: bool,            // key name: WASM_OPT_SUPPRESS_DEFAULT
    wasm_opt_preserve_unoptimized: bool,        // key name: WASM_OPT_PRESERVE_UNOPTIMIZED
    module_kind: Option<ModuleKind>,            // key name: MODULE_KIND
    wasm_exceptions: bool,                      // key name: WASM_EXCEPTIONS
    pic: bool,                                  // key name: PIC
    link_symbolic: bool,                        // key name: LINK_SYMBOLIC
}

impl UserSettings {
    pub fn sysroot_location(&self) -> Result<PathBuf> {
        if let Some(sysroot) = self.sysroot_location.as_deref() {
            Ok(sysroot.to_owned())
        } else {
            match (self.wasm_exceptions, self.pic) {
                (true, true) => Ok(self.sysroot_prefix.join("sysroot-ehpic")),
                (true, false) => Ok(self.sysroot_prefix.join("sysroot-eh")),
                (false, true) => {
                    bail!("PIC without wasm exceptions is not a valid build configuration")
                }
                (false, false) => Ok(self.sysroot_prefix.join("sysroot")),
            }
        }
    }

    pub fn ensure_sysroot_location(&self) -> Result<PathBuf> {
        let sysroot = self.sysroot_location()?;
        if !sysroot.is_dir() {
            bail!("sysroot does not exist: {}", sysroot.display());
        }
        Ok(sysroot)
    }

    pub fn module_kind(&self) -> ModuleKind {
        match (self.module_kind, self.pic) {
            (Some(kind), _) => kind,
            (None, true) => ModuleKind::DynamicMain,
            (None, false) => ModuleKind::StaticMain,
        }
    }
}

fn get_args_and_user_settings() -> Result<(Vec<String>, UserSettings)> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (settings_args, args) = separate_user_settings_args(args);
    let user_settings = gather_user_settings(&settings_args)?;
    Ok((args, user_settings))
}

fn run_command(mut command: Command) -> Result<()> {
    tracing::debug!("Executing build command: {command:?}");

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

pub fn get_sysroot() -> Result<PathBuf> {
    let (_, user_settings) = get_args_and_user_settings()?;
    user_settings.ensure_sysroot_location()
}

pub fn download_sysroot(tag_spec: TagSpec) -> Result<()> {
    tracing::info!("Downloading sysroot: {:?}", tag_spec);

    let (_, user_settings) = get_args_and_user_settings()?;
    download::download_sysroot(tag_spec, &user_settings)
}

pub fn download_llvm(tag_spec: TagSpec) -> Result<()> {
    tracing::info!("Downloading LLVM: {:?}", tag_spec);

    let (_, user_settings) = get_args_and_user_settings()?;
    download::download_llvm(tag_spec, &user_settings)
}

pub fn download_binaryen(tag_spec: TagSpec) -> Result<()> {
    tracing::info!("Downloading binaryen: {:?}", tag_spec);

    let (_, user_settings) = get_args_and_user_settings()?;
    download::download_binaryen(tag_spec, &user_settings)
}

fn separate_user_settings_args(args: Vec<String>) -> (Vec<String>, Vec<String>) {
    let mut seen_dash_dash = false;
    let mut settings_args = Vec::new();
    let mut tool_args = Vec::new();

    for arg in args {
        if arg == "--" {
            seen_dash_dash = true;
        } else if seen_dash_dash {
            tool_args.push(arg);
        } else if arg.starts_with("-s") && arg.contains('=') {
            settings_args.push(arg);
        } else {
            tool_args.push(arg);
        }
    }

    (settings_args, tool_args)
}

fn gather_user_settings(args: &[String]) -> Result<UserSettings> {
    let llvm_location = match try_get_user_setting_value("LLVM_LOCATION", args)? {
        Some(path) => LlvmLocation::UserProvided(PathBuf::from(path)),
        None => LlvmLocation::DefaultPath(
            std::env::home_dir()
                .map(|home| home.join(".wasixcc/llvm"))
                .unwrap_or_else(|| PathBuf::from("/lib/wasixcc/llvm")),
        ),
    };

    let binaryen_location = match try_get_user_setting_value("BINARYEN_LOCATION", args)? {
        Some(path) => BinaryenLocation::UserProvided(PathBuf::from(path)),
        None => BinaryenLocation::DefaultPath(
            std::env::home_dir()
                .map(|home| home.join(".wasixcc/binaryen"))
                .unwrap_or_else(|| PathBuf::from("/lib/wasixcc/binaryen")),
        ),
    };

    let sysroot_location = try_get_user_setting_value("SYSROOT", args)?;

    let sysroot_prefix = try_get_user_setting_value("SYSROOT_PREFIX", args)?
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".wasixcc/sysroot")))
        .unwrap_or_else(|| PathBuf::from("/lib/wasixcc/sysroot"));

    let extra_compiler_flags = match try_get_user_setting_value("COMPILER_FLAGS", args)? {
        Some(flags) => read_string_list_user_setting(&flags),
        None => vec![],
    };

    let extra_compiler_post_flags = match try_get_user_setting_value("COMPILER_POST_FLAGS", args)? {
        Some(flags) => read_string_list_user_setting(&flags),
        None => vec![],
    };

    let extra_compiler_flags_c = match try_get_user_setting_value("COMPILER_FLAGS_C", args)? {
        Some(flags) => read_string_list_user_setting(&flags),
        None => vec![],
    };

    let extra_compiler_post_flags_c =
        match try_get_user_setting_value("COMPILER_POST_FLAGS_C", args)? {
            Some(flags) => read_string_list_user_setting(&flags),
            None => vec![],
        };

    let extra_compiler_flags_cxx = match try_get_user_setting_value("COMPILER_FLAGS_CXX", args)? {
        Some(flags) => read_string_list_user_setting(&flags),
        None => vec![],
    };

    let extra_compiler_post_flags_cxx =
        match try_get_user_setting_value("COMPILER_POST_FLAGS_CXX", args)? {
            Some(flags) => read_string_list_user_setting(&flags),
            None => vec![],
        };

    let extra_linker_flags = match try_get_user_setting_value("LINKER_FLAGS", args)? {
        Some(flags) => read_string_list_user_setting(&flags),
        None => vec![],
    };

    let include_cpp_symbols = match try_get_user_setting_value("INCLUDE_CPP_SYMBOLS", args)? {
        Some(value) => read_bool_user_setting(&value)
            .with_context(|| format!("Invalid value {value} for INCLUDE_CPP_SYMBOLS"))?,
        None => false,
    };

    let wasm_opt_flags = match try_get_user_setting_value("WASM_OPT_FLAGS", args)? {
        Some(flags) => read_string_list_user_setting(&flags),
        None => vec![],
    };

    let run_wasm_opt = match try_get_user_setting_value("RUN_WASM_OPT", args)? {
        Some(value) => Some(
            read_bool_user_setting(&value)
                .with_context(|| format!("Invalid value {value} for RUN_WASM_OPT"))?,
        ),
        None => {
            if wasm_opt_flags.is_empty() {
                None
            } else {
                // Assume user wants to run wasm-opt if flags are provided
                Some(true)
            }
        }
    };

    let wasm_opt_suppress_default =
        match try_get_user_setting_value("WASM_OPT_SUPPRESS_DEFAULT", args)? {
            Some(value) => read_bool_user_setting(&value)
                .with_context(|| format!("Invalid value {value} for WASM_OPT_SUPPRESS_DEFAULT"))?,
            None => false,
        };

    let wasm_opt_preserve_unoptimized =
        match try_get_user_setting_value("WASM_OPT_PRESERVE_UNOPTIMIZED", args)? {
            Some(value) => read_bool_user_setting(&value).with_context(|| {
                format!("Invalid value {value} for WASM_OPT_PRESERVE_UNOPTIMIZED")
            })?,
            None => false,
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

    let link_symbolic = match try_get_user_setting_value("LINK_SYMBOLIC", args)? {
        Some(value) => read_bool_user_setting(&value)
            .with_context(|| format!("Invalid value {value} for LINK_SYMBOLIC"))?,
        None => true,
    };

    Ok(UserSettings {
        sysroot_location: sysroot_location.map(Into::into),
        sysroot_prefix,
        llvm_location,
        binaryen_location,
        extra_compiler_flags,
        extra_compiler_post_flags,
        extra_compiler_flags_c,
        extra_compiler_post_flags_c,
        extra_compiler_flags_cxx,
        extra_compiler_post_flags_cxx,
        extra_linker_flags,
        include_cpp_symbols,
        run_wasm_opt,
        wasm_opt_flags,
        wasm_opt_suppress_default,
        wasm_opt_preserve_unoptimized,
        module_kind,
        wasm_exceptions,
        pic,
        link_symbolic,
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
            "-sRUN_WASM_OPT=1".to_string(),
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
        assert_eq!(settings.run_wasm_opt, Some(true));
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
            llvm_location: LlvmLocation::UserProvided(tmp.path().to_path_buf()),
            ..Default::default()
        };
        run_tool_with_passthrough_args("dummytool", vec!["X".into(), "Y".into()], user_settings)
            .unwrap();
    }
}
