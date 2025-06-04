use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use tracing::level_filters::LevelFilter;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

const COMMANDS: &[&str] = &["cc", "++", "cc++", "ar", "nm", "ranlib", "ld"];

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

fn get_command() -> Result<String> {
    let executable_path = std::env::args().next().context("Empty argument list")?;
    let executable_path = std::path::Path::new(&executable_path);
    let executable_name = executable_path
        .file_name()
        .context("Failed to get executable file name")?
        .to_str()
        .context("Non-UTF8 characters in executable name")?;

    if let Some(command_name) = executable_name.strip_prefix("wasix-") {
        Ok(command_name.to_owned())
    } else if let Some(command_name) = executable_name.strip_prefix("wasix") {
        Ok(command_name.to_owned())
    } else {
        bail!(
            "Failed to get command name; this binary must be run with a name in \
            the form 'wasix-<command-name>' or 'wasix<command-name>`, such as \
            wasix-cc; given {executable_name}",
        )
    }
}

fn install_executables() -> Result<()> {
    let path = PathBuf::from(
        std::env::args()
            .nth(2)
            .context("Usage: wasixcc install-executables <PATH>")?,
    );

    std::fs::create_dir_all(&path)
        .with_context(|| format!("Failed to create directory at {path:?}"))?;

    let exe_path = std::env::current_exe().context("Failed to get current executable path")?;

    for command in COMMANDS {
        let target = path.join(format!("wasix{}", command));

        if std::fs::metadata(&target).is_ok() {
            std::fs::remove_file(&target)
                .with_context(|| format!("Failed to remove existing file at {target:?}"))?;
        }

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&exe_path, &target)
                .with_context(|| format!("Failed create symlink at {target:?}"))?;
            let permissions = std::os::unix::fs::PermissionsExt::from_mode(0o755);
            std::fs::set_permissions(&target, permissions)
                .with_context(|| format!("Failed to set permissions for {target:?}"))?;
        }
        #[cfg(not(unix))]
        {
            bail!("wasixcc only supports installation on unix systems at this time");
        }

        println!("Created command {target:?}");
    }

    Ok(())
}

fn print_version() {
    let version = env!("CARGO_PKG_VERSION");

    println!("wasixcc version: {version}");
}

fn run() -> Result<()> {
    if matches!(std::env::args().nth(1), Some(x) if x == "install-executables") {
        return install_executables();
    }

    if std::env::args().any(|arg| arg == "--version" || arg == "-v") {
        print_version();
        return Ok(());
    }

    let command_name = get_command()?;
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

fn main() {
    setup_tracing();

    match run() {
        Ok(()) => (),
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
