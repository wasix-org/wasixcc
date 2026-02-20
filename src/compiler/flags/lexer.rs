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
    flags_with_value: &HashSet<&str>,
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
