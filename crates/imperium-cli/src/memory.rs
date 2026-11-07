//! `imperium memory`: browse and edit saved memories directly in their folders.

use std::path::PathBuf;

use anyhow::{Context, bail};
use imperium_config::Paths;
use imperium_memory::{Scope, Store};

fn store(project: Option<PathBuf>) -> anyhow::Result<Store> {
    let paths = Paths::from_env()?;
    let project = match project {
        Some(project) => project,
        None => std::env::current_dir().context("could not read the current directory")?,
    };
    Ok(Store::new(paths.data_dir(), Some(&project)))
}

pub fn list(project: Option<PathBuf>) -> anyhow::Result<()> {
    let store = store(project)?;
    let entries = store.list();
    if entries.is_empty() {
        println!("No memories saved for this project or globally.");
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

pub fn search(query: &str, project: Option<PathBuf>) -> anyhow::Result<()> {
    for entry in store(project)?.search(query, 10) {
        println!(
            "{:<32} {:<16} {}",
            entry.name,
            entry.scope.as_str(),
            entry.description
        );
    }
    Ok(())
}

pub fn show(name: &str, project: Option<PathBuf>) -> anyhow::Result<()> {
    let Some(entry) = store(project)?.get(name) else {
        bail!("no memory named {name}");
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

pub fn forget(name: &str, scope: Option<String>, project: Option<PathBuf>) -> anyhow::Result<()> {
    let store = store(project)?;
    let scope = match scope {
        Some(scope) => Scope::parse(&scope).with_context(|| {
            format!("unknown scope {scope}; use global, project, or project-private")
        })?,
        None => store
            .get(name)
            .map(|entry| entry.scope)
            .with_context(|| format!("no memory named {name}"))?,
    };
    if store.forget(name, scope)? {
        println!("forgot {name} ({})", scope.as_str());
    } else {
        println!("{name} is not saved in {}", scope.as_str());
    }
    Ok(())
}
