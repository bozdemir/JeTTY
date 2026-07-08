//! OSC 133 shell-integration snippets emitted by
//! `jetty --print-shell-integration <zsh|bash|fish>`.
//!
//! JeTTY NEVER edits the user's dotfiles. The user opts in with ONE guarded line
//! they add themselves (printed in `--help` and at the top of each snippet),
//! which sources the snippet ONLY under JeTTY and produces no output in other
//! terminals or when the binary is missing (instant-prompt safe).
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
#   [[ -n "$JETTY" ]] && command -v jetty >/dev/null 2>&1 && source <(jetty --print-shell-integration zsh) 2>/dev/null
#
# powerlevel10k users: the most robust, instant-prompt-safe path is to let p10k
# emit the marks itself — add  POWERLEVEL9K_TERM_SHELL_INTEGRATION=true  to your
# ~/.p10k.zsh. When p10k is detected below, JeTTY installs NOTHING (a naive
# precmd $? capture is unreliable under p10k's hook order, and competing hooks
# can perturb instant prompt). On plain zsh the hooks below are correct.
if [[ -o interactive && -n "$JETTY" ]]; then
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
/// JeTTY's two features need only the A (prompt) and D (exit) marks, both
/// emittable from precmd, so bash integration rides `PROMPT_COMMAND` ALONE — it
/// never installs a DEBUG trap, so nothing an existing preexec/DEBUG handler
/// relies on is touched (reading the old trap via `$(trap -p DEBUG)` is
/// impossible anyway — command substitution resets the DEBUG trap). The C mark,
/// which would require a DEBUG trap, is intentionally omitted; JeTTY does not
/// render it. Registers via bash-preexec's `precmd_functions` when present, else
/// PREPENDS to a scalar or array `PROMPT_COMMAND` so `$?` on the first line is
/// the user command's true exit status.
pub const BASH: &str = r#"# JeTTY bash shell integration — OSC 133 semantic prompts.
# Opt in from ~/.bashrc with (guarded; silent in other terminals):
#   [[ -n "$JETTY" ]] && command -v jetty >/dev/null 2>&1 && source <(jetty --print-shell-integration bash) 2>/dev/null
#
# Emits only the A (prompt) and D (exit) marks — all JeTTY needs — from
# PROMPT_COMMAND, so it installs NO DEBUG trap and is fully non-destructive.
if [[ $- == *i* && -n "$JETTY" ]]; then
  _jetty_precmd() {
    local ret=$?                                    # user command's exit (first line)
    if [[ -n "${_jetty_started:-}" ]]; then printf '\033]133;D;%s\007' "$ret"; fi
    _jetty_started=1
    printf '\033]133;A\007'
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
fi
"#;

/// fish snippet — native events; captures `$status` first in fish_postexec.
pub const FISH: &str = r#"# JeTTY fish shell integration — OSC 133 semantic prompts.
# Opt in from ~/.config/fish/config.fish with (guarded; silent elsewhere):
#   test -n "$JETTY"; and command -q jetty; and jetty --print-shell-integration fish | source
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
    }

    #[test]
    fn fish_captures_status_first() {
        assert!(FISH.contains("set -l ret $status"));
        assert!(FISH.contains("status is-interactive"));
    }
}
