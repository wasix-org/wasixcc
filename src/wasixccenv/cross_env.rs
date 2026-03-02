use crate::args::{LlvmLocation, UserSettings};

/// Options controlling which sections appear in the generated cross-env script.
pub struct CrossEnvOptions {
    /// Skip wasmer binfmt registration.
    pub no_binfmt: bool,
    /// Disable DISCARD_UNSUPPORTED_FLAGS and AUTOCONF_WORKAROUNDS.
    pub no_hacks: bool,
    /// Disable wasm exception handling.
    pub no_exceptions: bool,
    /// Disable position-independent code (PIC is on by default).
    pub no_pic: bool,
}

/// Generate a sourceable POSIX shell script for entering a cross-compilation environment.
///
/// The script will:
/// - Check for required toolchain directories
/// - Export PATH with all toolchain bin directories
/// - Set toolchain variables (CC, CXX, AR, etc.)
/// - Export wasixcc configuration variables
/// - Export libtool-specific variables when libtool is present
/// - Detect SUDO availability
/// - Optionally register wasmer as a binfmt handler (unless `no_binfmt` is true)
pub fn generate_cross_env_script(
    user_settings: &UserSettings,
    options: &CrossEnvOptions,
) -> String {
    let mut script = String::new();

    script.push_str("#!/bin/sh\n\n");

    // Define location variables
    add_location_variables(&mut script, user_settings);

    // Check for required directories
    add_directory_checks(&mut script, user_settings);

    // Build and export PATH
    add_path_export(&mut script, user_settings);

    // Export toolchain variables
    add_toolchain_exports(&mut script);

    // Export wasixcc configuration
    add_wasixcc_settings(&mut script, options);

    // Export sysroot locations
    add_sysroot_exports(&mut script, user_settings, options);

    // Export libtool variables if libtool is present
    if user_settings.libtool_location.get_bin_dir().is_some() {
        add_libtool_exports(&mut script, user_settings);
    }

    // Add SUDO detection
    add_sudo_detection(&mut script);

    // Optionally add wasmer binfmt registration
    if !options.no_binfmt {
        add_binfmt_registration(&mut script);
    }

    script
}

/// Launch an interactive shell with the cross-compilation environment sourced.
///
/// Writes the generated script to a temporary file, then exec's into a shell
/// that sources it on startup.  This function does **not** return on success.
pub fn exec_cross_shell(
    user_settings: &UserSettings,
    options: &CrossEnvOptions,
) -> anyhow::Result<()> {
    use std::io::Write;

    let script = generate_cross_env_script(user_settings, options);

    // Write the script to a temporary file that persists until the shell exits.
    let mut tmp = tempfile::NamedTempFile::new()?;
    tmp.write_all(script.as_bytes())?;
    tmp.flush()?;
    let tmp_path = tmp.into_temp_path();

    // Build a bash invocation that sources our script then drops to interactive.
    let err = exec::Command::new("bash")
        .arg("--rcfile")
        .arg(&*tmp_path)
        .arg("-i")
        .exec();

    // exec::Command::exec only returns on error.
    Err(anyhow::anyhow!("Failed to exec into shell: {err}"))
}

fn add_location_variables(script: &mut String, user_settings: &UserSettings) {
    script.push_str("# Define toolchain locations\n");
    script.push_str(&format!(
        "export WASIXCC_LOCATION='{}'\n",
        user_settings.location.display()
    ));
    script.push_str(&format!(
        "export WASIXCC_BIN_LOCATION='{}'\n",
        user_settings.bin_location.display()
    ));

    match &user_settings.llvm_location {
        LlvmLocation::UserProvided(path) => {
            script.push_str(&format!(
                "export WASIXCC_LLVM_LOCATION='{}'\n",
                path.display()
            ));
        }
        LlvmLocation::DefaultPath(path) => {
            script.push_str(&format!(
                "export WASIXCC_LLVM_LOCATION='{}'\n",
                path.display()
            ));

            // Don't define the variable if LLVM is not present, to avoid confusion
        }
    }

    match &user_settings.binaryen_location {
        crate::args::BinaryenLocation::UserProvided(path) => {
            script.push_str(&format!(
                "export WASIXCC_BINARYEN_LOCATION='{}'\n",
                path.display()
            ));
        }
        crate::args::BinaryenLocation::DefaultPath(path) => {
            script.push_str(&format!(
                "export WASIXCC_BINARYEN_LOCATION='{}'\n",
                path.display()
            ));
        }
    }

    match &user_settings.libtool_location {
        crate::args::LibtoolLocation::UserProvided(path) => {
            script.push_str(&format!(
                "export WASIXCC_LIBTOOL_LOCATION='{}'\n",
                path.display()
            ));
        }
        crate::args::LibtoolLocation::DefaultPath(path) => {
            script.push_str(&format!(
                "export WASIXCC_LIBTOOL_LOCATION='{}'\n",
                path.display()
            ));
        }
    }

    script.push('\n');
}

fn add_directory_checks(script: &mut String, user_settings: &UserSettings) {
    script.push_str("# Check for required directories\n");

    add_dir_check(script, "$WASIXCC_BIN_LOCATION", "wasixcc bin directory");

    if user_settings.llvm_location.get_bin_dir().is_some() {
        add_dir_check(script, "$WASIXCC_LLVM_LOCATION", "LLVM bin directory");
    }

    if user_settings.binaryen_location.get_bin_dir().is_some() {
        add_dir_check(
            script,
            "$WASIXCC_BINARYEN_LOCATION",
            "binaryen bin directory",
        );
    }

    if user_settings.libtool_location.get_bin_dir().is_some() {
        add_dir_check(script, "$WASIXCC_LIBTOOL_LOCATION", "libtool bin directory");
    }

    script.push('\n');
}

fn add_dir_check(script: &mut String, var_ref: &str, name: &str) {
    script.push_str(&format!("if ! test -d \"{var_ref}\" ; then\n"));
    script.push_str(&format!(
        "    echo \"Error: {name} does not exist: {var_ref}\" >&2\n"
    ));
    script.push_str("    return 1\n");
    script.push_str("fi\n");
}

fn add_path_export(script: &mut String, user_settings: &UserSettings) {
    script.push_str("# Export PATH with all toolchain directories\n");

    let mut path_components = vec!["$WASIXCC_BIN_LOCATION".to_string()];

    if user_settings.llvm_location.get_bin_dir().is_some() {
        path_components.push("$WASIXCC_LLVM_LOCATION".to_string());
    }

    if user_settings.binaryen_location.get_bin_dir().is_some() {
        path_components.push("$WASIXCC_BINARYEN_LOCATION".to_string());
    }

    if user_settings.libtool_location.get_bin_dir().is_some() {
        path_components.push("$WASIXCC_LIBTOOL_LOCATION".to_string());
    }

    let path_string = path_components.join(":");
    script.push_str(&format!("export PATH=\"{path_string}:$PATH\"\n\n"));
}

fn add_toolchain_exports(script: &mut String) {
    script.push_str("# Export toolchain variables\n");
    script.push_str("export CC=wasixcc\n");
    script.push_str("export CXX=wasix++\n");
    script.push_str("export AR=wasixar\n");
    script.push_str("export NM=wasixnm\n");
    script.push_str("export LD=wasixld\n");
    script.push_str("export RANLIB=wasixranlib\n");
    script.push_str("export AS=llvm-as\n");
    script.push_str("export STRIP=llvm-strip\n");
    script.push('\n');
}

fn add_libtool_exports(script: &mut String, user_settings: &UserSettings) {
    script.push_str("# Export libtool-specific variables\n");
    // Resolve the libtool share directory at generation time for POSIX compliance
    // (avoids bash-only ${var%pattern} expansion)
    let libtool_bin = user_settings
        .libtool_location
        .get_bin_dir()
        .expect("libtool bin dir should exist when add_libtool_exports is called");
    let libtool_root = libtool_bin
        .parent()
        .expect("libtool bin dir should have a parent");
    let share_aclocal = libtool_root.join("share/aclocal");
    let share_libtool = libtool_root.join("share/libtool");
    script.push_str(&format!(
        "export ACLOCAL_PATH=\"{}:$ACLOCAL_PATH\"\n",
        share_aclocal.display()
    ));
    script.push_str(&format!(
        "export _lt_pkgdatadir=\"{}\"\n",
        share_libtool.display()
    ));
    script.push('\n');
}

fn add_wasixcc_settings(script: &mut String, options: &CrossEnvOptions) {
    script.push_str("# Export wasixcc configuration\n");

    // Cross-env flags unconditionally overwrite any pre-existing env var value.
    // When a flag is absent the default cross-env value is applied only if the
    // variable is not already set in the environment (POSIX `:=` assignment).
    if options.no_hacks {
        script.push_str("export WASIXCC_DISCARD_UNSUPPORTED_FLAGS=no\n");
        script.push_str("export WASIXCC_AUTOCONF_WORKAROUNDS=no\n");
        script.push_str("export WASIXCC_INCLUDE_USR_DIRS=no\n");
        script.push_str("export WASIXCC_IGNORE_SOME_WARNINGS=no\n");
    } else {
        script.push_str(": \"${WASIXCC_DISCARD_UNSUPPORTED_FLAGS:=yes}\"\n");
        script.push_str("export WASIXCC_DISCARD_UNSUPPORTED_FLAGS\n");
        script.push_str(": \"${WASIXCC_AUTOCONF_WORKAROUNDS:=yes}\"\n");
        script.push_str("export WASIXCC_AUTOCONF_WORKAROUNDS\n");
        // Not a hack, but added anyway to avoid adding more --no-* flags
        script.push_str(": \"${WASIXCC_INCLUDE_USR_DIRS:=yes}\"\n");
        script.push_str("export WASIXCC_INCLUDE_USR_DIRS\n");
        // This is definitely a hack but it's required for build-scripts for now
        script.push_str(": \"${WASIXCC_IGNORE_SOME_WARNINGS:=yes}\"\n");
        script.push_str("export WASIXCC_IGNORE_SOME_WARNINGS\n");
    }

    if options.no_exceptions {
        script.push_str("export WASIXCC_WASM_EXCEPTIONS=no\n");
    } else {
        script.push_str(": \"${WASIXCC_WASM_EXCEPTIONS:=yes}\"\n");
        script.push_str("export WASIXCC_WASM_EXCEPTIONS\n");
    }

    if options.no_pic {
        script.push_str("export WASIXCC_PIC=no\n");
        script.push_str("export WASIXCC_INCLUDE_CPP_SYMBOLS=no\n");
    } else {
        script.push_str(": \"${WASIXCC_PIC:=yes}\"\n");
        script.push_str("export WASIXCC_PIC\n");
        script.push_str(": \"${WASIXCC_INCLUDE_CPP_SYMBOLS:=yes}\"\n");
        script.push_str("export WASIXCC_INCLUDE_CPP_SYMBOLS\n");
    }

    script.push('\n');
}

fn add_sysroot_exports(
    script: &mut String,
    user_settings: &UserSettings,
    options: &CrossEnvOptions,
) {
    use crate::compiler::WasmExceptionStyle;

    script.push_str("# Export sysroot locations\n");
    script.push_str(&format!(
        "export WASIXCC_SYSROOT_PREFIX=\"{}\"\n",
        user_settings.sysroot_prefix.display()
    ));

    // Compute the concrete sysroot path, reflecting the EH/PIC options that
    // are being written into the script (mirrors UserSettings::sysroot_location).
    let effective_sysroot = if let Some(explicit) = &user_settings.sysroot_location {
        explicit.clone()
    } else {
        let wasm_exceptions = if options.no_exceptions {
            WasmExceptionStyle::Off
        } else {
            user_settings.wasm_exceptions
        };
        let pic = !options.no_pic;
        match (wasm_exceptions, pic) {
            (WasmExceptionStyle::Legacy, true) => {
                user_settings.sysroot_prefix.join("sysroot-ehpic")
            }
            (WasmExceptionStyle::Legacy, false) => user_settings.sysroot_prefix.join("sysroot-eh"),
            (WasmExceptionStyle::Exnref, true) => {
                user_settings.sysroot_prefix.join("sysroot-exnref-ehpic")
            }
            (WasmExceptionStyle::Exnref, false) => {
                user_settings.sysroot_prefix.join("sysroot-exnref-eh")
            }
            (WasmExceptionStyle::Off, _) => user_settings.sysroot_prefix.join("sysroot"),
        }
    };
    script.push_str(&format!(
        "export WASIXCC_SYSROOT=\"{}\"\n",
        effective_sysroot.display()
    ));

    script.push('\n');
}

fn add_sudo_detection(script: &mut String) {
    script.push_str("# Detect SUDO availability\n");
    script.push_str("if test -z \"$SUDO\" && test \"$(id -u)\" -ne 0 ; then\n");
    script.push_str("    SUDO=\"$(command -v sudo 2>/dev/null || true)\"\n");
    script.push_str("fi\n");
    script.push('\n');
}

fn add_binfmt_registration(script: &mut String) {
    script.push_str("# Register wasmer as a binfmt handler\n");
    script.push_str("WASMER_BIN=\"$(command -v wasmer 2>/dev/null)\"\n");
    script.push_str("if test -z \"$WASMER_BIN\" ; then\n");
    script.push_str("    echo \"Error: wasmer not found in PATH. Please install wasmer or use --no-binfmt.\" >&2\n");
    script.push_str("    return 1\n");
    script.push_str("fi\n");
    script.push('\n');
    script.push_str("if test \"$SUDO\" != \"\" ; then\n");
    script.push_str("    \"$SUDO\" \"$WASMER_BIN\" binfmt reregister >/dev/null || echo \"Warning: Failed to register wasmer as a binfmt handler. You might need to run 'sudo wasmer binfmt register' manually.\" >&2\n");
    script.push_str("else\n");
    script.push_str("    \"$WASMER_BIN\" binfmt reregister >/dev/null || echo \"Warning: Failed to register wasmer as a binfmt handler. You might need to run 'sudo wasmer binfmt register' manually.\" >&2\n");
    script.push_str("fi\n");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{BinaryenLocation, LibtoolLocation, LlvmLocation};
    use std::path::PathBuf;

    fn default_options() -> CrossEnvOptions {
        CrossEnvOptions {
            no_binfmt: false,
            no_hacks: false,
            no_exceptions: false,
            no_pic: false,
        }
    }

    fn make_test_settings() -> UserSettings {
        UserSettings {
            sysroot_location: None,
            sysroot_prefix: PathBuf::from("/home/user/.wasixcc/sysroot"),
            llvm_location: LlvmLocation::UserProvided(PathBuf::from("/home/user/.wasixcc/llvm")),
            binaryen_location: BinaryenLocation::UserProvided(PathBuf::from(
                "/home/user/.wasixcc/binaryen",
            )),
            libtool_location: LibtoolLocation::UserProvided(PathBuf::from(
                "/home/user/.wasixcc/libtool",
            )),
            extra_compiler_flags: vec![],
            extra_compiler_post_flags: vec![],
            extra_compiler_flags_c: vec![],
            extra_compiler_post_flags_c: vec![],
            extra_compiler_flags_cxx: vec![],
            extra_compiler_post_flags_cxx: vec![],
            extra_linker_flags: vec![],
            include_cpp_symbols: false,
            run_wasm_opt: None,
            wasm_opt_flags: vec![],
            wasm_opt_suppress_default: false,
            wasm_opt_preserve_unoptimized: false,
            module_kind: None,
            wasm_exceptions: crate::compiler::WasmExceptionStyle::Exnref,
            pic: false,
            link_symbolic: true,
            generate_shell_script: false,
            shell_script_wasmer_args: vec![],
            force_static_dependencies: false,
            discard_unsupported_flags: false,
            autoconf_workarounds: false,
            location: PathBuf::from("/home/user/.wasixcc"),
            bin_location: PathBuf::from("/home/user/.wasixcc/bin"),
            include_usr_dirs: false,
            ignore_some_warnings: false,
        }
    }

    #[test]
    fn test_generate_script_defaults() {
        let settings = make_test_settings();
        let options = default_options();
        let script = generate_cross_env_script(&settings, &options);

        // Check shebang
        assert!(script.starts_with("#!/bin/sh\n"));

        // Check location variable definitions
        assert!(script.contains("export WASIXCC_LOCATION=\"/home/user/.wasixcc\""));
        assert!(script.contains("export WASIXCC_BIN_LOCATION=\"/home/user/.wasixcc/bin\""));
        assert!(script.contains("export WASIXCC_LLVM_LOCATION=\"/home/user/.wasixcc/llvm/bin\""));
        assert!(
            script
                .contains("export WASIXCC_BINARYEN_LOCATION=\"/home/user/.wasixcc/binaryen/bin\"")
        );
        assert!(
            script.contains("export WASIXCC_LIBTOOL_LOCATION=\"/home/user/.wasixcc/libtool/bin\"")
        );

        // Check directory checks reference variables
        assert!(script.contains("test -d \"$WASIXCC_BIN_LOCATION\""));
        assert!(script.contains("test -d \"$WASIXCC_LLVM_LOCATION\""));
        assert!(script.contains("test -d \"$WASIXCC_BINARYEN_LOCATION\""));
        assert!(script.contains("test -d \"$WASIXCC_LIBTOOL_LOCATION\""));
        assert!(script.contains("return 1"));

        // Check PATH export uses variables
        assert!(script.contains("export PATH=\"$WASIXCC_BIN_LOCATION:$WASIXCC_LLVM_LOCATION:$WASIXCC_BINARYEN_LOCATION:$WASIXCC_LIBTOOL_LOCATION:$PATH\""));

        // Check toolchain exports
        assert!(script.contains("export CC=wasixcc"));
        assert!(script.contains("export CXX=wasix++"));
        assert!(script.contains("export AR=wasixar"));
        assert!(script.contains("export NM=wasixnm"));
        assert!(script.contains("export LD=wasixld"));
        assert!(script.contains("export RANLIB=wasixranlib"));
        assert!(script.contains("export AS=llvm-as"));
        assert!(script.contains("export STRIP=llvm-strip"));
        assert!(script.contains("export INSTALL=\"/usr/bin/install --strip-program llvm-strip\""));

        // Check wasixcc settings (defaults: hacks on, exceptions on, pic on)
        // Variables are set via POSIX conditional assignment (:=) so user env vars take precedence
        assert!(script.contains(": \"${WASIXCC_DISCARD_UNSUPPORTED_FLAGS:=yes}\""));
        assert!(script.contains(": \"${WASIXCC_AUTOCONF_WORKAROUNDS:=yes}\""));
        assert!(script.contains(": \"${WASIXCC_PIC:=yes}\""));
        assert!(script.contains(": \"${WASIXCC_INCLUDE_CPP_SYMBOLS:=yes}\""));
        assert!(script.contains(": \"${WASIXCC_WASM_EXCEPTIONS:=yes}\""));
        // All variables are exported after conditional assignment
        assert!(script.contains("export WASIXCC_DISCARD_UNSUPPORTED_FLAGS"));
        assert!(script.contains("export WASIXCC_AUTOCONF_WORKAROUNDS"));
        assert!(script.contains("export WASIXCC_PIC"));
        assert!(script.contains("export WASIXCC_INCLUDE_CPP_SYMBOLS"));
        assert!(script.contains("export WASIXCC_WASM_EXCEPTIONS"));

        // Check libtool exports use resolved paths (POSIX compliant)
        assert!(script.contains("export ACLOCAL_PATH=\"/home/user/.wasixcc/libtool/share/aclocal"));
        assert!(
            script.contains("export _lt_pkgdatadir=\"/home/user/.wasixcc/libtool/share/libtool\"")
        );

        // Check SUDO detection
        assert!(script.contains("id -u"));
        assert!(script.contains("command -v sudo"));

        // Check binfmt registration
        assert!(script.contains("WASMER_BIN=\"$(command -v wasmer 2>/dev/null)\""));
        assert!(script.contains("\"$WASMER_BIN\" binfmt reregister"));
    }

    #[test]
    fn test_generate_script_without_binfmt() {
        let settings = make_test_settings();
        let options = CrossEnvOptions {
            no_binfmt: true,
            ..default_options()
        };
        let script = generate_cross_env_script(&settings, &options);

        assert!(!script.contains("WASMER_BIN"));
        assert!(!script.contains("binfmt"));
        assert!(script.contains("export CC=wasixcc"));
        assert!(script.contains("id -u"));
    }

    #[test]
    fn test_no_hacks_disables_workarounds() {
        let settings = make_test_settings();
        let options = CrossEnvOptions {
            no_hacks: true,
            ..default_options()
        };
        let script = generate_cross_env_script(&settings, &options);

        assert!(script.contains("export WASIXCC_DISCARD_UNSUPPORTED_FLAGS=no"));
        assert!(script.contains("export WASIXCC_AUTOCONF_WORKAROUNDS=no"));
        // PIC should still use conditional assignment
        assert!(script.contains(": \"${WASIXCC_PIC:=yes}\""));
    }

    #[test]
    fn test_no_exceptions() {
        let settings = make_test_settings();
        let options = CrossEnvOptions {
            no_exceptions: true,
            ..default_options()
        };
        let script = generate_cross_env_script(&settings, &options);

        assert!(script.contains("export WASIXCC_WASM_EXCEPTIONS=no"));
    }

    #[test]
    fn test_no_pic_disables_pic_and_cpp_symbols() {
        let settings = make_test_settings();
        let options = CrossEnvOptions {
            no_pic: true,
            ..default_options()
        };
        let script = generate_cross_env_script(&settings, &options);

        assert!(script.contains("export WASIXCC_PIC=no"));
        assert!(script.contains("export WASIXCC_INCLUDE_CPP_SYMBOLS=no"));
        // Must not also emit the conditional-assignment variants
        assert!(!script.contains("${WASIXCC_PIC"));
        assert!(!script.contains("${WASIXCC_INCLUDE_CPP_SYMBOLS"));
    }

    #[test]
    fn test_sysroot_exports_default() {
        // Default settings: Exnref EH, no PIC → sysroot-exnref-eh
        let settings = make_test_settings();
        let options = default_options();
        let script = generate_cross_env_script(&settings, &options);

        assert!(script.contains("export WASIXCC_SYSROOT_PREFIX=\"/home/user/.wasixcc/sysroot\""));
        assert!(
            script.contains(
                "export WASIXCC_SYSROOT=\"/home/user/.wasixcc/sysroot/sysroot-exnref-eh\""
            )
        );
    }

    #[test]
    fn test_sysroot_exports_no_exceptions() {
        // no_exceptions → WasmExceptionStyle::Off, no PIC → plain sysroot
        let settings = make_test_settings();
        let options = CrossEnvOptions {
            no_exceptions: true,
            ..default_options()
        };
        let script = generate_cross_env_script(&settings, &options);

        assert!(script.contains("export WASIXCC_SYSROOT=\"/home/user/.wasixcc/sysroot/sysroot\""));
    }

    #[test]
    fn test_sysroot_exports_explicit_sysroot_location() {
        // When sysroot_location is set explicitly it should be forwarded as-is
        let mut settings = make_test_settings();
        settings.sysroot_location = Some(PathBuf::from("/custom/sysroot"));
        let options = default_options();
        let script = generate_cross_env_script(&settings, &options);

        assert!(script.contains("export WASIXCC_SYSROOT=\"/custom/sysroot\""));
    }

    #[test]
    fn test_all_flags_disabled() {
        let settings = make_test_settings();
        let options = CrossEnvOptions {
            no_binfmt: true,
            no_hacks: true,
            no_exceptions: true,
            no_pic: true,
        };
        let script = generate_cross_env_script(&settings, &options);

        assert!(script.contains("export WASIXCC_DISCARD_UNSUPPORTED_FLAGS=no"));
        assert!(script.contains("export WASIXCC_AUTOCONF_WORKAROUNDS=no"));
        assert!(script.contains("export WASIXCC_PIC=no"));
        assert!(script.contains("export WASIXCC_INCLUDE_CPP_SYMBOLS=no"));
        assert!(script.contains("export WASIXCC_WASM_EXCEPTIONS=no"));
        assert!(!script.contains("WASMER_BIN"));
        assert!(!script.contains("binfmt"));
        assert!(script.contains("export CC=wasixcc"));
        assert!(script.contains("export PATH="));
    }

    #[test]
    fn test_script_checks_all_directories() {
        let settings = make_test_settings();
        let options = default_options();
        let script = generate_cross_env_script(&settings, &options);

        assert!(script.contains("wasixcc bin directory does not exist: $WASIXCC_BIN_LOCATION"));
        assert!(script.contains("LLVM bin directory does not exist: $WASIXCC_LLVM_LOCATION"));
        assert!(
            script.contains("binaryen bin directory does not exist: $WASIXCC_BINARYEN_LOCATION")
        );
        assert!(script.contains("libtool bin directory does not exist: $WASIXCC_LIBTOOL_LOCATION"));
    }

    #[test]
    fn test_script_without_libtool() {
        let mut settings = make_test_settings();
        settings.libtool_location = LibtoolLocation::DefaultPath(PathBuf::from("/nonexistent"));

        let options = default_options();
        let script = generate_cross_env_script(&settings, &options);

        assert!(!script.contains("ACLOCAL_PATH"));
        assert!(!script.contains("_lt_pkgdatadir"));
        assert!(script.contains("export CC=wasixcc"));
    }

    #[test]
    fn test_script_without_optional_toolchains() {
        let mut settings = make_test_settings();
        settings.llvm_location = LlvmLocation::DefaultPath(PathBuf::from("/nonexistent"));
        settings.binaryen_location = BinaryenLocation::DefaultPath(PathBuf::from("/nonexistent"));
        settings.libtool_location = LibtoolLocation::DefaultPath(PathBuf::from("/nonexistent"));

        let options = default_options();
        let script = generate_cross_env_script(&settings, &options);

        // Should still define WASIXCC_BIN_LOCATION
        assert!(script.contains("export WASIXCC_BIN_LOCATION=\"/home/user/.wasixcc/bin\""));
        // Should not define optional location variables
        assert!(!script.contains("WASIXCC_LLVM_LOCATION"));
        assert!(!script.contains("WASIXCC_BINARYEN_LOCATION"));
        assert!(!script.contains("WASIXCC_LIBTOOL_LOCATION"));
        // Should not reference /nonexistent
        assert!(!script.contains("/nonexistent"));
        // PATH should only include bin dir
        assert!(script.contains("export PATH=\"$WASIXCC_BIN_LOCATION:$PATH\""));
        assert!(script.contains("export CC=wasixcc"));
    }

    #[test]
    fn test_path_order() {
        let settings = make_test_settings();
        let options = default_options();
        let script = generate_cross_env_script(&settings, &options);

        let path_line = script
            .lines()
            .find(|line| line.starts_with("export PATH="))
            .expect("PATH export should exist");

        // Verify order: bin, llvm, binaryen, libtool
        let bin_pos = path_line.find("$WASIXCC_BIN_LOCATION").unwrap();
        let llvm_pos = path_line.find("$WASIXCC_LLVM_LOCATION").unwrap();
        let binaryen_pos = path_line.find("$WASIXCC_BINARYEN_LOCATION").unwrap();
        let libtool_pos = path_line.find("$WASIXCC_LIBTOOL_LOCATION").unwrap();

        assert!(bin_pos < llvm_pos);
        assert!(llvm_pos < binaryen_pos);
        assert!(binaryen_pos < libtool_pos);
    }

    #[test]
    fn test_binfmt_checks_wasmer_availability() {
        let settings = make_test_settings();
        let options = default_options();
        let script = generate_cross_env_script(&settings, &options);

        // Should resolve wasmer to a full path
        assert!(script.contains("WASMER_BIN=\"$(command -v wasmer 2>/dev/null)\""));
        assert!(script.contains("if test -z \"$WASMER_BIN\""));
        assert!(script.contains("wasmer not found in PATH"));

        // Should use $WASMER_BIN (full path) with sudo
        assert!(script.contains("\"$SUDO\" \"$WASMER_BIN\" binfmt reregister"));
        assert!(script.contains("\"$WASMER_BIN\" binfmt reregister"));

        let wasmer_check_section = script
            .split("if test -z \"$WASMER_BIN\"")
            .nth(1)
            .expect("wasmer check should exist");
        assert!(wasmer_check_section.contains("return 1"));
    }

    #[test]
    fn test_sudo_detection_logic() {
        let settings = make_test_settings();
        let options = default_options();
        let script = generate_cross_env_script(&settings, &options);

        assert!(script.contains("if test -z \"$SUDO\""));
        assert!(script.contains("test \"$(id -u)\" -ne 0"));
        assert!(script.contains("command -v sudo"));
    }

    #[test]
    fn test_error_messages_go_to_stderr() {
        let settings = make_test_settings();
        let options = default_options();
        let script = generate_cross_env_script(&settings, &options);

        for line in script.lines() {
            if line.contains("echo") && line.contains("Error:") {
                assert!(line.contains(">&2"), "Error message should go to stderr");
            }
        }
    }

    #[test]
    fn test_uses_return_not_exit() {
        let settings = make_test_settings();
        let options = default_options();
        let script = generate_cross_env_script(&settings, &options);

        assert!(script.contains("return 1"));
        assert!(!script.contains("exit 1"));
    }

    #[test]
    fn test_script_is_posix_compliant() {
        let settings = make_test_settings();
        let options = default_options();
        let script = generate_cross_env_script(&settings, &options);

        // Must use #!/bin/sh, not bash
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(!script.contains("#!/usr/bin/env bash"));
        assert!(!script.contains("#!/bin/bash"));

        // Must not use bash-only parameter expansion; ${VAR:=default} is allowed (POSIX)
        for line in script.lines() {
            if let Some(rest) = line.find("${").map(|i| &line[i + 2..]) {
                assert!(
                    rest.contains(":="),
                    "Bash-only parameter expansion found: {line}"
                );
            }
        }

        assert!(
            !script.contains("which "),
            "Use 'command -v' instead of 'which'"
        );
    }
}
