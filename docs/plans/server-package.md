# Plan: server package (Docker Compose)

Revision: 2026-11-26, revision 6 (stage 1 security review: no nested-namespace rule, server mode enforced in the checks, migration on a server; over revision 5 (plan-gate round 4: C1 opt-outs match origin and name; over
revision 4's B1 and B2, revision 3's A1 and A2 and revision 2's F1 to F10)); written against
`experimental` at `2cb50590`, merged with `experimental` at revision 2 (last updated).
Roadmap [item 63](../12-roadmap.md#next-in-order). Status: in progress (stage 1 done).

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
   filesystem, every capability dropped and `no-new-privileges`. Check: the server job builds
   and runs the image on two native legs, `ubuntu-latest` (amd64) and `ubuntu-22.04-arm`
   (arm64); on each, `docker inspect` shows a non-zero `User`, the stack becomes healthy and a
   write to `/usr` inside the running container fails. Stage 9's release job inspects the
   published index for both platforms.
2. **The stack becomes healthy.** `docker compose up --wait` returns with the daemon healthy, and
   the service has a restart policy. Check: the server job.
3. **Nothing is published beyond loopback by default.** The base compose file publishes no port
   at all; the TLS override publishes only on `127.0.0.1` unless the operator sets another
   address. Check: `deploy/server/check-compose.py` in the server job reads
   `docker compose config --format json` for the base file and for every shipped override and
   fails on a published port whose host address is not loopback, on a missing `read_only`,
   `cap_drop` or `user`, on a mounted Docker socket, on `pid: host`, `privileged: true`,
   `network_mode: host`, `userns_mode: host` or `ipc: host` on the `imperium` service, and on any
   change to the `prepare` service's fixed shape ([D2](#d2-the-daemon-in-a-container)).
4. **The SSH path works into the container.** `imperium remote connect <host> --socket
   --server-dir <dir>` from the runner, through the runner's own `sshd`, reaches the daemon in
   the container and completes `system.handshake`. Check: the server job.
5. **The TLS path is authenticated and pinned.** With the TLS override, a client with the token
   and the pin completes the handshake, even from an address whose failures are being slowed; no
   token and a wrong token get HTTP 401, and repeated failures only delay those 401 replies; a
   client with a wrong pin refuses before sending the token; the listener refuses to start
   without a token secret. Check: stage 5 and 6 `cargo test`, and the server job.
6. **One headless task runs against the server.** `imperium --remote ci run --headless --json`
   with the fake model server completes, prints a result whose status is success, and the fake
   records the request. Check: the server job.
7. **Secrets come only from Docker secrets files.** The daemon reads secrets from `/run/secrets`
   read-only; every secret write is refused with `-32010`; no shipped compose file sets a secret
   in `environment` or interpolates one; an agent's command in the default tier cannot read
   `/run/secrets`, the daemon's data folder, `/run/imperium/endpoint.json` (the local token) or
   credential files under `HOME` (`~/.ssh/id_*`, `~/.config/gh/hosts.yml`, `~/.git-credentials`),
   and a connection to the daemon's socket without the token is refused. Check: stage 2 and 3
   `cargo test`,
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
  builds each architecture's binaries natively in the job and passes them as the build context.
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
  joined into one index with `docker buildx imagetools create`. Pull-request CI also runs both
  architectures natively ([D8](#d8-testing-in-ci)); nothing is built under QEMU.
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
  and Rekor), over the index digest. The signing job is in the reusable workflow `image.yml`, so
  Fulcio's certificate identity is that file at the caller's ref (the `job_workflow_ref` claim),
  one exact identity per channel:
  - main release and pre-release:
    `https://github.com/Densetsu152637/Imperium-AI/.github/workflows/image.yml@refs/tags/vX.Y.Z`
    (the release's own tag);
  - experimental:
    `https://github.com/Densetsu152637/Imperium-AI/.github/workflows/image.yml@refs/heads/experimental`.

  Both with `--certificate-oidc-issuer https://token.actions.githubusercontent.com`; the docs
  give the exact `cosign verify --certificate-identity <one of the above>` command, never a
  regular expression wider than one channel. Users verify the **digest** (`image@sha256:...`)
  and then pull and run that digest; a tag is never pulled again after verifying, since it can
  move between the two steps. The release's compose file names the digest for that reason.
  Rejected: a long-lived signing key (a secret to
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
  `network_mode: none`, a read-only root, no secrets, running a fixed script
  (`/usr/local/lib/imperium/prepare-volumes`). For each mounted volume and the socket folder it
  compares the owner of the mount's root with `IMPERIUM_UID:IMPERIUM_GID` and runs the
  recursive `chown -R -P -h` (never following a symbolic link) **only when the root's owner
  differs**, so an ordinary start walks nothing; it then sets the socket folder to `0700`. It
  runs no Imperium code. With the default UID it changes nothing. `check-compose.py` asserts
  that `prepare` is the only service running as root (or with no `user`), that its capabilities
  are exactly `CHOWN`, `FOWNER` and `DAC_OVERRIDE` after `cap_drop: [ALL]`, that it has
  `network_mode: none`, the fixed command and no `secrets`, and that no shipped override changes
  any of this. Rejected: starting the daemon itself as root and dropping privileges (root in
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
    link. The daemon cannot see the host side, so the docs require that the **parent** of
    `./run` (the stack's folder) is not writable by other users: whoever can write the parent can
    swap the folder before the bind mount is made.
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
    subprotocol) on the upgrade. Once TLS is up, the token is the **first** thing checked,
    compared in constant time, before any failure counter is looked at. The token is the Docker secret
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
  - **Rate limits and caps, without lockout.** A correct token always connects, whatever the
    failure counters say: an attacker who knows only the address cannot lock the owner out by
    failing on purpose, which matters because behind Docker's userland proxy every client may
    show the same source address. Failures only **delay the 401 reply**: after 5 failures a minute
    from one address, each further 401 is sent after double the last delay, up to 30 s; the delay
    never applies to a request whose token matches. Resources are bounded separately, and no
    bound refuses a newcomer outright:
    - **Handshake rate.** At most 16 new TLS handshakes a second per address and 64 overall. A
      connection over the rate waits in a short accept queue (at most 64 entries, each waiting at
      most 2 s) instead of being closed; when the queue is full its **oldest** entry is dropped
      for the newcomer.
    - **Pre-auth pool.** At most 32 connections that have not yet authenticated, each closed if
      TLS and the upgrade are not finished within 10 s. When the pool is full, a newcomer
      **evicts a random** unauthenticated connection rather than being refused (random, so an
      attacker cannot time its connections to always be the youngest).
    - **Tarpit pool.** A connection whose token failed leaves the pre-auth pool at once and waits
      out its delayed 401 in a separate pool of at most 16; when it is full the oldest entry is
      closed (its 401 sent early or not at all). Delayed 401s therefore never hold pre-auth
      slots.
    - **Reserved slots.** 64 connections in all, of which 16 are reserved for authenticated
      connections, so unauthenticated traffic can never fill the listener.

    **What this guarantees, and what it does not.** Under a flood of `r` new connections a second
    from the owner's address, the accept queue lets through 16 a second, so an attempt by the
    owner gets past it with probability about `16 / r`; once through, it survives random
    eviction from a full pre-auth pool with probability about `(1 - 1/32)^k`, where `k` (small,
    since at most 16 a second enter) is the number of newcomers during its TLS handshake and
    upgrade. With the stated parameters (`r = 100`, at most 16 entering a second, and a TLS
    handshake and upgrade window of 0.1 s, so `k <= 2`), per-attempt survival is at
    least `0.16 * (31/32)^2 = 0.150`. The CLI and desktop retry a dropped or reset connection
    with back-off, so a correct-token client gets in within 30 attempts more than 99 % of the
    time (`1 - 0.85^30 > 0.99`). A flood from the owner's own (shared) address therefore
    **degrades service**: it
    makes lockout probabilistic and costly for the attacker, not impossible. Blocking it outright
    needs a firewall in front of the listener or a private network (the docs recommend an
    overlay such as Tailscale for exposure beyond the LAN). The 256-bit token makes guessing
    hopeless; the limits protect the daemon's resources.
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
it must be the `imperium-desktop` installed beside `imperium-core` (`app_identity`), and a
terminal grant needs a peer whose ancestry the daemon can walk (`peer::may_grant`). In the image
these cannot hold, for reasons that do not depend on configuration:

- **Plugin approval and the auto-merge challenge: no installed app.** The image ships no
  `imperium-desktop`, and the read-only root filesystem means none can be put beside
  `/usr/local/bin/imperium-core` at run time (stage 7's image test asserts the file is absent).
  That absence is the structural barrier. The presence check is **not** one: as
  [06](../06-extensibility.md#who-may-grant-trust-and-approval-implemented) and `app_identity`'s
  check 6 say, an unavailable presence check records a `dialog-only` approval rather than
  refusing, and a container has no presence provider, so it would be "unavailable".
- **Terminal grants: the ancestry walk cannot be trusted in a container.** Inside a container
  a process can double-fork so that it is re-parented to `tini` (PID 1), which carries no
  `IMPERIUM_SPAWNED_BY_DAEMON` mark and has no `sshd` above it in the namespace, so the walk
  would pass; a rule on the peer's parent PID does not catch that. The daemon **cannot detect**
  that it runs in a container: Docker, Podman, distrobox and toolbox mount a fresh `/proc`, so its
  own `NSpid` line has one entry (the two-entry line is only the host's view of it), and comparing
  namespace inodes would also fire on WSL2 desktops, which are themselves nested. (Revision 6:
  stage 1 first built such an `NSpid` rule; the security review reproduced with
  `unshare --mount-proc` that it never fires, and it was removed.) The daemon is **not** made a
  child subreaper to keep re-parented processes in its tree: a daemon-wide subreaper steals
  agents' commands' exit statuses, as the terminal plan found.
- **Peers from outside.** A connection over the TLS listener has no peer process at all. Over the
  SSH-forwarded socket the peer is the host's `sshd`, outside the container's PID namespace, so
  `SO_PEERCRED` reports PID 0, which the peer lookup refuses (fails closed). `docker exec` peers
  are inside the namespace; only server mode refuses them.

**So server mode is the rule, enforced centrally.** The setting `daemon.server` (set by the image
as `IMPERIUM_DAEMON__SERVER=true`; environment variables are the last layer, so a `config.toml` on
the volume cannot unset it) makes the daemon answer these with `-32010` (`UNAVAILABLE_ON_SERVER`)
and a `feature` in the error's data, and report `server: true` in the handshake. Beyond the
per-method refusals, it is enforced inside the two checks every grant and approval goes through:
`app_identity::InstalledApp` is made from the daemon's state (`InstalledApp::of`) and refuses every
app-identity approval on a server, and `peer::may_grant` takes the daemon's state and refuses every
terminal grant on a server, both before any peer is looked at. Every present and future caller
(terminal panes, item 55; `files.write`, item 66) therefore inherits the refusal. It only ever
removes capabilities.

**Turning server mode off on a server is the operator's own act.** It means changing the image's
environment, which nothing inside the container can do. A daemon run in a container without
server mode (a derived image, or distrobox or toolbox on a desktop) has the double-fork gap above;
the docs say to keep server mode on in a container and to run the daemon on the host to make
grants. `check-compose.py` still refuses `pid: host`, `privileged: true`, `network_mode: host`,
`userns_mode: host` and `ipc: host` on the `imperium` service, in the base file and every
override.

| Feature | On a server | `feature` |
| --- | --- | --- |
| Plugin approval (`plugins.approval_challenge`, `plugins.approve`) | refused | `plugin_approval` |
| Presence check | never raised: server mode refuses before it, and without server mode the missing app refuses first | n/a |
| Enabling GitHub auto-merge (`pull_requests.auto_merge_challenge`, `set_auto_merge` with `enabled: true`) | refused; disabling still works from any client | `auto_merge_enable` |
| Terminal grants (`mods.install` `trusted` and `interactive`) | `trusted` is an error, `interactive` is dropped with a note, as when the check fails today | `terminal_grant` |
| Secret writes, `mcp.login`, `api.token` rotate | refused ([D4](#d4-secrets-backend)) | `secret_write`, `mcp_login` |
| `isolation.tier = "container"`, and an environment's Docker runtime (an image, or `docker exec` into a running container) | refused at config validation and at session start | `container_tier` |
| `imperium runtime vllm setup --docker` and Docker-mode vLLM starts | refused (would need the Docker socket) | `vllm_docker` |

**Carried-over approvals keep working.** Plugin approval records already in `<data>` (a data
folder restored from a desktop machine) are verified as today by the plugin's hash and run
summary, and count; stage 1 tests that loading a record does not re-run the desktop identity
check (which applies to making approvals, not to reading them). Nothing on the server creates a
new one: the first-start plugin migration, which on a desktop gives the plugins already installed
a carried-over approval, records them **without** approval on a server, so a fresh data volume or
a deleted `plugin-migration.json` approves nothing. **Until stage 3**, the authority to approve on
a server equals write access to the data volume: the file tools refuse writes to the approval
records (#304), but an agent's command in tier 0 can write `plugin-installs.json` or
`skill-installs.json` under `/var/lib/imperium` and mint a record; stage 3's Landlock ruleset
keeps agents' commands off `/var/lib/imperium`, which closes it. Auto-merge already enabled on GitHub stays
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
  environment layer (`IMPERIUM_ENDPOINTS__LLAMACPP__BASE_URL=http://llamacpp:8080/v1`, which the
  loader splits on `__` and lowercases into `endpoints.llamacpp.base_url`, the same table
  `[endpoints.<id>]` uses; stage 7 checks it with `imperium config explain
  endpoints.llamacpp.base_url` in the running container). vLLM gets
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
  - read and execute: `/usr`, `/bin`, `/sbin`, `/lib`, `/lib64`, `/etc`, `/opt`, `/proc`, and
    `/dev` device files;
  - from `/home/imperium`, read-only and only these named files: `.gitconfig` and
    `.ssh/known_hosts`. The rest of `HOME` is unreachable, so credentials a user or tool leaves
    there (`.ssh/id_*`, `.config/gh/hosts.yml`, `.git-credentials`) cannot be read, and nothing
    can be planted there for the daemon's own processes to read later. `HOME` stays set, and
    tools that want a writable cache get one through `XDG_CACHE_HOME` in the per-command
    temporary folder;
  - read and write: the session's folders and writable grants, and a per-command temporary
    folder under `/tmp` (set as `TMPDIR`);
  - nothing else: `/var/lib/imperium` (config, data, cache, TLS key), `/run/secrets` and the
    socket folder are unreachable.
  Landlock also stops a sandboxed process from tracing or reading the memory of the daemon (a
  process outside its domain).
  - **What Landlock does not mediate.** `connect()` to a pathname Unix socket is not an access
    Landlock checks, and Landlock has no right for walking a folder, so a sandboxed command
    **can** connect to `/run/imperium/daemon.sock` even though it cannot read the folder's
    files. Before ABI 6 (kernel 6.12) Landlock does not scope signals or abstract Unix
    sockets. The daemon's socket is protected by its token, which lives only in files the ruleset
    hides (`daemon.json`, `endpoint.json`); stage 8 asserts a token-less connect from inside the
    sandbox is refused. The loopback WebSocket is protected the same way.
  - **Network.** With `isolation.network = false`, Landlock ABI 4 denies TCP `connect` and `bind`
    (kernel 6.7 or later). UDP is not covered, so DNS lookups and any UDP traffic, including
    exfiltration over DNS queries, still work; the session label says "TCP blocked" rather than
    "no network", and the docs state the residual. If the kernel lacks Landlock (or the ABI a setting
  needs), commands **fail closed** with a message naming the two choices: a host with Landlock,
  or `isolation.tier = "policy"` set knowingly (which the docs describe as giving agents' commands
  every secret).
- **Assumption to verify first.** Docker's default seccomp profile allows the three Landlock
  system calls. Stage 3's first CI step checks it on the runner; if it does not, the stack ships
  a seccomp profile that is Docker's default plus those three calls, a much narrower change than
  bubblewrap's.
- **`container` (tier 2) is off.** It needs Docker inside the container (Docker-in-Docker, which
  is privileged) or the host's socket (root on the host). Server mode refuses it (D5).
- **Every MCP server and command hook is confined on a server, unless the user opts it out.**
  With `daemon.server` set and `isolation.os_backend = "landlock"`, the daemon applies the same
  ruleset (the session's folders writable, secrets, data, sockets and the rest of `HOME` hidden)
  to **every** stdio MCP server and **every** command hook it starts, whatever its origin: a
  trusted project's `.mcp.json` server (`crates/imperium-core/src/mcp.rs` loads those for
  projects in `mcp.trusted_projects`), a plugin's server, and the user's own `[mcp.servers]`.
  The default is inverted on purpose. Which servers are "workspace" ones cannot be decided
  reliably: a project server's `cwd` can point outside the session's folders or into another
  session's folder, and a user-level server can run a script that lives in a workspace, where an
  agent can rewrite it. Any of these left unconfined would be a tier-1 bypass that reads
  `/run/secrets`. A server that needs a secret gets it through `secret_env` or headers, from the
  daemon, not by reading the file.
  - **The only exception is an explicit opt-out, matched by origin and name together.**
    `mcp.unconfined` (a list of server names) and `hooks.unconfined` (a list of hook names) are
    read **only from the user layer**, `<config>/config.toml`. Environment variables are ignored
    for these keys, as are a project's `.imperium` files, an environment's config and a plugin
    (each ignored with a warning); the settings registry marks both keys user-file-only. An
    entry exempts a server or hook only if that server or hook is **itself defined in the same
    user layer** (`[mcp.servers.<name>]` or a `[[hooks]]` entry in `config.toml`). A trusted
    project's `.mcp.json` server, a plugin's server or a project or plugin hook with an
    opted-out name stays confined, and the daemon logs a name-collision warning and sends a
    notification naming the origin. Without the origin match, any project or plugin could pick an
    opted-out name and run unconfined. The docs say that an opted-out server can read every
    secret.
  - Still outside the domain, documented as such: HTTP MCP servers and HTTP hooks (no process is
    started), and model runtimes. The daemon's git in agent-writable repositories goes through `safe_git` (no hooks,
  fsmonitor or filters).

### D8. Testing in CI

- **A `server` job in `ci.yml`**, run when `detect.sh` reports `server` changes (`deploy/server/**`,
  `crates/**`, `Cargo.lock`, the workflow), with an always-run gate so a skipped job is not left
  pending. It has two legs, `ubuntu-latest` (amd64) and `ubuntu-22.04-arm` (arm64); each builds
  that architecture's release binaries natively, builds the image, starts the stack, and drives it
  ([stage 8](#stage-8-ci-server-job)). No QEMU. Docker in a
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

One minor at merge of [stage 1](#stage-1-server-mode-and-refusals) (0.62 if nothing else lands
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
  and the `IMPERIUM_SPAWNED_BY_DAEMON` mark (`crates/imperium-core/src/peer.rs`); a peer the
  process table does not have (as PID 0 from outside the namespace) is refused, while a missing
  parent ends the walk and allows. An unavailable presence check records `dialog-only`.
  Protocol is at 0.61 after the merge with `experimental`, so stage 1 takes 0.62 or later.
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
   secret of at least 256 bits, without which it does not start; the token is checked first once
   TLS is up, in constant time, before the WebSocket upgrade; only `/rpc` is served; failures
   delay only their own 401 replies from a separate tarpit pool, and a full pre-auth pool evicts
   a random connection rather than refusing the newcomer, so an attacker cannot lock the owner
   out by failing on purpose; a connection flood from the owner's own address degrades service
   (lockout becomes probabilistic and costly, not impossible, D3), while handshake rates with a
   short queue, the pre-authentication cap, reserved authenticated slots and a 10 s timeout
   bound resources. The healthcheck uses no network route. Published ports bypass host firewalls such
   as `ufw` (Docker writes its own iptables rules), so a non-loopback `IMPERIUM_TLS_BIND` is
   reachable whatever `ufw` says; the docs say so.
2. **Interception and impersonation.** TLS 1.3 only; clients verify through the platform
   verifier or a pin given at setup, never trust on first use; a pin mismatch stops the client
   before it sends the token. Over SSH, OpenSSH's host-key checking applies and the local end of
   the forward is `0600`.
3. **Token theft from inside the container.** The remote token, the TLS key, `daemon.json`,
   `endpoint.json` and the secrets are under `/run/secrets`, `/var/lib/imperium` and
   `/run/imperium`, which tier 1's Landlock ruleset leaves out of reach of agents' commands, as
   it does every file under `HOME` but `.gitconfig` and `.ssh/known_hosts`; Landlock also blocks
   tracing the daemon. A command can still `connect()` to the daemon's socket (Landlock does not
   mediate it), where the token it cannot read is required. Every stdio MCP server and command
   hook gets the same ruleset on a server, except one defined in the user's own `config.toml`
   and named there in `mcp.unconfined` or `hooks.unconfined` (origin and name must both match,
   so a project or plugin cannot borrow an opted-out name); model runtimes do not (D7). With tier 0 chosen knowingly, a command can read everything; the docs say so plainly.
   With `isolation.network = false`, UDP and DNS stay open (D7).
4. **Secrets leaking through configuration.** Secrets are only files under `/run/secrets`; the
   compose files never carry one in `environment`, labels or interpolation (`check-compose.py`);
   environment variables carry backend names and paths only. `docker compose config` output can
   still show interpolated values of the operator's own `.env`, so the docs keep secrets out of
   it. The directory store opens with `O_NOFOLLOW`, caps size, and never logs a value.
5. **Writes that would silently land somewhere weaker.** A secret write on a server is refused
   (`-32010`), never written to `secrets.enc`, the data volume or a log.
6. **Desktop-only approvals obtained on a server.** Refused by server mode, enforced centrally
   (D5): the app-identity check and the grant check take the daemon's state and refuse on a
   server before any peer is looked at, and each desktop-only method refuses first with
   `-32010`. Besides: there is no `imperium-desktop` to match, a TLS peer has no process, and an
   SSH-forwarded peer reads as PID 0 and fails closed. The presence check is not a barrier: where
   it is unavailable it would record `dialog-only`. There is **no structural rule** for a daemon
   in a container: it cannot detect that it is in one (a fresh `/proc` shows one `NSpid` entry),
   so a container run without server mode is open to double-forking to `tini`; the image sets
   server mode, and the docs say not to unset it. The first-start migration approves nothing on
   a server. **Residual until stage 3:** approval authority on a server equals write access to the
   data volume (`plugin-installs.json`), which agents' commands in tier 0 have; stage 3's Landlock
   ruleset removes it. `check-compose.py` refuses `pid: host` (and `privileged`, host network,
   host user namespace and host IPC) on the `imperium` service.
7. **Host compromise through the container.** Non-root user, every capability dropped,
   `no-new-privileges`, read-only root, default seccomp and AppArmor kept, no Docker socket, no
   privileged mode, no Docker-in-Docker. The `prepare` service is the only root process: no
   network, three capabilities, a fixed script, no secrets, no symlink following, and a recursive
   walk only when a root's owner differs; `check-compose.py` pins that shape.
8. **Other local users on the host.** The socket folder is `0700` and the socket `0600`, owned by
   `IMPERIUM_UID`; the daemon refuses a socket folder it does not own or that is a link; secret
   files must be `0400` or `0600` for that UID (the docs give the commands; the daemon warns when
   a secret file is readable by others, which it can see through `stat`). The parent of the
   host's `./run` must not be writable by other users (D3).
9. **Model services as a side door.** `llamacpp` and `vllm` publish no port and sit on an
   internal network with only the daemon; they hold no secret. An agent's command in the daemon's
   container can call them, which costs only GPU time, as a local model does on a desktop.
10. **Supply chain.** Base and service images pinned by digest; actions pinned by SHA; binaries
    from the same build as the release archives; SBOM and provenance attached; the index signed
    with cosign keyless; Trivy gates the build. Users verify a digest with `cosign verify` and the
    exact per-channel identity of D1, then pull that digest, never the tag again; the release's
    compose file pins the digest.
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

Stage 1 takes the next protocol minor at its merge (0.62 if no other change lands first) and
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
- `mcp.unconfined` and `hooks.unconfined` (lists of names, default empty): the only servers and
  hooks left unconfined on a server, and only those defined in the same user `config.toml`
  (origin and name both match). Read from the user file only; environment variables and every
  other layer are ignored with a warning.
- `remote.tls.listen` (unset: off), `remote.tls.allow_non_loopback` (default `false`),
  `remote.tls.token_secret` (default `imperium-remote-token`), `remote.tls.cert_secret` and
  `remote.tls.key_secret` (defaults `imperium-tls-cert`, `imperium-tls-key`).

## Assumptions

- **Docker Engine 28 or later** with Compose v2.24 or later on the server; the NVIDIA Container
  Toolkit for the GPU overrides. Earlier Engines let other hosts on the LAN reach a port
  published on `127.0.0.1` (direct routing to the bridge), which would undo goal 3; the docs
  require 28 and `check-compose.py` cannot check the Engine, so the guide's install steps do.
  Published ports bypass `ufw` and similar host firewalls on every version.
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
| 1 | Server mode and its refusals, enforced centrally | none | 750 |
| 2 | Read-only directory secrets | 1 | 600 |
| 3 | Landlock backend for tier 1, every MCP server and hook on a server | none (3b: 1) | 850 (split 3a/3b if past 800) |
| 4 | Socket folder and the SSH path into a container | none | 450 |
| 5 | TLS listener | 1, 2 | 800 |
| 6 | Pinned TLS client in the CLI | 5 | 500 |
| 7 | Image and compose files | 1 to 4 | 600 |
| 8 | CI server job | 5 to 7 | 750 |
| 9 | Publishing to GHCR | 7, 8 | 400 |
| 10 | Desktop app connects to a server | 4 to 6 | 750 |
| 11 | Docs and close-out | all | 350 |

## Stage 1: Server mode and refusals

- Objective: `daemon.server`; `-32010` `UNAVAILABLE_ON_SERVER` with its `feature` data;
  `HandshakeResult.server`; the refusals of [D5](#d5-features-unavailable-on-a-server) at
  `plugins.approval_challenge`, `plugins.approve`, `pull_requests.auto_merge_challenge`,
  `pull_requests.set_auto_merge` (`enabled: true` only), `mods.install` grants,
  `isolation.tier = "container"` (config validation and session start) and Docker-mode vLLM;
  server mode enforced inside `app_identity::InstalledApp` and `peer::may_grant`, so every caller
  inherits it; the first-start plugin migration approving nothing on a server; SIGTERM on Unix
  runs the same
  shutdown as Ctrl+C. Protocol minor bump.
- Depends on: none.
- Owned paths: `crates/imperium-protocol/src/{lib.rs,jsonrpc.rs,methods/system.rs}`,
  `packages/protocol/src/generated/`, `crates/imperium-config/src/{settings.rs,registry.rs}`,
  `crates/imperium-core/src/{main.rs,peer.rs,rpc/*,app_identity/*,mod_install.rs,automerge.rs}`,
  `crates/imperium-local/src/vllm_env.rs`, `crates/imperium-core/tests/server_mode.rs` (new),
  docs 02, 05, 06.
- Acceptance checks: `cargo test` (CI): with `daemon.server` each refused call returns `-32010`
  and the right `feature`, and nothing is recorded; without it today's behaviour is unchanged
  (existing tests); a `config.toml` setting `daemon.server = false` does not override
  `IMPERIUM_DAEMON__SERVER=true`; a carried-over plugin approval record still counts in server
  mode; `set_auto_merge` with `enabled: false` still works; SIGTERM on a test daemon removes
  `daemon.json` and the socket. Server mode through the production entry points:
  `InstalledApp::of` a server daemon refuses an approval for any peer, and `peer::may_grant` of a
  server daemon refuses a grant, each with a reason that differs from the off-server one, so
  removing the wiring fails a test; the first-start migration on a server records plugins with
  no approval (and with one off a server). A
  peer reported as PID 0 is refused (existing behaviour, now tested). The handshake snapshot shows
  `server`. `cargo clippy` clean.
- Status: done.
- Outcome: built, with these differences from the revision-5 plan.
  - **Protocol 0.66**: 0.62 to 0.64 were taken on `experimental` before this stage merged
    (`plugins.inspect_folder`, `pull_requests.set_monitor`, approval texts) and #304 holds 0.65.
    `HandshakeResult.server` (`#[serde(default)]`, so an older daemon reads as `false`) and
    `codes::UNAVAILABLE_ON_SERVER` (`-32010`) are in `imperium-protocol`, with every `feature`
    name of the table as a constant in `jsonrpc::server_features`, including the ones later stages
    use (`secret_write`, `mcp_login`). The text-editor plan's `FILE_CONFLICT` and `FILE_BUSY`
    move to `-32011` and `-32012`.
  - **No nested-namespace rule.** The first round built one from the `NSpid` line; the security
    review showed it never fires in Docker, Podman, distrobox or toolbox (a fresh `/proc`), and
    it was removed with its start-up warning (D5, revision 6). Server mode is enforced inside the
    checks instead: `InstalledApp { server }` is made with `InstalledApp::of(&daemon)` (tests use
    `InstalledApp::DESKTOP`), and `peer::may_grant(&daemon, pid, accepted_at)` answers
    `Err(Refused::Server)` on a server before any process is looked at. A peer of PID 0 is
    refused explicitly at the top of the walk.
  - **Per-method refusals.** `Connection::refuse_on_server` is still the first check of
    `plugins.approval_challenge`, `plugins.approve`, `pull_requests.auto_merge_challenge` and
    `set_auto_merge` with `enabled: true`; `mods.install` refuses `trusted` with `-32010`
    (`terminal_grant`) and a message that still starts with `trust was not granted`, so the CLI's
    sandbox fallback is unchanged, and drops `interactive` with a note. `config.set` of
    `isolation.tier = "container"` answers `-32010`; `Settings::validate` refuses that tier with
    `daemon.server` (the daemon does not start with it).
  - **First-start migration** (`plugin_records::migrate_with`, called by
    `DaemonState::migrate_plugins`) records the folders without approval on a server.
  - **Session start** needed files outside the owned paths: `CreateError::UnavailableOnServer`
    (`state.rs`) and checks in `state/create.rs` that refuse, on a server, any session whose
    sandbox is `Sandbox::Container` (the tier, or an environment's Docker image runtime) and any
    environment whose runtime is a running container (`docker exec`).
  - **Docker-mode vLLM** is `vllm_env::Mode::allowed(server)`: `imperium runtime vllm setup
    --docker` fails with its message before any other check (a CLI error, not a `-32010` reply:
    no RPC is involved; `crates/imperium-cli/src/vllm.rs`, test `tests/server_mode.rs` of the CLI),
    and a server daemon whose data folder records a Docker setup starts `vllm` from `PATH` instead
    (`local::vllm_launch`, outside the owned paths, with a unit test).
  - **SIGTERM** is listened for before the endpoint file is written, so one that arrives during
    start-up waits for the shutdown path; the test sends it to the real binary
    (`tests/server_mode.rs`, Unix only).
  - Tests: `crates/imperium-core/tests/server_mode.rs` (new), a server case in
    `tests/pr_automerge.rs` (outside the owned paths; it needs that file's fake GitHub),
    `crates/imperium-cli/tests/server_mode.rs` (new), unit tests in `peer.rs`,
    `app_identity/mod.rs`, `local.rs`, `vllm_env.rs` and the protocol's `system.rs`.
  - Docs: 02 gained [server mode](../02-architecture.md#server-mode-implemented) and the SIGTERM
    note, 05 the container tier and auto-merge on a server, 06 server mode in both approval
    sections and why there is no container rule.
- Handoff notes: server mode's refusals sit at the RPC boundary before any challenge is issued,
  and again inside `InstalledApp` and `peer::may_grant`, which take the daemon's state: a new
  caller of either (item 55's terminal panes, item 66's `files.write`) inherits the refusal
  without a check of its own. No subreaper is added (it would take agents' commands' exit
  statuses). A daemon in a container without server mode is open to double-forking; keep server
  mode on there.

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
  allows it and a fail-closed error otherwise; the session label names the backend. With
  `daemon.server` and the Landlock backend, the same ruleset is applied to every stdio MCP
  server and command hook except the user's named opt-outs (D7). First step:
  a CI probe that Docker's default seccomp profile allows the Landlock calls on the runner (a
  tiny static test binary in `docker run --rm` with the default profile); if not, add
  `deploy/server/seccomp.json` (default plus the three calls) and say so in D7 before going on.
- Depends on: none for the backend; stage 1 for the hook and MCP part (`daemon.server`). The
  server image consumes it in stage 7.
- Owned paths: `crates/imperium-tools/src/{sandbox.rs,sandbox_landlock.rs (new)}`,
  `crates/imperium-tools/Cargo.toml` (`landlock` crate, Linux only),
  `crates/imperium-config/src/settings.rs`, the spawn points of command hooks
  (`crates/imperium-extensions/src/lib.rs`) and MCP servers (`crates/imperium-mcp/src/servers.rs`),
  `crates/imperium-mcp/Cargo.toml` (a new path dependency on `imperium-tools`, which
  `imperium-mcp` does not have today; `imperium-tools` depends only on `imperium-api` and
  `imperium-policy`, so no cycle), the launch builder in `crates/imperium-core/src/mcp.rs`,
  `.github/workflows/ci.yml` (probe step), docs 05. If the hook and MCP part pushes the stage past
  about 800 lines it is split into 3a (backend) and 3b (hooks and MCP servers); 3b depends on
  stage 1 for `daemon.server`.
- How the decision reaches the spawn points: the spawners decide nothing themselves.
  `Launch::Stdio` gains an optional `confine: Option<imperium_tools::Confinement>` (the ruleset
  value `sandbox_landlock` builds, applied in the child before `exec`), and the command-hook
  runner in `imperium-extensions` (which already depends on `imperium-tools`) takes the same
  value. `imperium-core` fills it where it builds launches and hook invocations. When
  `daemon.server` is set and `isolation.os_backend` is `landlock`, it is `Some` for **every**
  stdio server and command hook, except a server or hook whose **origin is the user's
  `config.toml`** and whose name is in `mcp.unconfined` or `hooks.unconfined` there. Both lists
  are read from the user file only (the settings registry marks them user-file-only, so
  environment variables, the environment layer, project files and plugins cannot set them).
  The launch and hook specs already carry their origin (user config, trusted project
  `.mcp.json`, plugin); the check is `origin == UserConfig && list.contains(name)`, and a
  name in the list whose spec has another origin gets `Some`, a log line and a notification
  (name collision). Otherwise it is `None`, as today. No `cwd` test is made. The flag and the
  lists are read once per launch from the daemon's settings, never from the server's own
  config.
- Acceptance checks: `cargo test -p imperium-tools -p imperium-extensions -p imperium-mcp` on the
  Linux CI runner: a command can write in the workspace and its `TMPDIR`; reading a file under a
  stand-in for `/run/secrets`, the data folder and the socket folder (its `endpoint.json`
  included) fails with permission denied; with a stand-in `HOME` holding `.gitconfig`,
  `.ssh/known_hosts`, `.ssh/id_ed25519`, `.config/gh/hosts.yml` and `.git-credentials`, only the
  first two can be read, and writing anywhere in `HOME` fails; reading
  `/proc/<daemon pid>/environ` fails; in server mode, command hooks and MCP servers get the same
  denials, covering: a `.mcp.json` server of a trusted project whose `cwd` is outside the
  session's folders; a user-level server that runs a script living in a workspace; a server
  whose `cwd` is in another session's folder; a hook from the user's config. A server defined
  in the user's `config.toml` and named there in `mcp.unconfined` (and such a hook in
  `hooks.unconfined`) is not confined. **Name collision:** the user opts out `x`, and a trusted
  project's `.mcp.json` defines a server `x`; the project's `x` gets the same denials as any
  confined server, and the warning and notification name the project (the same for a plugin's
  server and a project hook with an opted-out name). The lists set through an environment
  variable, a project's `.imperium` files, an environment's config or a plugin are ignored (the
  server stays confined and a warning is logged); a `connect` to a loopback port fails with
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
  from one address gets its 401 late; **while that address is in back-off, a client from it with
  the correct token connects and completes the handshake within 1 s**. **Admission is a pure
  state machine** (`remote_tls::admission`): the rate queue, the pre-auth pool, the tarpit and
  the reserved slots, driven by an injected clock and a seeded RNG, with no sockets. Its unit
  tests are deterministic:
  - a scripted event stream (an attacker from the owner's address at 100 new connections a
    second keeping the pre-auth pool full, the owner retrying with back-off) admits the owner
    within 30 attempts on each of 10 fixed seeds;
  - a closed-form check computes per-attempt survival from the module's named parameters in
    `AdmissionParams` (`rate_per_addr` 16 a second, `preauth_cap` 32, and `owner_window`, the
    handshake-and-upgrade window the D3 claim assumes, 0.1 s), with the flood rate `r = 100`
    as the test's input, and asserts it is at least 0.15, so a change to any of these that breaks
    the D3 claim fails the build. The scripted stream uses the same `owner_window` for the
    owner's handshake duration;
  - with 32 unauthenticated connections held, a 33rd evicts one rather than being refused;
  - a flood of failed tokens fills only the tarpit (pre-auth slots stay free, its oldest entry
    is dropped at the 17th);
  - a handshake over the per-address rate waits in the queue and is admitted once the rate
    allows, and the queue drops its oldest entry when full.

  One real-socket smoke test, run once: a flooder with a fixed seed against the listener in
  process, and a correct-token client connects within 60 s. Further socket-level checks: an authenticated client still connects
  with all unauthenticated slots taken (the reserved slots); an idle TLS handshake is closed at
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
  `docker-compose.gpu.yml`, `secrets/README` (no secrets), `check-compose.py` (including the
  `prepare` shape of D2), and the image's
  environment (`IMPERIUM_HOME`, `IMPERIUM_DAEMON__SERVER`, `IMPERIUM_SECRETS__STORE`,
  `IMPERIUM_ISOLATION__TIER`, `IMPERIUM_ISOLATION__OS_BACKEND`, `IMPERIUM_DAEMON__SOCKET_DIR`,
  `IMPERIUM_WORKSPACES__ROOT`, `IMPERIUM_GITHUB__TOKEN_SOURCE`, `IMPERIUM_NO_DAEMON_SPAWN`).
- Depends on: stages 1 to 4 (the settings the image sets must exist).
- Owned paths: `deploy/server/**`, `scripts/ci/detect.sh` (the `server` output),
  `.github/workflows/ci.yml` (a build-only step until stage 8).
- Acceptance checks: in CI, `docker build` succeeds natively on `ubuntu-latest` (amd64) and
  `ubuntu-22.04-arm` (arm64); `check-compose.py` passes on every
  shipped file and fails on fixtures with a public port, a Docker socket mount, a secret in
  `environment`, a missing `read_only`, a second root service, an extra capability on `prepare`,
  `prepare` given a network, a secret or another command, an override that changes `prepare`, and
  one fixture each for `pid: host`, `privileged: true`, `network_mode: host`, `userns_mode: host`
  and `ipc: host` on the `imperium` service;
  `python3 -m pytest deploy/server` covers it; `prepare-volumes` under a test with a symbolic link
  to a file outside the volume leaves the target's owner unchanged, and with the roots already
  owned by the target UID it walks nothing (no recursive `chown` is run);
  `imperium config explain endpoints.llamacpp.base_url` in the container shows the value from the
  llama.cpp override's environment variable; the image has no `imperium-desktop`; under UID 1001 `id -un` and
  `ssh -V` work; the llama.cpp and vLLM pins match `runtimes/*.env`. `hadolint` and
  `actionlint` clean.
- Status: pending.
- Outcome:
- Handoff notes: the repository's compose file names the image by tag; stage 9 writes the
  digest into the release's copy.

## Stage 8: CI server job

- Objective: the `server` job of [D8](#d8-testing-in-ci) and its gate. On each leg
  (`ubuntu-latest` amd64, `ubuntu-22.04-arm` arm64): build that architecture's release binaries,
  build the image, `docker compose -f docker-compose.yml -f docker-compose.tls.yml -f
  deploy/server/ci/docker-compose.ci.yml up --wait` with test secrets generated in the job and
  `IMPERIUM_UID=$(id -u)`, then (steps 6 and 7 on the amd64 leg only):
  1. healthcheck healthy (`docker inspect --format '{{.State.Health.Status}}'`);
  2. start the runner's `sshd` with a throwaway key and run `imperium remote connect localhost
     --socket --server-dir "$PWD/run"` in the background (recorded, stopped at the end);
     `imperium --remote ci ping`;
  3. `imperium remote add tls wss://127.0.0.1:7443 --pin <from imperium server info>` with the
     token on standard input; `imperium --remote tls ping`; `curl` without a token and with a
     wrong one gets 401; a wrong pin fails;
  4. `imperium --remote ci run --headless --json --task deploy/server/ci/task.json --workspace
     /workspaces/ci --root-model http://fake-model:8080/v1#fake --permission-mode full`. The
     CLI checks that `--workspace` is a folder **on the client** before sending it
     (`crates/imperium-cli/src/eval.rs`, `run`), while the daemon uses the path on the server. So
     the job creates the same path on both sides: `sudo mkdir -p /workspaces/ci && sudo chown
     "$(id -u)" /workspaces/ci` on the runner, and the folder in the `imperium-workspaces` volume
     through `docker compose exec imperium mkdir -p /workspaces/ci`. The CLI's eval code is not
     changed (another session owns `eval*`); a server-aware workspace check is left for later. The
     fake model's scripted `run_command` runs, each separately so one failure does not hide the
     next: `cat /run/secrets/imperium-remote-token`, `cat /var/lib/imperium/data/daemon.json`,
     `cat /run/imperium/endpoint.json`, `ls /run/imperium`, `cat ~/.ssh/id_ed25519`,
     `cat ~/.config/gh/hosts.yml`, `cat ~/.git-credentials` (the CI override seeds those three
     `HOME` files with sentinels before the run, with `docker compose exec imperium` writing as the
     daemon's user outside any sandbox, so `prepare` keeps its fixed shape), and a token-less
     WebSocket connect to `/run/imperium/daemon.sock` with `curl --unix-socket`. The job asserts
     the run succeeded, the fake recorded the tool results, each read shows permission denied,
     the socket connect gets 401, and no token or sentinel appears anywhere in the result;
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
  (settings page, connection, badge, Plugins window), `apps/desktop/test/`, docs 07 (which also
  gains goal 7's list of what agents' commands cannot read on a server, shown in the server's
  settings page).
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
3. Default tier: `os` with Landlock, failing closed where a host lacks it (this plan, which the
   plan-gate review accepted now that `HOME` is limited to named files), or `policy` with a
   start-up warning that agents' commands can read every secret?
4. A writable store for credentials the daemon itself mints (MCP OAuth tokens), encrypted in the
   data volume with a key from a Docker secret: a later roadmap item, or never on a server?
5. Agents' commands on a server have no git credentials (the daemon's own pushes do). The
   answer is an **opt-in**: a per-environment setting that hands them a token or a deploy key
   from a Docker secret, off by default, as `github.session_gh` does on a desktop. Left open:
   whether it is built in this item (a stage after stage 3) or as a follow-up.
6. Phones through the TLS listener, with the existing scoped device tokens, are a **follow-up**
   item after this plan; the lockout-free limits of stage 5 are its prerequisite. Left open: when.
