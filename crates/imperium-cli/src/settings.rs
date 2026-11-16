//! `imperium config set` and `imperium config unset`: change one setting from the settings
//! registry (the same table the desktop Settings window uses). With a daemon running the change
//! goes through it, so settings it applies at once (such as `policy.personality`) take effect
//! for new sessions; otherwise the user's `config.toml` is written directly.

use anyhow::{Context, anyhow, bail};
use imperium_config::{Paths, SettingDef, SettingKind};
use imperium_protocol::methods::{ConfigSet, ConfigSetParams};
use serde_json::Value as Json;

use crate::{REMOTE, block_on, connect};

/// Sets `key` to `values` (several only for a list setting).
pub fn set(key: &str, values: Vec<String>) -> anyhow::Result<()> {
    let def = imperium_config::setting(key).ok_or_else(|| unknown(key))?;
    apply(key, value(def, values)?)
}

/// Removes `key` from the user's config file, so its default applies.
pub fn unset(key: &str) -> anyhow::Result<()> {
    imperium_config::setting(key).ok_or_else(|| unknown(key))?;
    apply(key, Json::Null)
}

fn unknown(key: &str) -> anyhow::Error {
    anyhow!("{key} is not a setting that can be set here; see docs/10-configuration.md")
}

fn apply(key: &str, value: Json) -> anyhow::Result<()> {
    let params = ConfigSetParams {
        key: key.into(),
        value: value.clone(),
    };
    let through_daemon = block_on(async {
        let Ok((mut client, _)) = connect().await else {
            return Ok(None);
        };
        Ok(Some(client.call::<ConfigSet>(params).await?))
    })?;
    let (shown, restart) = match through_daemon {
        Some(result) => (result.entry.value, result.restart),
        None if REMOTE.get().and_then(Option::as_deref).is_some() => {
            bail!("the remote daemon did not answer, so nothing was changed")
        }
        None => {
            let paths = Paths::from_env()?;
            let state = imperium_config::set_user_setting(&paths, key, &value, std::env::vars())
                .with_context(|| format!("could not set {key}"))?;
            // The offline path cannot refuse (only the daemon knows what is switched off), so it
            // says so after saving.
            if key == "models.default"
                && let Some(settings) = imperium_config::load(&paths, None, std::env::vars())
                    .ok()
                    .and_then(|config| config.settings().ok())
                && let Some(warning) = default_disabled_warning(&settings.models)
            {
                eprintln!("{warning}");
            }
            (state.value, state.def.restart)
        }
    };
    println!("{key} = {shown}");
    if restart {
        println!("Restart imperium-core for the change to apply.");
    }
    Ok(())
}

/// What the offline `settings set models.default` says after saving when the model it was set to
/// is switched off: the daemon would have refused, but offline only the saved settings know.
fn default_disabled_warning(models: &imperium_config::ModelSettings) -> Option<String> {
    models.default_is_disabled().then(|| {
        concat!(
            "warning: that model is switched off (\"Use this model\" in Settings -> Models), ",
            "so new sessions cannot start on it until it is switched on or another default is ",
            "chosen"
        )
        .to_string()
    })
}

/// The JSON value `config.set` takes for `values` given the setting's kind.
fn value(def: &SettingDef, values: Vec<String>) -> anyhow::Result<Json> {
    if matches!(def.kind, SettingKind::PathList | SettingKind::StringList) {
        return Ok(Json::from(values));
    }
    let [text] =
        <[String; 1]>::try_from(values).map_err(|_| anyhow!("{} takes one value", def.key))?;
    Ok(match def.kind {
        SettingKind::Bool => Json::from(
            text.parse::<bool>()
                .map_err(|_| anyhow!("{} takes true or false", def.key))?,
        ),
        SettingKind::Integer => Json::from(
            text.parse::<i64>()
                .map_err(|_| anyhow!("{} takes a whole number", def.key))?,
        ),
        SettingKind::Number => Json::from(
            text.parse::<f64>()
                .map_err(|_| anyhow!("{} takes a number", def.key))?,
        ),
        // `runtime.llamacpp.context` takes `auto` or a number; the registry reads both.
        _ if def.key == "runtime.llamacpp.context" => match text.parse::<u64>() {
            Ok(tokens) => Json::from(tokens),
            Err(_) => Json::from(text),
        },
        _ => Json::from(text),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(key: &str, values: &[&str]) -> anyhow::Result<Json> {
        value(
            imperium_config::setting(key).unwrap(),
            values.iter().map(|value| value.to_string()).collect(),
        )
    }

    #[test]
    fn values_follow_the_setting_kind() {
        assert_eq!(
            of("policy.personality", &["luxion"]).unwrap(),
            Json::from("luxion")
        );
        assert_eq!(of("pets.enabled", &["false"]).unwrap(), Json::from(false));
        assert_eq!(of("daemon.port", &["7100"]).unwrap(), Json::from(7100));
        assert_eq!(
            of("models.folders", &["/a", "/b"]).unwrap(),
            serde_json::json!(["/a", "/b"])
        );
        assert_eq!(
            of("runtime.llamacpp.context", &["16384"]).unwrap(),
            Json::from(16384)
        );
        assert!(of("pets.enabled", &["maybe"]).is_err());
        assert!(of("policy.personality", &["a", "b"]).is_err());
    }

    #[test]
    fn a_default_that_is_switched_off_is_warned_about_after_saving() {
        let mut models = imperium_config::ModelSettings::default();
        assert!(default_disabled_warning(&models).is_none());
        models.default = Some("llamacpp:gemma".into());
        assert!(default_disabled_warning(&models).is_none());
        models.disabled = vec!["llamacpp:gemma".into()];
        let warning = default_disabled_warning(&models).unwrap();
        assert!(warning.starts_with("warning: that model is switched off"));
        assert!(warning.contains("another default"));
        // Naming a different default that is on gives no warning.
        models.default = Some("llamacpp:other".into());
        assert!(default_disabled_warning(&models).is_none());
    }
}
