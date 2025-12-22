/// Parser for clang response files (also known as @files) and supporting
/// types for using them.
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub struct ArgumentsStack {
    stack: Vec<(std::vec::IntoIter<String>, Option<PathBuf>)>,
}

impl ArgumentsStack {
    pub fn new(cli_args: Vec<String>) -> Self {
        Self {
            stack: vec![(cli_args.into_iter(), None)],
        }
    }

    pub fn push_response_file(&mut self, file: impl AsRef<Path>) -> Result<()> {
        let path = std::fs::canonicalize(file.as_ref()).with_context(|| {
            format!(
                "Failed to canonicalize response file path: {}",
                file.as_ref().display()
            )
        })?;
        if self.stack.len() > 100 {
            return Err(anyhow::anyhow!(
                "Exceeded maximum response file nesting depth"
            ));
        }
        if self.stack.iter().any(|(_, p)| p.as_deref() == Some(&path)) {
            return Err(anyhow::anyhow!(
                "Cyclic response file inclusion detected: {}",
                path.display()
            ));
        }
        let args = parse_response_file(&path).context("Failed to parse response file")?;
        self.stack.push((args.into_iter(), Some(path)));
        Ok(())
    }

    fn next_inner(&mut self) -> Option<String> {
        while let Some(top) = self.stack.last_mut() {
            if let Some(arg) = top.0.next() {
                return Some(arg);
            } else {
                self.stack.pop();
            }
        }
        None
    }

    pub fn next(&mut self) -> Result<Option<String>> {
        let mut arg = self.next_inner();

        while let Some(response_file) = arg.as_mut().and_then(|a| a.strip_prefix('@')) {
            self.push_response_file(response_file)?;
            arg = self.next_inner();
        }

        Ok(arg)
    }
}

/// Parse a response file and return the list of arguments
pub fn parse_response_file(path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read response file: {}", path.display()))?;

    parse_response_content(&content)
        .with_context(|| format!("Failed to parse response file: {}", path.display()))
}

/// Parse response file content and return the list of arguments.
pub fn parse_response_content(content: &str) -> Result<Vec<String>> {
    let content = content.replace("\r\n", "\n"); // Handle Windows line continuations
    shell_words::split(&content).context("Failed to parse response file")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_arguments() {
        let content = "arg1 arg2 arg3";
        let args = parse_response_content(content).unwrap();
        assert_eq!(args, vec!["arg1", "arg2", "arg3"]);
    }

    #[test]
    fn test_quoted_arguments() {
        let content = r#"arg1 "arg with spaces" arg3"#;
        let args = parse_response_content(content).unwrap();
        assert_eq!(args, vec!["arg1", "arg with spaces", "arg3"]);
    }

    #[test]
    fn test_escaped_quotes() {
        let content = r#"arg1 "quote\"inside" arg3"#;
        let args = parse_response_content(content).unwrap();
        assert_eq!(args, vec!["arg1", "quote\"inside", "arg3"]);
    }

    #[test]
    fn test_escaped_backslash() {
        let content = r#"arg1 "path\\to\\file" arg3"#;
        let args = parse_response_content(content).unwrap();
        assert_eq!(args, vec!["arg1", "path\\to\\file", "arg3"]);
    }

    #[test]
    fn test_line_continuation() {
        let content = "arg1 \\\narg2 arg3";
        let args = parse_response_content(content).unwrap();
        assert_eq!(args, vec!["arg1", "arg2", "arg3"]);
    }

    #[test]
    fn test_line_continuation_windows() {
        let content = "arg1 \\\r\narg2 arg3";
        let args = parse_response_content(content).unwrap();
        assert_eq!(args, vec!["arg1", "arg2", "arg3"]);
    }

    #[test]
    fn test_multiple_whitespace() {
        let content = "arg1    \t\n  arg2\t\targ3";
        let args = parse_response_content(content).unwrap();
        assert_eq!(args, vec!["arg1", "arg2", "arg3"]);
    }

    #[test]
    fn test_empty_quotes() {
        let content = r#"arg1 "" arg3"#;
        let args = parse_response_content(content).unwrap();
        assert_eq!(args, vec!["arg1", "", "arg3"]);
    }

    #[test]
    fn test_mixed_quoted_unquoted() {
        let content = r#"prefix"quoted part"suffix"#;
        let args = parse_response_content(content).unwrap();
        assert_eq!(args, vec!["prefixquoted partsuffix"]);
    }

    #[test]
    fn test_single_backslash_dropped() {
        // Backslash before non-special characters is dropped
        let content = r#"\a \b \c"#;
        let args = parse_response_content(content).unwrap();
        assert_eq!(args, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_complex_example() {
        let content = r#"-I/path/to/include -D"SOME_DEFINE=value with spaces" -L"/usr/lib" -o "output file.o""#;
        let args = parse_response_content(content).unwrap();
        assert_eq!(
            args,
            vec![
                "-I/path/to/include",
                "-DSOME_DEFINE=value with spaces",
                "-L/usr/lib",
                "-o",
                "output file.o"
            ]
        );
    }

    #[test]
    fn test_newlines_in_file() {
        let content = "arg1\narg2\narg3";
        let args = parse_response_content(content).unwrap();
        assert_eq!(args, vec!["arg1", "arg2", "arg3"]);
    }

    #[test]
    fn test_trailing_backslash() {
        let content = r#"arg1\"#;
        let args = parse_response_content(content).unwrap();
        assert_eq!(args, vec!["arg1\\"]);
    }

    #[test]
    fn test_empty_content() {
        let content = "";
        let args = parse_response_content(content).unwrap();
        assert_eq!(args, Vec::<String>::new());
    }

    #[test]
    fn test_only_whitespace() {
        let content = "   \t\n  ";
        let args = parse_response_content(content).unwrap();
        assert_eq!(args, Vec::<String>::new());
    }

    // ArgumentsStack tests
    #[test]
    fn test_arguments_stack_basic() {
        let args = vec!["arg1".to_string(), "arg2".to_string(), "arg3".to_string()];
        let mut stack = ArgumentsStack::new(args);

        assert_eq!(stack.next().unwrap(), Some("arg1".to_string()));
        assert_eq!(stack.next().unwrap(), Some("arg2".to_string()));
        assert_eq!(stack.next().unwrap(), Some("arg3".to_string()));
        assert_eq!(stack.next().unwrap(), None);
    }

    #[test]
    fn test_arguments_stack_empty() {
        let mut stack = ArgumentsStack::new(vec![]);
        assert_eq!(stack.next().unwrap(), None);
    }

    #[test]
    fn test_arguments_stack_single_arg() {
        let mut stack = ArgumentsStack::new(vec!["single".to_string()]);
        assert_eq!(stack.next().unwrap(), Some("single".to_string()));
        assert_eq!(stack.next().unwrap(), None);
    }

    #[test]
    fn test_arguments_stack_with_response_file() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create a temporary response file
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "from_file1 from_file2").unwrap();
        let temp_path = temp_file.path();

        // Create stack with reference to response file
        let args = vec![
            "arg1".to_string(),
            format!("@{}", temp_path.display()),
            "arg2".to_string(),
        ];
        let mut stack = ArgumentsStack::new(args);

        assert_eq!(stack.next().unwrap(), Some("arg1".to_string()));
        assert_eq!(stack.next().unwrap(), Some("from_file1".to_string()));
        assert_eq!(stack.next().unwrap(), Some("from_file2".to_string()));
        assert_eq!(stack.next().unwrap(), Some("arg2".to_string()));
        assert_eq!(stack.next().unwrap(), None);
    }

    #[test]
    fn test_arguments_stack_multiple_response_files() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create two temporary response files
        let mut temp_file1 = NamedTempFile::new().unwrap();
        writeln!(temp_file1, "file1_arg1 file1_arg2").unwrap();
        let temp_path1 = temp_file1.path();

        let mut temp_file2 = NamedTempFile::new().unwrap();
        writeln!(temp_file2, "file2_arg1 file2_arg2").unwrap();
        let temp_path2 = temp_file2.path();

        let args = vec![
            "arg1".to_string(),
            format!("@{}", temp_path1.display()),
            "arg2".to_string(),
            format!("@{}", temp_path2.display()),
            "arg3".to_string(),
        ];
        let mut stack = ArgumentsStack::new(args);

        assert_eq!(stack.next().unwrap(), Some("arg1".to_string()));
        assert_eq!(stack.next().unwrap(), Some("file1_arg1".to_string()));
        assert_eq!(stack.next().unwrap(), Some("file1_arg2".to_string()));
        assert_eq!(stack.next().unwrap(), Some("arg2".to_string()));
        assert_eq!(stack.next().unwrap(), Some("file2_arg1".to_string()));
        assert_eq!(stack.next().unwrap(), Some("file2_arg2".to_string()));
        assert_eq!(stack.next().unwrap(), Some("arg3".to_string()));
        assert_eq!(stack.next().unwrap(), None);
    }

    #[test]
    fn test_arguments_stack_nested_response_files() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create nested response files
        let mut temp_file2 = NamedTempFile::new().unwrap();
        writeln!(temp_file2, "nested_arg1 nested_arg2").unwrap();
        let temp_path2 = temp_file2.path();

        let mut temp_file1 = NamedTempFile::new().unwrap();
        writeln!(
            temp_file1,
            "file1_arg1 @{} file1_arg2",
            temp_path2.display()
        )
        .unwrap();
        let temp_path1 = temp_file1.path();

        let args = vec![
            "arg1".to_string(),
            format!("@{}", temp_path1.display()),
            "arg2".to_string(),
        ];
        let mut stack = ArgumentsStack::new(args);

        assert_eq!(stack.next().unwrap(), Some("arg1".to_string()));
        assert_eq!(stack.next().unwrap(), Some("file1_arg1".to_string()));
        assert_eq!(stack.next().unwrap(), Some("nested_arg1".to_string()));
        assert_eq!(stack.next().unwrap(), Some("nested_arg2".to_string()));
        assert_eq!(stack.next().unwrap(), Some("file1_arg2".to_string()));
        assert_eq!(stack.next().unwrap(), Some("arg2".to_string()));
        assert_eq!(stack.next().unwrap(), None);
    }

    #[test]
    fn test_arguments_stack_response_file_with_quotes() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, r#""arg with spaces" -DDEFINE="value""#).unwrap();
        let temp_path = temp_file.path();

        let args = vec![
            "arg1".to_string(),
            format!("@{}", temp_path.display()),
            "arg2".to_string(),
        ];
        let mut stack = ArgumentsStack::new(args);

        assert_eq!(stack.next().unwrap(), Some("arg1".to_string()));
        assert_eq!(stack.next().unwrap(), Some("arg with spaces".to_string()));
        assert_eq!(stack.next().unwrap(), Some("-DDEFINE=value".to_string()));
        assert_eq!(stack.next().unwrap(), Some("arg2".to_string()));
        assert_eq!(stack.next().unwrap(), None);
    }

    #[test]
    fn test_arguments_stack_nonexistent_file() {
        let args = vec![
            "arg1".to_string(),
            "@/nonexistent/response/file.txt".to_string(),
            "arg2".to_string(),
        ];
        let mut stack = ArgumentsStack::new(args);

        assert_eq!(stack.next().unwrap(), Some("arg1".to_string()));
        // Should return error for nonexistent file
        assert!(stack.next().is_err());
    }

    #[test]
    fn test_arguments_stack_at_sign_without_file() {
        // @ at the end with no filename should be handled
        let mut stack = ArgumentsStack::new(vec!["@".to_string()]);
        // @ with empty path should fail when trying to read
        assert!(stack.next().is_err());
    }

    #[test]
    fn test_arguments_stack_literal_at_sign() {
        // Test that we can distinguish between @ as response file marker
        // vs @ in the middle of an argument
        let mut stack = ArgumentsStack::new(vec!["user@example.com".to_string()]);
        // This should work because @ is not at the start
        assert_eq!(stack.next().unwrap(), Some("user@example.com".to_string()));
        assert_eq!(stack.next().unwrap(), None);
    }

    #[test]
    fn test_arguments_stack_empty_response_file() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "").unwrap();
        let temp_path = temp_file.path();

        let args = vec![
            "arg1".to_string(),
            format!("@{}", temp_path.display()),
            "arg2".to_string(),
        ];
        let mut stack = ArgumentsStack::new(args);

        assert_eq!(stack.next().unwrap(), Some("arg1".to_string()));
        assert_eq!(stack.next().unwrap(), Some("arg2".to_string()));
        assert_eq!(stack.next().unwrap(), None);
    }

    #[test]
    fn test_arguments_stack_collect_all() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "from_file").unwrap();
        let temp_path = temp_file.path();

        let args = vec![
            "arg1".to_string(),
            format!("@{}", temp_path.display()),
            "arg2".to_string(),
        ];
        let mut stack = ArgumentsStack::new(args);

        let mut collected = Vec::new();
        while let Some(arg) = stack.next().unwrap() {
            collected.push(arg);
        }

        assert_eq!(
            collected,
            vec![
                "arg1".to_string(),
                "from_file".to_string(),
                "arg2".to_string()
            ]
        );
    }

    #[test]
    fn test_arguments_stack_cyclic_inclusion_direct() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create a response file that references itself
        let mut temp_file = NamedTempFile::new().unwrap();
        let temp_path = temp_file.path().to_owned();
        writeln!(temp_file, "arg1 @{} arg2", temp_path.display()).unwrap();

        let args = vec![format!("@{}", temp_path.display())];
        let mut stack = ArgumentsStack::new(args);

        // First arg should succeed
        assert_eq!(stack.next().unwrap(), Some("arg1".to_string()));
        // Second attempt to include same file should fail with cyclic error
        let result = stack.next();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Cyclic") || err_msg.contains("cyclic"));
    }

    #[test]
    fn test_arguments_stack_cyclic_inclusion_indirect() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create a two-file cycle: A -> B -> A
        let mut temp_file_b = NamedTempFile::new().unwrap();
        let temp_path_b = temp_file_b.path().to_owned();

        let mut temp_file_a = NamedTempFile::new().unwrap();
        let temp_path_a = temp_file_a.path().to_owned();

        // File A references File B
        writeln!(temp_file_a, "arg_a @{}", temp_path_b.display()).unwrap();
        temp_file_a.flush().unwrap();

        // File B references File A (creating a cycle)
        writeln!(temp_file_b, "arg_b @{}", temp_path_a.display()).unwrap();
        temp_file_b.flush().unwrap();

        let args = vec![format!("@{}", temp_path_a.display())];
        let mut stack = ArgumentsStack::new(args);

        // Should get arg_a
        assert_eq!(stack.next().unwrap(), Some("arg_a".to_string()));
        // Should get arg_b
        assert_eq!(stack.next().unwrap(), Some("arg_b".to_string()));
        // Should fail on cyclic inclusion of file A
        let result = stack.next();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Cyclic") || err_msg.contains("cyclic"));
    }

    #[test]
    fn test_arguments_stack_max_depth() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create a chain of response files that exceeds the depth limit
        let mut files = Vec::new();
        let mut paths = Vec::new();

        // Create 102 files (exceeding the limit of 100)
        for _ in 0..102 {
            let temp_file = NamedTempFile::new().unwrap();
            paths.push(temp_file.path().to_owned());
            files.push(temp_file);
        }

        // Write each file to reference the next one
        for i in 0..101 {
            writeln!(&mut files[i], "arg{} @{}", i, paths[i + 1].display()).unwrap();
            files[i].flush().unwrap();
        }
        writeln!(&mut files[101], "final_arg").unwrap();
        files[101].flush().unwrap();

        let args = vec![format!("@{}", paths[0].display())];
        let mut stack = ArgumentsStack::new(args);

        // Consume arguments until we hit the depth limit
        let mut count = 0;
        let mut hit_error = false;
        loop {
            match stack.next() {
                Ok(Some(_)) => count += 1,
                Ok(None) => break,
                Err(e) => {
                    let err_msg = e.to_string();
                    assert!(
                        err_msg.contains("depth") || err_msg.contains("nesting"),
                        "Expected depth/nesting error, got: {}",
                        err_msg
                    );
                    hit_error = true;
                    break;
                }
            }
        }

        assert!(hit_error, "Expected to hit max depth error");
        assert!(count < 102, "Should not have processed all 102 files");
    }

    #[test]
    fn test_arguments_stack_deep_nesting_within_limit() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create a chain of 10 response files (well within the limit)
        let mut files = Vec::new();
        let mut paths = Vec::new();

        for _ in 0..10 {
            let temp_file = NamedTempFile::new().unwrap();
            paths.push(temp_file.path().to_owned());
            files.push(temp_file);
        }

        // Write each file to reference the next one
        for i in 0..9 {
            writeln!(&mut files[i], "arg{} @{}", i, paths[i + 1].display()).unwrap();
            files[i].flush().unwrap();
        }
        writeln!(&mut files[9], "final_arg").unwrap();
        files[9].flush().unwrap();

        let args = vec![format!("@{}", paths[0].display())];
        let mut stack = ArgumentsStack::new(args);

        // Should successfully process all arguments
        let mut collected = Vec::new();
        while let Some(arg) = stack.next().unwrap() {
            collected.push(arg);
        }

        assert_eq!(collected.len(), 10); // arg0 through arg8, plus final_arg
        assert_eq!(collected[0], "arg0");
        assert_eq!(collected[8], "arg8");
        assert_eq!(collected[9], "final_arg");
    }

    #[test]
    fn test_arguments_stack_same_file_in_different_branches() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create a shared response file
        let mut shared_file = NamedTempFile::new().unwrap();
        let shared_path = shared_file.path().to_owned();
        writeln!(shared_file, "shared_arg").unwrap();

        // Create two files that both include the shared file (but not cyclically)
        let mut file_a = NamedTempFile::new().unwrap();
        let path_a = file_a.path().to_owned();
        writeln!(file_a, "arg_a @{}", shared_path.display()).unwrap();

        let mut file_b = NamedTempFile::new().unwrap();
        let path_b = file_b.path().to_owned();
        writeln!(file_b, "arg_b @{}", shared_path.display()).unwrap();

        // Include both file_a and file_b from the command line
        let args = vec![
            format!("@{}", path_a.display()),
            format!("@{}", path_b.display()),
        ];
        let mut stack = ArgumentsStack::new(args);

        // Should successfully process all arguments
        let mut collected = Vec::new();
        while let Some(arg) = stack.next().unwrap() {
            collected.push(arg);
        }

        // Both branches should be able to include the shared file
        assert_eq!(
            collected,
            vec![
                "arg_a".to_string(),
                "shared_arg".to_string(),
                "arg_b".to_string(),
                "shared_arg".to_string()
            ]
        );
    }

    #[test]
    fn test_arguments_stack_cyclic_three_way() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create a three-way cycle: A -> B -> C -> A
        let mut file_c = NamedTempFile::new().unwrap();
        let path_c = file_c.path().to_owned();

        let mut file_b = NamedTempFile::new().unwrap();
        let path_b = file_b.path().to_owned();

        let mut file_a = NamedTempFile::new().unwrap();
        let path_a = file_a.path().to_owned();

        writeln!(file_a, "arg_a @{}", path_b.display()).unwrap();
        file_a.flush().unwrap();
        writeln!(file_b, "arg_b @{}", path_c.display()).unwrap();
        file_b.flush().unwrap();
        writeln!(file_c, "arg_c @{}", path_a.display()).unwrap();
        file_c.flush().unwrap();

        let args = vec![format!("@{}", path_a.display())];
        let mut stack = ArgumentsStack::new(args);

        // Should process arg_a, arg_b, arg_c, then fail on cycle
        assert_eq!(stack.next().unwrap(), Some("arg_a".to_string()));
        assert_eq!(stack.next().unwrap(), Some("arg_b".to_string()));
        assert_eq!(stack.next().unwrap(), Some("arg_c".to_string()));

        let result = stack.next();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Cyclic") || err_msg.contains("cyclic"));
    }
}
