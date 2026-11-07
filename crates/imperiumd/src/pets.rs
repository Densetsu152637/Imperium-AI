//! The daemon's pet: one arbiter fed by every session's events, so every client shows the same
//! pet in the same state. State changes go to clients as `pet.state` notifications. Pets see
//! agent status only, never transcript content, and never call a model.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use imperium_pets::{Arbiter, Pet, PetEvent, Shown, discover};
use imperium_protocol::methods::{PET_STATE, PetInfo, PetStateInfo};
use imperium_protocol::session::{AgentEvent, SessionEvent};
use tokio::sync::{broadcast, mpsc};

use crate::DaemonState;

const TICK: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub struct PetEngine {
    events: mpsc::UnboundedSender<(String, PetEvent)>,
    pets: Vec<Pet>,
    selected: String,
    state: Mutex<Option<PetStateInfo>>,
}

impl PetEngine {
    /// Loads pets and starts the arbiter for the selected one; `None` when pets are off or no
    /// pet loads.
    pub fn start(daemon: &Arc<DaemonState>) -> Option<Arc<Self>> {
        let settings = &daemon.settings().pets;
        if !settings.enabled {
            return None;
        }
        let mut folders: Vec<PathBuf> = settings.folders.clone();
        folders.push(daemon.config_dir().join("pets"));
        // Pets made for the Claude Code plugin, and the pets bundled with Imperium.
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            folders.push(PathBuf::from(home).join(".claude").join("pets"));
        }
        if let Some(bundled) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("pets")))
        {
            folders.push(bundled);
        }
        let (pets, problems) = discover(&folders);
        for problem in problems {
            tracing::warn!(%problem, "pet skipped");
        }
        let pet = settings
            .pet
            .as_ref()
            .and_then(|id| pets.iter().find(|pet| &pet.id == id))
            .or_else(|| pets.first())?
            .clone();
        let (events, receiver) = mpsc::unbounded_channel();
        let engine = Arc::new(PetEngine {
            events,
            selected: pet.id.clone(),
            pets,
            state: Mutex::new(None),
        });
        let arbiter = Arbiter::new(
            pet,
            0,
            settings.sleep_after_minutes * 60_000,
            settings.fidget,
        );
        tokio::spawn(run(
            Arc::downgrade(&engine),
            Arc::downgrade(daemon),
            arbiter,
            receiver,
        ));
        let _ = engine
            .events
            .send(("daemon".into(), PetEvent::Once("waving".into())));
        Some(engine)
    }

    pub fn pets(&self) -> Vec<PetInfo> {
        self.pets
            .iter()
            .map(|pet| PetInfo {
                id: pet.id.clone(),
                display_name: pet.display_name.clone(),
                description: pet.description.clone(),
                author: pet.author.clone(),
                license: pet.license.clone(),
                cell_width: pet.cell_width,
                cell_height: pet.cell_height,
            })
            .collect()
    }

    pub fn selected(&self) -> &str {
        &self.selected
    }

    pub fn state(&self) -> Option<PetStateInfo> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The selected pet's strip PNG for an animation it draws.
    pub fn strip(&self, animation: &str) -> Option<Vec<u8>> {
        let pet = self.pets.iter().find(|pet| pet.id == self.selected)?;
        std::fs::read(pet.strip(animation)?).ok()
    }

    pub fn report(&self, source: String, event: PetEvent) {
        let _ = self.events.send((source, event));
    }

    /// Follows a session's events for the pet. Subagents are their own sources.
    pub fn watch(
        self: &Arc<Self>,
        session_id: String,
        mut events: broadcast::Receiver<AgentEvent>,
    ) {
        let engine = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                let Some(engine) = engine.upgrade() else {
                    return;
                };
                let source = format!("{session_id}/{}", event.agent_id);
                for pet_event in pet_events(&event.event) {
                    engine.report(source.clone(), pet_event);
                }
            }
        });
    }
}

/// How agent activity maps onto the pet (docs/16-pets.md#event-mapping).
pub fn pet_events(event: &SessionEvent) -> Vec<PetEvent> {
    let once = |name: &str| PetEvent::Once(name.into());
    let state = |name: &str| PetEvent::Loop(name.into());
    match event {
        SessionEvent::TurnStarted { .. } | SessionEvent::ToolStarted { .. } => {
            vec![state("working")]
        }
        SessionEvent::PermissionRequested { .. } => vec![state("waiting")],
        SessionEvent::ToolFinished {
            ok: false, output, ..
        } if output.contains("denied") => {
            vec![state("working"), once("confused")]
        }
        SessionEvent::TaskSpawned { .. } => vec![once("summoning")],
        SessionEvent::TaskFinished { ok, .. } => vec![once(if *ok { "jumping" } else { "failed" })],
        SessionEvent::TurnCompleted { .. } => vec![PetEvent::Clear, once("jumping")],
        SessionEvent::TurnFailed { .. } => vec![PetEvent::Clear, once("failed")],
        SessionEvent::MemorySaved { .. } => vec![once("eating")],
        _ => Vec::new(),
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

fn info(pet_id: &str, shown: &Shown, started: Instant, epoch: u64) -> PetStateInfo {
    let since_start = shown.since;
    let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(0);
    PetStateInfo {
        pet_id: pet_id.to_string(),
        animation: shown.animation.name.clone(),
        frames: shown.animation.frames,
        durations: shown.animation.durations.clone(),
        looping: shown.animation.looping,
        since_unix_ms: epoch.saturating_sub(elapsed.saturating_sub(since_start)),
        hidden: shown.hidden,
    }
}

async fn run(
    engine: Weak<PetEngine>,
    daemon: Weak<DaemonState>,
    mut arbiter: Arbiter,
    mut events: mpsc::UnboundedReceiver<(String, PetEvent)>,
) {
    let started = Instant::now();
    let now = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut ticks = tokio::time::interval(TICK);
    loop {
        let changed = tokio::select! {
            event = events.recv() => match event {
                Some((source, event)) => arbiter.handle(&source, event, now()),
                None => return,
            },
            _ = ticks.tick() => arbiter.tick(now()),
        };
        let Some(shown) = changed else { continue };
        let (Some(engine), Some(daemon)) = (engine.upgrade(), daemon.upgrade()) else {
            return;
        };
        let state = info(&arbiter.pet().id, &shown, started, unix_ms());
        *engine.state.lock().unwrap_or_else(PoisonError::into_inner) = Some(state.clone());
        daemon.announce(PET_STATE, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_activity_maps_to_animations() {
        let started = SessionEvent::TurnStarted { text: "hi".into() };
        assert_eq!(pet_events(&started), [PetEvent::Loop("working".into())]);
        let denied = SessionEvent::ToolFinished {
            call_id: "c".into(),
            ok: false,
            output: "The user denied this action.".into(),
        };
        assert!(pet_events(&denied).contains(&PetEvent::Once("confused".into())));
        let failed = SessionEvent::TurnFailed {
            message: "x".into(),
        };
        assert_eq!(
            pet_events(&failed),
            [PetEvent::Clear, PetEvent::Once("failed".into())]
        );
        let text = SessionEvent::TextDelta {
            text: "secret".into(),
        };
        assert!(
            pet_events(&text).is_empty(),
            "pets never see transcript content"
        );
    }
}
