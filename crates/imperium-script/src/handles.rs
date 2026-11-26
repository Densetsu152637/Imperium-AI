//! The program's handle table: which host functions it was given, re-checked on every call
//! (plan C3, F1, and the stage 1a finding).
//!
//! The worker is untrusted, and a program that calls a bare name reaches the host as a call by
//! that name with no name lookup before it. So a call is never admitted because its name was
//! looked up once: [`HandleTable::admit`] resolves the name again at call time, through the same
//! resolver the lookups use (the callable set, the universe), and refuses a name that does not
//! resolve now or that resolves to a different tool than the one the program was given.

use std::collections::HashMap;

/// Whether a call may run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Run this tool (the engine's name for it).
    Admitted(String),
    /// Refuse the call, for this reason.
    Refused(String),
}

/// Host functions issued to one program, by the name the program uses.
#[derive(Debug, Default, Clone)]
pub struct HandleTable {
    issued: HashMap<String, String>,
}

impl HandleTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that a lookup of `function` was answered with the host function for `tool`.
    pub fn issue(&mut self, function: &str, tool: &str) {
        self.issued.insert(function.to_owned(), tool.to_owned());
    }

    /// The tool issued under `function`, if a lookup issued one.
    #[must_use]
    pub fn issued(&self, function: &str) -> Option<&str> {
        self.issued.get(function).map(String::as_str)
    }

    /// Decides a call to `function` at call time. `resolve` maps a name to the tool it stands
    /// for now, or `None` when the program may not call it (unknown, script-refused, outside the
    /// universe); it is the resolver the name lookups use.
    pub fn admit(&self, function: &str, resolve: impl Fn(&str) -> Option<String>) -> Admission {
        let Some(now) = resolve(function) else {
            return Admission::Refused(format!(
                "{function} is not a function this program may call"
            ));
        };
        match self.issued(function) {
            Some(given) if given != now => Admission::Refused(format!(
                "{function} no longer names the tool this program was given"
            )),
            _ => Admission::Admitted(now),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver(callable: &'static [&'static str]) -> impl Fn(&str) -> Option<String> {
        move |name| callable.contains(&name).then(|| name.to_owned())
    }

    #[test]
    fn a_bare_name_call_is_admitted_only_when_it_resolves_now() {
        let table = HandleTable::new();
        assert_eq!(
            table.admit("read_file", resolver(&["read_file"])),
            Admission::Admitted("read_file".to_owned())
        );
        // Never looked up and not callable: a forged or refused name.
        assert!(matches!(
            table.admit("spawn_agent", resolver(&["read_file"])),
            Admission::Refused(_)
        ));
    }

    #[test]
    fn an_issued_function_is_checked_again_at_call_time() {
        let mut table = HandleTable::new();
        table.issue("grep", "grep");
        assert_eq!(
            table.admit("grep", resolver(&["grep"])),
            Admission::Admitted("grep".to_owned())
        );
        // The tool left the universe after the lookup.
        assert!(matches!(
            table.admit("grep", resolver(&[])),
            Admission::Refused(_)
        ));
    }

    #[test]
    fn a_name_that_now_resolves_to_another_tool_is_refused() {
        let mut table = HandleTable::new();
        table.issue("search", "grep");
        let now = |name: &str| (name == "search").then(|| "web_search".to_owned());
        assert!(matches!(table.admit("search", now), Admission::Refused(_)));
    }
}
