//! The built-in prompts of `/code-review` and `/security-review`. Like `/init` they are sent as an
//! ordinary user message (`session.builtin`): the agent reads the changes with its normal tools
//! (`git`, `gh` when it is installed, the file tools) under the session's permissions, and the
//! prompts tell it to report, not to edit. One agent does the whole review.

/// Which review a prompt asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewKind {
    /// Correctness bugs.
    Code,
    /// Vulnerabilities in the changes.
    Security,
}

/// How hard the review looks: few findings it is sure of, up to many it is less sure of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReviewEffort {
    Low,
    #[default]
    Medium,
    High,
    Max,
}

/// What to review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewTarget {
    /// No target: the uncommitted changes and the branch's commits against its base branch.
    Pending,
    /// A pull request of this repository, by number.
    PullRequest(u64),
    /// A pull request by its URL, which may be of another repository: `gh` gets the URL itself.
    PullRequestUrl(String),
    /// A branch or a path; the agent works out which.
    Named(String),
}

/// The arguments of a review command: an optional effort word, then an optional target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewArgs {
    pub effort: ReviewEffort,
    pub target: ReviewTarget,
}

impl ReviewArgs {
    /// Reads `[low|medium|high|xhigh|max] [target]`. The target is a pull request number
    /// (`123`, `#123` or a pull request URL), or text naming a branch or a path.
    pub fn parse(args: &str) -> ReviewArgs {
        let args = args.trim();
        let (first, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
        let effort = match first.to_ascii_lowercase().as_str() {
            "low" => Some(ReviewEffort::Low),
            "medium" => Some(ReviewEffort::Medium),
            "high" => Some(ReviewEffort::High),
            "xhigh" | "max" => Some(ReviewEffort::Max),
            _ => None,
        };
        let (effort, target) = match effort {
            Some(effort) => (effort, rest.trim()),
            None => (ReviewEffort::default(), args),
        };
        ReviewArgs {
            effort,
            target: parse_target(target),
        }
    }
}

fn parse_target(text: &str) -> ReviewTarget {
    if text.is_empty() {
        return ReviewTarget::Pending;
    }
    if let Ok(number) = text.trim_start_matches('#').parse::<u64>() {
        return ReviewTarget::PullRequest(number);
    }
    // `https://github.com/owner/repo/pull/123`, possibly with a trailing path or fragment. The
    // URL is kept whole: it may name another repository than the session's.
    let one_word = !text.contains(|c: char| c.is_whitespace() || c == '`');
    if one_word && (text.starts_with("http://") || text.starts_with("https://")) {
        let is_pull_request = text
            .split(['/', '?', '#'])
            .skip_while(|part| *part != "pull")
            .nth(1)
            .is_some_and(|part| part.parse::<u64>().is_ok());
        if is_pull_request {
            return ReviewTarget::PullRequestUrl(text.to_string());
        }
    }
    ReviewTarget::Named(text.to_string())
}

/// The prompt of a review command; `args` is the text after the command name.
pub fn review_prompt(kind: ReviewKind, args: &str) -> String {
    let ReviewArgs { effort, target } = ReviewArgs::parse(args);
    let mut prompt = String::new();
    prompt.push_str(match kind {
        ReviewKind::Code => {
            "Review code changes for correctness bugs: behaviour that is wrong, crashes, loses \
             data, races, mishandles errors, edge cases or cancellation, breaks a caller, or \
             contradicts what the change says it does. Style, naming and taste are not findings.\n\n"
        }
        ReviewKind::Security => {
            "Review code changes for security vulnerabilities. Cover at least: injection (SQL, \
             command, template, prompt, header, log), missing or bypassable authentication and \
             authorisation (including IDOR and confused-deputy cases), secrets and credentials in \
             code, config or logs, path traversal and unsafe file handling, SSRF and open \
             redirects, XSS and CSRF, unsafe deserialisation, weak or misused cryptography and \
             randomness, race conditions that cross a trust boundary, unsafe use of `unsafe`, \
             FFI or `eval`, dependency and supply-chain changes, and excessive permissions. \
             Follow untrusted input from where it enters to where it is used.\n\n"
        }
    });
    prompt.push_str("What to review:\n");
    prompt.push_str(&match &target {
        ReviewTarget::Pending => String::from(
            "- The pending changes. Run `git status`, `git diff HEAD` (staged and unstaged) and \
             read untracked files that belong to the change. Also review the commits of this \
             branch that its base branch lacks: find the base (the upstream, `origin/HEAD`, or \
             `main`/`master`), then `git diff <base>...HEAD`. If there are no changes at all, \
             say so and stop.\n",
        ),
        ReviewTarget::PullRequestUrl(url) => format!(
            "- The pull request at `{url}`, which may be in another repository than this one. Use \n             `gh pr view {url}` and `gh pr diff {url}` with the URL exactly as given, when `gh` is \n             installed and signed in. If it is not, say so and stop. Do not check anything out; read \n             this repository's files only where they are the same project.
"
        ),
        ReviewTarget::PullRequest(number) => format!(
            "- Pull request #{number}. Use `gh pr view {number}` and `gh pr diff {number}` when \
             `gh` is installed and signed in. If it is not, say so; you may try `git fetch origin \
             pull/{number}/head` and diff it against the base branch, which the user approves \
             like any command. Do not check the branch out.\n"
        ),
        ReviewTarget::Named(name) => format!(
            "- `{name}`. If it names a branch or ref (check with `git rev-parse --verify`), review \
             what that branch changes against its base: `git diff <base>...<ref>`. Otherwise, if \
             it is a file or folder, review its current contents and, where it is tracked, its \
             uncommitted and most recent changes. If it is neither, say so and stop.\n"
        ),
    });
    prompt.push_str(
        "- Read the changed files around the diff, and the callers and callees the change \
         touches, so findings rest on the real code and not on the diff alone.\n\n",
    );
    prompt.push_str(match effort {
        ReviewEffort::Low => {
            "Effort: low. Look for the few problems that matter most. Report only findings you \
             are highly confident are real, at most five.\n\n"
        }
        ReviewEffort::Medium => {
            "Effort: medium. Report the real problems you are confident about; leave out \
             speculation.\n\n"
        }
        ReviewEffort::High => {
            "Effort: high. Be thorough: trace each changed path and its callers, and check the \
             failure and edge paths. Include findings you are only moderately confident of, \
             marked as such.\n\n"
        }
        ReviewEffort::Max => {
            "Effort: max. Be exhaustive: go through every changed file and every path it \
             affects. Include uncertain findings, marked with their confidence, and say what you \
             could not check.\n\n"
        }
    });
    prompt.push_str(
        "Report. Do not edit, create or delete any file, and do not run builds, tests or \
         anything that changes state, unless the user asks you to after the review. Use \
         read-only tools. For each finding give: the file and line (`path:line`); a severity \
         (critical, high, medium or low); your confidence (high, medium or low); what is wrong \
         and why, in a few sentences",
    );
    prompt.push_str(match kind {
        ReviewKind::Code => ", with the input or sequence that triggers it; and a suggested fix",
        ReviewKind::Security => {
            ", with exploitability reasoning (who can reach it, what they control, what they \
             gain, and what already limits it); and a suggested fix"
        }
    });
    prompt.push_str(
        ". Order the findings by severity. Do not pad the list: if you find nothing, say what \
         you checked and that you found nothing. End with a one-line summary.",
    );
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_are_an_optional_effort_then_an_optional_target() {
        let parse = ReviewArgs::parse;
        assert_eq!(parse("").target, ReviewTarget::Pending);
        assert_eq!(parse("").effort, ReviewEffort::Medium);
        assert_eq!(parse("123").target, ReviewTarget::PullRequest(123));
        assert_eq!(parse(" #45 ").target, ReviewTarget::PullRequest(45));
        assert_eq!(
            parse("high 7"),
            ReviewArgs {
                effort: ReviewEffort::High,
                target: ReviewTarget::PullRequest(7)
            }
        );
        assert_eq!(parse("LOW").effort, ReviewEffort::Low);
        assert_eq!(parse("LOW").target, ReviewTarget::Pending);
        assert_eq!(parse("xhigh").effort, ReviewEffort::Max);
        assert_eq!(
            parse("max feat/x").target,
            ReviewTarget::Named("feat/x".into())
        );
        assert_eq!(
            parse("src/lib.rs").target,
            ReviewTarget::Named("src/lib.rs".into())
        );
        // An effort word with a longer name is a target, not an effort.
        assert_eq!(parse("highway").effort, ReviewEffort::Medium);
        assert_eq!(
            parse("highway").target,
            ReviewTarget::Named("highway".into())
        );
        assert_eq!(
            parse("https://github.com/o/r/pull/99/files#x").target,
            ReviewTarget::PullRequestUrl("https://github.com/o/r/pull/99/files#x".into())
        );
        assert_eq!(
            parse("high https://github.com/o/r/pull/9").target,
            ReviewTarget::PullRequestUrl("https://github.com/o/r/pull/9".into())
        );
        // Not one word: a target the agent reads, not a URL handed to `gh`.
        assert_eq!(
            parse("https://github.com/o/r/pull/9 `x`").target,
            ReviewTarget::Named("https://github.com/o/r/pull/9 `x`".into())
        );
        assert_eq!(
            parse("https://example.com/page").target,
            ReviewTarget::Named("https://example.com/page".into())
        );
    }

    #[test]
    fn a_pull_request_url_goes_to_gh_whole_so_another_repository_is_reviewed() {
        let url = "https://github.com/other/repo/pull/7";
        let prompt = review_prompt(ReviewKind::Code, url);
        assert!(prompt.contains(&format!("`gh pr view {url}`")));
        assert!(prompt.contains(&format!("`gh pr diff {url}`")));
        assert!(!prompt.contains("gh pr diff 7"));
    }

    #[test]
    fn every_review_prompt_reports_findings_and_does_not_edit() {
        for kind in [ReviewKind::Code, ReviewKind::Security] {
            let prompt = review_prompt(kind, "");
            assert!(prompt.contains("`path:line`"));
            assert!(prompt.contains("severity"));
            assert!(prompt.contains("confidence"));
            assert!(prompt.contains("suggested fix"));
            assert!(prompt.contains("Do not edit"));
            assert!(prompt.contains("git diff HEAD"));
            assert!(prompt.contains("Effort: medium"));
        }
    }

    #[test]
    fn the_prompts_differ_by_kind_and_carry_the_target_and_effort() {
        let code = review_prompt(ReviewKind::Code, "low 123");
        assert!(code.contains("correctness bugs"));
        assert!(code.contains("gh pr diff 123"));
        assert!(code.contains("Effort: low"));
        assert!(!code.contains("exploitability"));
        let security = review_prompt(ReviewKind::Security, "max release/1.2");
        assert!(security.contains("path traversal"));
        assert!(security.contains("injection"));
        assert!(security.contains("exploitability"));
        assert!(security.contains("`release/1.2`"));
        assert!(security.contains("Effort: max"));
        assert!(!security.contains("gh pr diff"));
    }
}
