# Local paths

Per-user, machine-specific paths the workflow resolves. Everything in this folder except this README is gitignored, so your paths are never committed.

## How it works

- Each owner guide links the one file it needs and reads it only when its trigger applies. Agents never read this README.
- A missing file means the owner's default rule applies.
- Agents never create or edit these files unless you ask.

## Supported files

| File | Read by | Placeholders | Without the file |
| --- | --- | --- | --- |
| `worktrees.md` | [Worktrees](../guides/git-worktrees.md#parallel-work-and-worktrees), when creating a worktree | `<repository-name>` (basename of the repository's top-level directory), `<task-id>` | Task-ID-specific path outside every other checkout, or the host's managed-worktree path |
| `plan.md` | [Agent pool](../workflows/manager-delegation.md#agent-pool), when setting the session cap | none | Telemetry-discovered plan, else cap 2 |
Configured paths still follow the owner's other rules; for worktrees, the path must stay task-ID-specific and outside every other checkout. In the Claude Code desktop app, keep Settings → Claude Code → worktree location matching `worktrees.md`; agents report a mismatch rather than picking one.

## Setup

Create the file in this folder with the path on its own line, for example `worktrees.md`:

```markdown
# Worktree location

`D:/Git-Worktrees/<repository-name>/<task-id>`
```

PowerShell:

```powershell
Set-Content -Encoding utf8 local-paths/worktrees.md "# Worktree location`n`n``D:/Git-Worktrees/<repository-name>/<task-id>``"
```

Bash:

```bash
printf '# Worktree location\n\n`%s`\n' 'D:/Git-Worktrees/<repository-name>/<task-id>' > local-paths/worktrees.md
```

Plan, for example `plan.md` (a tier from the agent pool table, or `Subagent cap: N`):

```markdown
# Plan

`Max 20x`
```

PowerShell:

```powershell
Set-Content -Encoding utf8 local-paths/plan.md "# Plan`n`n``Max 20x``"
```

Bash:

```bash
printf '# Plan\n\n`%s`\n' 'Max 20x' > local-paths/plan.md
```

## Adding a setting

- One setting per file; keep each file to a couple of lines.
- Link the exact file from the owning guide, at the point the setting is used, with its placeholders and fallback.
- Add a row to the table above.
- Never link this README from agent-facing owners.
