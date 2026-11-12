//! A connection's choice of notification topics (`events.subscribe`, `events.unsubscribe`).
//!
//! The choice sits on top of what the connection may receive anyway and can only narrow it: it
//! is consulted after the restricted-connection rules, never instead of them.

use std::collections::BTreeSet;

use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::methods::{EventsSelection, is_topic, topic_matches};

/// Most distinct topics one connection may name, so a client cannot grow the daemon's memory.
const MAX_TOPICS: usize = 128;

#[derive(Debug, Default, Clone)]
pub(crate) struct EventFilter {
    /// `None` until the first subscription: everything the connection may receive.
    include: Option<BTreeSet<String>>,
    /// Topics withheld from `include` (or from everything).
    exclude: BTreeSet<String>,
}

impl EventFilter {
    /// Whether a notification of this `method` passes the connection's choice.
    pub fn allows(&self, method: &str) -> bool {
        self.include
            .as_ref()
            .is_none_or(|include| include.iter().any(|topic| topic_matches(topic, method)))
            && !self
                .exclude
                .iter()
                .any(|topic| topic_matches(topic, method))
    }

    pub fn subscribe(&mut self, topics: &[String]) -> Result<(), ErrorObject> {
        check(topics)?;
        if topics.is_empty() {
            return Ok(());
        }
        if topics.iter().any(|topic| topic == "*") {
            *self = EventFilter::default();
            return Ok(());
        }
        let mut next = self.clone();
        let include = next.include.get_or_insert_with(BTreeSet::new);
        include.extend(topics.iter().cloned());
        // A subscription lifts every mute it covers, in either direction: a mute of `*` or of
        // `session.*` no longer hides a later `session.event`, and `session.*` lifts the mute
        // of `session.updated`.
        next.exclude.retain(|muted| {
            !topics
                .iter()
                .any(|topic| topic_matches(muted, topic) || topic_matches(topic, muted))
        });
        next.commit(self)
    }

    pub fn unsubscribe(&mut self, topics: &[String]) -> Result<(), ErrorObject> {
        check(topics)?;
        let mut next = self.clone();
        for topic in topics {
            if let Some(include) = &mut next.include {
                // Everything the topic covers goes, so `session.*` also drops `session.event`.
                include.retain(|subscribed| !topic_matches(topic, subscribed));
            }
            let still_delivered = next.include.as_ref().is_none_or(|include| {
                include
                    .iter()
                    .any(|subscribed| topic_matches(subscribed, topic))
            });
            if still_delivered {
                next.exclude.insert(topic.clone());
            }
        }
        next.commit(self)
    }

    /// Replaces `current` with this choice unless it names too many topics.
    fn commit(self, current: &mut EventFilter) -> Result<(), ErrorObject> {
        if self.exclude.len() + self.include.as_ref().map_or(0, BTreeSet::len) > MAX_TOPICS {
            return Err(too_many());
        }
        *current = self;
        Ok(())
    }

    pub fn selection(&self) -> EventsSelection {
        EventsSelection {
            topics: self
                .include
                .as_ref()
                .map(|include| include.iter().cloned().collect()),
            muted: self.exclude.iter().cloned().collect(),
        }
    }
}

fn check(topics: &[String]) -> Result<(), ErrorObject> {
    if topics.len() > MAX_TOPICS {
        return Err(too_many());
    }
    match topics.iter().find(|topic| !is_topic(topic)) {
        Some(bad) => Err(ErrorObject::new(
            codes::INVALID_PARAMS,
            format!(
                "{bad:?} is not a topic: use a notification method such as session.event, a prefix such as session.*, or *"
            ),
        )),
        None => Ok(()),
    }
}

fn too_many() -> ErrorObject {
    ErrorObject::new(
        codes::INVALID_PARAMS,
        format!("a connection may name at most {MAX_TOPICS} topics"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(topics: &[&str]) -> Vec<String> {
        topics.iter().map(|topic| (*topic).to_string()).collect()
    }

    #[test]
    fn a_new_filter_allows_everything() {
        let filter = EventFilter::default();
        assert!(filter.allows("session.event"));
        assert!(filter.allows("anything.else"));
        assert_eq!(filter.selection(), EventsSelection::default());
    }

    #[test]
    fn the_first_subscription_narrows_and_later_ones_add() {
        let mut filter = EventFilter::default();
        filter.subscribe(&names(&["session.event"])).unwrap();
        assert!(filter.allows("session.event"));
        assert!(!filter.allows("session.updated"));
        filter.subscribe(&names(&["model.*"])).unwrap();
        assert!(filter.allows("model.residency"));
        assert!(!filter.allows("models.bench_progress"));
        assert_eq!(
            filter.selection().topics,
            Some(names(&["model.*", "session.event"]))
        );
    }

    #[test]
    fn star_returns_to_everything_and_an_empty_list_changes_nothing() {
        let mut filter = EventFilter::default();
        filter.subscribe(&names(&["session.event"])).unwrap();
        filter.subscribe(&[]).unwrap();
        assert!(!filter.allows("model.residency"));
        filter.subscribe(&names(&["*"])).unwrap();
        assert!(filter.allows("model.residency"));
        assert_eq!(filter.selection(), EventsSelection::default());
    }

    #[test]
    fn unsubscribing_removes_a_subscription_or_mutes_from_everything() {
        let mut filter = EventFilter::default();
        filter.unsubscribe(&names(&["model.residency"])).unwrap();
        assert!(!filter.allows("model.residency"));
        assert!(filter.allows("session.event"));
        assert_eq!(filter.selection().muted, names(&["model.residency"]));
        // Subscribing again lifts the mute.
        filter.subscribe(&names(&["model.residency"])).unwrap();
        assert!(filter.allows("model.residency"));
        assert!(filter.selection().muted.is_empty());

        filter.unsubscribe(&names(&["model.residency"])).unwrap();
        assert!(filter.selection().topics.unwrap().is_empty());
        assert!(!filter.allows("session.event"), "nothing is subscribed");
    }

    #[test]
    fn a_prefix_unsubscribe_and_a_single_mute_inside_a_prefix_subscription() {
        let mut filter = EventFilter::default();
        filter
            .subscribe(&names(&["session.*", "budgets.changed"]))
            .unwrap();
        filter.unsubscribe(&names(&["session.updated"])).unwrap();
        assert!(filter.allows("session.event"));
        assert!(!filter.allows("session.updated"));
        assert!(filter.allows("budgets.changed"));
        filter.unsubscribe(&names(&["session.*"])).unwrap();
        assert!(!filter.allows("session.event"));
        assert!(filter.allows("budgets.changed"));
    }

    #[test]
    fn subscribing_after_muting_everything_or_a_family_delivers_again() {
        let mut filter = EventFilter::default();
        filter.unsubscribe(&names(&["*"])).unwrap();
        assert!(!filter.allows("session.event"));
        filter.subscribe(&names(&["session.event"])).unwrap();
        assert!(filter.allows("session.event"));
        assert!(!filter.allows("model.residency"));
        assert!(filter.selection().muted.is_empty());

        let mut filter = EventFilter::default();
        filter.unsubscribe(&names(&["session.*"])).unwrap();
        assert!(!filter.allows("session.event"));
        filter.subscribe(&names(&["session.event"])).unwrap();
        assert!(filter.allows("session.event"));
        assert!(!filter.allows("session.updated"));
    }

    #[test]
    fn malformed_and_excessive_topics_are_refused_without_a_partial_change() {
        let mut filter = EventFilter::default();
        let error = filter
            .subscribe(&names(&["session.event", "Not A Topic"]))
            .unwrap_err();
        assert_eq!(error.code, codes::INVALID_PARAMS);
        assert!(filter.allows("model.residency"), "nothing changed");
        let many: Vec<String> = (0..=MAX_TOPICS).map(|n| format!("topic.n{n}")).collect();
        assert!(filter.subscribe(&many).is_err());
    }
}
