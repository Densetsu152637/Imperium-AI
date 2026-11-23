//! Repository suggestions (U2): the marketplaces and plugins a **trusted** project's own
//! `.imperium/config.toml` offers (`[plugins] suggest_marketplaces`, `suggest_plugins`).
//!
//! An offer is data for the user. Making one adds, fetches, installs and enables nothing, and
//! its text (the repository's) goes only to the protocol result, never into a prompt, a notice
//! the model sees or a tool result. A dismissed offer and a muted project (by the canonical path
//! of its folder) are kept in the environment record, so they survive a change to the file.

use std::path::Path;

use imperium_protocol::methods::{
    PluginsDismissSuggestionParams, PluginsDismissSuggestionResult, PluginsSuggestionsParams,
    PluginsSuggestionsResult, SuggestedMarketplace, SuggestedPlugin,
};

use crate::state::DaemonState;

/// How many dismissed offers and muted folders one environment keeps.
const MAX_KEPT: usize = 500;
/// The longest key a dismissal may carry.
const MAX_KEY_CHARS: usize = 1200;

/// The key `plugins.dismiss_suggestion` takes for a marketplace offer.
fn marketplace_key(source: &str, revision: Option<&str>) -> String {
    match revision {
        Some(revision) => format!("marketplace:{source}@{revision}"),
        None => format!("marketplace:{source}"),
    }
}

fn plugin_key(id: &str) -> String {
    format!("plugin:{}", id.to_ascii_lowercase())
}

/// A source as compared with the added marketplaces' sources: no case, no trailing slash or
/// `.git`, and `https://github.com/` read as the `owner/repo` form.
fn comparable(source: &str) -> String {
    let source = source.trim().to_ascii_lowercase();
    let source = source.trim_end_matches('/');
    let source = source.strip_suffix(".git").unwrap_or(source);
    source
        .strip_prefix("https://github.com/")
        .unwrap_or(source)
        .to_string()
}

/// The folder as the muted list holds it: canonical where it can be.
fn canonical_folder(folder: &str) -> String {
    Path::new(folder)
        .canonicalize()
        .map_or_else(|_| folder.to_string(), |path| path.display().to_string())
}

/// What an environment's project folder offers, minus what the user dismissed or muted.
pub(crate) fn suggestions(
    state: &DaemonState,
    params: PluginsSuggestionsParams,
) -> Result<PluginsSuggestionsResult, String> {
    let env = state
        .environments()
        .find(&params.environment)
        .map_err(|error| error.to_string())?;
    let folder = env
        .folders
        .first()
        .filter(|_| !env.runtime.is_remote())
        .cloned();
    let Some(folder) = folder else {
        return Ok(PluginsSuggestionsResult {
            source_folder: None,
            trusted: false,
            muted: false,
            marketplaces: Vec::new(),
            plugins: Vec::new(),
        });
    };
    let source_folder = canonical_folder(&folder);
    let trusted = state.settings().mcp.trusts(Path::new(&folder));
    let muted = env
        .plugins
        .muted_suggestion_sources
        .iter()
        .any(|entry| canonical_folder(entry) == source_folder);
    let mut result = PluginsSuggestionsResult {
        source_folder: Some(source_folder),
        trusted,
        muted,
        marketplaces: Vec::new(),
        plugins: Vec::new(),
    };
    if muted || !trusted {
        return Ok(result);
    }
    // Read on every call: the offers are the file as it is now.
    let offers = state.environment_settings(Some(&env.id)).suggestions;
    let added: Vec<(String, String)> = crate::marketplaces::list(state)?
        .marketplaces
        .into_iter()
        .map(|info| (info.name.to_ascii_lowercase(), comparable(&info.source)))
        .collect();
    let installed: Vec<String> = crate::plugin_install::list(state, None)?
        .plugins
        .into_iter()
        .filter_map(|plugin| plugin.id)
        .map(|id| id.to_ascii_lowercase())
        .collect();
    let dismissed = &env.plugins.dismissed_suggestions;
    for offer in offers.marketplaces {
        let key = marketplace_key(&offer.source, offer.revision.as_deref());
        if dismissed.contains(&key) {
            continue;
        }
        let wanted = comparable(&offer.source);
        result.marketplaces.push(SuggestedMarketplace {
            added: added.iter().any(|(_, source)| *source == wanted),
            key,
            source: offer.source,
            revision: offer.revision,
            file: offer.file.display().to_string(),
        });
    }
    for offer in offers.plugins {
        let key = plugin_key(&offer.id);
        if dismissed.contains(&key) {
            continue;
        }
        let marketplace = offer
            .id
            .split_once('@')
            .map(|(_, marketplace)| marketplace.to_ascii_lowercase())
            .unwrap_or_default();
        result.plugins.push(SuggestedPlugin {
            marketplace_added: added.iter().any(|(name, _)| *name == marketplace),
            installed: installed.contains(&offer.id.to_ascii_lowercase()),
            key,
            id: offer.id,
            file: offer.file.display().to_string(),
        });
    }
    Ok(result)
}

/// `plugins.dismiss_suggestion`: one offer, or the whole project. Changes only the environment
/// record's two lists.
pub(crate) fn dismiss(
    state: &DaemonState,
    params: PluginsDismissSuggestionParams,
) -> Result<PluginsDismissSuggestionResult, String> {
    let env = state
        .environments()
        .find(&params.environment)
        .map_err(|error| error.to_string())?;
    match (&params.key, &params.source_folder) {
        (Some(key), None) => {
            if key.chars().count() > MAX_KEY_CHARS {
                return Err("that suggestion key is too long".into());
            }
            if !key.starts_with("marketplace:") && !key.starts_with("plugin:") {
                return Err(
                    "a suggestion key is the `key` plugins.suggestions gave for an offer".into(),
                );
            }
            if env.plugins.dismissed_suggestions.len() >= MAX_KEPT
                && !env.plugins.dismissed_suggestions.contains(key)
            {
                return Err(format!(
                    "{MAX_KEPT} suggestions are already dismissed here; mute the project instead"
                ));
            }
            state
                .environments()
                .dismiss_suggestion(&env.id, key)
                .map_err(|error| error.to_string())?;
            Ok(PluginsDismissSuggestionResult {
                dismissed: true,
                muted: false,
            })
        }
        (None, Some(source_folder)) => {
            // Only the environment's own project folder can be muted, so a caller cannot grow
            // the list with arbitrary paths.
            let own = env
                .folders
                .first()
                .map(|folder| canonical_folder(folder))
                .ok_or("this environment has no project folder")?;
            if canonical_folder(source_folder) != own {
                return Err(
                    "source_folder is not this environment's project folder (use the one \
                     plugins.suggestions gave)"
                        .into(),
                );
            }
            if env.plugins.muted_suggestion_sources.len() >= MAX_KEPT {
                return Err(format!("{MAX_KEPT} projects are already muted here"));
            }
            state
                .environments()
                .mute_suggestion_source(&env.id, &own)
                .map_err(|error| error.to_string())?;
            Ok(PluginsDismissSuggestionResult {
                dismissed: false,
                muted: true,
            })
        }
        _ => Err("give exactly one of `key` (one offer) or `source_folder` (the project)".into()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use imperium_config::{Config, Layer, MemorySecrets, Paths, Source};
    use imperium_protocol::methods::{EnvironmentCreateParams, PluginsMarketplaceAddParams};

    use super::*;

    const OFFER: &str = "[plugins]\nsuggest_marketplaces = [{ source = \"acme/plugins\" }]\nsuggest_plugins = [\"lint@acme\", \"other@acme\"]\n";

    struct Fixture {
        home: tempfile::TempDir,
        project: tempfile::TempDir,
        state: Arc<DaemonState>,
        environment: String,
    }

    fn fixture(trusted: bool) -> Fixture {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let config = project.path().join(".imperium/config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, OFFER).unwrap();
        let user = if trusted {
            format!(
                "[mcp]\ntrusted_projects = [\"{}\"]",
                project.path().display().to_string().replace('\\', "/")
            )
        } else {
            String::new()
        };
        let settings = Config::from_layers([
            Layer::defaults(),
            Layer::new(Source::User("test".into()), user.parse().unwrap()),
        ])
        .settings()
        .unwrap();
        let state = DaemonState::with_secrets(
            Paths::under(home.path()),
            settings,
            None,
            Arc::new(MemorySecrets::new()),
        );
        state.migrate_plugins();
        let environment = state
            .environments()
            .create(EnvironmentCreateParams {
                name: "Work".into(),
                folders: vec![project.path().display().to_string()],
                ..EnvironmentCreateParams::default()
            })
            .unwrap()
            .id;
        Fixture {
            home,
            project,
            state,
            environment,
        }
    }

    fn ask(fixture: &Fixture) -> PluginsSuggestionsResult {
        suggestions(
            &fixture.state,
            PluginsSuggestionsParams {
                environment: fixture.environment.clone(),
            },
        )
        .unwrap()
    }

    /// Every file under `dir` with its bytes: what an offer must leave alone.
    fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(tree(&path));
            } else {
                found.push((
                    path.display().to_string(),
                    std::fs::read(&path).unwrap_or_default(),
                ));
            }
        }
        found.sort();
        found
    }

    #[test]
    fn a_trusted_folders_offers_are_listed_and_change_nothing() {
        let fixture = fixture(true);
        let before = tree(fixture.home.path());
        let settings_before = format!("{:?}", fixture.state.settings());
        let result = ask(&fixture);
        assert!(result.trusted && !result.muted);
        assert_eq!(result.marketplaces.len(), 1);
        assert_eq!(result.marketplaces[0].source, "acme/plugins");
        assert!(!result.marketplaces[0].added);
        let ids: Vec<_> = result.plugins.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["lint@acme", "other@acme"]);
        assert!(!result.plugins[0].installed && !result.plugins[0].marketplace_added);
        assert!(result.plugins[0].file.ends_with("config.toml"));

        // No file under the config or data folder changed or appeared (no clone, staging
        // folder, record, journal entry or marker), and the settings are the same.
        assert_eq!(tree(fixture.home.path()), before);
        assert_eq!(format!("{:?}", fixture.state.settings()), settings_before);
        assert!(
            crate::plugin_install::list(&fixture.state, None)
                .unwrap()
                .plugins
                .is_empty()
        );
        assert!(
            fixture
                .state
                .settings()
                .extensions
                .disabled_plugins
                .is_empty()
        );
        let env = fixture
            .state
            .environments()
            .get(&fixture.environment)
            .unwrap();
        assert_eq!(env.plugins, Default::default());
        let added = crate::marketplaces::list(&fixture.state).unwrap();
        assert!(added.marketplaces.iter().all(|m| m.name != "acme"));
    }

    #[test]
    fn an_untrusted_folders_offers_are_refused_and_shown_in_the_environment_config() {
        let fixture = fixture(false);
        let result = ask(&fixture);
        assert!(!result.trusted);
        assert!(result.marketplaces.is_empty() && result.plugins.is_empty());
        let config = fixture.state.environment_config(&fixture.environment);
        let refused = config.refused.join("; ");
        assert!(refused.contains("mcp.trusted_projects"), "{refused}");
        // The refusal holds none of the repository's text.
        assert!(
            !refused.contains("acme") && !refused.contains("lint"),
            "{refused}"
        );
    }

    #[test]
    fn an_added_marketplace_is_marked_on_the_plugin_offers() {
        let fixture = fixture(true);
        let market = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(market.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            market.path().join(".claude-plugin/marketplace.json"),
            r#"{"name": "acme", "owner": {"name": "A"}, "plugins": []}"#,
        )
        .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(crate::marketplaces::add(
                &fixture.state,
                PluginsMarketplaceAddParams {
                    source: market.path().display().to_string(),
                    revision: None,
                },
            ))
            .unwrap();
        let result = ask(&fixture);
        // A marketplace of that name is added, though the offered source is another one.
        assert!(result.plugins.iter().all(|p| p.marketplace_added));
        assert!(!result.marketplaces[0].added);
    }

    #[test]
    fn a_dismissed_offer_stays_dismissed_and_a_muted_project_offers_nothing_after_its_file_changes()
    {
        let fixture = fixture(true);
        let first = ask(&fixture);
        let key = first.plugins[0].key.clone();
        let done = dismiss(
            &fixture.state,
            PluginsDismissSuggestionParams {
                environment: fixture.environment.clone(),
                key: Some(key),
                source_folder: None,
            },
        )
        .unwrap();
        assert!(done.dismissed && !done.muted);
        let after = ask(&fixture);
        assert_eq!(after.plugins.len(), 1);
        assert_eq!(after.plugins[0].id, "other@acme");
        assert_eq!(after.marketplaces.len(), 1);

        // Mute the project, by the folder `plugins.suggestions` named.
        let folder = after.source_folder.clone().unwrap();
        let muted = dismiss(
            &fixture.state,
            PluginsDismissSuggestionParams {
                environment: fixture.environment.clone(),
                key: None,
                source_folder: Some(folder),
            },
        )
        .unwrap();
        assert!(muted.muted);
        let nothing = ask(&fixture);
        assert!(nothing.muted);
        assert!(nothing.marketplaces.is_empty() && nothing.plugins.is_empty());
        // A changed file, with a brand new offer, still offers nothing.
        std::fs::write(
            fixture.project.path().join(".imperium/config.toml"),
            "[plugins]\nsuggest_plugins = [\"brand-new@acme\"]\n",
        )
        .unwrap();
        let still = ask(&fixture);
        assert!(still.muted && still.plugins.is_empty() && still.marketplaces.is_empty());
    }

    #[test]
    fn dismissing_takes_exactly_one_thing_and_only_the_environments_own_folder() {
        let fixture = fixture(true);
        let call = |key: Option<&str>, folder: Option<&str>| {
            dismiss(
                &fixture.state,
                PluginsDismissSuggestionParams {
                    environment: fixture.environment.clone(),
                    key: key.map(str::to_string),
                    source_folder: folder.map(str::to_string),
                },
            )
        };
        assert!(call(None, None).is_err());
        assert!(call(Some("plugin:x@y"), Some("/somewhere")).is_err());
        assert!(call(Some("not a key"), None).is_err());
        let other = tempfile::tempdir().unwrap();
        assert!(call(None, Some(&other.path().display().to_string())).is_err());
        let env = fixture
            .state
            .environments()
            .get(&fixture.environment)
            .unwrap();
        assert!(env.plugins.dismissed_suggestions.is_empty());
        assert!(env.plugins.muted_suggestion_sources.is_empty());
    }
}
