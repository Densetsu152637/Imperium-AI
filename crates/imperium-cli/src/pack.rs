//! `imperium pack`: policy pack updates. A pack is fetched from a source the user added, shown
//! as a diff against the pinned version, and pinned only when the user accepts it. Nothing here
//! talks to the daemon: sources live in the user's config folder, pins in the data folder, and
//! sessions read the pins when they start.

use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use imperium_config::Paths;
use imperium_workflow::update::{
    Diff, MAX_FILE_BYTES, MAX_FILES, MIN_HASH_PREFIX, PackStore, Pending, PinnedSnapshot, Snapshot,
    Source, Sources, check_revision, check_url, valid_name,
};

/// Longest a fetch may take.
const GIT_TIMEOUT: Duration = Duration::from_secs(300);

fn store(paths: &Paths) -> PackStore {
    PackStore::new(paths.data_dir())
}

fn short(hash: &str) -> &str {
    &hash[..MIN_HASH_PREFIX.min(hash.len())]
}

pub fn list() -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let sources = Sources::load(paths.config_dir())?;
    if sources.iter().next().is_none() {
        println!(
            "No pack sources. Add one with: imperium pack add-source <name> <git-url>\n\
             (sources are kept in {})",
            Sources::path(paths.config_dir()).display()
        );
        return Ok(());
    }
    let store = store(&paths);
    for (name, source) in sources.iter() {
        let revision = source
            .revision
            .as_deref()
            .map(|r| format!(" ({r})"))
            .unwrap_or_default();
        println!("{name}  {}{revision}", source.url);
        match store.pinned_snapshot(name) {
            Ok(Some(found)) => println!(
                "  pinned   sha256:{} at commit {}",
                short(&found.pin.hash),
                short(&found.pin.commit)
            ),
            Ok(None) => println!("  pinned   nothing yet"),
            Err(error) => println!("  pinned   problem: {error}"),
        }
        match store.pending(name) {
            Ok(Some((pending, _))) => println!(
                "  pending  sha256:{} (review with: imperium pack diff {name})",
                short(&pending.hash)
            ),
            Ok(None) => {}
            Err(error) => println!("  pending  problem: {error}"),
        }
    }
    Ok(())
}

pub fn add_source(name: String, url: String, revision: Option<String>) -> anyhow::Result<()> {
    if !valid_name(&name) {
        bail!(
            "{name:?} cannot name a pack source: use 1-40 letters, digits, '-', '_' or '.', not starting with '.'"
        );
    }
    check_url(&url)?;
    if let Some(revision) = &revision {
        check_revision(revision)?;
    }
    let paths = Paths::from_env()?;
    // A pack's name prefixes its references (`<name>:<path>`), so a source may not share the
    // name of a pack already listed in workflow.packs.
    let settings = imperium_config::load(&paths, None, std::env::vars())?.settings()?;
    for root in &settings.workflow.packs {
        let listed = imperium_workflow::Pack::load(root)
            .map(|pack| pack.name)
            .unwrap_or_else(|_| {
                root.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        if listed == name {
            bail!(
                "{} in workflow.packs is already called {name}; pick another source name",
                root.display()
            );
        }
    }
    let mut sources = Sources::load(paths.config_dir())?;
    sources.add(&name, Source { url, revision })?;
    sources.save(paths.config_dir())?;
    println!("Added pack source {name}. Fetch it with: imperium pack check {name}");
    Ok(())
}

pub fn remove_source(name: String) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let mut sources = Sources::load(paths.config_dir())?;
    if !sources.remove(&name) {
        bail!("no pack source named {name}");
    }
    sources.save(paths.config_dir())?;
    store(&paths).forget(&name)?;
    println!(
        "Removed {name}, its pin and any pending update. Sessions started from now on no longer use it."
    );
    Ok(())
}

pub fn check(name: Option<String>) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let sources = Sources::load(paths.config_dir())?;
    let names: Vec<String> = match name {
        Some(name) => {
            if sources.get(&name).is_none() {
                bail!("no pack source named {name}; see imperium pack list");
            }
            vec![name]
        }
        None => sources.iter().map(|(name, _)| name.clone()).collect(),
    };
    if names.is_empty() {
        println!("No pack sources. Add one with: imperium pack add-source <name> <git-url>");
        return Ok(());
    }
    let store = store(&paths);
    let mut failed = 0;
    for name in names {
        let source = sources.get(&name).expect("the name came from the sources");
        if let Err(error) = check_one(&store, &name, source) {
            eprintln!("{name}: {error:#}");
            failed += 1;
        }
    }
    if failed > 0 {
        bail!("{failed} pack source(s) could not be checked");
    }
    Ok(())
}

fn check_one(store: &PackStore, name: &str, source: &Source) -> anyhow::Result<()> {
    let (snapshot, commit) = fetch(store, source)?;
    if snapshot.is_empty() {
        bail!("the source has no files");
    }
    let (pinned, note) = baseline(store, name);
    let hash = snapshot.hash();
    if pinned.as_ref().is_some_and(|found| found.pin.hash == hash) {
        store.reject(name)?;
        println!("{name}: up to date (sha256:{})", short(&hash));
        return Ok(());
    }
    let diff = Diff::between(pinned.as_ref().map(|found| &found.snapshot), &snapshot);
    let pending = store.stage(name, &source.url, &commit, &snapshot)?;
    println!("{name}: an update is waiting for your decision");
    print_update(
        &source.url,
        &pending,
        pinned.as_ref().map(|p| p.pin.hash.as_str()),
    );
    if !snapshot.skipped.is_empty() {
        println!(
            "  left out {} entries that are not regular files",
            snapshot.skipped.len()
        );
    }
    if let Some(note) = note {
        println!("\n{note}");
    }
    println!("{}", diff.render(false));
    println!("Nothing has been applied; sessions keep using the pinned version.");
    println!(
        "To apply it: imperium pack accept {name} --hash {}{}\nTo discard it: imperium pack reject {name}",
        short(&pending.hash),
        if diff.has_loosening() {
            " --allow-loosening"
        } else {
            ""
        }
    );
    Ok(())
}

/// The pinned version to compare with and, when there is none, what that means for the review.
fn baseline(store: &PackStore, name: &str) -> (Option<PinnedSnapshot>, Option<String>) {
    let bundled = if store.pins().map(|pins| pins.is_empty()).unwrap_or(true) {
        " With no folders in workflow.packs, the bundled Agent-Custom-Workflow snapshot also \
         stops loading once this is the first pack pinned."
    } else {
        ""
    };
    match store.pinned_snapshot(name) {
        Ok(Some(found)) => (Some(found), None),
        Ok(None) => (
            None,
            Some(format!(
                "NO COMPARISON: nothing is pinned for {name}, so this version is shown in full \
                 and nothing can be flagged as loosening.{bundled}"
            )),
        ),
        Err(error) => (
            None,
            Some(format!(
                "NO COMPARISON: the pinned version of {name} could not be verified ({error}), so \
                 this version is shown in full and nothing can be flagged as loosening.{bundled}"
            )),
        ),
    }
}

fn print_update(url: &str, pending: &Pending, pinned: Option<&str>) {
    println!("  source   {url}");
    println!("  commit   {}", pending.commit);
    match pinned {
        Some(hash) => println!("  pinned   sha256:{hash}"),
        None => println!("  pinned   nothing yet"),
    }
    println!("  new      sha256:{}", pending.hash);
}

pub fn diff(name: String, full: bool) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let store = store(&paths);
    let Some((pending, snapshot)) = store.pending(&name)? else {
        bail!("{name} has no pending update; run imperium pack check {name}");
    };
    let (pinned, note) = baseline(&store, &name);
    let diff = Diff::between(pinned.as_ref().map(|found| &found.snapshot), &snapshot);
    print_update(
        &pending.url,
        &pending,
        pinned.as_ref().map(|p| p.pin.hash.as_str()),
    );
    if let Some(note) = note {
        println!("\n{note}");
    }
    println!("{}", diff.render(full));
    Ok(())
}

pub fn accept(name: String, hash: Option<String>, allow_loosening: bool) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let store = store(&paths);
    let Some((pending, snapshot)) = store.pending(&name)? else {
        bail!("{name} has no pending update; run imperium pack check {name}");
    };
    let id = short(&pending.hash).to_string();
    let (pinned, note) = baseline(&store, &name);
    if let Some(note) = &note {
        println!("{note}");
    }
    let (hash, allow) = match hash {
        Some(hash) => (hash, allow_loosening),
        None => {
            if !std::io::stdin().is_terminal() {
                bail!(
                    "accepting needs your answer at a terminal, or pass --hash {id} after reading \
                     imperium pack diff {name}"
                );
            }
            let diff = Diff::between(pinned.as_ref().map(|found| &found.snapshot), &snapshot);
            println!("{}", diff.render(false));
            let loosening = diff.has_loosening();
            let answer = if loosening {
                format!("accept {id} loosening")
            } else {
                format!("accept {id}")
            };
            print!("Type `{answer}` to pin this version, anything else to leave it pending: ");
            std::io::stdout().flush()?;
            let mut typed = String::new();
            std::io::stdin().read_line(&mut typed)?;
            if typed.trim() != answer {
                println!("Not applied; the update stays pending.");
                return Ok(());
            }
            (pending.hash.clone(), loosening)
        }
    };
    let pin = store.accept(&name, &hash, allow)?;
    println!(
        "Pinned {name} at sha256:{} (commit {}). Sessions started from now on use it.",
        short(&pin.hash),
        short(&pin.commit)
    );
    Ok(())
}

pub fn reject(name: String) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    if store(&paths).reject(&name)? {
        println!("Discarded the pending update for {name}; the pinned version is unchanged.");
    } else {
        println!("{name} has no pending update.");
    }
    Ok(())
}

/// Removes its folder when dropped.
struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Clones the source and reads its files. Nothing from the clone runs: hooks are off, the
/// `ext::` transport is refused, there are no prompts, LFS downloads or submodules, and `.git`
/// and anything that is not a regular file is left out of the pack. The clone is made without
/// a checkout and without files over the size limit; the file count and any omitted file are
/// checked before a single file is written to disk.
fn fetch(store: &PackStore, source: &Source) -> anyhow::Result<(Snapshot, String)> {
    let scratch = store.scratch();
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("could not create {}", scratch.display()))?;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let work = scratch.join(format!("{}-{unique}", std::process::id()));
    let _cleanup = Cleanup(work.clone());
    let repo = work.join("repo");
    let hooks = work.join("no-hooks");
    let repo_text = repo.display().to_string();
    let limit = format!("--filter=blob:limit={MAX_FILE_BYTES}");
    let mut args: Vec<&str> = vec![
        "clone",
        "--quiet",
        "--no-recurse-submodules",
        "--no-checkout",
        "--depth",
        "1",
        limit.as_str(),
    ];
    if let Some(revision) = &source.revision {
        args.extend(["--branch", revision.as_str()]);
    }
    args.extend(["--", source.url.as_str(), repo_text.as_str()]);
    git(&args, None, &hooks)?;
    let listing = git(&["ls-tree", "-r", "HEAD"], Some(&repo), &hooks)?;
    if listing.lines().count() > MAX_FILES {
        bail!("the source has more than {MAX_FILES} files");
    }
    let objects = git(
        &["rev-list", "--objects", "--missing=print", "HEAD"],
        Some(&repo),
        &hooks,
    )?;
    if objects.lines().any(|line| line.starts_with('?')) {
        bail!(
            "the source has a file over {} MiB, which a pack may not",
            MAX_FILE_BYTES / (1024 * 1024)
        );
    }
    git(&["reset", "--hard", "--quiet", "HEAD"], Some(&repo), &hooks)?;
    let commit = git(&["rev-parse", "HEAD"], Some(&repo), &hooks)?;
    let snapshot = Snapshot::read(&repo)?;
    Ok((snapshot, commit.trim().to_string()))
}

fn git(args: &[&str], dir: Option<&Path>, hooks: &Path) -> anyhow::Result<String> {
    let mut command = Command::new("git");
    // Nothing of the caller's git environment (a GIT_DIR, GIT_CONFIG_*, an askpass helper)
    // steers a fetch of a pack.
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(&name);
        }
    }
    command
        .args(["-c", "protocol.ext.allow=never"])
        .args(["-c", "core.symlinks=false"])
        .args(["-c", "core.fsmonitor=false"])
        .args(["-c", &format!("core.hooksPath={}", hooks.display())])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let mut child = command
        .spawn()
        .context("could not run git; it must be installed to fetch packs")?;
    // Both pipes are drained as the process runs, so a chatty git cannot fill one and stall.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + GIT_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("git took longer than 5 minutes");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if !status.success() {
        bail!("git {} failed: {}", args[0], stderr.trim());
    }
    Ok(stdout)
}
