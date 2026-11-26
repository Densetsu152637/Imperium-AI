//! Replies, Fix CI on the wake path, and the auto-fix budget (pr-card stage 8). A wake for
//! review comments on the diff replies in each comment's thread with the session's summary,
//! defanged and with a footer, only after the user approved its exact text (or edited it), at
//! most `comment_replies_per_hour` per pull request; `comment = "auto"` posts without asking. A
//! failing commit wakes the bound session the same way as review comments, Fix CI for watched
//! pull requests keeps its handled commits and attempts across restarts, and a spent
//! `budget_usd_per_day` stops wakes.

mod pr_world;

use std::time::Duration;

use imperium_api::PermissionMode;
use pr_world::{Poll, REPLY_MARKER, REPO, Script, World, git};
use serde_json::{Value, json};

fn write_greeting() -> Vec<(&'static str, Value)> {
    vec![(
        "write_file",
        json!({"path": "greeting.txt", "content": "hello\n"}),
    )]
}

/// Approves everything, with the reply text as proposed.
fn yes(_: &Value) -> (bool, Option<String>) {
    (true, None)
}

/// Adds a review comment on the diff from alice; returns its thread and id.
fn inline(world: &World, text: &str) -> (String, u64) {
    world
        .fake
        .add_review_comment(REPO, 7, "alice", "MEMBER", "greeting.txt", text)
}

#[tokio::test]
async fn a_reply_waits_for_approval_and_is_posted_verbatim() {
    let mut world = World::new(
        Script::new(write_greeting(), "Ended greeting.txt with a newline."),
        "",
    )
    .await;
    let session = world.new_session();
    world.arm(&session).await;
    let (thread, comment) = inline(&world, "Please end greeting.txt with a newline.");
    let mut proposed = None;
    let seen = world
        .run(
            Poll::Monitor,
            |params| {
                if params["kind"] == "reply" {
                    proposed = params["text"].as_str().map(str::to_string);
                }
                (true, None)
            },
            &["replied", "failed", "declined"],
        )
        .await;
    assert_eq!(
        seen.statuses,
        ["started", "awaiting_approval", "pushed", "replied"],
        "{seen:?}"
    );
    // The push came first, then the reply asked, with its exact text.
    let kinds: Vec<&str> = seen
        .approvals
        .iter()
        .map(|a| a["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.last(), Some(&"reply"), "{kinds:?}");
    let reply = seen.approvals.last().unwrap();
    let text = proposed.expect("the approval carries the text");
    assert_eq!(text, "Ended greeting.txt with a newline.");
    let summary = reply["summary"].as_str().unwrap();
    assert!(summary.contains(&text), "{summary}");
    assert!(summary.contains(&format!("comment {comment}")), "{summary}");
    // Posted once, in the comment's thread, verbatim with the footer.
    let replies = world.replies();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0].0, thread);
    assert!(
        replies[0].1.starts_with(&format!("{text}\n\n---\n")),
        "{replies:?}"
    );
    assert!(replies[0].1.ends_with(REPLY_MARKER), "{replies:?}");
    assert_eq!(seen.events.last().unwrap()["trigger"], "review");
    // The reply does not wake the session.
    world.quiet(Poll::Monitor).await;
}

#[tokio::test]
async fn nothing_is_posted_before_the_reply_is_approved_and_a_declined_one_is_not_posted() {
    let mut world = World::new(Script::new(vec![], "Nothing needed changing."), "").await;
    let session = world.new_session();
    world.arm(&session).await;
    inline(&world, "Is this right?");
    let mut posted_before = None;
    let seen = world
        .run(
            Poll::Monitor,
            |params| {
                if params["kind"] == "reply" {
                    posted_before = Some(());
                    return (false, None);
                }
                (true, None)
            },
            &["reply_declined", "replied", "failed"],
        )
        .await;
    assert!(posted_before.is_some());
    assert_eq!(
        seen.statuses,
        ["started", "nothing", "reply_declined"],
        "{seen:?}"
    );
    assert!(world.replies().is_empty());
}

#[tokio::test]
async fn an_edited_reply_is_posted_instead_and_text_is_refused_for_a_push() {
    let mut world = World::new(Script::new(write_greeting(), "Done as asked."), "").await;
    let session = world.new_session();
    world.arm(&session).await;
    inline(&world, "Please add a newline.");
    while world.notices.try_recv().is_ok() {}
    // Answered through the protocol: a push approval refuses text, a reply takes it.
    let mut push_refused = false;
    let mut statuses = Vec::new();
    world.poll(Poll::Monitor).await;
    loop {
        let notice = world
            .next(Duration::from_secs(60))
            .await
            .expect("the wake went on");
        let params = notice.params;
        match notice.method.as_str() {
            "approval.requested" => {
                let id = params["id"].as_u64().unwrap();
                match params["kind"].as_str().unwrap() {
                    "push" => {
                        let refused = world
                            .rpc(
                                "approvals.respond",
                                json!({"id": id, "approve": true, "text": "x"}),
                            )
                            .await;
                        assert!(refused.is_err(), "{refused:?}");
                        push_refused = true;
                        let found = world
                            .rpc("approvals.respond", json!({"id": id, "approve": true}))
                            .await
                            .unwrap();
                        assert_eq!(found["found"], true);
                    }
                    "reply" => {
                        let found = world
                            .rpc(
                                "approvals.respond",
                                json!({"id": id, "approve": true,
                                       "text": "My own words, @carol."}),
                            )
                            .await
                            .unwrap();
                        assert_eq!(found["found"], true);
                    }
                    _ => {
                        world.daemon.approvals().respond(id, true);
                    }
                }
            }
            "autofix.event" => {
                let status = params["status"].as_str().unwrap().to_string();
                statuses.push(status.clone());
                if ["replied", "failed", "reply_declined"].contains(&status.as_str()) {
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(push_refused);
    assert_eq!(
        statuses.last().map(String::as_str),
        Some("replied"),
        "{statuses:?}"
    );
    // The user's own text is posted, defanged, with the footer.
    let replies = world.replies();
    assert_eq!(replies.len(), 1);
    assert!(
        // The edit is defanged like a proposed reply.
        replies[0].1.starts_with("My own words, `@carol`.\n\n---\n"),
        "{replies:?}"
    );
    assert!(replies[0].1.ends_with(REPLY_MARKER));
}

#[tokio::test]
async fn the_reply_is_defanged() {
    let summary = "Thanks @alice and @owner/team, see https://evil.example/x, [x](http://y) \
                   and <img src=x> www.evil.example";
    let mut world = World::new(Script::new(vec![], summary), "").await;
    let session = world.new_session();
    world.arm(&session).await;
    inline(&world, "Look at this.");
    let mut proposed = String::new();
    let seen = world
        .run(
            Poll::Monitor,
            |params| {
                if params["kind"] == "reply" {
                    proposed = params["text"].as_str().unwrap().to_string();
                }
                (true, None)
            },
            &["replied", "failed"],
        )
        .await;
    assert_eq!(seen.statuses.last().map(String::as_str), Some("replied"));
    // What the user approved is the defanged text, and that is what was posted.
    for text in [proposed.clone(), world.replies()[0].1.clone()] {
        let body = text.trim_end_matches(REPLY_MARKER);
        assert!(
            body.contains("`@alice`") && body.contains("`@owner/team`"),
            "{text}"
        );
        assert!(!body.contains("://"), "{text}");
        assert!(!body.contains("]("), "{text}");
        assert!(!body.contains('<'), "{text}");
        assert!(!body.to_ascii_lowercase().contains("www."), "{text}");
    }
}

#[tokio::test]
async fn over_the_hourly_cap_a_reply_is_held() {
    let mut world = World::new(
        Script::new(vec![], "Looked at both."),
        "[github.autofix]\ncomment_replies_per_hour = 1\n",
    )
    .await;
    let session = world.new_session();
    world.arm(&session).await;
    inline(&world, "First thread.");
    inline(&world, "Second thread.");
    let seen = world.run(Poll::Monitor, yes, &["held"]).await;
    assert_eq!(
        seen.statuses,
        ["started", "nothing", "replied", "held"],
        "{seen:?}"
    );
    let held = seen.events.last().unwrap()["summary"].as_str().unwrap();
    assert!(held.contains("comment_replies_per_hour"), "{held}");
    // Only one reply asked and was posted.
    assert_eq!(
        seen.approvals
            .iter()
            .filter(|a| a["kind"] == "reply")
            .count(),
        1
    );
    assert_eq!(world.replies().len(), 1);
    // The cap is per hour across wakes, and kept in the ledger: a later wake holds too.
    inline(&world, "Third thread.");
    let seen = world.run(Poll::Monitor, yes, &["held"]).await;
    assert_eq!(seen.statuses, ["started", "nothing", "held"], "{seen:?}");
    assert_eq!(world.replies().len(), 1);
}

#[tokio::test]
async fn comment_auto_posts_without_asking() {
    let mut world = World::new(
        Script::new(vec![], "Nothing to change."),
        "[github.autofix]\ncomment = \"auto\"\n",
    )
    .await;
    let session = world.new_session();
    world.arm(&session).await;
    inline(&world, "Fine?");
    let seen = world.run(Poll::Monitor, yes, &["replied", "failed"]).await;
    assert_eq!(seen.statuses, ["started", "nothing", "replied"], "{seen:?}");
    assert!(seen.approvals.iter().all(|a| a["kind"] != "reply"));
    let replies = world.replies();
    assert_eq!(replies.len(), 1);
    assert!(replies[0].1.starts_with("Nothing to change."));
}

#[tokio::test]
async fn conversation_comments_and_reviews_get_no_thread_reply() {
    let mut world = World::new(Script::new(vec![], "Read it."), "").await;
    let session = world.new_session();
    world.arm(&session).await;
    world
        .fake
        .add_issue_comment(REPO, 7, "alice", "MEMBER", "A general remark.");
    world
        .fake
        .add_review(REPO, 7, "alice", "MEMBER", "A review.");
    let seen = world.run(Poll::Monitor, yes, &["nothing", "failed"]).await;
    assert_eq!(seen.statuses, ["started", "nothing"], "{seen:?}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(world.replies().is_empty());
}

#[tokio::test]
async fn a_failing_commit_wakes_the_bound_session_capped_once_and_its_push_asks() {
    let mut world = World::new(
        Script::new(write_greeting(), "Fixed the lint."),
        "[github.autofix]
enabled = true
push = \"auto\"
[[github.watch]]
repo = \"owner/repo\"
project = \"{clone}\"
",
    )
    .await;
    let session = world.new_session();
    let id = session.id().to_string();
    world.arm(&session).await;
    world.fake.set_check(REPO, 7, "lint", Some("FAILURE"));
    // The pull request is watched as well, but its bound session handles it: Fix CI does not.
    world.quiet(Poll::FixCi).await;
    assert_eq!(world.daemon.sessions().len(), 1);
    let before = world.origin_head();
    let (owner, origin) = (std::sync::Arc::clone(&session), world.origin.clone());
    let seen = world
        .run(
            Poll::Monitor,
            move |params| {
                if params["kind"] == "tool" {
                    // The woken turn is capped: even the edit asks.
                    assert!(params["summary"].as_str().unwrap().contains("write_file"));
                    assert_eq!(owner.permission_mode(), PermissionMode::Ask);
                } else {
                    assert_eq!(git(&origin, &["rev-parse", "feature"]), before);
                    // With `push = "auto"` too: a CI log is untrusted text.
                    assert!(
                        params["summary"]
                            .as_str()
                            .unwrap()
                            .contains("failing CI log"),
                        "{params}"
                    );
                }
                (true, None)
            },
            &["pushed", "failed", "declined", "nothing"],
        )
        .await;
    assert_eq!(
        seen.statuses,
        ["started", "awaiting_approval", "pushed"],
        "{seen:?}"
    );
    assert_eq!(seen.events[0]["trigger"], "ci");
    assert_eq!(seen.events[0]["session_id"], id);
    assert!(seen.approvals.iter().any(|a| a["kind"] == "tool"));
    let message = git(&world.origin, &["log", "-1", "--format=%B", "feature"]);
    assert!(message.starts_with("fix: lint on #7"), "{message}");
    assert_eq!(session.permission_mode(), PermissionMode::Default);
    // The prompt had the failing check's log inside the envelope.
    let prompt = world
        .model
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter_map(|r| {
            let last = r["messages"]
                .as_array()?
                .iter()
                .rev()
                .find(|m| m["role"] == "user")?;
            let text = last["content"].to_string();
            text.contains("CI-DATA-").then_some(text)
        })
        .next()
        .unwrap();
    assert!(
        prompt.contains("failed CI") && prompt.contains("log of job"),
        "{prompt}"
    );
    assert_eq!(world.status(&id).await["untrusted_handled"], true);
    // The same failing commit is not handled again.
    world.quiet(Poll::Monitor).await;
}

#[tokio::test]
async fn fix_ci_keeps_its_handled_commit_and_attempts_across_a_restart() {
    let mut world = World::new(
        Script::new(write_greeting(), "Fixed it."),
        "[github.autofix]
enabled = true
max_attempts_per_pr = 1
[[github.watch]]
repo = \"owner/repo\"
project = \"{clone}\"
",
    )
    .await;
    world.fake.set_check(REPO, 7, "lint", Some("FAILURE"));
    let seen = world
        .run(
            Poll::FixCi,
            |params| (params["kind"] != "push", None),
            &["declined", "pushed", "failed", "nothing"],
        )
        .await;
    assert_eq!(
        seen.statuses,
        ["started", "awaiting_approval", "declined"],
        "{seen:?}"
    );
    assert_eq!(seen.events[0]["trigger"], "ci");
    // After a restart the same commit is not fixed again.
    let mut world = world.restart().await;
    world.quiet(Poll::FixCi).await;
    // A new failing commit: the declined push used the only attempt.
    world.fake.force_push(REPO, 7, "def456");
    world.fake.set_check(REPO, 7, "lint", Some("FAILURE"));
    let seen = world.run(Poll::FixCi, yes, &["stopped", "started"]).await;
    assert_eq!(seen.statuses, ["stopped"], "{seen:?}");
    assert!(
        seen.events[0]["summary"]
            .as_str()
            .unwrap()
            .contains("1 fix attempts"),
        "{seen:?}"
    );
}

#[tokio::test]
async fn the_daily_budget_stops_wakes_and_says_so() {
    // Each model call costs $1 (600 prompt and 400 completion tokens at $1000 per million).
    let mut world = World::new(
        Script::new(vec![], "Looked."),
        "[github.autofix]
budget_usd_per_day = 0.5
[endpoints.fake.pricing.m]
input_usd_per_mtok = 1000.0
output_usd_per_mtok = 1000.0
",
    )
    .await;
    let session = world.new_session();
    let id = session.id().to_string();
    world.arm(&session).await;
    world
        .fake
        .add_issue_comment(REPO, 7, "alice", "MEMBER", "First.");
    let seen = world.run(Poll::Monitor, yes, &["nothing", "failed"]).await;
    assert_eq!(seen.statuses, ["started", "nothing"], "{seen:?}");
    // The woken turn's calls counted toward the auto-fix budget.
    let today = chrono::Local::now().date_naive();
    for _ in 0..50 {
        if world.daemon.budgets().spent_today(today, "autofix") >= 1.0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(world.daemon.budgets().spent_today(today, "autofix") >= 1.0);
    world
        .fake
        .add_issue_comment(REPO, 7, "alice", "MEMBER", "Second.");
    world
        .rpc(
            "pull_requests.set_monitor",
            json!({"session_id": id, "url": pr_world::URL, "ack_untrusted": true}),
        )
        .await
        .unwrap();
    let seen = world.run(Poll::Monitor, yes, &["stopped", "started"]).await;
    assert_eq!(seen.statuses, ["stopped"], "{seen:?}");
    let summary = seen.events[0]["summary"].as_str().unwrap();
    assert!(summary.contains("budget_usd_per_day"), "{summary}");
    // Nothing was taken: the comment waits, and the card has no new notice.
    assert_eq!(world.status(&id).await["untrusted_handled"], false);
    // Said once a day, not at every poll.
    world.quiet(Poll::Monitor).await;
}

/// A repository whose own config points `core.hooksPath` at a folder in the worktree (as husky
/// does), with hooks that leave a mark when they run: the daemon's checkout, commit and push of
/// a Fix CI run start none of them.
#[tokio::test]
async fn no_repository_hook_runs_in_the_daemons_git() {
    let mut world = World::new(
        Script::new(write_greeting(), "Fixed it."),
        "[github.autofix]
enabled = true
[[github.watch]]
repo = \"owner/repo\"
project = \"{clone}\"
",
    )
    .await;
    let marks = world.root.path().join("marks");
    std::fs::create_dir_all(&marks).unwrap();
    let hooks = world.clone.join(".husky");
    std::fs::create_dir_all(&hooks).unwrap();
    let mark_dir = marks.display().to_string().replace('\\', "/");
    for hook in [
        "pre-commit",
        "prepare-commit-msg",
        "commit-msg",
        "post-commit",
        "pre-push",
        "post-checkout",
        "reference-transaction",
    ] {
        let path = hooks.join(hook);
        std::fs::write(&path, format!("#!/bin/sh\ntouch \"{mark_dir}/{hook}\"\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    git(&world.clone, &["add", "-A"]);
    git(&world.clone, &["commit", "-q", "-m", "add hooks"]);
    git(&world.clone, &["push", "-q", "origin", "feature"]);
    // Only now, so the setup's own git calls ran no hook.
    git(&world.clone, &["config", "core.hooksPath", ".husky"]);
    let before = world.origin_head();
    world.fake.set_check(REPO, 7, "lint", Some("FAILURE"));
    let seen = world
        .run(
            Poll::FixCi,
            yes,
            &["pushed", "failed", "declined", "nothing"],
        )
        .await;
    assert_eq!(
        seen.statuses,
        ["started", "awaiting_approval", "pushed"],
        "{seen:?}"
    );
    assert_ne!(world.origin_head(), before);
    let ran: Vec<String> = std::fs::read_dir(&marks)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(ran.is_empty(), "hooks ran: {ran:?}");
}
