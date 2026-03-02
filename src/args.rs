#![cfg_attr(target_vendor = "wasmer", allow(unexpected_cfgs))]

use std::{collections::HashMap, path::PathBuf};

use anyhow::{Context, Result, bail};

use crate::compiler::{ModuleKind, WasmExceptionStyle};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlvmLocation {
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
                        Use `wasixccenv download-llvm` to download a compatible version."
                    );
                    let tool_path = format!("{}-{}", tool, 21);
                    PathBuf::from(tool_path)
                }
            }
        }
    }
    pub fn get_bin_dir(&self) -> Option<PathBuf> {
        match self {
            // Never override a user-provided path...
            Self::UserProvided(path) => Some(path.join("bin")),

            // ... but a default path with fallbacks is generally acceptable.
            Self::DefaultPath(path) => {
                if path.join("bin").exists() {
                    Some(path.join("bin"))
                } else {
                    None
                }
            }
        }
    }
    pub fn get_resource_dir(&self) -> Result<PathBuf> {
        let clang_executable = self.get_tool_path("clang");
        if !clang_executable.exists() {
            bail!(
                "Clang executable not found at expected path: {}. Cannot determine resource dir.",
                clang_executable.display()
            );
        }
        // Try to find the resource dir by running `clang --print-resource-dir`
        let output = std::process::Command::new(clang_executable)
            .arg("-print-resource-dir")
            .output()?;

        if !output.status.success() {
            bail!("Failed to get resource dir from clang");
        }
        let resource_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok(PathBuf::from(resource_dir));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinaryenLocation {
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
                        Use `wasixccenv download-binaryen` to download a compatible version."
                    );
                    PathBuf::from(tool)
                }
            }
        }
    }
    pub fn get_bin_dir(&self) -> Option<PathBuf> {
        match self {
            // Never override a user-provided path...
            Self::UserProvided(path) => Some(path.join("bin")),

            // ... but a default path with fallbacks is generally acceptable.
            Self::DefaultPath(path) => {
                if path.join("bin").exists() {
                    Some(path.join("bin"))
                } else {
                    None
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibtoolLocation {
    UserProvided(PathBuf),
    DefaultPath(PathBuf),
}

impl LibtoolLocation {
    pub fn get_bin_dir(&self) -> Option<PathBuf> {
        match self {
            // Never override a user-provided path...
            Self::UserProvided(path) => Some(path.join("bin")),

            // ... but a default path with fallbacks is generally acceptable.
            Self::DefaultPath(path) => {
                if path.join("bin").exists() {
                    Some(path.join("bin"))
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
impl Default for LibtoolLocation {
    fn default() -> Self {
        LibtoolLocation::DefaultPath(PathBuf::new())
    }
}

/// Settings provided by user through env vars or -s flags. Some can be overridden by
/// compiler flags; e.g. `-fno-exceptions` takes priority over `-sWASM_EXCEPTIONS=1`.
#[derive(Debug)]
pub struct UserSettings {
    pub sysroot_location: Option<PathBuf>,      // key name: SYSROOT
    pub sysroot_prefix: PathBuf,                // key name: SYSROOT_PREFIX
    pub llvm_location: LlvmLocation,            // key name: LLVM_LOCATION
    pub binaryen_location: BinaryenLocation,    // key name: BINARYEN_LOCATION
    pub libtool_location: LibtoolLocation,      // key name: LIBTOOL_LOCATION
    pub extra_compiler_flags: Vec<String>,      // key name: COMPILER_FLAGS
    pub extra_compiler_post_flags: Vec<String>, // key name: COMPILER_POST_FLAGS
    pub extra_compiler_flags_c: Vec<String>,    // key name: COMPILER_FLAGS_C
    pub extra_compiler_post_flags_c: Vec<String>, // key name: COMPILER_POST_FLAGS_C
    pub extra_compiler_flags_cxx: Vec<String>,  // key name: COMPILER_FLAGS_CXX
    pub extra_compiler_post_flags_cxx: Vec<String>, // key name: COMPILER_POST_FLAGS_CXX
    pub extra_linker_flags: Vec<String>,        // key name: LINKER_FLAGS
    pub include_cpp_symbols: bool,              // key name: INCLUDE_CPP_SYMBOLS
    pub run_wasm_opt: Option<bool>,             // key name: RUN_WASM_OPT
    pub wasm_opt_flags: Vec<String>,            // key name: WASM_OPT_FLAGS
    pub wasm_opt_suppress_default: bool,        // key name: WASM_OPT_SUPPRESS_DEFAULT
    pub wasm_opt_preserve_unoptimized: bool,    // key name: WASM_OPT_PRESERVE_UNOPTIMIZED
    pub module_kind: Option<ModuleKind>,        // key name: MODULE_KIND
    pub wasm_exceptions: WasmExceptionStyle,    // key name: WASM_EXCEPTIONS
    pub pic: bool,                              // key name: PIC
    pub link_symbolic: bool,                    // key name: LINK_SYMBOLIC
    pub generate_shell_script: bool,            // key name: GENERATE_SHELL_SCRIPT
    pub shell_script_wasmer_args: Vec<String>,  // key name: SHELL_SCRIPT_WASMER_ARGS
    pub force_static_dependencies: bool,        // key name: FORCE_STATIC_DEPENDENCIES
    pub discard_unsupported_flags: bool,        // key name: DISCARD_UNSUPPORTED_FLAGS
    pub autoconf_workarounds: bool,             // key name: AUTOCONF_WORKAROUNDS
    pub location: PathBuf,                      // key name: LOCATION
    pub bin_location: PathBuf,                  // key name: BIN_LOCATION
    pub include_usr_dirs: bool,                 // key name: INCLUDE_USR_DIRS
    pub ignore_some_warnings: bool,             // key name: IGNORE_SOME_WARNINGS
}

#[cfg(test)]
impl Default for UserSettings {
    fn default() -> Self {
        gather_user_settings(&[], &Default::default()).unwrap()
    }
}

impl UserSettings {
    pub fn sysroot_location(&self) -> Result<PathBuf> {
        if let Some(sysroot) = self.sysroot_location.as_deref() {
            Ok(sysroot.to_owned())
        } else {
            match (self.wasm_exceptions, self.pic) {
                (WasmExceptionStyle::Legacy, true) => Ok(self.sysroot_prefix.join("sysroot-ehpic")),
                (WasmExceptionStyle::Legacy, false) => Ok(self.sysroot_prefix.join("sysroot-eh")),
                (WasmExceptionStyle::Exnref, true) => {
                    Ok(self.sysroot_prefix.join("sysroot-exnref-ehpic"))
                }
                (WasmExceptionStyle::Exnref, false) => {
                    Ok(self.sysroot_prefix.join("sysroot-exnref-eh"))
                }
                (WasmExceptionStyle::Off, true) => {
                    bail!("PIC without wasm exceptions is not a valid build configuration")
                }
                (WasmExceptionStyle::Off, false) => Ok(self.sysroot_prefix.join("sysroot")),
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

pub fn get_args_and_user_settings() -> Result<(Vec<String>, UserSettings)> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let envs = std::env::vars().collect();
    let (settings_args, args) = separate_user_settings_from_tool_args(args);
    let user_settings = gather_user_settings(&settings_args, &envs)?;
    Ok((args, user_settings))
}

fn separate_user_settings_from_tool_args(args: Vec<String>) -> (Vec<String>, Vec<String>) {
    let mut settings_args = Vec::new();
    let mut tool_args = Vec::new();

    for arg in args {
        if arg
            .strip_prefix("-s")
            .is_some_and(|rest| rest.starts_with(char::is_uppercase))
            && arg.contains('=')
        {
            settings_args.push(arg[2..].to_owned());
        } else {
            tool_args.push(arg);
        }
    }

    (settings_args, tool_args)
}

pub fn gather_user_settings(
    args: &[String],
    envs: &HashMap<String, String>,
) -> Result<UserSettings> {
    // The main .wasixcc directory
    let location = match try_get_user_setting_value("LOCATION", args, envs)? {
        Some(path) => {
            let path = PathBuf::from(path);
            if path.is_relative() {
                tracing::warn!(
                    ?path,
                    "Using relative LOCATION for wasixcc. This is not recommended and may cause \
                    unexpected behavior. Use this only if you know what you're doing."
                );
            }
            path
        }
        None => std::env::home_dir()
            .map(|home| home.join(".wasixcc"))
            .unwrap_or_else(|| PathBuf::from("/lib/wasixcc")),
    };

    let llvm_location = match try_get_user_setting_value("LLVM_LOCATION", args, envs)? {
        Some(path) => LlvmLocation::UserProvided(PathBuf::from(path)),
        None => LlvmLocation::DefaultPath(location.join("llvm")),
    };

    let bin_location = match try_get_user_setting_value("BIN_LOCATION", args, envs)? {
        Some(path) => PathBuf::from(path),
        None => location.join("bin"),
    };

    let libtool_location = match try_get_user_setting_value("LIBTOOL_LOCATION", args, envs)? {
        Some(path) => LibtoolLocation::UserProvided(PathBuf::from(path)),
        None => LibtoolLocation::DefaultPath(location.join("libtool")),
    };

    let binaryen_location = match try_get_user_setting_value("BINARYEN_LOCATION", args, envs)? {
        Some(path) => BinaryenLocation::UserProvided(PathBuf::from(path)),
        None => BinaryenLocation::DefaultPath(location.join("binaryen")),
    };

    let sysroot_location = try_get_user_setting_value("SYSROOT", args, envs)?;

    let sysroot_prefix = try_get_user_setting_value("SYSROOT_PREFIX", args, envs)?
        .map(PathBuf::from)
        .unwrap_or(location.join("sysroot"));

    let extra_compiler_flags = match try_get_user_setting_value("COMPILER_FLAGS", args, envs)? {
        Some(flags) => read_string_list_user_setting(&flags),
        None => vec![],
    };

    let extra_compiler_post_flags =
        match try_get_user_setting_value("COMPILER_POST_FLAGS", args, envs)? {
            Some(flags) => read_string_list_user_setting(&flags),
            None => vec![],
        };

    let extra_compiler_flags_c = match try_get_user_setting_value("COMPILER_FLAGS_C", args, envs)? {
        Some(flags) => read_string_list_user_setting(&flags),
        None => vec![],
    };

    let extra_compiler_post_flags_c =
        match try_get_user_setting_value("COMPILER_POST_FLAGS_C", args, envs)? {
            Some(flags) => read_string_list_user_setting(&flags),
            None => vec![],
        };

    let extra_compiler_flags_cxx =
        match try_get_user_setting_value("COMPILER_FLAGS_CXX", args, envs)? {
            Some(flags) => read_string_list_user_setting(&flags),
            None => vec![],
        };

    let extra_compiler_post_flags_cxx =
        match try_get_user_setting_value("COMPILER_POST_FLAGS_CXX", args, envs)? {
            Some(flags) => read_string_list_user_setting(&flags),
            None => vec![],
        };

    let extra_linker_flags = match try_get_user_setting_value("LINKER_FLAGS", args, envs)? {
        Some(flags) => read_string_list_user_setting(&flags),
        None => vec![],
    };

    let include_cpp_symbols = match try_get_user_setting_value("INCLUDE_CPP_SYMBOLS", args, envs)? {
        Some(value) => read_bool_user_setting(&value)
            .with_context(|| format!("Invalid value {value} for INCLUDE_CPP_SYMBOLS"))?,
        None => false,
    };

    let wasm_opt_flags = match try_get_user_setting_value("WASM_OPT_FLAGS", args, envs)? {
        Some(flags) => read_string_list_user_setting(&flags),
        None => vec![],
    };

    let run_wasm_opt = match try_get_user_setting_value("RUN_WASM_OPT", args, envs)? {
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
        match try_get_user_setting_value("WASM_OPT_SUPPRESS_DEFAULT", args, envs)? {
            Some(value) => read_bool_user_setting(&value)
                .with_context(|| format!("Invalid value {value} for WASM_OPT_SUPPRESS_DEFAULT"))?,
            None => false,
        };

    let wasm_opt_preserve_unoptimized =
        match try_get_user_setting_value("WASM_OPT_PRESERVE_UNOPTIMIZED", args, envs)? {
            Some(value) => read_bool_user_setting(&value).with_context(|| {
                format!("Invalid value {value} for WASM_OPT_PRESERVE_UNOPTIMIZED")
            })?,
            None => false,
        };

    let module_kind = match try_get_user_setting_value("MODULE_KIND", args, envs)? {
        Some(kind) => Some(match kind.as_str() {
            "static-main" => ModuleKind::StaticMain,
            "dynamic-main" => ModuleKind::DynamicMain,
            "shared-library" => ModuleKind::SharedLibrary,
            "object-file" => ModuleKind::ObjectFile,
            _ => bail!("Unknown module kind: {}", kind),
        }),
        None => None, // Default to static main
    };

    let wasm_exceptions =
        match try_get_user_setting_value("WASM_EXCEPTIONS", args, envs)?.as_deref() {
            Some("legacy") => WasmExceptionStyle::Legacy,
            Some("exnref") => WasmExceptionStyle::Exnref,
            Some(value) => {
                if read_bool_user_setting(value)
                    .with_context(|| format!("Invalid value {value} for WASM_EXCEPTIONS"))?
                {
                    WasmExceptionStyle::Exnref
                } else {
                    WasmExceptionStyle::Off
                }
            }
            None => WasmExceptionStyle::Exnref,
        };

    let pic = match try_get_user_setting_value("PIC", args, envs)? {
        Some(value) => read_bool_user_setting(&value)
            .with_context(|| format!("Invalid value {value} for PIC"))?,
        None => false,
    };

    let link_symbolic = match try_get_user_setting_value("LINK_SYMBOLIC", args, envs)? {
        Some(value) => read_bool_user_setting(&value)
            .with_context(|| format!("Invalid value {value} for LINK_SYMBOLIC"))?,
        None => true,
    };

    let generate_shell_script =
        match try_get_user_setting_value("GENERATE_SHELL_SCRIPT", args, envs)? {
            Some(value) => read_bool_user_setting(&value)
                .with_context(|| format!("Invalid value {value} for GENERATE_SHELL_SCRIPT"))?,
            None => false,
        };

    let shell_script_wasmer_args =
        match try_get_user_setting_value("SHELL_SCRIPT_WASMER_ARGS", args, envs)? {
            Some(flags) => read_string_list_user_setting(&flags),
            None => vec![],
        };

    let force_static_dependencies =
        match try_get_user_setting_value("FORCE_STATIC_DEPENDENCIES", args, envs)? {
            Some(value) => read_bool_user_setting(&value)
                .with_context(|| format!("Invalid value {value} for FORCE_STATIC_DEPENDENCIES"))?,
            None => false,
        };

    let discard_unsupported_flags =
        match try_get_user_setting_value("DISCARD_UNSUPPORTED_FLAGS", args, envs)? {
            Some(value) => read_bool_user_setting(&value)
                .with_context(|| format!("Invalid value {value} for DISCARD_UNSUPPORTED_FLAGS"))?,
            None => false,
        };

    let autoconf_workarounds = match try_get_user_setting_value("AUTOCONF_WORKAROUNDS", args, envs)?
    {
        Some(value) => read_bool_user_setting(&value)
            .with_context(|| format!("Invalid value {value} for AUTOCONF_WORKAROUNDS"))?,
        None => false,
    };

    let include_usr_dirs = match try_get_user_setting_value("INCLUDE_USR_DIRS", args, envs)? {
        Some(value) => read_bool_user_setting(&value)
            .with_context(|| format!("Invalid value {value} for INCLUDE_USR_DIRS"))?,
        None => false,
    };

    let ignore_some_warnings = match try_get_user_setting_value("IGNORE_SOME_WARNINGS", args, envs)?
    {
        Some(value) => read_bool_user_setting(&value)
            .with_context(|| format!("Invalid value {value} for IGNORE_SOME_WARNINGS"))?,
        None => false,
    };

    Ok(UserSettings {
        sysroot_location: sysroot_location.map(Into::into),
        sysroot_prefix,
        llvm_location,
        binaryen_location,
        libtool_location,
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
        generate_shell_script,
        shell_script_wasmer_args,
        force_static_dependencies,
        discard_unsupported_flags,
        autoconf_workarounds,
        location,
        bin_location,
        include_usr_dirs,
        ignore_some_warnings,
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

fn try_get_user_setting_value(
    name: &str,
    args: &[String],
    envs: &HashMap<String, String>,
) -> Result<Option<String>> {
    for arg in args {
        if arg.starts_with(&format!("{}=", name)) {
            let value = arg.split('=').nth(1).unwrap();
            return Ok(Some(value.to_owned()));
        }
    }

    let env_name = format!("WASIXCC_{}", name);
    if let Some(env_value) = envs.get(&env_name) {
        return Ok(Some(env_value.clone()));
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::ModuleKind;
    use std::path::PathBuf;

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
        let (settings, rest) = separate_user_settings_from_tool_args(args.clone());
        assert_eq!(settings, vec!["A=1".to_string(), "B=2".to_string()]);
        assert_eq!(rest, vec!["-c".to_string(), "file.c".to_string()]);
    }

    #[test]
    fn test_separate_user_settings_args_does_not_match_compiler_flags() {
        // Flags like -std=c++20 should NOT be treated as user settings
        let args = vec![
            "-std=c++20".to_string(),
            "-sSYSROOT=/path".to_string(),
            "-std=c11".to_string(),
            "-static".to_string(),
            "-stack-size=1000".to_string(),
            "-save-temps".to_string(),
            "file.c".to_string(),
        ];
        let (settings, rest) = separate_user_settings_from_tool_args(args.clone());
        // Only -sSYSROOT should be a settings arg (starts with -s and has uppercase letter)
        assert_eq!(settings, vec!["SYSROOT=/path".to_string()]);
        // All other flags should be passed as tool args
        assert_eq!(
            rest,
            vec![
                "-std=c++20".to_string(),
                "-std=c11".to_string(),
                "-static".to_string(),
                "-stack-size=1000".to_string(),
                "-save-temps".to_string(),
                "file.c".to_string(),
            ]
        );
    }

    #[test]
    fn test_try_get_user_setting_value_arg_and_env() {
        let args = vec!["FOO=bar".to_string()];
        let envs = HashMap::new();
        let got = try_get_user_setting_value("FOO", &args, &envs).unwrap();
        assert_eq!(got, Some("bar".to_string()));
        // fallback to env
        let args2: Vec<String> = Vec::new();
        let mut envs2 = HashMap::new();
        envs2.insert("WASIXCC_FOO".to_string(), "baz".to_string());
        let got2 = try_get_user_setting_value("FOO", &args2, &envs2).unwrap();
        assert_eq!(got2, Some("baz".to_string()));
    }

    #[test]
    fn test_gather_user_settings() {
        let args = vec![
            "SYSROOT=/sys".to_string(),
            "COMPILER_FLAGS=a:b".to_string(),
            "LINKER_FLAGS=x:y".to_string(),
            "RUN_WASM_OPT=1".to_string(),
            "WASM_OPT_FLAGS=m:n".to_string(),
            "MODULE_KIND=shared-library".to_string(),
            "WASM_EXCEPTIONS=yes".to_string(),
            "PIC=false".to_string(),
        ];
        let envs = HashMap::new();
        let settings = gather_user_settings(&args, &envs).unwrap();
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
        assert_eq!(settings.wasm_exceptions, WasmExceptionStyle::Exnref);
        assert!(!settings.pic);
    }
}
