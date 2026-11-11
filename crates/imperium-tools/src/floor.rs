//! The safety floor's command check: whether a command line deletes recursively or by force,
//! formats or partitions disks, shuts the machine down, raises privileges, or rewrites git
//! history or discards work. The line is split into its commands (also inside `$( )`, backticks,
//! and parentheses); each is unwrapped from shells (`sh -c`, `bash -lc`, `cmd /c`,
//! `powershell -Command`, …) and wrappers (`env`, `nice`, `command`, `exec`, `nohup`, `time`,
//! `timeout`, `xargs`, `find -exec`); program names are compared without their folder, `.exe`,
//! and case; and the dangerous flags are looked for anywhere in the arguments.
//!
//! This is a best-effort guard against an agent's mistakes, not a security boundary: a script,
//! an alias, or a program that deletes files itself is not seen.

use crate::permissions::split_command;

/// How deep shells inside shells are unwrapped.
const MAX_DEPTH: usize = 4;

/// Whether the safety floor covers this command line.
pub(crate) fn dangerous_command(line: &str) -> bool {
    dangerous_line(line, 0)
}

fn dangerous_line(line: &str, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        // Nested past what is unwrapped: too odd to run without asking.
        return true;
    }
    // `^` escapes characters for cmd (`r^m`); substitutions and groups hold commands too.
    let line: String = line
        .chars()
        .filter(|c| *c != '^')
        .map(|c| {
            if matches!(c, '`' | '(' | ')' | '{' | '}') {
                ';'
            } else {
                c
            }
        })
        .collect();
    let line = line.replace("$;", ";");
    split_command(&line)
        .iter()
        .any(|command| dangerous_words(&words(command), depth))
}

/// A command's words, without surrounding quotes.
fn words(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .map(|word| word.trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|word| !word.is_empty())
        .collect()
}

/// A program's name as compared: no folder, no `.exe`/`.cmd`/`.bat`/`.com`/`.ps1`, lower case.
fn program(word: &str) -> String {
    let name = word
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(word)
        .to_ascii_lowercase();
    for suffix in [".exe", ".cmd", ".bat", ".com", ".ps1"] {
        if let Some(stem) = name.strip_suffix(suffix) {
            return stem.to_string();
        }
    }
    name
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn dangerous_words(words: &[String], depth: usize) -> bool {
    let mut rest = words;
    // Leading `NAME=value` assignments.
    while rest.first().is_some_and(|word| is_assignment(word)) {
        rest = &rest[1..];
    }
    let Some(first) = rest.first() else {
        return false;
    };
    let name = program(first);
    let args = &rest[1..];
    match name.as_str() {
        // Wrappers that run the rest as a command.
        "env" => {
            let skip = args
                .iter()
                .take_while(|word| word.starts_with('-') || is_assignment(word))
                .count();
            dangerous_words(&args[skip..], depth)
        }
        "nice" | "ionice" | "nohup" | "time" | "command" | "exec" | "builtin" | "stdbuf"
        | "chrt" | "taskset" => {
            let mut skip = 0;
            while let Some(word) = args.get(skip) {
                if !word.starts_with('-') {
                    break;
                }
                // `nice -n 5`: an option that takes the next word.
                let takes_value = matches!(word.as_str(), "-n" | "-c" | "-p" | "--adjustment");
                skip += if takes_value { 2 } else { 1 };
            }
            dangerous_words(args.get(skip..).unwrap_or_default(), depth)
        }
        "timeout" => {
            let skip = args.iter().take_while(|word| word.starts_with('-')).count();
            // Then the duration.
            dangerous_words(args.get(skip + 1..).unwrap_or_default(), depth)
        }
        "xargs" => {
            let mut skip = 0;
            while let Some(word) = args.get(skip) {
                if !word.starts_with('-') {
                    break;
                }
                let takes_value = matches!(
                    word.as_str(),
                    "-I" | "-L" | "-n" | "-P" | "-s" | "-d" | "-E" | "-a"
                );
                skip += if takes_value { 2 } else { 1 };
            }
            dangerous_words(args.get(skip..).unwrap_or_default(), depth)
        }
        "find" => find_dangerous(args, depth),
        // Shells running a command string.
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish" | "ash" | "busybox" => {
            match args.iter().position(|word| {
                word.starts_with('-') && !word.starts_with("--") && word.contains('c')
            }) {
                Some(at) => dangerous_line(&args[at + 1..].join(" "), depth + 1),
                None if name == "busybox" => dangerous_words(args, depth),
                None => false,
            }
        }
        "cmd" => match args.iter().position(|word| {
            let word = word.to_ascii_lowercase();
            word.starts_with("/c") || word.starts_with("/k")
        }) {
            Some(at) => {
                // `/c` may be glued to the command (`/crm`); keep what follows it.
                let glued = &args[at][2..];
                let mut line = glued.to_string();
                line.push(' ');
                line.push_str(&args[at + 1..].join(" "));
                dangerous_line(&line, depth + 1)
            }
            None => false,
        },
        "powershell" | "pwsh" => match args.iter().position(|word| {
            let word = word.to_ascii_lowercase();
            word.starts_with('-')
                && (word == "-c"
                    || ("-command".starts_with(&word) && word.len() >= 3)
                    || word == "-encodedcommand"
                    || word == "-e"
                    || word == "-ec")
        }) {
            Some(at) => {
                let flag = args[at].to_ascii_lowercase();
                if flag.starts_with("-e") {
                    // An encoded command cannot be read: ask.
                    return true;
                }
                dangerous_line(&args[at + 1..].join(" "), depth + 1)
            }
            None => false,
        },
        _ => dangerous_program(&name, args),
    }
}

/// `find … -exec cmd … ;` runs `cmd`; `-delete` deletes.
fn find_dangerous(args: &[String], depth: usize) -> bool {
    let mut index = 0;
    while index < args.len() {
        let word = args[index].as_str();
        if word == "-delete" {
            return true;
        }
        if matches!(word, "-exec" | "-execdir" | "-ok" | "-okdir") {
            let end = args[index + 1..]
                .iter()
                .position(|word| matches!(word.as_str(), ";" | "\\;" | "+"))
                .map_or(args.len(), |at| index + 1 + at);
            if dangerous_words(&args[index + 1..end], depth) {
                return true;
            }
            index = end;
        }
        index += 1;
    }
    false
}

/// Short flags (`-rf`) and long flags (`--force`) among `args`.
fn has_short(args: &[String], letters: &[char]) -> bool {
    args.iter().any(|word| {
        word.starts_with('-')
            && !word.starts_with("--")
            && word[1..].chars().any(|c| letters.contains(&c))
    })
}

fn has_long(args: &[String], names: &[&str]) -> bool {
    args.iter().any(|word| {
        let word = word.split('=').next().unwrap_or(word);
        names.contains(&word)
    })
}

/// A Windows-style switch (`/s`, `/S`, `/s/q`), case-insensitively.
fn has_switch(args: &[String], letter: char) -> bool {
    args.iter().any(|word| {
        word.starts_with('/')
            && word
                .split('/')
                .any(|part| part.len() == 1 && part.eq_ignore_ascii_case(&letter.to_string()))
    })
}

/// A PowerShell parameter (`-Recurse`, `-r`, `-rec`), case-insensitively, by any prefix of
/// `name` at least `shortest` long.
fn has_parameter(args: &[String], name: &str, shortest: usize) -> bool {
    args.iter().any(|word| {
        let word = word.to_ascii_lowercase();
        let Some(given) = word.strip_prefix('-') else {
            return false;
        };
        let given = given.split(':').next().unwrap_or(given);
        given.len() >= shortest && name.starts_with(given)
    })
}

fn dangerous_program(name: &str, args: &[String]) -> bool {
    match name {
        "rm" => {
            has_short(args, &['r', 'R', 'f'])
                || has_long(args, &["--recursive", "--force", "--no-preserve-root"])
                // PowerShell's rm is Remove-Item.
                || has_parameter(args, "recurse", 1)
                || has_parameter(args, "force", 2)
        }
        "rmdir" | "rd" => has_switch(args, 's') || has_parameter(args, "recurse", 1),
        "del" | "erase" => {
            has_switch(args, 's')
                || has_switch(args, 'q')
                || has_parameter(args, "recurse", 1)
                || has_parameter(args, "force", 2)
        }
        "remove-item" | "ri" => {
            has_parameter(args, "recurse", 1) || has_parameter(args, "force", 2)
        }
        "shred" | "format" | "diskpart" | "dd" | "shutdown" | "reboot" | "halt" | "poweroff"
        | "sudo" | "su" | "doas" | "runas" | "format-volume" | "clear-disk" => true,
        name if name.starts_with("mkfs") => true,
        "git" => git_dangerous(args),
        _ => false,
    }
}

/// `git` with its global options (`-C dir`, `-c k=v`, `--git-dir=…`) skipped.
fn git_dangerous(args: &[String]) -> bool {
    let mut index = 0;
    while let Some(word) = args.get(index) {
        match word.as_str() {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path" => {
                index += 2;
            }
            word if word.starts_with('-') => index += 1,
            _ => break,
        }
    }
    let Some(subcommand) = args.get(index) else {
        return false;
    };
    let rest = &args[index + 1..];
    match subcommand.as_str() {
        "push" => {
            has_short(rest, &['f', 'd'])
                || has_long(
                    rest,
                    &[
                        "--force",
                        "--force-with-lease",
                        "--force-if-includes",
                        "--delete",
                        "--mirror",
                        "--prune",
                    ],
                )
                // `+ref` forces, `:ref` deletes.
                || rest
                    .iter()
                    .any(|word| !word.starts_with('-') && (word.starts_with('+') || word.starts_with(':')))
        }
        "reset" => has_long(rest, &["--hard", "--merge", "--keep"]),
        "clean" => !has_long(rest, &["--dry-run"]) && !has_short(rest, &['n']),
        "branch" => {
            has_short(rest, &['D'])
                || (has_short(rest, &['d'])
                    && (has_short(rest, &['f']) || has_long(rest, &["--force"])))
                || (has_long(rest, &["--delete"]) && has_long(rest, &["--force"]))
        }
        "checkout" => {
            rest.iter().any(|word| word == "--" || word == ".")
                || has_short(rest, &['f'])
                || has_long(rest, &["--force"])
        }
        "switch" => has_short(rest, &['f']) || has_long(rest, &["--force", "--discard-changes"]),
        "restore" => true,
        "stash" => rest
            .first()
            .is_some_and(|word| matches!(word.as_str(), "drop" | "clear")),
        "filter-branch" | "filter-repo" => true,
        "update-ref" => has_short(rest, &['d']),
        "reflog" => rest
            .first()
            .is_some_and(|word| matches!(word.as_str(), "delete" | "expire")),
        "gc" => has_long(rest, &["--prune"]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destructive_commands_are_caught() {
        for line in [
            "rm -rf build",
            "rm -r -f build",
            "rm build -rf",
            "rm --recursive x",
            "cargo test && rm -fr /",
            "/bin/rm -rf x",
            "RM.EXE -rf x",
            "git push --force origin main",
            "git push -f",
            "git push origin +main",
            "git push origin :feature",
            "git push --force-with-lease",
            "git -C ../repo reset --hard HEAD~1",
            "git -c core.x=y reset --hard",
            "git reset --hard",
            "git clean -fdx",
            "git branch -D old",
            "git checkout -- src",
            "git checkout .",
            "git restore src/main.rs",
            "git stash drop",
            "sudo apt install x",
            "dd if=/dev/zero of=/dev/sda",
            "mkfs.ext4 /dev/sdb1",
            "rd /s /q build",
            "rd /S build",
            "rmdir /s/q build",
            "del /s *.tmp",
            "del /q x",
            "Remove-Item -Recurse build",
            "Remove-Item build -r",
            "ri -r build",
            "ri -Force x",
            "rm -Recurse build",
        ] {
            assert!(dangerous_command(line), "{line}");
        }
    }

    #[test]
    fn wrappers_and_shells_are_unwrapped() {
        for line in [
            "sh -c 'rm -rf build'",
            "bash -lc \"git reset --hard\"",
            "bash -c 'cd x && rm -rf y'",
            "cmd /c rd /s /q build",
            "cmd /C \"del /s x\"",
            "powershell -Command Remove-Item -Recurse x",
            "pwsh -c 'Remove-Item x -Recurse'",
            "powershell -EncodedCommand ZQBjAGgAbwA=",
            "env FOO=1 rm -rf x",
            "env -i rm -rf x",
            "nice -n 10 rm -rf x",
            "command rm -rf x",
            "exec rm -rf x",
            "nohup rm -rf x",
            "timeout 10 rm -rf x",
            "find . -name '*.o' | xargs rm -f",
            "xargs -I {} rm -rf {}",
            "find . -name tmp -exec rm -rf {} ;",
            "find . -delete",
            "echo $(rm -rf x)",
            "echo `rm -rf x`",
            "(rm -rf x)",
            "r^m -rf x",
            "git -C /repo push origin +HEAD:main",
        ] {
            assert!(dangerous_command(line), "{line}");
        }
    }

    #[test]
    fn ordinary_commands_are_not() {
        for line in [
            "cargo test",
            "git push origin main",
            "git push -u origin feature",
            "git reset HEAD~1",
            "git reset --soft HEAD~1",
            "git clean -n",
            "git branch -d merged",
            "git checkout main",
            "git checkout -b new",
            "git stash",
            "git stash pop",
            "rm notes.txt",
            "rmdir empty",
            "del notes.txt",
            "Remove-Item notes.txt",
            "sh -c 'cargo build'",
            "cmd /c dir",
            "find . -name '*.rs'",
            "ls -rf",
            "npm run format",
            "cargo fmt",
        ] {
            assert!(!dangerous_command(line), "{line}");
        }
    }
}
