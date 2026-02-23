use crate::{
    args::{UserSettings, gather_user_settings},
    download::{self, TagSpec},
    wasixccenv::shell_env::{install_env_files, setup_shell_rcs},
};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

mod shell_env;

#[cfg(unix)]
const COMMANDS: &[&str] = &["cc", "++", "cc++", "ar", "nm", "ranlib", "ld"];

#[derive(Parser)]
// The config help text assumes an 80-character terminal width, so replicate that for
// clap output as well.
#[command(version = env!("CARGO_PKG_VERSION"), max_term_width = 80)]
struct Args {
    #[command(subcommand)]
    command: WasixccCommand,

    /// User settings in the form KEY=VALUE, see 'help-config' output for details
    #[arg(short = 's')]
    // Needed to let clap parse the user settings passed via -sKEY=VALUE
    user_settings: Vec<String>,
}

#[derive(Subcommand)]
enum WasixccCommand {
    /// Install wasixcc executables (via symlinks to this binary) to the
    /// BIN_LOCATION specified in user settings.
    InstallExecutables {
        #[arg(long, default_value = "false")]
        /// Set to create a symlink to wasixccenv instead of copying it to the target location.
        /// This is useful for development and testing purposes, but for production use
        /// it's recommended to copy the executable rather than symlinking it.
        symlink_wasixccenv: bool,
    },
    /// Download the WASIX sysroot
    DownloadSysroot {
        /// The tag from which to download the sysroot, either 'latest' or a
        /// specific tag starting with 'v'. Defaults to 'latest'.
        tag: Option<TagSpec>,
    },
    /// Download the custom LLVM toolchain (Unix only)
    DownloadLlvm {
        /// The tag from which to download the LLVM toolchain, either 'latest' or a
        /// specific tag starting with 'v'. Defaults to 'latest'.
        tag: Option<TagSpec>,
    },
    /// Download binaryen (Unix only)
    DownloadBinaryen {
        /// The tag from which to download binaryen, either 'latest' or a
        /// specific tag starting with 'v'. Defaults to 'latest'.
        tag: Option<TagSpec>,
    },
    /// Download and install everything
    DownloadAll {
        #[arg(long)]
        /// The tag from which to download the sysroot, either 'latest' or a
        /// specific tag starting with 'v'. Defaults to 'latest'.
        sysroot_tag: Option<TagSpec>,
        #[arg(long)]
        /// The tag from which to download the LLVM toolchain, either 'latest' or a
        /// specific tag starting with 'v'. Defaults to 'latest'.
        llvm_tag: Option<TagSpec>,
        #[arg(long)]
        /// The tag from which to download binaryen, either 'latest' or a
        /// specific tag starting with 'v'. Defaults to 'latest'.
        binaryen_tag: Option<TagSpec>,
    },
    /// Install wasixcc into the home of the current user.
    ///
    /// This will create a .wasixcc directory in the user's home directory and adjust all rc files to add that to the PATH.
    AioInstall {
        #[arg(long)]
        /// The tag from which to download the sysroot, either 'latest' or a
        /// specific tag starting with 'v'. Defaults to 'latest'.
        sysroot_tag: Option<TagSpec>,
        #[arg(long)]
        /// The tag from which to download the LLVM toolchain, either 'latest' or a
        /// specific tag starting with 'v'. Defaults to 'latest'.
        llvm_tag: Option<TagSpec>,
        #[arg(long)]
        /// The tag from which to download binaryen, either 'latest' or a
        /// specific tag starting with 'v'. Defaults to 'latest'.
        binaryen_tag: Option<TagSpec>,
    },
    /// Print the sysroot location according to current configuration
    PrintSysroot,
    /// Print help information about wasixcc configuration options
    HelpConfig,
    /// Install sourceable env files to the wasixcc location
    InstallEnvFiles,
    /// Add sourcing of the env files to shell rc files
    SetupShellRcs {
        #[arg(long)]
        /// The home directory to look for shell rc files in. Defaults to the current user's home directory.
        home: Option<PathBuf>,
    },
}

pub(crate) fn run() -> Result<()> {
    let args = Args::parse();
    let envs = std::env::vars().collect();

    let user_settings = gather_user_settings(&args.user_settings, &envs)?;

    match args.command {
        WasixccCommand::InstallExecutables { symlink_wasixccenv } => {
            let installer_path =
                std::env::current_exe().context("Failed to get the path of wasixccenv")?;
            let installer_path = installer_path
                .canonicalize()
                .context("Failed to get the absolute path of wasixccenv")?;
            install_executables(&user_settings, &installer_path, symlink_wasixccenv)
        }
        WasixccCommand::DownloadSysroot { tag } => {
            download_sysroot(tag.unwrap_or(TagSpec::Latest), &user_settings)
        }
        WasixccCommand::DownloadLlvm { tag } => {
            download_llvm(tag.unwrap_or(TagSpec::Latest), &user_settings)
        }
        WasixccCommand::DownloadBinaryen { tag } => {
            download_binaryen(tag.unwrap_or(TagSpec::Latest), &user_settings)
        }
        WasixccCommand::DownloadAll {
            binaryen_tag,
            llvm_tag,
            sysroot_tag,
        } => {
            download_llvm(llvm_tag.unwrap_or(TagSpec::Latest), &user_settings)?;
            download_sysroot(sysroot_tag.unwrap_or(TagSpec::Latest), &user_settings)?;
            download_binaryen(binaryen_tag.unwrap_or(TagSpec::Latest), &user_settings)?;
            Ok(())
        }
        WasixccCommand::AioInstall {
            binaryen_tag,
            llvm_tag,
            sysroot_tag,
        } => {
            let home =
                std::env::home_dir().context("Failed to get current user's home directory")?;
            let installer_path =
                std::env::current_exe().context("Failed to get the path of wasixccenv")?;
            let installer_path = installer_path
                .canonicalize()
                .context("Failed to get the absolute path of wasixccenv")?;

            download_llvm(llvm_tag.unwrap_or(TagSpec::Latest), &user_settings)?;
            download_sysroot(sysroot_tag.unwrap_or(TagSpec::Latest), &user_settings)?;
            download_binaryen(binaryen_tag.unwrap_or(TagSpec::Latest), &user_settings)?;

            install_executables(&user_settings, &installer_path, false)?;

            install_env_files(&user_settings.location)?;
            setup_shell_rcs(&home)?;

            let wasixcc_location = user_settings.location.to_string_lossy().to_string();
            let env_posix_shell =
                wasixcc_location.replace(&home.to_string_lossy().to_string(), "~") + "/env";
            let env_nu_shell = format!(
                "$\"{}\"",
                wasixcc_location.replace(&home.to_string_lossy().to_string(), "($nu.home-path)")
                    + "/env.nu"
            );

            let message = format!(
                r#"wasixcc installed successfully!

To start using wasixcc, please restart your terminal or
run the appropriate command to source the environment file for your shell:

. {env_posix_shell}
source {env_posix_shell}  # For fish
source {env_nu_shell}  # For nushell"
"#
            );
            println!("{message}");
            Ok(())
        }
        WasixccCommand::PrintSysroot => print_sysroot(&user_settings),
        WasixccCommand::HelpConfig => {
            print_configuration_help();
            Ok(())
        }
        WasixccCommand::InstallEnvFiles => install_env_files(&user_settings.location),
        WasixccCommand::SetupShellRcs { home } => setup_shell_rcs(
            &home
                .or_else(|| std::env::home_dir())
                .context("Failed to get current user's home directory")?,
        ),
    }
}

pub fn download_sysroot(tag_spec: TagSpec, user_settings: &UserSettings) -> Result<()> {
    tracing::info!("Downloading sysroot: {:?}", tag_spec);
    download::download_sysroot(tag_spec, user_settings)
}

pub fn download_llvm(tag_spec: TagSpec, user_settings: &UserSettings) -> Result<()> {
    tracing::info!("Downloading LLVM: {:?}", tag_spec);
    download::download_llvm(tag_spec, user_settings)
}

pub fn download_binaryen(tag_spec: TagSpec, user_settings: &UserSettings) -> Result<()> {
    tracing::info!("Downloading binaryen: {:?}", tag_spec);
    download::download_binaryen(tag_spec, user_settings)
}

// Install the current wasixccenv binary to the specified path
fn install_wasixccenv(bin_dir: &Path, wasixccenv: &Path, symlink: bool) -> Result<PathBuf> {
    let target_path = bin_dir.join("wasixccenv");
    let exe_path = wasixccenv;

    if symlink {
        symlink_executable(&exe_path, &target_path)?;
    } else {
        copy_executable(&exe_path, &target_path)?;
    }

    Ok(target_path)
}

#[cfg_attr(target_vendor = "wasmer", allow(unused_variables))]
fn install_executables(
    user_settings: &UserSettings,
    wasixccenv: &Path,
    symlink_wasixccenv: bool,
) -> Result<()> {
    #[cfg(not(unix))]
    {
        anyhow::bail!("wasixcc only supports installation on unix systems at this time");
    }

    use std::fs;
    let bin_dir = &user_settings.bin_location;

    fs::create_dir_all(&bin_dir)
        .with_context(|| format!("Failed to create directory at {bin_dir:?}"))?;

    let relative_wasixccenv = PathBuf::from("./wasixccenv");

    for command in COMMANDS {
        let target = bin_dir.join(format!("wasix{}", command));

        if fs::metadata(&target).is_ok() {
            fs::remove_file(&target)
                .with_context(|| format!("Failed to remove existing file at {target:?}"))?;
        }

        symlink_executable(&relative_wasixccenv, &target)?;
    }

    // For wasixccenv itself, symlink or copy depending on what was requested.
    install_wasixccenv(&bin_dir, &wasixccenv, symlink_wasixccenv)?;

    Ok(())
}

#[cfg(unix)]
fn symlink_executable(original: &Path, link: &Path) -> Result<()> {
    use std::os::unix::fs as unix_fs;

    unix_fs::symlink(original, link)
        .with_context(|| format!("Failed to create symlink at {link:?}"))?;

    println!("Created command {link:?}");

    Ok(())
}

fn copy_executable(original: &Path, link: &Path) -> Result<()> {
    let canonicalized_original = original
        .canonicalize()
        .context("Failed to canonicalize current executable path")?;
    let canonicalized_link = link.canonicalize().unwrap_or_else(|_| link.to_path_buf());

    if canonicalized_original == canonicalized_link {
        // Don't install if the target path is the same as the current executable path
        tracing::info!("Target path {link:?} is already installed");
        return Ok(());
    }

    if link.exists() {
        std::fs::remove_file(&link)
            .with_context(|| format!("Failed to remove existing wasixccenv at {link:?}"))?;
    }

    let target_dir = link
        .parent()
        .context("Failed to get parent directory of target path")?;
    std::fs::create_dir_all(target_dir)
        .with_context(|| format!("Failed to create directory at {target_dir:?}"))?;

    std::fs::copy(&canonicalized_original, link)
        .with_context(|| format!("Failed to copy executable to {link:?}"))?;

    // Set permissions to 755 to ensure the copied executable is runnable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(link)
            .with_context(|| format!("Failed to get metadata for {link:?}"))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(link, perms)
            .with_context(|| format!("Failed to set permissions for {link:?}"))?;
    }

    println!("Created command {link:?}");

    Ok(())
}

fn print_sysroot(user_settings: &UserSettings) -> Result<()> {
    let sysroot = user_settings.ensure_sysroot_location()?;
    println!("{}", sysroot.display());
    Ok(())
}

fn print_configuration_help() {
    println!(
        r#"wasixcc can be configured using various options to control its behavior
when building WebAssembly modules.

Configuration options can be provided on the command line using the
'-s' flag, or using environment variables prefixed with 'WASIXCC_'.

'-sKEY=VALUE' can be specified when running any of the wasixcc commands
(e.g., 'wasixcc -sSYSROOT=/path/to/sysroot code.c -o code.wasm').

This is also true for wasixccenv, which uses the same mechanism to figure out
where to download the sysroot and LLVM toolchain to, as well as when using
`print-sysroot`. Note that when running wasixccenv, the '-s' flags must be
specified first (e.g., 'wasixccenv -sSYSROOT=... download-sysroot').

The following configuration options are available:
  SYSROOT=<PATH>           Set the sysroot location directly. The sysroot
                           needs to have the same configuration as expected
                           by wasixcc. It's *HIGHLY* recommended to use
                           SYSROOT_PREFIX instead so wasixcc can pick the
                           sysroot with the correct configuration.
  SYSROOT_PREFIX=<PREFIX>  Set the sysroot prefix, which is expected to
                           contain the following 5 subdirectories:
                             - 'sysroot'
                             - 'sysroot-exnref-eh'
                             - 'sysroot-exnref-ehpic'
                             - 'sysroot-eh'
                             - 'sysroot-ehpic'
  LLVM_LOCATION=<PATH>     Set the location of LLVM toolchain which will be
                           invoked without a version suffix. The path must
                           point to the installation directory of the
                           toolchain, NOT the bin directory inside it; tools
                           will be executed from LLVM_LOCATION/bin/tool-name.
  BINARYEN_LOCATION=<PATH> Set the location of the binaryen installation,
                           which will be used to invoke the wasm-opt binary.
  COMPILER_FLAGS=<FLAGS>   Extra flags to pass to the compiler, separated
                           by colons (':')
  COMPILER_POST_FLAGS=<FLAGS>
                           Extra flags to pass to the compiler, separated
                           by colons (':'), passed after the arguments
                           provided on the command line. This is useful for
                           overriding command-line flags, such as for disabling
                           warnings.
  COMPILER_FLAGS_C=<FLAGS> Same as COMPILER_FLAGS, but only for C
                           files. This is useful for passing flags that are
                           not compatible with C++.
  COMPILER_POST_FLAGS_C=<FLAGS>
                           Same as COMPILER_POST_FLAGS, but only for C files.
  COMPILER_FLAGS_CXX=<FLAGS>
                           Same as COMPILER_FLAGS, but only for C++ files.
                           This is useful for passing flags that are not
                           compatible with C.
  COMPILER_POST_FLAGS_CXX=<FLAGS>
                           Same as COMPILER_POST_FLAGS, but only for C++ files.
  LINKER_FLAGS=<FLAGS>     Extra flags to pass to the linker, separated
                           by colons (':')
  INCLUDE_CPP_SYMBOLS=<BOOL>
                           Whether to include C++ symbols when building a
                           dynamic main module from C sources. This is useful
                           when the main is expected to be able to load side
                           modules implemented in C++.
  RUN_WASM_OPT=<BOOL>      Whether to run `wasm-opt` on the output of the
                           compiler. If this setting is left out, wasixcc
                           will look at compiler flags to determine whether
                           to run `wasm-opt`. If no flags are found, default
                           behavior is to run `wasm-opt`.
  WASM_OPT_FLAGS=<FLAGS>   Extra flags to pass to `wasm-opt`, separated by
                           colons (':'). Specifying a non-empty list of
                           extra flags for wasm-opt will imply
                           `RUN_WASM_OPT=yes` unless an explicit value is
                           provided for `RUN_WASM_OPT`.
  WASM_OPT_SUPPRESS_DEFAULT=<BOOL>
                           Whether to suppress the default flags wasixcc
                           passes to wasm-opt. The default flags are:
                           * `-O*` for all modules. The optimization
                                 level is determined by the `-O` flag passed
                                 to the compiler. 
                           * `--emit-exnref` for modules with exception
                                 handling enabled, required for running
                                 the module with engines that only support
                                 the 'new' exnref proposal (e.g. the LLVM
                                 backend in Wasmer)
                           * `--asyncify` for modules without exception
                                 handling enabled, required for forks and
                                 setjmp/longjmp to work
  WASM_OPT_PRESERVE_UNOPTIMIZED=<BOOL>
                           Whether to preserve a copy of the unoptimized
                           artifact before running wasm-opt. If the wasm-opt
                           invocation fails, the unoptimized artifact will be
                           preserved at a temporary location and its path
                           will be printed to stderr. This is useful for
                           debugging wasm-opt failures. By default, wasm-opt
                           runs in-place and the unoptimized artifact is
                           deleted.
  MODULE_KIND=<KIND>       The kind of module to generate. wasixcc can
                           guess this setting most of the time based on
                           compiler/linker flags. Valid values are:
                           * static-main: An executable main module with no
                                 dynamic linking capability
                           * dynamic-main: A main module capable of loading
                                 dynamically-linked side modules at runtime
                           * shared-library: A dynamically-linked side module
                                 which can be loaded by a dynamic main
                           * object-file: An object file
  WASM_EXCEPTIONS=<TYPE>   Whether to enable WebAssembly exception handling
                           support. The default for this value is `yes`, but
                           will be deduced to `no` if `-fno-exceptions`
                           is passed to the compiler, or to `legacy` if
                           `-mllvm --wasm-use-legacy-eh=true` is passed to
                           the compiler or linker. Valid values are:
                           * yes (default): Enable exception handling support using
                               the standardized exnref proposal.
                           * no: No exception handling support.
                           * exnref: Same as 'yes'.
                           * legacy: Enable legacy exception handling support,
                               which is compatible with engines that don't
                               support the standardized exnref proposal. By
                               default, wasm-opt is then used to transform
                               the resulting module into one that uses exnref-
                               based EH.
  PIC=<BOOL>               Whether to enable position-independent code (PIC),
                           required for dynamic linking. PIC will be enabled
                           if module kind is `dynamic-main` or `shared-library`,
                           or if the `-fPIC` flag is passed to the compiler.
  LINK_SYMBOLIC=<BOOL>     Whether to link the output with `-Bsymbolic`, which
                           binds defined symbols locally, hence preventing
                           similarly named symbols from other modules from
                           overriding the module's local symbols. This is
                           enabled by default, but can be disabled by setting
                           this option to `false`. This option is only
                           relevant for dynamic main modules and shared
                           libraries.
  GENERATE_SHELL_SCRIPT=<BOOL>
                           Whether to generate shell scripts for running
                           the resulting binary like a normal native program.
                           This setting applies to executables only. This is
                           useful for running builds that don't have proper
                           support for cross-compilation. Such builds will
                           build a binary and assume they can run it
                           immediately. This option will append a .wasm
                           extension to the output file name, and generate
                           a shell script with the original output name that,
                           once called, will run the wasm binary with wasmer
                           and pass all arguments through to it.
  SHELL_SCRIPT_WASMER_ARGS=<FLAGS>
                           Additional arguments to be passed to wasmer in
                           the shell script. There will be a $SCRIPT_DIR
                           variable in the script pointing to the script's
                           directory. The default is to pass `--dir $SCRIPT_DIR
                           --cwd $SCRIPT_DIR --net --forward-host-env`. Specifying
                           a non-empty list will *override* the default
                           rather than be appended to it. Options must be
                           separated with colons (':').
  DISCARD_UNSUPPORTED_FLAGS=<BOOL>
                           Whether to discard unsupported flags passed to
                           the compiler or linker, such as optimization
                           flags not supported by the underlying LLVM
                           toolchain. By default, unknown flags will be passed
                           through to the underlying tools, which may result
                           in errors if the flags are not supported.
                           Setting this option to `yes` will cause some known
                           unsupported flags and settings to be discarded.
  AUTOCONF_WORKAROUNDS=<BOOL>
                           Attempt to detect autoconf tests and run workarounds
                           to make them behave as intended. conftests for functions
                           usually check the availability of a function name by calling
                           it as `int funcname(void);` and testing if the compiler
                           can compile that. However when targeting WebAssembly you can
                           only call functions with the correct signature, so such
                           tests will always fail. Enabling this option will try to
                           detect such tests and allow the compilation to succeed
                           even if there is a signature mismatch.
  LOCATION=<PATH>          The base location for wasixcc to store its files, such as
                           the sysroot and LLVM toolchain. wasixcc will create
                           a directory at LOCATION and store everything there.
                           The default location is ~/.wasixcc.
  BIN_LOCATION=<PATH>      The location for wasixcc to install its executables, such as
                           wasixccenv and the compiler toolchain. The default is
                           LOCATION/bin, which means executables will be installed
                           to the 'bin' subdirectory of LOCATION.
"#
    );
}
