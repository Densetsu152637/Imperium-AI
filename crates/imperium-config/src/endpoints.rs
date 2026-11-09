//! Adding `[endpoints.<id>]` entries to the user's `config.toml`, for apps that connect a hosted
//! API without the user editing the file. Like [`crate::set_user_setting`], the edit keeps the
//! file's comments and unknown keys, and is undone when it leaves the configuration invalid. The
//! API key is written only as a `{ secret = "<name>" }` reference.

use toml_edit::{InlineTable, Item, Table, value};

use crate::registry::edit_user_file;
use crate::{EndpointSettings, LLAMACPP_ENDPOINT, Paths, SecretName, SetError, VLLM_ENDPOINT};

/// Whether `id` may name an endpoint: 1-40 lowercase letters, digits, `_` or `-`, starting with
/// a letter or digit, and not one of the names reserved for local models.
pub fn valid_endpoint_id(id: &str) -> bool {
    let allowed = |c: char| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-');
    id.chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id.len() <= 40
        && id.chars().all(allowed)
        && id != LLAMACPP_ENDPOINT
        && id != VLLM_ENDPOINT
}

/// Writes `[endpoints.<id>]` with `base_url` and, when given, `api_key = { secret = ... }`,
/// replacing those keys of an existing entry (its prices and other keys stay). Returns the
/// entry as loaded back.
pub fn save_endpoint(
    paths: &Paths,
    id: &str,
    base_url: &str,
    api_key: Option<&SecretName>,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<EndpointSettings, SetError> {
    let key = format!("endpoints.{id}");
    if !valid_endpoint_id(id) {
        return Err(SetError::InvalidValue {
            key,
            message: "an endpoint name is 1-40 lowercase letters, digits, '_' or '-', and not \
                      llamacpp or vllm"
                .into(),
        });
    }
    let base_url = base_url.trim().trim_end_matches('/');
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return Err(SetError::InvalidValue {
            key,
            message: "the base URL starts with http:// or https://".into(),
        });
    }
    let config = edit_user_file(paths, env, |document| {
        let endpoints = document
            .entry("endpoints")
            .or_insert_with(|| {
                let mut table = Table::new();
                table.set_implicit(true);
                Item::Table(table)
            })
            .as_table_mut()
            .ok_or("endpoints is not a table")?;
        let entry = endpoints
            .entry(id)
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .ok_or_else(|| format!("endpoints.{id} is not a table"))?;
        entry["base_url"] = value(base_url);
        match api_key {
            Some(name) => {
                let mut reference = InlineTable::new();
                reference.insert("secret", name.as_str().into());
                entry["api_key"] = value(reference);
            }
            None => {
                entry.remove("api_key");
            }
        }
        Ok(())
    })?;
    config
        .settings()
        .map_err(SetError::Rejected)?
        .endpoints
        .get(id)
        .cloned()
        .ok_or_else(|| SetError::InvalidValue {
            key: format!("endpoints.{id}"),
            message: "the entry did not load back".into(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_added_and_updated_in_place() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(
            paths.user_config_file(),
            "# mine\n[endpoints.router]\nbase_url = \"https://old\"\n[endpoints.router.pricing]\nm = { input_usd_per_mtok = 1.0, output_usd_per_mtok = 2.0 }\n",
        )
        .unwrap();
        let secret: SecretName = "router-api-key".parse().unwrap();
        let saved = save_endpoint(
            &paths,
            "router",
            "https://openrouter.ai/api/v1/",
            Some(&secret),
            [],
        )
        .unwrap();
        assert_eq!(saved.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(saved.api_key.unwrap().secret, secret);
        assert_eq!(saved.pricing.len(), 1);
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(text.starts_with("# mine"));

        let fresh = save_endpoint(&paths, "local-box", "http://gpu:8080/v1", None, []).unwrap();
        assert_eq!(fresh.api_key, None);
        for (id, url) in [
            ("llamacpp", "http://x"),
            ("Bad Name", "http://x"),
            ("ok", "ftp://x"),
        ] {
            assert!(save_endpoint(&paths, id, url, None, []).is_err(), "{id}");
        }
    }
}
