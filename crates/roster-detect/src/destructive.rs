//! Destructive-ask predicate: is a pending tool call irreversible?
//!
//! Feeds the `destructive` flag of `roster_core::attention::rank` once the
//! hook bridge forwards `PreToolUse` payloads (docs/05, attention inbox).
//! Behavior lives in the [`RULES`] table — extend the data, not the
//! matcher.
//!
//! **Input contract:** `tool_input` is the extracted command or statement
//! string — for Claude Code's `Bash` tool, the payload's `tool_input.command`
//! field — never the serialized JSON envelope. JSON-shaped input matches
//! nothing (pinned by test), so a caller that forgets to extract gets a
//! silent all-false, not garbage matches.
//!
//! Biases, both deliberate: a shell rule fires only when the program sits
//! in command position (past wrappers like `sudo` or `bash -c`), so prose
//! that merely *mentions* `rm -rf` stays unflagged — the destructive band
//! must not become noise — while flag and subcommand matching within the
//! command errs toward flagging (a false positive only floats an ask
//! higher). Unknown tool kinds are never destructive. Known misses: a
//! command inside an `ssh host '…'` remote string, and quoting exotic
//! enough to defeat whitespace tokenization.

/// Which tools a rule applies to, judged from the tool name alone.
#[derive(Clone, Copy)]
enum Scope {
    /// Tools that execute shell commands (`Bash`, `run_shell`, …).
    Shell,
    /// Tools whose name says SQL — and only those; word rules loose enough
    /// to misfire on shell text (bare `truncate`) live here.
    Sql,
    /// SQL statements precise enough to also scan shell input for
    /// (`psql -c "DROP TABLE …"`).
    ShellOrSql,
}

/// How a rule matches inside the tool input.
enum Pattern {
    /// A program invocation carrying every flag group. The program must sit
    /// in command position of a segment (first token past wrappers, `-`
    /// flags, and `VAR=x` assignments), compared by path basename with
    /// surrounding quotes trimmed — so `sudo /bin/rm`, `bash -c "rm …"`,
    /// and `cd x && rm …` all count, but `echo rm` does not. `subcommand`
    /// (when set) and one alternative from each group must appear after it.
    /// Short alternatives (`-f`) match inside combined flags (`-rf`); long
    /// ones (`--force`) also match their `=value` form; alternatives with
    /// no dash (`+`) match any token starting with them.
    Command {
        /// The program's basename.
        program: &'static str,
        /// A required subcommand anywhere after the program, if any.
        subcommand: Option<&'static str>,
        /// All-of groups; each group is satisfied by any-of its flags.
        flag_groups: &'static [&'static [&'static str]],
    },
    /// Adjacent words, case-insensitive, punctuation-trimmed — for SQL
    /// statements wherever they are quoted or nested.
    Words(&'static [&'static str]),
    /// A raw substring of the input.
    Contains(&'static str),
}

/// Programs that wrap another command rather than being the command:
/// skipped when finding the program position of a segment.
const WRAPPERS: &[&str] = &[
    "sudo", "doas", "env", "nohup", "time", "command", "xargs", "bash", "sh", "zsh",
];

/// The rule table — the single place destructiveness is defined.
///
/// | rule | why it is irreversible |
/// |---|---|
/// | `rm` with recursive+force | deletes trees without prompting |
/// | `git push` forced (`-f`/`--force*` flag or `+refspec`) | can discard remote commits (`--force-with-lease` included: it still overwrites) |
/// | `git push --delete` | removes a remote ref others may depend on |
/// | `git reset --hard` | discards working-tree and index changes |
/// | `git checkout --`, `git restore` | overwrite working-tree edits in place |
/// | `git clean` with force | deletes untracked files, unrecoverable by git |
/// | `chmod` recursive | rewrites permissions across a tree |
/// | `find … -delete` | recursive delete without prompting |
/// | `truncate` (coreutil or SQL) | empties files or tables in place |
/// | `>\|` redirect | overwrites a file past `noclobber` |
/// | `DROP TABLE`/`DATABASE`/`SCHEMA` | destroys schema objects and their data |
/// | `DELETE FROM` | removes rows; without a WHERE, all of them |
const RULES: &[(Scope, Pattern)] = &[
    (
        Scope::Shell,
        Pattern::Command {
            program: "rm",
            subcommand: None,
            flag_groups: &[&["-r", "-R", "--recursive"], &["-f", "--force"]],
        },
    ),
    (
        Scope::Shell,
        Pattern::Command {
            program: "git",
            subcommand: Some("push"),
            flag_groups: &[&["-f", "--force", "--force-with-lease", "+"]],
        },
    ),
    (
        Scope::Shell,
        Pattern::Command {
            program: "git",
            subcommand: Some("push"),
            flag_groups: &[&["--delete"]],
        },
    ),
    (
        Scope::Shell,
        Pattern::Command {
            program: "git",
            subcommand: Some("reset"),
            flag_groups: &[&["--hard"]],
        },
    ),
    (
        Scope::Shell,
        Pattern::Command {
            program: "git",
            subcommand: Some("checkout"),
            flag_groups: &[&["--"]],
        },
    ),
    (
        Scope::Shell,
        Pattern::Command {
            program: "git",
            subcommand: Some("restore"),
            flag_groups: &[],
        },
    ),
    (
        Scope::Shell,
        Pattern::Command {
            program: "git",
            subcommand: Some("clean"),
            flag_groups: &[&["-f", "--force"]],
        },
    ),
    (
        Scope::Shell,
        Pattern::Command {
            program: "chmod",
            subcommand: None,
            flag_groups: &[&["-R", "--recursive"]],
        },
    ),
    (
        Scope::Shell,
        Pattern::Command {
            program: "find",
            subcommand: None,
            flag_groups: &[&["-delete"]],
        },
    ),
    (
        Scope::Shell,
        Pattern::Command {
            program: "truncate",
            subcommand: None,
            flag_groups: &[],
        },
    ),
    (Scope::Shell, Pattern::Contains(">|")),
    (Scope::ShellOrSql, Pattern::Words(&["drop", "table"])),
    (Scope::ShellOrSql, Pattern::Words(&["drop", "database"])),
    (Scope::ShellOrSql, Pattern::Words(&["drop", "schema"])),
    (Scope::ShellOrSql, Pattern::Words(&["delete", "from"])),
    (Scope::Sql, Pattern::Words(&["truncate"])),
];

/// Whether a pending tool call is irreversible enough to outrank longer
/// waits in the attention queue. `tool_name` decides which rules apply;
/// `tool_input` is the extracted command/statement string (see the module
/// doc for the contract). Anything unmatched — including every
/// unrecognized tool kind — is `false`.
pub fn is_destructive(tool_name: &str, tool_input: &str) -> bool {
    let shell = is_shell_tool(tool_name);
    let sql = is_sql_tool(tool_name);
    // Backslash-newline continuations are one logical line to the shell;
    // unfold them so the flags stay in their program's segment.
    let unfolded = tool_input.replace("\\\n", " ");
    RULES.iter().any(|(scope, pattern)| {
        let applies = match scope {
            Scope::Shell => shell,
            Scope::Sql => sql,
            Scope::ShellOrSql => shell || sql,
        };
        applies && matches(pattern, &unfolded)
    })
}

/// Whether the tool name suggests shell-command execution.
fn is_shell_tool(tool_name: &str) -> bool {
    let name = tool_name.to_ascii_lowercase();
    ["bash", "shell", "terminal", "exec", "command", "cmd"]
        .iter()
        .any(|hint| name.contains(hint))
}

/// Whether the tool name says SQL. Deliberately just `sql`: broader hints
/// (`query`, `database`) would scope in read-only tools like
/// `notion-query-data-sources` and turn the loose word rules into noise.
fn is_sql_tool(tool_name: &str) -> bool {
    tool_name.to_ascii_lowercase().contains("sql")
}

fn matches(pattern: &Pattern, input: &str) -> bool {
    match pattern {
        Pattern::Command {
            program,
            subcommand,
            flag_groups,
        } => segments(input)
            .any(|segment| command_matches(segment, program, *subcommand, flag_groups)),
        Pattern::Words(words) => words_match(input, words),
        Pattern::Contains(needle) => input.contains(needle),
    }
}

/// Command segments of a shell input, split on separators so flags from
/// one command can't satisfy a rule against another (`rm -r x; tar -f y`).
fn segments(input: &str) -> impl Iterator<Item = &str> {
    input.split(['|', '&', ';', '\n'])
}

/// A token with surrounding shell quotes removed, so `bash -c "rm -rf x"`
/// tokenizes to a recognizable `rm`.
fn unquote(token: &str) -> &str {
    token.trim_matches(['"', '\''])
}

/// Whether one command segment invokes `program` in command position,
/// followed by the subcommand and every flag group.
fn command_matches(
    segment: &str,
    program: &str,
    subcommand: Option<&str>,
    flag_groups: &[&[&str]],
) -> bool {
    let tokens: Vec<&str> = segment.split_whitespace().map(unquote).collect();
    // Command position: the first token that isn't a wrapper, a wrapper's
    // `-c`-style flag, or a `VAR=x` environment assignment.
    let Some(at) = tokens.iter().position(|t| {
        let base = t.rsplit('/').next().unwrap_or(t);
        !(WRAPPERS.contains(&base) || t.starts_with('-') || t.contains('='))
    }) else {
        return false;
    };
    if tokens[at].rsplit('/').next() != Some(program) {
        return false;
    }
    // Subcommand and flags may sit anywhere after the program: position
    // matching would miss `git -C repo push --force`, and missing a real
    // force is worse than boosting a `git stash push -f`.
    let rest = &tokens[at + 1..];
    if let Some(sub) = subcommand {
        if !rest.contains(&sub) {
            return false;
        }
    }
    flag_groups.iter().all(|group| {
        rest.iter()
            .any(|t| group.iter().any(|f| flag_token_matches(t, f)))
    })
}

/// Whether one token carries the flag: long flags match exactly or with a
/// `=value` suffix; short flags match their letter inside combined runs
/// (`-rf` carries `-r` and `-f`); dashless alternatives (`+`) match any
/// token starting with them (a `+refspec` force push).
fn flag_token_matches(token: &str, flag: &str) -> bool {
    if let Some(long) = flag.strip_prefix("--") {
        token
            .strip_prefix("--")
            .is_some_and(|t| t == long || t.strip_prefix(long).is_some_and(|r| r.starts_with('=')))
    } else if let Some(letter) = flag.strip_prefix('-') {
        token.starts_with('-') && !token.starts_with("--") && token[1..].contains(letter)
    } else {
        token.starts_with(flag)
    }
}

/// Whether the words appear adjacent in the input, case-insensitive, with
/// surrounding punctuation ignored (`…-c "DROP TABLE users;"` matches).
fn words_match(input: &str, words: &[&str]) -> bool {
    // An empty word list is a malformed rule; never-match keeps the
    // predicate total, and the table review is where it gets caught.
    if words.is_empty() {
        return false;
    }
    let tokens: Vec<&str> = input
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
        .collect();
    tokens.windows(words.len()).any(|window| {
        window
            .iter()
            .zip(words)
            .all(|(t, w)| t.eq_ignore_ascii_case(w))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_push_is_destructive() {
        assert!(is_destructive("Bash", "git push --force origin main"));
        assert!(is_destructive("Bash", "git push -f"));
        assert!(is_destructive(
            "Bash",
            "git push --force-with-lease=origin/main origin main"
        ));
        assert!(is_destructive("Bash", "git -C /repo push --force"));
        assert!(!is_destructive("Bash", "git push origin main"));
    }

    #[test]
    fn plus_refspec_push_is_destructive() {
        assert!(is_destructive("Bash", "git push origin +main"));
    }

    #[test]
    fn remote_branch_deletion_is_destructive() {
        assert!(is_destructive("Bash", "git push origin --delete fix/foo"));
    }

    #[test]
    fn rm_rf_variants_are_destructive() {
        assert!(is_destructive("Bash", "rm -rf /tmp/build"));
        assert!(is_destructive("Bash", "rm -fr target"));
        assert!(is_destructive("Bash", "rm -r -f target"));
        assert!(is_destructive("Bash", "rm --recursive --force target"));
        assert!(is_destructive("Bash", "sudo /bin/rm -rf /var/cache"));
        assert!(!is_destructive("Bash", "rm -r target"));
    }

    #[test]
    fn reset_hard_is_destructive() {
        assert!(is_destructive("Bash", "git reset --hard HEAD~1"));
        assert!(!is_destructive("Bash", "git reset HEAD~1"));
    }

    #[test]
    fn discarding_working_tree_edits_is_destructive() {
        assert!(is_destructive("Bash", "git checkout -- ."));
        assert!(is_destructive("Bash", "git restore src/main.rs"));
        assert!(is_destructive("Bash", "git clean -fd"));
        assert!(!is_destructive("Bash", "git checkout main"));
    }

    #[test]
    fn find_delete_is_destructive() {
        assert!(is_destructive("Bash", "find . -name '*.pyc' -delete"));
        assert!(!is_destructive("Bash", "find . -name '*.pyc'"));
    }

    #[test]
    fn plain_ls_is_not_destructive() {
        assert!(!is_destructive("Bash", "ls -la"));
    }

    #[test]
    fn empty_input_is_not_destructive() {
        assert!(!is_destructive("Bash", ""));
    }

    #[test]
    fn chmod_recursive_is_destructive() {
        assert!(is_destructive("Bash", "chmod -R 755 ."));
        assert!(!is_destructive("Bash", "chmod 644 README.md"));
    }

    #[test]
    fn noclobber_override_redirect_is_destructive() {
        assert!(is_destructive("Bash", "echo done >| status.txt"));
    }

    #[test]
    fn sql_statements_match_sql_and_shell_tools() {
        assert!(is_destructive("execute_sql", "DROP TABLE users;"));
        assert!(is_destructive("execute_sql", "truncate sessions"));
        assert!(is_destructive("execute_sql", "DELETE FROM sessions"));
        assert!(is_destructive("Bash", "psql -c \"drop table users\""));
        assert!(!is_destructive("execute_sql", "SELECT * FROM users"));
    }

    #[test]
    fn unknown_tools_are_never_destructive() {
        assert!(!is_destructive("Write", "rm -rf /"));
        assert!(!is_destructive("Read", "DROP TABLE users"));
    }

    #[test]
    fn query_named_tools_are_not_sql_scoped() {
        assert!(!is_destructive(
            "notion-query-data-sources",
            "truncate the summary column"
        ));
    }

    #[test]
    fn wrapped_commands_still_match() {
        assert!(is_destructive("Bash", "bash -c \"rm -rf /tmp/x\""));
        assert!(is_destructive("Bash", "cd /tmp && rm -rf scratch"));
        assert!(is_destructive("Bash", "xargs rm -rf < list.txt"));
    }

    #[test]
    fn line_continuations_stay_one_command() {
        assert!(is_destructive("Bash", "rm \\\n  -rf /tmp/build"));
    }

    #[test]
    fn prose_mentioning_a_program_is_not_destructive() {
        assert!(!is_destructive("Bash", "echo rm -rf is dangerous"));
        assert!(!is_destructive("Bash", "rg truncate crates/"));
        assert!(!is_destructive("Bash", "man truncate"));
        assert!(!is_destructive(
            "Bash",
            "git commit -m \"truncate old logs\""
        ));
    }

    #[test]
    fn truncate_invocation_is_destructive() {
        assert!(is_destructive("Bash", "truncate -s 0 app.log"));
    }

    #[test]
    fn flags_in_a_later_segment_do_not_combine() {
        assert!(!is_destructive("Bash", "rm -r target && tar -cf out.tar ."));
    }

    #[test]
    fn json_envelope_input_matches_nothing() {
        // The contract: callers pass the extracted command string. The
        // serialized envelope must not half-match.
        assert!(!is_destructive("Bash", "{\"command\":\"rm -rf /\"}"));
    }
}
