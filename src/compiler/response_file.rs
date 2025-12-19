/// Parser for clang response files (also known as @files) and supporting
/// types for using them.
use anyhow::{Context, Result};
use std::path::Path;

pub struct ArgumentsStack {
    stack: Vec<std::vec::IntoIter<String>>,
}

impl ArgumentsStack {
    pub fn new(cli_args: Vec<String>) -> Self {
        Self {
            stack: vec![cli_args.into_iter()],
        }
    }

    pub fn push_response_file(&mut self, file: impl AsRef<Path>) -> Result<()> {
        let args = parse_response_file(file.as_ref()).context("Failed to parse response file")?;
        self.stack.push(args.into_iter());
        Ok(())
    }

    fn next_inner(&mut self) -> Option<String> {
        while let Some(top) = self.stack.last_mut() {
            if let Some(arg) = top.next() {
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

    Ok(parse_response_content(&content))
}

/// Parse response file content and return the list of arguments
pub fn parse_response_content(content: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut chars = content.chars().peekable();

    while chars.peek().is_some() {
        // Skip whitespace
        while chars.peek().map_or(false, |c| c.is_whitespace()) {
            chars.next();
        }

        if chars.peek().is_none() {
            break;
        }

        // Parse one argument
        if let Some(arg) = parse_argument(&mut chars) {
            args.push(arg);
        }
    }

    args
}

/// Parse a single argument from the character stream
fn parse_argument(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<String> {
    let mut arg = String::new();
    let mut in_quotes = false;

    while let Some(&ch) = chars.peek() {
        match ch {
            // Whitespace ends the argument if not quoted
            c if c.is_whitespace() && !in_quotes => {
                break;
            }

            // Double quote toggles quoted mode
            '"' => {
                chars.next();
                in_quotes = !in_quotes;
            }

            // Backslash for escape sequences
            '\\' => {
                chars.next();
                if let Some(&next_ch) = chars.peek() {
                    match next_ch {
                        // Backslash-newline is a line continuation (skip both)
                        '\n' => {
                            chars.next();
                        }
                        // Backslash-r-newline (Windows line ending continuation)
                        '\r' => {
                            chars.next();
                            if chars.peek() == Some(&'\n') {
                                chars.next();
                            }
                        }
                        // Escaped double quote
                        '"' => {
                            chars.next();
                            arg.push('"');
                        }
                        // Escaped backslash
                        '\\' => {
                            chars.next();
                            arg.push('\\');
                        }
                        // Any other character after backslash is preserved literally
                        // This matches clang behavior where \x becomes \x unless x is special
                        _ => {
                            arg.push('\\');
                        }
                    }
                } else {
                    // Trailing backslash
                    arg.push('\\');
                }
            }

            // Regular character
            _ => {
                chars.next();
                arg.push(ch);
            }
        }
    }

    if !arg.is_empty() {
        Some(arg)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_arguments() {
        let content = "arg1 arg2 arg3";
        let args = parse_response_content(content);
        assert_eq!(args, vec!["arg1", "arg2", "arg3"]);
    }

    #[test]
    fn test_quoted_arguments() {
        let content = r#"arg1 "arg with spaces" arg3"#;
        let args = parse_response_content(content);
        assert_eq!(args, vec!["arg1", "arg with spaces", "arg3"]);
    }

    #[test]
    fn test_escaped_quotes() {
        let content = r#"arg1 "quote\"inside" arg3"#;
        let args = parse_response_content(content);
        assert_eq!(args, vec!["arg1", "quote\"inside", "arg3"]);
    }

    #[test]
    fn test_escaped_backslash() {
        let content = r#"arg1 "path\\to\\file" arg3"#;
        let args = parse_response_content(content);
        assert_eq!(args, vec!["arg1", "path\\to\\file", "arg3"]);
    }

    #[test]
    fn test_line_continuation() {
        let content = "arg1 \\\narg2 arg3";
        let args = parse_response_content(content);
        assert_eq!(args, vec!["arg1", "arg2", "arg3"]);
    }

    #[test]
    fn test_line_continuation_windows() {
        let content = "arg1 \\\r\narg2 arg3";
        let args = parse_response_content(content);
        assert_eq!(args, vec!["arg1", "arg2", "arg3"]);
    }

    #[test]
    fn test_multiple_whitespace() {
        let content = "arg1    \t\n  arg2\t\targ3";
        let args = parse_response_content(content);
        assert_eq!(args, vec!["arg1", "arg2", "arg3"]);
    }

    #[test]
    fn test_empty_quotes() {
        let content = r#"arg1 "" arg3"#;
        let args = parse_response_content(content);
        assert_eq!(args, vec!["arg1", "arg3"]);
    }

    #[test]
    fn test_mixed_quoted_unquoted() {
        let content = r#"prefix"quoted part"suffix"#;
        let args = parse_response_content(content);
        assert_eq!(args, vec!["prefixquoted partsuffix"]);
    }

    #[test]
    fn test_backslash_preserving() {
        // Backslash before non-special characters is preserved
        let content = r#"\a \b \c"#;
        let args = parse_response_content(content);
        assert_eq!(args, vec!["\\a", "\\b", "\\c"]);
    }

    #[test]
    fn test_complex_example() {
        let content = r#"-I/path/to/include -D"SOME_DEFINE=value with spaces" -L"/usr/lib" -o "output file.o""#;
        let args = parse_response_content(content);
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
        let args = parse_response_content(content);
        assert_eq!(args, vec!["arg1", "arg2", "arg3"]);
    }

    #[test]
    fn test_trailing_backslash() {
        let content = r#"arg1\"#;
        let args = parse_response_content(content);
        assert_eq!(args, vec!["arg1\\"]);
    }

    #[test]
    fn test_empty_content() {
        let content = "";
        let args = parse_response_content(content);
        assert_eq!(args, Vec::<String>::new());
    }

    #[test]
    fn test_only_whitespace() {
        let content = "   \t\n  ";
        let args = parse_response_content(content);
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
}
