use imperium_config::{SecretError, SecretName, Settings};
use imperium_models::{Endpoint, Pricing};

/// A model chosen for a session: where to call it, its id, and its prices.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    /// The `<endpoint>:<model>` spec it was resolved from.
    pub spec: String,
    pub endpoint: Endpoint,
    pub model: String,
    pub pricing: Option<Pricing>,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("no model given and models.default is not set")]
    NoDefault,
    #[error("model {0:?} must be written as <endpoint>:<model>")]
    BadSpec(String),
    #[error("endpoint {0:?} is not configured under [endpoints]")]
    UnknownEndpoint(String),
    #[error("secret {0:?} for endpoint {1:?} is not in the keychain; run: imperium secret set {0}")]
    MissingSecret(String, String),
    #[error(transparent)]
    Secret(#[from] SecretError),
}

/// Resolves a `<endpoint>:<model>` spec (or `models.default`) against the configured
/// endpoints, loading the endpoint's API key with `load_secret`.
pub fn resolve_model(
    settings: &Settings,
    requested: Option<&str>,
    load_secret: impl Fn(&SecretName) -> Result<Option<String>, SecretError>,
) -> Result<ResolvedModel, ModelError> {
    let spec = requested
        .or(settings.models.default.as_deref())
        .ok_or(ModelError::NoDefault)?;
    let (endpoint_id, model) = spec
        .split_once(':')
        .filter(|(endpoint, model)| !endpoint.is_empty() && !model.is_empty())
        .ok_or_else(|| ModelError::BadSpec(spec.to_string()))?;
    let endpoint = settings
        .endpoints
        .get(endpoint_id)
        .ok_or_else(|| ModelError::UnknownEndpoint(endpoint_id.to_string()))?;
    let api_key = match &endpoint.api_key {
        Some(reference) => Some(load_secret(&reference.secret)?.ok_or_else(|| {
            ModelError::MissingSecret(reference.secret.to_string(), endpoint_id.to_string())
        })?),
        None => None,
    };
    Ok(ResolvedModel {
        spec: spec.to_string(),
        endpoint: Endpoint {
            base_url: endpoint.base_url.clone(),
            api_key,
        },
        model: model.to_string(),
        pricing: endpoint.pricing.get(model).copied(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_config::{Config, Layer, Source};

    fn settings(extra: &str) -> Settings {
        Config::from_layers([
            Layer::defaults(),
            Layer::new(Source::User("u".into()), extra.parse().unwrap()),
        ])
        .settings()
        .unwrap()
    }

    const ENDPOINTS: &str = r#"
        [models]
        default = "local:qwen3:8b"
        [endpoints.local]
        base_url = "http://127.0.0.1:8080/v1"
        [endpoints.hosted]
        base_url = "https://example.com/v1"
        api_key = { secret = "hosted" }
        pricing."big" = { input_usd_per_mtok = 1.0, output_usd_per_mtok = 2.0 }
    "#;

    #[test]
    fn the_default_model_splits_on_the_first_colon() {
        let resolved = resolve_model(&settings(ENDPOINTS), None, |_| Ok(None)).unwrap();
        assert_eq!(resolved.model, "qwen3:8b");
        assert_eq!(resolved.endpoint.api_key, None);
        assert_eq!(resolved.pricing, None);
    }

    #[test]
    fn hosted_models_load_their_key_and_prices() {
        let resolved = resolve_model(&settings(ENDPOINTS), Some("hosted:big"), |_| {
            Ok(Some("k".into()))
        })
        .unwrap();
        assert_eq!(resolved.endpoint.api_key.as_deref(), Some("k"));
        assert_eq!(resolved.pricing.unwrap().output_usd_per_mtok, 2.0);

        let missing = resolve_model(&settings(ENDPOINTS), Some("hosted:big"), |_| Ok(None));
        assert!(matches!(missing, Err(ModelError::MissingSecret(..))));
    }

    #[test]
    fn bad_specs_are_explained() {
        let settings = settings(ENDPOINTS);
        assert!(matches!(
            resolve_model(&settings, Some("nocolon"), |_| Ok(None)),
            Err(ModelError::BadSpec(_))
        ));
        assert!(matches!(
            resolve_model(&settings, Some("other:m"), |_| Ok(None)),
            Err(ModelError::UnknownEndpoint(_))
        ));
        assert!(matches!(
            resolve_model(&self::settings(""), None, |_| Ok(None)),
            Err(ModelError::NoDefault)
        ));
    }
}
