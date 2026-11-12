# 14 — Projects and automation

## Environments (implemented)

An **environment** is the unit users work in: a name, an optional icon and colour, and one or
more folders. The folders are the sandbox the agent works in: its file tools accept any path
inside any of them, and `run_command` runs in the first folder (the working directory) under the
session's isolation tier, which can write to every folder
([isolation](05-tools-and-isolation.md#isolation-tiers)). A session belongs to one environment
and records it (`environment_id` and `folders` in `session.list`); folders added later apply to
sessions started from then on.

The daemon keeps environments in `environments.json` in its data folder and serves them over
RPC:

| Method | Params | Result |
| --- | --- | --- |
| `environments.list` | `{}` | `{ environments: EnvironmentInfo[], git: string[] }` (`git`: ids of local environments with a folder inside a git repository, which can start sessions on worktrees; also sent with `environments.changed`) |
| `environments.create` | `{ name, icon?, color?, folders, runtime?, persistent_agent?, pet?, project_worktrees? }` (`persistent_agent` is [deprecated](#persistent-agents-implemented)) | `EnvironmentInfo` |
| `environments.update` | `{ id, name?, icon?, color?, pinned?, runtime?, persistent_agent?, pet?, project_worktrees? }` (empty icon/colour/pet clears it; `persistent_agent` is [deprecated](#persistent-agents-implemented): `true` adds the default agent when there are none, `false` removes only that default agent and is refused with an error when the environment has more than one agent or an agent with a role) | `EnvironmentInfo` |
| `environments.test_runtime` | `{ id }` | `{ ok, message }` |
| `environments.delete` | `{ id }` | `{}` (folders and running sessions are left alone; Home is refused) |
| `environments.add_folder` | `{ id, folder }` (an existing absolute folder) | `EnvironmentInfo` |
| `environments.remove_folder` | `{ id, folder }` | `EnvironmentInfo` |

`EnvironmentInfo` is `{ id, name, icon, color, folders, pinned, runtime, agents, persistent_agent
(deprecated), agent_session_id (deprecated), pet, project_worktrees, builtin }`, where `builtin` marks [Home](#home-implemented), and `project_worktrees` (on unless turned off)
opens the folders' [git worktrees](#worktrees) to its sessions, and `pet` is the environment's default pet (an installed pet's id) for the
[nursery](16-pets.md#imperium-nursery), or none to let the nursery decide; folders first-to-last with the working directory first; pinned environments are listed first by clients. Every change is announced to all clients as `environments.changed`
(the full list). `session.create` takes `environment_id`; `project_dir` still works and maps to
the environment whose first folder it is, created (named after the folder) when there is none.
Sessions from before environments are moved the same way when the daemon resumes them, and the
desktop app turns each folder it had opened into an environment the first time it connects.

From the command line: `imperium env list`, `imperium env create <name> [folder...]`,
`imperium env add-folder <env> <folder>`, `imperium env remove-folder <env> <folder>`,
`imperium env rename <env> <name>`, `imperium env delete <env>` (an environment by name or id),
`imperium env set-runtime <env> local|docker|wsl|ssh [--image I] [--container C] [--distro D]
[--host H]`, `imperium env test <env>`, `imperium env agent list|add|set|rm`
([persistent agents](#persistent-agents-implemented)), and
`imperium chat --env <name|id> [--worktree [BRANCH]]`.

### Runtimes (implemented)

An environment's `runtime` says where its sessions' file and command tools run. It is an
object tagged by `kind`:

| `kind` | Fields | Tools run | Folders are |
| --- | --- | --- | --- |
| `local` (default) | none | on this machine, under `isolation.tier` | local folders |
| `docker` | `image` | each command in a fresh container from `image` (the container tier, network on) | local folders, mounted at `/workspace`, `/folders/2`, … |
| `docker` | `image`, `container` | `docker exec` into that running container | paths inside the container |
| `wsl` | `distro` | `wsl.exe -d <distro> --cd <folder> --exec sh -c …` (Windows only) | Linux paths; Windows paths are translated to `/mnt/<drive>/…` |
| `ssh` | `host` | the system `ssh` client (`BatchMode`, so keys and agents from your SSH config; never passwords) | paths on the host |

For a remote runtime (a container, WSL, or SSH) the folders are not checked on this machine; a
missing one fails the tool that uses it, and `environments.test_runtime` (the **Test** button,
`imperium env test`) checks that the runtime can be reached: Docker's engine runs and has the
image (or the container is running), the WSL distribution is installed, or SSH connects without
a prompt. Details of how tools run there are in
[isolation](05-tools-and-isolation.md#environment-runtimes-implemented). The approval layer is
unchanged. Changing an environment's runtime keeps its folders (translating Windows paths for
WSL) and applies to sessions started afterwards. Project files on a remote runtime
(instructions, skills, project memory) are not read yet.

### Persistent agents (implemented)

An environment can keep any number of **persistent agents**, each one session that is always
available, listed first among that environment's sessions with a **Persistent** badge
(`persistent: true` in `session.list`) and titled `<environment> · <agent name>` (renamed with
either). A new environment has
none: nothing is created until you add one, and turning the deprecated switch off or deleting an
agent never brings one back by itself.

Each agent has its own **name** (1 to 60 characters on one line, no control characters, unique in
the environment ignoring case with Unicode case folding), an
optional **role** (what it is for, up to 4000 characters, added to its instructions after "Your
name is …"), its own session, and its own session limits (at most 2 sessions it started at once and
20 a day). Agents of one environment run independently: separate conversations, separate
compaction, separate reports. An agent starts at once when it is added (or as soon as the
environment has a folder) and with every daemon start, and resumes the same conversation each
time, however long ago it was used: its session id is kept with the agent, outside the
`agents.resume_days` window that applies to ordinary sessions. Like a project agent (below) it has
`start_session`, which starts sessions in the same environment and reports back to it. When a turn
starts with the conversation over the model's context budget, older turns are summarised first (the
same compaction as `/compact`), so it can run for days. Deleting an agent stops its session and
archives it, so its history is listed with the other archived sessions. Changing an agent's name or
role restarts it with the same conversation; changing the environment's runtime restarts all of
them; saving an agent with the same name and role restarts nothing, and a restart keeps the
agent's limits and the reports of the sessions it already started. Deleting the environment stops
them too. Agents have no pet in the nursery. The project-private memory and the environment's `Memory.md` ([17](17-memory.md#environment-memory-implemented)) are
shared by all its agents, so each agent is told to begin the names of the entries it saves with its own
name (lower case, with dashes; an environment-scope entry is a section of `Memory.md` named
the same way) and to keep its open-task list in `<name>-open-tasks`. Adding,
renaming, changing the role of, or deleting agents of one environment is serialised: it never starts
an agent twice.

| Method | Params | Result |
| --- | --- | --- |
| `environments.agent_list` | `{ environment_id }` | `{ agents: EnvironmentAgent[] }` |
| `environments.agent_create` | `{ environment_id, name, role?, permissions? }` | `EnvironmentAgent` plus `started` and, when it could not start (no folder yet, no model), `reason`; the agent is added anyway and starts when the cause is fixed |
| `environments.agent_update` | `{ environment_id, agent_id, name?, role?, permissions? }` (`permissions` replaces the [permission profile](#agent-permission-profiles-implemented) and applies at once, without a restart) | `EnvironmentAgent` |
| `environments.agent_delete` | `{ environment_id, agent_id }` | `{}` |

`EnvironmentAgent` is `{ id, name, role, session_id, permissions }` (`session_id` is none until
the agent has started; `permissions` is the [permission profile](#agent-permission-profiles-implemented)); `EnvironmentInfo.agents` carries them, and every change is announced as
`environments.changed`. From the command line: `imperium env agent list <env>`, `imperium env
agent add <env> <name> [--role TEXT] [profile flags]`, `imperium env agent set <env> <agent> [--name N] [--role
TEXT] [profile flags]`, and `imperium env agent rm <env> <agent>` (an agent by name or id). **Breaking:** the
earlier `imperium env agent <env> on|off` no longer exists. In the desktop, the
environment window's *Agents* section lists them with *Add agent*, edit and delete.

#### Agent permission profiles (implemented)

Each agent can carry a **permission profile**: what it may use *beyond* the environment's own
permissions. A profile **only narrows**: it hides tools the agent would otherwise have and
refuses calls to them. It grants nothing: the environment's path grants, rules and permission
mode ([05](05-tools-and-isolation.md#permissions)) and the safety floor decide every call that
remains, exactly as for any session, and a tool the session was never offered (a server that is
not configured, a plugin that is not installed) stays absent however the profile reads. The
default profile, which an agent has until you set one, restricts nothing, so existing agents
behave as before.

| Field | Values | Effect |
| --- | --- | --- |
| `mcp` | `all` (default), `none`, `named` | Which MCP servers' tools (`mcp__<server>__<tool>`) the agent is offered and may call; `named` uses `mcp_servers`, the servers' names as in the configuration, compared ignoring case. `named` with no name is `none` |
| `plugins` | `all` (default), `none`, `named` | Which plugins the agent may activate (`activate_plugin`) and use the tools of; `named` uses `plugin_names` |
| `local_files_only` | `false` (default), `true` | The lock below; overrides `mcp` and `plugins` |

**Local files only** removes everything that reaches outside the files: the web, browser,
search, media and live-preview tools, MCP servers' and plugins' tools (and `activate_plugin`),
the other integrations (model listing, app-config and session-search tools, suggestions), and
`run_command` with `command_output` and `command_stop`. What remains are the file tools
(`read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`), planning, skills, memory,
`ask_user` and delegation. The file tools are confined to the environment's folders and the
agent's own scratch folder: no path grant, project worktree or attachments folder opens
anything more, the daemon's own configuration and data folders are refused even when an
environment folder contains them (the scratch folder is exempt), and `..`, absolute paths and symbolic links are checked after resolution, like
the folder sandbox itself. Where a read-only grant covers part of a folder it stays read-only.

**Enforcement.** The profile is applied when the tool list is assembled for each model call
(so the model is not told about a hidden tool) and again when a call arrives, so a call to a tool
the profile hides (from a stale list, from memory, or after the profile was tightened during a
turn) is refused with a message naming the profile, never run. The file tools and command
runner check it themselves too, so a local-files-only session cannot run a command or reach
another path even if a call got past the agent loop. **Delegation does not escape it:**
subagents and chunk agents of the agent's session share its profile, and the sessions the agent
starts with `start_session` are bound to the same agent: the binding is recorded with the
session (`agent_id` in its meta file), so it survives a daemon restart, and an agent deleted
since leaves its running sessions under the profile they hold until the daemon restarts. A
changed profile (`environments.agent_update`) applies from the next call of the agent's running
session and of every session it started; the agent is not restarted.

**Servers and plugins a profile forbids do not start.** A session bound to an agent leaves out
the MCP servers its profile refuses (all of them under local files only) before they are
started, the project's own `.mcp.json` included, and does not start eager or restored plugins it
refuses, so an agent that can write a project's `.mcp.json` cannot get a command run by starting a
session. A session whose agent no longer exists keeps the profile recorded with it
(`agent_permissions` in its meta file, kept current while the agent exists); with no record it
gets the strictest profile (local files only, no servers, no plugins), so a session is never
looser than the agent that started it. Switching an agent to local files only also stops the
background commands its sessions had started, at the session's next step (servers that were
already running are left running but unreachable).

**A hard boundary needs local files only.** An agent that may run commands can reach the daemon
like any client (`imperium env agent set …`, or the protocol directly) and loosen its own
profile, so the allowlists alone are a guard against mistakes, not a security boundary: a
command that runs `imperium env agent add|set|rm` always asks a person (the safety floor,
[05](05-tools-and-isolation.md#permissions)), but a script can still call the protocol. Only
**local files only**, which gives the agent no commands, holds against the agent itself. Even
then a locked agent can influence other sessions through what it may edit: files in the
environment's folders that other sessions read (instructions, skills, commands, hooks'
scripts, a project's `.mcp.json`, which sessions of a trusted project start) and the shared
memory. Allowlist names are compared ignoring case, so two servers whose
names differ only by case cannot be told apart.

**Remote runtimes.** For an environment on a Docker, WSL or SSH runtime the folder confinement
is lexical (`..` and absolute paths are resolved, symbolic links inside the runtime cannot be
followed from here), as it is for every session there.

Not covered: the hooks the user configured run as configured (they are the user's, not the
agent's), the hooks a plugin brings still run for a session whose profile says `plugins: none`
(only the plugin's servers and tools are held back), and a mod's panel is not an agent tool, so
profiles do not apply to mods.

From the command line, `imperium env agent add|set` take `--mcp all|none|NAME,NAME`,
`--plugins all|none|NAME,NAME`, and `--local-only` / `--no-local-only`; a flag not given keeps
the agent's current setting, and `imperium env agent list` shows what each agent is limited to.
In the desktop, the agent form has a *Permissions* group (a local-files-only checkbox and an
*All / None / Only these* choice, with names, for MCP servers and for plugins), and a row shows
a short summary of a limited agent.

**Migration and deprecated fields.** An environment saved with the earlier single persistent
session (`persistent_agent: true` with an `agent_session_id`) becomes one agent named "Persistent
agent" the first time the daemon loads it; the session id is kept, so its history and conversation
carry on, and the file is rewritten with the agent. For one protocol minor (0.22) the old fields
remain as aliases and are removed in 0.23: `EnvironmentInfo.persistent_agent` is true while there
is any agent and `agent_session_id` is the first agent's session (with no agent it keeps the
archived session of an agent turned off, which `persistent_agent: true` continues);
`environments.create` or `environments.update` with `persistent_agent: true` adds an agent named
"Persistent agent" when there is none, and `persistent_agent: false` removes only that default agent
(archiving its session, which stays linked): it is refused when the environment has more than one
agent or an agent with a role, so a switch cannot delete a set-up. An environment saved with
`persistent_agent: false` and a session id keeps the id. Clients should use the
`environments.agent_*` methods.

Sessions that a schedule, a project agent, or auto-fix start still work in a single folder and
carry no environment.

### Home (implemented)

**Home** is the one environment that exists without being created: a general-purpose place for
menial work that touches web-hosted files and connectors, not a project. The daemon makes it when
it starts (`id` `home`, named *Home*, with its own house icon), puts it first in
`environments.list` with `builtin: true`, and keeps exactly one however often it restarts. A
store written before Home existed gains it on the next start; nothing else in the store is
touched.

- **Default target.** A session started with neither `environment_id` nor `project_dir`
  (`session.create {}`, `imperium chat` with neither `--env` nor `--project`, and the desktop's
  new-session panel until another environment is chosen) works in Home. This changes `imperium
  chat`, which used to start in the current directory: `imperium chat --project .` does that.
  The chat's first lines say when the session is in Home and how to choose a project, and
  `--worktree` without `--project` or `--env` is an error pointing at `--project`, since Home has
  no project to put on a worktree.
- **No project folders.** Its `folders` are empty. Each session works in its own
  [scratch folder](#environment-working-folders-implemented) (`<root>/home/sessions/<id>/`),
  which is the session's working directory and only folder. The file tools refuse any path
  outside it (and outside the paths the user granted). `run_command` starts there, but a command
  is not confined to a folder: how far it reaches follows the permission mode and the
  [isolation tier](05-tools-and-isolation.md#isolation-tiers) like any session's. Home's
  `Memory.md` is `<root>/home/Memory.md`, read into every Home session like any environment's.
  A user who wants Home's agents to see a project adds its folder (`imperium env add-folder home
  <folder>`, or the environment window), or grants a path in Home's permissions; Home then
  works in that folder like any environment, with the scratch folder beside it. A worktree
  start is refused while Home has no folders.
- **Web files and connectors.** Home's sessions get the same tools as any session: `web_fetch`,
  `web_search` and the browser tools when `[web]` allows them, and the user's MCP connectors.
  They follow the session's [permission mode](05-tools-and-isolation.md#permissions) and the
  safety floor: in the default mode a site not in `web.allow_domains` asks first, and plan mode
  refuses what would ask.
- **No persistent agent.** `persistent_agent` is off, as for any new environment. Nothing runs
  in Home until the user starts a session or explicitly turns the agent on.
- **What can change.** Home can be renamed, given another icon, colour or pet, pinned (it is
  listed first either way), and given folders and permissions. Its runtime stays this machine.
  `environments.delete` on it fails with an error, the CLI and the desktop (no *Delete* in its
  menu or window) do not offer it, and `imperium env list` marks it `built in`.

### Environment working folders (implemented)

Every environment has a **working folder** of its own, apart from its project folders: a place for
the environment's own memory and for each session's scratch files. All of them live under one
folder, `workspaces.root` (`<data>/workspaces` unless the user picks another place, for example
on the drive where their other data is kept):

```text
<workspaces.root>/
  <environment-id>/
    Memory.md                    notes for this environment only (see below)
    sessions/
      <session-id>/              that session's scratch folder
```

- **Session scratch folders.** A session in an environment on this machine gets
  `<root>/<environment-id>/sessions/<session-id>/` when it starts (so only sessions that exist
  have one; nothing is made when an environment is created), and it is added to the session as a
  **writable path grant** through the same workspace as every other path: the permission rules,
  deny rules, and the safety floor apply to it like to any other path, sandboxed commands can
  write to it, and the agent's system prompt names it as its **scratch folder**, to be used for
  temporary files and intermediates instead of the project's folders. Sessions in an environment
  whose tools run in a container, WSL, or over SSH get no scratch folder, since the folder is on
  this machine (they still read the environment's `Memory.md`). Project agents and schedule runs,
  which carry no environment or are not remembered, get none either.
- **Archiving deletes it.** Archiving a session (by hand, when its pull request closes, or after
  inactivity) deletes its scratch folder with everything in it; unarchiving makes a new, empty one.
  Deleting a session deletes it too. Nothing is moved to a trash: the folder is scratch space, and
  what should last belongs in the environment's folders or in memory. Deleting an environment
  leaves its working folder alone (its sessions' folders go with those sessions, and `Memory.md`
  stays until the user deletes it). At start, after sessions were resumed and the agents
  started, the daemon deletes scratch folders whose session no longer exists (the daemon was
  killed before it could clean up, or the session was deleted while it was off). A folder stays
  when its session is running, is an environment's or project's agent, or has a record that is
  not known to be archived: only a missing record, or one that says archived, makes a session
  gone, so a record that cannot be read or parsed, or a session older than `agents.resume_days`
  that is no longer resumed, keeps its folder. Only folders directly under
  `<root>/<environment-id>/sessions/` are touched, with a safe environment name and a generated
  session id (16 hex digits), and never through a link: a symlink or junction as the environment
  folder, as `sessions`, or as the session folder is skipped, and links inside a session folder
  are removed, not followed.
- **`Memory.md`** is the environment's own memory, read into the context of every session in that
  environment and written by agents with the `environment` scope of `memory_remember`
  ([17](17-memory.md#environment-memory-implemented)). It is created on its first write.
- **Moving the folders.** Changing `workspaces.root` (Settings → *Working folders*, `imperium
  config set workspaces.root <path>`, `imperium workspaces move <path>`, or `workspaces.move`)
  moves everything to the new place, in three steps so the originals outlive the setting:
  - *stage*: the data is put at the new place. On the same volume the folder is renamed;
    otherwise the files are copied and every file compared with its original. The new folder
    must be an absolute path, empty or not yet existing, and not inside the current one;
    otherwise the move is refused and nothing changes. A failure removes the copy and leaves the
    originals untouched. A link (symlink or junction) that cannot be recreated at the new place,
    for lack of the privilege, fails the move with an error naming the link;
  - *commit*: `workspaces.root` is saved and the new place recorded. If saving fails, the stage
    is undone (the copy removed, or the folder renamed back);
  - *finalise*: only now are the originals of a copy deleted. A crash before this leaves the data
    in both places, never in neither;
  - `workspaces.move` and `config.set` of `workspaces.root` both run off the connection, so
    `workspaces.progress { phase, files_done, files_total }` reaches every client while the move
    works, before the reply, which lists the files and bytes moved and any warnings (such as an
    old folder that could not be deleted completely after a verified copy);
  - the move is refused while a session that has a scratch folder is working, waiting on the
    user, paused, or watching a pull request, because it holds the folder open. Idle sessions are
    restarted from their journals afterwards (as after a daemon restart) so their scratch folder
    is the new one: the new session is started first and replaces the old only on success, and a
    client subscribed to the session keeps receiving, because the connection follows the session
    of the same id. Starting, archiving, and deleting sessions wait while a move runs;
  - `workspaces.root` set in the config file by hand, or by `IMPERIUM_WORKSPACES__ROOT`, is
    noticed when the daemon starts (the place the folders were last put is recorded in
    `<data>/workspaces-root.txt`) and moved the same way; if that fails, the daemon keeps using
    the old place and `workspaces.where` carries a `warning`. A move through the commands above
    is refused while the environment variable is set, since it would win over the file.

| Method | Params | Result |
| --- | --- | --- |
| `workspaces.where` | `{}` | `{ root, default_root, environments: [{ environment_id, dir, memory_file, memory_exists, session_folders }], warning? }` |
| `workspaces.move` | `{ path }` (absolute; empty is the default) | `{ from, to, method: "nothing" \| "rename" \| "copy", files, bytes, warnings, restarted_sessions }` |
| `workspaces.open` | `{ environment_id, target: "folder" \| "memory" }` | `{}` (opens it with the machine's file manager or editor; `Memory.md` is created empty when missing) |

From the command line: `imperium workspaces where` and `imperium workspaces move <path>`. The
desktop's environment window shows the environment's working folder with *Open Memory.md* and
*Open folder*, and Settings → *Working folders* has the root with a folder picker.

### Planned

An environment will also define:

- **Its configuration layer** (`.imperium/config.toml`), instructions, policy overrides,
  plugins, MCP servers, routing rules, and budgets. Today these come from the working directory
  (the first folder).
- **Narrowed sessions:** a session limited to a subfolder or a read-only mount.
- **Schedules** (below), which are still configured per folder.

## Worktrees

**Implemented: sessions on a worktree.** `session.create { environment_id, worktree: true,
worktree_branch? }` (the **Start in a worktree** switch in the desktop's
[new-session composer](07-desktop-ui.md#environments-and-sessions-implemented), `imperium chat
--worktree [BRANCH]`) starts a session on git worktrees: every folder of the environment inside
a git repository is replaced by a new worktree of that repository on a new branch,
`imperium/<first 8 characters of the session id>` unless a branch is named, made from the
repository's current `HEAD`. Folders outside git are used as they are; a subfolder of a
repository becomes the same subfolder of its worktree. Worktrees go in
`<sessions.worktree_dir>/<repo name>/<branch>` when that setting is set (Settings → Sessions),
and otherwise inside the repository in `.worktrees/<branch>` (slashes in the branch become
`-`), which is added to the repository's `.git/info/exclude` so it is never committed. The
session records its worktrees (`worktrees: [{ folder, path, branch, repo }]` in
`session.list` and `session.create`, a **Worktree** badge in the app), keeps them when it
resumes after a restart, and `session.delete { session_id, remove_worktrees }` (`imperium
session delete <id> --remove-worktrees`) stops and forgets the session and removes its
worktrees (except any that another session still records, which stay; see
[Branching and comparison](#branching-and-comparison)), keeping their branches. Worktrees are made only for local folders (not remote
runtimes). The daemon's git runs with hooks and fsmonitor off (no `post-checkout` hook runs),
and a repository whose own config names the review's commit-time keys (filter drivers or
signing programs; see [refused repository config](#review-pane-implemented)) is refused rather
than checked out with them. Removing a session's worktree looks it up in the repository's own
`git worktree list` (the linked worktree holding the recorded folder), never by asking git
inside the session's folder, whose `.git` the session could have rewritten; when a link stands
at the recorded folder, or between it and its worktree's top, nothing is removed.

**Implemented: project worktrees by default.** An environment's sessions can also use the git
worktrees of the repositories its folders are in, with no folders or grants to add: the main
checkout and every linked worktree, the ones `git worktree list` shows. A folder below the top
of its repository opens the same subfolder of each worktree. They are discovered, never guessed:

- The repository is pinned when the session starts, from the folder's `.git` (a linked
  worktree's `.git` file counts only when its entry in the repository's `worktrees/` names it
  back).
- Discovery reads git's own records (`<repo>/.git/worktrees/<name>/gitdir`) rather than running
  git, so nothing in the repository's configuration runs, and a worktree counts only when its
  own `.git` file points back at that entry. A session can add entries to a repository inside
  its folders, but cannot write a `.git` file where it could not already write, so a forged
  entry opens nothing.
- The list is read again whenever a path falls outside every folder and grant, and before each
  command, so worktrees added while the session runs (by the agent with `git worktree add`, or
  by you) are covered at once and removed ones drop out.

**Access.** Worktrees get the same access as the project folder they come from: environment
folders are writable, so the worktrees are too, for the file tools and for commands under the
OS-sandbox and container tiers (mounted like further folders). Everything else is unchanged:
paths go through the same checks as folders (`..` and links resolved, the deepest folder or
grant decides, so a read-only grant inside a worktree still makes that part read-only), and
deny rules still win. The system prompt names the worktrees beside the environment's folders.

The **Project worktrees** switch in the environment's window (`project_worktrees: false` in
`environments.update`) turns this off for sessions started afterwards. A session started on its
own worktree (above) keeps to its worktrees, so its edits stay apart from your checkout, and
remote runtimes do not get project worktrees.

**Implemented: worktrees for parallel writers** (roadmap item 26). `spawn_agent` takes
`access: "read" | "write"` (default `read`). A **read** subagent shares its delegator's folders,
as before. A **write** subagent works in git worktrees of its own, so writers running at the
same time never edit the same files. A write subagent is refused while the session is in plan
mode, and an `access` other than `read` or `write` is an error.

- **Start.** Every folder of the delegator inside a git repository is replaced by the same place
  in a new worktree of that repository, on a new branch `imperium/<first 8 characters of the
  session id>-<task id>` (`imperium/ab12cd34-t1`) made from the repository's current `HEAD` (the
  delegator's own worktree, if it works on one). Folders outside git stay as they are. The
  writer's file tools, commands, hooks (they run in its working directory) and system prompt
  use the worktrees; its working directory is the first. Everything it may use is its own
  worktrees plus the session's path grants: the [project worktrees](#worktrees) do not open
  for it, `..`, absolute paths and links that lead to the delegator's checkout are refused like
  any path outside the folders, it cannot start worktrees itself, and tools tied to the
  session's own folders (the live preview) are not offered to it. The session's scratch folder
  is shared, so a writer is not told it is its own. Permission rules written for the delegator's
  paths (absolute path patterns) also match the same file in a writer's worktree, which is
  checked as that path; relative patterns are relative to its working directory as usual.
  Worktrees are made like a session's own (above: `<sessions.worktree_dir>/<repo name>/<branch>`
  or the repository's `.worktrees/<branch>`, which is added to the repository's
  `.git/info/exclude`; hooks off; repositories with filter drivers or signing programs
  refused), off the agent's thread. The writer starts from the last commit: uncommitted changes
  in the delegator's checkout are not in the worktree, and the start says so.
- **Outside git** (no folder in a repository, a repository with no commit yet, or a session
  that cannot make worktrees, such as a remote runtime), a writer shares the delegator's
  folders as before, with a notice (a `notice` event, and a note in `spawn_agent`'s result).
  A failure to make or record the worktree is returned to the delegator (nothing is left
  behind), which can start the subagent without `access`. `access` is ignored for
  `single_call` subagents.
- **Result.** When the writer stops working (its report is ready), everything it changed is
  committed on its branch (`add -A`, `commit --no-verify`, never signed, 120 seconds at most,
  with the daemon's git safeguards; a commit identity is supplied if none is configured),
  except files the safety floor covers (`.env`, keys and the like), which stay uncommitted in
  the worktree and are listed. The commit is refused if the worktree is no longer on the
  writer's branch. A rule denying `git_commit`, or plan mode, skips the commit: the changes
  stay in the worktree and the report says so. The report starts with the branch, the
  worktree, the commit and the changed files, then: whether merging the branch into the
  delegator's current branch would be clean or would conflict, and in which files; whether it
  would conflict with the branch of another writer that has commits, and in which files; and
  a caution when the delegator's checkout has uncommitted changes in files the branch also
  changes (all with `git merge-tree` on commits, which changes no checkout). Nothing is merged:
  the delegator (or you) merges with ordinary git, in the review flow above, where the merge
  then shows as the session's changes; conflicts are reported, never resolved by the daemon.
  The daemon never resets, checks out, or edits anything in the delegator's folders for a
  writer; the only things it adds to the repository are the writer's branch, its worktree, and
  the objects git writes to compute the merge outlook.
- **Resume and cleanup.** A finished writer can be resumed (`resume_agent`) and goes back to its
  worktrees with its prompt; its next stop commits again. A writer whose worktrees are gone is
  not resumed (in the delegator's folders instead): resuming it is an error. Writers'
  worktrees are recorded beside the session's journal (`<session>.writers.json`, written whole
  to a temporary file and moved into place; one that cannot be parsed is reported and kept as
  `.corrupt`). Like a session's own, they stay until `session.delete { remove_worktrees: true }`
  removes them (first, before the session's own, with the same looking-up in the repository's
  own `git worktree list`, then `git worktree prune`), keeping every branch; what a writer
  stopped part-way had not committed yet is committed on its branch first, as well as that can
  be done (this does not ask the `git_commit` rule). Without `remove_worktrees` they stay, and
  their branches are in `git branch` and `git worktree list`.

**Planned:** worktree behaviour as explicit configuration, set globally and overridable
per project:

```toml
[worktrees]
mode = "per-session"            # off | per-session | per-writer (each writing subagent gets one)
location = "data"               # data | sibling | in-repo | custom
custom_path = ""                # used when location = "custom"; placeholders: {project}, {repo}, {session}, {task}
branch_prefix = "imperium/"     # branches are <prefix><session-or-task-slug>
base = "default"                # default branch, current branch, or a named ref
cleanup = "on-archive"          # on-archive | on-merge | manual
keep_unmerged = true            # never delete a worktree with unmerged or uncommitted work
max_worktrees = 20              # per project
max_disk_gb = 50                # per project; new worktrees are refused above this
```

| `location` | Path                                                      |
| ---------- | --------------------------------------------------------- |
| `data`     | `<data>/worktrees/<project>/<name>` (default; outside the repo) |
| `sibling`  | `<repo>/../<repo-name>.worktrees/<name>`                  |
| `in-repo`  | `<repo>/.imperium/worktrees/<name>` (added to `.git/info/exclude`) |
| `custom`   | `custom_path` with placeholders expanded                  |

- (Planned, as the settings above are:) A Worktrees pane lists every worktree with its branch, owning session or task, ahead/behind
  counts, dirty state, and disk use, with actions to open, sync with base, merge, or remove.
- Merging a writer's worktree back is an explicit step (by the parent agent under policy, or by
  the user); conflicts are reported, never force-resolved.
- On SSH hosts, worktrees are created on the host using the host's configuration.
- Non-Git projects can use copy-on-write snapshots (where the filesystem supports them) or run
  writers sequentially.

### Checkpoints and rewinding

**Implemented: git-based checkpoints** (roadmap item 6). Checkpoints are kept in git, not in a
snapshot store of their own. Before each root turn, the daemon lists the files of every git
repository the session's folders are in (an environment folder, a session's own worktree, or a
linked worktree, each by its own working tree) and hashes them into the repository's object
store; after the turn it lists them again, and when anything changed it keeps the "before"
state as a commit on a private ref:

- `refs/imperium/checkpoints/<session>/<n>` (`<n>-<k>` for the session's further
  repositories), whose parent is `HEAD` as it was, authored by `Imperium`. Private refs are not
  branches or tags: `git push` does not send them and `git log --all` shows them, which is a
  way to look at one by hand.
- It holds every file git would see: tracked files and untracked files that are not ignored,
  byte for byte (no clean or smudge filters, no line-ending conversion). Ignored files
  (`node_modules/`, build output) and submodules' own files are not in it.
- The repository's index, `HEAD`, and branches are never touched: the commit is built from a
  temporary index of the daemon's own (`<data>/sessions/<id>.checkpoints/index.tmp`) with
  plumbing (`ls-files`, `hash-object --no-filters`, `update-index --index-info`,
  `write-tree`, `commit-tree`, `update-ref`). Like worktree discovery, nothing a repository's
  config names runs: every git call turns hooks and fsmonitor off (`-c
  core.hooksPath=/dev/null`, `NUL` on Windows, and `-c core.fsmonitor=false`), signs nothing,
  uses no pager, prompt, diff driver, or filter, and drops git's own environment variables.
- Its record, `<data>/sessions/<id>.checkpoints/<n>.json`, links it to the journal (its length
  when the turn started) and names the turn's message; `<n>.messages.json` keeps the root
  conversation as it was, so the conversation can be rewound too.
- Unchanged files are not read again (sizes and modification times are remembered), so a turn
  in a large repository costs one listing and the files that changed; the first turn hashes
  every file once. Large untracked files that are not ignored are copied into the repository's
  object store like any other.
- `sessions.checkpoints = false` turns them off. Folders outside git, sessions on a remote
  runtime whose folders are not on this machine, and project-agent sessions have no checkpoints;
  `session.checkpoints` then says why (`available: false, reason`). Deleting a session deletes
  its refs and records.

`session.checkpoints { session_id }` lists them, oldest first (`number`, `at_unix_ms`, `kind`,
`prompt`, `repos: [{ root, commit, reference }]`, `conversation`), and `session.checkpoint`
announces each new one to every client.

**Rewinding.** `session.rewind { session_id, checkpoint, mode }` (`/rewind` in `imperium
chat`) puts a session back, refused while a turn is running. `mode` is one of:

- `files`: every file git would see is put back to the checkpoint: changed and deleted files
  (tracked or untracked) are written back, files added since are removed, along with folders
  they leave empty. Ignored files, the index, `HEAD`, and branches are left as they are, so
  commits made since stay; `git status` shows the rewound files as changes against them. The
  files as they were are first kept as a `rewind` checkpoint, and rewinding to it (files only)
  undoes the rewind.
- `conversation`: the conversation is forked at the checkpoint into a new session (titled
  "… (rewound)", in the same folders, worktrees, and environment) whose transcript and
  conversation end just before that turn's message; the message comes back in the result
  (`prompt`) to edit and send again. The original session is kept as it was, so nothing said is
  lost; archive or delete it once the fork is right. Subagents of the original are not carried
  into the fork.
- `both` (the default): files, then the conversation.

The result lists the files written back and removed, the undo checkpoint, and the new session.

### Branching and comparison

**Implemented** (roadmap item 7). A branch is a new session made from a copy of another's
journal and root conversation, so it continues independently; nothing is merged back.

`session.branch { session_id, at?, title?, worktree? }` (`/branch` in `imperium chat`):

- `at` is the index, in the source's `session.subscribe` history, of any event; absent means the
  session's current point. The turn containing the event is kept whole, so the branch has
  exactly the messages up to the end of that turn. When the event is the user message that
  started a turn, that turn is left out and its text returns as `prompt`, to edit and send
  again; files attached to that message are not carried into the prompt (they stay where they
  are, listed in the original session), so attach them again. A turn that is still running, a point from before a compaction, and a session from
  before the turn index ([agent engine](03-agent-engine.md)) are refused.
- The journal's first events and the saved conversation at that point are copied; the branch
  records `branched_from { session_id, at }` (in its summary and meta file) and is titled
  "… (branch)" unless `title` is given. Environment, folders, permissions and model are the
  source's, and the source may be running or archived (it must be a session the daemon
  knows; an id that is not a plain session id is refused). What belongs to the running session
  rather than its record starts afresh: "always this session" approvals and the permission mode
  reset on a branch. The model, effort, skills and plugins carry over. Only the root conversation is carried:
  subagents that were running are reported as interrupted in the branch, and their kept state is
  not copied. Attachments stay where they are and are referenced by path, not copied.
- A branch shares the source's folders and worktrees unless `worktree` is set. With it, the
  branch gets a git worktree of its own and the files as they were at that point: the "before"
  state of the first later checkpoint of the source, else its files as they are now (no turn
  since changed them), written byte for byte like a rewind. The source's working tree, index,
  HEAD and branches are only read, never changed. It needs the folders to be in a git
  repository on this machine.

`session.compare { session_id, message, models, worktree? }` (`/compare <model> <model> ... --
<message>`): best-of-N. The session must be idle. It is branched at its current point once per
model (two to four, each named once, each usable now, checked up front), each branch is switched
to its model (the next turn fits the conversation to it, as for any model switch) and sent the
message, and they run in parallel as ordinary sessions: agent limits apply, and a local model is
loaded or swapped by the [residency controller](04-models-and-routing.md) like any other
request, never around it. Each branch carries `branched_from` and the shared `comparison_id`.
Branches get a worktree each when the folders are in a git repository (`worktree: false` shares
the folders, which suits read-only questions; shared folders mean the branches edit the same
files). The result lists the branches and their models.

`session.compare_pick { comparison_id, session_id }` (`/pick <n>`) keeps one branch and archives
the others (which stops them; they stay archived, with their `comparison_id`, and can be
unarchived). A comparison that fails part-way (a model that cannot be switched to, a branch that stops)
drops the branches it made, removing only worktrees they created themselves; deleting any
session leaves a worktree another session still works in.

Branches of a comparison can also be told apart by `comparison_id` in
`session.list`, and the desktop's compare view reads each branch's events for its tokens, cost
and time ([07](07-desktop-ui.md#branches-and-model-comparison-implemented)).

Not offered: merging branches, and diffing files across sessions beyond what the review pane
shows.

### Review pane (implemented)

The checkpoints are also what a session's changes are reviewed against
([Review panel](07-desktop-ui.md#review-panel-implemented), `/diff [n]` in `imperium chat`):

- `session.review { session_id, since? }` lists the files the session's turns changed since a
  checkpoint (its first, the session's start, by default), each with its status (`added`,
  `modified`, `deleted`) and unified diff hunks (three lines of context) computed by the daemon
  from the stored contents, so no diff driver or filter runs. Only files a turn changed are
  listed: each `turn` checkpoint records the files its turn changed, so edits you make between
  turns are not taken for the session's. Binary files and files over 2 MiB (sized with
  `cat-file -s` before anything is read) are listed without hunks.
- `session.review_hunk { repo, path, hunk, action }` accepts a hunk (it is kept and no longer
  shown) or rejects it (the hunk is reverted in the working tree; an emptied file the checkpoint
  did not have is removed). Accepting keeps the working tree's file mode, rejecting puts the
  checkpoint's back. `hunk: "file:<id>"`, with the file's `id` from `session.review`, acts on
  the whole file (contents and mode). Hunks are named by a digest of their place and lines, and
  files by a digest of both sides, so a hunk or file that changed since it was shown is
  refused; so is a file no turn of the session changed.
- Accepted hunks build the review's base: each acceptance is a commit on
  `refs/imperium/review/<session>` (`-<k>` for further repositories) whose parent is the base
  before it, so the bases form a chain from the checkpoint. The base belongs to the chosen
  checkpoint: choosing another *since* starts a fresh review from that checkpoint, and the
  acceptances made against the previous one are dropped (the files themselves are unchanged).
- `session.review_commit { repo, message }` commits, on the repository's current branch, every
  file the session changed since the checkpoint as it now is (accepted hunks kept, rejected ones
  reverted, unreviewed ones included), and nothing else you have staged or changed:
  `git add -A -- <files>` and `git commit --no-verify -- <files>` with hooks and fsmonitor off
  and paths taken literally (`GIT_LITERAL_PATHSPECS`), so a file named `[ab].txt` never matches
  `a.txt`. The commit message is prefilled with the session's title (written by its model) and
  the files. If the commit fails, or there turns out to be nothing to commit, the repository's
  index is put back byte for byte as it was before the files were staged, so changes you had
  staged in those files are kept; if that restore itself fails, its error is appended to the
  commit error.
- `session.review_pull_request { repo, title, body, base? }` pushes the current branch (read
  in full from `HEAD`, so a tag of the same name does not confuse it) to `origin` with
  `git push --recurse-submodules=no -u -- origin refs/heads/<branch>` and runs `gh pr create`
  with hooks and fsmonitor off, and submodules never entered (`submodule.recurse=false`,
  `diff.ignoreSubmodules=all`, `push.recurseSubmodules=no`), for the git it starts. A submodule's
  own config therefore never runs, whatever the superproject's `push.recurseSubmodules` says.
  Pushes use network transports only (`https`, `http`, `ssh`, `git`): an `origin` that is a
  local path or a `file://` URL cannot be pushed from the panel, and `ext::` never runs. Without
  `gh` on the PATH it returns the two commands to run instead: the body is written to a file
  in the session's data folder and passed with `--body-file`, and the branch, title, and paths
  are quoted for the shell you are likely to paste into: single quotes for PowerShell on Windows
  (a quote inside doubled, typographic ones too) and for a POSIX shell elsewhere (`'` becomes
  `'\''`), inside which nothing is expanded. A branch (or base) starting with `-` is refused.
- **Refused repository config.** The repository's own config (`.git/config` with its includes,
  or the worktree's config) is checked against two lists. *Commit-time* keys, which `git add`,
  `git commit`, or a checkout would run: `filter.*.clean`, `filter.*.smudge`,
  `filter.*.process`, `gpg.*program`, `gpg.ssh.defaultKeyCommand`, and, for a commit,
  `commit.gpgSign = true` when your global config does not turn signing on. Committing and
  making a session's worktree check only these (a worktree skips the signing check), so a local
  `protocol.version`, credential helper, or `core.hooksPath` (hooks are off anyway) does not block
  them. *Push-time* keys, checked before pushing and opening a pull request, add:
  `core.hooksPath`, `core.sshCommand`, `core.gitProxy`, `core.askPass`, `core.fsmonitor`,
  `core.alternateRefsCommand`, `credential.*`, `remote.*.receivepack`, `remote.*.uploadpack`,
  `remote.*.vcs`, `url.*.insteadOf`, `url.*.pushInsteadOf`, and `protocol.*`. Each would make
  git run a program or reach a place the repository chose; Imperium runs nothing a repository's
  config names, so these are done from a terminal. Your
  global and system config (Git LFS, signing, credential helpers) apply as usual. Every git call
  the daemon makes for checkpoints and the review also sets `GIT_NO_LAZY_FETCH=1` and
  `GIT_ALLOW_PROTOCOL=none` (the push names its own network list), so a partial clone never
  fetches a missing object through a transport its config names.
- Committing and opening a pull request are gated by the permission rules `git_commit` and
  `open_pull_request` ([rules](05-tools-and-isolation.md#rules)): without a rule they return
  `needs_confirmation` until called again with `confirmed: true` (the pane asks), an `allow`
  rule runs them at once, and `deny` refuses them. Accepting, rejecting, committing, and
  opening a pull request are refused while a turn runs.

## Project agent

A long-lived agent per project that watches for work and **dispatches it into new sessions**.
An environment's [persistent agents](#persistent-agents-implemented) use the same machinery,
keyed by agent instead of folder.

**Implemented (phase 6):** `[[projects]]` entries with `agent = true` get an agent when the
daemon starts: an ordinary session (it appears in the session list and can be talked to) with
coordinator instructions and a `start_session` tool. It is woken by messages only: a schedule
with `action.agent`, `imperium project tell "<text>"` (`projects.message`), or a report from a
session it started, delivered as `Session <id> finished. Its report: ...` when that session's
turn ends. It uses no tokens between events. `max_sessions` (default 2) caps sessions running
at once and `max_sessions_per_day` (default 20) per day. Permission requests from the agent and
its sessions, and from scheduled prompt runs, become approval requests (the Approvals panel,
`imperium approvals`). `imperium daemon autostart enable|disable|status` starts `imperium-core` at
login (a per-user `Run` registry value on Windows, a LaunchAgent on macOS, an XDG autostart
entry on Linux). The sessions an agent starts are [background sessions](#quiet-periods): they
rank below yours, wait for a quiet machine before starting (`prefer_quiet`, default on), and
pause while the machine is busy. Not yet: file, Git, and webhook wake-ups, cost budgets, a tray
icon, and notes beyond the agent's memory tools.

- **Toggle per project** (off by default). When off, nothing runs in the background.
- **Runs only while the daemon runs**, i.e. while the computer is on and the user is logged in.
  An optional "start daemon at login" setting (and a tray icon) keeps it available without the
  desktop window open. Nothing runs while the machine is off or asleep.
- **Reactive, not polling a model:** the project agent is woken by events — schedule fires,
  file or Git changes, webhooks, completed sessions, messages from the user or the phone app.
  Between events it consumes no tokens.
- On each wake it decides what to do with the event and either acts directly (small things) or
  **creates a new session** with a brief, which runs under normal routing, budgets, and
  permissions. It receives the session's completion report, not its transcript.
- It keeps a short project memory (notes file and task backlog) so it can resume after restarts.
- Budgets: a daily token/cost cap and a max number of concurrent sessions it may start.
- Permissions are the project's, with one extra rule: actions that would normally prompt the
  user are queued as **approval requests** (shown in the desktop app and phone app) instead of
  blocking a background session indefinitely.

## Schedules

Schedules let tasks run automatically with the flexibility of provider-side scheduled tasks and
routines, but locally.

**Implemented (phase 6, `imperium-automation` and the daemon's scheduler):** `[[schedules]]` in
the user configuration with a `cron` (five fields, local time; names, lists, ranges, steps),
`every` (`90s`, `30m`, `2h`, `1d`), or `at` (a local time) trigger, and a `prompt` (a new
session in `action.project`, with an optional `model`) or `command` (a shell command run there)
action, with `timeout_s` (default one hour). The daemon runs them whether or not a window is
open. Missed runs follow `missed` (`run_once_on_wake`, the default, or `skip`), judged from the
run history in `<data>/schedules/history.json`; `overlap` is `skip` (default) or `allow`. Every
connected client receives `schedule.event` notifications (`started`, `completed`, `failed`,
with the session id and a summary), and `notify = ["os"]` also shows a system notification.
`imperium schedule list` and `imperium schedule run <id>` use the `schedules.list` and
`schedules.run` protocol methods. A prompt run whose session asks for permission waits for an
answer from a client until it times out; approval queues arrive with the project agent. Not
yet: time zones other than local, jitter, file, Git, webhook, and event triggers, the project
agent and hook actions, conditions other than `when_quiet`, project-level schedules, `schedule.*` hook events, and the
desktop Schedules pane.

**Implemented (stage 2g): quiet periods.** A schedule's `prefer_quiet = true` (default `false`)
holds a due run until the machine is quiet, for at most `max_defer` (default `2h`; `90m`, `1d`
also work), then runs it anyway so nothing is postponed forever. The condition
`conditions = { when_quiet = true }` waits for a quiet machine however long it takes. A waiting
run shows as `deferred` in `imperium schedule list` and the `schedules.list` row, clients get a
`schedule.event` with status `deferred`, and `imperium schedule run <id>` (`schedules.run`)
starts it at once; a person's run never waits. While a run waits, ticks that come due join it
instead of starting another, also with `overlap = "allow"` (the daemon logs them as skipped). A
waiting run is not kept across a restart: it was never recorded, so it counts as missed
(`missed`). The other conditions listed under
[Behaviour](#behaviour) (AC power, GPU idle, time window, previous run succeeded) are not
implemented yet.

```toml
[[schedules]]
id = "nightly-audit"
trigger = { cron = "0 2 * * *" }
action = { project = "D:/Git Repositories/Imperium-AI", prompt = "Audit new dependencies." }
prefer_quiet = true
max_defer = "3h"
```

### Quiet periods

Background work should not slow the sessions you are driving. The daemon keeps a **load view**
that reads `quiet`, `busy`, or `saturated`, and the work it starts on its own gives way to you:

- **The load view** samples every `load.sample_s` (5 s): your sessions with a turn running, slots
  in use and calls queued on local models (yours and background calls counted apart), models
  swapped out in the last minute, overall CPU use, and available memory. *Saturated*: a call of
  yours is queued for a local slot, `load.saturated_swaps_per_min` models were swapped out in a
  minute, memory is below `load.saturated_free_ram_mb`, or the CPU is at
  `load.saturated_cpu_pct`. *Busy*: a session of yours has a turn running or a call in flight, or
  the CPU or memory crosses the `busy` thresholds. *Quiet*: none of that. The level rises at once
  and drops only after the machine has looked calmer for `load.settle_s` (60 s). CPU use is not
  counted while a background session is working, because the daemon cannot tell its own load from
  yours (follow-up: subtract the CPU of the daemon's own child processes instead, so a heavy
  build of yours is still noticed while background work runs). Your keyboard and mouse idle time is not used (it needs OS calls this codebase does not
  make); your sessions stand in for it, and GPU utilisation is not polled (`nvidia-smi` runs once,
  at start, for memory); the slot queue and swaps are the GPU signal.
- **Background sessions.** A session started by a schedule's prompt or by a project agent's
  `start_session` is marked `background` (kept with the session, so it survives a restart). In the
  local runtime its calls rank below yours and never swap out a model you used in the last
  `load.interactive_hold_min` minutes, and they wait while the view is `saturated`
  ([04](04-models-and-routing.md#runtimes)). Sessions from auto-fix and ones you start are not
  background.
- **Deferral.** A project agent's `start_session` creates the session at once, but with
  `prefer_quiet` (default `true` for `[[projects]]`) it gets its brief only when the view is
  `quiet`, or after `max_defer`. Until then it is listed as `paused` with a `deferred` hold.
  Sessions waiting still count against `max_sessions`. The waiting brief is kept with the
  session (`deferred_brief` in its `.meta.json`, with the time `max_defer` ends), so after a
  restart the session waits again for what is left of `max_defer`, then gets its brief and
  reports to the project's agent; if the project no longer has an agent, it still runs, with
  nobody to report to. A session released by `max_defer` or by
  *run now* (also a schedule prompt run that waited out its `max_defer`, or one a person started
  with `schedules.run`) does not give way again for the rest of its life: "then run anyway"
  means it runs to the end. This is kept with the session (`run_now`), so a restart does not
  undo it. A message you write to a background session (`session.send`) releases it the same
  way; messages from the daemon (a schedule, a project agent) do not.
- **Yielding mid-run.** A running background session checks the view before each model call. If
  it is `busy` or `saturated`, the call in flight has already finished, so it pauses there: no
  slot held, no tokens spent, like `await_tasks`. It resumes when the view is `quiet`. The pause
  reuses the usage-limit pause (a `paused` event with `cause = "load"` and `until_unix_ms = 0`,
  then `resumed`). Subagents of a background session yield the same way, and a paused agent is
  detached from its local model, so a model you ask for can swap it out without waiting for it; if the model is swapped out
  meanwhile, its slot is not saved and it re-reads its context on resume.
- **Visibility and run now.** Session summaries carry `background` and a `hold`
  (`cause` `deferred` or `load`, and the reason); the desktop shows `Paused · waiting for a quiet
  machine` or `giving way to you`, a `· background` suffix, and the existing **Resume now**
  button; `sessions.control` with `resume_now` does the same. Running now ends the wait and stops
  that session giving way for the rest of its life. The CLI shows deferred schedules and prints a
  line when a session it watches waits. The desktop has no Projects pane or Schedules pane yet,
  so deferred scheduled runs are visible through `imperium schedule list` only.

### Triggers

| Trigger     | Example                                       |
| ----------- | --------------------------------------------- |
| Cron        | `0 8 * * MON-FRI` in a chosen time zone        |
| Interval    | every 30 minutes, with jitter                 |
| One-shot    | at 2026-11-08 15:00                           |
| File watch  | changes under `src/**` (debounced)            |
| Git         | new commits on a branch, new tag, PR comment (via polling or webhook) |
| Webhook     | `POST` to a local, token-protected URL        |
| Event       | any hook event, e.g. `session.end` in another project |

### Actions

| Action           | What runs                                                         |
| ---------------- | ----------------------------------------------------------------- |
| Prompt           | A new session with a stored prompt, model/routing overrides, and budget |
| Project agent    | Delivers the trigger as an event to the project agent             |
| Command          | A shell command inside the project's isolation tier               |
| Hook             | Fires the `schedule.fire` hook event with the schedule payload     |

### Behaviour

- **Missed runs** (computer off or asleep): per schedule, `skip`, `run_once_on_wake` (default),
  or `run_all` (capped).
- **Overlap**: `skip`, `queue`, or `allow` when the previous run is still going.
- **Conditions**: only on AC power, only when a given GPU is idle, only within a time window,
  only if the previous run succeeded, only when the machine is quiet (`when_quiet`, implemented;
  see [quiet periods](#quiet-periods)).
- **Outputs**: each run is a normal session (reviewable later), plus optional notifications to
  the desktop, the phone app, or a webhook.
- Defined in project config (shareable) or user config, editable in a Schedules pane and via
  `imperium schedule add|list|run|pause`.

```toml
[[schedules]]
id = "morning-triage"
trigger = { cron = "0 8 * * MON-FRI", tz = "Europe/London" }
action = { prompt = "Triage new GitHub issues and draft replies; do not post.", budget_usd = 0.25 }
missed = "run_once_on_wake"
notify = ["desktop", "phone"]
```

### Hooks integration

Schedules emit `schedule.fire`, `schedule.complete`, and `schedule.failed` hook events, and
hooks can create, pause, or trigger schedules through the CLI or protocol. A plugin can ship
schedules and hooks together (for example, a "nightly dependency audit" plugin).

## Archiving sessions

**Implemented:** a session can be archived, the way Claude's desktop app keeps its list tidy.
An archived session stops running and is not resumed when the daemon restarts, but its journal
and record are kept: `session.list` still returns it, with `archived: true` and `archived_at`
(milliseconds since the Unix epoch), and `session.subscribe` returns its history with nothing
live to follow. It takes no messages. `session.control` `archive` archives it and `unarchive`
resumes it where it left off; both are announced as `session.updated` (unarchiving also as
`session.created`), and the state is kept in the session's meta file, so it survives restarts.
`session.delete` deletes an archived session as well. From the CLI: `imperium session archive
<id>`, `imperium session unarchive <id>`, and `imperium session list [--archived]`.

Sessions are also archived on their own:

- **When its pull request closes** (`sessions.auto_archive_on_pr_close`, on by default): the
  watcher that follows the pull requests sessions [watch](#github-auto-fix) archives the
  session once the pull request is merged or closed. A merged session still busy is archived
  at a later check, once it is idle; it keeps its `merged` status in the list.
- **After inactivity** (`sessions.auto_archive_after_days`, `0` = off, the default): the daemon
  looks every hour (first a minute after it starts) for sessions whose last journalled event is
  that many days old.

Archiving deletes the session's [scratch folder](#environment-working-folders-implemented);
unarchiving makes a new, empty one.

Neither rule archives a session that is working, waiting on the user (an approval, a usage
limit, a failed turn, a model), has queued messages, is pinned, or is an environment's
[persistent agent](#persistent-agents-implemented) or a [project agent](#project-agent); those
two cannot be archived by hand either.

## GitHub auto-fix

Projects linked to a GitHub repository can react to repository activity automatically: fix a
pull request whose checks fail, answer review comments, triage new issues, and investigate
failed workflow runs.

**Implemented (phase 6, `imperium-github` and the daemon's auto-fix):** with
`github.autofix.enabled`, the daemon polls each `[[github.watch]]` repository (every `poll_s`,
default 120 s) through the `gh` CLI for the signed-in user's open pull requests whose latest
commit has a failed check. For each new failing commit it runs **Fix CI**: it prepares a
worktree of the pull request's branch from the watch's local clone
(`<data>/worktrees/<owner>__<repo>/pr-<n>`, on a local `imperium/fix-pr-<n>` branch), gives a
new session the failed checks and the end of the failed job's log (`gh run view --job
--log-failed`), with file writes, edits, and commands pre-approved inside that worktree
(except what the [safety floor](05-tools-and-isolation.md#permission-modes-implemented)
covers, such as `rm -rf` or `git reset --hard`: those wait in the approval queue for the user),
commits whatever it changed with an `Imperium-Auto-Fix: session <id>` trailer, and, with
`push = "ask"` (the default), queues an approval request; only on approval does it push to the
pull request's branch, as a plain push. It never force-pushes, merges, closes, or approves.
After `max_attempts_per_pr` (default 3) failing commits on one pull request it stops and
says so. Progress goes to every client as `autofix.event` notifications (`started`,
`awaiting_approval`, `pushed`, `declined`, `nothing`, `failed`, `stopped`); approvals are
answered in the desktop app's Approvals panel or with `imperium approvals list|approve|deny`
(`approvals.list` and `approvals.respond`, with `approval.requested` notifications). Not yet:
review, conflict, triage, and workflow-failure responders, `github.*` hook events, webhooks,
labelled or all pull requests, budgets, `comment`, and persisting attempt counts across
restarts.

### Events

Delivered by polling the GitHub API (default, works behind NAT) or by a webhook through the
optional relay ([15](15-mobile-and-remote.md#connectivity)). Each becomes a hook event, so
plugins and user hooks can react too.

| Hook event              | Raised when                                                         |
| ----------------------- | ------------------------------------------------------------------- |
| `github.check.failed`   | A check run or workflow fails on a watched pull request or branch   |
| `github.pr.review`      | A review or review comment arrives on a watched pull request        |
| `github.pr.conflict`    | A watched pull request can no longer merge cleanly                  |
| `github.issue.opened`   | An issue is opened (optionally filtered by label)                   |
| `github.issue.comment`  | Someone comments on a watched issue or mentions the bot handle      |
| `github.workflow.failed` | A workflow run fails on a watched branch (for example `main` or a schedule) |

### Built-in responders

| Responder        | Does                                                                                   |
| ---------------- | -------------------------------------------------------------------------------------- |
| **Fix CI**       | Reads the failed job's logs, reproduces locally where possible, commits a fix to the PR branch in its own worktree, and pushes |
| **Address review** | Applies requested changes, replies to each comment with what changed, and resolves threads it addressed |
| **Resolve conflicts** | Merges the base branch in a worktree, resolves conflicts, runs checks, and pushes |
| **Triage issue** | Labels, asks for missing details, links duplicates, and optionally opens a draft PR with a fix |
| **Workflow failure** | Opens or updates an issue with the failure's cause, or starts Fix CI when it maps to an open PR |

Each responder runs as a normal session (reviewable later, journaled, routed and budgeted like
any other) started by the project agent or directly by the event.

### Configuration and guardrails

```toml
[[github.watch]]
repo = "Densetsu152637/Imperium-AI"
branches = ["main"]                 # workflow failures on these branches
pull_requests = "mine"              # mine | labelled | all
issues = { labels = ["auto-triage"] }

[github.autofix]
enabled = true
responders = ["fix_ci", "address_review", "resolve_conflicts"]
max_attempts_per_pr = 3             # stop and notify after this many failed fixes
budget_usd_per_day = 2.0
push = "ask"                        # ask | auto: ask queues an approval before each push
comment = "auto"                    # replies and issue comments
```

- **Never** force-pushes, merges, closes, or approves; those stay with people. Pushes go to the
  pull request's own branch only, never to protected branches.
- Every push and comment is attributed (commit trailer and a comment footer) and recorded in the
  session journal.
- With `push = "ask"`, the fix waits as an approval request in the desktop and phone apps.
- Repeated failures with the same cause stop after `max_attempts_per_pr` and notify instead of
  looping.
- Authentication uses the `gh` CLI's login or a GitHub App / fine-grained token stored in the
  keychain, scoped to the watched repositories.
