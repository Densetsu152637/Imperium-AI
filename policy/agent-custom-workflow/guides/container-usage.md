# Compose-first container usage

This guide owns Compose discovery, project identity, command semantics, and container lifecycle. Configuration/secrets belong to [environment configuration](environment-configuration.md); common validation evidence belongs to [validation](validation.md). Load only for container/Compose work.

## Discovery and project identity

- Inspect the project's Compose files/overrides, Dockerfiles, scripts, Makefiles, and READMEs before running commands; its Compose model is the source of truth. Prefer documented wrapper scripts encoding project-specific options.
- Resolve actual services with `docker compose ... config --services`; never present guessed names as executable defaults.
- Use the modern `docker compose` CLI. Keep the same ordered `-f` files, `--env-file`, profiles, project name, and project directory throughout related operations; do not silently switch project identity or configuration.
- `...`, `SERVICE`, and `COMMAND` here are placeholders; replace them with resolved options, names, and commands.
- Handle configuration output under [environment configuration](environment-configuration.md#configuration-output).

## Lifecycle commands

- Inspect state with `docker compose ... ps`; reuse suitable running services rather than rebuilding or restarting without reason. Diagnose with the same options and `logs --tail <count> SERVICE`.
- Start selected services and dependencies with `up -d SERVICE...`; add `--build` when changed build inputs must be incorporated. `build SERVICE...` alone does not update a running container.
- Bind-mounted source may be immediately visible, while source copied into an image needs rebuild and recreation. `restart` does not apply changed Compose configuration/environment. Inspect mounts/images first.
- Apply [readiness checks](#readiness) before dependent work.
- Use standalone `docker build`/`docker run` for Compose services only for an explicit reason, such as a minimal reproduction; state it and preserve build arguments, mounts, networking, and environment.

## Check execution

- `docker compose ... exec -T SERVICE COMMAND` runs in an existing service (omit `-T` for interactive use); it does not create a fresh environment.
- `docker compose ... run --rm SERVICE COMMAND` gives a fresh one-off environment; verify it has the files, dependencies, and environment the check needs. `run` does not publish service ports by default; `--rm` removes only the one-off container, while volumes/databases may remain shared.
- Apply [shared resource isolation](git-worktrees.md#parallel-work-and-worktrees) before concurrent container checks.

## Cleanup

- Stop only in-scope services/project resources. Prefer `stop SERVICE...` to preserve containers/networks; use `down` only when removing them is intended.
- Do not remove volumes/images or add `-v` as routine cleanup without explicit scope and ownership/data checks. Avoid global prune commands.
- Remove task-created temporary images when in scope and unneeded; preserve unrelated resources.

References: [Compose CLI](https://docs.docker.com/reference/cli/docker/compose/), [up](https://docs.docker.com/reference/cli/docker/compose/up/), [build](https://docs.docker.com/reference/cli/docker/compose/build/), [exec](https://docs.docker.com/reference/cli/docker/compose/exec/), [run](https://docs.docker.com/reference/cli/docker/compose/run/), [ps](https://docs.docker.com/reference/cli/docker/compose/ps/), [logs](https://docs.docker.com/reference/cli/docker/compose/logs/), [down](https://docs.docker.com/reference/cli/docker/compose/down/).

## Readiness

- Compose `up --wait` waits for running/healthy services; health depends on a configured healthcheck and does not by itself prove the application-level condition a check needs.
- Apply [shared readiness and evidence rules](validation.md#shared-validation-rules) before dependent checks.

## Compose validation sequence

1. Complete [Compose discovery](#discovery-and-project-identity).
2. Inspect current state under [lifecycle commands](#lifecycle-commands).
3. Start/update only required services and dependencies, incorporating changed build inputs.
4. Confirm [readiness](#readiness).
5. Choose existing-service or one-off execution under [check execution](#check-execution), applying its isolation rules before parallel checks.
6. Capture outcomes under [shared validation rules](validation.md#shared-validation-rules); on failure, use status/log diagnostics and [configuration-output handling](environment-configuration.md#configuration-output).
7. Clean up only authorized task-created resources under [cleanup](#cleanup).

Readiness reference: [Docker startup order](https://docs.docker.com/compose/how-tos/startup-order/).
