//! `imperium memory`: browse and edit the user's global memories directly in their folder.
//!
//! An environment's memory is its `Memory.md` (open it from the environment's window, or find
//! the folders with `imperium workspaces`); it is a file to edit, not entries to list here.

use anyhow::{Context, bail};
use imperium_config::Paths;
use imperium_memory::{Scope, Store};

fn store() -> anyhow::Result<Store> {
    let paths = Paths::from_env()?;
    Ok(Store::new(paths.data_dir()))
}

pub fn list() -> anyhow::Result<()> {
    let entries = store()?.list();
    if entries.is_empty() {
        println!("No global memories saved. An environment's notes are in its Memory.md.");
    }
    for entry in entries {
        println!(
            "{:<32} {:<16} {:<10} {}",
            entry.name,
            entry.scope.as_str(),
            entry.kind,
            entry.description
        );
    }
    Ok(())
}

pub fn search(query: &str) -> anyhow::Result<()> {
    for entry in store()?.search(query, 10) {
        println!(
            "{:<32} {:<16} {}",
            entry.name,
            entry.scope.as_str(),
            entry.description
        );
    }
    Ok(())
}

pub fn show(name: &str) -> anyhow::Result<()> {
    let Some(entry) = store()?.get(name) else {
        bail!("no global memory named {name}");
    };
    println!(
        "{} ({}, {}, saved {} from {})\n{}\nfile: {}\n\n{}",
        entry.name,
        entry.scope.as_str(),
        entry.kind,
        entry.created,
        entry.source,
        entry.description,
        entry.path.display(),
        entry.body
    );
    Ok(())
}

pub fn forget(name: &str, scope: Option<String>) -> anyhow::Result<()> {
    let store = store()?;
    let scope = match scope {
        Some(scope) => Scope::parse(&scope).with_context(|| {
            format!("unknown scope {scope}; only global memories are listed here")
        })?,
        None => Scope::Global,
    };
    if scope != Scope::Global {
        bail!(
            "environment memory is the Memory.md file of the environment: edit it there \
             (`imperium workspaces` shows where)"
        );
    }
    if store.forget(name, scope)? {
        println!("forgot {name} ({})", scope.as_str());
    } else {
        println!("{name} is not saved in {}", scope.as_str());
    }
    Ok(())
}
