use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

use anyhow::{Context, Result};

/// The marker used in shell rc files to identify the wasixcc initialization block.
const BLOCK_START_MARKER: &str = r#"# >>> wasixcc initialize >>>
"#;
const BLOCK_END_MARKER: &str = r#"# <<< wasixcc initialize <<<
"#;

/// Contents of `~/.wasixcc/env` — mirrors the heredoc in `create_env_files()`.
pub const ENV_SH_CONTENT: &str = r#"echo "$PATH" | grep "$HOME/.wasixcc/bin" 2>/dev/null >/dev/null || export PATH="$HOME/.wasixcc/bin:$PATH" 2>/dev/null >/dev/null
true
"#;

/// Contents of `~/.wasixcc/env.nu` — mirrors the heredoc in `create_env_files()`.
pub const ENV_NU_CONTENT: &str = r#"let bin = $"($env.HOME)/.wasixcc/bin"
if not ($env.PATH | any {|p| $p == $bin }) {
  $env.PATH = ([$bin] ++ $env.PATH)
}
"#;

/// Contents of `~/.wasixcc/env.xsh` — mirrors the heredoc in `create_env_files()`.
pub const ENV_XSH_CONTENT: &str = r#"import os
home = os.path.expanduser("~")
bin = home + "/.wasixcc/bin"
if bin not in $PATH:
    $PATH.insert(0, bin)
"#;

/// The body appended to POSIX-compatible shell rc files (.bashrc, .zshrc, etc.).
pub const POSIX_BODY: &str = r#"test -f "$HOME/.wasixcc/env" && . "$HOME/.wasixcc/env"
"#;

/// The body appended to fish's config.fish.
pub const FISH_BODY: &str = r#"test -f "$HOME/.wasixcc/env" && source "$HOME/.wasixcc/env"
"#;

/// The body appended to .xonshrc.
pub const XONSH_BODY: &str = r#"import os
p = os.path.expanduser('~/.wasixcc/env.xsh')
if os.path.isfile(p):
    execx(open(p).read(), 'exec', locals(), globals())
"#;

/// The body appended to nushell's config.nu.
pub const NU_BODY: &str = r#"let p = ($env.HOME | path join '.wasixcc' 'env.nu')
if ($p | path exists) { source $p }
"#;

/// Describes a file to create as part of the env-file setup, its path relative
/// to `wasixcc_dir`, its content, and whether it should be made executable.
pub struct EnvFile {
    pub relative_path: &'static str,
    pub content: &'static str,
    pub executable: bool,
}

/// Returns the list of env files that `create_env_files` creates, in order.
pub fn env_files() -> &'static [EnvFile] {
    &[
        EnvFile {
            relative_path: "env",
            content: ENV_SH_CONTENT,
            executable: true,
        },
        EnvFile {
            relative_path: "env.nu",
            content: ENV_NU_CONTENT,
            executable: true,
        },
        EnvFile {
            relative_path: "env.xsh",
            content: ENV_XSH_CONTENT,
            executable: true,
        },
    ]
}

/// Describes an rc file target: the path (relative to home) and the block body
/// to insert.
pub struct RcTarget {
    /// Path relative to the user's home directory.
    pub relative_path: &'static str,
    /// The body that goes between the wasixcc markers.
    pub body: &'static str,
}

/// Returns all rc-file targets that `add_env_files_to_rcs` appends to.
pub fn rc_targets() -> &'static [RcTarget] {
    &[
        RcTarget {
            relative_path: ".bashrc",
            body: POSIX_BODY,
        },
        RcTarget {
            relative_path: ".zshrc",
            body: POSIX_BODY,
        },
        RcTarget {
            relative_path: ".kshrc",
            body: POSIX_BODY,
        },
        RcTarget {
            relative_path: ".mkshrc",
            body: POSIX_BODY,
        },
        RcTarget {
            relative_path: ".profile",
            body: POSIX_BODY,
        },
        RcTarget {
            relative_path: ".config/fish/config.fish",
            body: FISH_BODY,
        },
        RcTarget {
            relative_path: ".xonshrc",
            body: XONSH_BODY,
        },
        RcTarget {
            relative_path: ".config/nushell/config.nu",
            body: NU_BODY,
        },
    ]
}

/// Write the env file contents for the given `wasixcc_dir`.
///
/// This mirrors the `create_env_files` shell function.
pub fn install_env_files(wasixcc_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(wasixcc_dir).with_context(|| {
        format!(
            "Failed to create wasixcc directory at {}",
            wasixcc_dir.display()
        )
    })?;

    for env_file in env_files() {
        let path = wasixcc_dir.join(env_file.relative_path);
        write_env_file(&path, env_file.content)?;

        #[cfg(unix)]
        if env_file.executable {
            set_executable(&path)?;
        }
    }
    Ok(())
}

fn write_env_file(path: &Path, content: &str) -> Result<()> {
    fs::write(path, content)
        .with_context(|| format!("Failed to write env file at {}", path.display()))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .with_context(|| format!("Failed to read metadata for {}", path.display()))?
        .permissions();
    let current_mode = perms.mode();
    // Add executable bit for owner, group, other (like `chmod +x`)
    perms.set_mode(current_mode | 0o111);
    fs::set_permissions(path, perms)
        .with_context(|| format!("Failed to set permissions on {}", path.display()))
}

/// Append the wasixcc initialization block to a shell rc file.
///
/// This mirrors `append_block_to_env_file` in the shell script:
/// - If the file does not exist, it is left untouched.
/// - If the marker is already present, it is left untouched.
/// - Otherwise the block is appended.
pub fn append_block_to_env_file(file: &Path, body: &str) -> Result<()> {
    if !file.exists() {
        return Ok(());
    }

    let mut existing = String::new();
    fs::File::open(file)
        .with_context(|| format!("Failed to open {}", file.display()))?
        .read_to_string(&mut existing)
        .with_context(|| format!("Failed to read {}", file.display()))?;

    if existing.contains(BLOCK_START_MARKER) {
        return Ok(());
    }

    let block = format!("\n{BLOCK_START_MARKER}{body}{BLOCK_END_MARKER}");

    fs::OpenOptions::new()
        .append(true)
        .open(file)
        .with_context(|| format!("Failed to open {} for appending", file.display()))?
        .write_all(block.as_bytes())
        .with_context(|| format!("Failed to append to {}", file.display()))
}

/// Append the wasixcc block to all known shell rc files found under `home_dir`.
///
/// This mirrors the `add_env_files_to_rcs` shell function.
pub fn setup_shell_rcs(home_dir: &Path) -> Result<()> {
    for target in rc_targets() {
        let path = home_dir.join(target.relative_path);
        append_block_to_env_file(&path, target.body)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    // -----------------------------------------------------------------------
    // Helper
    // -----------------------------------------------------------------------

    /// Build a temporary directory hierarchy that looks like a home directory
    /// with various pre-existing shell rc files.
    fn make_home_with_rcs(tmp: &tempfile::TempDir) -> PathBuf {
        let home = tmp.path().to_path_buf();
        // Create all the rc files the installer touches
        for name in [
            ".bashrc", ".zshrc", ".kshrc", ".mkshrc", ".profile", ".xonshrc",
        ] {
            fs::write(home.join(name), "# existing content\n").unwrap();
        }
        fs::create_dir_all(home.join(".config/fish")).unwrap();
        fs::write(
            home.join(".config/fish/config.fish"),
            "# existing content\n",
        )
        .unwrap();
        fs::create_dir_all(home.join(".config/nushell")).unwrap();
        fs::write(
            home.join(".config/nushell/config.nu"),
            "# existing content\n",
        )
        .unwrap();
        home
    }

    // -----------------------------------------------------------------------
    // create_env_files
    // -----------------------------------------------------------------------

    #[test]
    fn create_env_files_writes_all_three_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wasixcc = tmp.path().join(".wasixcc");
        fs::create_dir_all(&wasixcc).unwrap();

        install_env_files(&wasixcc).unwrap();

        assert!(wasixcc.join("env").exists(), "env file should exist");
        assert!(wasixcc.join("env.nu").exists(), "env.nu file should exist");
        assert!(
            wasixcc.join("env.xsh").exists(),
            "env.xsh file should exist"
        );
    }

    #[test]
    fn create_env_files_env_sh_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wasixcc = tmp.path().join(".wasixcc");
        fs::create_dir_all(&wasixcc).unwrap();

        install_env_files(&wasixcc).unwrap();

        let content = fs::read_to_string(wasixcc.join("env")).unwrap();
        assert_eq!(content, ENV_SH_CONTENT);
    }

    #[test]
    fn create_env_files_env_nu_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wasixcc = tmp.path().join(".wasixcc");
        fs::create_dir_all(&wasixcc).unwrap();

        install_env_files(&wasixcc).unwrap();

        let content = fs::read_to_string(wasixcc.join("env.nu")).unwrap();
        assert_eq!(content, ENV_NU_CONTENT);
    }

    #[test]
    fn create_env_files_env_xsh_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wasixcc = tmp.path().join(".wasixcc");
        fs::create_dir_all(&wasixcc).unwrap();

        install_env_files(&wasixcc).unwrap();

        let content = fs::read_to_string(wasixcc.join("env.xsh")).unwrap();
        assert_eq!(content, ENV_XSH_CONTENT);
    }

    #[test]
    #[cfg(unix)]
    fn create_env_files_sets_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let wasixcc = tmp.path().join(".wasixcc");
        fs::create_dir_all(&wasixcc).unwrap();

        install_env_files(&wasixcc).unwrap();

        for name in ["env", "env.nu", "env.xsh"] {
            let path = wasixcc.join(name);
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert!(
                mode & 0o111 != 0,
                "{name} should have executable bit set, mode = {mode:o}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // append_block_to_env_file
    // -----------------------------------------------------------------------

    #[test]
    fn append_block_skips_nonexistent_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("no_such_file");
        // Must not fail and must not create the file
        append_block_to_env_file(&path, POSIX_BODY).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn append_block_appends_to_existing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join(".bashrc");
        fs::write(&path, "# existing\n").unwrap();

        append_block_to_env_file(&path, POSIX_BODY).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains(BLOCK_START_MARKER));
        assert!(content.contains(POSIX_BODY));
        assert!(content.contains(BLOCK_END_MARKER));
        // Original content must be preserved
        assert!(content.starts_with("# existing\n"));
    }

    #[test]
    fn append_block_exact_format() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join(".bashrc");
        fs::write(&path, "# existing\n").unwrap();

        append_block_to_env_file(&path, POSIX_BODY).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let expected_suffix = format!("\n{}{}{}", BLOCK_START_MARKER, POSIX_BODY, BLOCK_END_MARKER);
        assert!(
            content.ends_with(&expected_suffix),
            "File should end with the exact block format.\nActual:\n{content:?}\nExpected suffix:\n{expected_suffix:?}"
        );
    }

    #[test]
    fn append_block_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join(".bashrc");
        fs::write(&path, "# existing\n").unwrap();

        append_block_to_env_file(&path, POSIX_BODY).unwrap();
        let after_first = fs::read_to_string(&path).unwrap();

        append_block_to_env_file(&path, POSIX_BODY).unwrap();
        let after_second = fs::read_to_string(&path).unwrap();

        assert_eq!(
            after_first, after_second,
            "Second call should not modify the file"
        );
    }

    #[test]
    fn append_block_does_not_touch_file_with_existing_marker() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join(".bashrc");
        let initial =
            format!("# existing\n{BLOCK_START_MARKER}\n# something\n{BLOCK_END_MARKER}\n");
        fs::write(&path, &initial).unwrap();

        append_block_to_env_file(&path, POSIX_BODY).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, initial);
    }

    // -----------------------------------------------------------------------
    // setup_shell_rcs
    // -----------------------------------------------------------------------

    #[test]
    fn setup_shell_rcs_appends_to_all_existing_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = make_home_with_rcs(&tmp);

        setup_shell_rcs(&home).unwrap();

        for target in rc_targets() {
            let path = home.join(target.relative_path);
            let content = fs::read_to_string(&path).unwrap();
            assert!(
                content.contains(BLOCK_START_MARKER),
                "{} should contain the marker",
                target.relative_path
            );
            assert!(
                content.contains(target.body),
                "{} should contain its body",
                target.relative_path
            );
        }
    }

    #[test]
    fn setup_shell_rcs_skips_nonexistent_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        // Only create .bashrc; all others are absent
        fs::write(home.join(".bashrc"), "# existing\n").unwrap();

        setup_shell_rcs(&home).unwrap();

        // .bashrc got the block
        let bashrc = fs::read_to_string(home.join(".bashrc")).unwrap();
        assert!(bashrc.contains(BLOCK_START_MARKER));

        // .zshrc doesn't even exist
        assert!(!home.join(".zshrc").exists());
    }

    #[test]
    fn setup_shell_rcs_correct_body_per_shell() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = make_home_with_rcs(&tmp);

        setup_shell_rcs(&home).unwrap();

        // POSIX shells get POSIX_BODY
        for rc in [".bashrc", ".zshrc", ".kshrc", ".mkshrc", ".profile"] {
            let content = fs::read_to_string(home.join(rc)).unwrap();
            assert!(content.contains(POSIX_BODY), "{rc} should have POSIX body");
        }

        // fish gets FISH_BODY
        let fish = fs::read_to_string(home.join(".config/fish/config.fish")).unwrap();
        assert!(
            fish.contains(FISH_BODY),
            "fish config should have FISH body"
        );

        // xonsh gets XONSH_BODY
        let xonsh = fs::read_to_string(home.join(".xonshrc")).unwrap();
        assert!(
            xonsh.contains("env.xsh"),
            "xonshrc should reference env.xsh"
        );

        // nushell gets NU_BODY
        let nu = fs::read_to_string(home.join(".config/nushell/config.nu")).unwrap();
        assert!(
            nu.contains("env.nu"),
            "nushell config should reference env.nu"
        );
    }

    #[test]
    fn setup_shell_rcs_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = make_home_with_rcs(&tmp);

        setup_shell_rcs(&home).unwrap();
        // Collect content after first run
        let contents_after_first: Vec<String> = rc_targets()
            .iter()
            .map(|t| fs::read_to_string(home.join(t.relative_path)).unwrap())
            .collect();

        setup_shell_rcs(&home).unwrap();
        // Compare with second run
        for (target, first) in rc_targets().iter().zip(contents_after_first.iter()) {
            let second = fs::read_to_string(home.join(target.relative_path)).unwrap();
            assert_eq!(
                *first, second,
                "{} should not change on second run",
                target.relative_path
            );
        }
    }

    // -----------------------------------------------------------------------
    // Content constant cross-checks (env file contents match shell heredocs)
    // -----------------------------------------------------------------------

    #[test]
    fn env_sh_contains_wasixcc_bin_path_manipulation() {
        assert!(ENV_SH_CONTENT.contains(".wasixcc/bin"));
        assert!(ENV_SH_CONTENT.contains("PATH"));
    }

    #[test]
    fn env_nu_contains_wasixcc_bin_path_manipulation() {
        assert!(ENV_NU_CONTENT.contains(".wasixcc/bin"));
        assert!(ENV_NU_CONTENT.contains("PATH"));
    }

    #[test]
    fn env_xsh_contains_wasixcc_bin_path_manipulation() {
        assert!(ENV_XSH_CONTENT.contains(".wasixcc/bin"));
        assert!(ENV_XSH_CONTENT.contains("PATH"));
    }
}
