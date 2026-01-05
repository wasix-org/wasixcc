use std::{path::PathBuf, str::FromStr};

use anyhow::{Context, Result, bail};
use tracing::level_filters::LevelFilter;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};
use wasixcc::download::TagSpec;

#[cfg(unix)]
const COMMANDS: &[&str] = &["cc", "++", "cc++", "ar", "nm", "ranlib", "ld"];

enum WasixccCommand {
    Help,
    Version,
    InstallExecutables(PathBuf),
    DownloadSysroot(TagSpec),
    DownloadLlvm(TagSpec),
    DownloadBinaryen(TagSpec),
    DownloadAll,
    PrintSysroot,
    RunTool,
}

fn setup_tracing() {
    let fmt_layer = fmt::layer()
        .with_target(true)
        .with_ansi(true)
        .with_thread_ids(true)
        .with_span_events(fmt::format::FmtSpan::CLOSE)
        .with_writer(std::io::stderr)
        .compact();

    let filter_layer = EnvFilter::builder()
        .with_default_directive(LevelFilter::OFF.into())
        .from_env_lossy();

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();
}

fn get_executable_name() -> Result<String> {
    let executable_path = std::env::args().next().context("Empty argument list")?;
    let executable_path = std::path::Path::new(&executable_path);
    Ok(executable_path
        .file_name()
        .context("Failed to get executable file name")?
        .to_str()
        .context("Non-UTF8 characters in executable name")?
        .to_owned())
}

fn get_command(exe_name: &str) -> Result<String> {
    if let Some(command_name) = exe_name.strip_prefix("wasix-") {
        Ok(command_name.to_owned())
    } else if let Some(command_name) = exe_name.strip_prefix("wasix") {
        Ok(command_name.to_owned())
    } else {
        bail!(
            "Failed to get command name; this binary must be run with a name in \
            the form 'wasix-<command-name>' or 'wasix<command-name>`, such as \
            wasix-cc; given {exe_name}",
        )
    }
}

#[cfg_attr(target_vendor = "wasmer", allow(unused_variables))]
fn install_executables(path: PathBuf) -> Result<()> {
    #[cfg(not(unix))]
    {
        bail!("wasixcc only supports installation on unix systems at this time");
    }

    #[cfg(unix)]
    {
        use std::{env, fs, os::unix::fs as unix_fs};

        fs::create_dir_all(&path)
            .with_context(|| format!("Failed to create directory at {path:?}"))?;

        let exe_path = env::current_exe().context("Failed to get current executable path")?;

        for command in COMMANDS {
            let target = path.join(format!("wasix{}", command));

            if fs::metadata(&target).is_ok() {
                fs::remove_file(&target)
                    .with_context(|| format!("Failed to remove existing file at {target:?}"))?;
            }

            unix_fs::symlink(&exe_path, &target)
                .with_context(|| format!("Failed create symlink at {target:?}"))?;
            let permissions = unix_fs::PermissionsExt::from_mode(0o755);
            fs::set_permissions(&target, permissions)
                .with_context(|| format!("Failed to set permissions for {target:?}"))?;

            println!("Created command {target:?}");
        }

        Ok(())
    }
}

fn print_version(exe_name: &str) {
    let version = env!("CARGO_PKG_VERSION");

    println!("{exe_name} version: {version}");
}

fn print_sysroot() -> Result<()> {
    let sysroot = wasixcc::get_sysroot()?;
    println!("{}", sysroot.display());
    Ok(())
}

fn print_help(exe_name: &str) {
    println!(
        r#"Usage: {exe_name} [OPTIONS] -- [PASS-THROUGH OPTIONS]

Options:
  --help, -h                     Print this help message
  --version, -v                  Print version information
  -s[CONFIG]=[VALUE]             Set a configuration value, see list below
  --install-executables <PATH>   Install executables to the specified path
  --download-sysroot <TAG>       Download and install the wasix-libc sysroot.
                                 The tag can be 'latest' or a specific tag
                                 such as 'v2025-01-01.1'. If the tag is
                                 omitted, the latest version will be
                                 downloaded. The downloaded sysroot will be
                                 unpacked into the directory pointed to by
                                 the SYSROOT_PREFIX setting.
  --download-llvm <TAG>          Download and install the LLVM toolchain.
                                 The tag can be 'latest' or a specific tag
                                 such as 'v2025-01-01.1'. If the tag is
                                 omitted, the latest version will be
                                 downloaded. The downloaded toolchain will be
                                 unpacked into the directory pointed to by
                                 the LLVM_LOCATION setting.
  --download-all                 Download the latest version of both the 
                                 sysroot and the LLVM toolchain.
  --print-sysroot                Print sysroot location corresponding to
                                 current build configuration

Configuration options can be provided on the command line using the
'-s' flag, or using environment variables prefixed with 'WASIXCC_'.
The following configuration options are available:");
  SYSROOT=<PATH>           Set the sysroot location
  SYSROOT_PREFIX=<PREFIX>  Set the sysroot prefix, which is expected to
                           contain 3 subdirectories: 'sysroot',
                           'sysroot-eh', and 'sysroot-ehpic'.
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
                           compiler. If this setting is left out, {exe_name}
                           will look at compiler flags to determine whether
                           to run `wasm-opt`. If no flags are found, default
                           behavior is to run `wasm-opt`.
  WASM_OPT_FLAGS=<FLAGS>   Extra flags to pass to `wasm-opt`, separated by
                           colons (':'). Specifying a non-empty list of
                           extra flags for wasm-opt will imply
                           `RUN_WASM_OPT=yes` unless an explicit value is
                           provided for `RUN_WASM_OPT`.
  WASM_OPT_SUPPRESS_DEFAULT=<BOOL>
                           Whether to suppress the default flags {exe_name}
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
  MODULE_KIND=<KIND>       The kind of module to generate. {exe_name} can
                           guess this setting most of the time based on
                           compiler/linker flags. Valid values are:
                           * static-main: An executable main module with no
                                 dynamic linking capability
                           * dynamic-main: A main module capable of loading
                                 dynamically-linked side modules at runtime
                           * shared-library: A dynamically-linked side module
                                 which can be loaded by a dynamic main
                           * object-file: An object file
  WASM_EXCEPTIONS=<BOOL>   Whether to enable WebAssembly exception handling
                           support. This value can be deduced from the
                           `-fwasm-exceptions`/`-fno-wasm-exceptions` flags
                           passed to the compiler.
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
                           rather than be appended to it.  Options must be
                           separated with colons (':').

Note: Pass-through options are passed directly to the underlying
LLVM executables (e.g., clang, wasm-ld, etc.). This is useful for
getting version information or help messages from the underlying
tools, but has little use otherwise.
"#
    );
}

fn get_wasixcc_command(exe_name: &str) -> WasixccCommand {
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        return match arg.as_str() {
            "--help" | "-h" => WasixccCommand::Help,

            "--version" | "-v" => WasixccCommand::Version,

            "--install-executables" => {
                let Some(path) = args.next() else {
                    println!("Usage: {exe_name} --install-executables <PATH>");
                    std::process::exit(1);
                };
                WasixccCommand::InstallExecutables(PathBuf::from(path))
            }

            "--download-sysroot" => {
                let tag_spec = match args.next() {
                    Some(spec) => match TagSpec::from_str(&spec) {
                        Ok(x) => x,
                        Err(e) => {
                            eprintln!("{e}");
                            std::process::exit(1);
                        }
                    },
                    None => TagSpec::Latest,
                };
                WasixccCommand::DownloadSysroot(tag_spec)
            }

            "--download-llvm" => {
                let tag_spec = match args.next() {
                    Some(spec) => match TagSpec::from_str(&spec) {
                        Ok(x) => x,
                        Err(e) => {
                            eprintln!("{e}");
                            std::process::exit(1);
                        }
                    },
                    None => TagSpec::Latest,
                };
                WasixccCommand::DownloadLlvm(tag_spec)
            }

            "--download-binaryen" => {
                let tag_spec = match args.next() {
                    Some(spec) => match TagSpec::from_str(&spec) {
                        Ok(x) => x,
                        Err(e) => {
                            eprintln!("{e}");
                            std::process::exit(1);
                        }
                    },
                    None => TagSpec::Latest,
                };
                WasixccCommand::DownloadBinaryen(tag_spec)
            }

            "--download-all" => WasixccCommand::DownloadAll,

            "--print-sysroot" => WasixccCommand::PrintSysroot,

            "--" => WasixccCommand::RunTool,

            _ => continue,
        };
    }

    WasixccCommand::RunTool
}

fn run() -> Result<()> {
    let exe_name = get_executable_name()?;

    let command = get_wasixcc_command(&exe_name);

    match command {
        WasixccCommand::Help => {
            print_help(&exe_name);
            Ok(())
        }
        WasixccCommand::Version => {
            print_version(&exe_name);
            Ok(())
        }
        WasixccCommand::InstallExecutables(path) => install_executables(path),
        WasixccCommand::DownloadSysroot(tag_spec) => wasixcc::download_sysroot(tag_spec),
        WasixccCommand::DownloadLlvm(tag_spec) => wasixcc::download_llvm(tag_spec),
        WasixccCommand::DownloadBinaryen(tag_spec) => wasixcc::download_binaryen(tag_spec),
        WasixccCommand::DownloadAll => {
            wasixcc::download_llvm(TagSpec::Latest)?;
            wasixcc::download_sysroot(TagSpec::Latest)?;
            wasixcc::download_binaryen(TagSpec::Latest)?;
            Ok(())
        }
        WasixccCommand::PrintSysroot => print_sysroot(),
        WasixccCommand::RunTool => {
            let command_name = get_command(&exe_name)?;
            match command_name.as_str() {
                "cc" => wasixcc::run_compiler(false),
                "++" | "cc++" => wasixcc::run_compiler(true),
                "ld" => wasixcc::run_linker(),
                "ar" => wasixcc::run_ar(),
                "nm" => wasixcc::run_nm(),
                "ranlib" => wasixcc::run_ranlib(),
                cmd => bail!("Unknown command {cmd}"),
            }
        }
    }
}

fn main() {
    setup_tracing();

    tracing::debug!("Starting with CLI args: {:?}", std::env::args());

    match run() {
        Ok(()) => (),
        Err(e) => {
            eprintln!("Error: {:?}", e);
            std::process::exit(1);
        }
    }
}
