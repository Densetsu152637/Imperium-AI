# GitHub Actions

This guide owns workflow authoring and local iteration. [Git publication](git-publication.md#pull-requests-and-merge-rules) owns merge gates; [validation](validation.md#shared-validation-rules) owns check evidence; [environment configuration](environment-configuration.md#secret-handling) owns secret handling.

## Author workflow files

- Inspect existing workflows, project scripts, required checks, and runner requirements first. Use the same formatter, linter, build, and test commands locally and in CI; keep application logic in project scripts, not YAML.
- Use `pull_request` for PR checks and restrict `push` to branches needing post-merge validation to avoid duplicate runs on feature branches. Add `merge_group` when the repository's merge queue requires it. Keep required job names stable.
- Install dependencies from the committed lockfile. Follow repository action-pinning rules; otherwise prefer reviewed full commit SHAs.
- Start with minimal `permissions`, typically `contents: read`; grant more only to jobs that need it. Untrusted PR code must not execute with privileged tokens or deployment secrets. See [GitHub token permissions](https://docs.github.com/en/actions/tutorials/authenticate-with-github_token).

Adapt this skeleton to the consuming project. `scripts/check.sh` represents its existing check script; add its documented toolchain/dependency setup before running. The action tag is illustrative; resolve an approved version or SHA first.

```yaml
name: Checks
on:
  pull_request:
  push:
    branches: [main]
permissions:
  contents: read
concurrency:
  group: ${{ github.workflow }}-${{ github.event_name }}-${{ github.event.pull_request.number || github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}
jobs:
  checks:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@v6
        with:
          persist-credentials: false
      # Add the project's runtime and locked dependency installation here.
      - name: Project checks
        shell: bash
        run: bash scripts/check.sh
```

Syntax and events: [GitHub workflow reference](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax). Checkout requirements: [actions/checkout](https://github.com/actions/checkout).

## Fast local checks

Run the project's check scripts directly first. Use [actionlint](https://github.com/rhysd/actionlint) for workflow syntax and expression errors; plain YAML validation does not understand Actions semantics.

```text
actionlint
```

Use [pre-commit](https://pre-commit.com/) for fast formatter, lint, whitespace, and workflow checks, adding hooks to the project's `.pre-commit-config.yaml` with its existing pinned versions; actionlint documents a [pre-commit integration](https://github.com/rhysd/actionlint/blob/main/docs/usage.md#pre-commit). After pre-commit and hook dependencies are installed:

```text
pre-commit install
pre-commit run --all-files
```

Keep expensive integration tests and full container runs outside the default fast hook; run the same relevant checks in CI as well.

## Control hosted runtime

- Use positive path filters for optional expensive workflows (source, tests, dependency manifests/lockfiles, build configuration, shared scripts, `.github/workflows/**`). Example fragment, adapted to actual project paths:

  ```yaml
  on:
    pull_request:
      branches: [main]
      paths:
        - 'src/**'
        - 'tests/**'
        - 'scripts/**'
        - 'package.json'
        - 'package-lock.json'
        - '.github/workflows/**'
  ```

  Branch and path filters must both match; PR branch filters target the base branch. A workflow skipped by path/branch filtering can leave required checks Pending and block merging. Keep required workflows triggered for every relevant PR; filter expensive jobs inside them and provide an always-run required gate that reports failures and handles intentional skips. Do not apply this optional fragment to the required-check skeleton above. See [filter semantics and required-check warning](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#onpushpull_requestpull_request_targetpathspaths-ignore).
- Set strict, realistic job `timeout-minutes` and tighter step timeouts for potentially hanging commands.
- Cache package-manager downloads with a supported setup action or `actions/cache`; custom keys include OS, toolchain version, and lockfile hash; still run the locked install after restoring. Never cache secrets. See [GitHub dependency caching](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching).
- Cancel obsolete PR runs using a group scoped to workflow, event, and PR/ref, as in the skeleton, so independent PRs/workflows cannot cancel one another. Deployment cancellation needs a separate decision. See [GitHub concurrency](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency).

## Verify before handoff

Run static checks and the relevant project scripts, then load [local rehearsal](github-actions-local.md) and use act for supported jobs. Exercise event-dependent conditions with representative payloads (PR/base branches, missing optional fields). Verify path-filter cases (source, docs-only, lockfile, workflow edits), fork permissions, cache misses/hits, timeouts, and cancellation on GitHub where local execution cannot establish behavior. Report unrun hosted checks explicitly under the linked validation guide.
