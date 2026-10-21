//! Argument parsing for the `claude-tui` binary.
//!
//! The grammar is small enough to hand-roll (SPEC Phase E), so there is no
//! argument-parsing dependency. Every form not listed here is an error: the
//! binary prints [`USAGE`] and exits with code 2.

/// Printed for an unknown invocation.
pub const USAGE: &str =
    "usage: claude-tui [--demo]\n       claude-tui status --line\n       claude-tui status --json";

/// What the binary was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Run the TUI. `demo` replaces daemon data with mock data (AC-11).
    Tui { demo: bool },
    /// One line for sketchybar (AC-10).
    StatusLine,
    /// The versioned JSON document the Pi extensions read (AC-18).
    StatusJson,
}

/// Parse the arguments after the program name.
///
/// Returns [`USAGE`] as the error message so the caller can print it unchanged.
pub fn parse_args(args: &[String]) -> Result<Command, String> {
    match args {
        [] => Ok(Command::Tui { demo: false }),
        [only] if only == "--demo" => Ok(Command::Tui { demo: true }),
        [sub, flag] if sub == "status" && flag == "--line" => Ok(Command::StatusLine),
        [sub, flag] if sub == "status" && flag == "--json" => Ok(Command::StatusJson),
        _ => Err(USAGE.to_string()),
    }
}

/// Read the process arguments and parse them.
///
/// Non-UTF-8 arguments are an unknown argument, not a reason to panic.
pub fn command_from_env() -> Result<Command, String> {
    let mut args = Vec::new();
    for arg in std::env::args_os().skip(1) {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(_) => return Err(USAGE.to_string()),
        }
    }
    parse_args(&args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_string()).collect()
    }

    #[test]
    fn cli_parses_the_tui_forms() {
        assert_eq!(parse_args(&args(&[])), Ok(Command::Tui { demo: false }));
        assert_eq!(
            parse_args(&args(&["--demo"])),
            Ok(Command::Tui { demo: true })
        );
    }

    #[test]
    fn cli_parses_the_status_forms() {
        assert_eq!(
            parse_args(&args(&["status", "--line"])),
            Ok(Command::StatusLine)
        );
        assert_eq!(
            parse_args(&args(&["status", "--json"])),
            Ok(Command::StatusJson)
        );
    }

    #[test]
    fn cli_rejects_unknown_arguments_with_usage() {
        for bad in [
            vec!["--help"],
            vec!["status"],
            vec!["status", "--yaml"],
            vec!["--demo", "status", "--line"],
            vec!["demo"],
        ] {
            assert_eq!(parse_args(&args(&bad)), Err(USAGE.to_string()), "{bad:?}");
        }
    }
}
