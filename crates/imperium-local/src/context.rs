//! How much context a local model's agents get and how big its KV cache is.
//!
//! llama.cpp reserves the whole KV cache (`--ctx-size`) when the server starts and cannot grow
//! it in place. Reserved but empty cells cost memory, not bandwidth (attention reads only the
//! filled ones), so what is kept small is the reservation:
//!
//! - The **ceiling** is the most one agent may use: the least of what the model was trained
//!   for, the configured limit (`auto` sets none), and the largest pool that fits beside the
//!   weights without pushing layers off the GPU. Compaction is planned against it.
//! - The **pool** is what the server reserves: it starts at the initial pool (or what the first
//!   call needs) and, when a call needs more, grows by doubling, capped at the ceiling. A grown
//!   pool takes a restart, made between calls; the supervisor shrinks it back when the model is
//!   unloaded.

use imperium_config::{ModelLaunch, NO_CONTEXT_LIMIT};

/// The ceiling of an `auto` context when the model does not say what it was trained for.
pub const UNKNOWN_TRAINED_CONTEXT: u32 = 32_768;
/// Pools grow to multiples of this many tokens.
pub const GROW_ALIGN: u32 = 1024;
/// Smallest pool the fit search considers.
pub const MIN_FIT_TOKENS: u32 = 1024;
/// The fewest tokens a model's pool and context are cut to so its layers stay on the GPU (or
/// its context, if smaller): below this an agent cannot work, and layers go to the CPU instead.
pub const MIN_WORKING_CONTEXT: u32 = 8192;
/// Largest pool the fit search considers.
const MAX_FIT_TOKENS: u32 = 1 << 24;
/// Tokens of reply a call is assumed to need when replies are not capped.
pub const UNCAPPED_REPLY_TOKENS: u32 = 8192;

/// Tokens a call needs in the pool: its prompt and the reply it may generate. `None` when the
/// prompt is unknown, so the need cannot be told.
pub fn demand(prompt_tokens: Option<u32>, max_output_tokens: u32) -> Option<u32> {
    let reply = if max_output_tokens > 0 {
        max_output_tokens
    } else {
        UNCAPPED_REPLY_TOKENS
    };
    prompt_tokens.map(|prompt| prompt.saturating_add(reply))
}

/// The upper end of the fit search for `launch`: every slot at the full context (its own limit
/// and what the model was trained for, assumed for an `auto` context of a model that does not
/// say), or its pool if larger. No pool is ever worth more than that.
pub fn search_limit(launch: &ModelLaunch, trained: Option<u64>) -> u32 {
    trained_cap(launch, trained)
        .min(launch.context)
        .saturating_mul(launch.slots.max(1))
        .max(launch.pool_tokens)
        .clamp(MIN_FIT_TOKENS, MAX_FIT_TOKENS)
}

fn trained_cap(launch: &ModelLaunch, trained: Option<u64>) -> u32 {
    match trained {
        Some(trained) => u32::try_from(trained).unwrap_or(u32::MAX).max(1),
        None if launch.context_auto => UNKNOWN_TRAINED_CONTEXT,
        None => NO_CONTEXT_LIMIT,
    }
}

/// The largest pool in `lo..=hi` for which `fits` holds, assuming that once a pool does not fit
/// no larger one does; `lo` when none does. Found to [`GROW_ALIGN`] tokens.
pub fn largest_fitting(lo: u32, hi: u32, fits: impl Fn(u32) -> bool) -> u32 {
    if hi <= lo || !fits(lo) {
        return lo;
    }
    if fits(hi) {
        return hi;
    }
    // `fits(good)`, `!fits(bad)`.
    let (mut good, mut bad) = (lo, hi);
    while bad - good > GROW_ALIGN {
        let mid = good + (bad - good) / 2;
        if fits(mid) {
            good = mid;
        } else {
            bad = mid;
        }
    }
    good
}

/// `launch` with its context and pool sized for one model: `trained` is the context the model
/// was trained for, `fit` the largest pool that fits (see [`largest_fitting`]), `grown` the pool
/// the model's calls have asked for so far, and `slot_aware` whether the runtime shares one
/// pool between slots (llama.cpp) rather than sizing its own (vLLM).
///
/// The KV memory is what gives way first: a model whose weights fit on the GPU keeps every
/// layer there, and its pool (fixed by a setting or not) is cut to the tokens left, never below
/// [`MIN_WORKING_CONTEXT`] (or the context, if smaller). The context becomes the ceiling; a pool
/// no setting fixes becomes the larger of the initial pool and `grown`, capped at what fits
/// ([`ModelLaunch::pool_max`]), which may be more than one context so several agents fill it.
/// Slots never size the pool. A fixed pool that does not fit is cut and flagged
/// ([`ModelLaunch::pool_clamped`]).
pub fn size(
    launch: &ModelLaunch,
    trained: Option<u64>,
    fit: u32,
    grown: u32,
    slot_aware: bool,
) -> ModelLaunch {
    let mut sized = launch.clone();
    let wanted = launch.context.min(trained_cap(launch, trained)).max(1);
    // The most KV the memory takes, but always a working context.
    let room = fit.max(MIN_WORKING_CONTEXT.min(wanted));
    if launch.pool_fixed && slot_aware {
        // The pool is what a setting fixed, cut to what fits with the layers on the GPU; one
        // agent may use at most all of it.
        sized.pool_tokens = launch.pool_tokens.min(room).max(1);
        sized.pool_clamped = sized.pool_tokens < launch.pool_tokens;
        sized.pool_max = sized.pool_tokens;
        sized.context = wanted.min(sized.pool_tokens);
        return sized;
    }
    let initial = launch.pool_tokens.max(1);
    sized.context = wanted.min(room);
    sized.pool_clamped = false;
    if !slot_aware {
        // vLLM sizes its own KV cache; the pool only estimates its memory.
        sized.pool_tokens = if launch.pool_fixed {
            launch.pool_tokens
        } else {
            sized.context
        };
        sized.pool_max = sized.pool_tokens;
        return sized;
    }
    sized.pool_max = room;
    sized.pool_tokens = initial.max(grown).min(room);
    sized
}

/// The pool to restart with so a call needing `need` tokens fits, growing from `pool` by
/// doubling, rounded up to [`GROW_ALIGN`], capped at `ceiling`; `None` when `pool` already
/// holds it or cannot grow.
pub fn grow_target(pool: u32, need: u32, ceiling: u32) -> Option<u32> {
    if need <= pool || pool >= ceiling {
        return None;
    }
    let need = need.min(ceiling);
    let mut target = pool.max(GROW_ALIGN);
    while target < need {
        target = target.saturating_mul(2);
    }
    let aligned = target.div_ceil(GROW_ALIGN).saturating_mul(GROW_ALIGN);
    Some(aligned.min(ceiling)).filter(|&target| target > pool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_config::{INITIAL_POOL_TOKENS, LlamaCppSettings};

    fn launch(context: Option<u32>, pool: Option<u32>) -> ModelLaunch {
        let mut launch = LlamaCppSettings {
            backend: "auto".into(),
            custom_path: None,
            find_existing: true,
            auto_install: false,
            update_external: "off".into(),
            idle_unload_s: 900,
            context: context.map_or(imperium_config::ContextLimit::Auto, Into::into),
            parallel: 2,
            slots: None,
            pool_tokens: pool,
            cache_type: "f16".into(),
            flash_attn: "auto".into(),
            max_output_tokens: 8192,
            models: Default::default(),
            ram_headroom_mb: 0,
            cache_ram_mb: 0,
            swap_cache_mb: 0,
            min_residency_s: 30,
            max_swaps_per_min: 6,
            swap_gain: 2.0,
            max_wait_s: 600,
            extra_args: Vec::new(),
        }
        .default_launch();
        launch.max_output_tokens = 8192;
        launch
    }

    #[test]
    fn the_ceiling_is_the_least_of_trained_configured_and_fit() {
        let auto = launch(None, None);
        // Trained for 256k, 128k fits: an agent may use 128k, and the pool starts small.
        let sized = size(&auto, Some(262_144), 131_072, 0, true);
        assert_eq!(
            (sized.context, sized.pool_tokens),
            (131_072, INITIAL_POOL_TOKENS)
        );
        // Everything fits: the trained context is the ceiling.
        assert_eq!(
            size(&auto, Some(262_144), u32::MAX, 0, true).context,
            262_144
        );
        // A configured limit below both wins.
        let fixed = launch(Some(65_536), None);
        assert_eq!(
            size(&fixed, Some(262_144), 131_072, 0, true).context,
            65_536
        );
        // A model that does not say what it was trained for gets a modest `auto` ceiling, and a
        // configured limit is taken as it is.
        assert_eq!(
            size(&auto, None, u32::MAX, 0, true).context,
            UNKNOWN_TRAINED_CONTEXT
        );
        assert_eq!(size(&fixed, None, u32::MAX, 0, true).context, 65_536);
        // When little fits, the pool and context are cut to what does, but never below a
        // working context (or the configured one, if smaller).
        let sized = size(&auto, Some(262_144), 2048, 0, true);
        assert_eq!(
            (sized.context, sized.pool_tokens),
            (MIN_WORKING_CONTEXT, MIN_WORKING_CONTEXT)
        );
        let small = launch(Some(4096), None);
        let sized = size(&small, Some(262_144), 2048, 0, true);
        assert_eq!((sized.context, sized.pool_tokens), (4096, 4096));
        let sized = size(&small, Some(262_144), u32::MAX, 0, true);
        assert_eq!((sized.context, sized.pool_tokens), (4096, 8192));
    }

    #[test]
    fn a_fixed_pool_that_does_not_fit_is_cut_rather_than_moving_layers_off_the_gpu() {
        // 8 slots x 40960 tokens pinned, about 129k fit beside the weights.
        let pinned = launch(Some(40_960), Some(327_680));
        let sized = size(&pinned, Some(262_144), 129_024, 0, true);
        assert_eq!(
            (sized.context, sized.pool_tokens, sized.pool_clamped),
            (40_960, 129_024, true)
        );
        // Never below a working context, and untouched when it fits.
        let sized = size(&pinned, Some(262_144), 1024, 0, true);
        assert_eq!(sized.pool_tokens, MIN_WORKING_CONTEXT);
        let sized = size(&pinned, Some(262_144), u32::MAX, 0, true);
        assert_eq!((sized.pool_tokens, sized.pool_clamped), (327_680, false));
    }

    #[test]
    fn the_pool_grows_to_what_was_asked_up_to_the_ceiling() {
        let auto = launch(None, None);
        let sized = size(&auto, Some(262_144), 131_072, 65_536, true);
        assert_eq!(sized.pool_tokens, 65_536);
        let sized = size(&auto, Some(262_144), 131_072, 1 << 20, true);
        assert_eq!(sized.pool_tokens, 131_072, "capped at the ceiling");
        // A fixed pool never grows, and caps the ceiling.
        let fixed = launch(None, Some(32_768));
        let sized = size(&fixed, Some(262_144), 131_072, 65_536, true);
        assert_eq!((sized.context, sized.pool_tokens), (32_768, 32_768));
        // vLLM's pool is its context.
        let sized = size(&auto, Some(262_144), 131_072, 0, false);
        assert_eq!((sized.context, sized.pool_tokens), (131_072, 131_072));
    }

    #[test]
    fn growth_doubles_rounds_and_stops_at_the_ceiling() {
        assert_eq!(grow_target(16_384, 16_000, 131_072), None, "it fits");
        assert_eq!(grow_target(16_384, 20_000, 131_072), Some(32_768));
        assert_eq!(grow_target(16_384, 70_000, 131_072), Some(131_072));
        assert_eq!(grow_target(16_384, 40_000, 100_000), Some(65_536));
        assert_eq!(grow_target(16_384, 90_000, 100_000), Some(100_000));
        assert_eq!(
            grow_target(100_000, 120_000, 100_000),
            None,
            "at the ceiling"
        );
        // A pool not on the grid grows onto it.
        assert_eq!(grow_target(5000, 6000, 131_072), Some(10_240));
    }

    #[test]
    fn a_call_needs_its_prompt_and_its_reply() {
        assert_eq!(demand(Some(1000), 512), Some(1512));
        assert_eq!(demand(Some(1000), 0), Some(1000 + UNCAPPED_REPLY_TOKENS));
        assert_eq!(demand(None, 512), None);
    }

    #[test]
    fn the_fit_search_finds_the_largest_pool_that_fits() {
        let found = largest_fitting(1024, 1 << 20, |t| t <= 300_000);
        assert!(found <= 300_000 && 300_000 - found <= GROW_ALIGN, "{found}");
        assert_eq!(largest_fitting(1024, 65_536, |_| true), 65_536);
        assert_eq!(largest_fitting(1024, 65_536, |_| false), 1024);
        let auto = launch(None, None);
        // Every slot (16 by default) at the full context, at most.
        assert_eq!(search_limit(&auto, Some(262_144)), 16 * 262_144);
        assert_eq!(search_limit(&auto, None), 16 * UNKNOWN_TRAINED_CONTEXT);
        assert_eq!(search_limit(&launch(Some(8192), None), None), 16 * 8192);
    }
}
