//! OSC 133 shell-integration snippets emitted by
//! `jetty --print-shell-integration <zsh|bash|fish>`.
//!
//! JeTTY NEVER edits the user's dotfiles. The user opts in with ONE guarded line
//! they add themselves (printed in `--help` and at the top of each snippet),
//! which sources the snippet ONLY under JeTTY and produces no output in other
//! terminals or when the binary is missing (instant-prompt safe). The line runs
//! `$JETTY_BIN` — the absolute path JeTTY exports to its shells — so it also works
//! for AppImage / tarball installs where `jetty` is not on `PATH`.
//!
//! Marks emitted: OSC 133 `A` (prompt), `C` (command start), `D;<exit>` (done).
//! `B` (input start) is intentionally omitted — it is the p10k-fragile part and
//! is unused by JeTTY's two features (failed-command marker + prompt jump).
//!
//! KNOWN LIMITATION (tmux/screen): OSC 133 emitted inside a multiplexer reaches
//! the multiplexer, not JeTTY, unless passthrough is configured, and
//! `$JETTY`/`$TERM_PROGRAM` may be stale inside it.

/// zsh snippet — powerlevel10k-safe.
///
/// Under p10k, an `add-zsh-hook precmd` that reads `$?` can report 0 depending on
/// hook order (p10k's precmd runs commands that reset `$?` before ours reads it),
/// so we do NOT install competing hooks when p10k is detected — instead the user
/// enables `POWERLEVEL9K_TERM_SHELL_INTEGRATION=true` and p10k emits correct,
/// instant-prompt-aware OSC 133 itself. On plain zsh (no p10k) our own hooks
/// capture `$?` on the FIRST line of precmd, which is provably correct.
pub const ZSH: &str = r#"# JeTTY zsh shell integration — OSC 133 semantic prompts.
# (prompt marks + failed-command markers + Ctrl+Shift+Z/X prompt jump)
#
# Opt in from ~/.zshrc with (guarded; silent in other terminals):
#   [[ -n "${JETTY-}" ]] && source <("${JETTY_BIN:-jetty}" --print-shell-integration zsh 2>/dev/null)
#
# powerlevel10k users: the most robust, instant-prompt-safe path is to let p10k
# emit the marks itself — add  POWERLEVEL9K_TERM_SHELL_INTEGRATION=true  to your
# ~/.p10k.zsh. When p10k is detected below, JeTTY installs NOTHING (a naive
# precmd $? capture is unreliable under p10k's hook order, and competing hooks
# can perturb instant prompt). On plain zsh the hooks below are correct.
if [[ -o interactive && -n "${JETTY-}" ]]; then
  if (( ${+functions[p10k]} )) || [[ -n "${POWERLEVEL9K_MODE:-}${POWERLEVEL9K_TERM_SHELL_INTEGRATION:-}" ]]; then
    # powerlevel10k detected: see the note above — set
    # POWERLEVEL9K_TERM_SHELL_INTEGRATION=true in ~/.p10k.zsh for correct marks.
    # (No hooks installed, no runtime output: instant-prompt safe.)
    :
  else
    autoload -Uz add-zsh-hook
    typeset -gi _jetty_run=0
    _jetty_precmd() {
      local __jetty_ret=$?                      # MUST be the first line
      (( _jetty_run )) && { print -rn -- $'\033]133;D;'"${__jetty_ret}"$'\007'; _jetty_run=0; }
      print -rn -- $'\033]133;A\007'
    }
    _jetty_preexec() { print -rn -- $'\033]133;C\007'; _jetty_run=1; }
    add-zsh-hook precmd  _jetty_precmd
    add-zsh-hook preexec _jetty_preexec
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
/// tells the resize clean-prompt wipe that real output exists. Registers via
/// bash-preexec's `precmd_functions` when present, else PREPENDS to a scalar or
/// array `PROMPT_COMMAND` so `$?` on the first line is the user command's true
/// exit status. The A mark carries `redraw=0` (kitty's extension): readline
/// repaints only the LAST line of a multi-line `PS1` after a resize, so JeTTY
/// must not wipe the prompt then. Safe under `set -u` (every variable read has a
/// default).
pub const BASH: &str = r#"# JeTTY bash shell integration — OSC 133 semantic prompts.
# Opt in from ~/.bashrc with (guarded; silent in other terminals):
#   [[ -n "${JETTY-}" ]] && source <("${JETTY_BIN:-jetty}" --print-shell-integration bash 2>/dev/null)
#
# A (prompt) and D (exit) come from PROMPT_COMMAND, C (command start) from PS0
# (bash >= 4.4); no DEBUG trap is installed — fully non-destructive.
if [[ $- == *i* && -n "${JETTY-}" && -z "${_jetty_bash_loaded:-}" ]]; then
  _jetty_bash_loaded=1   # sourcing twice must not register the hook twice
  _jetty_precmd() {
    local ret=$?                                    # user command's exit (first line)
    if [[ -n "${_jetty_started:-}" ]]; then printf '\033]133;D;%s\007' "$ret"; fi
    _jetty_started=1
    # redraw=0: after a resize readline repaints only the last line of the prompt.
    printf '\033]133;A;redraw=0\007'
  }
  if [[ -n "${__bp_imported:-}" || -n "${bash_preexec_imported:-}" ]]; then
    # bash-preexec present: register through its array (it preserves $?).
    precmd_functions+=(_jetty_precmd)
  elif [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a "* ]]; then
    # bash 5.1+ array PROMPT_COMMAND: prepend our element.
    PROMPT_COMMAND=(_jetty_precmd "${PROMPT_COMMAND[@]}")
  else
    # Scalar PROMPT_COMMAND: prepend, preserving any existing value.
    PROMPT_COMMAND="_jetty_precmd${PROMPT_COMMAND:+$'\n'$PROMPT_COMMAND}"
  fi
  # Command start: PS0 is printed after a command line is read, before it runs.
  [[ "${PS0-}" == *$'\033]133;C'* ]] || PS0+=$'\033]133;C\007'
fi
"#;

/// fish snippet — native events; captures `$status` first in fish_postexec.
pub const FISH: &str = r#"# JeTTY fish shell integration — OSC 133 semantic prompts.
# Opt in from ~/.config/fish/config.fish with (guarded; silent elsewhere):
#   test -n "$JETTY"; and test -n "$JETTY_BIN"; and "$JETTY_BIN" --print-shell-integration fish | source
if status is-interactive; and set -q JETTY
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
        assert!(BASH.contains(r"PS0+=$'\033]133;C\007'"), "C mark via PS0 append");
        assert!(BASH.contains(r#"[[ "${PS0-}" == *$'\033]133;C'* ]] ||"#), "idempotent");
        // readline repaints only a multi-line prompt's last line after a resize.
        assert!(BASH.contains(r"133;A;redraw=0"), "the A mark tells JeTTY not to wipe");
    }

    /// The bash snippet run for real: an interactive `bash -u` (nounset — some
    /// users enable it in their rc) on a PTY, sourcing the snippet, then a
    /// passing and a failing command. No rc files, no history file, a scratch
    /// HOME. Skipped when there is no bash.
    #[cfg(unix)]
    #[test]
    fn bash_snippet_runs_clean_under_set_u_in_a_real_pty() {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};
        use std::io::{Read, Write};
        let Some(bash) = ["/bin/bash", "/usr/bin/bash", "/usr/local/bin/bash", "/opt/homebrew/bin/bash"]
            .into_iter()
            .find(|p| std::path::Path::new(p).is_file())
        else {
            eprintln!("no bash — skipped");
            return;
        };
        let dir = std::env::temp_dir().join(format!("jetty-bash-u-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let snippet = dir.join("snippet.bash");
        std::fs::write(&snippet, BASH).unwrap();

        let pair = native_pty_system()
            .openpty(PtySize { rows: 24, cols: 120, pixel_width: 0, pixel_height: 0 })
            .expect("openpty");
        let mut cmd = CommandBuilder::new(bash);
        cmd.args(["--norc", "--noprofile", "-u", "-i"]);
        cmd.env_clear();
        cmd.env("HOME", &dir);
        cmd.env("HISTFILE", "/dev/null");
        cmd.env("PATH", "/usr/bin:/bin");
        cmd.env("TERM", "dumb");
        cmd.env("JETTY", "test");
        cmd.env("PS1", "$ ");
        cmd.cwd(&dir);
        let mut child = pair.slave.spawn_command(cmd).expect("spawn bash");
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
        let script = format!(
            "source '{}'\nprintf 'BASHV=%s%02d\\n' \"${{BASH_VERSINFO[0]}}\" \"${{BASH_VERSINFO[1]}}\"\ntrue\nfalse\nexit\n",
            snippet.display()
        );
        writer.write_all(script.as_bytes()).unwrap();
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
        let text = String::from_utf8_lossy(&out);
        assert!(!text.contains("unbound variable"), "set -u broke the snippet:\n{text}");
        assert!(text.contains("\x1b]133;A;redraw=0\x07"), "A mark missing:\n{text}");
        assert!(text.contains("\x1b]133;D;0\x07"), "`true` exit mark missing:\n{text}");
        assert!(text.contains("\x1b]133;D;1\x07"), "`false` exit mark missing:\n{text}");
        // PS0 (the C mark) exists from bash 4.4 on (macOS ships 3.2).
        let version: u32 = text
            .split("BASHV=")
            .skip(1) // the echoed command lines carry the format, not digits
            .find_map(|v| v.get(..3)?.parse().ok())
            .expect("bash version printed");
        if version >= 404 {
            assert!(text.contains("\x1b]133;C\x07"), "command-start mark missing:\n{text}");
        }
    }

    #[test]
    fn opt_in_lines_use_jetty_bin() {
        // AppImage / tarball installs have no `jetty` on PATH; JeTTY exports
        // $JETTY_BIN to its shells, so every opt-in line must prefer it.
        assert!(ZSH.contains(r#""${JETTY_BIN:-jetty}" --print-shell-integration zsh"#));
        assert!(BASH.contains(r#""${JETTY_BIN:-jetty}" --print-shell-integration bash"#));
        assert!(FISH.contains(r#""$JETTY_BIN" --print-shell-integration fish"#));
    }

    #[test]
    fn fish_captures_status_first() {
        assert!(FISH.contains("set -l ret $status"));
        assert!(FISH.contains("status is-interactive"));
    }
}
