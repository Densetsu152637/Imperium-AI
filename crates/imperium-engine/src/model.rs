use imperium_api::{Availability, LocalModels};
use imperium_config::{
    LLAMACPP_ENDPOINT, Location, Preference, RoutingSettings, SecretError, SecretName, Settings,
    Tier, VLLM_ENDPOINT,
};
use imperium_models::{Endpoint, Pricing};

/// A model chosen for a session: where to call it, its id, and its prices.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    /// The `<endpoint>:<model>` spec it was resolved from.
    pub spec: String,
    pub endpoint: Endpoint,
    pub model: String,
    pub pricing: Option<Pricing>,
    /// Served by the supervised llama.cpp runtime: `model` is the local model id, and the
    /// endpoint is filled in when the agent takes a slot on it.
    pub local: bool,
    /// Tokens the model reads per request, from the model table's `context_window`. A local
    /// model's server reports its own, which takes precedence.
    pub context_window: Option<u64>,
    /// The endpoint's `kind`, naming the client that calls it (see
    /// [`ChatClients`](imperium_models::ChatClients)).
    pub kind: String,
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
    let context_window = settings
        .models
        .catalog
        .iter()
        .find(|entry| entry.spec == spec)
        .and_then(|entry| entry.context_window)
        .map(u64::from);
    if endpoint_id == LLAMACPP_ENDPOINT || endpoint_id == VLLM_ENDPOINT {
        return Ok(ResolvedModel {
            spec: spec.to_string(),
            endpoint: Endpoint {
                base_url: String::new(),
                api_key: None,
            },
            model: model.to_string(),
            pricing: None,
            local: true,
            context_window,
            // llama.cpp offers its tokenizer; vLLM is reached as any OpenAI-compatible server.
            kind: if endpoint_id == LLAMACPP_ENDPOINT {
                "llamacpp"
            } else {
                imperium_models::DEFAULT_KIND
            }
            .into(),
        });
    }
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
        local: false,
        context_window,
        kind: endpoint.kind.clone(),
    })
}

/// A row of the model table, resolved and ready to call.
#[derive(Debug, Clone)]
pub struct CatalogModel {
    pub id: String,
    pub model: ResolvedModel,
    pub tier: Tier,
    pub location: Location,
    pub tags: Vec<String>,
}

/// What a delegating agent asked for.
#[derive(Debug, Clone, Copy, Default)]
pub struct TaskRequest<'a> {
    /// A model table id or `<endpoint>:<model>` spec.
    pub named: Option<&'a str>,
    pub tier: Tier,
    pub location: Option<Location>,
    pub tags: &'a [String],
}

/// Picks a subagent's model. Local models are considered only when `local` reports they can run
/// (loaded with a free slot, fitting in free memory, or fitting once idle models are swapped
/// out); a loaded model is preferred to loading one, and loading to swapping, and nothing in use
/// is evicted, so when memory is short the choice falls to hosted models.
///
/// 1. a model the agent named, if it is in the table;
/// 2. the first `[[routing.rules]]` entry matching the task's tier and tags decides: its `use`
///    list is tried in order (an explicit choice, so tier is not checked; location is), then
///    its location restriction applies to the rest;
/// 3. otherwise the suitable models (at or above the tier, in the requested location) are
///    ranked by `routing.prefer`, then by price, with unpriced models counted as free;
/// 4. otherwise `fallback`, the session's model.
pub fn route(
    catalog: &[CatalogModel],
    routing: &RoutingSettings,
    fallback: &ResolvedModel,
    request: TaskRequest<'_>,
    local: Option<&dyn LocalModels>,
) -> ResolvedModel {
    let availability = |c: &CatalogModel| match (c.model.local, local) {
        (false, _) => Availability::Loaded,
        (true, Some(local)) => local.availability(&c.model.model),
        (true, None) => Availability::Unavailable,
    };
    let usable = |c: &&CatalogModel| availability(c) != Availability::Unavailable;
    if let Some(named) = request.named
        && let Some(chosen) = catalog
            .iter()
            .filter(usable)
            .find(|c| c.id == named || c.model.spec == named)
    {
        return chosen.model.clone();
    }
    let rule = routing.rules.iter().find(|rule| {
        rule.when_tier.is_none_or(|tier| tier == request.tier)
            && rule
                .when_tag
                .as_ref()
                .is_none_or(|tag| request.tags.contains(tag))
    });
    let location = request.location.or(rule.and_then(|rule| rule.location));
    let in_location = |c: &CatalogModel| location.is_none_or(|wanted| c.location == wanted);
    if let Some(rule) = rule {
        let preferred = rule
            .use_models
            .iter()
            .filter_map(|id| catalog.iter().find(|c| &c.id == id))
            .filter(usable)
            .find(|c| in_location(c));
        if let Some(chosen) = preferred {
            return chosen.model.clone();
        }
    }
    let rank = |c: &CatalogModel| {
        let preferred = match routing.prefer {
            Preference::Cheapest => true,
            Preference::Local => c.location == Location::Local,
            Preference::Cloud => c.location == Location::Cloud,
        };
        let price = c
            .model
            .pricing
            .map_or(0.0, |p| p.input_usd_per_mtok + p.output_usd_per_mtok);
        (!preferred, price, availability(c))
    };
    catalog
        .iter()
        .filter(usable)
        .filter(|c| c.tier >= request.tier && in_location(c))
        .min_by(|a, b| {
            let (a_rank, b_rank) = (rank(a), rank(b));
            a_rank
                .0
                .cmp(&b_rank.0)
                .then(a_rank.1.total_cmp(&b_rank.1))
                .then(a_rank.2.cmp(&b_rank.2))
        })
        .map_or_else(|| fallback.clone(), |chosen| chosen.model.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_config::RoutingRule;
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
    fn the_context_window_comes_from_the_model_table() {
        let table = format!(
            "{ENDPOINTS}\n[[models.catalog]]\nid = \"big\"\nspec = \"hosted:big\"\n\
             location = \"cloud\"\ntier = \"strong\"\ncontext_window = 32768"
        );
        let settings = settings(&table);
        let big = resolve_model(&settings, Some("hosted:big"), |_| Ok(Some("k".into()))).unwrap();
        assert_eq!(big.context_window, Some(32768));
        // Not in the table: unknown, never guessed.
        let other = resolve_model(&settings, None, |_| Ok(None)).unwrap();
        assert_eq!(other.context_window, None);
    }

    fn catalog() -> Vec<CatalogModel> {
        let row = |id: &str, price: Option<f64>, tier: Tier, location: Location, tags: &[&str]| {
            CatalogModel {
                id: id.into(),
                model: ResolvedModel {
                    spec: format!("endpoint:{id}"),
                    endpoint: Endpoint {
                        base_url: String::new(),
                        api_key: None,
                    },
                    model: id.into(),
                    pricing: price.map(|p| Pricing {
                        input_usd_per_mtok: p,
                        output_usd_per_mtok: p,
                        cached_input_usd_per_mtok: None,
                    }),
                    local: false,
                    context_window: None,
                    kind: "openai".into(),
                },
                tier,
                location,
                tags: tags.iter().map(|tag| tag.to_string()).collect(),
            }
        };
        vec![
            row("cheap", Some(0.1), Tier::Basic, Location::Cloud, &[]),
            row("mid", Some(1.0), Tier::Standard, Location::Cloud, &["code"]),
            row("local", None, Tier::Standard, Location::Local, &["code"]),
            row("big", Some(5.0), Tier::Strong, Location::Cloud, &[]),
        ]
    }

    fn fallback() -> ResolvedModel {
        ResolvedModel {
            spec: "session".into(),
            endpoint: Endpoint {
                base_url: String::new(),
                api_key: None,
            },
            model: "session".into(),
            pricing: None,
            local: false,
            context_window: None,
            kind: "openai".into(),
        }
    }

    fn pick(routing: &RoutingSettings, request: TaskRequest<'_>) -> String {
        route(&catalog(), routing, &fallback(), request, None).model
    }

    #[test]
    fn routing_ranks_by_preference_then_price() {
        let cheapest = RoutingSettings::default();
        let basic = TaskRequest {
            tier: Tier::Basic,
            ..TaskRequest::default()
        };
        assert_eq!(pick(&cheapest, basic), "local");
        let strong = TaskRequest {
            tier: Tier::Strong,
            ..TaskRequest::default()
        };
        assert_eq!(pick(&cheapest, strong), "big");

        let cloud_first = RoutingSettings {
            prefer: Preference::Cloud,
            ..RoutingSettings::default()
        };
        assert_eq!(pick(&cloud_first, basic), "cheap");

        let cloud_only = TaskRequest {
            location: Some(Location::Cloud),
            ..basic
        };
        assert_eq!(pick(&cheapest, cloud_only), "cheap");
        let local_strong = TaskRequest {
            location: Some(Location::Local),
            ..strong
        };
        assert_eq!(pick(&cheapest, local_strong), "session");
    }

    #[test]
    fn named_models_and_rules_take_precedence() {
        let routing = RoutingSettings {
            prefer: Preference::Cheapest,
            rules: vec![
                RoutingRule {
                    when_tag: Some("code".into()),
                    location: Some(Location::Cloud),
                    use_models: vec!["local".into(), "mid".into()],
                    ..RoutingRule::default()
                },
                RoutingRule {
                    when_tier: Some(Tier::Frontier),
                    use_models: vec!["big".into()],
                    ..RoutingRule::default()
                },
            ],
        };
        let code = ["code".to_string()];
        let coding = TaskRequest {
            tier: Tier::Basic,
            tags: &code,
            ..TaskRequest::default()
        };
        // "local" is listed first but the rule restricts to cloud, so "mid" is used.
        assert_eq!(pick(&routing, coding), "mid");
        // An explicit use list is honoured even above the models' tier.
        let frontier = TaskRequest {
            tier: Tier::Frontier,
            ..TaskRequest::default()
        };
        assert_eq!(pick(&routing, frontier), "big");
        let named = TaskRequest {
            named: Some("cheap"),
            ..coding
        };
        assert_eq!(pick(&routing, named), "cheap");
        let by_spec = TaskRequest {
            named: Some("endpoint:local"),
            ..coding
        };
        assert_eq!(pick(&routing, by_spec), "local");
    }

    /// Local models with fixed availability, standing in for the llama.cpp supervisor.
    #[derive(Debug)]
    struct FakeLocal(Vec<(&'static str, Availability)>);

    impl LocalModels for FakeLocal {
        fn availability(&self, model: &str) -> Availability {
            self.0
                .iter()
                .find(|(id, _)| *id == model)
                .map_or(Availability::Unavailable, |(_, availability)| *availability)
        }

        fn acquire<'a>(
            &'a self,
            _model: &'a str,
            _holder: &'a str,
        ) -> imperium_api::AcquireFuture<'a> {
            Box::pin(async { Err("not used".to_string()) })
        }
    }

    fn local_catalog() -> Vec<CatalogModel> {
        let local_row = |id: &str| {
            let mut row = catalog().remove(2);
            row.id = id.into();
            row.model.model = id.into();
            row.model.local = true;
            row
        };
        let mut rows = catalog();
        rows.retain(|c| c.id == "mid");
        rows.extend([local_row("small"), local_row("large")]);
        rows
    }

    #[test]
    fn local_placement_reuses_loaded_models_and_falls_back_to_hosted() {
        let routing = RoutingSettings {
            prefer: Preference::Local,
            ..RoutingSettings::default()
        };
        let request = TaskRequest::default();
        let pick = |local: &FakeLocal| {
            route(
                &local_catalog(),
                &routing,
                &fallback(),
                request,
                Some(local),
            )
            .model
        };
        // Both fit: the loaded one is reused rather than loading the other.
        let one_loaded = FakeLocal(vec![
            ("small", Availability::Fits),
            ("large", Availability::Loaded),
        ]);
        assert_eq!(pick(&one_loaded), "large");
        let none_loaded = FakeLocal(vec![
            ("small", Availability::Fits),
            ("large", Availability::Fits),
        ]);
        assert_eq!(pick(&none_loaded), "small");
        // Memory is short and nothing is loaded: the hosted model is used.
        let short = FakeLocal(vec![
            ("small", Availability::Unavailable),
            ("large", Availability::Unavailable),
        ]);
        assert_eq!(pick(&short), "mid");
        // Without a local runtime, local rows are never chosen, even by name.
        let named = TaskRequest {
            named: Some("small"),
            ..request
        };
        assert_eq!(
            route(&local_catalog(), &routing, &fallback(), named, None).model,
            "mid"
        );
    }

    #[test]
    fn llamacpp_specs_resolve_to_local_models() {
        let resolved = resolve_model(&settings(ENDPOINTS), Some("llamacpp:qwen3-8b-q4"), |_| {
            Ok(None)
        })
        .unwrap();
        assert!(resolved.local);
        assert_eq!(resolved.model, "qwen3-8b-q4");
        let vllm =
            resolve_model(&settings(ENDPOINTS), Some("vllm:Qwen3-8B"), |_| Ok(None)).unwrap();
        assert!(vllm.local);
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
