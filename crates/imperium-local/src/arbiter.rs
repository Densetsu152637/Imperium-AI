//! How the supervisor makes room for a local model: which loaded models to swap out, or whether
//! to wait. Pure functions over a snapshot of the loaded models, so the policy is tested without
//! servers.
//!
//! - A model with a slot in use, or a call queued for one, is never swapped out ([`Use::Busy`]).
//! - The GPU rule: no call runs (partly) on the CPU while nothing generates. When the newcomer
//!   would run partly on the CPU beside the loaded models and no model has a call running
//!   ([`Resident::generating`]), every model not generating (and not protected for the user)
//!   gives way, mid-turn agents included and past `min_residency` and the swaps of the
//!   minute. While another agent generates, a call may run beside it on the CPU.
//! - A model whose agents are all waiting for subagents, or done, is swapped out when the
//!   newcomer does not fit beside it, would get more of its layers on the GPU once the model is
//!   gone, or `swap_gain` times the GPU memory ([`Use::Free`]). Otherwise the newcomer loads
//!   beside it.
//! - A model with an agent mid-turn on it ([`Use::Claimed`]) is expected back soon: the newcomer
//!   waits for that agent to finish or start waiting, up to `min_residency`. After that it loads
//!   beside the model if it can, and swaps it out only when nothing else fits and the model has
//!   been loaded at least `min_residency`.
//! - A model pinned to the CPU lane holds RAM only, so evicting it never gives a newcomer more
//!   of the GPU: it is not a GPU swap victim. It is a candidate last, only when a newcomer
//!   cannot be placed unless the RAM it holds is freed, and then under the same rules as any
//!   model ([`Resident::cpu_pinned`]).
//! - When the swaps allowed per minute are used up, a newcomer that does not fit beside the
//!   loaded models waits for the window to move on; when swapping is off, it is refused.

use std::time::Duration;

use crate::planner::{Budget, Placement};

/// What a loaded model is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Use {
    /// A slot is in use or a call is queued for one.
    Busy,
    /// No slot is in use, but an agent of the requester's own session that took one this time
    /// it was loaded is mid-turn. (An agent of another session counts as `Busy`.)
    Claimed,
    /// No slot is in use and no agent is mid-turn on it.
    Free,
}

/// A loaded model as the arbiter sees it.
#[derive(Debug, Clone)]
pub(crate) struct Resident {
    pub id: String,
    /// Memory it holds, its prompt cache included.
    pub vram_bytes: u64,
    pub ram_bytes: u64,
    /// The part of `ram_bytes` that is its prompt cache.
    pub cache_bytes: u64,
    pub use_: Use,
    /// A call is running on it now (a slot is in use): an agent is generating.
    pub generating: bool,
    /// The user used it lately and the newcomer is background work: it never gives way.
    pub protected: bool,
    /// Pinned to the CPU lane: it holds RAM only, and is evicted after every other candidate.
    pub cpu_pinned: bool,
    /// Time since it was loaded.
    pub resident_for: Duration,
    /// Time since a slot on it was last released.
    pub idle_for: Duration,
}

/// Whether models may be swapped out now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Swaps {
    Allowed,
    /// The swaps allowed this minute are used up.
    Spent,
    /// `max_swaps_per_min` is 0.
    Off,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Policy {
    pub min_residency: Duration,
    pub swaps: Swaps,
    /// How many times the GPU memory a newcomer gets beside the loaded models it must get once
    /// free models are gone for them to be swapped out (`swap_gain`).
    pub gain: f64,
}

impl Policy {
    fn may_swap(&self) -> bool {
        self.swaps == Swaps::Allowed
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Decision {
    /// Unload `evict` (in order), then load the model at `placement`.
    Load {
        evict: Vec<String>,
        placement: Placement,
    },
    /// Wait for a loaded model to finish, settle, or age.
    Wait,
    /// The model does not fit even on an empty machine.
    TooBig,
    /// It does not fit beside the loaded models and swapping is off.
    SwapsOff,
}

/// What loading a model would take, for routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outlook {
    /// It fits beside the loaded models well enough not to swap them out.
    Fits,
    /// It needs models nobody has a slot on swapped out.
    Swap,
    /// Models in use are in the way, or swapping is spent or off and it does not fit beside
    /// them.
    Unavailable,
}

/// How to make room for a model that `place` places within a budget, `waited` after it was
/// first asked for.
pub(crate) fn decide(
    place: impl Fn(Budget) -> Option<Placement>,
    budget: Budget,
    residents: &[Resident],
    waited: Duration,
    policy: Policy,
) -> Decision {
    if place(budget).is_none() {
        return Decision::TooBig;
    }
    let beside = place(left_after(budget, residents, &[]));
    let free = candidates(residents, |r| r.use_ == Use::Free);
    let best_now = place(left_after(budget, residents, &free));
    if policy.may_swap() && worth_swapping(&best_now, &beside, policy.gain) {
        return evict_fewest(&place, budget, residents, &free, &best_now);
    }
    // Nothing generates anywhere: no call may run on the CPU while the GPU is idle. When the
    // newcomer would run (partly) on the CPU beside the loaded models, everything not
    // generating gives way (agents mid-turn between calls included; their slots are saved),
    // past `min_residency` and the swaps of this minute, so it gets the most GPU layers it can.
    // A newcomer that does not fit beside them at all follows the usual rules below.
    let generating = residents.iter().any(|r| r.generating);
    let yieldable = candidates(residents, |r| !r.generating && !r.protected);
    if !generating && policy.swaps != Swaps::Off && beside.is_some() {
        let most = place(left_after(budget, residents, &yieldable));
        if better(&most, &beside) {
            return evict_fewest(&place, budget, residents, &yieldable, &most);
        }
    }
    let aged = waited >= policy.min_residency;
    let idle = candidates(residents, |r| r.use_ != Use::Busy);
    let settled = place(left_after(budget, residents, &idle));
    // While another agent generates, a call may run beside it on the CPU rather than wait.
    if policy.may_swap()
        && !aged
        && worth_swapping(&settled, &beside, policy.gain)
        && (beside.is_none() || !generating)
    {
        // An agent mid-turn, such as an orchestrator about to wait for this subagent, holds a
        // model that is in the way; it is usually moments from letting go.
        return Decision::Wait;
    }
    if let Some(placement) = beside {
        return Decision::Load {
            evict: Vec::new(),
            placement,
        };
    }
    match policy.swaps {
        Swaps::Off => return Decision::SwapsOff,
        // Once the window moves on; bounded by `max_wait_s`.
        Swaps::Spent => return Decision::Wait,
        Swaps::Allowed if !aged => return Decision::Wait,
        Swaps::Allowed => {}
    }
    let movable = candidates(residents, |r| {
        r.use_ == Use::Free || (r.use_ == Use::Claimed && r.resident_for >= policy.min_residency)
    });
    let best = place(left_after(budget, residents, &movable));
    if best.is_none() {
        return Decision::Wait;
    }
    evict_fewest(&place, budget, residents, &movable, &best)
}

/// Whether a model fits beside the loaded ones, needs a swap, or cannot run now.
pub(crate) fn outlook(
    place: impl Fn(Budget) -> Option<Placement>,
    budget: Budget,
    residents: &[Resident],
    policy: Policy,
) -> Outlook {
    let beside = place(left_after(budget, residents, &[]));
    let idle = candidates(residents, |r| r.use_ != Use::Busy);
    let best = place(left_after(budget, residents, &idle));
    if best.is_none() {
        Outlook::Unavailable
    } else if !worth_swapping(&best, &beside, policy.gain) {
        Outlook::Fits
    } else if policy.may_swap() {
        Outlook::Swap
    } else if beside.is_some() {
        Outlook::Fits
    } else {
        Outlook::Unavailable
    }
}

/// The budget a new model's prompt cache is sized from: what is left after the models that stay,
/// less room for the models it displaced that would fit back beside it without their own prompt
/// caches. A model swapped out only because another's cache took the RAM then comes back beside
/// it with a smaller cache, rather than swapping the newcomer out in turn.
pub(crate) fn cache_room(
    budget: Budget,
    residents: &[Resident],
    evicted: &[String],
    placement: &Placement,
) -> Budget {
    let gone: Vec<&Resident> = residents
        .iter()
        .filter(|r| evicted.contains(&r.id))
        .collect();
    let mut room = left_after(budget, residents, &gone);
    let mut spare_vram = room.vram_bytes.saturating_sub(placement.vram_bytes);
    let mut spare_ram = room.ram_bytes.saturating_sub(placement.ram_bytes);
    for resident in gone {
        let ram = resident.ram_bytes.saturating_sub(resident.cache_bytes);
        if resident.vram_bytes <= spare_vram && ram <= spare_ram {
            spare_vram -= resident.vram_bytes;
            spare_ram -= ram;
            room.ram_bytes -= ram;
        }
    }
    room
}

/// The budget left once every resident not generating (and not protected) has given way.
pub(crate) fn room_after_yield(budget: Budget, residents: &[Resident]) -> Budget {
    let yieldable = candidates(residents, |r| !r.generating && !r.protected);
    left_after(budget, residents, &yieldable)
}

/// Residents matching `keep`, the longest idle first.
fn candidates(residents: &[Resident], keep: impl Fn(&Resident) -> bool) -> Vec<&Resident> {
    let mut chosen: Vec<&Resident> = residents.iter().filter(|r| keep(r)).collect();
    // CPU-lane models last, since they free no GPU memory; then free models before claimed
    // ones, then the least recently used first.
    chosen.sort_by_key(|r| {
        (
            r.cpu_pinned,
            r.use_ != Use::Free,
            std::cmp::Reverse(r.idle_for),
        )
    });
    chosen
}

/// The budget less what the residents other than `evicted` hold.
fn left_after(budget: Budget, residents: &[Resident], evicted: &[&Resident]) -> Budget {
    let (vram, ram) = residents
        .iter()
        .filter(|r| !evicted.iter().any(|gone| gone.id == r.id))
        .fold((0, 0), |(vram, ram), r| {
            (vram + r.vram_bytes, ram + r.ram_bytes)
        });
    Budget {
        vram_bytes: budget.vram_bytes.saturating_sub(vram),
        ram_bytes: budget.ram_bytes.saturating_sub(ram),
        unified: budget.unified,
    }
}

/// Unloads the fewest `candidates`, in order, that still reach `target`'s placement.
fn evict_fewest(
    place: &impl Fn(Budget) -> Option<Placement>,
    budget: Budget,
    residents: &[Resident],
    candidates: &[&Resident],
    target: &Option<Placement>,
) -> Decision {
    for count in 1..=candidates.len() {
        let evicted = &candidates[..count];
        let placement = place(left_after(budget, residents, evicted));
        if let Some(placement) = placement
            && !better(target, &Some(placement))
        {
            return Decision::Load {
                evict: evicted.iter().map(|r| r.id.clone()).collect(),
                placement,
            };
        }
    }
    Decision::Wait
}

/// Whether swapping models out to reach `gain` is worth it over `base`: the model does not fit
/// otherwise, gets more of its layers on the GPU (a newcomer never runs on the CPU for want of
/// what a model nobody uses holds), or gets more than `factor` times the GPU memory (with 1,
/// any more at all).
fn worth_swapping(gain: &Option<Placement>, base: &Option<Placement>, factor: f64) -> bool {
    match (gain, base) {
        (Some(_), None) => true,
        (Some(gain), Some(base)) if gain.gpu_layers > base.gpu_layers => true,
        (Some(gain), Some(base)) => (base.vram_bytes as f64) * factor < gain.vram_bytes as f64,
        (None, _) => false,
    }
}

/// Whether `a` puts more of the model on the GPU than `b` (any placement beats none).
fn better(a: &Option<Placement>, b: &Option<Placement>) -> bool {
    let layers = |p: &Option<Placement>| p.map_or(-1, |p| i64::from(p.gpu_layers));
    layers(a) > layers(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::{Footprint, place};

    const GIB: u64 = 1024 * 1024 * 1024;

    /// A 6 GiB model with 32 layers on a 10 GiB GPU and 16 GiB of RAM.
    fn budget() -> Budget {
        Budget {
            vram_bytes: 10 * GIB,
            ram_bytes: 16 * GIB,
            unified: false,
        }
    }

    fn six_gib(budget: Budget) -> Option<Placement> {
        place(
            Footprint {
                weights_bytes: 5 * GIB,
                kv_bytes: GIB / 2,
                layers: Some(32),
                max_gpu_layers: None,
            },
            budget,
        )
    }

    fn resident(id: &str, use_: Use, vram_gib: u64) -> Resident {
        Resident {
            id: id.into(),
            vram_bytes: vram_gib * GIB,
            ram_bytes: 0,
            cache_bytes: 0,
            use_,
            // A busy model has a call running unless a test says otherwise.
            generating: use_ == Use::Busy,
            protected: false,
            cpu_pinned: false,
            resident_for: Duration::from_secs(60),
            idle_for: Duration::from_secs(5),
        }
    }

    fn policy() -> Policy {
        Policy {
            min_residency: Duration::from_secs(30),
            swaps: Swaps::Allowed,
            gain: 2.0,
        }
    }

    fn evicted(decision: &Decision) -> Option<Vec<String>> {
        match decision {
            Decision::Load { evict, .. } => Some(evict.clone()),
            _ => None,
        }
    }

    #[test]
    fn a_model_nobody_is_working_on_is_swapped_out_for_a_full_gpu_placement() {
        // Beside an 8 GiB model the newcomer would mostly run from RAM.
        let residents = [resident("orchestrator", Use::Free, 8)];
        let decision = decide(six_gib, budget(), &residents, Duration::ZERO, policy());
        assert_eq!(evicted(&decision), Some(vec!["orchestrator".into()]));
        let Decision::Load { placement, .. } = decision else {
            unreachable!()
        };
        assert!(placement.all_layers_on_gpu);
    }

    #[test]
    fn only_as_many_models_as_needed_are_swapped_out_least_recently_used_first() {
        // Beside both, the newcomer gets under 2 GiB of the GPU; without one, all of it.
        let mut older = resident("older", Use::Free, 4);
        older.idle_for = Duration::from_secs(50);
        let residents = [resident("newer", Use::Free, 4), older];
        let decision = decide(six_gib, budget(), &residents, Duration::ZERO, policy());
        assert_eq!(evicted(&decision), Some(vec!["older".into()]));
    }

    #[test]
    fn a_model_nobody_is_working_on_stays_only_while_the_newcomer_fits_beside_it_on_the_gpu() {
        // Beside a 3 GiB model the newcomer has every layer on the GPU: nothing moves.
        let residents = [resident("small", Use::Free, 3)];
        let decision = decide(six_gib, budget(), &residents, Duration::ZERO, policy());
        assert_eq!(evicted(&decision), Some(Vec::new()));
        assert_eq!(
            outlook(six_gib, budget(), &residents, policy()),
            Outlook::Fits
        );
        // Beside a 5 GiB one some of its layers would run on the CPU: the idle model goes.
        let residents = [resident("small", Use::Free, 5)];
        let decision = decide(six_gib, budget(), &residents, Duration::ZERO, policy());
        assert_eq!(evicted(&decision), Some(vec!["small".into()]));
    }

    #[test]
    fn a_model_in_use_is_never_swapped_out() {
        let no_ram = Budget {
            ram_bytes: 0,
            ..budget()
        };
        let residents = [resident("busy", Use::Busy, 10)];
        let long = Duration::from_secs(3600);
        assert_eq!(
            decide(six_gib, no_ram, &residents, long, policy()),
            Decision::Wait
        );
        assert_eq!(
            outlook(six_gib, no_ram, &residents, policy()),
            Outlook::Unavailable
        );
    }

    /// A 1 GiB model with a call running on it.
    fn generator() -> Resident {
        resident("generator", Use::Busy, 1)
    }

    #[test]
    fn with_the_gpu_idle_nothing_runs_on_the_cpu_and_a_waiting_root_gives_way() {
        // The orchestrator is mid-turn between calls (or waiting on this very subagent) and
        // nothing generates: the subagent does not run partly on the CPU, it swaps the
        // orchestrator out at once, whatever its residency.
        let mut orchestrator = resident("orchestrator", Use::Claimed, 8);
        orchestrator.resident_for = Duration::ZERO;
        let decision = decide(
            six_gib,
            budget(),
            &[orchestrator.clone()],
            Duration::ZERO,
            policy(),
        );
        assert_eq!(evicted(&decision), Some(vec!["orchestrator".into()]));
        let Decision::Load { placement, .. } = decision else {
            unreachable!()
        };
        assert!(placement.all_layers_on_gpu);
        // Even when the swaps of this minute are spent.
        let spent = Policy {
            swaps: Swaps::Spent,
            ..policy()
        };
        let decision = decide(six_gib, budget(), &[orchestrator], Duration::ZERO, spent);
        assert_eq!(evicted(&decision), Some(vec!["orchestrator".into()]));
    }

    #[test]
    fn while_another_agent_generates_a_call_may_run_on_the_cpu_beside_it() {
        let residents = [resident("orchestrator", Use::Claimed, 8), generator()];
        // With the GPU busy, the newcomer loads beside both, partly in RAM, rather than wait.
        let decision = decide(six_gib, budget(), &residents, Duration::ZERO, policy());
        assert_eq!(evicted(&decision), Some(Vec::new()));
        let Decision::Load { placement, .. } = decision else {
            unreachable!()
        };
        assert!(!placement.all_layers_on_gpu);
        assert_eq!(
            outlook(six_gib, budget(), &residents, policy()),
            Outlook::Swap
        );
    }

    #[test]
    fn a_model_mid_turn_is_swapped_out_only_when_nothing_else_fits_and_after_its_residency() {
        // Another agent generates, so the GPU rule does not apply and the usual one does.
        let no_ram = Budget {
            ram_bytes: 0,
            ..budget()
        };
        let mut claimed = resident("other", Use::Claimed, 8);
        claimed.resident_for = Duration::from_secs(10);
        let aged = Duration::from_secs(40);
        assert_eq!(
            decide(
                six_gib,
                no_ram,
                &[claimed.clone(), generator()],
                aged,
                policy()
            ),
            Decision::Wait,
            "loaded too recently"
        );
        claimed.resident_for = Duration::from_secs(30);
        let decision = decide(six_gib, no_ram, &[claimed, generator()], aged, policy());
        assert_eq!(evicted(&decision), Some(vec!["other".into()]));
    }

    #[test]
    fn a_model_too_big_for_an_empty_gpu_still_offloads_part_of_its_layers() {
        // 14 GiB of weights on a 10 GiB GPU: partial offload is the best there is, so it is
        // allowed even with the GPU idle; an idle model in the way still gives way.
        let big = |budget: Budget| {
            place(
                Footprint {
                    weights_bytes: 14 * GIB,
                    kv_bytes: GIB / 2,
                    layers: Some(32),
                    max_gpu_layers: None,
                },
                budget,
            )
        };
        let roomy = Budget {
            ram_bytes: 32 * GIB,
            ..budget()
        };
        let decision = decide(big, roomy, &[], Duration::ZERO, policy());
        let Decision::Load { placement, .. } = decision else {
            panic!("{decision:?}")
        };
        assert!(!placement.all_layers_on_gpu && placement.gpu_layers > 0);
        let decision = decide(
            big,
            roomy,
            &[resident("idle", Use::Free, 4)],
            Duration::ZERO,
            policy(),
        );
        assert_eq!(evicted(&decision), Some(vec!["idle".into()]));
    }

    #[test]
    fn when_swaps_are_spent_the_newcomer_loads_beside_or_waits_and_when_off_is_refused() {
        let paused = Policy {
            swaps: Swaps::Spent,
            ..policy()
        };
        // Another agent generates, so the newcomer may run beside the idle model on the CPU.
        let residents = [resident("orchestrator", Use::Free, 8), generator()];
        assert_eq!(
            evicted(&decide(
                six_gib,
                budget(),
                &residents,
                Duration::ZERO,
                paused
            )),
            Some(Vec::new())
        );
        assert_eq!(
            outlook(six_gib, budget(), &residents, paused),
            Outlook::Fits
        );
        let no_ram = Budget {
            ram_bytes: 0,
            ..budget()
        };
        assert_eq!(
            decide(six_gib, no_ram, &residents, Duration::ZERO, paused),
            Decision::Wait
        );
        let off = Policy {
            swaps: Swaps::Off,
            ..policy()
        };
        assert_eq!(
            decide(six_gib, no_ram, &residents, Duration::ZERO, off),
            Decision::SwapsOff
        );
        assert_eq!(
            outlook(six_gib, no_ram, &residents, paused),
            Outlook::Unavailable
        );
    }

    /// A CPU-lane model holding `ram_gib` of RAM and no VRAM.
    fn lane(use_: Use, ram_gib: u64) -> Resident {
        Resident {
            id: "lane".into(),
            ram_bytes: ram_gib * GIB,
            cpu_pinned: true,
            ..resident("lane", use_, 0)
        }
    }

    #[test]
    fn cpu_lane_models_are_not_gpu_swap_victims() {
        // 10 GiB of VRAM and 20 GiB of RAM; the lane holds 12 GiB of RAM and an idle GPU model
        // holds 8 GiB of VRAM. The newcomer gets the whole GPU once the GPU model is gone, which
        // the lane's RAM has no part in, so only the GPU model is swapped out, though the lane
        // was idle longer.
        let roomy = Budget {
            ram_bytes: 20 * GIB,
            ..budget()
        };
        let mut idle_lane = lane(Use::Free, 12);
        idle_lane.idle_for = Duration::from_secs(3600);
        let residents = [idle_lane, resident("gpu", Use::Free, 8)];
        let decision = decide(six_gib, roomy, &residents, Duration::ZERO, policy());
        assert_eq!(evicted(&decision), Some(vec!["gpu".into()]));
        let Decision::Load { placement, .. } = decision else {
            unreachable!()
        };
        assert!(placement.all_layers_on_gpu);
        assert_eq!(outlook(six_gib, roomy, &residents, policy()), Outlook::Swap);
        // Beside the lane alone, the newcomer fits the GPU: nothing is swapped.
        let alone = [lane(Use::Free, 12)];
        assert_eq!(
            evicted(&decide(six_gib, roomy, &alone, Duration::ZERO, policy())),
            Some(Vec::new())
        );
        assert_eq!(outlook(six_gib, roomy, &alone, policy()), Outlook::Fits);
    }

    #[test]
    fn cpu_lane_ram_is_given_up_only_when_the_newcomer_cannot_be_placed_without_it() {
        // 2 GiB of VRAM is free beside a busy GPU model, and the lane holds 14 of the 16 GiB of
        // RAM: the newcomer's CPU share does not fit until the idle lane gives its RAM up.
        let residents = [resident("busy", Use::Busy, 8), lane(Use::Free, 14)];
        let decision = decide(six_gib, budget(), &residents, Duration::ZERO, policy());
        assert_eq!(evicted(&decision), Some(vec!["lane".into()]));
        assert_eq!(
            outlook(six_gib, budget(), &residents, policy()),
            Outlook::Swap
        );
        // A lane in use is held like any model in use.
        let working = [resident("busy", Use::Busy, 8), lane(Use::Busy, 14)];
        assert_eq!(
            decide(
                six_gib,
                budget(),
                &working,
                Duration::from_secs(3600),
                policy()
            ),
            Decision::Wait
        );
        assert_eq!(
            outlook(six_gib, budget(), &working, policy()),
            Outlook::Unavailable
        );
    }

    #[test]
    fn cpu_lane_newcomers_need_ram_only_and_leave_gpu_models_alone() {
        use crate::planner::place_on_cpu;
        let cpu = |budget| {
            place_on_cpu(
                Footprint {
                    weights_bytes: 10 * GIB,
                    kv_bytes: GIB,
                    layers: Some(48),
                    max_gpu_layers: None,
                },
                budget,
            )
        };
        let ram = Budget {
            ram_bytes: 32 * GIB,
            ..budget()
        };
        // Idle GPU models hold VRAM and no RAM: the lane loads beside them, evicting nothing.
        let residents = [
            resident("gpu", Use::Free, 8),
            resident("gpu2", Use::Free, 2),
        ];
        let decision = decide(cpu, ram, &residents, Duration::ZERO, policy());
        let Decision::Load { evict, placement } = decision else {
            panic!("{decision:?}")
        };
        assert!(evict.is_empty());
        assert_eq!((placement.gpu_layers, placement.vram_bytes), (0, 0));
        assert_eq!(outlook(cpu, ram, &residents, policy()), Outlook::Fits);
        // A lane bigger than the RAM is refused outright.
        let tiny = Budget {
            ram_bytes: 8 * GIB,
            ..budget()
        };
        assert_eq!(
            decide(cpu, tiny, &[], Duration::ZERO, policy()),
            Decision::TooBig
        );
    }

    #[test]
    fn a_model_bigger_than_the_machine_is_refused() {
        let tiny = Budget {
            vram_bytes: GIB,
            ram_bytes: GIB,
            unified: false,
        };
        assert_eq!(
            decide(six_gib, tiny, &[], Duration::ZERO, policy()),
            Decision::TooBig
        );
    }

    #[test]
    fn the_prompt_cache_leaves_room_for_a_model_displaced_only_by_its_cache() {
        // RAM only: the resident's 4 GiB model plus its 8 GiB cache fill the 12 GiB.
        let ram = Budget {
            vram_bytes: 0,
            ram_bytes: 12 * GIB,
            unified: false,
        };
        let resident = Resident {
            ram_bytes: 12 * GIB,
            cache_bytes: 8 * GIB,
            vram_bytes: 0,
            ..resident("first", Use::Free, 0)
        };
        let placement = Placement {
            gpu_layers: 0,
            all_layers_on_gpu: false,
            vram_bytes: 0,
            ram_bytes: 4 * GIB,
            required_bytes: 4 * GIB,
        };
        let room = cache_room(ram, &[resident], &["first".into()], &placement);
        // 12 GiB less 4 GiB kept for the first model to come back beside the newcomer.
        assert_eq!(room.ram_bytes, 8 * GIB);
    }
}
