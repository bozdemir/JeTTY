//! The command line: what `jetty …` asks for. Pure — `run` acts on it.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::ipc::TabRequest;

/// What a launch's arguments ask for.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Cli {
    /// `--version`
    Version,
    /// `--help`
    Help,
    /// `--check-config`
    CheckConfig,
    /// `--print-shell-integration <shell>`, with the shell named (if any).
    PrintShellIntegration(Option<String>),
    /// Start JeTTY, or reach the running one, with `verb`: `toggle`, `show`,
    /// `hide`, `background` — or `new-tab`, with the tab to open.
    Launch { verb: &'static str, tab: Option<TabRequest> },
}

/// Read `args` (the program name left out). `cwd` is the launch's current
/// directory: what a relative `--cwd` is relative to, and where a new tab
/// starts without one. Everything after `-e` or `--` is the command's — none
/// of it is ours; any other argument JeTTY doesn't know is ignored, as it
/// always was. A tab option (`--new-tab`, `--cwd`, `--working-directory`, a
/// command) makes the launch a `new-tab`. The error is a usage error, to
/// print (exit status 2).
pub(crate) fn parse(args: &[OsString], cwd: Option<&Path>) -> Result<Cli, String> {
    let split = args.iter().position(|a| a == "-e" || a == "--");
    let (ours, command) = match split {
        Some(i) => (&args[..i], Some((args[i].as_os_str(), &args[i + 1..]))),
        None => (args, None),
    };
    // First, as it always was: it takes the argument after it.
    if let Some(pos) = ours.iter().position(|a| a == "--print-shell-integration") {
        return Ok(Cli::PrintShellIntegration(ours.get(pos + 1).and_then(|a| a.to_str()).map(str::to_owned)));
    }
    let mut verb = None;
    let mut new_tab = false;
    // The directory option as given (its name, for an error) and its value.
    let mut dir: Option<(&str, &OsStr)> = None;
    let mut it = ours.iter();
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("--version" | "-version" | "-V" | "version") => return Ok(Cli::Version),
            Some("--help" | "-help" | "-h" | "help") => return Ok(Cli::Help),
            Some("--check-config") => return Ok(Cli::CheckConfig),
            Some("--toggle") => verb = Some("toggle"),
            Some("--show") => verb = Some("show"),
            Some("--hide") => verb = Some("hide"),
            Some("--background") => verb = Some("background"),
            Some("--new-tab") => new_tab = true,
            Some(flag @ ("--cwd" | "--working-directory")) => {
                let value = it.next().ok_or_else(|| format!("{flag} needs a directory"))?;
                dir = Some((flag, value));
            }
            // `--cwd=DIR` / `--working-directory=DIR` (xdg-terminal-exec's
            // form): the directory may be any bytes.
            _ => {
                for flag in ["--cwd", "--working-directory"] {
                    let value = arg.as_bytes().strip_prefix(flag.as_bytes()).and_then(|v| v.strip_prefix(b"="));
                    if let Some(value) = value {
                        dir = Some((flag, OsStr::from_bytes(value)));
                    }
                }
            }
        }
    }
    let argv: Vec<OsString> = command.map_or_else(Vec::new, |(_, rest)| rest.to_vec());
    if command.is_some_and(|(sep, _)| sep == "-e") && argv.is_empty() {
        return Err("-e needs a command to run".to_string());
    }
    if !new_tab && dir.is_none() && argv.is_empty() {
        return Ok(Cli::Launch { verb: verb.unwrap_or("toggle"), tab: None });
    }
    if let Some(v @ ("hide" | "background")) = verb {
        return Err(format!("--{v} can't open a tab (--new-tab, --cwd, -e)"));
    }
    let cwd = match dir {
        Some((flag, d)) if d.is_empty() => return Err(format!("{flag} needs a directory")),
        Some((_, d)) => Some(absolute(Path::new(d), cwd)?),
        None => cwd.map(Path::to_path_buf),
    };
    Ok(Cli::Launch { verb: "new-tab", tab: Some(TabRequest { cwd, argv }) })
}

/// `dir` made absolute against the launch's directory `cwd` — nothing else
/// resolved: the running JeTTY reads it from elsewhere.
fn absolute(dir: &Path, cwd: Option<&Path>) -> Result<PathBuf, String> {
    if dir.is_absolute() {
        return Ok(dir.to_path_buf());
    }
    cwd.map(|c| c.join(dir))
        .ok_or_else(|| format!("{}: relative, and the current directory can't be read", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/u";

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn parse_in(list: &[&str]) -> Result<Cli, String> {
        parse(&args(list), Some(Path::new(HOME)))
    }

    fn launch(verb: &'static str) -> Result<Cli, String> {
        Ok(Cli::Launch { verb, tab: None })
    }

    fn tab(cwd: &str, argv: &[&str]) -> Result<Cli, String> {
        Ok(Cli::Launch {
            verb: "new-tab",
            tab: Some(TabRequest { cwd: Some(PathBuf::from(cwd)), argv: args(argv) }),
        })
    }

    #[test]
    fn the_summon_flags_are_as_they_were() {
        assert_eq!(parse_in(&[]), launch("toggle"));
        assert_eq!(parse_in(&["--show"]), launch("show"));
        assert_eq!(parse_in(&["--hide"]), launch("hide"));
        assert_eq!(parse_in(&["--background"]), launch("background"));
        assert_eq!(parse_in(&["--toggle", "--show"]), launch("show"), "the last one wins");
        assert_eq!(parse_in(&["--frobnicate", "x"]), launch("toggle"), "unknown arguments are ignored");
        assert_eq!(parse_in(&["--"]), launch("toggle"), "nothing to run");
        assert_eq!(parse_in(&["-V"]), Ok(Cli::Version));
        assert_eq!(parse_in(&["--show", "--help", "--version"]), Ok(Cli::Help), "the first of them");
        assert_eq!(parse_in(&["--check-config"]), Ok(Cli::CheckConfig));
        assert_eq!(parse_in(&["--print-shell-integration", "zsh"]), Ok(Cli::PrintShellIntegration(Some("zsh".into()))));
        assert_eq!(parse_in(&["--version", "--print-shell-integration"]), Ok(Cli::PrintShellIntegration(None)));
    }

    #[test]
    fn a_new_tab_starts_here_or_where_asked() {
        assert_eq!(parse_in(&["--new-tab"]), tab(HOME, &[]));
        assert_eq!(parse_in(&["--new-tab", "--cwd", "/srv/x"]), tab("/srv/x", &[]));
        assert_eq!(parse_in(&["--cwd", "proj/a"]), tab("/home/u/proj/a", &[]), "relative to the launch; implies --new-tab");
        assert_eq!(parse_in(&["--cwd=/srv/x"]), tab("/srv/x", &[]));
        assert_eq!(parse_in(&["--working-directory", "/srv/x"]), tab("/srv/x", &[]));
        // xdg-terminal-exec's `X-TerminalArgDir=--working-directory=`.
        assert_eq!(parse_in(&["--working-directory=/srv/x", "--", "htop"]), tab("/srv/x", &["htop"]));
        assert_eq!(parse_in(&["--show", "--new-tab"]), tab(HOME, &[]), "a new tab is summoned anyway");
        // Unknown to the launch: the default place.
        assert_eq!(
            parse(&args(&["--new-tab"]), None),
            Ok(Cli::Launch { verb: "new-tab", tab: Some(TabRequest::default()) })
        );
        assert_eq!(parse(&args(&["--cwd", "/srv"]), None), tab("/srv", &[]));
        assert!(parse(&args(&["--cwd", "rel"]), None).is_err());
    }

    #[test]
    fn everything_after_the_command_flag_is_the_commands() {
        assert_eq!(parse_in(&["-e", "htop", "-d", "10"]), tab(HOME, &["htop", "-d", "10"]));
        assert_eq!(parse_in(&["--new-tab", "--", "htop"]), tab(HOME, &["htop"]));
        assert_eq!(parse_in(&["--", "vim", "--help"]), tab(HOME, &["vim", "--help"]), "not JeTTY's help");
        assert_eq!(parse_in(&["-e", "vim", "--version", "-e", "x"]), tab(HOME, &["vim", "--version", "-e", "x"]));
        assert_eq!(parse_in(&["-e", "x", "--print-shell-integration", "zsh"]), tab(HOME, &["x", "--print-shell-integration", "zsh"]));
        assert_eq!(parse_in(&["-e", "sh", "-c", "a; b", ""]), tab(HOME, &["sh", "-c", "a; b", ""]), "as given, empty ones too");
        assert_eq!(parse_in(&["--cwd", "/srv", "-e", "make"]), tab("/srv", &["make"]));
        assert_eq!(parse_in(&["--new-tab", "--"]), tab(HOME, &[]), "the shell");
        // Arguments and directories are bytes, not UTF-8.
        let raw = |b: &[u8]| OsStr::from_bytes(b).to_os_string();
        let got = parse(&[raw(b"--cwd=/tmp/\xff"), raw(b"-e"), raw(b"cat"), raw(b"\xfe")], Some(Path::new(HOME)));
        let want = TabRequest { cwd: Some(PathBuf::from(raw(b"/tmp/\xff"))), argv: vec![raw(b"cat"), raw(b"\xfe")] };
        assert_eq!(got, Ok(Cli::Launch { verb: "new-tab", tab: Some(want) }));
    }

    #[test]
    fn a_tab_asked_for_wrongly_is_a_usage_error() {
        assert_eq!(parse_in(&["-e"]), Err("-e needs a command to run".into()));
        assert_eq!(parse_in(&["--new-tab", "-e"]), Err("-e needs a command to run".into()));
        assert_eq!(parse_in(&["--cwd"]), Err("--cwd needs a directory".into()));
        assert_eq!(parse_in(&["--cwd", "-e", "htop"]), Err("--cwd needs a directory".into()));
        assert_eq!(parse_in(&["--working-directory="]), Err("--working-directory needs a directory".into()));
        assert_eq!(parse_in(&["--hide", "--new-tab"]), Err("--hide can't open a tab (--new-tab, --cwd, -e)".into()));
        assert!(parse_in(&["--background", "-e", "tmux"]).is_err());
    }
}
