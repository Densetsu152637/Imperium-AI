# Pattern guide: object-oriented design

Load under the [coding standards loading rule](../coding-standards.md#apply-these-rules) when choosing, implementing, or reviewing these patterns in non-trivial code. Existing project/framework conventions win; this guide refines the [coding conventions](../coding-conventions.md#language-defaults) defaults. Apply [function and class structure](structure.md) alongside. Do not change working code's paradigm merely to follow it.

## Use when / avoid when

Use when:

- Entities have identity, lifecycle, or state that must keep invariants across operations.
- The design needs substitution or plugin points, or a framework drives object lifecycles (UI, DI containers, ORMs).
- Behavior varies by type or configuration and callers should not know which variant they hold.

Avoid when:

- The work is a pure data transformation or pipeline: use [functional design](functional.md).
- The work is a hot loop over large homogeneous data: use [data-oriented design](data-oriented.md).
- A class would only hold unrelated utilities, wrap one function, or mirror one interface per class; use a function or module.

## Patterns

### Encapsulated invariants and value objects

Intent: a type owns its state and rejects invalid states; values compare by content. Use when rules span several fields or primitives carry domain meaning (money, ids, ranges). Avoid when the data has no rules; a plain data type suffices.

- Validate at construction; expose behavior and narrow accessors, not mutable internals.
- Default to immutable values; return copies or read-only views of internal collections.

### Composition and delegation

Intent: assemble behavior from collaborating objects instead of inheriting it. Use when reusing behavior or varying parts of an object. Avoid inheritance unless there is a true is-a relationship designed for extension.

- Hold collaborators in fields and delegate; seal or finalize classes not designed for extension.
- Tell, don't ask: put behavior with the data it uses.

### Strategy

Intent: swap an algorithm behind a small interface or function. Use when several interchangeable policies exist at runtime. Avoid for a single implementation with no foreseeable second one.

- Prefer a function type or lambda when the strategy is one operation; use an interface when it has several.

### Factory and builder

Intent: centralize construction. Use factories when the concrete type varies or construction can fail; builders when there are many optional parameters. Avoid when a constructor with named or default arguments suffices.

- Make constructors private behind a named factory when invariants need checking or caching.
- Return an immutable object from `build()`; validate there.

### Adapter and facade

Intent: adapt a foreign interface (adapter) or hide a subsystem behind a small API (facade). Use at integration boundaries with third-party, legacy, or I/O code. Avoid wrapping stable, well-fitting APIs.

- Translate types and errors at the boundary so foreign models do not leak into the domain.

### Decorator

Intent: add behavior (logging, caching, retries) by wrapping an object with the same interface. Use for cross-cutting behavior composable at runtime. Avoid deep wrapper stacks that hide order-dependent behavior.

- Keep the wrapper interface identical and delegate everything it does not change.

### Observer and events

Intent: notify subscribers without coupling publisher to subscribers. Use when several independent parties react to a change. Avoid for a single known caller; call it directly.

- Define who owns subscription lifetime and unsubscribe; handle subscriber failures without breaking the publisher.
- Beware re-entrancy and ordering assumptions; document delivery thread or context.

### Dependency injection

Intent: receive collaborators instead of creating or locating them. Use for I/O, clocks, and anything replaced in tests. Avoid service locators, global state, and injecting values that are plain parameters.

- Prefer constructor injection of required dependencies; keep the composition root at the application edge.
- Introduce an interface only where a real substitution or test boundary exists.

### State via sealed or closed types

Intent: represent states and variants as a closed set the compiler checks. Use for finite domain states, results, and messages. Avoid open hierarchies when new variants should be added by third parties.

- Use sealed types, enums, or tagged unions with exhaustive matching instead of flags and unchecked casts.
- Put state-specific data in the variant that owns it.

## Languages

### Java

- Use `record` for immutable data carriers and validate in the compact constructor; use classes for identity, lifecycle, or inheritance.
- Model closed hierarchies with `sealed` interfaces plus records/enums and exhaustive pattern-matching `switch`.
- Make fields `private final` and classes `final` by default; use constructor injection, not field injection or mutable statics.
- Use `Optional` only as a return type; never as a field or parameter. Do not return `null` collections.
- Use unchecked exceptions for programming errors, checked for recoverable conditions; never swallow exceptions, preserve causes, use try-with-resources.
- Override `equals`, `hashCode`, `toString` together (or use a record); use `List.of`/`copyOf` for immutable collections.
- Prefer `java.util.concurrent` over raw threads; use virtual threads for blocking I/O only where the project's JDK supports them.
- Use streams for declarative transforms and loops for side effects or early exit; run the project's formatter and static analysis (Checkstyle, Spotless, Error Prone, SpotBugs).

References: [Google Java Style Guide](https://google.github.io/styleguide/javaguide.html); [JEP 395: Records](https://openjdk.org/jeps/395); [JEP 409: Sealed Classes](https://openjdk.org/jeps/409); [SEI CERT Oracle Coding Standard for Java](https://cmu-sei.github.io/secure-coding-standards/sei-cert-oracle-coding-standard-for-java/); [Java Language Specification, SE 21](https://docs.oracle.com/javase/specs/jls/se21/html/index.html). Effective Java (Bloch) covers the design idioms.

### C#

- Use `record`/`record struct` for value objects and `init`-only or `required` members for immutable construction; seal classes not designed for extension.
- Model closed variants with abstract records or sealed hierarchies and `switch` expressions with pattern matching.
- Use constructor injection with the built-in DI container; register interfaces only at real boundaries.
- Enable nullable reference types and treat warnings as errors; use `IDisposable`/`using` for resources and `IReadOnlyList<T>` in signatures.
- Use events or `IObservable<T>` for observers and unsubscribe to avoid leaks; use `async`/`await` with `CancellationToken`.

References: [Common C# code conventions](https://learn.microsoft.com/en-us/dotnet/csharp/fundamentals/coding-style/coding-conventions).

### Kotlin

- Use `data class` with `val` for value objects and `copy` for updates; keep classes final unless designed for extension.
- Use `sealed interface`/`sealed class` with exhaustive `when` for state and result types.
- Use delegation (`by`) for composition and decoration; prefer default and named parameters over builders and overloads.
- Use read-only collection interfaces and `val`; use extension functions with restricted visibility instead of utility classes.
- Use constructor injection; use `object` only for stateless singletons.

References: [Kotlin coding conventions](https://kotlinlang.org/docs/coding-conventions.html).

### TypeScript

- Use classes only for meaningful state or lifecycle; otherwise use modules, functions, and plain typed objects.
- Use `readonly` fields, parameter properties, and `interface` for object shapes at real boundaries.
- Model variants as discriminated unions with exhaustive `switch` (a `never` check) instead of class hierarchies.
- Use constructor injection of interfaces or function types; prefer `unknown` to `any` and avoid non-null and type assertions.
- Return disposers from subscriptions and own their cleanup.

References: [Google TypeScript Style Guide](https://google.github.io/styleguide/tsguide.html).

### Python

- Use `@dataclass(frozen=True, slots=True)` for value objects; validate in `__post_init__`; use `default_factory` for mutable defaults.
- Use `typing.Protocol` for structural interfaces at boundaries instead of abstract base classes everywhere.
- Prefer composition and delegation; use mixins sparingly and keep inheritance shallow.
- Use `enum` and tagged dataclass unions with `match` for closed variants.
- Pass dependencies via `__init__`; use context managers (`with`) for resources.

References: [PEP 8](https://peps.python.org/pep-0008/); [dataclasses](https://docs.python.org/3/library/dataclasses.html); [Protocols](https://typing.python.org/en/latest/spec/protocol.html).

### C++

- Use value semantics and RAII; follow the rule of zero (or five).
- Prefer composition to inheritance; use `final`, virtual destructors for polymorphic bases, and `override` consistently.
- Use `std::variant` with `std::visit` or `enum class` for closed variants; use `const` and immutable members by default.
- Pass dependencies via constructors using references or smart pointers that state ownership; avoid raw owning pointers.
- Use templates/concepts for compile-time polymorphism and virtual dispatch only for runtime substitution.

References: [C++ Core Guidelines](https://isocpp.github.io/CppCoreGuidelines/CppCoreGuidelines): see section C, Classes and class hierarchies.

## References

- [Refactoring.Guru: Design Patterns](https://refactoring.guru/design-patterns): catalogue of patterns with intent, structure, and trade-offs.
