# Research paper documentation

Load for research undertaken to write or maintain a research paper/manuscript, or refactoring a research implementation with a maintained paper. Ordinary lookups, software comparisons, and papers used only as sources do not trigger these persistent artifacts.

## Retain and reuse research results

Apply this section only under the [paper documentation scope](AGENTS.research.md#scope-and-documentation).

- Record every paper-related research goal or prompt result for future agents, including inconclusive or negative outcomes. Use the project's existing research documents or evidence ledger; give each record a stable ID and link it from a discoverable index at a persistent path. Preserve the goal and exact prompt (redacting secrets), scope and assumptions, methods/settings, dates and source or implementation revisions, results, supporting evidence, and limitations.
- Before new research, consult relevant prior records. Record changed scope, methods, or context and explain conflicting findings in a new entry; do not overwrite prior records.
- Add every source used to the repository's existing BibTeX bibliography; if none exists, create `paper/references.bibtex` under the repository root. Reuse existing entries and citation keys; verify metadata against the source and omit unavailable fields rather than inventing them. Reference citation keys in retained records.

## Update the paper after a refactor

- When refactoring a research implementation with a maintained paper, update the paper to reflect the final implementation under [documentation alignment](../guides/coding-standards.md#documentation-alignment). Map code changes to affected paper content before editing; this does not require creating a paper for an ordinary software task.
- Update only affected parts, including dependent claims, equations, algorithm descriptions, figures, tables, or cross-references. Do not rewrite the whole paper or make unrelated editorial changes.
- Check each changed passage against the final code and validation evidence, and report affected paper sections with the corresponding implementation changes. If the refactor invalidates reported results, mark them as needing revalidation and update measurements only from actual reruns. Record inaccessible paper sources or unresolved discrepancies as documentation gaps.
