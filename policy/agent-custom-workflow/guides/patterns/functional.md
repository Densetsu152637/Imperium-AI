# Pattern guide: functional programming

Load under the [coding standards loading rule](../coding-standards.md#apply-these-rules) when choosing, implementing, or reviewing these patterns in non-trivial code. Existing project/framework conventions win; this guide refines the [coding conventions](../coding-conventions.md#language-defaults) defaults. Apply [function and class structure](structure.md) alongside. Do not change working code's paradigm merely to follow it, and do not force currying, point-free style, or recursion when plain control flow reads better.

## Use when / avoid when

Use when:

- Encoding business rules, calculations, and state transitions that can be expressed as input to output.
- Transforming, filtering, or aggregating collections and records.
- Parsing and validating untrusted input into trusted domain types.
- Writing logic that must be safe under concurrency, easy to test, or easy to replay.

Avoid when:

- The code is a hot loop over large mutable buffers where copying costs matter; see [data-oriented design](data-oriented.md).
- A framework owns the lifecycle and state (UI components, ORMs, services with managed resources); see [object-oriented design](object-oriented.md).
- Local mutation is clearer or faster and does not escape the function.

## Patterns

### Functional core, imperative shell

Keep decisions pure; push I/O, time, randomness, and shared mutation to thin edges.

- Use when logic needs fast, deterministic tests. Avoid when the function is only I/O glue.
- Core functions take values and return values or descriptions of actions; the shell reads input, calls the core, and performs effects.
- Pass dependencies (clock, ids, config) as arguments instead of reading globals.

### Algebraic data types, illegal states unrepresentable

Model alternatives as sum types and combinations as product types.

- Use when a value is in one of several distinct states with different data. Avoid when a plain record with independent fields suffices.
- Replace flag-plus-optional-field combinations with one variant per state.
- Match exhaustively with no catch-all default so new variants fail compilation or review.

### Option/Result for expected failure

Return absence and expected errors as values; reserve exceptions for bugs and unrecoverable faults.

- Use when callers can reasonably recover or branch. Avoid when the project already has an error strategy; follow it ([errors and asynchronous work](../coding-conventions.md#errors-and-asynchronous-work)).
- Make functions total: every input yields a value of the declared type.
- Do not add a second `Result`/`Either` framework when existing error composition suffices.

### Pipelines and composition

Build behavior from small named steps applied in sequence.

- Use when a result is a chain of transformations. Avoid when a chain hides control flow or exceeds what a reader can hold.
- Name intermediate values or steps instead of nesting or one-line chains.
- Keep each step pure and independently testable.

### Immutability and copy-on-update

Treat values as unchangeable; derive new values instead of editing in place.

- Use when values are shared, cached, compared, or passed across async or thread boundaries. Avoid when copying dominates cost; mutate locally and return the result.
- Mark data read-only in the language where supported.
- Update with copy-with-changes constructs, not mutation of arguments.

### Higher-order functions and small combinators

Pass behavior as arguments; use `map`, `filter`, `fold`, `flatMap`, and small project-specific combinators.

- Use when only the varying step differs between several loops. Avoid when a callback only wraps a single call or obscures stack traces.
- Prefer a named function over a nested anonymous one beyond a single expression.
- Do not build generic combinators for a single use.

### Parse, don't validate

Convert untrusted input into a precise type once at the boundary; downstream code accepts only the parsed type.

- Use when input crosses a trust boundary (network, storage, user, config). Avoid re-checking the same invariant in the core.
- Return a typed value or an explicit error; never a validated-but-unchanged raw value.
- Hide raw constructors behind the parser when invariants exist.

## Languages

### Haskell

- Make functions total: avoid `head`, `tail`, `fromJust`, `!!`, `read`, and incomplete patterns; use `NonEmpty`, pattern matching, or safe lookups, with incomplete-pattern warnings as errors in CI.
- Use `newtype` and algebraic data types for domain values and states; hide constructors behind smart constructors when invariants exist.
- Keep `IO` at the edge; follow the project's effect style (mtl, effect library, or `ReaderT`) without mixing styles. Represent failure with `Maybe`, `Either e`, or `ExceptT`.
- Use strict fields, `foldl'`, bang patterns, or strict maps for accumulators to avoid space leaks; prefer `Text` over `String`; avoid lazy I/O.
- Give top-level definitions type signatures, explicit export lists, and qualified imports; run HLint and the project formatter (ormolu or fourmolu).
- [HaskellWiki Programming guidelines](https://wiki.haskell.org/Programming_guidelines): shared style and good-practice rules.
- [Kowainik Haskell Style Guide](https://kowainik.github.io/posts/2019-02-06-style-guide): practical style, imports, and naming.
- [HLint](https://github.com/ndmitchell/hlint): linter suggesting simpler, more idiomatic code.
- [GHC User's Guide](https://downloads.haskell.org/ghc/latest/docs/users_guide/): compiler flags, warnings, and language extensions.

### TypeScript

- Enable `strict` (with `noUncheckedIndexedAccess` where allowed); never weaken it to silence an error.
- Model variants as discriminated unions with a literal tag; switch exhaustively with a `never` check.
- Use `readonly`, `Readonly<T>`, and `as const`; return new values instead of mutating arguments.
- No `any`: take `unknown` at boundaries, narrow with type guards, and validate untrusted input at runtime with the project's validator; avoid unchecked `as` and `!`.
- Use `map`/`filter`/`reduce` where clearer; use classes only for meaningful state or lifecycle; handle promises per [errors and asynchronous work](../coding-conventions.md#errors-and-asynchronous-work); use the project's ESLint (`no-explicit-any`, `no-floating-promises`, `switch-exhaustiveness-check`) and formatter.
- [TypeScript Handbook](https://www.typescriptlang.org/docs/handbook/): official language reference (narrowing, unions, generics).
- [Google TypeScript Style Guide](https://google.github.io/styleguide/tsguide.html): conventions for types, imports, and language features.
- [typescript-eslint rules](https://typescript-eslint.io/rules/): index of type-aware lint rules.

### Rust

- Model states with `enum` and exhaustive `match`; avoid wildcard arms where new variants should force a decision.
- Return `Result<T, E>` and `Option<T>` and propagate with `?`; reserve `unwrap`/`expect` for proven invariants.
- Use iterator adapters (`map`, `filter`, `fold`, `collect`) when clearer than index loops; bindings are immutable by default, so add `mut` only locally.
- Use newtypes and private fields with constructors that parse input into valid values.
- [The Rust Programming Language](https://doc.rust-lang.org/book/): official book covering enums, pattern matching, error handling, and iterators.

### Python

- Use frozen dataclasses (`@dataclass(frozen=True)`) for immutable records and `dataclasses.replace` for copy-on-update.
- Prefer comprehensions and generator expressions to manual accumulator loops; use `functools` and `itertools` for composition, and `def` over complex `lambda`.
- Model variants with union types and `match` statements with a final assertion on unexpected cases; keep functions free of module-level mutable state.
- Parse input into typed objects at the boundary and raise or return an explicit error for invalid input.
- [Functional Programming HOWTO](https://docs.python.org/3/howto/functional.html): iterators, generators, comprehensions, `itertools`, and `functools`.
- [dataclasses](https://docs.python.org/3/library/dataclasses.html): frozen records, `replace`, and field options.

### Java

- Use records for immutable data and sealed interfaces for closed variant sets; switch over them with pattern matching and no default branch.
- Use streams for transformations when clearer than loops; keep lambdas short and side-effect free.
- Use `Optional` only as a return type for absence, never as a field or parameter; return `Optional` or a project result type for expected failure.
- Parse input into validated types in constructors or factories; make collections unmodifiable (`List.of`, `Collections.unmodifiableList`).
- [JEP 409: Sealed Classes](https://openjdk.org/jeps/409): restricting which types may extend a type.
- [JEP 441: Pattern Matching for switch](https://openjdk.org/jeps/441): exhaustive switch over sealed hierarchies and records.

## References

- [Parse, don't validate](https://lexi-lambda.github.io/blog/2019/11/05/parse-don-t-validate/): type-driven design for making illegal states unrepresentable.
