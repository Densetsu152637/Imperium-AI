use imperium_api::{Availability, LocalModels};
use imperium_config::{
    Capability, LLAMACPP_ENDPOINT, Location, Preference, RoutingRule, RoutingSettings, SecretError,
    SecretName, Settings, Tier, VLLM_ENDPOINT,
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
    /// Only a rule or a name sends work to the row, never the ranking by tier: a row pinned to
    /// the CPU lane (`placement = "cpu"`, slow and for work that can wait), and a local model
    /// outside the model table, which `list_models` offers by name.
    pub named_only: bool,
    /// The row is pinned to the CPU lane (`placement = "cpu"`): the one the built-in rules for
    /// `critique`, `review` and `verify` tasks prefer.
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
    /// The subagent will call tools, so a model that cannot (`no-tools` on its row) is skipped.
    /// False for a `single_call` subagent, which gets none.
    pub needs_tools: bool,
}

/// A routing choice and what routing said about it on the way.
#[derive(Debug, Clone)]
pub struct Routed {
    pub model: ResolvedModel,
    /// Sentences for the user: the rule that sent the task to the CPU lane, and the models that
    /// were skipped, with the reason (a missing capability, a backed-up queue). Empty when
    /// routing had nothing out of the ordinary to say.
    pub notes: Vec<String>,
    /// Nothing fit, so `model` is the session's model that routing falls back to. False when
    /// `model` is a table row or a named model, even one that is also the session's model.
    pub fell_back: bool,
}

/// Picks a subagent's model, as [`route_noted`] does, and drops the notes.
pub fn route(
    catalog: &[CatalogModel],
    routing: &RoutingSettings,
    fallback: &ResolvedModel,
    request: TaskRequest<'_>,
    local: Option<&dyn LocalModels>,
) -> ResolvedModel {
    route_noted(catalog, routing, fallback, request, local).model
}

/// Picks a subagent's model. Local models are considered only when `local` reports they can run
/// (loaded with a free slot, fitting in free memory, or fitting once idle models are swapped
/// out); a loaded model is preferred to loading one, and loading to swapping, and nothing in use
/// is evicted, so when memory is short the choice falls to hosted models.
///
/// Models are skipped, with a reason in the notes, when they lack a capability the task needs
/// (`vision`, `tools`, `long_context`: from the task's tags, the delegating call, or the
/// matching rule's `require`), or when a local model's expected queue wait is over
/// `routing.max_queue_wait_s` (rows reached only by a rule or a name are never skipped for
/// that).
///
/// 1. a model the agent named, if it is in the table;
/// 2. the first `[[routing.rules]]` entry matching the task's tier and tags decides: its `use`
///    list is tried in order (an explicit choice, so tier is not checked; location is), then
///    its location restriction applies to the rest. When no rule matched and
///    `routing.builtin_rules` is on, the first matching built-in task-class rule applies, but
///    its location and tier are preferences: with no suitable model there, its tier is tried in
///    every allowed location, then the task's own tier. Failing that, the built-in rules for
///    `critique`, `review`, `verify` and `proof-check` tasks prefer a CPU-lane row that can take
///    the task now (`routing.builtin_cpu_lane`), else the ranking decides;
/// 3. otherwise the suitable models (at or above the tier, in the requested location) are
///    ranked by `routing.prefer`, then by price, with unpriced models counted as free; rows
///    pinned to the CPU lane are left out, since only work that can wait belongs there and a
///    rule or a name sends it;
/// 4. otherwise `fallback`, the session's model.
pub fn route_noted(
    catalog: &[CatalogModel],
    routing: &RoutingSettings,
    fallback: &ResolvedModel,
    request: TaskRequest<'_>,
    local: Option<&dyn LocalModels>,
) -> Routed {
    let availability = |c: &CatalogModel| match (c.model.local, local) {
        (false, _) => Availability::Loaded,
        (true, Some(local)) => match request.session {
            Some(session) => local.availability_for(&c.model.model, session),
            None => local.availability(&c.model.model),
        },
        (true, None) => Availability::Unavailable,
    };
    let matches = |rule: &&RoutingRule| {
        rule.when_tier.is_none_or(|tier| tier == request.tier)
            && rule
                .when_tag
                .as_ref()
                .is_none_or(|tag| request.tags.contains(tag))
    };
    // User rules first; the built-in task-class rules apply only when none of them matched,
    // and the built-in CPU-lane rules only when neither did.
    let user_rule = routing.rules.iter().find(matches);
    let builtin_rule = if user_rule.is_none() && routing.builtin_rules {
        RoutingSettings::builtin_rules()
            .into_iter()
            .find(|r| matches(&r))
    } else {
        None
    };
    let lane_rule = if user_rule.is_none()
        && builtin_rule.is_none()
        && routing.builtin_rules
        && routing.builtin_cpu_lane
    {
        RoutingSettings::cpu_lane_rules()
            .into_iter()
            .find(|r| matches(&r))
    } else {
        None
    };
    let rule = user_rule.or(builtin_rule.as_ref()).or(lane_rule.as_ref());
    // What the task needs of a model: from the call, its tags, and the matching rule.
    let mut required: Vec<Capability> = Vec::new();
    if request.needs_tools {
        required.push(Capability::Tools);
    }
    if request.tags.iter().any(|tag| tag == "vision") {
        required.push(Capability::Vision);
    }
    if request.tags.iter().any(|tag| tag == "long-context") {
        required.push(Capability::LongContext);
    }
    if let Some(rule) = rule {
        required.extend(rule.require.iter().copied());
    }
    let missing = |c: &CatalogModel| -> Option<String> {
        required.iter().find_map(|capability| match capability {
            Capability::Vision => (!c.tags.iter().any(|tag| tag == "vision"))
                .then(|| "it reads no images (its row lacks the tag `vision`)".to_string()),
            Capability::Tools => c
                .tags
                .iter()
                .any(|tag| tag == "no-tools")
                .then(|| "it cannot call tools (its row has the tag `no-tools`)".to_string()),
            Capability::LongContext => {
                let wanted = u64::from(routing.long_context_tokens);
                let window = c.model.context_window.or_else(|| {
                    local
                        .filter(|_| c.model.local)
                        .and_then(|local| local.planned_context(&c.model.model))
                });
                match window {
                    Some(window) if window >= wanted => None,
                    Some(window) => Some(format!(
                        "its context window of {window} tokens is under {wanted}"
                    )),
                    None => Some(format!(
                        "its context window is unknown, and {wanted} tokens are needed"
                    )),
                }
            }
        })
    };
    let queue_wait = |c: &CatalogModel| match (c.model.local, local) {
        (true, Some(local)) => local.queue_wait_s(&c.model.model),
        _ => 0,
    };
    let backed_up =
        |c: &CatalogModel| routing.max_queue_wait_s > 0 && queue_wait(c) > routing.max_queue_wait_s;
    // Why the router passes over a row that could otherwise take the task. Rows reached only
    // by a rule or a name (the CPU lane) are slow on purpose and never skipped for their queue.
    let skip = |c: &CatalogModel| -> Option<String> {
        if let Some(reason) = missing(c) {
            return Some(reason);
        }
        (!c.named_only && backed_up(c)).then(|| {
            format!(
                "its expected queue wait of {} s is over the {} s limit (routing.max_queue_wait_s)",
                queue_wait(c),
                routing.max_queue_wait_s
            )
        })
    };
    let usable =
        |c: &&CatalogModel| availability(c) != Availability::Unavailable && skip(c).is_none();
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
    // The models passed over that were in the running, for the user to read.
    let skipped = || -> Option<String> {
        let passed_over: Vec<String> = catalog
            .iter()
            .filter(|c| availability(c) != Availability::Unavailable)
            .filter(|c| {
                request
                    .named
                    .is_some_and(|named| c.id == named || c.model.spec == named)
                    || rule.is_some_and(|rule| rule.use_models.contains(&c.id))
                    || (!c.named_only && c.tier >= tier && in_location(c))
            })
            .filter_map(|c| skip(c).map(|reason| format!("{} ({reason})", c.id)))
            .collect();
        (!passed_over.is_empty()).then(|| format!("routing skipped {}", passed_over.join("; ")))
    };
    let finish = |model: ResolvedModel, note: Option<String>, fell_back: bool| Routed {
        model,
        notes: note.into_iter().chain(skipped()).collect(),
        fell_back,
    };
    if let Some(named) = request.named
        && let Some(chosen) = catalog
            .iter()
            .filter(usable)
            .find(|c| c.id == named || c.model.spec == named)
    {
        return finish(chosen.model.clone(), None, false);
    }
    let mut note = None;
    if let Some(rule) = rule {
        let preferred = rule
            .use_models
            .iter()
            .filter_map(|id| catalog.iter().find(|c| &c.id == id))
            .filter(usable)
            .find(|c| in_location(c));
        if let Some(chosen) = preferred {
            return finish(chosen.model.clone(), None, false);
        }
        if rule.cpu_lane {
            // Work that can wait prefers the slow lane, but only a row that can take it now:
            // a lane backed up past the limit is passed over like any other local model.
            let lane = catalog
                .iter()
                .filter(usable)
                .filter(|c| c.cpu_lane && c.tier >= tier && in_location(c) && !backed_up(c))
                .min_by_key(|c| (std::cmp::Reverse(c.tier), availability(c)));
            let tag = rule.when_tag.as_deref().unwrap_or("this");
            if let Some(chosen) = lane {
                return finish(
                    chosen.model.clone(),
                    Some(format!(
                        "{tag} tasks prefer the CPU lane: sent to {}",
                        chosen.id
                    )),
                    false,
                );
            }
            if catalog.iter().any(|c| c.cpu_lane) {
                note = Some(format!(
                    "no CPU-lane row can take the {tag} task now, so it was routed by tier"
                ));
            }
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
                !c.named_only
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
    let chosen = best(preferred_location, preferred_tier)
        .or_else(|| best(None, preferred_tier))
        .or_else(|| best(None, tier));
    if let Some(chosen) = chosen {
        return finish(chosen.model.clone(), note, false);
    }
    // A task confined to one location (an explicit `location`, or a rule's) stays there: when
    // only the queue limit left nothing in it, take the least bad row in that location rather
    // than the session's model, which may be somewhere else.
    if location.is_some() && routing.max_queue_wait_s > 0 {
        let relaxed = RoutingSettings {
            max_queue_wait_s: 0,
            ..routing.clone()
        };
        let mut routed = route_noted(catalog, &relaxed, fallback, request, local);
        if !routed.fell_back {
            routed.notes.insert(
                0,
                "every model in the task's location was backed up, so the queue limit was not applied"
                    .to_string(),
            );
            return routed;
        }
    }
    finish(fallback.clone(), note, true)
}

/// The location a task is confined to: the call's, else the first matching rule of the user's.
pub fn effective_location(routing: &RoutingSettings, request: TaskRequest<'_>) -> Option<Location> {
    request.location.or_else(|| {
        routing
            .rules
            .iter()
            .find(|rule| {
                rule.when_tier.is_none_or(|tier| tier == request.tier)
                    && rule
                        .when_tag
                        .as_ref()
                        .is_none_or(|tag| request.tags.contains(tag))
            })
            .and_then(|rule| rule.location)
    })
}

/// The next tier up from `tier`; `None` for the highest.
fn tier_above(tier: Tier) -> Option<Tier> {
    match tier {
        Tier::Basic => Some(Tier::Standard),
        Tier::Standard => Some(Tier::Strong),
        Tier::Strong => Some(Tier::Frontier),
        Tier::Frontier => None,
    }
}

/// A stronger route for a task whose model `failed` (a `<endpoint>:<model>` spec) failed it or
/// answered with low confidence, or `None` when there is none to try. `attempt` counts the
/// retries including this one, starting at 1; the caller stops at
/// `routing.escalation_attempts`, which this function does not check, and checks the budget.
///
/// The first rule matching the task whose `escalate` list has an entry for this attempt names
/// the model (when it is usable and has the capabilities the task needs); otherwise the task
/// is routed again from the tier above the failed model's, never to the failed model. The
/// highest tier has nowhere to go.
pub fn escalate(
    catalog: &[CatalogModel],
    routing: &RoutingSettings,
    fallback: &ResolvedModel,
    request: TaskRequest<'_>,
    failed: &str,
    attempt: u32,
    local: Option<&dyn LocalModels>,
) -> Option<Routed> {
    let rest: Vec<CatalogModel> = catalog
        .iter()
        .filter(|c| c.model.spec != failed)
        .cloned()
        .collect();
    // A route is acceptable only when it is a table row (never what the router falls back to
    // when nothing fits; a real row that happens to be the session's model is fine), in the
    // task's location, at or above `floor`.
    let acceptable = |routed: &Routed, floor: Tier| {
        !routed.fell_back
            && rest
                .iter()
                .find(|c| c.model.spec == routed.model.spec)
                .is_some_and(|c| {
                    c.tier >= floor && request.location.is_none_or(|wanted| c.location == wanted)
                })
    };
    let listed = routing
        .rules
        .iter()
        .find(|rule| {
            rule.when_tier.is_none_or(|tier| tier == request.tier)
                && rule
                    .when_tag
                    .as_ref()
                    .is_none_or(|tag| request.tags.contains(tag))
        })
        .and_then(|rule| {
            let index = usize::try_from(attempt.saturating_sub(1)).ok()?;
            rule.escalate.get(index)
        });
    if let Some(id) = listed {
        let named = TaskRequest {
            named: Some(id.as_str()),
            ..request
        };
        let routed = route_noted(&rest, routing, fallback, named, local);
        if acceptable(&routed, request.tier)
            && catalog
                .iter()
                .any(|c| &c.id == id && c.model.spec == routed.model.spec)
        {
            return Some(routed);
        }
    }
    let failed_tier = catalog
        .iter()
        .find(|c| c.model.spec == failed)
        .map_or(request.tier, |c| c.tier);
    let above = tier_above(failed_tier)?;
    let stronger = TaskRequest {
        named: None,
        tier: above,
        ..request
    };
    let routed = route_noted(&rest, routing, fallback, stronger, local);
    acceptable(&routed, above).then_some(routed)
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
                named_only: false,
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
        lane.named_only = true;
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

    /// Local models with fixed availability and queue waits, standing in for the supervisor.
    #[derive(Debug)]
    struct BusyLocal(Vec<(&'static str, Availability, u32)>);

    impl LocalModels for BusyLocal {
        fn availability(&self, model: &str) -> Availability {
            self.0
                .iter()
                .find(|(id, ..)| *id == model)
                .map_or(Availability::Unavailable, |(_, availability, _)| {
                    *availability
                })
        }

        fn queue_wait_s(&self, model: &str) -> u32 {
            self.0
                .iter()
                .find(|(id, ..)| *id == model)
                .map_or(0, |(.., wait)| *wait)
        }

        fn acquire<'a>(
            &'a self,
            _model: &'a str,
            _holder: &'a str,
        ) -> imperium_api::AcquireFuture<'a> {
            Box::pin(async { Err("not used".to_string()) })
        }
    }

    fn tagged(rows: &mut [CatalogModel], id: &str, tags: &[&str]) {
        let row = rows.iter_mut().find(|c| c.id == id).unwrap();
        row.tags = tags.iter().map(|tag| tag.to_string()).collect();
    }

    #[test]
    fn capability_filters_skip_models_with_a_reason() {
        let routing = RoutingSettings::default();
        let mut rows = catalog();
        tagged(&mut rows, "big", &["vision"]);
        tagged(&mut rows, "cheap", &["no-tools"]);
        rows.iter_mut()
            .find(|c| c.id == "mid")
            .unwrap()
            .model
            .context_window = Some(200_000);
        rows.iter_mut()
            .find(|c| c.id == "cheap")
            .unwrap()
            .model
            .context_window = Some(8_000);
        let run = |request: TaskRequest<'_>, rows: &[CatalogModel]| {
            route_noted(rows, &routing, &fallback(), request, None)
        };
        let basic = TaskRequest {
            tier: Tier::Basic,
            ..TaskRequest::default()
        };
        // Without a need, the cheapest model (the unpriced one) is chosen and nothing is said.
        let plain = run(basic, &rows);
        assert_eq!(plain.model.model, "local");
        assert!(plain.notes.is_empty());
        // A task tagged `vision` goes to the row that declares it.
        let tags = ["vision".to_string()];
        let seeing = run(
            TaskRequest {
                tags: &tags,
                ..basic
            },
            &rows,
        );
        assert_eq!(seeing.model.model, "big");
        assert!(seeing.notes[0].contains("cheap") && seeing.notes[0].contains("vision"));
        // A subagent that calls tools skips a row marked `no-tools`.
        let tools = run(
            TaskRequest {
                needs_tools: true,
                ..basic
            },
            &rows,
        );
        assert_eq!(tools.model.model, "local");
        assert!(tools.notes[0].contains("cheap") && tools.notes[0].contains("no-tools"));
        // Long context needs a known window of at least `long_context_tokens`.
        let tags = ["long-context".to_string()];
        let long = run(
            TaskRequest {
                tags: &tags,
                ..basic
            },
            &rows,
        );
        assert_eq!(long.model.model, "mid");
        assert!(long.notes[0].contains("8000") && long.notes[0].contains("unknown"));
        // A rule can require a capability for the tasks it matches, and a named model that
        // lacks it is passed over too.
        let rule_routing = RoutingSettings {
            rules: vec![RoutingRule {
                when_tag: Some("scan".into()),
                require: vec![Capability::Vision],
                ..RoutingRule::default()
            }],
            ..RoutingSettings::default()
        };
        let tags = ["scan".to_string()];
        let scan = TaskRequest {
            tags: &tags,
            named: Some("cheap"),
            ..basic
        };
        let routed = route_noted(&rows, &rule_routing, &fallback(), scan, None);
        assert_eq!(routed.model.model, "big");
        assert!(routed.notes[0].contains("cheap"));
        // No model qualifies: the session's model carries on.
        let mut blind = catalog();
        tagged(&mut blind, "big", &[]);
        let none = run(
            TaskRequest {
                tags: &["vision".to_string()],
                ..basic
            },
            &blind,
        );
        assert_eq!(none.model.model, "session");
    }

    #[test]
    fn a_backed_up_local_model_falls_to_the_next_route_with_a_notice() {
        let routing = RoutingSettings {
            prefer: Preference::Local,
            ..RoutingSettings::default()
        };
        let request = TaskRequest::default();
        let run = |routing: &RoutingSettings, local: &BusyLocal, request: TaskRequest<'_>| {
            route_noted(&local_catalog(), routing, &fallback(), request, Some(local))
        };
        let loaded = |small: u32, large: u32| {
            BusyLocal(vec![
                ("small", Availability::Loaded, small),
                ("large", Availability::Loaded, large),
            ])
        };
        // Idle or within the limit: the first local row wins as before, with nothing said.
        let calm = run(&routing, &loaded(0, 0), request);
        assert_eq!(calm.model.model, "small");
        assert!(calm.notes.is_empty());
        assert_eq!(run(&routing, &loaded(120, 0), request).model.model, "small");
        // Past the limit: the next eligible local row, with the reason.
        let backed_up = run(&routing, &loaded(300, 10), request);
        assert_eq!(backed_up.model.model, "large");
        assert!(backed_up.notes[0].contains("small") && backed_up.notes[0].contains("300"));
        // Every local row backed up: the hosted model takes the task.
        assert_eq!(run(&routing, &loaded(300, 400), request).model.model, "mid");
        // The limit is configurable; zero turns the check off.
        let patient = RoutingSettings {
            max_queue_wait_s: 600,
            ..routing.clone()
        };
        assert_eq!(
            run(&patient, &loaded(300, 10), request).model.model,
            "small"
        );
        let off = RoutingSettings {
            max_queue_wait_s: 0,
            ..routing.clone()
        };
        assert_eq!(run(&off, &loaded(9999, 9999), request).model.model, "small");
        // A named local model that is backed up is passed over like the ranking's choice.
        let named = TaskRequest {
            named: Some("small"),
            ..request
        };
        assert_eq!(run(&routing, &loaded(300, 10), named).model.model, "large");
    }

    #[test]
    fn a_task_confined_to_a_location_stays_there_when_every_row_is_backed_up() {
        let routing = RoutingSettings {
            prefer: Preference::Local,
            ..RoutingSettings::default()
        };
        let local = BusyLocal(vec![
            ("small", Availability::Loaded, 300),
            ("large", Availability::Loaded, 400),
        ]);
        let confined = TaskRequest {
            location: Some(Location::Local),
            ..TaskRequest::default()
        };
        let routed = route_noted(
            &local_catalog(),
            &routing,
            &fallback(),
            confined,
            Some(&local),
        );
        // Not the session's model, not the hosted row: a local row, with the reason.
        assert_eq!(routed.model.model, "small");
        assert!(routed.notes[0].contains("backed up"), "{:?}", routed.notes);
        // A rule's location confines the task the same way.
        let ruled = RoutingSettings {
            rules: vec![RoutingRule {
                when_tag: Some("local-only".into()),
                location: Some(Location::Local),
                ..RoutingRule::default()
            }],
            ..routing
        };
        let tags = ["local-only".to_string()];
        let request = TaskRequest {
            tags: &tags,
            ..TaskRequest::default()
        };
        let routed = route_noted(&local_catalog(), &ruled, &fallback(), request, Some(&local));
        assert_eq!(routed.model.model, "small");
    }

    #[test]
    fn a_user_rule_confining_a_tag_to_local_wins_over_a_repo_rule_naming_an_unusable_local_row() {
        // The user's rule comes first: `private` work stays local. The repository's rule for the
        // same tag names a local row that cannot run; it never gets a say, and the task still
        // lands on a local row that can (not the hosted row the ranking would otherwise prefer).
        let routing = RoutingSettings {
            prefer: Preference::Cloud,
            rules: vec![
                RoutingRule {
                    when_tag: Some("private".into()),
                    location: Some(Location::Local),
                    ..RoutingRule::default()
                },
                RoutingRule {
                    when_tag: Some("private".into()),
                    use_models: vec!["large".into()],
                    ..RoutingRule::default()
                },
            ],
            ..RoutingSettings::default()
        };
        // `large` is not in the supervisor's list, so it cannot run; `small` is loaded.
        let local = BusyLocal(vec![("small", Availability::Loaded, 0)]);
        let tags = ["private".to_string()];
        let request = TaskRequest {
            tags: &tags,
            ..TaskRequest::default()
        };
        let routed = route_noted(
            &local_catalog(),
            &routing,
            &fallback(),
            request,
            Some(&local),
        );
        assert_eq!(routed.model.model, "small");
        assert!(routed.model.local);
    }

    #[test]
    fn escalation_accepts_a_real_row_that_is_also_the_session_model_but_not_the_fallback() {
        let routing = RoutingSettings::default();
        let request = TaskRequest {
            tier: Tier::Basic,
            ..TaskRequest::default()
        };
        // The table's strong row is the session's own model: a real row, so a fine target.
        let mut rows = catalog();
        let strong = rows.iter_mut().find(|c| c.id == "big").unwrap();
        strong.model.spec = fallback().spec;
        let routed = escalate(
            &rows,
            &routing,
            &fallback(),
            request,
            "endpoint:mid",
            1,
            None,
        )
        .unwrap();
        assert!(!routed.fell_back);
        assert_eq!(routed.model.model, "big");
        // With no row above the failed one, routing falls back to the session model: not a target.
        let mut rest = catalog();
        rest.retain(|c| c.id == "mid");
        let fell = route_noted(
            &rest,
            &routing,
            &fallback(),
            TaskRequest {
                tier: Tier::Strong,
                ..request
            },
            None,
        );
        assert!(fell.fell_back);
        assert!(
            escalate(
                &rest,
                &routing,
                &fallback(),
                request,
                "endpoint:mid",
                1,
                None
            )
            .is_none()
        );
    }

    #[test]
    fn escalation_stays_in_the_tasks_location_and_never_falls_back_to_the_session_model() {
        let routing = RoutingSettings::default();
        let request = TaskRequest {
            tier: Tier::Basic,
            location: Some(Location::Local),
            ..TaskRequest::default()
        };
        // Nothing local above the failed model: no retry, though cloud rows exist.
        let mut rows = catalog();
        rows.retain(|c| c.location == Location::Cloud);
        assert!(
            escalate(
                &rows,
                &routing,
                &fallback(),
                request,
                "endpoint:cheap",
                1,
                None
            )
            .is_none()
        );
        // A rule's escalate list naming a cloud row does not move a local task off the machine.
        let listed = RoutingSettings {
            rules: vec![RoutingRule {
                when_tag: Some("code".into()),
                escalate: vec!["big".into()],
                ..RoutingRule::default()
            }],
            ..RoutingSettings::default()
        };
        let tags = ["code".to_string()];
        let coding = TaskRequest {
            tags: &tags,
            ..request
        };
        assert!(
            escalate(
                &rows,
                &listed,
                &fallback(),
                coding,
                "endpoint:cheap",
                1,
                None
            )
            .is_none()
        );
        // A row that happens to be the session's own model is still a real row, so a stronger
        // route (only the result routing falls back to is refused).
        let mut same = catalog();
        same.retain(|c| c.id == "cheap" || c.id == "mid");
        same.iter_mut().find(|c| c.id == "mid").unwrap().model.spec = fallback().spec;
        let anywhere = TaskRequest {
            tier: Tier::Basic,
            ..TaskRequest::default()
        };
        let same_row = escalate(
            &same,
            &routing,
            &fallback(),
            anywhere,
            "endpoint:cheap",
            1,
            None,
        )
        .unwrap();
        assert_eq!(same_row.model.model, "mid");
        assert!(!same_row.fell_back);
        // The retry is at least the tier above the failed model's.
        let low = TaskRequest {
            tier: Tier::Strong,
            ..TaskRequest::default()
        };
        let mut weak = catalog();
        weak.retain(|c| c.id != "big");
        assert!(escalate(&weak, &routing, &fallback(), low, "endpoint:mid", 1, None).is_none());
    }

    #[test]
    fn rows_reached_by_name_are_not_skipped_for_their_queue() {
        let mut rows = local_catalog();
        let lane = rows.iter_mut().find(|c| c.id == "large").unwrap();
        lane.named_only = true;
        lane.cpu_lane = true;
        let local = BusyLocal(vec![
            ("small", Availability::Fits, 0),
            ("large", Availability::Loaded, 3600),
        ]);
        let named = TaskRequest {
            named: Some("large"),
            ..TaskRequest::default()
        };
        let routed = route_noted(
            &rows,
            &RoutingSettings::default(),
            &fallback(),
            named,
            Some(&local),
        );
        assert_eq!(routed.model.model, "large");
        assert!(routed.notes.is_empty());
    }

    fn lane_catalog() -> Vec<CatalogModel> {
        let mut rows = local_catalog();
        let lane = rows.iter_mut().find(|c| c.id == "large").unwrap();
        lane.named_only = true;
        lane.cpu_lane = true;
        lane.tier = Tier::Strong;
        rows
    }

    #[test]
    fn critique_review_and_verify_prefer_the_cpu_lane_when_it_can_take_them() {
        let rows = lane_catalog();
        let routing = RoutingSettings::default();
        let idle = BusyLocal(vec![
            ("small", Availability::Fits, 0),
            ("large", Availability::Fits, 0),
        ]);
        for tag in ["critique", "review", "verify", "proof-check"] {
            let tags = [tag.to_string()];
            let request = TaskRequest {
                tags: &tags,
                ..TaskRequest::default()
            };
            let routed = route_noted(&rows, &routing, &fallback(), request, Some(&idle));
            assert_eq!(routed.model.model, "large", "{tag}");
            assert!(routed.notes[0].contains("CPU lane"), "{tag}");
        }
        let tags = ["review".to_string()];
        let review = TaskRequest {
            tags: &tags,
            ..TaskRequest::default()
        };
        // An ordinary task never lands on the lane by ranking.
        let plain = route_noted(
            &rows,
            &routing,
            &fallback(),
            TaskRequest::default(),
            Some(&idle),
        );
        assert_ne!(plain.model.model, "large");
        // The lane cannot take the task (RAM short), or is backed up past the limit: the
        // ranking decides, and the notice says why the lane was not used.
        let short = BusyLocal(vec![
            ("small", Availability::Fits, 0),
            ("large", Availability::Unavailable, 0),
        ]);
        let busy = BusyLocal(vec![
            ("small", Availability::Fits, 0),
            ("large", Availability::Loaded, 500),
        ]);
        for local in [&short, &busy] {
            let routed = route_noted(&rows, &routing, &fallback(), review, Some(local));
            assert_eq!(routed.model.model, "small");
            assert!(routed.notes[0].contains("no CPU-lane row"));
        }
        // A task that asks for a tier above the lane's goes by tier.
        let frontier = TaskRequest {
            tier: Tier::Frontier,
            ..review
        };
        assert_ne!(
            route_noted(&rows, &routing, &fallback(), frontier, Some(&idle))
                .model
                .model,
            "large"
        );
        // A rule of the user's for the same tag replaces the built-in one, and the switches
        // turn it off.
        let own = RoutingSettings {
            rules: vec![RoutingRule {
                when_tag: Some("review".into()),
                location: Some(Location::Cloud),
                ..RoutingRule::default()
            }],
            ..RoutingSettings::default()
        };
        assert_eq!(
            route_noted(&rows, &own, &fallback(), review, Some(&idle))
                .model
                .model,
            "mid"
        );
        for off in [
            RoutingSettings {
                builtin_cpu_lane: false,
                ..RoutingSettings::default()
            },
            RoutingSettings {
                builtin_rules: false,
                ..RoutingSettings::default()
            },
        ] {
            let routed = route_noted(&rows, &off, &fallback(), review, Some(&idle));
            assert_ne!(routed.model.model, "large");
            assert!(routed.notes.is_empty());
        }
    }

    #[test]
    fn escalation_retries_on_a_stronger_route_and_never_on_the_failed_model() {
        let routing = RoutingSettings::default();
        let rows = catalog();
        let request = TaskRequest {
            tier: Tier::Basic,
            ..TaskRequest::default()
        };
        // From the basic row, the cheapest model of the tier above.
        let up = escalate(
            &rows,
            &routing,
            &fallback(),
            request,
            "endpoint:cheap",
            1,
            None,
        )
        .unwrap();
        assert_eq!(up.model.model, "local");
        // From the strong row, the frontier tier has nothing in the table, so no retry.
        let strong = TaskRequest {
            tier: Tier::Strong,
            ..TaskRequest::default()
        };
        assert!(
            escalate(
                &rows,
                &routing,
                &fallback(),
                strong,
                "endpoint:big",
                1,
                None
            )
            .is_none()
        );
        // A rule lists the models to try, one per attempt, then the tier above takes over.
        let listed = RoutingSettings {
            rules: vec![RoutingRule {
                when_tag: Some("code".into()),
                escalate: vec!["big".into()],
                ..RoutingRule::default()
            }],
            ..RoutingSettings::default()
        };
        let tags = ["code".to_string()];
        let coding = TaskRequest {
            tags: &tags,
            ..request
        };
        let first = escalate(
            &rows,
            &listed,
            &fallback(),
            coding,
            "endpoint:cheap",
            1,
            None,
        )
        .unwrap();
        assert_eq!(first.model.model, "big");
        let second = escalate(
            &rows,
            &listed,
            &fallback(),
            coding,
            "endpoint:cheap",
            2,
            None,
        )
        .unwrap();
        assert_eq!(second.model.model, "local");
        // The session's own model is not a stronger route.
        let mut only = catalog();
        only.retain(|c| c.id == "cheap");
        assert!(
            escalate(
                &only,
                &routing,
                &fallback(),
                request,
                "endpoint:cheap",
                1,
                None
            )
            .is_none()
        );
    }
}
