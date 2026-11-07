# Run GitHub Actions locally

Load when considering or executing act rehearsal for supported jobs. Workflow authoring and hosted gates remain in [GitHub Actions](github-actions.md); local success does not establish hosted behavior, except as the narrowly conditioned [local act fallback](git-publication.md#pull-requests-and-merge-rules) merge gate, whose conditions that owner defines. For act-only rehearsal, this owner covers tool-managed runner containers; no application Compose procedures apply. Load [container usage](container-usage.md) only when managing application container or Compose services relevant to the rehearsal.

## Run workflows locally with act

Linux container jobs need Docker Engine or Docker Desktop (Linux containers on Windows). Install [nektos/act](https://nektosact.com/installation/index.html) with one package manager already on the host:

```text
# Homebrew (macOS/Linux)
brew install act
# Chocolatey (Windows)
choco install act-cli
# Scoop (Windows)
scoop install act
```

From the consuming repository root:

```text
act pull_request -l
act pull_request
act pull_request -W .github/workflows/checks.yml -j checks
act pull_request -W .github/workflows/checks.yml -j checks -e event.json
```

Use a sanitized `event.json` containing the PR fields your expressions read, for example:

```json
{"pull_request":{"number":42,"head":{"ref":"feature/example"},"base":{"ref":"main"}}}
```

Select a runner image with the required tools; act's default images differ from hosted runners, and Linux images do not reproduce native Windows/macOS jobs. act's opt-in [host execution](https://nektosact.com/usage/runners.html) runs directly on the machine and still does not recreate GitHub's hosted image.

Provide only needed local secrets using act's secure prompt, such as `act pull_request -s MY_SECRET`; use ignored secret files only under [secret handling](environment-configuration.md#secret-handling). GitHub's automatic token and environment secrets are not supplied locally. See [act usage](https://nektosact.com/usage/index.html).

act ignores concurrency, job permissions, and job timeouts, among other [unsupported features](https://nektosact.com/not_supported.html); event context, token permissions, caches, and hosted services can also differ. A local success does not verify those controls.
