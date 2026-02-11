use std::process::Command;

use anyhow::{Context, Result, bail};
use tracing::level_filters::LevelFilter;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::args::{UserSettings, get_args_and_user_settings};

mod args;
mod compiler;
mod download;
mod wasixccenv;

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

fn get_command_name() -> Result<String> {
    let exe_path = std::env::args().next().context("Empty argument list")?;
    let exe_path = std::path::Path::new(&exe_path);
    let exe_name = exe_path
        .file_name()
        .context("Failed to get executable file name")?
        .to_str()
        .context("Non-UTF8 characters in executable name")?;
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

fn run() -> Result<()> {
    let command_name = get_command_name()?;

    match command_name.as_str() {
        // Management command; the executable name is "wasixccenv", which
        // shows up as "ccenv" here.
        "ccenv" => wasixccenv::run(),

        // Tool commands
        "cc" => run_compiler(false),
        "++" | "cc++" => run_compiler(true),
        "ld" => run_linker(),
        "ar" => run_ar(),
        "nm" => run_nm(),
        "ranlib" => run_ranlib(),

        cmd => bail!("Unknown command {cmd}"),
    }
}

fn run_tool_with_passthrough_args(
    tool: &str,
    args: Vec<String>,
    user_settings: UserSettings,
) -> Result<()> {
    if is_version_command() {
        print_version();
    }

    let tool_path = user_settings.llvm_location.get_tool_path(tool);
    let mut command = Command::new(tool_path);
    command.args(args);
    compiler::run_command(command)
}

fn run_compiler(run_cxx: bool) -> Result<()> {
    tracing::info!("Starting in compiler mode");

    let (args, user_settings) = get_args_and_user_settings()?;
    if is_version_command() {
        run_tool_with_passthrough_args(
            if run_cxx { "clang++" } else { "clang" },
            args,
            user_settings,
        )
    } else {
        compiler::run(args, user_settings, run_cxx)
    }
}

fn run_linker() -> Result<()> {
    tracing::info!("Starting in linker mode");

    let (args, user_settings) = get_args_and_user_settings()?;
    if is_version_command() {
        run_tool_with_passthrough_args("wasm-ld", args, user_settings)
    } else {
        compiler::link_only(args, user_settings)
    }
}

fn run_ar() -> Result<()> {
    tracing::info!("Starting in ar mode");

    let (args, user_settings) = get_args_and_user_settings()?;
    run_tool_with_passthrough_args("llvm-ar", args, user_settings)
}

fn run_nm() -> Result<()> {
    tracing::info!("Starting in nm mode");

    let (args, user_settings) = get_args_and_user_settings()?;
    run_tool_with_passthrough_args("llvm-nm", args, user_settings)
}

fn run_ranlib() -> Result<()> {
    tracing::info!("Starting in ranlib mode");

    let (args, user_settings) = get_args_and_user_settings()?;
    run_tool_with_passthrough_args("llvm-ranlib", args, user_settings)
}

fn is_version_command() -> bool {
    std::env::args().skip(1).any(|a| a == "--version")
}

fn print_version() {
    println!("wasixcc {}", env!("CARGO_PKG_VERSION"));
    println!("----------------------------------");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::LlvmLocation;

    use std::fs;
    use tempfile::TempDir;

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
