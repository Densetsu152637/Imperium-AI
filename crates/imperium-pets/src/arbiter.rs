//! The Pixel Pets arbiter: decides which animation the pet shows, from events reported by
//! sources (sessions, subagents, schedules). Time is passed in as milliseconds, so the rules
//! are deterministic and testable.
//!
//! 1. Loops are states, one-shots are reactions; each source holds at most one loop, and the
//!    highest-priority loop is shown (the most recent on a tie).
//! 2. A one-shot interrupts only a loop of equal or lower priority; otherwise it is dropped.
//! 3. One-shots within 1500 ms of each other coalesce: the higher-priority one plays.
//! 4. An animation stays at least its `minMs` before one of the same or lower priority
//!    replaces it; P5 transitions finish unless a P0 or P1 event arrives.
//! 5. While sleeping, anything above P6 plays `waking` first.
//! 6. After `duck-in` the pet is hidden until `pop-out`.
//! 7. `alert` replaces `waiting` when a source other than the shown one needs the person while
//!    the shown source is busy.

use std::collections::HashMap;

use crate::package::{Animation, Pet};

const COALESCE_MS: u64 = 1500;
const FIDGET_EVERY_MS: u64 = 40_000;

/// What a source reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PetEvent {
    /// The source is now in a state, such as `working` or `waiting`.
    Loop(String),
    /// The source's state ended.
    Clear,
    /// A reaction, such as `jumping` or `failed`.
    Once(String),
}

/// What the pet shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    /// The animation drawn (after fallbacks).
    pub animation: Animation,
    /// When it started, in the caller's milliseconds.
    pub since: u64,
    pub source: Option<String>,
    pub hidden: bool,
}

#[derive(Debug, Clone)]
struct Active {
    name: String,
    priority: u8,
    started: u64,
}

#[derive(Debug)]
pub struct Arbiter {
    pet: Pet,
    loops: HashMap<String, Active>,
    /// The current one-shot, its source, and when it ends.
    once: Option<(Animation, Option<String>, u64, u64)>,
    /// A reaction waiting for `waking` to finish.
    after_waking: Option<(String, Option<String>)>,
    shown: Shown,
    last_activity: u64,
    last_fidget: u64,
    sleep_after_ms: u64,
    fidget: bool,
    hidden: bool,
}

impl Arbiter {
    pub fn new(pet: Pet, now: u64, sleep_after_ms: u64, fidget: bool) -> Self {
        let idle = pet.resolve("idle").clone();
        Arbiter {
            shown: Shown {
                animation: idle,
                since: now,
                source: None,
                hidden: false,
            },
            pet,
            loops: HashMap::new(),
            once: None,
            after_waking: None,
            last_activity: now,
            last_fidget: now,
            sleep_after_ms,
            fidget,
            hidden: false,
        }
    }

    pub fn pet(&self) -> &Pet {
        &self.pet
    }

    pub fn shown(&self) -> &Shown {
        &self.shown
    }

    fn standard(&self, name: &str) -> Animation {
        self.pet
            .animations
            .get(name)
            .cloned()
            .unwrap_or_else(|| self.pet.animations["idle"].clone())
    }

    /// Applies an event; returns the new shown state when it changed.
    pub fn handle(&mut self, source: &str, event: PetEvent, now: u64) -> Option<Shown> {
        let priority = match &event {
            PetEvent::Loop(name) | PetEvent::Once(name) => self.standard(name).priority,
            PetEvent::Clear => 6,
        };
        if priority < 6 {
            self.last_activity = now;
        }
        let asleep = self.shown.animation.name == self.pet.resolve("sleeping").name
            && self.once.is_none()
            && self.loops.is_empty();
        match event {
            PetEvent::Loop(name) => {
                let priority = self.standard(&name).priority;
                self.loops.insert(
                    source.to_string(),
                    Active {
                        name,
                        priority,
                        started: now,
                    },
                );
            }
            PetEvent::Clear => {
                self.loops.remove(source);
            }
            PetEvent::Once(name) => {
                if asleep && priority < 6 && name != "waking" {
                    self.after_waking = Some((name, Some(source.to_string())));
                    self.start_once("waking", None, now);
                } else {
                    self.offer_once(&name, Some(source.to_string()), now);
                }
                return self.refresh(now);
            }
        }
        if asleep && priority < 6 {
            self.start_once("waking", None, now);
        }
        self.refresh(now)
    }

    /// Advances time: ends one-shots, falls asleep, fidgets. Returns the new state if it changed.
    pub fn tick(&mut self, now: u64) -> Option<Shown> {
        let finished = self
            .once
            .as_ref()
            .is_some_and(|(_, _, _, until)| now >= *until);
        if finished && let Some((animation, ..)) = self.once.take() {
            if animation.name == "duck-in" {
                self.hidden = true;
            }
            if let Some(next) = animation.next.clone() {
                self.start_once(&next, None, now);
            } else if let Some((name, source)) = self.after_waking.take() {
                self.start_once(&name, source, now);
            }
        }
        let idle = self.loops.is_empty() && self.once.is_none();
        if idle && now.saturating_sub(self.last_activity) >= self.sleep_after_ms {
            if self.shown.animation.name != self.pet.resolve("sleeping").name {
                return self.set_at(self.pet.resolve("sleeping").clone(), None, now);
            }
            return None;
        }
        if idle && self.fidget && now.saturating_sub(self.last_fidget) >= FIDGET_EVERY_MS {
            self.last_fidget = now;
            self.start_once("fidget", None, now);
        }
        self.refresh(now)
    }

    /// Plays a one-shot if the rules allow it.
    fn offer_once(&mut self, name: &str, source: Option<String>, now: u64) {
        let candidate = self.standard(name);
        if let Some((current, _, started, _)) = &self.once {
            // Coalesce nearby reactions; the more important one plays.
            if now.saturating_sub(*started) < COALESCE_MS {
                if candidate.priority < current.priority {
                    self.start_once(name, source, now);
                }
                return;
            }
            // A transition finishes unless something urgent arrives.
            if current.priority == 5 && candidate.priority > 1 {
                return;
            }
        }
        if let Some(winner) = self.winning_loop()
            && candidate.priority > winner.1.priority
        {
            return;
        }
        if name == "pop-out" {
            self.hidden = false;
        }
        self.start_once(name, source, now);
    }

    fn start_once(&mut self, name: &str, source: Option<String>, now: u64) {
        let animation = self.pet.resolve(name).clone();
        let length: u64 = animation.durations.iter().map(|ms| u64::from(*ms)).sum();
        // Keep the requested name's behaviour (priority, next) with the drawn frames.
        let wanted = self.standard(name);
        let animation = Animation {
            priority: wanted.priority,
            next: wanted.next,
            name: if name == "duck-in" || name == "pop-out" {
                name.to_string()
            } else {
                animation.name
            },
            ..animation
        };
        self.once = Some((animation, source, now, now + length.max(1)));
    }

    fn winning_loop(&self) -> Option<(&String, &Active)> {
        self.loops.iter().min_by(|a, b| {
            a.1.priority
                .cmp(&b.1.priority)
                .then(b.1.started.cmp(&a.1.started))
        })
    }

    /// Recomputes what to show from the one-shot and the loops.
    fn refresh(&mut self, now: u64) -> Option<Shown> {
        if let Some((animation, source, started, _)) = self.once.clone() {
            if self.shown.animation.name == animation.name && self.shown.since == started {
                return None;
            }
            return self.set_at(animation, source, started);
        }
        let (animation, source) = match self.winning_loop() {
            Some((source, active)) => {
                let mut name = active.name.clone();
                // Someone else needs the person while the shown source is busy: alert.
                if active.priority == 1
                    && self
                        .shown
                        .source
                        .as_deref()
                        .is_some_and(|shown| shown != source)
                    && self
                        .shown
                        .source
                        .as_ref()
                        .and_then(|shown| self.loops.get(shown))
                        .is_some_and(|other| other.priority >= 2)
                {
                    name = "alert".into();
                }
                (self.pet.resolve(&name).clone(), Some(source.clone()))
            }
            None => (self.pet.resolve("idle").clone(), None),
        };
        if self.shown.animation.name == animation.name && self.shown.source == source {
            return None;
        }
        // No flicker: a same-or-lower-priority change waits out the current minMs.
        let held = now.saturating_sub(self.shown.since) < u64::from(self.shown.animation.min_ms);
        if held
            && animation.priority >= self.shown.animation.priority
            && self.shown.animation.looping
        {
            return None;
        }
        self.set_at(animation, source, now)
    }

    fn set_at(
        &mut self,
        animation: Animation,
        source: Option<String>,
        since: u64,
    ) -> Option<Shown> {
        self.shown = Shown {
            animation,
            since,
            source,
            hidden: self.hidden,
        };
        Some(self.shown.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::tests::bundled;

    fn arbiter() -> Arbiter {
        Arbiter::new(
            Pet::load(&bundled().canonicalize().unwrap()).unwrap(),
            0,
            300_000,
            false,
        )
    }

    fn name(arbiter: &Arbiter) -> &str {
        &arbiter.shown().animation.name
    }

    #[test]
    fn loops_follow_priority_and_reactions_play_over_them() {
        let mut pet = arbiter();
        pet.handle("s1", PetEvent::Loop("working".into()), 1_000);
        assert_eq!(name(&pet), "working");
        // A P1 loop from another source wins; with s1 busy it is an alert.
        pet.handle("s2", PetEvent::Loop("waiting".into()), 2_000);
        assert_eq!(name(&pet), "alert");
        pet.handle("s2", PetEvent::Clear, 3_000);
        assert_eq!(name(&pet), "working");
        // jumping (P3) interrupts working (P4), then working resumes.
        pet.handle("s1", PetEvent::Once("jumping".into()), 4_000);
        assert_eq!(name(&pet), "jumping");
        pet.tick(10_000);
        assert_eq!(name(&pet), "working");
        // fidget (P6) cannot interrupt working (P4).
        pet.handle("s1", PetEvent::Once("fidget".into()), 12_000);
        assert_eq!(name(&pet), "working");
    }

    #[test]
    fn nearby_reactions_coalesce_to_the_most_important() {
        let mut pet = arbiter();
        pet.handle("t1", PetEvent::Once("jumping".into()), 1_000);
        pet.handle("t2", PetEvent::Once("jumping".into()), 1_200);
        pet.handle("t3", PetEvent::Once("failed".into()), 1_400);
        assert_eq!(name(&pet), "failed");
        assert_eq!(pet.shown().since, 1_400);
        pet.handle("t4", PetEvent::Once("jumping".into()), 1_600);
        assert_eq!(name(&pet), "failed");
    }

    #[test]
    fn idle_pets_sleep_and_wake_before_reacting() {
        let mut pet = arbiter();
        pet.tick(299_000);
        assert_eq!(name(&pet), "idle");
        pet.tick(300_000);
        assert_eq!(name(&pet), "sleeping");
        pet.handle("s1", PetEvent::Once("waving".into()), 301_000);
        assert_eq!(name(&pet), "waking");
        pet.tick(310_000);
        assert_eq!(name(&pet), "waving");
        pet.tick(320_000);
        assert_eq!(name(&pet), "idle");
    }

    #[test]
    fn duck_in_hides_until_pop_out() {
        let mut pet = arbiter();
        pet.handle("app", PetEvent::Once("duck-in".into()), 1_000);
        pet.tick(5_000);
        assert!(pet.shown().hidden || pet.tick(5_001).is_some_and(|shown| shown.hidden));
        pet.handle("app", PetEvent::Once("pop-out".into()), 6_000);
        assert!(!pet.shown().hidden);
    }
}
