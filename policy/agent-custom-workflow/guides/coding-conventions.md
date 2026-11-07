# Coding conventions

Use the [coding standards loading rule](coding-standards.md#apply-these-rules) to select relevant sections. Existing project/framework conventions take precedence; this reference supplies defaults where they are missing.

## Language defaults

FP = functional programming
OOP = object-oriented programming
DOD = data-oriented design

| Language                | Default paradigm and emphasis                                                                             |
| ----------------------- | --------------------------------------------------------------------------------------------------------- |
| Haskell                 | FP; pure transformations, algebraic data types, explicit effects                                          |
| C++                     | OOP/DOD with value semantics, RAII, composition, and generic algorithms and templates                     |
| C                       | DOD; Procedural/structured; explicit data ownership, lifetimes, and cleanup                               |
| C# / Java               | OOP; cohesive types, interfaces at boundaries, composition                                                |
| TypeScript / JavaScript | FP; Functional composition for transformations; modules/components; OOP for meaningful state or lifecycle |
| Python                  | Procedural/functional core; classes for domain state and behavior; idiomatic iteration                    |
| Rust                    | DOD; Ownership-oriented, structs/enums/traits and functional iterators                                    |
| Go                      | Procedural composition; small interfaces, explicit errors and concurrency ownership                       |
| Kotlin / Swift          | OOP + functional/value-oriented design; idiomatic framework conventions                                   |
| F# / OCaml / Scala      | FP-first; algebraic modeling and explicit effects; idiomatic interop                                      |
| SQL                     | Declarative, set-based queries; explicit joins, constraints, and transactions                             |
| Shell / PowerShell      | Procedural orchestration; small commands/functions, quoted paths, explicit failure handling               |

Prefer local, controlled mutation when it makes an algorithm clearer or more efficient; avoid hidden shared mutable state.

Function reuse, function and class structure, and paradigm patterns with per-language idioms are in the [pattern guides](patterns/README.md).

## Errors and asynchronous work

- Use the existing error strategy. Catch only to recover, add context, clean up, or present a safe error; do not swallow errors or expose tokens, raw sensitive responses, or stack traces in user-facing messages.
- Await or explicitly handle every asynchronous operation; a `void` prefix does not itself handle a rejected promise. Own cancellation, loading state, and cleanup.
- Prevent stale or cancelled requests from overwriting current state, including error and loading state; use cancellation plus a current-request/lifecycle guard where cancellation alone cannot guarantee this.
- Validate HTTP status, decoding, and application-level success per the endpoint contract. Reuse shared request helpers; do not introduce a second `Result`/`Either` framework for routine requests when existing Promise/error composition suffices.

### Existing helper conventions

Inspect and use shared helpers and preserve connector rejections.

React effects must handle their background promises and cancel in-flight work on cleanup. Ignore expected cleanup aborts; guard state updates from superseded requests, including updates in `finally`. Event handlers must handle awaited connector failures; use `void` in JSX only when the invoked handler handles its own rejection. Verify these helper paths/APIs exist; do not assume every project contains this web-app or copy missing helpers blindly.
