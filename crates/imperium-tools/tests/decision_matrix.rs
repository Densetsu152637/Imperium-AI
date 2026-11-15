//! The permission decision for every mode, kind of call, matching rule and safety-floor result,
//! written out as a table that does not call the code that computes it, and the two ways a
//! caller composes rules, floor and mode (the floor computed from the call, and the floor
//! given by the caller). The agent profile is not part of it: it stays with the engine.

use imperium_tools::permissions::{
    CallKind, Decision, PermissionMode, PermissionRule, Permissions, Refusal, RuleAction,
    SharedRules, Target, decide_call, safety_floor,
};

const R: Decision = Decision::Run;
/// Asks, and "allow for this session" is offered.
const A: Decision = Decision::Ask { sticky: true };
/// Asks every time.
const E: Decision = Decision::Ask { sticky: false };
const D: Decision = Decision::Refuse(Refusal::Rule);
const P: Decision = Decision::Refuse(Refusal::Plan);

/// Each row is the decisions for (no rule, floor off), (no rule, floor on), (allow, off),
/// (allow, on), (ask, off), (ask, on), (deny, off), (deny, on).
type Row = [Decision; 8];

const KINDS: [CallKind; 5] = [
    CallKind::Read,
    CallKind::Edit,
    CallKind::Execute,
    CallKind::Other { asks: true },
    CallKind::Other { asks: false },
];

const RULES: [Option<RuleAction>; 4] = [
    None,
    Some(RuleAction::Allow),
    Some(RuleAction::Ask),
    Some(RuleAction::Deny),
];

/// Rows in the order of [`KINDS`].
fn table(mode: PermissionMode) -> [Row; 5] {
    let read: Row = [R, R, R, R, E, E, D, D];
    match mode {
        PermissionMode::Ask => [
            read,
            [E, E, E, E, E, E, D, D],
            [E, E, E, E, E, E, D, D],
            [E, E, E, E, E, E, D, D],
            [R, E, R, E, E, E, D, D],
        ],
        PermissionMode::Default => [
            read,
            [A, E, R, E, E, E, D, D],
            [A, E, R, E, E, E, D, D],
            [A, E, R, E, E, E, D, D],
            [R, E, R, E, E, E, D, D],
        ],
        PermissionMode::AcceptEdits => [
            read,
            [R, E, R, E, E, E, D, D],
            [A, E, R, E, E, E, D, D],
            [A, E, R, E, E, E, D, D],
            [R, E, R, E, E, E, D, D],
        ],
        PermissionMode::Plan => [
            read,
            [P, P, P, P, P, P, D, D],
            [P, P, P, P, P, P, D, D],
            [P, P, P, P, P, P, D, D],
            [R, R, R, R, P, P, D, D],
        ],
        PermissionMode::Full => [
            read,
            [R, E, R, E, E, E, D, D],
            [R, E, R, E, E, E, D, D],
            [R, E, R, E, E, E, D, D],
            [R, E, R, E, E, E, D, D],
        ],
    }
}

#[test]
fn every_mode_kind_rule_and_floor_gives_the_tabled_decision() {
    assert_eq!(PermissionMode::ALL.len(), 5, "a new mode needs table rows");
    for mode in PermissionMode::ALL {
        let rows = table(mode);
        for (kind, row) in KINDS.into_iter().zip(rows) {
            for (index, expected) in row.into_iter().enumerate() {
                let rule = RULES[index / 2];
                let floor = index % 2 == 1;
                assert_eq!(
                    decide_call(mode, kind, rule, floor),
                    expected,
                    "{mode} {kind:?} rule {rule:?} floor {floor}"
                );
            }
        }
    }
}

fn rule(tool: &str, pattern: &str, action: RuleAction) -> PermissionRule {
    PermissionRule {
        tool: tool.into(),
        pattern: pattern.into(),
        action,
    }
}

fn permissions(mode: PermissionMode, rules: Vec<PermissionRule>) -> Permissions {
    Permissions::new(SharedRules::new(rules), SharedRules::default()).with_mode(mode)
}

/// What a caller gets from rules, floor and mode together: the decision, the matching rule's
/// action, and whether the floor applied. `floor` replaces the floor computed from the call.
fn judged(
    permissions: &Permissions,
    tool: &str,
    target: &Target,
    kind: CallKind,
    floor: Option<bool>,
) -> (Decision, Option<RuleAction>, bool) {
    let rule = permissions.evaluate(tool, target);
    let floored = floor.unwrap_or_else(|| safety_floor(kind, target));
    (
        decide_call(permissions.mode(), kind, rule, floored),
        rule,
        floored,
    )
}

fn command(line: &str) -> Target {
    Target::Command(line.into())
}

#[test]
fn the_floor_computed_from_the_call_joins_the_rules_and_the_mode() {
    let rules = vec![
        rule("run_command", "rm", RuleAction::Allow),
        rule("run_command", "cargo test", RuleAction::Allow),
        rule("run_command", "curl", RuleAction::Deny),
    ];
    let call = |mode, line: &str| {
        judged(
            &permissions(mode, rules.clone()),
            "run_command",
            &command(line),
            CallKind::Execute,
            None,
        )
    };
    let allow = Some(RuleAction::Allow);
    // An allowed command runs, unless the floor covers it: then it asks every time.
    assert_eq!(
        call(PermissionMode::Default, "cargo test"),
        (R, allow, false)
    );
    assert_eq!(
        call(PermissionMode::Default, "rm -rf build"),
        (E, allow, true)
    );
    assert_eq!(call(PermissionMode::Full, "rm -rf build"), (E, allow, true));
    assert_eq!(call(PermissionMode::Plan, "rm -rf build"), (P, allow, true));
    // No rule: asks, with the session allowance offered, when the floor does not cover it.
    assert_eq!(
        call(PermissionMode::Default, "cargo build"),
        (A, None, false)
    );
    assert_eq!(
        call(PermissionMode::Default, "git reset --hard HEAD~1"),
        (E, None, true)
    );
    // A deny rule wins in every mode, floor or not.
    for mode in PermissionMode::ALL {
        assert_eq!(
            call(mode, "curl example.com"),
            (D, Some(RuleAction::Deny), false),
            "{mode}"
        );
    }
}

#[test]
fn a_floor_given_by_the_caller_replaces_the_computed_one() {
    let path = Target::Path {
        absolute: "/p/.env".into(),
        relative: Some(".env".into()),
    };
    let allowed = vec![rule("write_file", "", RuleAction::Allow)];
    let call = |floor, kind, target: &Target| {
        judged(
            &permissions(PermissionMode::Default, allowed.clone()),
            "write_file",
            target,
            kind,
            floor,
        )
    };
    let allow = Some(RuleAction::Allow);
    // The computed floor covers a write to `.env`; a given `false` lifts it and a given `true`
    // is used as it is.
    assert_eq!(call(None, CallKind::Edit, &path), (E, allow, true));
    assert_eq!(call(Some(false), CallKind::Edit, &path), (R, allow, false));
    assert_eq!(
        call(Some(true), CallKind::Edit, &Target::Other),
        (E, allow, true)
    );
    // A tool that always asks (a provider's `always_asks`) is a given floor on another kind.
    let provider = vec![rule("mcp__srv__do", "", RuleAction::Allow)];
    let provider_call = |floor| {
        judged(
            &permissions(PermissionMode::Default, provider.clone()),
            "mcp__srv__do",
            &Target::Other,
            CallKind::Other { asks: false },
            floor,
        )
    };
    assert_eq!(provider_call(Some(false)), (R, allow, false));
    assert_eq!(provider_call(Some(true)), (E, allow, true));
    assert_eq!(provider_call(None), (R, allow, false));
}
