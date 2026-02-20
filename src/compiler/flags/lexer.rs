use anyhow::{Result, bail};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy)]
pub(super) enum Separator {
    // Attached with no separator, e.g. -Ifoo
    None,
    // Separated by a space
    Space,
    // Attached with an equal sign, e.g. -I=foo
    Equals,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Flag<'a> {
    Simple(&'a str),
    WithValue(&'a str, &'a str, Separator),
    Positional(&'a str),
    Terminator(),
}

impl Flag<'_> {
    /// Convert this flag back to arguments
    pub(super) fn to_args(self) -> Vec<String> {
        match self {
            Flag::Simple(arg) => vec![arg.to_string()],
            Flag::WithValue(arg, value, Separator::None) => vec![format!("{}{}", arg, value)],
            Flag::WithValue(arg, value, Separator::Space) => {
                vec![arg.to_string(), value.to_string()]
            }
            Flag::WithValue(arg, value, Separator::Equals) => {
                vec![format!("{}={}", arg, value)]
            }
            Flag::Positional(value) => vec![value.to_string()],
            Flag::Terminator() => vec!["--".to_string()],
        }
    }
}
impl<'a> std::fmt::Display for Flag<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Flag::Simple(arg) => write!(f, "{}", arg),
            Flag::WithValue(arg, value, Separator::Space) => {
                write!(f, "{} {}", arg, value)
            }
            Flag::WithValue(arg, value, Separator::None) => {
                write!(f, "{}{}", arg, value)
            }
            Flag::WithValue(arg, value, Separator::Equals) => {
                write!(f, "{}={}", arg, value)
            }
            Flag::Positional(value) => write!(f, "{}", value),
            Flag::Terminator() => write!(f, "--"),
        }
    }
}

pub(super) fn lex_args<'a>(
    args: impl IntoIterator<Item = &'a (impl AsRef<str> + 'a)>,
    // Args that capture the next argument as their value, e.g. -I
    // They can also be used with an equals sign, e.g. -I=foo
    // If the flag is a dash followed by a single character, it can also be used with no separator, e.g. -Ifoo
    flags_with_value: &HashSet<&str>,
    // Flags with a value that is optional. They DO NOT capture the next argument as their value
    // They can be used with an equals sign, e.g. -I=foo
    // If the flag is a dash followed by a single character, it can also be used with no separator, e.g. -Ifoo
    flags_with_optional_value: &HashSet<&str>,
) -> Result<Vec<Flag<'a>>> {
    enum State<'a> {
        Normal,
        ArgWithValue(&'a str),
        Terminated,
    }

    let mut state = State::Normal;
    let mut flags = Vec::new();

    for arg in args {
        state = match state {
            State::Normal => match arg.as_ref() {
                "--" => {
                    flags.push(Flag::Terminator());
                    State::Terminated
                }
                arg if flags_with_value.contains(arg) => {
                    // This is not perfect since some flags have an optional optional arg
                    State::ArgWithValue(arg)
                }
                arg if flags_with_optional_value.contains(arg) => {
                    flags.push(Flag::Simple(arg));
                    State::Normal
                }
                arg if flags_with_value
                    .iter()
                    .chain(flags_with_optional_value.iter())
                    .any(|flag| flag.len() == 2 && arg.starts_with(flag)) =>
                {
                    let flag = &arg[..2];
                    let value = &arg[2..];
                    flags.push(Flag::WithValue(flag, value, Separator::None));
                    State::Normal
                }
                arg if flags_with_value
                    .iter()
                    .chain(flags_with_optional_value.iter())
                    .any(|flag| {
                        arg.starts_with(flag) && (arg.chars().nth(flag.len()) == Some('='))
                    }) =>
                {
                    let (flag, value) = arg.split_once('=').unwrap();
                    flags.push(Flag::WithValue(flag, value, Separator::Equals));
                    State::Normal
                }
                arg if arg.starts_with("-") => {
                    flags.push(Flag::Simple(arg));
                    State::Normal
                }
                arg => {
                    flags.push(Flag::Positional(arg));
                    State::Normal
                }
            },
            State::ArgWithValue(flag) => {
                flags.push(Flag::WithValue(flag, arg.as_ref(), Separator::Space));
                State::Normal
            }
            State::Terminated => {
                flags.push(Flag::Positional(arg.as_ref()));
                State::Terminated
            }
        }
    }

    match state {
        State::ArgWithValue(flag) => {
            bail!("Expected argument after {}", flag);
        }
        State::Normal | State::Terminated => Ok(flags),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(flags: &[&'static str]) -> HashSet<&'static str> {
        flags.iter().copied().collect()
    }

    fn empty() -> HashSet<&'static str> {
        HashSet::new()
    }

    // Empty input produces an empty flag list.
    #[test]
    fn test_empty_input() {
        let empty_args: Vec<String> = vec![];
        let result = lex_args(&empty_args, &empty(), &empty()).unwrap();
        assert!(result.is_empty());
    }

    // A plain word with no leading `-` is emitted as Positional.
    #[test]
    fn test_positional_arg() {
        let args = vec!["file.c".to_string()];
        let result = lex_args(&args, &empty(), &empty()).unwrap();
        assert!(matches!(result[..], [Flag::Positional("file.c")]));
    }

    // An unknown flag (starts with `-`, not in either set) is emitted as Simple.
    #[test]
    fn test_unknown_flag_is_simple() {
        let args = vec!["-O2".to_string()];
        let result = lex_args(&args, &empty(), &empty()).unwrap();
        assert!(matches!(result[..], [Flag::Simple("-O2")]));
    }

    // A flag in flags_with_value followed by a separate value produces WithValue(_, _, Space).
    #[test]
    fn test_flags_with_value_space_separated() {
        let args = vec!["-I".to_string(), "/usr/include".to_string()];
        let result = lex_args(&args, &set(&["-I"]), &empty()).unwrap();
        assert!(matches!(
            result[..],
            [Flag::WithValue("-I", "/usr/include", Separator::Space)]
        ));
    }

    // A flags_with_value flag at end of input (no following value) is an error.
    #[test]
    fn test_flags_with_value_missing_value_is_error() {
        let args = vec!["-I".to_string()];
        let err = lex_args(&args, &set(&["-I"]), &empty()).unwrap_err();
        assert!(err.to_string().contains("Expected argument after -I"));
    }

    // A flags_with_optional_value flag on its own is emitted as Simple and does NOT
    // consume the next argument as its value.
    #[test]
    fn test_flags_with_optional_value_standalone_does_not_consume_next() {
        let args = vec!["-g".to_string(), "file.c".to_string()];
        let result = lex_args(&args, &empty(), &set(&["-g"])).unwrap();
        assert!(matches!(
            result[..],
            [Flag::Simple("-g"), Flag::Positional("file.c")]
        ));
    }

    // A 2-char flags_with_value prefix with an attached value produces WithValue(_, _, None).
    #[test]
    fn test_flags_with_value_two_char_no_separator() {
        let args = vec!["-Ifoo".to_string()];
        let result = lex_args(&args, &set(&["-I"]), &empty()).unwrap();
        assert!(matches!(
            result[..],
            [Flag::WithValue("-I", "foo", Separator::None)]
        ));
    }

    // A 2-char flags_with_optional_value prefix with an attached value also produces
    // WithValue(_, _, None).
    #[test]
    fn test_flags_with_optional_value_two_char_no_separator() {
        let args = vec!["-g3".to_string()];
        let result = lex_args(&args, &empty(), &set(&["-g"])).unwrap();
        assert!(matches!(
            result[..],
            [Flag::WithValue("-g", "3", Separator::None)]
        ));
    }

    // A long (>2 char) flags_with_value flag joined with `=` produces WithValue(_, _, Equals).
    #[test]
    fn test_flags_with_value_equals_separator() {
        let args = vec!["--sysroot=/usr".to_string()];
        let result = lex_args(&args, &set(&["--sysroot"]), &empty()).unwrap();
        assert!(matches!(
            result[..],
            [Flag::WithValue("--sysroot", "/usr", Separator::Equals)]
        ));
    }

    // A long flags_with_optional_value flag joined with `=` also produces WithValue(_, _, Equals).
    #[test]
    fn test_flags_with_optional_value_equals_separator() {
        let args = vec!["--target=wasm32".to_string()];
        let result = lex_args(&args, &empty(), &set(&["--target"])).unwrap();
        assert!(matches!(
            result[..],
            [Flag::WithValue("--target", "wasm32", Separator::Equals)]
        ));
    }

    // `--` emits a Terminator; all following args become Positional, even flags-with-value
    // (which do NOT consume the arg after them).
    #[test]
    fn test_terminator_makes_rest_positional() {
        let args = vec!["--".to_string(), "-I".to_string(), "file.c".to_string()];
        let result = lex_args(&args, &set(&["-I"]), &empty()).unwrap();
        assert!(matches!(
            result[..],
            [
                Flag::Terminator(),
                Flag::Positional("-I"),
                Flag::Positional("file.c"),
            ]
        ));
    }

    // The value that follows a flags_with_value flag may itself look like a flag.
    #[test]
    fn test_value_may_look_like_a_flag() {
        let args = vec!["-x".to_string(), "-foo".to_string()];
        let result = lex_args(&args, &set(&["-x"]), &empty()).unwrap();
        assert!(matches!(
            result[..],
            [Flag::WithValue("-x", "-foo", Separator::Space)]
        ));
    }

    // Mixed argument kinds are emitted in the correct order.
    #[test]
    fn test_multiple_mixed_args() {
        let args = vec![
            "-I".to_string(),
            "/inc".to_string(),
            "-O2".to_string(),
            "main.c".to_string(),
        ];
        let result = lex_args(&args, &set(&["-I"]), &empty()).unwrap();
        assert!(matches!(
            result[..],
            [
                Flag::WithValue("-I", "/inc", Separator::Space),
                Flag::Simple("-O2"),
                Flag::Positional("main.c"),
            ]
        ));
    }

    // A long flags_with_value flag is consumed via the exact-match arm (Space), not the
    // 2-char prefix arm.
    #[test]
    fn test_long_flag_with_value_not_matched_as_two_char_prefix() {
        let args = vec!["--output".to_string(), "a.out".to_string()];
        let result = lex_args(&args, &set(&["--output"]), &empty()).unwrap();
        assert!(matches!(
            result[..],
            [Flag::WithValue("--output", "a.out", Separator::Space)]
        ));
    }

    // A flags_with_value flag that is a long prefix of another arg but without `=`
    // falls through to Simple (no splitting occurs).
    #[test]
    fn test_long_flag_prefix_without_equals_is_simple() {
        let args = vec!["--sysroot-extra".to_string()];
        let result = lex_args(&args, &set(&["--sysroot"]), &empty()).unwrap();
        assert!(matches!(result[..], [Flag::Simple("--sysroot-extra")]));
    }
}
