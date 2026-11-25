# Plan: server package (Docker Compose)

Revision: 2026-11-26, revision 1; written against `experimental` at `2cb50590` (last updated).
Roadmap [item 63](../12-roadmap.md#next-in-order). Status: planned (no stage started).

Imperium runs today as one daemon per user on a desktop or an SSH host. This plan packages it to
be self-hosted on a server: a container image of `imperium-core` and the `imperium` CLI for Linux
x64 and arm64, published by the release workflow, and a `docker-compose.yml` that runs the daemon
as a long-lived service with its data, configuration and workspaces on named volumes. Clients
reach it over SSH or an opt-in TLS listener. Running on a server moves the daemon beyond the
user's own machine, so most of this plan is about what must **not** change: no unauthenticated
port, no secret in the compose file or the environment, and no approval that only the desktop
app's own native dialog may give. Each stage below is one pull request of about 800 changed lines
or fewer (eleven stages).

## Goals

Each goal is boolean and has a check that runs in CI. No goal needs a local Docker run, a GPU or
a real model. "The server job" is the CI job built in
[stage 8](#stage-8-ci-server-job): it builds the image, starts the compose stack and drives it.

1. **The image is multi-arch, non-root and read-only.** One image index holds `linux/amd64` and
   `linux/arm64`; its default user is not root; the compose service runs with a read-only root
   filesystem, every capability dropped and `no-new-privileges`. Check: the server job
   (`docker buildx build --platform linux/amd64,linux/arm64`; `docker inspect` shows a non-zero
   `User`; a write to `/usr` inside the running container fails), and stage 9's release job
   inspects the published index.
2. **The stack becomes healthy.** `docker compose up --wait` returns with the daemon healthy, and
   the service has a restart policy. Check: the server job.
3. **Nothing is published beyond loopback by default.** The base compose file publishes no port
   at all; the TLS override publishes only on `127.0.0.1` unless the operator sets another
   address. Check: `deploy/server/check-compose.py` in the server job reads
   `docker compose config --format json` for the base file and for every shipped override and
   fails on a published port whose host address is not loopback, on a missing `read_only`,
   `cap_drop` or `user`, and on a mounted Docker socket.
4. **The SSH path works into the container.** `imperium remote connect <host> --socket
   --server-dir <dir>` from the runner, through the runner's own `sshd`, reaches the daemon in
   the container and completes `system.handshake`. Check: the server job.
5. **The TLS path is authenticated and pinned.** With the TLS override, a client with the token
   and the pin completes the handshake; no token and a wrong token get HTTP 401 and repeated
   failures are slowed; a client with a wrong pin refuses before sending the token; the listener
   refuses to start without a token secret. Check: stage 5 and 6 `cargo test`, and the server job.
6. **One headless task runs against the server.** `imperium --remote ci run --headless --json`
   with the fake model server completes, prints a result whose status is success, and the fake
   records the request. Check: the server job.
7. **Secrets come only from Docker secrets files.** The daemon reads secrets from `/run/secrets`
   read-only; every secret write is refused with `-32010`; no shipped compose file sets a secret
   in `environment` or interpolates one; an agent's command in the default tier cannot read
   `/run/secrets`, the daemon's data folder or its socket folder. Check: stage 2 `cargo test`,
   `check-compose.py`, and the server job's scripted `run_command` (see
   [stage 8](#stage-8-ci-server-job)).
8. **Desktop-only gates stay refused on a server.** On a server daemon,
   `plugins.approval_challenge`, `plugins.approve`, `pull_requests.auto_merge_challenge` and
   `pull_requests.set_auto_merge` with `enabled: true` fail with `-32010`; `mods.install` counts
   no `trusted` or `interactive` grant; approvals recorded earlier on a desktop machine and
   carried over in the data folder still count. Check: stage 1 `cargo test`, and the server job
   calls each method over both remote paths.
9. **The container tier and Docker-launched runtimes are off.** On a server daemon
   `isolation.tier = "container"` and `imperium runtime vllm setup --docker` are refused with a
   message, and no shipped compose file mounts the Docker socket. Check: stage 1 `cargo test`,
   `check-compose.py`.
10. **The image has no known fixable critical or high vulnerability.** Check: the server job's
    Trivy scan (`--severity CRITICAL,HIGH --ignore-unfixed`, exit code 1 on a finding).
11. **Released images are signed and attested.** Each release and experimental build pushes the
    index to GHCR with an SPDX SBOM and SLSA provenance attached, signed keylessly with cosign.
    Check: stage 9's publish job runs `cosign verify` with the workflow's certificate identity and
    `docker buildx imagetools inspect` for the SBOM and provenance after pushing.
12. **Every check runs in CI.** Check: the new checks are the server job, `cargo test` and
    `npm test`; nothing in this plan's acceptance asks for a local Docker run.

## Decisions

These are the design choices this plan makes. Each records why and what was rejected; the
[open questions](#open-questions) are the ones left for the user.

### D1. Image

- **Base: `debian:trixie-slim`, pinned by digest.** Agents run commands in the daemon's
  environment, so the image needs a shell, `git`, `ca-certificates`, `openssh-client` and
  coreutils; a distroless image has none of them. Debian's glibc runs the release binaries
  (built on Ubuntu 22.04's older glibc) and llama.cpp's Linux builds unchanged. Rejected: Alpine
  (musl; llama.cpp's builds and many agent toolchains assume glibc), distroless (no shell for
  agents), Ubuntu (larger for the same packages). Packages beyond the base: `bash`, `git`,
  `openssh-client`, `ca-certificates`, `tini`, `libnss-wrapper`. Users who want toolchains
  (Node, Python, Rust) build `FROM` this image; the docs show how.
- **Binaries from the release artifacts, not compiled in Docker.** The image copies `imperium`
  and `imperium-core` (and the `policy/` and `pets/` resources) out of the
  `imperium-linux-x64.tar.gz` and `imperium-linux-arm64.tar.gz` archives the build workflow
  already makes, picked by `TARGETARCH`. The image then carries the same bits as the CLI archive,
  and arm64 needs no compile under emulation. Rejected: a Rust build stage in the Dockerfile
  (a second release compile per architecture, slow under QEMU). For pull-request CI, stage 8
  builds the x64 binaries in the job and passes them as the build context.
- **User.** A user `imperium` with UID and GID `10001`, home `/home/imperium`. `IMPERIUM_HOME`
  is `/var/lib/imperium`, so config, data and cache are `/var/lib/imperium/{config,data,cache}`.
- **PID 1 is `tini`** (`ENTRYPOINT ["tini", "--", "imperium-core"]`), so model runtimes and
  agents' commands that outlive their parent are reaped and signals reach the daemon whether
  the image runs under Compose or plain `docker run`. `STOPSIGNAL SIGTERM`; stage 1 makes the
  daemon treat SIGTERM like Ctrl+C (it handles only Ctrl+C today, so `docker stop` would kill it
  without the shutdown path).
- **Read-only root filesystem.** The compose service sets `read_only: true`. Writable places are
  the named volumes and a `tmpfs` at `/tmp` (1 GiB, mode 1777). Nothing the daemon writes lives
  elsewhere once `IMPERIUM_HOME` and `HOME` point at volumes.
- **Multi-arch.** `linux/amd64` and `linux/arm64`, each built on a native runner
  (`ubuntu-22.04` and `ubuntu-22.04-arm`, the runners the bundles use) and pushed by digest, then
  joined into one index with `docker buildx imagetools create`.
- **Pinned digests.** The base image in the Dockerfile, the llama.cpp and vLLM images in the
  compose overrides, the fake model server's base in CI and every action are pinned by digest or
  commit SHA. The repository's `docker-compose.yml` names the image by version tag; the copy
  attached to each release names it by tag **and** digest, written by the publish job, so a
  release's compose file pulls exactly what was signed.
- **SBOM and provenance.** BuildKit's own attestations: an SPDX SBOM (`--sbom=true`) and SLSA
  provenance (`--provenance=mode=max`), attached to the index in the registry. Rejected: a
  separate SBOM tool (Syft) uploading a file beside the image, which can drift from the image it
  describes.
- **Signing.** cosign keyless signing with the workflow's GitHub OIDC identity (Sigstore Fulcio
  and Rekor), over the index digest; the docs give the `cosign verify` command with
  `--certificate-identity` pinned to the release workflow and `--certificate-oidc-issuer
  https://token.actions.githubusercontent.com`. Rejected: a long-lived signing key (a secret to
  guard and rotate), Docker Content Trust (Notary v1, deprecated). GitHub's
  `actions/attest-build-provenance` was considered; BuildKit provenance plus cosign covers the
  same need with tools any registry client understands, so only one is used.
- **Registry: GHCR, `ghcr.io/densetsu152637/imperium-server`.** The repository is on GitHub, the
  publish job can push with `GITHUB_TOKEN` and `packages: write` (no extra secret), pulls are
  anonymous for a public package, and the package links to the repository and its releases.
  Rejected: Docker Hub (a second account and a stored token, pull rate limits). Tags: `X.Y.Z` and
  `latest` from a main release, `X.Y.Z-beta.N` from a main pre-release, and `experimental` plus
  `X.Y.<run>-experimental` from the experimental channel ([11](../11-ci-cd.md#release-channels)).

### D2. The daemon in a container

- **Shared lifetime.** The entrypoint starts `imperium-core` directly, which
  [02](../02-architecture.md#shared-daemon-lifetime-implemented) already treats as a
  **persistent** daemon: it never exits for want of clients, so no lifetime code changes. The
  image sets `IMPERIUM_NO_DAEMON_SPAWN=1`, so an `imperium` command run in the container
  (`docker compose exec imperium imperium ...`, the healthcheck) attaches and never starts a
  second daemon; the single-instance lock on `<data>/daemon.lock` stays the arbiter. A restart
  leaves a stale `daemon.json` and socket on the volumes, which the next start already replaces.
  Sessions resume from their journals as after any restart.
- **Restart policy** `unless-stopped`. Docker without Swarm does not restart an unhealthy
  container; the healthcheck is for `up --wait`, `depends_on: service_healthy` and monitoring.
  A crash is covered by the restart policy.
- **Healthcheck: `imperium ping`, run inside the container.** It reads `daemon.json`, connects on
  the local socket with the token, completes the handshake and `system.ping`, and exits non-zero
  otherwise; it never starts a daemon. Interval 30 s, timeout 5 s, start period 60 s, retries 3;
  declared in the Dockerfile (`HEALTHCHECK`) and in the compose file. Rejected: an HTTP
  `/healthz` route, which would be an unauthenticated endpoint on a listener (goal 3 and the
  spec's "never an unauthenticated port"); a TCP probe, which proves only that a port is open.
- **Volumes.** Named volumes, so a backup is a volume backup and nothing depends on host paths:

  | Volume | Mounted at | Holds |
  | --- | --- | --- |
  | `imperium-config` | `/var/lib/imperium/config` | `config.toml`, `models.toml`, agents, skills |
  | `imperium-data` | `/var/lib/imperium/data` | sessions, journals, ledger, plugins, runtimes, TLS key |
  | `imperium-cache` | `/var/lib/imperium/cache` | caches (safe to drop) |
  | `imperium-workspaces` | `/workspaces` | `workspaces.root` (set by `IMPERIUM_WORKSPACES__ROOT`) |
  | `imperium-models` | `/models` | `models.folders`; shared read-only with the model services |
  | `imperium-home` | `/home/imperium` | `HOME`: git and tool settings |

  The socket folder is the one bind mount (below, [D3](#d3-remote-access)).
- **UID and GID.** The service runs as `user: "${IMPERIUM_UID:-10001}:${IMPERIUM_GID:-10001}"`.
  A different UID is needed for bind-mounted host folders and for the SSH path (the host account
  must own the socket). Named volumes are created owned by `10001`, so a one-shot `prepare`
  service runs first (`depends_on: condition: service_completed_successfully`): the same image,
  as root, with every capability dropped except `CHOWN`, `FOWNER` and `DAC_OVERRIDE`,
  `network_mode: none`, a read-only root, running a fixed script
  (`/usr/local/lib/imperium/prepare-volumes`) that changes the owner of each mounted volume and
  of the socket folder only when it differs, never follows a symbolic link (`chown -R -P -h`),
  and sets the socket folder to `0700`. It runs no Imperium code. With the default UID it
  changes nothing. Rejected: starting the daemon itself as root and dropping privileges (root in
  the long-lived process for the length of start-up, and a privilege-dropping path to get right);
  requiring a rebuilt image per UID.
- **An arbitrary UID has no `/etc/passwd` entry** on a read-only root, which breaks `ssh` and some
  git calls. The entrypoint writes `/tmp/passwd` and `/tmp/group` for the running UID and sets
  `NSS_WRAPPER_PASSWD`, `NSS_WRAPPER_GROUP` and `LD_PRELOAD=libnss_wrapper.so`, only when the UID
  has no entry. Stage 7 tests `id -un` and `ssh -V` under UID 1001.

### D3. Remote access

- **Loopback only by default.** The base compose file publishes nothing. The daemon's WebSocket
  stays on `127.0.0.1` **inside the container's network namespace**, which nothing outside the
  container reaches. The two ways in are below; there is never an unauthenticated port.
- **The SSH path, into a container.** Today `imperium remote connect <host> --socket` runs
  `imperium daemon endpoint` on the host over SSH and forwards `<data>/ipc/daemon.sock`. On a
  server the daemon is in a container, `imperium` is usually not installed on the host, and the
  socket is inside a volume. So:
  - A new setting `daemon.socket_dir` (default `<data>/ipc`; the image sets `/run/imperium`) moves
    the socket folder; the compose file bind-mounts it from a host folder,
    `${IMPERIUM_SOCKET_DIR:-./run}`, prepared `0700` and owned by `IMPERIUM_UID`. The daemon keeps
    today's checks there (folder `0700`, socket `0600`, a stale socket replaced, anything else
    refused) and adds two: the folder must be owned by the daemon's UID and not be a symbolic
    link.
  - In that folder the daemon also writes `endpoint.json` (mode `0600`): the same
    `DaemonEndpoint` as `daemon.json`, with `socket` naming `daemon.sock` relative to the folder.
  - `imperium remote connect <host> --socket --server-dir <host folder>` reads
    `<folder>/endpoint.json` with `cat` over SSH (no `imperium` on the host; the folder is passed
    as one quoted argument and must be absolute), and forwards `<folder>/daemon.sock` as today
    (`StreamLocalBindMask=0177`). The command never starts anything on the host: a missing file
    says to start the stack.
  - **Who may connect:** the SSH account must have the socket's UID, because the socket stays
    `0600` (never loosened to a group). That is the reason for `IMPERIUM_UID`. Rejected: an
    `sshd` inside the container (a second remote service, its own keys and hardening);
    `docker exec` over SSH (the SSH account would need the Docker group, which is root on the
    host).
  - The per-launch token changes when the container restarts; `remote connect` reads it again,
    as for any host today.
- **The opt-in TLS listener.** Off unless `remote.tls.listen` is set. `docker-compose.tls.yml`
  sets it to `0.0.0.0:7443` inside the container and publishes
  `${IMPERIUM_TLS_BIND:-127.0.0.1}:7443:7443`. Two opt-ins are needed to expose it beyond the
  host: the override and a non-loopback `IMPERIUM_TLS_BIND`. Inside the daemon, a non-loopback
  `remote.tls.listen` also needs `remote.tls.allow_non_loopback = true` (as
  `api.openai_compatible.allow_lan` does), which the override sets because the container's own
  interface is not the host's.
  - **Token.** The listener requires `Authorization: Bearer <token>` (or the `imperium.token.<t>`
    subprotocol) on the upgrade, compared in constant time. The token is the Docker secret
    `imperium-remote-token`, at least 32 bytes of random data (shorter is refused), stable across
    restarts and separate from the daemon's per-launch local token. Without it the listener does
    not start and the daemon logs why. Reason for a separate token: the local token changes at
    every launch and sits in `daemon.json`; a remote client needs a stable credential the operator
    can rotate by replacing one file, and a leak of either does not give the other.
  - **Certificate.** Operator-provided first: the Docker secrets `imperium-tls-cert` and
    `imperium-tls-key` (PEM). Otherwise the daemon generates a P-256 key and a self-signed
    certificate on first start into `<data>/tls/` (key `0600`), keeps it across restarts, and
    regenerates only if the files are deleted. TLS 1.3 only (our clients are the only clients),
    rustls (already in the tree through `reqwest`), no client certificates.
  - **Pinning.** The pin is the SHA-256 of the certificate's SubjectPublicKeyInfo, written
    `sha256/<base64>`. `imperium server info`, run in the container, prints the listener's address
    and pin. A client verifies a certificate either through the platform verifier (an operator
    certificate from a public CA) **or** against a pin; a self-signed certificate is accepted only
    with a matching pin, given when the remote is added. There is no trust on first use: a client
    never records a pin it was not given. A pin survives certificate renewal while the key is
    kept.
  - **Rate limits and caps.** Per source address: at most 5 failed upgrades a minute, then each
    further attempt waits double the last delay up to 15 minutes; at most 16 TLS handshakes a
    second. Overall: at most 64 open connections, 10 s to finish TLS and the upgrade, and a global
    ceiling of 60 failures a minute. Behind Docker's userland proxy every client may show the same
    source address, so the per-address limit can act as a global one; the docs say so. The
    256-bit token makes guessing hopeless; the limits protect the daemon's resources.
  - **Paths.** The listener serves only `/rpc`. It does not serve the phone web app, `/pair` or
    the OpenAI-compatible API.
  - **What it is not.** A TLS connection is not the local socket, so the calls the daemon takes
    only there stay refused over it: evaluation runs (`session.eval_create`, so
    `imperium run --headless` needs the SSH path) as well as the desktop-only gates of
    [D5](#d5-features-unavailable-on-a-server).
- **How clients connect and pin.**
  - CLI over SSH: `imperium remote connect <host> --socket --server-dir <dir>`, then
    `imperium --remote <name> ...`, as today.
  - CLI over TLS: `imperium remote add <name> wss://<host>:7443 --pin sha256/<b64>` reads the
    token from standard input (never an argument) and stores it in the **client's** secret store
    as `remote.<name>.token`; the record in `<data>/remotes/<name>.json` holds the URL and pin,
    not the token. `imperium --remote <name> ...` then connects directly with the pinned
    verifier in `imperium-client`.
  - Desktop app: Settings → Servers adds an SSH server (host and socket folder) or a TLS server
    (URL, pin, token). The webview cannot pin a certificate, so the app's Rust side runs the
    connection: for TLS a local forwarder from a loopback port (a `0600` Unix socket where the
    platform has one) to the pinned `wss://` endpoint; for SSH the same `ssh -L` the CLI uses.
    The webview connects to the local end with the remote token, as to any recorded remote. When
    the handshake says `server: true` the app hides the desktop-only controls
    ([D5](#d5-features-unavailable-on-a-server)) and shows a "server" badge.

### D4. Secrets backend

- **A read-only directory store.** A new `SecretStore` backend, `DirectorySecrets`, reads the
  secret named `<name>` from the file `<dir>/<name>`. It is chosen by configuration:
  `secrets.store = "directory"` and `secrets.directory` (default `/run/secrets`), read only from
  the user layer and environment variables, never from a project's files. The image sets
  `IMPERIUM_SECRETS__STORE=directory`; that variable names a backend and holds no secret. The
  daemon records `directory` in `secrets.backend`, and `imperium secret` follows the record.
  There is no fallback: a name with no file is "not stored", never looked up in a keychain or
  `secrets.enc`.
- **Reading.** A secret name is validated as today (1 to 64 of `a-z0-9._-`), so it cannot name a
  path outside the folder. The file must be a regular file and not a symbolic link (opened with
  `O_NOFOLLOW`), at most 64 KiB; one trailing newline is removed. The file is read at each load,
  so replacing a secret and restarting the stack is enough. The folder is never listed into a
  log, and errors name the secret, never the content.
- **Writes are refused, never redirected.** `store` and `delete` return a new
  `SecretError::ReadOnly`, which the protocol reports as `-32010` with
  `{feature: "secret_write", secret: <name>}` and the text "secrets on this server are Docker
  secrets: add the file `<name>` to the stack's secrets and restart". This covers `secret.set`,
  `secret.delete`, `imperium secret set|delete`, `plugins.set_secret`, `plugins.delete_secret`,
  `plugins.uninstall` with `delete_secrets` (the plugin is uninstalled and the result says the
  secrets were left), and `api.token` with rotate (`api.token` show reads
  `/run/secrets/api.openai-token`; a missing file leaves the API off rather than minting one).
- **MCP OAuth on a server is refused.** `mcp.login` needs to store and refresh tokens, and its
  loopback redirect cannot reach a browser elsewhere. It returns `-32010`
  (`{feature: "mcp_login"}`); a remote MCP server that takes a static token gets it through
  `secret_env` or headers from a Docker secret. A writable store for tokens the daemon itself
  mints is an [open question](#open-questions).
- **GitHub.** The daemon's GitHub token comes from `gh auth token` today. A new setting
  `github.token_source` (`gh`, the default, or `secret`) selects a `TokenSource` that reads the
  secret `github.token`; the image sets `secret`. This is the seam the
  [pull-request card plan](pr-card.md#decisions) (D1) left for a stored token. Agents' commands
  still never receive it.
- **Compose wiring.** Each secret is declared once at the top level as `file: ./secrets/<name>`
  and listed under the service; the docs give `umask 077` and `openssl rand -hex 32` for the
  remote token. Compose file-based secrets are bind mounts that keep the host file's owner and
  mode (the long-form `uid`, `gid` and `mode` apply only under Swarm), so the files must be
  readable by `IMPERIUM_UID` and by nobody else on the host. `check-compose.py` refuses a shipped
  file that sets a secret-looking key in `environment` or interpolates `${...}` into one.

### D5. Features unavailable on a server

**The mechanism: on a server there is no installed-app identity, so these stay refused.** Every
desktop-only approval is decided by the daemon from what the operating system says about the
process on the other end of the local socket ([06](../06-extensibility.md#who-may-grant-trust-and-approval-implemented)):
it must be the `imperium-desktop` installed beside `imperium-core`, then pass the OS presence
check. In the image none of that can hold, for reasons that do not depend on configuration:

- the image ships no `imperium-desktop`, and the read-only root filesystem means none can be put
  beside `/usr/local/bin/imperium-core` at run time (stage 7's image test asserts the file is
  absent);
- there is no desktop session, so the presence check (polkit, Windows Hello, Touch ID) is never
  available;
- a connection over the TLS listener has no peer process at all; over the SSH-forwarded socket
  the peer is `sshd`, which is already refused; through `docker exec` the peer's parent is outside
  the container's PID namespace (parent PID 0 while not PID 1), which stage 1 adds as a refusal
  to the ancestry walk on Linux, so the walk fails closed instead of ending early as it does
  today for a missing parent.

On top of that structural refusal, a **server mode** gives clear errors early and tells clients:
the setting `daemon.server` (set by the image as `IMPERIUM_DAEMON__SERVER=true`; environment
variables are the last layer, so a `config.toml` on the volume cannot unset it) makes the daemon
answer these with `-32010` (`UNAVAILABLE_ON_SERVER`) and a `feature` in the error's data, and
report `server: true` in the handshake. Server mode only ever removes capabilities; turning it
off grants nothing, because the structural checks above still fail.

| Feature | On a server | `feature` |
| --- | --- | --- |
| Plugin approval (`plugins.approval_challenge`, `plugins.approve`) | refused | `plugin_approval` |
| Presence check | never raised (follows from the row above) | n/a |
| Enabling GitHub auto-merge (`pull_requests.auto_merge_challenge`, `set_auto_merge` with `enabled: true`) | refused; disabling still works from any client | `auto_merge_enable` |
| Terminal grants (`mods.install` `trusted` and `interactive`) | `trusted` is an error, `interactive` is dropped with a note, as when the check fails today | `terminal_grant` |
| Secret writes, `mcp.login`, `api.token` rotate | refused ([D4](#d4-secrets-backend)) | `secret_write`, `mcp_login` |
| `isolation.tier = "container"` | refused at config validation and at session start | `container_tier` |
| `imperium runtime vllm setup --docker` and Docker-mode vLLM starts | refused (would need the Docker socket) | `vllm_docker` |

**Carried-over approvals keep working.** Plugin approval records already in `<data>` (a data
folder restored from a desktop machine) are verified as today by the plugin's hash and run
summary, and count; stage 1 tests that loading a record does not re-run the desktop identity
check. Nothing on the server creates a new one. Auto-merge already enabled on GitHub stays
enabled, and the poll still turns it off if the head moves.

### D6. GPU services

- **Default: separate, optional compose services, no Docker socket.** `docker-compose.llamacpp.yml`
  adds `llamacpp` from ggml-org's `server-cuda` image at the ref pinned in
  `runtimes/llama-cpp.env`, and `docker-compose.vllm.yml` adds `vllm` from the image pinned in
  `runtimes/vllm.env` (`vllm/vllm-openai:v0.11.0` today), each by digest, each with
  `deploy.resources.reservations.devices: [{driver: nvidia, count: ${GPU_COUNT:-1},
  capabilities: [gpu]}]`, the `imperium-models` volume read-only and the model chosen by
  `LLAMACPP_MODEL` or `VLLM_MODEL`. They join an `internal: true` network shared only with the
  daemon, publish no port, and the override registers them in the daemon as endpoints through the
  environment layer (`IMPERIUM_ENDPOINTS__LLAMACPP__BASE_URL=http://llamacpp:8080/v1`). vLLM gets
  `shm_size` instead of `ipc: host`. A CI check fails when the llama.cpp image's ref differs from
  `LLAMA_CPP_REF` or the vLLM tag from `VLLM_DOCKER_IMAGE`.
- **Trade-off.** These are static endpoints: one model each, loaded at service start, not
  managed by the residency controller. Dynamic loading needs the daemon to start runtimes:
  - **Sibling containers through the Docker socket** (what `vllm setup --docker` does on a
    desktop) would hand the daemon, and so any agent that reaches it, root on the host. Rejected
    for every shipped file; server mode refuses it (goal 9).
  - **Runtimes inside the daemon's image** keep the residency controller: the daemon installs
    llama.cpp's CPU build into `<data>/runtimes` as on any Linux host (verified by digest, as
    today) and runs GGUF models from `/models` as children. That works by default on the CPU.
    A GPU in the daemon's container (the same reservation on the `imperium` service, in
    `docker-compose.gpu.yml`) is offered as an opt-in only: llama.cpp's Linux release builds that
    the installer picks are CPU and Vulkan, and Vulkan through the NVIDIA Container Toolkit is
    not verified in CI. The docs say so.
- No GPU job runs in CI; the server job only renders each override with `docker compose config`
  and runs `check-compose.py` on it.

### D7. Sandbox tiers inside the container

- **The problem.** In tier 0 an agent's command runs as the daemon's UID with the daemon's view,
  so it can read `/run/secrets/*`, `daemon.json` (the local token) and the TLS key. On a desktop
  the keychain stands between a command and most secrets; in a container a file does not. The
  server's default tier must therefore hide those paths from commands.
- **`os` (tier 1) with bubblewrap does not run under Docker's defaults.** bubblewrap needs
  `unshare`, `clone` with namespace flags, `mount` and `pivot_root`, which Docker's default
  seccomp profile allows only with `CAP_SYS_ADMIN`, and the default AppArmor profile denies
  `mount`. Making it work means a custom seccomp profile and `apparmor=unconfined` (or a host
  AppArmor profile), which widens the host kernel surface reachable from the container. Rejected
  as the default; documented as not supported.
- **Chosen: a Landlock backend for tier 1.** Landlock lets an unprivileged process restrict its
  own file access with no namespace and no mount. A new `isolation.os_backend` (`bwrap`, the
  default and today's behaviour, or `landlock`) selects it; the image sets `landlock` and
  `IMPERIUM_ISOLATION__TIER=os`. In the child, before `exec`, the runner sets `no_new_privs` and
  applies a ruleset that allows:
  - read and execute: `/usr`, `/bin`, `/sbin`, `/lib`, `/lib64`, `/etc`, `/opt`, `/home/imperium`
    (read-only, so a command cannot plant git or shell settings that the daemon's own
    processes later read), `/proc`, and `/dev` device files;
  - read and write: the session's folders and writable grants, and a per-command temporary
    folder under `/tmp` (set as `TMPDIR`);
  - nothing else: `/var/lib/imperium` (config, data, cache, TLS key), `/run/secrets` and the
    socket folder are unreachable.
  Landlock also stops a sandboxed process from tracing or reading the memory of the daemon (a
  process outside its domain). Network: with `isolation.network = false`, Landlock ABI 4 denies
  TCP `connect` and `bind` (kernel 6.7 or later); UDP is not covered, and the session label says
  "TCP blocked" rather than "no network". If the kernel lacks Landlock (or the ABI a setting
  needs), commands **fail closed** with a message naming the two choices: a host with Landlock,
  or `isolation.tier = "policy"` set knowingly (which the docs describe as giving agents' commands
  every secret).
- **Assumption to verify first.** Docker's default seccomp profile allows the three Landlock
  system calls. Stage 3's first CI step checks it on the runner; if it does not, the stack ships
  a seccomp profile that is Docker's default plus those three calls, a much narrower change than
  bubblewrap's.
- **`container` (tier 2) is off.** It needs Docker inside the container (Docker-in-Docker, which
  is privileged) or the host's socket (root on the host). Server mode refuses it (D5).
- **What tier 1 does not confine.** The daemon's own children (MCP servers, hooks, model
  runtimes) run outside the Landlock domain, as they run outside bubblewrap today; they are
  user-configured, and a project's own servers start only in `mcp.trusted_projects`. The daemon's
  git in agent-writable repositories goes through `safe_git` (no hooks, fsmonitor or filters).

### D8. Testing in CI

- **A `server` job in `ci.yml`**, run when `detect.sh` reports `server` changes (`deploy/server/**`,
  `crates/**`, `Cargo.lock`, the workflow), with an always-run gate so a skipped job is not left
  pending. It builds x64 release binaries, builds the image (and the arm64 image under QEMU,
  build only), starts the stack, and drives it ([stage 8](#stage-8-ci-server-job)). Docker in a
  hosted runner is fine; **nothing in this plan needs a local Docker run**, and no local check
  stands in for the job.
- **The fake model server** is `deploy/server/ci/fake-model.py`: Python's standard library only,
  an OpenAI-compatible `/v1/chat/completions` that replays a scripted list of responses (one
  `run_command` tool call, then a final answer) and records each request. It runs as a compose
  service in the CI override from a digest-pinned `python:3.12-slim`. No real model is called.
- **Vulnerability scan:** Trivy on the built image, pinned by commit SHA, `--severity
  CRITICAL,HIGH --ignore-unfixed`, failing the job on a finding. Exceptions go in
  `deploy/server/.trivyignore` with a reason and a review date each.

### D9. Protocol

One minor at merge of [stage 1](#stage-1-server-mode-and-refusals) (0.61 if nothing else lands
first): `HandshakeResult.server` and the `-32010` error. Later stages change files, not the wire:
`DaemonEndpoint` gains an optional `pin` for recorded remotes. See
[protocol additions](#protocol-additions).

## Current state (found on `experimental` at `2cb50590`)

Implemented:

- **Daemon lifetime and discovery.** A daemon started on its own is persistent; clients hold
  leases; `daemon.json` and the local socket (`<data>/ipc/daemon.sock`, `0700` folder, `0600`
  socket) are on by default; the WebSocket is on `127.0.0.1` with a per-launch token
  ([02](../02-architecture.md#protocol)). Shutdown runs on Ctrl+C or `system.shutdown` only
  (`crates/imperium-core/src/main.rs`); SIGTERM is not handled.
- **SSH remotes.** `crates/imperium-cli/src/remote.rs`: `remote connect` runs
  `imperium daemon endpoint` (starting the host's daemon if needed) and forwards the port or,
  with `--socket`, the Unix socket; the endpoint is recorded in `<data>/remotes/<name>.json`.
- **Other listeners.** The phone listener (`phone.bind`, plaintext, paired device tokens with
  scopes, [15](../15-mobile-and-remote.md)) and the OpenAI-compatible API (`allow_lan` opt-in,
  its own token in the secret store). Neither is TLS.
- **Secrets.** `SecretStore` (`load`, `store`, `delete`) with the keychain and, on Linux without
  one, the encrypted `secrets.enc` ([10](../10-configuration.md#encrypted-file-fallback-implemented)).
  Both are writable; nothing reads a folder of plain files.
- **Desktop-only gates.** `app_identity` decides plugin approvals and the auto-merge challenge
  from the local-socket peer; `mods.install` grants walk the peer's ancestry and refuse `sshd`
  and the `IMPERIUM_SPAWNED_BY_DAEMON` mark; a missing parent ends the walk.
- **Isolation.** Tiers `policy`, `os` (bubblewrap on Linux, Seatbelt on macOS, with masks for
  the daemon's folders) and `container` (`docker run`) in `crates/imperium-tools/src/sandbox.rs`.
- **Runtimes.** llama.cpp release builds installed into `<data>/runtimes`; vLLM in a managed `uv`
  environment or, with `--docker`, sibling containers started through the Docker CLI
  ([04](../04-models-and-routing.md#runtimes)).
- **TLS crates.** `rustls` 0.23 and `tokio-rustls` are in `Cargo.lock` through `reqwest` 0.13.
- **Release.** `build.yml` makes `imperium-linux-{x64,arm64}.tar.gz` with `imperium`,
  `imperium-core`, `policy/` and `pets/`; `release.yml` and `experimental.yml` publish GitHub
  releases. No workflow touches a container registry.

Missing: any Dockerfile, compose file or image; a read-only secret store; a TLS listener and a
pinned TLS client; server mode; a socket folder outside the data folder; a tier-1 backend that
runs in a container.

## Scope and non-goals

In scope: the image, the compose files, server mode and its refusals, the directory secret
store, the Landlock tier-1 backend, the socket folder and the SSH path into a container, the TLS
listener and pinned clients (CLI and desktop), the CI server job, publishing to GHCR, and the
docs.

Not in scope:

- Kubernetes manifests or a Helm chart, Docker Swarm, Podman-specific files (the image should run
  under Podman; nothing is tested there).
- Multi-user or multi-tenant servers: one daemon serves one user, as on a desktop.
- The phone over the TLS listener, Noise pairing, the relay ([15](../15-mobile-and-remote.md)).
- A writable runtime secret store (MCP OAuth on a server) and giving agents' commands git
  credentials on a server ([open questions](#open-questions)).
- Installing `imperium-core` on a plain SSH host (the existing "not yet" of
  [05](../05-tools-and-isolation.md#ssh-hosts)).
- GPU tests in CI, AMD and Intel GPU overrides (documented as user-edited variants).
- Automatic certificates (ACME); an operator who wants a public CA certificate supplies it.

Off-limits paths owned by another session, which **no stage needs**:
`crates/imperium-engine/src/agent/{tools.rs,turn.rs,compaction.rs,prompts.rs}`,
`crates/imperium-engine/src/compact.rs`, `crates/imperium-core/src/eval*`,
`crates/imperium-models/src/chat.rs` and `crates/imperium-memory/`. The tier-1 backend lives in
`imperium-tools`; the headless run is only driven, not changed. If a stage finds it must touch
one of these, it stops and reports before editing.

## Threat model

Assets: the user's secrets (model API keys, the GitHub token, plugin secrets, the remote token,
the TLS key), the code and data in the workspaces, the host the container runs on, the GPUs and
the model budget, and the authority to approve code-running plugins and to merge.

Attackers: anyone who can reach a published port; a prompt-injected agent (through repository
content, web pages, MCP results, CI logs) running commands inside the container; another local
user on the host; a tampered image or dependency; and an operator mistake (a port published to
the internet, a world-readable secret file, a weak token).

1. **Network access without credentials.** Mitigations: nothing is published by default (goal 3);
   the TLS listener needs the override, a non-loopback bind chosen by the operator, and a token
   secret of at least 256 bits, without which it does not start; the token is checked before the
   WebSocket upgrade, in constant time; only `/rpc` is served; rate limits and caps bound failed
   attempts and open connections. The healthcheck uses no network route.
2. **Interception and impersonation.** TLS 1.3 only; clients verify through the platform
   verifier or a pin given at setup, never trust on first use; a pin mismatch stops the client
   before it sends the token. Over SSH, OpenSSH's host-key checking applies and the local end of
   the forward is `0600`.
3. **Token theft from inside the container.** The remote token, the TLS key, `daemon.json` and
   the secrets are under `/run/secrets`, `/var/lib/imperium` and `/run/imperium`, which tier 1's
   Landlock ruleset leaves out of reach of agents' commands; Landlock also blocks tracing the
   daemon. With tier 0 chosen knowingly, a command can read them all; the docs say so plainly.
   The daemon's own children (MCP servers, hooks) are not confined (D7).
4. **Secrets leaking through configuration.** Secrets are only files under `/run/secrets`; the
   compose files never carry one in `environment`, labels or interpolation (`check-compose.py`);
   environment variables carry backend names and paths only. `docker compose config` output can
   still show interpolated values of the operator's own `.env`, so the docs keep secrets out of
   it. The directory store opens with `O_NOFOLLOW`, caps size, and never logs a value.
5. **Writes that would silently land somewhere weaker.** A secret write on a server is refused
   (`-32010`), never written to `secrets.enc`, the data volume or a log.
6. **Desktop-only approvals obtained on a server.** Refused structurally (no installed app, no
   presence, peers that are `sshd`, absent, or entered from outside the PID namespace) and by
   server mode (D5). A derived image that adds a file named `imperium-desktop` still gets no
   approval: server mode refuses before any check, and the presence check is unavailable.
7. **Host compromise through the container.** Non-root user, every capability dropped,
   `no-new-privileges`, read-only root, default seccomp and AppArmor kept, no Docker socket, no
   privileged mode, no Docker-in-Docker. The `prepare` service is the only root process: no
   network, three capabilities, a fixed script, no symlink following.
8. **Other local users on the host.** The socket folder is `0700` and the socket `0600`, owned by
   `IMPERIUM_UID`; the daemon refuses a socket folder it does not own or that is a link; secret
   files must be `0400` or `0600` for that UID (the docs give the commands; the daemon warns when
   a secret file is readable by others, which it can see through `stat`).
9. **Model services as a side door.** `llamacpp` and `vllm` publish no port and sit on an
   internal network with only the daemon; they hold no secret. An agent's command in the daemon's
   container can call them, which costs only GPU time, as a local model does on a desktop.
10. **Supply chain.** Base and service images pinned by digest; actions pinned by SHA; binaries
    from the same build as the release archives; SBOM and provenance attached; the index signed
    with cosign keyless; Trivy gates the build. Users verify with `cosign verify` before pulling
    a new tag; the release's compose file pins the digest.
11. **Denial of service.** Connection caps, handshake timeouts and back-off on the TLS listener;
    `pids_limit` and an optional memory limit on the service; `tmpfs` size cap. A restart loses
    no session (journals).
12. **Stale state after a restart.** A stale `daemon.json`, socket or `endpoint.json` is replaced
    at start; a client holding an old token gets 401 and reconnects through `remote connect`.

## Protocol additions

| Change | Stage | Does |
| --- | --- | --- |
| `HandshakeResult.server` (bool, default `false`) | 1 | the daemon runs in server mode; clients hide desktop-only controls |
| Error `-32010` `UNAVAILABLE_ON_SERVER` | 1 | `data: {feature, secret?}`, `feature` one of `plugin_approval`, `auto_merge_enable`, `terminal_grant`, `secret_write`, `mcp_login`, `container_tier`, `vllm_docker` |
| `DaemonEndpoint.pin` (optional string) | 6 | a recorded remote's certificate pin; a file format in `imperium-protocol`, not a wire change |

Stage 1 takes the next protocol minor at its merge (0.61 if no other change lands first) and
updates `PROTOCOL_VERSION`, [02](../02-architecture.md#protocol), the method snapshot and the
generated TypeScript types. No method is added or removed. The TLS listener speaks the same
JSON-RPC as the loopback WebSocket.

Config added (all user layer or environment; none readable from a project's files):

- `daemon.server` (bool, default `false`).
- `daemon.socket_dir` (path, default `<data>/ipc`).
- `secrets.store` (`auto`, the default and today's behaviour, or `directory`) and
  `secrets.directory` (default `/run/secrets`).
- `github.token_source` (`gh` default, or `secret`).
- `isolation.os_backend` (`bwrap` default, or `landlock`).
- `remote.tls.listen` (unset: off), `remote.tls.allow_non_loopback` (default `false`),
  `remote.tls.token_secret` (default `imperium-remote-token`), `remote.tls.cert_secret` and
  `remote.tls.key_secret` (defaults `imperium-tls-cert`, `imperium-tls-key`).

## Assumptions

- Docker Engine 24 or later with Compose v2.24 or later on the server; the NVIDIA Container
  Toolkit for the GPU overrides.
- Host kernels with Landlock enabled (Ubuntu 22.04 and later, Debian 12 and later with the
  default LSM list) for tier 1; ABI 4 (kernel 6.7) for `isolation.network = false`. Verified on
  the hosted runner in stage 3; elsewhere it fails closed.
- Docker's default seccomp profile permits the Landlock system calls (verified in stage 3; the
  fallback profile is in D7).
- GitHub-hosted `ubuntu-latest` runners can run `sshd` on localhost for the SSH-path test.

## Relevant paths

`crates/imperium-core/src/{main.rs,server.rs,rpc/*,app_identity/*,plugin_install.rs,automerge.rs,mod_install.rs,github_token.rs}`,
`crates/imperium-config/src/{settings.rs,registry.rs,secrets.rs,secrets_file.rs}`,
`crates/imperium-ipc/`, `crates/imperium-tools/src/sandbox.rs`,
`crates/imperium-local/src/vllm_env.rs`, `crates/imperium-cli/src/{main.rs,remote.rs}`,
`crates/imperium-client/`, `crates/imperium-protocol/src/{lib.rs,jsonrpc.rs,discovery.rs,methods/system.rs}`,
`apps/desktop/src-tauri/src/`, `apps/desktop/src/` (settings and connection),
`deploy/server/` (new), `.github/workflows/{ci.yml,release.yml,experimental.yml,image.yml}`,
`scripts/ci/detect.sh`.

## Related docs

[02 process model and protocol](../02-architecture.md#process-model),
[05 isolation and SSH hosts](../05-tools-and-isolation.md#isolation-tiers),
[06 who may grant trust and approval](../06-extensibility.md#who-may-grant-trust-and-approval-implemented),
[10 secrets](../10-configuration.md#secrets), [04 runtimes](../04-models-and-routing.md#runtimes),
[11 CI/CD](../11-ci-cd.md), [15 mobile and remote](../15-mobile-and-remote.md), and the
[pull-request card plan](pr-card.md) (the auto-merge challenge and `TokenSource`). The numbered
docs win for built behaviour; this plan wins for planned behaviour until a stage merges and
updates them.

## Stages

Each stage is one pull request against `experimental`, with the docs it changes. Sizes are
estimates of changed lines, tests included.

| Stage | Name | Depends on | Estimate |
| --- | --- | --- | --- |
| 1 | Server mode and refusals | none | 650 |
| 2 | Read-only directory secrets | 1 | 600 |
| 3 | Landlock backend for tier 1 | none | 750 |
| 4 | Socket folder and the SSH path into a container | none | 450 |
| 5 | TLS listener | 1, 2 | 750 |
| 6 | Pinned TLS client in the CLI | 5 | 500 |
| 7 | Image and compose files | 1 to 4 | 600 |
| 8 | CI server job | 5 to 7 | 650 |
| 9 | Publishing to GHCR | 7, 8 | 400 |
| 10 | Desktop app connects to a server | 4 to 6 | 750 |
| 11 | Docs and close-out | all | 350 |

## Stage 1: Server mode and refusals

- Objective: `daemon.server`; `-32010` `UNAVAILABLE_ON_SERVER` with its `feature` data;
  `HandshakeResult.server`; the refusals of [D5](#d5-features-unavailable-on-a-server) at
  `plugins.approval_challenge`, `plugins.approve`, `pull_requests.auto_merge_challenge`,
  `pull_requests.set_auto_merge` (`enabled: true` only), `mods.install` grants,
  `isolation.tier = "container"` (config validation and session start) and Docker-mode vLLM; the
  Linux ancestry walk refuses a peer whose parent PID is 0 while it is not PID 1; SIGTERM on Unix
  runs the same shutdown as Ctrl+C. Protocol minor bump.
- Depends on: none.
- Owned paths: `crates/imperium-protocol/src/{lib.rs,jsonrpc.rs,methods/system.rs}`,
  `packages/protocol/src/generated/`, `crates/imperium-config/src/{settings.rs,registry.rs}`,
  `crates/imperium-core/src/{main.rs,rpc/*,app_identity/*,mod_install.rs,automerge.rs}`,
  `crates/imperium-local/src/vllm_env.rs`, `crates/imperium-core/tests/server_mode.rs` (new),
  docs 02, 05, 06.
- Acceptance checks: `cargo test` (CI): with `daemon.server` each refused call returns `-32010`
  and the right `feature`, and nothing is recorded; without it today's behaviour is unchanged
  (existing tests); a `config.toml` setting `daemon.server = false` does not override
  `IMPERIUM_DAEMON__SERVER=true`; a carried-over plugin approval record still counts in server
  mode; `set_auto_merge` with `enabled: false` still works; a fake peer with parent PID 0 is
  refused a `trusted` grant; SIGTERM on a test daemon removes `daemon.json` and the socket. The
  handshake snapshot shows `server`. `cargo clippy` clean.
- Status: pending.
- Outcome:
- Handoff notes: the refusals sit at the RPC boundary before any challenge is issued, so the
  existing identity code is untouched.

## Stage 2: Read-only directory secrets

- Objective: `DirectorySecrets` and `SecretError::ReadOnly`; `secrets.store` and
  `secrets.directory`; the `secrets.backend` record and `imperium secret` following it; every write
  path of [D4](#d4-secrets-backend) refused with `-32010` (`secret_write`, `mcp_login`);
  `api.token` reading the file; `github.token_source = "secret"` with a `TokenSource` reading
  `github.token`; a warning when a secret file is readable by others.
- Depends on: stage 1 (the error code).
- Owned paths: `crates/imperium-config/src/{secrets.rs,secrets_dir.rs (new),settings.rs,lib.rs}`,
  `crates/imperium-core/src/{github_token.rs,rpc/*,plugin_install.rs,mcp.rs}`,
  `crates/imperium-cli/src/main.rs` (secret subcommands' messages), docs 10.
- Acceptance checks: `cargo test -p imperium-config -p imperium-core` (CI): load from a temporary
  folder; a symbolic link, a FIFO, a file over 64 KiB and a name with `/` or `..` are refused; one
  trailing newline is trimmed; `store` and `delete` fail and write nothing anywhere (the folder,
  `secrets.enc` and the data folder are unchanged); `plugins.set_secret`, `secret.set`,
  `mcp.login` and `api.token` rotate return `-32010` with the secret's name and no value; the
  GitHub token source reads the file and a sentinel token never appears in logs or errors;
  `secrets.store` in a project's `.imperium/config.toml` changes nothing.
- Status: pending.
- Outcome:
- Handoff notes: `plugins.list`'s `missing_secrets` keeps its meaning; on a server the desktop
  shows the file names to add instead of an input box (stage 10).

## Stage 3: Landlock backend for tier 1

- Objective: `isolation.os_backend = "landlock"` in `Sandbox::Os` on Linux, with the ruleset of
  [D7](#d7-sandbox-tiers-inside-the-container), a per-command `TMPDIR`, TCP denial when the ABI
  allows it and a fail-closed error otherwise; the session label names the backend. First step:
  a CI probe that Docker's default seccomp profile allows the Landlock calls on the runner (a
  tiny static test binary in `docker run --rm` with the default profile); if not, add
  `deploy/server/seccomp.json` (default plus the three calls) and say so in D7 before going on.
- Depends on: none (the server image consumes it in stage 7).
- Owned paths: `crates/imperium-tools/src/{sandbox.rs,sandbox_landlock.rs (new)}`,
  `crates/imperium-tools/Cargo.toml` (`landlock` crate, Linux only),
  `crates/imperium-config/src/settings.rs`, `.github/workflows/ci.yml` (probe step), docs 05.
- Acceptance checks: `cargo test -p imperium-tools` on the Linux CI runner: a command can write in
  the workspace and its `TMPDIR`; reading a file under a stand-in for `/run/secrets`, the data
  folder and the socket folder fails with permission denied; writing to `$HOME` fails; reading
  `/proc/<daemon pid>/environ` fails; a `connect` to a loopback port fails with
  `isolation.network = false` where ABI 4 is present (skipped with a logged reason where not); a
  forced "no Landlock" path fails the command with the documented message and never runs it
  unconfined. bubblewrap's tests unchanged. `cargo clippy` clean.
- Status: pending.
- Outcome:
- Handoff notes: the backend is usable on any Linux host, not only in the image; it stays opt-in
  outside the image.

## Stage 4: Socket folder and the SSH path into a container

- Objective: `daemon.socket_dir` with the ownership and link checks; `endpoint.json` beside the
  socket; `imperium remote connect --server-dir`; `imperium server info` (prints the socket
  folder, and in stage 5 the TLS address and pin) reading local files only.
- Depends on: none.
- Owned paths: `crates/imperium-ipc/`, `crates/imperium-core/src/{main.rs,server.rs}`,
  `crates/imperium-config/src/settings.rs`, `crates/imperium-cli/src/{main.rs,remote.rs}`,
  docs 02 and 05 (SSH hosts).
- Acceptance checks: `cargo test` (CI): a socket folder owned by another UID, a link, or with group
  or other bits is refused at start with a message; `endpoint.json` is `0600` and removed on
  shutdown; `remote connect --server-dir` builds `cat -- '<dir>/endpoint.json'` with the folder
  quoted as one argument and refuses a relative folder or one with a newline or quote; the
  forward arguments match today's socket forward with the folder's socket. The end-to-end SSH
  check is the server job's (stage 8).
- Status: pending.
- Outcome:
- Handoff notes: a desktop or plain SSH host keeps `<data>/ipc` and today's commands unchanged.

## Stage 5: TLS listener

- Objective: the listener of [D3](#d3-remote-access) in `imperium-core`: `remote.tls.*` settings,
  the token from the directory store (or the user's secret store off a server), operator or
  generated certificate with the SPKI pin, TLS 1.3 with rustls, the upgrade check before any
  WebSocket frame, `/rpc` only, the rate limits and caps, and server mode's refusals applying as on
  the local socket. `imperium server info` gains the address and pin.
- Depends on: stages 1 and 2.
- Owned paths: `crates/imperium-core/src/{remote_tls.rs (new),server.rs,main.rs}`,
  `crates/imperium-core/Cargo.toml` (`tokio-rustls`, `rcgen`),
  `crates/imperium-config/src/settings.rs`, `crates/imperium-cli/src/main.rs`,
  `crates/imperium-core/tests/remote_tls.rs` (new), docs 02.
- Acceptance checks: `cargo test -p imperium-core --test remote_tls` (CI): no token secret, or one
  shorter than 32 bytes, keeps the listener off with a log line; a non-loopback `listen` without
  `allow_non_loopback` is refused; with a test client: the right token and pin connect and
  handshake; no token and a wrong token get 401 before any frame; the sixth failure in a minute
  from one address is delayed; the 65th connection is refused; an idle TLS handshake is closed at
  10 s; TLS 1.2 is refused; any path but `/rpc` is 404 after authentication and 401 before it;
  the generated key is `0600` and kept across restarts; a desktop-only call over TLS returns
  `-32010` in server mode and is refused as not-local outside it.
- Status: pending.
- Outcome:
- Handoff notes: the listener is independent of the phone listener; later work may serve phones
  through it.

## Stage 6: Pinned TLS client in the CLI

- Objective: `wss://` with a pin in `imperium-client` (a rustls verifier that checks the pin, or
  the platform verifier when no pin is given); `imperium remote add|list|remove` for TLS remotes;
  the token stored as `remote.<name>.token` in the client's secret store and read from standard
  input; `DaemonEndpoint.pin`; `imperium --remote <name>` over TLS.
- Depends on: stage 5.
- Owned paths: `crates/imperium-client/`, `crates/imperium-cli/src/{main.rs,remote.rs}`,
  `crates/imperium-protocol/src/discovery.rs`, docs 05 (SSH hosts and remotes).
- Acceptance checks: `cargo test -p imperium-client -p imperium-cli` (CI) against stage 5's
  listener in process: a matching pin connects; a mismatched pin fails before the upgrade is sent
  (the server records no request); a self-signed certificate without a pin is refused; the token
  is absent from the record file, the process arguments and error text (sentinel search).
- Status: pending.
- Outcome:
- Handoff notes: a forwarder (`imperium remote connect <name>` for a TLS remote, exposing a local
  `0600` socket) is added here only if stage 10 needs the CLI to provide it; otherwise the
  desktop's Rust side has its own.

## Stage 7: Image and compose files

- Objective: `deploy/server/` with the `Dockerfile` ([D1](#d1-image)), the entrypoint (NSS wrapper
  when needed), `prepare-volumes`, `docker-compose.yml` ([D2](#d2-the-daemon-in-a-container)),
  `docker-compose.tls.yml`, `docker-compose.llamacpp.yml`, `docker-compose.vllm.yml`,
  `docker-compose.gpu.yml`, `secrets/README` (no secrets), `check-compose.py`, and the image's
  environment (`IMPERIUM_HOME`, `IMPERIUM_DAEMON__SERVER`, `IMPERIUM_SECRETS__STORE`,
  `IMPERIUM_ISOLATION__TIER`, `IMPERIUM_ISOLATION__OS_BACKEND`, `IMPERIUM_DAEMON__SOCKET_DIR`,
  `IMPERIUM_WORKSPACES__ROOT`, `IMPERIUM_GITHUB__TOKEN_SOURCE`, `IMPERIUM_NO_DAEMON_SPAWN`).
- Depends on: stages 1 to 4 (the settings the image sets must exist).
- Owned paths: `deploy/server/**`, `scripts/ci/detect.sh` (the `server` output),
  `.github/workflows/ci.yml` (a build-only step until stage 8).
- Acceptance checks: in CI, `docker build` succeeds for x64; `check-compose.py` passes on every
  shipped file and fails on fixtures with a public port, a Docker socket mount, a secret in
  `environment` and a missing `read_only`; `python3 -m pytest deploy/server` covers it;
  `prepare-volumes` under a test with a symbolic link to a file outside the volume leaves the
  target's owner unchanged; the image has no `imperium-desktop`; under UID 1001 `id -un` and
  `ssh -V` work; the llama.cpp and vLLM pins match `runtimes/*.env`. `hadolint` and
  `actionlint` clean.
- Status: pending.
- Outcome:
- Handoff notes: the repository's compose file names the image by tag; stage 9 writes the
  digest into the release's copy.

## Stage 8: CI server job

- Objective: the `server` job of [D8](#d8-testing-in-ci) and its gate: build x64 release
  binaries, build the image, build arm64 under QEMU (no run), `docker compose -f
  docker-compose.yml -f docker-compose.tls.yml -f deploy/server/ci/docker-compose.ci.yml up
  --wait` with test secrets generated in the job and `IMPERIUM_UID=$(id -u)`, then:
  1. healthcheck healthy (`docker inspect --format '{{.State.Health.Status}}'`);
  2. start the runner's `sshd` with a throwaway key and run `imperium remote connect localhost
     --socket --server-dir "$PWD/run"` in the background (recorded, stopped at the end);
     `imperium --remote ci ping`;
  3. `imperium remote add tls wss://127.0.0.1:7443 --pin <from imperium server info>` with the
     token on standard input; `imperium --remote tls ping`; `curl` without a token and with a
     wrong one gets 401; a wrong pin fails;
  4. `imperium --remote ci run --headless --json --task deploy/server/ci/task.json --workspace
     /workspaces/ci --root-model http://fake-model:8080/v1#fake --permission-mode full`: the
     fake model's scripted `run_command` runs `cat /run/secrets/imperium-remote-token;
     cat /var/lib/imperium/data/daemon.json; ls /run/imperium`; the job asserts the run succeeded,
     the fake recorded the tool result, and the result shows permission denied for all three and
     contains no token;
  5. call each desktop-only method over both paths and assert `-32010`;
  6. `docker compose config` for the llama.cpp, vLLM and GPU overrides, through
     `check-compose.py`;
  7. Trivy scan;
  8. `docker compose down` (volumes are the job's own and are removed with the runner).
  Logs (`docker compose logs --tail 200`) are uploaded on failure, with the test secrets out of
  them.
- Depends on: stages 5 to 7.
- Owned paths: `.github/workflows/ci.yml`, `deploy/server/ci/**` (fake model, task, CI override,
  driver script `deploy/server/ci/run.sh`), `deploy/server/.trivyignore`, docs 11.
- Acceptance checks: the job is green on its own pull request, and red on a temporary commit in
  that pull request that publishes `0.0.0.0` or sets `isolation.tier = "policy"` in the CI
  override (reverted before merge; its run is linked in the pull request).
- Status: pending.
- Outcome:
- Handoff notes: the job runs only on `server` changes; its gate passes when skipped.

## Stage 9: Publishing to GHCR

- Objective: a reusable `image.yml` called by `release.yml` and `experimental.yml` after the
  build: per-arch jobs on native runners take the CLI archives, build with SBOM and provenance,
  run the stage 8 smoke steps 1, 2 and 4 against the built image before pushing, push by digest;
  a merge job creates the index with the channel's tags, signs it with cosign keyless, verifies
  the signature and attestations, and attaches a `docker-compose.yml` with the digest to the
  GitHub release. Only the publish jobs get `packages: write` and `id-token: write`.
- Depends on: stages 7 and 8.
- Owned paths: `.github/workflows/{image.yml (new),release.yml,experimental.yml}`,
  `scripts/ci/publish-image.sh` (new), `scripts/ci/publish-release.sh`,
  `scripts/ci/publish-experimental.sh`, docs 11.
- Acceptance checks: `actionlint` clean; the first experimental build after merge shows the index
  with both platforms in GHCR, `cosign verify` passing in the job log, and the release carrying
  the pinned compose file. The release smoke test failing stops the push (shown by the job's
  ordering and `needs`).
- Status: pending.
- Outcome:
- Handoff notes: the GHCR package must be made public once, by the repository owner, after the
  first push ([open questions](#open-questions)).

## Stage 10: Desktop app connects to a server

- Objective: Settings → Servers (add, test, remove) for SSH and TLS servers; the Rust side owns the
  SSH forward or the pinned TLS forwarder and the token (in the OS keychain); the webview connects
  to the local end; `server: true` hides plugin approve, auto-merge enable and mod trust prompts
  and shows a server badge; the Plugins window shows which Docker secret files a plugin needs
  instead of an input on a server.
- Depends on: stages 4 to 6.
- Owned paths: `apps/desktop/src-tauri/src/` (server connections), `apps/desktop/src/`
  (settings page, connection, badge, Plugins window), `apps/desktop/test/`, docs 07.
- Acceptance checks: `npm test` and `cargo test` (CI): the forwarder refuses a wrong pin; the
  token never reaches the webview's storage, logs or the remote record; with a handshake of
  `server: true` the hidden controls are absent from the rendered tree; an SSH server's record
  runs the same arguments as the CLI's.
- Status: pending.
- Outcome:
- Handoff notes: switching the app between the local daemon and a server keeps one connection at
  a time.

## Stage 11: Docs and close-out

- Objective: a self-hosting guide `docs/22-server.md` (install, secrets, UID, the two remote paths,
  pins, GPU overrides, upgrades, backups of the named volumes, `cosign verify`, what is
  unavailable and why); final passes on 02, 04, 05, 06, 10, 11 and 15; the README's install
  section; roadmap item 63 marked done.
- Depends on: all.
- Owned paths: `docs/**`, `README.md`, `deploy/server/README.md`.
- Acceptance checks: `python3 scripts/ci/check-links.py .` and markdownlint clean in CI; every
  setting and command in the guide exists in the code at the merge commit.
- Status: pending.
- Outcome:
- Handoff notes: none yet.

## Risks across stages

- **Landlock availability.** Hosts without it get failing commands by default. Mitigation: a clear
  error with the two choices; the docs list kernels; the assumption is checked in CI first.
- **Docker defaults change.** A new Docker release could change the seccomp profile. The server
  job runs the probe of stage 3 on every run.
- **UID mismatches.** Wrong ownership is the most likely support problem. `prepare` fixes the
  volumes; the daemon's start error names the folder and its owner.
- **Image size and scan noise.** Debian packages bring CVEs without fixes; `--ignore-unfixed`
  and a reviewed ignore file with dates keep the gate meaningful.
- **Static model services surprise users** who expect the desktop's dynamic loading. The docs
  compare the two options of D6.
- **Protocol minors.** Concurrent pull requests take the next free minor at merge.

## Open questions

1. Publish the image from the experimental channel too (`:experimental` and
   `X.Y.<run>-experimental`)? This plan assumes yes, since experimental is the channel in use.
2. Is `ghcr.io/densetsu152637/imperium-server`, public, the name and visibility you want?
   Making the package public is a one-time setting only the owner can change.
3. Default tier: `os` with Landlock, failing closed where a host lacks it (this plan), or `policy`
   with a start-up warning that agents' commands can read every secret?
4. A writable store for credentials the daemon itself mints (MCP OAuth tokens), encrypted in the
   data volume with a key from a Docker secret: a later roadmap item, or never on a server?
5. Agents' commands on a server have no git credentials (the daemon's own pushes do). Should a
   per-environment opt-in hand them a token or a deploy key from a Docker secret, as
   `github.session_gh` does on a desktop?
6. Serve phones through the TLS listener with the existing scoped device tokens, as a follow-up
   item?
