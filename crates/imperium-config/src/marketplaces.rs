//! Adding, changing, and removing `[plugins.marketplaces.<name>]` entries in the user's
//! `config.toml`. Like [`crate::save_mcp_server`], edits keep the file's comments, formatting, and
//! unknown keys, and an edit that leaves the configuration invalid (the official marketplace's
//! name pointed at another source, for one) is undone.
//!
//! The official marketplace is built in: it needs no entry to exist. Turning it off writes
//! `enabled = false` under its name; nothing here can give that name another source.

use toml_edit::{DocumentMut, Item, Table, TableLike, Value};

use crate::mcp_servers::set_key;
use crate::registry::edit_user_file;
use crate::{MarketplaceSettings, OFFICIAL_MARKETPLACE, Paths, SetError};

/// Whether `name` may name a marketplace: 1-64 letters, digits, `.`, `_` or `-`, not starting
/// with a dot and not ending with one (Windows drops a trailing dot, so `x.` would be `x`). Names
/// are also unique ignoring case ([`crate::Settings::validate`]), since some file systems are.
pub fn valid_marketplace_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !name.ends_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn check_name(name: &str) -> Result<(), SetError> {
    if valid_marketplace_name(name) {
        Ok(())
    } else {
        Err(SetError::InvalidValue {
            key: format!("plugins.marketplaces.{name}"),
            message: "a marketplace name is 1-64 letters, digits, '.', '_' or '-', not starting \
                      or ending with a dot"
                .into(),
        })
    }
}

/// Writes `[plugins.marketplaces.<name>]` with `source` and `revision`, keeping other keys of an
/// existing entry (and its `enabled`). Returns the entry as loaded back.
pub fn save_marketplace(
    paths: &Paths,
    name: &str,
    source: &str,
    revision: Option<&str>,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<MarketplaceSettings, SetError> {
    check_name(name)?;
    if source.trim().is_empty() {
        return Err(SetError::InvalidValue {
            key: format!("plugins.marketplaces.{name}.source"),
            message: "a marketplace needs a source".into(),
        });
    }
    let config = edit_user_file(paths, env, |document| {
        let entry = marketplace_table(document, name, true)?.expect("created");
        set_key(entry, "source", Some(Value::from(source)));
        set_key(entry, "revision", revision.map(Value::from));
        Ok(())
    })?;
    loaded(&config, name)
}

/// Turns a marketplace on or off with its `enabled` key. The built-in one needs no entry first.
pub fn set_marketplace_enabled(
    paths: &Paths,
    name: &str,
    enabled: bool,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<MarketplaceSettings, SetError> {
    check_name(name)?;
    let built_in = name == OFFICIAL_MARKETPLACE;
    let config = edit_user_file(paths, env, |document| {
        let entry = marketplace_table(document, name, built_in)?
            .ok_or_else(|| format!("no [plugins.marketplaces.{name}] in the user config"))?;
        set_key(entry, "enabled", (!enabled).then(|| false.into()));
        Ok(())
    })?;
    if built_in && enabled {
        // An entry that only said "on" is the default again: leave no empty table behind.
        edit_user_file(paths, [], |document| {
            if let Some(entry) = marketplace_table(document, name, false)?
                && entry.is_empty()
            {
                remove_entry(document, name);
            }
            Ok(())
        })?;
    }
    loaded(&config, name)
}

/// Removes `[plugins.marketplaces.<name>]` from the user config; `false` when there was none.
pub fn remove_marketplace(
    paths: &Paths,
    name: &str,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<bool, SetError> {
    check_name(name)?;
    let mut found = false;
    edit_user_file(paths, env, |document| {
        found = remove_entry(document, name);
        Ok(())
    })?;
    Ok(found)
}

fn remove_entry(document: &mut DocumentMut, name: &str) -> bool {
    document
        .get_mut("plugins")
        .and_then(Item::as_table_like_mut)
        .and_then(|plugins| plugins.get_mut("marketplaces"))
        .and_then(Item::as_table_like_mut)
        .is_some_and(|marketplaces| marketplaces.remove(name).is_some())
}

fn loaded(config: &crate::Config, name: &str) -> Result<MarketplaceSettings, SetError> {
    let settings = config.settings().map_err(SetError::Rejected)?;
    settings
        .plugins
        .marketplaces
        .get(name)
        .cloned()
        .ok_or_else(|| SetError::InvalidValue {
            key: format!("plugins.marketplaces.{name}"),
            message: "an environment variable removes this marketplace".into(),
        })
}

/// The `[plugins.marketplaces.<name>]` table, created (with implicit parents) when `create` is set.
fn marketplace_table<'a>(
    document: &'a mut DocumentMut,
    name: &str,
    create: bool,
) -> Result<Option<&'a mut dyn TableLike>, String> {
    let mut table: &mut dyn TableLike = document.as_table_mut();
    for (part, implicit) in [("plugins", true), ("marketplaces", true), (name, false)] {
        if table.get(part).is_none() {
            if !create {
                return Ok(None);
            }
            let mut child = Table::new();
            child.set_implicit(implicit);
            table.insert(part, Item::Table(child));
        }
        table = table
            .get_mut(part)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("{part} in the user config is not a table"))?;
    }
    Ok(Some(table))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Layer, OFFICIAL_MARKETPLACE_SOURCE, Source, same_marketplace_source};

    fn home() -> (tempfile::TempDir, Paths) {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        (home, paths)
    }

    fn read(paths: &Paths) -> String {
        std::fs::read_to_string(paths.user_config_file()).unwrap_or_default()
    }

    fn settings_of(layers: &[(&str, Source)]) -> Result<crate::Settings, String> {
        let mut all = vec![Layer::defaults()];
        for (text, source) in layers {
            all.push(Layer::new(source.clone(), text.parse().unwrap()));
        }
        Config::from_layers(all)
            .settings()
            .map_err(|error| error.to_string())
    }

    #[test]
    fn the_official_marketplace_is_a_default() {
        let settings = settings_of(&[]).unwrap();
        let official = &settings.plugins.marketplaces[OFFICIAL_MARKETPLACE];
        assert_eq!(official.source, OFFICIAL_MARKETPLACE_SOURCE);
        assert!(official.enabled);
        // Pinned to Anthropic's documented source (the Claude Code plugin discovery page).
        assert_eq!(OFFICIAL_MARKETPLACE, "claude-plugins-official");
        assert_eq!(
            OFFICIAL_MARKETPLACE_SOURCE,
            "anthropics/claude-plugins-official"
        );
    }

    #[test]
    fn the_official_name_keeps_its_source_in_every_layer_on_or_off() {
        let user = Source::User("u".into());
        let env = Source::Environment(
            "IMPERIUM_PLUGINS__MARKETPLACES__CLAUDE-PLUGINS-OFFICIAL__SOURCE".into(),
        );
        for source in [user, env] {
            for enabled in ["true", "false"] {
                let text = format!(
                    "[plugins.marketplaces.claude-plugins-official]\nsource = \"evil/plugins\"\nenabled = {enabled}\n"
                );
                let error = settings_of(&[(&text, source.clone())]).unwrap_err();
                assert!(error.contains("reserved"), "{error}");
            }
        }
        // The same source spelled another way is still the pinned source; off is allowed.
        let spelled = "[plugins.marketplaces.claude-plugins-official]\nsource = \"https://github.com/Anthropics/claude-plugins-official.git\"\nenabled = false\n";
        let settings = settings_of(&[(spelled, Source::User("u".into()))]).unwrap();
        assert!(!settings.plugins.marketplaces[OFFICIAL_MARKETPLACE].enabled);
        // Another name may carry the pinned source.
        let other =
            "[plugins.marketplaces.mine]\nsource = \"anthropics/claude-plugins-official\"\n";
        assert!(settings_of(&[(other, Source::User("u".into()))]).is_ok());
    }

    #[test]
    fn marketplace_names_are_checked_in_every_layer() {
        for bad in [
            "../../..",
            "a/b",
            "x.",
            "Claude-Plugins-Official",
            "claude-plugins-official ",
            "",
        ] {
            let text = format!("[plugins.marketplaces.\"{bad}\"]\nsource = \"a/b\"\n");
            let error = settings_of(&[(&text, Source::User("u".into()))]).unwrap_err();
            assert!(error.contains("plugins.marketplaces"), "{bad:?}: {error}");
        }
        // Two names that differ only by case would share a folder on some file systems.
        let both = "[plugins.marketplaces.acme]\nsource = \"a/b\"\n[plugins.marketplaces.ACME]\nsource = \"c/d\"\n";
        let error = settings_of(&[(both, Source::User("u".into()))]).unwrap_err();
        assert!(error.contains("letter case"), "{error}");
        assert!(!valid_marketplace_name("x."));
        assert!(valid_marketplace_name("my.market_1-b"));
    }

    #[test]
    fn sources_compare_by_repository() {
        for (a, b, same) in [
            ("anthropics/x", "https://github.com/anthropics/x.git", true),
            ("anthropics/x", "https://github.com/Anthropics/X/", true),
            ("anthropics/x", "anthropics/y", false),
            (
                "https://example.test/a.git",
                "https://example.test/a.git",
                true,
            ),
            (
                "https://example.test/a.git",
                "https://example.test/b.git",
                false,
            ),
            ("/home/me/market", "/home/me/market", true),
        ] {
            assert_eq!(same_marketplace_source(a, b), same, "{a} {b}");
        }
    }

    #[test]
    fn marketplaces_are_added_disabled_and_removed_with_comments_kept() {
        let (_home, paths) = home();
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(paths.user_config_file(), "# mine\n[daemon]\nport = 7000\n").unwrap();
        let added = save_marketplace(&paths, "acme", "acme/plugins", Some("v1"), []).unwrap();
        assert_eq!(added.source, "acme/plugins");
        assert_eq!(added.revision.as_deref(), Some("v1"));
        assert!(added.enabled);
        let text = read(&paths);
        assert!(
            text.starts_with("# mine\n[daemon]\nport = 7000\n"),
            "{text}"
        );
        assert!(text.contains("[plugins.marketplaces.acme]"), "{text}");

        assert!(
            !set_marketplace_enabled(&paths, "acme", false, [])
                .unwrap()
                .enabled
        );
        assert!(read(&paths).contains("enabled = false"));
        assert!(set_marketplace_enabled(&paths, "missing", false, []).is_err());

        assert!(remove_marketplace(&paths, "acme", []).unwrap());
        assert!(!remove_marketplace(&paths, "acme", []).unwrap());
        assert_eq!(read(&paths), "# mine\n[daemon]\nport = 7000\n");
    }

    #[test]
    fn turning_the_official_marketplace_off_and_on_leaves_no_trace_when_on() {
        let (_home, paths) = home();
        let off = set_marketplace_enabled(&paths, OFFICIAL_MARKETPLACE, false, []).unwrap();
        assert!(!off.enabled);
        assert_eq!(off.source, OFFICIAL_MARKETPLACE_SOURCE);
        assert!(read(&paths).contains("enabled = false"));
        let on = set_marketplace_enabled(&paths, OFFICIAL_MARKETPLACE, true, []).unwrap();
        assert!(on.enabled);
        assert!(
            !read(&paths).contains("claude-plugins-official"),
            "{}",
            read(&paths)
        );
    }

    #[test]
    fn the_official_name_cannot_be_saved_with_another_source() {
        let (_home, paths) = home();
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(paths.user_config_file(), "[daemon]\nport = 7000\n").unwrap();
        assert!(save_marketplace(&paths, OFFICIAL_MARKETPLACE, "evil/x", None, []).is_err());
        assert_eq!(read(&paths), "[daemon]\nport = 7000\n");
        assert!(save_marketplace(&paths, "bad name", "a/b", None, []).is_err());
        assert!(save_marketplace(&paths, "ok", "  ", None, []).is_err());
    }
}
