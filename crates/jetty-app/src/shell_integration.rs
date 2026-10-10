//! OSC 133 shell-integration snippets: written for each run as files the
//! user's rc line sources (`jetty.zsh`, `jetty.bash`, `jetty.fish` in
//! `$JETTY_SHELL_INTEGRATION_DIR` — see [`shell_env`]), and printed by
//! `jetty --print-shell-integration <zsh|bash|fish>`.
//!
//! JeTTY NEVER edits the user's dotfiles. The user opts in with ONE guarded line
//! they add themselves ([`ZSH_LINE`], [`BASH_LINE`], [`FISH_LINE`] — shown by
//! `--help`, the README and at the top of each snippet), which sources the
//! snippet ONLY under JeTTY and produces no output in other terminals or when
//! the file is missing (instant-prompt safe). A file, not the old
//! `source <(jetty --print-shell-integration …)`: no process starts with each
//! shell — an AppImage mounted itself for every new tab — and bash 3.2 (macOS),
//! whose `source` reads nothing from a pipe, works too. The old line still does.
//!
//! Marks emitted: OSC 133 `A` (prompt), `C` (command start), `D;<exit>` (done),
//! and — zsh only — `B` (input start), from `zle-line-init`, which runs once the
//! prompt is drawn: it tells JeTTY where a multi-line prompt ends, so a resize at
//! a clean prompt may wipe the reflow's fragments. Never by editing `PROMPT` /
//! `PS1` (the p10k-fragile way).
//!
//! KNOWN LIMITATION (tmux/screen): OSC 133 emitted inside a multiplexer reaches
//! the multiplexer, not JeTTY, unless passthrough is configured, and
//! `$JETTY`/`$TERM_PROGRAM` may be stale inside it.

use std::path::{Path, PathBuf};

/// The rc-file line that opts zsh in (end of `~/.zshrc`; see the module doc).
pub const ZSH_LINE: &str =
    r#"[[ -r "${JETTY_SHELL_INTEGRATION_DIR-}/jetty.zsh" ]] && source "$JETTY_SHELL_INTEGRATION_DIR/jetty.zsh""#;
/// The rc-file line that opts bash in (end of `~/.bashrc`).
pub const BASH_LINE: &str =
    r#"[[ -r "${JETTY_SHELL_INTEGRATION_DIR-}/jetty.bash" ]] && source "$JETTY_SHELL_INTEGRATION_DIR/jetty.bash""#;
/// The line that opts fish in (`~/.config/fish/config.fish`); fish 4 marks its
/// prompts itself, and the snippet then adds nothing.
pub const FISH_LINE: &str =
    r#"test -r "$JETTY_SHELL_INTEGRATION_DIR/jetty.fish"; and source "$JETTY_SHELL_INTEGRATION_DIR/jetty.fish""#;

/// zsh snippet — powerlevel10k-safe.
///
/// Under p10k we install NO hooks: p10k emits OSC 133 itself, inside its prompt,
/// so its marks follow the transient prompt (redrawn one line up on Enter) —
/// a precmd-printed A of ours would sit on the blank line above it, and the
/// failed marker with it. p10k ships those marks OFF, so the snippet turns them
/// on (`POWERLEVEL9K_TERM_SHELL_INTEGRATION=true`, read at the first prompt)
/// unless ~/.p10k.zsh set the option either way; before, p10k users who added
/// the line got no marks at all. On plain zsh our own hooks capture `$?` on the
/// FIRST line of precmd (zsh hands every precmd hook the command's status), and
/// a `zle-line-init` hook (zsh ≥ 5.3, chained via `add-zle-hook-widget`) marks
/// the input line with `B` once per prompt. Safe under `setopt nounset`.
pub const ZSH: &str = r#"# JeTTY zsh shell integration — OSC 133 semantic prompts.
# (prompt marks + failed-command markers + Ctrl+Shift+Z/X prompt jump)
#
# Opt in from the END of ~/.zshrc with (guarded; silent in other terminals):
#   [[ -r "${JETTY_SHELL_INTEGRATION_DIR-}/jetty.zsh" ]] && source "$JETTY_SHELL_INTEGRATION_DIR/jetty.zsh"
#
# powerlevel10k: p10k emits the marks itself, in step with its transient prompt;
# this turns them on (POWERLEVEL9K_TERM_SHELL_INTEGRATION=true) unless your
# ~/.p10k.zsh sets that option, and installs no hooks. No output either way:
# instant-prompt safe. On plain zsh the hooks below are used.
if [[ -o interactive && -n "${JETTY-}" ]]; then
  if (( ${+functions[p10k]} )) || [[ -n "${POWERLEVEL9K_MODE:-}${POWERLEVEL9K_TERM_SHELL_INTEGRATION:-}" ]]; then
    (( ${+POWERLEVEL9K_TERM_SHELL_INTEGRATION} )) || typeset -g POWERLEVEL9K_TERM_SHELL_INTEGRATION=true
  else
    autoload -Uz add-zsh-hook
    typeset -gi _jetty_run=0 _jetty_input=0
    _jetty_precmd() {
      local __jetty_ret=$?                      # MUST be the first line
      (( _jetty_run )) && { print -rn -- $'\033]133;D;'"${__jetty_ret}"$'\007'; _jetty_run=0; }
      _jetty_input=0
      print -rn -- $'\033]133;A\007'
    }
    _jetty_preexec() { print -rn -- $'\033]133;C\007'; _jetty_run=1; }
    add-zsh-hook precmd  _jetty_precmd
    add-zsh-hook preexec _jetty_preexec
    # B = input start: zle-line-init runs on the input line once the prompt is
    # drawn (once per prompt — not again on a continuation line).
    _jetty_line_init() { (( _jetty_input )) || { print -rn -- $'\033]133;B\007'; _jetty_input=1; }; }
    autoload -Uz is-at-least
    if is-at-least 5.3; then
      autoload -Uz add-zle-hook-widget
      zle -N _jetty_line_init
      add-zle-hook-widget line-init _jetty_line_init
    fi
  fi
fi
"#;

/// bash snippet — non-destructive; bash-preexec-aware.
///
/// The A (prompt) and D (exit) marks come from precmd, riding `PROMPT_COMMAND` —
/// no DEBUG trap is ever installed, so nothing an existing preexec/DEBUG handler
/// relies on is touched (reading the old trap via `$(trap -p DEBUG)` is
/// impossible anyway — command substitution resets the DEBUG trap). The C
/// (command start) mark comes from `PS0`, which bash ≥ 4.4 prints after reading
/// a command line and before running it — appended to any existing `PS0`, and a
/// plain unused variable to older bash. C gives Run & Notify a real duration and
/// tells the resize clean-prompt wipe that real output exists. The same `PS0`
/// flags that a command line ran (an arithmetic array subscript that expands
/// to nothing), so D is sent only for commands — an empty Enter or ^C at the
/// prompt keeps the PREVIOUS command's `$?` and used to mark every empty prompt
/// after a failure as failed. Without that flag (bash < 4.4, `promptvars` off,
/// or a `PS0` replaced since) every prompt after the first reports, as before.
/// Registers via bash-preexec's `precmd_functions` when present, else PREPENDS
/// to a scalar or array `PROMPT_COMMAND` so `$?` on the first line is the user
/// command's true exit status — and returns it, so the user's own
/// `PROMPT_COMMAND` hooks after ours still see it. Registered as
/// `_jetty_precmd && :`: the left side of `&&` is exempt from the ERR trap
/// (and `set -e`), which a failing hook of its own fired again at every
/// prompt after a failure. The A mark carries
/// `redraw=0` (kitty's extension): readline repaints only the LAST line of a
/// multi-line `PS1` after a resize, so JeTTY must not wipe the prompt then.
/// Safe under `set -u` (every variable read has a default).
pub const BASH: &str = r#"# JeTTY bash shell integration — OSC 133 semantic prompts.
# Opt in from the END of ~/.bashrc with (guarded; silent in other terminals):
#   [[ -r "${JETTY_SHELL_INTEGRATION_DIR-}/jetty.bash" ]] && source "$JETTY_SHELL_INTEGRATION_DIR/jetty.bash"
#
# A (prompt) and D (exit) come from PROMPT_COMMAND, C (command start) from PS0
# (bash >= 4.4); no DEBUG trap is installed — fully non-destructive.
if [[ $- == *i* && -n "${JETTY-}" && -z "${_jetty_bash_loaded:-}" ]]; then
  _jetty_bash_loaded=1   # sourcing twice must not register the hook twice
  # PS0 is printed after a command line is read, before it runs — never for an
  # empty line or ^C at the prompt. Ours sends C and (promptvars on) sets
  # _jetty_ran through an array subscript that expands to nothing.
  _jetty_ps0=$'\033]133;C\007'
  if (( BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1] >= 404 )) && shopt -q promptvars; then
    _jetty_ps0+='${_jetty_nul[_jetty_ran=1]-}'
    _jetty_flags_runs=1
  fi
  _jetty_precmd() {
    local ret=$?                                    # user command's exit (first line)
    # D only for a command line that RAN (an empty Enter keeps the previous $?).
    if [[ -n "${_jetty_ran:-}" ]]; then printf '\033]133;D;%s\007' "$ret"; fi
    # Without our flag in PS0 there is no telling: every later prompt reports.
    if [[ -n "${_jetty_flags_runs:-}" && "${PS0-}" == *"$_jetty_ps0"* ]]; then _jetty_ran=; else _jetty_ran=1; fi
    # redraw=0: after a resize readline repaints only the last line of the prompt.
    printf '\033]133;A;redraw=0\007'
    return "$ret"                                   # PROMPT_COMMAND hooks after ours see it too
  }
  if [[ -n "${__bp_imported:-}" || -n "${bash_preexec_imported:-}" ]]; then
    # bash-preexec present: register through its array (it preserves $?).
    precmd_functions+=(_jetty_precmd)
  elif [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a "* ]]; then
    # bash 5.1+ array PROMPT_COMMAND: prepend our element. `&& :` keeps the
    # status it returns from firing an ERR trap (the list's $? is still it).
    PROMPT_COMMAND=('_jetty_precmd && :' "${PROMPT_COMMAND[@]}")
  else
    # Scalar PROMPT_COMMAND: prepend, preserving any existing value.
    PROMPT_COMMAND="_jetty_precmd && :${PROMPT_COMMAND:+$'\n'$PROMPT_COMMAND}"
  fi
  [[ "${PS0-}" == *$'\033]133;C'* ]] || PS0+=$_jetty_ps0
fi
"#;

/// fish snippet — native events; captures `$status` first in fish_postexec.
/// fish 4.0+ marks its prompts itself (A, C, `D;status`), so the handlers are
/// installed only for fish 3 — or when fish 4's `no-mark-prompt` feature
/// turned its own marks off; otherwise every command got a second C and D.
pub const FISH: &str = r#"# JeTTY fish shell integration — OSC 133 semantic prompts.
# Opt in from ~/.config/fish/config.fish with (guarded; silent elsewhere):
#   test -r "$JETTY_SHELL_INTEGRATION_DIR/jetty.fish"; and source "$JETTY_SHELL_INTEGRATION_DIR/jetty.fish"
# fish 4+ marks its prompts itself: there this adds nothing.
if status is-interactive; and set -q JETTY
    if string match -qr '^[0-3]\.' -- $version; or status test-feature no-mark-prompt 2>/dev/null
        function _jetty_prompt --on-event fish_prompt
            printf '\033]133;A\007'
        end
        function _jetty_preexec --on-event fish_preexec
            printf '\033]133;C\007'
        end
        function _jetty_postexec --on-event fish_postexec
            set -l ret $status               # MUST be the first statement
            printf '\033]133;D;%s\007' $ret
        end
    end
end
"#;

/// Emit the snippet for a shell name, or `None` for an unknown shell.
pub fn snippet_for(shell: &str) -> Option<&'static str> {
    match shell {
        "zsh" => Some(ZSH),
        "bash" => Some(BASH),
        "fish" => Some(FISH),
        _ => None,
    }
}

/// The variable naming the directory of this run's snippets ([`shell_env`]).
const DIR_VAR: &str = "JETTY_SHELL_INTEGRATION_DIR";

/// What every JeTTY shell needs for its opt-in line: `$JETTY_SHELL_INTEGRATION_DIR`,
/// naming this run's snippets — written now unless they are there already
/// ([`install_in`]), so one that went missing comes back with the next tab.
/// Under `$XDG_RUNTIME_DIR` (a per-user tmpfs) when it is ours and private, as
/// for the IPC socket (`crate::session_runtime_dir`), else the `jetty` dir of
/// the user's cache (macOS) — never the shared temp dir. Empty when they could
/// not be written: the line then does nothing.
pub fn shell_env() -> Vec<(String, String)> {
    let root = crate::session_runtime_dir(std::env::var_os("XDG_RUNTIME_DIR")).or_else(|| {
        use std::os::unix::fs::DirBuilderExt;
        let dir = dirs::cache_dir()?.join("jetty");
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).ok()?;
        Some(dir)
    });
    root.and_then(|root| install_in(&root))
        .and_then(|dir| dir.to_str().map(str::to_string))
        .map(|dir| vec![(DIR_VAR.to_string(), dir)])
        .unwrap_or_default()
}

/// Write the three snippets into `<root>/jetty-shell-<id>/` (0700) unless they
/// are there, and return that directory. `<id>` hashes their content, so two
/// JeTTYs at once (another config dir, another version) never source each
/// other's. `None` when `root` or that directory is not ours or is writable by
/// other users — a snippet planted there would run in every shell — or a file
/// could not be written. Each file is renamed into place whole: a starting
/// shell never reads half of one.
fn install_in(root: &Path) -> Option<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::sync::atomic::{AtomicUsize, Ordering};
    // Temp names unique within this process too: the first shell's worker
    // thread and a new tab may install at once.
    static TMP: AtomicUsize = AtomicUsize::new(0);
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let ours = |m: std::fs::Metadata| m.is_dir() && m.uid() == uid && m.mode() & 0o022 == 0;
    if !std::fs::metadata(root).is_ok_and(ours) {
        return None;
    }
    let files = [("jetty.zsh", ZSH), ("jetty.bash", BASH), ("jetty.fish", FISH)];
    let id = files
        .iter()
        .flat_map(|(_, text)| text.bytes().chain([0]))
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    let dir = root.join(format!("jetty-shell-{id:016x}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return None,
    }
    // One that was there already must be ours too (not a symlink to someone's).
    if !std::fs::symlink_metadata(&dir).is_ok_and(ours) {
        return None;
    }
    if files.iter().all(|(name, _)| dir.join(name).is_file()) {
        return Some(dir);
    }
    for (name, text) in files {
        let tmp = dir.join(format!(".{name}.{}.{}", std::process::id(), TMP.fetch_add(1, Ordering::Relaxed)));
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .and_then(|()| std::fs::rename(&tmp, dir.join(name)));
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return None;
        }
    }
    Some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_for_known_shells() {
        assert!(snippet_for("zsh").is_some());
        assert!(snippet_for("bash").is_some());
        assert!(snippet_for("fish").is_some());
        assert!(snippet_for("tcsh").is_none());
        assert!(snippet_for("").is_none());
    }

    #[test]
    fn zsh_emits_the_three_marks_and_is_p10k_guarded() {
        assert!(ZSH.contains("133;D;"), "zsh emits the D exit-code mark");
        assert!(ZSH.contains("133;A"), "zsh emits the A prompt mark");
        assert!(ZSH.contains("133;C"), "zsh emits the C output mark");
        assert!(ZSH.contains("add-zle-hook-widget line-init"), "and B from zle-line-init");
        // The exit code is captured on the FIRST line of precmd.
        assert!(ZSH.contains("local __jetty_ret=$?"), "captures $? first");
        // p10k detection / recommendation is present.
        assert!(ZSH.contains("POWERLEVEL9K_TERM_SHELL_INTEGRATION"));
    }

    #[test]
    fn bash_is_non_destructive() {
        // Never installs a DEBUG trap at all (so nothing existing is clobbered);
        // registers via bash-preexec's array when present, else prepends to a
        // scalar or array PROMPT_COMMAND (preserving any existing value).
        assert!(
            BASH.lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .all(|l| !l.contains("trap")),
            "no code line may install/replace a trap"
        );
        assert!(BASH.contains("precmd_functions+=(_jetty_precmd)"), "bash-preexec path");
        assert!(BASH.contains("declare -a "), "handles array-typed PROMPT_COMMAND");
        assert!(BASH.contains("${PROMPT_COMMAND:+"), "preserves an existing scalar PROMPT_COMMAND");
        assert!(BASH.contains("133;D;%s"), "emits the D exit-code mark");
        // C comes from PS0, APPENDED (an existing PS0 keeps working) and guarded
        // against sourcing twice.
        assert!(BASH.contains(r"_jetty_ps0=$'\033]133;C\007'"), "C mark via PS0");
        assert!(BASH.contains(r#"[[ "${PS0-}" == *$'\033]133;C'* ]] || PS0+=$_jetty_ps0"#), "appended, idempotent");
        // readline repaints only a multi-line prompt's last line after a resize.
        assert!(BASH.contains(r"133;A;redraw=0"), "the A mark tells JeTTY not to wipe");
    }

    /// Run the first installed of `shells` with `args` on a REAL PTY, in an empty
    /// scratch HOME (no rc files, no history file), type `source <snippet>` then
    /// `script` into it, and return everything it printed until it exits (15 s
    /// at most). `None` when none of `shells` is installed.
    #[cfg(unix)]
    fn run_snippet_in_pty(shells: &[&str], args: &[&str], term: &str, snippet: &str, script: &str) -> Option<String> {
        run_in_pty(shells, args, term, snippet, script, true, &[])
    }

    /// [`run_snippet_in_pty`], but the snippet is the scratch HOME's rc file
    /// (`.bashrc`, `$ZDOTDIR/.zshrc`) — loaded at startup, before any command,
    /// as in real use — so `args` must not suppress rc files.
    #[cfg(unix)]
    fn run_rc_in_pty(shells: &[&str], args: &[&str], term: &str, snippet: &str, script: &str) -> Option<String> {
        run_in_pty(shells, args, term, snippet, script, false, &[])
    }

    /// `env` is exported to the shell on top of the scratch environment.
    #[cfg(unix)]
    fn run_in_pty(
        shells: &[&str],
        args: &[&str],
        term: &str,
        snippet: &str,
        script: &str,
        typed: bool,
        env: &[(&str, &str)],
    ) -> Option<String> {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};
        use std::io::{Read, Write};
        // Tests run in parallel: one scratch dir per call.
        static RUN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let shell = shells.iter().find(|p| std::path::Path::new(p).is_file())?;
        let name = std::path::Path::new(shell).file_name().unwrap().to_string_lossy().into_owned();
        let run = RUN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("jetty-{name}-pty-{}-{run}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("snippet");
        std::fs::write(&path, snippet).unwrap();
        if !typed {
            std::fs::write(dir.join(".bashrc"), snippet).unwrap();
            std::fs::write(dir.join(".zshrc"), snippet).unwrap();
        }

        let pair = native_pty_system()
            .openpty(PtySize { rows: 24, cols: 120, pixel_width: 0, pixel_height: 0 })
            .expect("openpty");
        let mut cmd = CommandBuilder::new(shell);
        cmd.args(args);
        cmd.env_clear();
        cmd.env("HOME", &dir);
        cmd.env("ZDOTDIR", &dir);
        cmd.env("HISTFILE", "/dev/null");
        cmd.env("PATH", "/usr/bin:/bin");
        cmd.env("TERM", term);
        cmd.env("JETTY", "test");
        cmd.env("PS1", "$ ");
        for (key, value) in env {
            cmd.env(key, value);
        }
        cmd.cwd(&dir);
        let mut child = pair.slave.spawn_command(cmd).expect("spawn shell");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut writer = pair.master.take_writer().unwrap();
        let input = if typed { format!("source '{}'\n{script}", path.display()) } else { script.to_string() };
        writer.write_all(input.as_bytes()).unwrap();
        writer.flush().unwrap();

        let mut out = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(chunk) => out.extend_from_slice(&chunk),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if child.try_wait().ok().flatten().is_some() {
                        while let Ok(chunk) = rx.recv_timeout(std::time::Duration::from_millis(200)) {
                            out.extend_from_slice(&chunk);
                        }
                        break;
                    }
                }
            }
        }
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&dir);
        Some(String::from_utf8_lossy(&out).into_owned())
    }

    const BASHES: &[&str] = &["/bin/bash", "/usr/bin/bash", "/usr/local/bin/bash", "/opt/homebrew/bin/bash"];
    const ZSHES: &[&str] = &["/bin/zsh", "/usr/bin/zsh", "/usr/local/bin/zsh", "/opt/homebrew/bin/zsh"];

    /// A script line printing `BASHV=<major><minor:02>`; read back by [`bash_version`].
    const PRINT_BASH_VERSION: &str = "printf 'BASHV=%s%02d\\n' \"${BASH_VERSINFO[0]}\" \"${BASH_VERSINFO[1]}\"\n";

    /// The version [`PRINT_BASH_VERSION`] printed, e.g. `503` for bash 5.3.
    fn bash_version(text: &str) -> u32 {
        text.split("BASHV=")
            .skip(1) // the echoed command lines carry the format, not digits
            .find_map(|v| v.get(..3)?.parse().ok())
            .expect("bash version printed")
    }

    /// The bash snippet run for real: an interactive `bash -u` (nounset — some
    /// users enable it in their rc) on a PTY, sourcing the snippet, then a
    /// passing and a failing command. Skipped when there is no bash.
    #[cfg(unix)]
    #[test]
    fn bash_snippet_runs_clean_under_set_u_in_a_real_pty() {
        let script = format!("{PRINT_BASH_VERSION}true\nfalse\nexit\n");
        let Some(text) = run_snippet_in_pty(BASHES, &["--norc", "--noprofile", "-u", "-i"], "dumb", BASH, &script)
        else {
            eprintln!("no bash — skipped");
            return;
        };
        assert!(!text.contains("unbound variable"), "set -u broke the snippet:\n{text}");
        assert!(text.contains("\x1b]133;A;redraw=0\x07"), "A mark missing:\n{text}");
        assert!(text.contains("\x1b]133;D;0\x07"), "`true` exit mark missing:\n{text}");
        assert!(text.contains("\x1b]133;D;1\x07"), "`false` exit mark missing:\n{text}");
        // PS0 (the C mark) exists from bash 4.4 on (macOS ships 3.2).
        if bash_version(&text) >= 404 {
            assert!(text.contains("\x1b]133;C\x07"), "command-start mark missing:\n{text}");
        }
    }

    /// A status (`D`) only for a command line that RAN: an empty Enter after a
    /// failure reported the failure again — another failed marker (and pulse)
    /// on every empty prompt. Sourced twice (a re-sourced ~/.bashrc): still one
    /// mark each. bash >= 4.4 (3.2 cannot tell an empty line from a command).
    #[cfg(unix)]
    #[test]
    fn bash_reports_a_status_only_for_command_lines_that_ran() {
        let script = format!("{PRINT_BASH_VERSION}false\n\n\ntrue\n\nexit\n");
        let snippet = format!("{BASH}\n{BASH}");
        let Some(text) = run_snippet_in_pty(BASHES, &["--norc", "--noprofile", "-u", "-i"], "dumb", &snippet, &script)
        else {
            eprintln!("no bash — skipped");
            return;
        };
        if bash_version(&text) < 404 {
            return;
        }
        assert!(!text.contains("unbound variable"), "set -u broke the snippet:\n{text}");
        assert_eq!(text.matches("\x1b]133;D;1\x07").count(), 1, "`false` reported once:\n{text}");
        assert_eq!(text.matches("\x1b]133;D;0\x07").count(), 2, "printf + `true`, once each:\n{text}");
        // printf, false, true and `exit` itself.
        assert_eq!(text.matches("\x1b]133;C\x07").count(), 4, "one C per command:\n{text}");
    }

    /// Loaded the real way — from ~/.bashrc at startup, before any command — the
    /// very FIRST command is reported too. (bash's own command counter, `\#`,
    /// skips exactly that one once PS0 is in use, so it cannot be the signal.)
    #[cfg(unix)]
    #[test]
    fn bash_from_bashrc_reports_the_first_command_too() {
        let script = format!("false\n\n{PRINT_BASH_VERSION}exit\n");
        let Some(text) = run_rc_in_pty(BASHES, &["--noprofile", "-i"], "dumb", BASH, &script) else {
            eprintln!("no bash — skipped");
            return;
        };
        if bash_version(&text) < 404 {
            return;
        }
        assert_eq!(text.matches("\x1b]133;D;1\x07").count(), 1, "the first command, `false`:\n{text}");
        assert_eq!(text.matches("\x1b]133;D;0\x07").count(), 1, "printf, and nothing for the empty line:\n{text}");
    }

    /// powerlevel10k's own marks follow its transient prompt (ours would sit on
    /// the blank line above it), but p10k ships them OFF — users who added
    /// JeTTY's line got no marks at all. Under p10k the snippet turns them on
    /// unless ~/.p10k.zsh chose either way, and installs no hooks of its own.
    #[cfg(unix)]
    #[test]
    fn zsh_under_p10k_turns_on_p10ks_own_marks_unless_the_user_chose() {
        let script = "print -r -- \"OPT=[${POWERLEVEL9K_TERM_SHELL_INTEGRATION-unset}]\"\nexit\n";
        for (setup, want) in [("", "true"), ("typeset -g POWERLEVEL9K_TERM_SHELL_INTEGRATION=false\n", "false")] {
            // A stand-in for a loaded p10k (it defines `p10k`).
            let snippet = format!("p10k() {{ :; }}\n{setup}{ZSH}");
            let Some(text) = run_snippet_in_pty(ZSHES, &["-f", "-i"], "dumb", &snippet, script) else {
                eprintln!("no zsh — skipped");
                return;
            };
            assert!(text.contains(&format!("OPT=[{want}]")), "{setup:?}: {text:?}");
            assert!(!text.contains("\x1b]133;"), "no marks of our own under p10k: {text:?}");
        }
    }

    /// zsh, loaded from ~/.zshrc at startup: the first command is reported.
    #[cfg(unix)]
    #[test]
    fn zsh_from_zshrc_reports_the_first_command_too() {
        let Some(text) = run_rc_in_pty(ZSHES, &["-i"], "xterm-256color", ZSH, "false\n\ntrue\nexit\n") else {
            eprintln!("no zsh — skipped");
            return;
        };
        assert_eq!(text.matches("\x1b]133;D;1\x07").count(), 1, "the first command, `false`: {text:?}");
        assert_eq!(text.matches("\x1b]133;D;0\x07").count(), 1, "`true`, and nothing for the empty line: {text:?}");
    }

    /// The user's own `PROMPT_COMMAND` (set before the snippet — e.g. a prompt
    /// that shows the last exit status) runs AFTER our hook and must still see
    /// the command's `$?`, not our hook's. Scalar and (bash >= 5.1) array forms.
    #[cfg(unix)]
    #[test]
    fn bash_hands_the_exit_status_on_to_the_users_prompt_command() {
        for setup in [r#"PROMPT_COMMAND='printf "<ST=%s>" $?'"#, r#"PROMPT_COMMAND=('printf "<ST=%s>" $?')"#] {
            let snippet = format!("{setup}\n{BASH}");
            let script = format!("{PRINT_BASH_VERSION}false\nexit\n");
            let Some(text) = run_snippet_in_pty(BASHES, &["--norc", "--noprofile", "-i"], "dumb", &snippet, &script)
            else {
                eprintln!("no bash — skipped");
                return;
            };
            if setup.contains("=(") && bash_version(&text) < 501 {
                continue; // array PROMPT_COMMAND is bash 5.1+
            }
            assert!(text.contains("\x1b]133;D;1\x07"), "our D still sees `false` ({setup}):\n{text}");
            assert!(text.contains("<ST=1>"), "the user's hook must see `false`'s status ({setup}):\n{text}");
        }
    }

    /// The user's ERR trap fires once per failed command — never for our hook,
    /// which returns that command's status to hand it on: run as a plain
    /// `PROMPT_COMMAND` command it re-fired the trap at the prompt after every
    /// failure, and again at each empty Enter. Scalar and (bash >= 5.1) array
    /// `PROMPT_COMMAND`.
    #[cfg(unix)]
    #[test]
    fn bash_never_fires_the_users_err_trap_itself() {
        for setup in ["", "PROMPT_COMMAND=('true')"] {
            let snippet = format!("trap 'printf \"<ERR>\"' ERR\n{setup}\n{BASH}");
            let script = format!("{PRINT_BASH_VERSION}false\n\n\ntrue\nexit\n");
            let Some(text) = run_snippet_in_pty(BASHES, &["--norc", "--noprofile", "-i"], "dumb", &snippet, &script)
            else {
                eprintln!("no bash — skipped");
                return;
            };
            if !setup.is_empty() && bash_version(&text) < 501 {
                continue; // array PROMPT_COMMAND is bash 5.1+
            }
            assert_eq!(text.matches("<ERR>").count(), 1, "`false` alone fires it ({setup:?}):\n{text}");
            assert!(text.contains("\x1b]133;D;1\x07"), "our D still sees `false` ({setup:?}):\n{text}");
        }
    }

    /// zsh: a status only after a command ran (preexec), and sourcing the
    /// snippet twice (a re-sourced ~/.zshrc) registers every hook once.
    #[cfg(unix)]
    #[test]
    fn zsh_reports_a_status_only_for_commands_that_ran_even_when_sourced_twice() {
        let snippet = format!("{ZSH}\n{ZSH}");
        let Some(text) =
            run_snippet_in_pty(ZSHES, &["-f", "-o", "nounset", "-i"], "xterm-256color", &snippet, "false\n\n\ntrue\n\nexit\n")
        else {
            eprintln!("no zsh — skipped");
            return;
        };
        assert_eq!(text.matches("\x1b]133;D;1\x07").count(), 1, "`false` reported once: {text:?}");
        assert_eq!(text.matches("\x1b]133;D;0\x07").count(), 1, "`true` reported once: {text:?}");
        // false, true and `exit` itself.
        assert_eq!(text.matches("\x1b]133;C\x07").count(), 3, "one C per command: {text:?}");
        // Six prompts follow the `source` (after it, false, two empty lines, true, one more).
        assert_eq!(text.matches("\x1b]133;A\x07").count(), 6, "one A per prompt: {text:?}");
    }

    /// The zsh snippet run for real (`zsh -f -i`, `setopt nounset`, a two-line
    /// prompt): B must land on the INPUT line, after the whole prompt is drawn —
    /// JeTTY's resize wipe trusts it.
    #[cfg(unix)]
    #[test]
    fn zsh_snippet_marks_the_input_line_in_a_real_pty() {
        let script = "PROMPT=$'INFO-LINE\\n> '\ntrue\nfalse\nexit\n";
        let Some(text) = run_snippet_in_pty(
            &["/bin/zsh", "/usr/bin/zsh", "/usr/local/bin/zsh", "/opt/homebrew/bin/zsh"],
            &["-f", "-o", "nounset", "-i"],
            "xterm-256color",
            ZSH,
            script,
        ) else {
            eprintln!("no zsh — skipped");
            return;
        };
        assert!(!text.contains("parameter not set"), "nounset broke the snippet:\n{text:?}");
        assert!(text.contains("\x1b]133;D;0\x07") && text.contains("\x1b]133;D;1\x07"), "{text:?}");
        // The prompt drawn with the new PROMPT, then B, then the input.
        let at = text.find("INFO-LINE\r\n> ").unwrap_or_else(|| panic!("two-line prompt drawn: {text:?}"));
        let after = &text[at..];
        let b = after.find("\x1b]133;B\x07").unwrap_or_else(|| panic!("B after the prompt: {text:?}"));
        let next = after.find("\x1b]133;D").unwrap_or(after.len());
        assert!(b < next, "B on this prompt's input line, before the command ran: {text:?}");
    }

    #[test]
    fn one_opt_in_line_per_shell_everywhere() {
        // The same line in each snippet's header, `--help` (printed from these
        // consts) and the README: they used to drift apart — the `--help` lines
        // lacked `2>/dev/null` and printed an error at every shell start once
        // $JETTY_BIN went stale.
        for (snippet, line) in [(ZSH, ZSH_LINE), (BASH, BASH_LINE), (FISH, FISH_LINE)] {
            assert!(snippet.contains(&format!("#   {line}\n")), "the snippet's header shows {line}");
        }
        let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md")).expect("README.md");
        for line in [ZSH_LINE, BASH_LINE, FISH_LINE] {
            assert!(readme.contains(&format!("`{line}`")), "README shows {line}");
        }
    }

    /// A fresh directory only we can write, like `$XDG_RUNTIME_DIR`.
    #[cfg(unix)]
    fn private_root(tag: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("jetty-si-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    #[cfg(unix)]
    #[test]
    fn the_snippets_are_written_once_into_a_private_dir() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = private_root("install");
        let dir = install_in(&root).expect("installed");
        assert!(dir.starts_with(&root));
        assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700, "only we can read or replace them");
        for (name, text) in [("jetty.zsh", ZSH), ("jetty.bash", BASH), ("jetty.fish", FISH)] {
            assert_eq!(std::fs::read_to_string(dir.join(name)).unwrap(), text, "{name}");
        }
        // The next tab finds them; one that went missing comes back.
        assert_eq!(install_in(&root), Some(dir.clone()));
        std::fs::remove_file(dir.join("jetty.bash")).unwrap();
        assert_eq!(install_in(&root), Some(dir.clone()));
        assert_eq!(std::fs::read_to_string(dir.join("jetty.bash")).unwrap(), BASH);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3, "no temp file left behind");
        // A root other users can write (a shared temp dir): never — a snippet
        // planted there would run in every shell.
        let shared = private_root("shared");
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(install_in(&shared), None);
        // A snippet directory that was there already must be ours: not a link
        // to a directory of planted snippets, not one others can write in.
        let planted = private_root("planted");
        for name in ["jetty.zsh", "jetty.bash", "jetty.fish"] {
            std::fs::write(planted.join(name), "echo planted").unwrap();
        }
        let other = private_root("other");
        std::os::unix::fs::symlink(&planted, other.join(dir.file_name().unwrap())).unwrap();
        assert_eq!(install_in(&other), None, "a symlinked snippet directory");
        let loose = private_root("loose");
        std::fs::create_dir(loose.join(dir.file_name().unwrap())).unwrap();
        std::fs::set_permissions(loose.join(dir.file_name().unwrap()), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(install_in(&loose), None, "a snippet directory others can write in");
        for d in [root, shared, planted, other, loose] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// The opt-in line, loaded from the rc file as in real use, sources the
    /// snippet JeTTY wrote for this run — a file: no process per shell, and
    /// bash 3.2 sources it too (its `source` read nothing from `<(…)`). Without
    /// `$JETTY_SHELL_INTEGRATION_DIR` (another terminal) it does nothing, silently.
    #[cfg(unix)]
    #[test]
    fn the_rc_line_sources_this_runs_snippet_and_is_silent_elsewhere() {
        let root = private_root("rc-line");
        let dir = install_in(&root).expect("installed");
        let dir = dir.to_str().unwrap();
        let shells = [(BASHES, &["--noprofile", "-i"][..], "dumb", BASH_LINE), (ZSHES, &["-i"][..], "xterm-256color", ZSH_LINE)];
        for (shells, args, term, line) in shells {
            let Some(text) = run_in_pty(shells, args, term, line, "false\nexit\n", false, &[(DIR_VAR, dir)]) else {
                eprintln!("no {line:?} shell — skipped");
                continue;
            };
            assert!(text.contains("\x1b]133;D;1\x07"), "{line}: the snippet ran:\n{text:?}");
            let text = run_in_pty(shells, args, term, line, "false\nexit\n", false, &[]).unwrap();
            assert!(!text.contains("\x1b]133;"), "{line}: no marks elsewhere:\n{text:?}");
            assert!(!text.contains("jetty."), "{line}: and no error about the file:\n{text:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fish_captures_status_first() {
        assert!(FISH.contains("set -l ret $status"));
        assert!(FISH.contains("status is-interactive"));
    }

    #[test]
    fn fish_adds_marks_only_where_fish_sends_none() {
        // fish 4.0+ sends A, C and D;status itself (unless `no-mark-prompt`):
        // every handler sits inside the version / feature check.
        let guard = "    if string match -qr '^[0-3]\\.' -- $version; or status test-feature no-mark-prompt 2>/dev/null\n";
        let (before, guarded) = FISH.split_once(guard).expect("the fish 3 / no-mark-prompt guard");
        assert!(!before.contains("function "), "no handler before it");
        assert_eq!(guarded.matches("        function _jetty_").count(), 3, "all three inside it");
    }
}
