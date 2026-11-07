# Environment configuration and secrets

This guide owns environment wiring, sensitive configuration output, and secret handling. Load [Compose discovery and project identity](container-usage.md#discovery-and-project-identity) before inspecting or changing container configuration.

## Environment wiring

- Discover required variables from the project's documentation and files; do not invent variable names or create runtime configuration as generic setup.
- Compose shell variables and `.env`/`--env-file` values can interpolate the model but do not automatically enter a container: the service must wire them through `environment`, `env_file`, or another mechanism. Service-level `env_file` and CLI `--env-file` have different roles; inspect the project's intended wiring before changing either.

## Configuration output

`docker compose config` may contain resolved credentials. Treat interpolated output as sensitive; keep sensitive values out of logs, chat, and committed files.

## Secret handling

- Keep credentials out of source control, command examples, shell history, logs, and generated artifacts. Use the project's supported local secret mechanism, ignored env file, or managed secret store. Verify ignore rules before creating a local credentials file; never replace a user-owned secret file.
- Commit only sanitized placeholder examples when the project expects them.
- Compose secrets are mounted files distinct from environment variables; do not assume they have the protection of an external production secret manager.

References: [Compose environment variables](https://docs.docker.com/compose/how-tos/environment-variables/), [interpolation](https://docs.docker.com/compose/how-tos/environment-variables/variable-interpolation/), [Compose secrets](https://docs.docker.com/compose/how-tos/use-secrets/), [Docker Engine secrets](https://docs.docker.com/engine/swarm/secrets/).
