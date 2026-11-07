# Pattern guide: data-oriented design

Load under the [coding standards loading rule](../coding-standards.md#apply-these-rules) when choosing, implementing or reviewing these patterns in non-trivial code. Existing project/framework conventions win over this guide, which refines the [coding conventions](../coding-conventions.md#language-defaults) defaults. Apply [function and class structure](structure.md) alongside. Do not change working code's paradigm merely to follow it. Per [measure before claiming performance gains](../coding-standards.md#validation-and-completion), layout changes need profiling evidence.

## Use when / avoid when

Use when:

- Hot loops process many similar items (thousands or more per frame or batch).
- Games, simulations and engines with large entity counts and fixed time budgets.
- Batch, numeric or pipeline processing where throughput and memory footprint matter.

Avoid when:

- Low-volume business logic, where domain modeling clarity outweighs layout: see [object-oriented](object-oriented.md) or [functional](functional.md).
- I/O-bound or latency-dominated code, where memory layout is not the bottleneck.
- No profile shows the data access pattern as a cost.

## Patterns

### Struct of arrays vs array of structs

Intent: store fields so the data a loop touches is contiguous.

- Use SoA when a hot loop reads few fields of many records; use AoS when most fields are used together or records are accessed individually.
- Start with AoS for simplicity; switch to SoA (or hybrid chunks) only on measured evidence.

### Hot/cold splitting

Intent: keep frequently touched fields dense and move rarely used ones aside.

- Use when records are large but loops read a small subset; avoid for small records that already fit a cache line.
- Move cold fields (names, debug data, config) to a parallel array or side table keyed by the same index.

### Batch transforms over collections

Intent: process many items per call instead of dispatching per object.

- Write functions taking a range/slice and returning or mutating a range; keep per-item branches out of the inner loop.
- Partition by type or state first, then run one tight loop per partition instead of virtual or dynamic dispatch per item.

### Entity-component-system

Intent: compose entities from plain data components processed by systems in bulk.

- Use when entity kinds vary and are created/destroyed at scale; avoid for a handful of long-lived objects.
- Entities are IDs; components are plain data in contiguous storage; systems are functions over component queries.

### Arenas, pools and generational handles

Intent: control allocation and lifetime without per-element heap objects.

- Use arenas for data that dies together, pools for fixed-size recycled items, handles for stable references into relocating storage.
- Refer by index or handle with a generation counter so stale references are detected rather than dangling.
- Free in bulk (reset the arena); avoid per-item allocation and deallocation in hot paths.

### Contiguous containers and views

Intent: expose data as contiguous memory and pass non-owning views.

- Prefer one contiguous array per data kind; pass pointer plus length, span or slice rather than owning containers.
- Avoid linked lists, node-based maps and pointer-heavy trees on hot paths; use sorted arrays or index-based structures.

### Data-driven tables instead of type hierarchies

Intent: replace class hierarchies that differ only by data with rows in a table.

- Use when variants differ in parameters, not behavior; avoid when variants need genuinely different logic.
- Store parameters in an array indexed by an enum or ID; keep the few behavioral differences in a switch or small function table.

## Languages

### C

- Give every allocation one owner; release with a single exit or `goto cleanup` in reverse acquisition order.
- Use `size_t` for sizes and indices; check arithmetic before allocating or indexing; bounds-check every buffer.
- Pass pointer plus length together; avoid unchecked string functions; use `const`, and `restrict` only when aliasing is ruled out.
- Hide layout behind opaque types at module boundaries; use plain struct arrays or parallel arrays for hot data.
- Build with `-Wall -Wextra` and run sanitizers (ASan, UBSan) and static analysis where the project allows.

References:

- [SEI CERT C Coding Standard](https://cmu-sei.github.io/secure-coding-standards/sei-cert-c-coding-standard/): rules for secure C.
- [Linux kernel coding style](https://www.kernel.org/doc/html/latest/process/coding-style.html): idioms for error-path cleanup in large C codebases.

### C++

- Use RAII and the rule of zero; `std::unique_ptr` for ownership; no naked `new`/`delete`.
- Store hot data in `std::vector`/`std::array`; pass `std::span` views; use indices or handles, not pointers that reallocation invalidates.
- Avoid virtual dispatch in hot loops; partition data by type and run direct calls or `std::variant` visitation per batch.
- Prefer standard algorithms and ranges; mark `const`, `constexpr`, `noexcept` and `[[nodiscard]]` where correct.
- Follow the project's error model (exceptions or error codes/`std::expected`); do not mix styles within a module.

References:

- [C++ Core Guidelines](https://isocpp.github.io/CppCoreGuidelines/CppCoreGuidelines): resource management, interfaces and performance rules.
- [Google C++ Style Guide](https://google.github.io/styleguide/cppguide.html): conventions for classes, ownership and headers.

### Rust

- Prefer plain ownership and borrowing over `Rc`/`RefCell`/`Arc<Mutex>` unless sharing is required.
- Use `Vec<T>` with `&[T]`/`&mut [T]` slices; model states with enums; prefer iterators over indexed loops.
- Use index/arena/slotmap or ECS patterns with typed IDs for graph-like data instead of reference cycles.
- Return `Result` with `?` and context; no `unwrap`/`expect` in library code except on proven invariants.
- Minimise `unsafe` behind a small safe API with a `// SAFETY:` comment on each block; keep `cargo clippy` and `cargo fmt` clean.

References:

- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/): checklist for idiomatic public APIs.
- [Rust Style Guide](https://doc.rust-lang.org/style-guide/): default formatting implemented by `rustfmt`.
- [Google Rust Style Guide](https://google.github.io/styleguide/rustguide.html): conventions for ownership, error reporting, traits, naming and `unsafe`.
- [Clippy Lints](https://clippy.rust-lang.org/master/index.html): lint catalogue for correctness, performance and idiom.
- [The Rustonomicon](https://doc.rust-lang.org/nomicon/): rules for sound `unsafe` code.

### C# (Unity DOTS)

- Use Unity Entities: unmanaged `struct` components (`IComponentData`), systems over queries, ECS as the entity-component-system pattern.
- Run bulk work in Jobs compiled with Burst; keep job data blittable.
- Store data in `NativeArray`/`NativeList`; dispose explicitly and respect job safety and dependency rules.
- Avoid managed allocations, boxing, LINQ and `class` components in hot paths.

References:

- [Unity Entities package](https://docs.unity3d.com/Packages/com.unity.entities@latest/): ECS, components and systems.
- [Burst compiler](https://docs.unity3d.com/Packages/com.unity.burst@latest/): compiling job code to optimised native code.
- [Unity job system](https://docs.unity3d.com/Manual/job-system.html): scheduling safe multithreaded jobs.

### Java

- Use primitive arrays or parallel arrays (`int[]`, `float[]`) for hot data instead of collections of objects.
- Avoid boxing (`List<Integer>`, `Map<Long, ...>`); use primitive-specialised structures.
- Avoid per-object allocation in game or processing loops; reuse buffers and pool short-lived objects.
- Keep classes small and field-dense; process in index loops, not by chasing object references.

## References

- [Data-Oriented Design (Richard Fabian)](https://www.dataorienteddesign.com/dodbook/): foundations, structs of arrays and the measure-analyse-implement-confirm loop.
