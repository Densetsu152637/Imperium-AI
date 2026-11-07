# Function and class structure

Language-agnostic rules for reuse and structure. Load under the [coding standards loading rule](../coding-standards.md#apply-these-rules) when implementing, designing, or reviewing non-trivial code. Existing project conventions take precedence. Pair with the paradigm guide that fits the code: [object-oriented](object-oriented.md), [data-oriented](data-oriented.md), [functional](functional.md).

## Function reuse

- Search for and use existing project helpers before writing new ones.
- Extract shared code when the duplicates share one reason to change. Three similar copies is a guide, not a law.
- Do not merge look-alike code that changes for different reasons; coincidental similarity is not duplication.
- Add no generic helpers, parameters, or extension points for hypothetical callers.
- Place a shared helper in the lowest module that owns the concept, not in a grab-bag `utils` file.
- Prefer a small composable function over a configurable one with many modes.
- When changing a shared helper, check every caller.
- When consolidating, remove the dead duplicates and update their callers.

## Function structure

- Give each function one purpose at one level of abstraction; do not mix orchestration with low-level detail.
- Name it for what it does or returns.
- Prefer guard clauses and early return over deep nesting.
- Keep side effects explicit; separate queries (return a value) from commands (change state).
- Prefer pure transformations for business rules and server logic; keep I/O and orchestration at clear boundaries.
- Avoid boolean mode flags and long positional argument lists; use separate operations or a typed options group.
- Handle empty, invalid, and missing values deliberately; do not invent silent defaults that erase domain distinctions.
- Do not force currying, one-line functions, recursion, or chained expressions when straightforward control flow reads better.
- Let length follow one idea, not a line count: split when a function holds several ideas, not to hit a number.

## Class and module structure

- Give each class or module one cohesive responsibility and one reason to change.
- Establish invariants in constructors or factories; do not allow half-built or invalid instances.
- Hide internals and expose behavior, not raw state.
- Point dependencies inward: domain logic does not depend on I/O or framework details; inject them.
- Do not create a class solely to hold unrelated utilities. Prefer composition to inheritance; introduce interfaces at real substitution, integration, or testing boundaries, not one per class.
- Model domain states with types, enums, or tagged unions; avoid unchecked casts and overly broad types.
- Avoid god classes and modules; split by responsibility when one unit changes for unrelated reasons.
- Organise files and modules by feature or domain, following project convention.
- Keep the public surface minimal; export only what callers need.

## References

- [Refactoring catalog](https://refactoring.com/catalog/): named refactorings for extraction, consolidation, and splitting.
- [Google style guides](https://google.github.io/styleguide/): per-language conventions to check when the project has none.
