use imperium_api::{Availability, LocalModels};
use imperium_config::{
    LLAMACPP_ENDPOINT, Location, Preference, RoutingRule, RoutingSettings, SecretError, SecretName,
    Settings, Tier, VLLM_ENDPOINT,
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
    /// Whether the model can reason, so calls carry the session's effort: the model table row's
    /// `reasoning` setting or tags, else (hosted models) whether its name is a known reasoning
    /// model. `None` for a local model nothing configured: its runtime decides from the chat
    /// template (see [`ResolvedModel::reasons`]).
    pub reasoning: Option<bool>,
}

impl ResolvedModel {
    /// Whether the model can reason: [`ResolvedModel::reasoning`], else the local runtime's
    /// answer from the model's chat template, else its name.
    pub fn reasons(&self, local: Option<&dyn LocalModels>) -> bool {
        self.reasoning
            .or_else(|| {
                self.local
                    .then_some(local)
                    .flatten()
                    .and_then(|local| local.reasons(&self.model))
            })
            .unwrap_or_else(|| imperium_api::reasoning_by_name(&self.model))
    }
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
    let row = settings
        .models
        .catalog
        .iter()
        .find(|entry| entry.spec == spec);
    let context_window = row.and_then(|entry| entry.context_window).map(u64::from);
    let reasoning = row.and_then(imperium_config::CatalogEntry::reasoning_setting);
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
            // Unset, the runtime reads the model's chat template.
            reasoning,
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
        reasoning: Some(reasoning.unwrap_or_else(|| imperium_api::reasoning_by_name(model))),
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
    /// The row is pinned to the CPU lane (`placement = "cpu"`): slow and for work that can
    /// wait, so only a rule or a name sends work to it, never the ranking by tier.
    pub cpu_lane: bool,
}

/// What a delegating agent asked for.
#[derive(Debug, Clone, Copy, Default)]
pub struct TaskRequest<'a> {
    /// A model table id or `<endpoint>:<model>` spec.
    pub named: Option<&'a str>,
    pub tier: Tier,
    pub location: Option<Location>,
    pub tags: &'a [String],
    /// The delegating session: local models other sessions are using are not available to it.
    pub session: Option<&'a str>,
}

/// Picks a subagent's model. Local models are considered only when `local` reports they can run
/// (loaded with a free slot, fitting in free memory, or fitting once idle models are swapped
/// out); a loaded model is preferred to loading one, and loading to swapping, and nothing in use
/// is evicted, so when memory is short the choice falls to hosted models.
///
/// 1. a model the agent named, if it is in the table;
/// 2. the first `[[routing.rules]]` entry matching the task's tier and tags decides: its `use`
///    list is tried in order (an explicit choice, so tier is not checked; location is), then
///    its location restriction applies to the rest. When no rule matched and
///    `routing.builtin_rules` is on, the first matching built-in task-class rule applies, but
///    its location and tier are preferences: with no suitable model there, its tier is tried in
///    every allowed location, then the task's own tier;
/// 3. otherwise the suitable models (at or above the tier, in the requested location) are
///    ranked by `routing.prefer`, then by price, with unpriced models counted as free; rows
///    pinned to the CPU lane are left out, since only work that can wait belongs there and a
///    rule or a name sends it;
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
        (true, Some(local)) => match request.session {
            Some(session) => local.availability_for(&c.model.model, session),
            None => local.availability(&c.model.model),
        },
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
    let matches = |rule: &&RoutingRule| {
        rule.when_tier.is_none_or(|tier| tier == request.tier)
            && rule
                .when_tag
                .as_ref()
                .is_none_or(|tag| request.tags.contains(tag))
    };
    // User rules first; the built-in task-class rules apply only when none of them matched.
    let user_rule = routing.rules.iter().find(matches);
    let builtin_rule = if user_rule.is_none() && routing.builtin_rules {
        RoutingSettings::builtin_rules()
            .into_iter()
            .find(|r| matches(&r))
    } else {
        None
    };
    let rule = user_rule.or(builtin_rule.as_ref());
    // A user rule's tier is a requirement; a built-in rule's is a preference.
    let tier = user_rule
        .and_then(|rule| rule.tier)
        .map_or(request.tier, |floor| floor.max(request.tier));
    let preferred_tier = builtin_rule
        .as_ref()
        .and_then(|rule| rule.tier)
        .map_or(tier, |floor| floor.max(tier));
    // An agent's or user rule's location is a requirement; a built-in rule's is a preference.
    // If no model fits a built-in rule's location and tier, its tier is tried in every allowed
    // location, then the task's own tier.
    let location = request
        .location
        .or_else(|| user_rule.and_then(|rule| rule.location));
    let preferred_location = builtin_rule.as_ref().and_then(|rule| rule.location);
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
    let best = |wanted: Option<Location>, tier: Tier| {
        catalog
            .iter()
            .filter(usable)
            .filter(|c| {
                !c.cpu_lane
                    && c.tier >= tier
                    && in_location(c)
                    && wanted.is_none_or(|w| c.location == w)
            })
            .min_by(|a, b| {
                let (a_rank, b_rank) = (rank(a), rank(b));
                a_rank
                    .0
                    .cmp(&b_rank.0)
                    .then(a_rank.1.total_cmp(&b_rank.1))
                    .then(a_rank.2.cmp(&b_rank.2))
            })
    };
    best(preferred_location, preferred_tier)
        .or_else(|| best(None, preferred_tier))
        .or_else(|| best(None, tier))
        .map_or_else(|| fallback.clone(), |chosen| chosen.model.clone())
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
    fn reasoning_comes_from_the_row_then_tags_then_the_name() {
        let table = format!(
            r#"{ENDPOINTS}
[[models.catalog]]
id = "r"
spec = "hosted:plain-model"
location = "cloud"
tier = "strong"
tags = ["reasoning"]

[[models.catalog]]
id = "off"
spec = "hosted:o3"
location = "cloud"
tier = "strong"
reasoning = false

[[models.catalog]]
id = "loc"
spec = "llamacpp:qwen"
location = "local"
tier = "basic"
"#
        );
        let settings = settings(&table);
        let resolve =
            |spec: &str| resolve_model(&settings, Some(spec), |_| Ok(Some("k".into()))).unwrap();
        assert_eq!(resolve("hosted:plain-model").reasoning, Some(true));
        assert_eq!(resolve("hosted:o3").reasoning, Some(false));
        assert_eq!(resolve("hosted:o4-mini").reasoning, Some(true));
        assert_eq!(resolve("hosted:gpt-4o").reasoning, Some(false));
        // A local model nothing configured is left to its runtime, then its name.
        let local = resolve("llamacpp:qwen");
        assert_eq!(local.reasoning, None);
        assert!(!local.reasons(None));
        assert!(resolve("llamacpp:Qwen3-8B").reasons(None));
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
                    reasoning: None,
                },
                tier,
                location,
                tags: tags.iter().map(|tag| tag.to_string()).collect(),
                cpu_lane: false,
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
            reasoning: None,
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
            ..RoutingSettings::default()
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

    fn class_catalog() -> Vec<CatalogModel> {
        let mut rows = catalog();
        let mut frontier = rows[3].clone();
        frontier.id = "frontier".into();
        frontier.model.model = "frontier".into();
        frontier.tier = Tier::Frontier;
        rows.push(frontier);
        rows
    }

    fn class_pick(routing: &RoutingSettings, tags: &[String]) -> String {
        let request = TaskRequest {
            tags,
            ..TaskRequest::default()
        };
        route(&class_catalog(), routing, &fallback(), request, None).model
    }

    #[test]
    fn task_classes_route_by_default_and_user_rules_override() {
        let routing = RoutingSettings::default();
        // Menial work goes to the local row even though a hosted one is as cheap or cheaper.
        assert_eq!(class_pick(&routing, &["tests".into()]), "local");
        // Hard work goes to a cloud frontier row, whatever tier the agent asked for.
        assert_eq!(class_pick(&routing, &["math".into()]), "frontier");
        // Untagged tasks are unaffected.
        assert_eq!(class_pick(&routing, &[]), "local");

        // A user rule that matches wins over the built-in one.
        let overridden = RoutingSettings {
            rules: vec![RoutingRule {
                when_tag: Some("tests".into()),
                use_models: vec!["mid".into()],
                ..RoutingRule::default()
            }],
            ..RoutingSettings::default()
        };
        assert_eq!(class_pick(&overridden, &["tests".into()]), "mid");
        assert_eq!(class_pick(&overridden, &["math".into()]), "frontier");

        // The built-ins can be switched off.
        let off = RoutingSettings {
            builtin_rules: false,
            ..RoutingSettings::default()
        };
        assert_eq!(class_pick(&off, &["math".into()]), "local");
    }

    #[test]
    fn a_task_class_floor_is_a_preference_and_menial_work_keeps_its_tier() {
        let routing = RoutingSettings::default();
        // No frontier row: a math task takes the best row at its own tier, not the session model.
        let tags = ["math".to_string()];
        let request = TaskRequest {
            tags: &tags,
            ..TaskRequest::default()
        };
        assert_eq!(
            route(&catalog(), &routing, &fallback(), request, None).model,
            "local"
        );
        let strong = TaskRequest {
            tier: Tier::Strong,
            ..request
        };
        assert_eq!(
            route(&catalog(), &routing, &fallback(), strong, None).model,
            "big"
        );
        // Menial work does not raise the tier, so a basic local row qualifies.
        let mut rows = catalog();
        rows.retain(|c| c.id == "cheap" || c.id == "mid");
        let mut basic_local = rows[0].clone();
        basic_local.id = "tiny".into();
        basic_local.model.model = "tiny".into();
        basic_local.location = Location::Local;
        rows.push(basic_local);
        let tests = ["tests".to_string()];
        let basic = TaskRequest {
            tier: Tier::Basic,
            tags: &tests,
            ..TaskRequest::default()
        };
        assert_eq!(
            route(&rows, &routing, &fallback(), basic, None).model,
            "tiny"
        );
        // A hard class beats a menial one on the same task.
        let both = ["ui".to_string(), "design".to_string()];
        let mixed = TaskRequest {
            tags: &both,
            ..TaskRequest::default()
        };
        assert_eq!(
            route(&class_catalog(), &routing, &fallback(), mixed, None).model,
            "frontier"
        );
    }

    #[test]
    fn a_task_class_location_is_a_preference() {
        let routing = RoutingSettings::default();
        let mut hosted_only = class_catalog();
        hosted_only.retain(|c| c.location == Location::Cloud);
        let request = |tags: &'static [String]| TaskRequest {
            tags,
            ..TaskRequest::default()
        };
        let tests: &'static [String] = Box::leak(vec!["tests".to_string()].into());
        // No local row: the normal router picks the cheapest hosted standard-or-better model.
        let chosen = route(&hosted_only, &routing, &fallback(), request(tests), None).model;
        assert_eq!(chosen, "mid");
    }

    #[test]
    fn a_hard_class_without_a_cloud_frontier_row_tries_frontier_elsewhere() {
        let routing = RoutingSettings::default();
        let mut rows = catalog();
        let mut local_frontier = rows[2].clone();
        local_frontier.id = "local-frontier".into();
        local_frontier.model.model = "local-frontier".into();
        local_frontier.tier = Tier::Frontier;
        rows.push(local_frontier);
        let tags = ["proof".to_string()];
        let request = TaskRequest {
            tags: &tags,
            ..TaskRequest::default()
        };
        // The frontier floor holds in another location before the task's own tier is used.
        assert_eq!(
            route(&rows, &routing, &fallback(), request, None).model,
            "local-frontier"
        );
        // An explicit location still binds: in the cloud there is no frontier row.
        let cloud = TaskRequest {
            location: Some(Location::Cloud),
            ..request
        };
        assert_eq!(
            route(&rows, &routing, &fallback(), cloud, None).model,
            "mid"
        );
    }

    #[test]
    fn a_user_rule_tier_is_a_floor() {
        let routing = RoutingSettings {
            rules: vec![RoutingRule {
                when_tag: Some("code".into()),
                tier: Some(Tier::Strong),
                ..RoutingRule::default()
            }],
            ..RoutingSettings::default()
        };
        let code = ["code".to_string()];
        let coding = TaskRequest {
            tier: Tier::Basic,
            tags: &code,
            ..TaskRequest::default()
        };
        assert_eq!(pick(&routing, coding), "big");
        // Unlike a built-in floor, it is a requirement: with nothing at or above it in the
        // allowed location, the session model is used.
        let local = TaskRequest {
            location: Some(Location::Local),
            ..coding
        };
        assert_eq!(pick(&routing, local), "session");
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
    fn cpu_lane_rows_are_chosen_by_rule_or_name_not_by_ranking() {
        let mut rows = local_catalog();
        let lane = rows.iter_mut().find(|c| c.id == "large").unwrap();
        lane.cpu_lane = true;
        lane.tier = Tier::Strong;
        lane.tags = vec!["critique".into()];
        let local = FakeLocal(vec![
            ("small", Availability::Fits),
            ("large", Availability::Fits),
        ]);
        let routing = RoutingSettings {
            prefer: Preference::Local,
            rules: vec![RoutingRule {
                when_tag: Some("critique".into()),
                use_models: vec!["large".into()],
                ..RoutingRule::default()
            }],
            ..RoutingSettings::default()
        };
        let pick = |routing: &RoutingSettings, request: TaskRequest<'_>| {
            route(&rows, routing, &fallback(), request, Some(&local)).model
        };
        // A strong task with no rule: the slow lane is not in the ranking, so it falls back to
        // the session's model (nothing else is strong).
        let strong = TaskRequest {
            tier: Tier::Strong,
            ..TaskRequest::default()
        };
        assert_eq!(pick(&routing, strong), "session");
        // A rule names it, or the agent does.
        let tags = ["critique".to_string()];
        let critique = TaskRequest {
            tags: &tags,
            ..TaskRequest::default()
        };
        assert_eq!(pick(&routing, critique), "large");
        let named = TaskRequest {
            named: Some("large"),
            ..TaskRequest::default()
        };
        assert_eq!(pick(&RoutingSettings::default(), named), "large");
        // When the lane cannot take the work (RAM short), the rule falls through to the ranking.
        let short = FakeLocal(vec![
            ("small", Availability::Fits),
            ("large", Availability::Unavailable),
        ]);
        assert_eq!(
            route(&rows, &routing, &fallback(), critique, Some(&short)).model,
            "small"
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
