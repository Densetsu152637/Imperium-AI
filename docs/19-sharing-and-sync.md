# 19 — Sharing and sync

**Status: designed** (roadmap item 11, [12](12-roadmap.md)). Nothing here is implemented
except what is marked as existing; the document records decisions so that later phases can be
built without re-arguing them. Where it names current behaviour it links the section that
owns it.

This is an architecture decision record in two parts: [session sharing](#part-1-session-sharing-and-export)
(one session, handed to one person or a few) and [team sync](#part-2-team-sync) (settings and
extensions kept consistent across a team's machines), followed by a [phased plan](#phased-plan)
and [open questions](#open-questions). Each key decision lists two or three options, their
trade-offs, and a recommendation.

## Requirements and non-goals

Requirements, taken from the vision and the shipped design:

- **Local first.** The daemon holds all state ([01](01-vision.md), [02](02-architecture.md#process-model),
  D1 and D11 in [13](13-decisions.md)). Nothing leaves the machine unless the user acts, and
  no feature may need a hosted Imperium service to work.
- **Secrets never travel.** Secrets live in the OS keychain and config files name them only
  ([10](10-configuration.md#secrets), D7). Sharing and sync must not add a path that carries
  one.
- **The safety floor holds.** Permission prompts for destructive actions and secret handling
  cannot be disabled by any layer ([08](08-agent-policy.md#layering-and-overrides)). Synced
  content is a lower-trust layer than the user's own choices.
- **Build on existing formats.** Journals, checkpoints, config layers, packs, and the project
  `.imperium/` folder already exist; add to them rather than beside them.

Non-goals: real-time collaboration in one session, a hosted multi-tenant service, syncing
model weights or runtime packs (large, per-machine, re-downloadable), and merging branches of
a conversation (not offered today, [12](12-roadmap.md) item 7).

## What exists today

| Capability | Behaviour | Source |
| --- | --- | --- |
| Transcript export | `session.export` builds Markdown or JSON from the journal (running or archived), including the subagent task tree, compaction notices and the files attached or made. Verbatim; the result carries a `warning` when text looks like a credential. CLI: `imperium session export <id> [--format md\|json] [--no-subagents] [-o file]`. The desktop refuses transcripts over 64 MiB and asks before saving one with a warning | [02](02-architecture.md#protocol), [07](07-desktop-ui.md#exporting-transcripts-implemented) |
| Session files | `<data>/sessions/<id>.jsonl` (journal), `<id>.meta.json`, `<id>.messages.json`, `<id>.turns.jsonl`, `<id>.agents/`, `<id>.checkpoints/` | [03](03-agent-engine.md#persistence) |
| Branching | `session.branch` copies the journal and root conversation up to a message into a new session (`branched_from`), using the `<id>.turns.jsonl` index | [03](03-agent-engine.md#persistence), [14](14-projects-and-automation.md#branching-and-comparison) |
| Checkpoints | Commits on private refs `refs/imperium/checkpoints/<session>/<n>` in the user's own repository, linked by `<id>.checkpoints/<n>.json` to the journal and saved conversation; `git push` does not send them | [14](14-projects-and-automation.md#checkpoints-and-rewinding) |
| Secret check | `imperium_memory::looks_secret`: known key prefixes (private key, `sk-`, `ghp_`, `github_pat_`, `xox`, `AKIA`, `AIza`, `hf_`) and long key-like tokens. Memory writes already refuse text that trips it | [17](17-memory.md) |
| Config layers | Defaults, user `config.toml`, app `models.toml`, host, project `.imperium/config.toml` (shareable) and `.imperium/config.local.toml` (git-ignored), session, environment variables | [10](10-configuration.md#layers) |
| Shareable project files | `<project>/.imperium/memory/` ("can be committed"), `.imperium/config.toml`, `.imperium/policy/`, `.imperium/skills/` | [17](17-memory.md#scopes), [10](10-configuration.md#layers), [08](08-agent-policy.md#layering-and-overrides), [06](06-extensibility.md) |
| Environments, permissions | `environments.json` in the data folder, with each environment's folders, runtime and permission rules; global rules in `[permissions]` | [14](14-projects-and-automation.md#environments-implemented), [05](05-tools-and-isolation.md#permissions) |
| Plugin trust | A plugin with hooks or MCP servers that an agent installs is added to `extensions.disabled_plugins`; nothing in it runs until the user turns it on in Connectors, where its source is shown; installs record `.imperium-source.json` provenance and are logged in the config change journal with undo | [06](06-extensibility.md#agents-manage-the-app-config-implemented), PR #122 |
| Settings export | "Import/export of the whole configuration (secrets excluded)" is listed as a recommended option, not built | [10](10-configuration.md#other-recommended-options) |
| Memory sync | Global memory stays local unless the user enables syncing it to an SSH host | [17](17-memory.md#scopes) |

Two observations drive the design. First, much team-shareable state already has a git-friendly
home inside the project (`.imperium/`). Team sync therefore concerns what lives in the
*user's* config and data folders and has no project to travel with. Second, session export is
a lossy, human-readable view; it cannot be re-opened by a daemon, so "send this session to a
colleague" today means a file they can read, not one they can continue.

---

## Part 1: Session sharing and export

### Session sharing goals

1. Hand a colleague a session they can read with full fidelity (tool calls, subagents,
   compaction notices, attachments).
2. Optionally let them open it in their own Imperium, read-only or as a fork they continue.
3. Make leaks hard: the default path redacts, previews, and warns.
4. Make tampering detectable when it matters, without requiring a PKI.

### Decision 1.1: format

| Option | What it is | Trade-offs |
| --- | --- | --- |
| **A. Keep Markdown/JSON export only** | Status quo | No work, readable anywhere; cannot be imported, no integrity, no artefacts or checkpoints, so forks are impossible |
| **B. Portable session bundle** (recommended, alongside A) | One archive (`.imperium-session`, a zip) holding a manifest, the journal, the conversation, the turn index, artefacts, subagent state and an optional checkpoint pack | Importable and forkable; verifiable; larger and needs a versioned format to maintain |
| **C. Sync the session folder files directly** | Copy `<data>/sessions/<id>.*` between machines | Cheapest; leaks every internal field and ties both sides to the same internal layout; no redaction point |

**Recommendation: B, with A kept for reading.** Markdown and JSON stay the way to *read* a
session without Imperium; the bundle is the way to *move* one. Both come from the same
journal pass, so redaction is implemented once.

Bundle layout (a versioned contract; additive changes bump the minor version):

```text
manifest.json        format version, session id, title, created/exported times, producing app version,
                     environment name (not path), model specs used, redaction report, file list with SHA-256
journal.jsonl        the journal, after redaction
conversation.json    the root conversation (from <id>.messages.json), after redaction
turns.jsonl          the turn index, so the importer can branch (see below)
agents/              each subagent's saved state
artefacts/           files attached to messages or made by agents, each with its recorded name
checkpoints.pack     optional: a `git bundle` of the session's checkpoint refs (a manifest entry per ref)
```

The checkpoint pack is optional because it carries the *content of the user's files* at each
turn, which is the largest and most sensitive part. It is produced with `git bundle` over the
private refs (`refs/imperium/checkpoints/<session>/*`), so it needs nothing the repository does
not already hold. An importer that opts in unpacks it under a private ref namespace in a
repository of its own choosing; without it, rewinding files is unavailable but the
conversation fork still works ([rewinding](14-projects-and-automation.md#checkpoints-and-rewinding)).

New surface (additive, so a protocol minor bump per [02](02-architecture.md#protocol)):
`session.bundle` (build, with a `dry_run` that returns the preview described next),
`session.import`, `imperium session bundle <id>` and `imperium session import <file>`, and
matching entries in the session menu beside the existing exports.

### Decision 1.2: redaction

Raw journals hold everything an agent read or ran. Redaction classes:

| Class | Examples | Default |
| --- | --- | --- |
| Secrets | Anything `looks_secret` flags; values of secrets resolved from the keychain during the session; `.env`-style assignments | Replace with `[redacted: <reason>]`; cannot be switched off for a bundle (the user may still export raw Markdown/JSON, as today) |
| Local paths | Home folder, environment folders, worktree locations, the data folder | Rewrite to placeholders: `~` for the home folder and `<env>/relative` for paths inside an environment folder, with the mapping kept only in the exporter's memory |
| Personal data | Username, hostname, email addresses, machine name in tool output | Pattern-based replacement; on by default, switchable |
| Remote content | URLs with tokens in the query string, `Authorization` headers in `web_fetch` results | Strip credentials from URLs; drop headers |
| Memory | `<relevant-memories>` blocks injected into turns ([17](17-memory.md)) | Dropped unless the user includes them, because global memory is private by design |

| Option | Trade-offs |
| --- | --- |
| **A. Regex redaction only** | Simple and fast; misses secrets that do not look like secrets (a password in a sentence) |
| **B. Regex plus known-value redaction, then a mandatory preview** (recommended) | Known-value matching (every keychain value the daemon resolved in this session, and values of the user's configured endpoint keys) removes secrets the regexes cannot see; the preview lets a human catch the rest |
| **C. Model-assisted redaction** | Catches prose secrets and names; sends the transcript to a model (a leak if the model is a cloud one), non-deterministic, costs tokens. Only acceptable with a local model, as an opt-in extra pass |

**Recommendation: B.** Reuse `looks_secret` rather than fork it, extracting it into a shared
function in `imperium-memory` (or a small crate both depend on) so memory, export and bundles
cannot drift. Extend it with known-value matching, which lives in the daemon because only it
knows which secrets a session resolved. Redaction changes the bytes, so the manifest records
what was done (counts per class, not the matches) and the file hashes are over the *redacted*
content.

**Preview before share.** `session.bundle { dry_run: true }` returns the redaction report and a
diff-style list of replaced spans with surrounding context. The desktop shows it in a dialog
(replacements highlighted, each reversible to "keep" for non-secret classes, never for
secrets); the CLI prints it and requires `--yes` to write. This extends the existing behaviour
where the desktop asks before saving a transcript with a credential warning
([07](07-desktop-ui.md#exporting-transcripts-implemented)): the warning becomes a report, and
the ask becomes a review. As today, no finding does not mean no secrets, and the dialog says so.

### Decision 1.3: import semantics

| Option | Trade-offs |
| --- | --- |
| **A. Read-only import only** | Safest; a viewer needs no environment and runs no tools; colleagues cannot continue the work |
| **B. Read-only by default, explicit "Fork"** (recommended) | Read-only session opens as an archived session (the same read-only presentation archived sessions have, [14](14-projects-and-automation.md#archiving-sessions)); *Fork* creates a live session from the conversation, in an environment the importer picks |
| **C. Import straight to a live session** | One step; risks running someone else's conversation, with their instructions embedded, in the importer's environment before they have read it |

**Recommendation: B.** An imported session is stored like an archived session with an
`imported_from` field (bundle id, signer, hash) and cannot receive messages. *Fork* reuses
`session.branch`: the bundle carries `turns.jsonl`, so the importer can fork at any recorded
point, and the fork "is an ordinary session that resumes from the copied files"
([03](03-agent-engine.md#persistence)). Safety rules for forks:

- The fork binds to an environment of the importer's choosing; no path from the bundle is
  trusted. File paths in the conversation are display text only.
- The fork starts with the importer's own instructions, permission rules and safety floor.
  Imported conversation is *data*: it is shown to the model as prior turns, never as system
  instructions, and any tool call in it is history, not a pending call.
- Activated plugins, hooks and `extension_loaded` events in the journal are not replayed; the
  fork starts with the importer's own set, so a bundle cannot turn on code.
- A bundle with a checkpoint pack is unpacked only after the importer chooses a repository, and
  only into the private ref namespace; HEAD, the index and branches are not touched, the same
  guarantee as for local checkpoints.

### Decision 1.4: signing and integrity

| Option | Trade-offs |
| --- | --- |
| **A. Hashes only** | The manifest lists SHA-256 for each file; detects corruption but not a rewritten bundle |
| **B. Optional detached signature with a per-install key** (recommended) | Ed25519 key generated on first use, private half in the OS keychain (the same place as all secrets, D7), public key shown as a fingerprint. Importers see "signed by fingerprint X", can pin a fingerprint to a name, and a changed file fails verification. No PKI or server |
| **C. Mandatory signing with team-issued keys or Sigstore** | Strong identity; needs infrastructure or an online service, which conflicts with local-first for the smallest team |

**Recommendation: B, optional.** Unsigned bundles import with a visible "unsigned" label; a
signature never makes content trusted, it only says who produced it and that it is unchanged.
The same key can sign team-sync commits later ([trust](#decision-23-trust-model)). The Tauri updater
key ([11](11-ci-cd.md)) is for releases and is not reused.

### Part 1 summary

| Decision | Recommendation |
| --- | --- |
| Format | Bundle (B) alongside existing Markdown/JSON |
| Redaction | Shared `looks_secret` plus known-value matching, mandatory preview |
| Import | Read-only by default, explicit Fork |
| Integrity | SHA-256 manifest always; optional Ed25519 signature |

---

## Part 2: Team sync

### Team sync goals

1. A new teammate gets the team's environments, policy pack, model setup, skills and rules in
   one step; changes propagate without everyone editing files by hand.
2. Personal and machine-specific state stays personal.
3. It works offline, never blocks a session, and never silently widens what agents may do.

### What syncs, and what never does

| Item | Syncs? | Notes |
| --- | --- | --- |
| Policy pack selection and overrides (`<config>/policy/`, a pack's Git URL) | Yes | A pack is already a Git repository ([08](08-agent-policy.md#pack-format)); sync pins the URL and revision |
| Environment *definitions* | Yes, as templates | Name, icon, colour, runtime kind and image, permission rules, project worktrees switch, default pet. **Folders are not synced** (local paths): a template has named folder slots (`repo`, `docs`) that each machine binds to a path on first use |
| Model table rows, aliases, cards | Yes | `[[models.catalog]]`, `alias`, `card` ([10](10-configuration.md#model-aliases), [10](10-configuration.md#model-cards)); endpoint `base_url` yes, `api_key` as the secret *name* only |
| Skills and plugins | Yes, as a lock list | Source URL and pinned revision, not the files; each machine installs through the normal install path |
| Mods | Yes, as a lock list | Same as plugins |
| Permission rules (global and per-environment) | Yes, with restrictions | See [trust](#decision-23-trust-model): deny and ask rules sync freely; allow rules need review |
| Budgets (`[budgets]` limits, thresholds, actions) | Yes, as team defaults | Not `usage.json` (that is this machine's ledger) |
| Shared memory | Already shared | `<project>/.imperium/memory/` travels with the repository ([17](17-memory.md#scopes)); not part of sync |
| Secrets, keychain contents, pairing keys, device tokens (`devices.json`) | **Never** | A synced entry may name a secret; the value is entered per machine ([10](10-configuration.md#secrets)) |
| Local paths: folder paths, path grants, worktree locations, `local-paths` values | **Never** | Grants are machine paths ([05](05-tools-and-isolation.md#path-grants)); `worktrees.location` is per-machine ([08](08-agent-policy.md#local-paths-come-from-app-settings)) |
| Personal settings: theme, layout, personality, pets owned, notification preferences, window state | **Never** (opt-in per key later) | Identity of the user, not of the team |
| Transcripts, journals, checkpoints, `usage.json`, session search index | **Never** by default | Shared deliberately with [bundles](#part-1-session-sharing-and-export) |
| Global memory | **Never** | Private by design ([17](17-memory.md#privacy)) |
| Daemon files (`daemon.json`, `daemon.lock`) | **Never** | Per-process |

The rule that makes this enforceable: sync works on an **allow-listed set of keys and
files**, not a deny-list. A new setting does not sync until someone marks it syncable in the
settings table ([10](10-configuration.md#implemented-settings)), so adding a setting can never
leak one by accident.

### Decision 2.1: transport

| Option | What it is | Trade-offs |
| --- | --- | --- |
| **A. Git-backed sync repository** (recommended first) | A repository (the team's existing host) holding a `team.toml` manifest and the synced files; each machine clones it into `<data>/team/<name>/` | No new infrastructure; history, review, blame and rollback for free; auth, access control and offline clones come from git and the user's existing credentials; works with `file://` for a shared drive. Eventual, pull-based, and commits are plain text anyone with write access can alter |
| **B. Optional sync server** (later) | A small self-hostable service speaking a documented API (list, fetch, push with ETag) | Push notifications of changes, per-key access control and an audit log; but a service to run, secure and version, and a second place for state to drift. Fits the open "hosted relay" question ([13](13-decisions.md#open-questions)) |
| **C. Sync through the phone/remote channel** | Reuse paired-device links ([15](15-mobile-and-remote.md)) between a user's machines | Fits one user's devices, not a team; the pairing model is per-computer and scoped, not shared identity |

**Recommendation: A first; B only if teams outgrow polling.** Git also matches how the rest of
the product already shares: packs are Git repositories (D9), extensions install from git URLs,
and project memory and config travel in the project's repository. The sync repository is
read by a *synced layer* (below), which is the same code a server transport would feed later.

Mechanics:

- `imperium team join <git-url>` clones, shows the manifest, and asks for consent per category
  before applying anything (see [trust](#decision-23-trust-model)). `imperium team status|pull|diff|leave`.
- The daemon pulls on start and on a timer (`team.pull_interval`, default 1 hour) and on
  demand. Clones are shallow, with hooks, filters and submodules off and the `ext::` transport
  refused, matching how checkpoints and extension installs already treat git
  ([14](14-projects-and-automation.md#checkpoints-and-rewinding),
  [06](06-extensibility.md#agents-manage-the-app-config-implemented)).
- Pushing is an explicit user action (`imperium team push`, or *Share with team* in a settings
  dialog) that commits the allow-listed changes with a message and pushes. Agents never push
  to the sync repository; it is not reachable from agent tools, in the spirit of D16 (automation that pushes
  stays narrow and reviewable).
- **Where it sits in the layers.** Team config is a new layer between *user config* and
  *app settings*, which users can override locally: the order becomes defaults, team, user
  config, app settings, and so on ([10](10-configuration.md#layers)). `config explain` reports
  "set by team layer `<name>` @ `<commit>`". Dropping a key from the team repository removes
  its effect on the next pull.

### Decision 2.2: conflict handling

Because the team layer is a separate, read-only-on-the-client layer rather than a merge into
the user's files, most conflicts do not occur.

| Option | Trade-offs |
| --- | --- |
| **A. Last writer wins, silently** | Simple; hides local edits and surprises people |
| **B. Layered precedence, no merge** (recommended) | Team values are defaults; local layers override key by key, and the UI shows when a local value shadows a team value. Conflicts left are git's, in the team repository, resolved by whoever pushes |
| **C. Three-way merge into the user's files** | Looks like true sync; rewrites hand-written `config.toml` (which the app layer deliberately never does, [10](10-configuration.md#layers)) and needs conflict UI |

**Recommendation: B.** Specifics:

- Local override wins for ordinary keys; a *locked* team key (marked in `team.toml`) cannot be
  overridden locally and is shown as managed. Locks exist for safety-relevant values only
  (denied tools, budgets set to `block`), never for preferences.
- Lists (permission rules, model rows) merge by identity: model rows by id, rules by
  (tool, pattern), the same keys `permissions.add_rule` already replaces on
  ([05](05-tools-and-isolation.md#permissions)), budgets by `id`. The evaluation rule stays
  the existing one: deny wins over ask, ask over allow, across every source.
- A push is a fast-forward only. If the remote moved, the pusher pulls, sees the diff, and
  pushes again. No force-push, no rebase is performed on their behalf.

### Decision 2.3: trust model

Pulled content tells every teammate's agents what they may do and what code to run. It is the
highest-risk part of the design.

Principles:

1. **The safety floor is not syncable.** Nothing in a team layer can disable permission
   prompts for destructive actions or secret handling ([08](08-agent-policy.md#layering-and-overrides)).
2. **Sync can only narrow by default; widening needs the user.** Deny and ask rules, lower
   budgets and stricter isolation apply once the user has joined. Allow rules,
   new endpoints, new plugins and loosened isolation are *proposals*: shown as a diff and
   applied only after the user accepts, per change, on each machine.
3. **Plugins keep the PR #122 rule.** A synced plugin or mod is installed through the same
   install path as an agent's, with the same outcome: one with hooks or MCP servers is added to
   `extensions.disabled_plugins` and does not run until the user turns it on in Connectors,
   where the source, commit and "installed by team sync `<name>`" are shown
   ([06](06-extensibility.md#agents-manage-the-app-config-implemented)). Team-installed
   plugins do not read the daemon's environment through `${VAR}`, as for agent-installed ones.
   Skills and commands without hooks or servers are listed at once. Installs are logged in
   the config change journal with undo.
4. **Who may push** is the team repository's own access control (write access), plus an
   optional **signed commits requirement** (`require_signed = true` in `team.toml`, checked
   against fingerprints the user accepted at join time, using the key from
   [decision 1.4](#decision-14-signing-and-integrity)). Pins use full commit hashes, never moving
   branches, for plugins and packs.
5. **Consent is recorded.** `team join` writes the accepted categories and the commit they
   were accepted at to `<config>/team.toml` (local); a later pull that adds a *category* or an
   allow-class change asks again.

| Option | Trade-offs |
| --- | --- |
| **A. Trust the repository fully** | One consent; a compromised or careless commit turns on code and allow rules on every machine |
| **B. Narrow automatically, widen on review** (recommended) | Matches how installs already work (PR #122) and the "deny wins" ordering; more prompts, but only when a push widens something |
| **C. Admin roles in a server** | Fine-grained roles; requires the optional server (decision 2.1 B), not available in phase 1 |

**Recommendation: B**, with signed commits as an opt-in for teams that want an author check.

### Decision 2.4: offline behaviour

- The last good pull is a complete layer on disk; the daemon never needs the network to start
  or to run a session. A failed pull logs a warning and surfaces as a status line, not an error.
- If the clone is corrupt or fails validation, the previous good checkout is kept and used
  (the same "drop the entry, warn, keep going" stance as `models.toml`,
  [10](10-configuration.md#layers)).
- A team layer older than `team.stale_after_days` (default 30) is flagged in settings; it still
  applies.
- Pushing needs the network; a failed push leaves the local commit staged for retry.
- Leaving a team (`team leave`) removes the layer; installed extensions stay (listed with
  their source, removable) and local overrides are kept.

### Part 2 summary

| Decision | Recommendation |
| --- | --- |
| Transport | Git repository first, optional server later |
| Conflicts | Layered precedence, locks for safety keys, merge lists by identity, fast-forward pushes |
| Trust | Narrow automatically, widen on per-machine review; PR #122 plugin rule; signed commits optional |
| Offline | Last good pull applies; sync never blocks a session |

---

## Phased plan

| Phase | Delivers | Why this order |
| --- | --- | --- |
| **0. Shared redaction** | Extract `looks_secret` into a shared function; add known-value matching; `redaction report` type in the protocol; export uses it for its warning | Both parts need it; no new format, so smallest risk. **Smallest useful first slice** |
| **1. Settings export/import** | `imperium config export` / `import` of the allow-listed syncable keys as one file (secrets by name only, local paths excluded), with a preview diff; the "whole configuration, secrets excluded" item in [10](10-configuration.md#other-recommended-options) | Gives a manual, offline form of team sync and defines the allow-list that every later phase needs. A teammate can be onboarded with a file today |
| **2. Session bundle** | `session.bundle` with dry-run preview, read-only import, SHA-256 manifest | Real sharing; no checkpoints or signing yet |
| **3. Fork from import, checkpoint pack, signing** | Fork at a recorded point, optional `git bundle`, Ed25519 signatures | Needs the trust rules for imported conversations tested first |
| **4. Team sync over git** | `imperium team join/pull/push/status`, the team layer, consent flow, extension lock lists using the PR #122 install path | Builds on phase 1's allow-list and the install path; the largest slice |
| **5. Optional server** | Documented sync API and a self-hostable implementation | Only if phase 4 shows polling or access control to be a problem |

Phases 0–1 are the "smallest useful first phase": they need no new transport, no new file
format beyond a settings file, and they remove the worst failure (exporting a secret).
Each phase updates the owning documents in the same pull request as its code (protocol in
[02](02-architecture.md#protocol), settings in [10](10-configuration.md#implemented-settings),
the export UI in [07](07-desktop-ui.md#exporting-transcripts-implemented)).

Validation criteria per phase: redaction has tests for every class and for a secret resolved
from the keychain during the session; an exported bundle re-imports to an identical read-only
transcript; a bundle with a flipped byte fails verification; a team layer that adds a plugin
with hooks leaves it disabled; a pull with no network leaves the daemon and sessions working.

## Compatibility and migration

- Bundles carry a format version; an importer refuses a higher major and reads any lower one.
  Unknown manifest fields are ignored, matching how unknown config keys are ([10](10-configuration.md#layers)).
- The protocol changes are additive methods (`session.bundle`, `session.import`, `team.*`)
  and a minor version bump; existing `session.export` is unchanged.
- The team layer adds one layer to the config order and one local file; with no team joined,
  behaviour is identical to today. Rolling back is `team leave`.

## Risks

- **Redaction false negatives.** Mitigated by known-value matching and the mandatory
  preview, but a human decision remains; the UI must never imply a bundle is safe.
- **Allow-list drift.** A setting that should sync but is not marked is a usability cost; one
  that is marked wrongly is a leak. The table of syncable keys is reviewed like a permission
  change.
- **Prompt-injection through synced skills.** Skills and cards are text read by models.
  Narrowing-by-default does not catch a hostile skill body; pinning commits, showing the
  source, and review on change are the controls, and teams should treat the sync repository
  as code.
- **Bundle size.** Checkpoint packs can be large; they are optional and sized in the
  preview.

## Open questions

| Question | Owner | Needed by |
| --- | --- | --- |
| Should imported bundles be searchable by [session search](17-memory.md#session-search-implemented) alongside the user's own sessions, or kept out of the index? | Maintainers | Phase 2 |
| Is a per-install Ed25519 key enough, or do teams need to name signers (a fingerprint-to-name list in `team.toml`)? | User | Phase 3 |
| Which settings are syncable on day one? The proposed start: model table, aliases, cards, budgets, permission rules, policy pack pin, environment templates | User | Phase 1 |
| Should environment templates bind folders by name slot, or by a `.imperium/` marker file in each repository? | Maintainers | Phase 4 |
| Do teams want shared (not private) cost reporting? It would need `usage.json` aggregates, which this design excludes | User | Phase 5 |
| Offer a hosted sync/relay service, or self-host only (tied to the open relay question in [13](13-decisions.md#open-questions))? | User | Phase 5 |
| Should memory entries marked `share: local-only` be dropped or just flagged inside a bundle? | Maintainers | Phase 2 |

## Decision summary

| ID | Decision | Revisit if |
| --- | --- | --- |
| S1 | Sessions move as a signed-optional bundle; Markdown/JSON stay as the read format | Bundles prove too heavy to maintain |
| S2 | Redaction is shared code with known-value matching and a mandatory preview | Model-assisted redaction becomes safe to run locally by default |
| S3 | Imports are read-only; forking is explicit and never replays code | Users need one-step continue and accept the risk |
| T1 | Team sync starts as a git repository read as a config layer | Teams need push notification or roles |
| T2 | Local layers override team defaults; safety keys can be locked; lists merge by identity | Silent drift between machines becomes a problem |
| T3 | Sync narrows automatically and widens only on per-machine review; plugins follow PR #122 | A signed-commit workflow makes review redundant for a team |
