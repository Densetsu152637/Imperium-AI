# Pattern guides

Load under the [coding standards loading rule](../coding-standards.md#apply-these-rules) when designing, implementing, or reviewing the structure of non-trivial code. Existing project/framework conventions win; the [language defaults](../coding-conventions.md#language-defaults) give each language's default paradigm. Pair [function and class structure](structure.md) with the paradigm guide that fits the code; mixed code (for example an object-oriented shell around a data-oriented core) uses each guide for its part.

| Guide | Use for | Languages linked |
| ----- | ------- | ---------------- |
| [Function and class structure](structure.md) | Function reuse; function, class, and module structure in every paradigm | All |
| [Object-oriented](object-oriented.md) | Entities with identity or lifecycle, substitution points, classic design patterns | Java, C#, Kotlin, TypeScript, Python, C++ |
| [Data-oriented](data-oriented.md) | Hot loops over many similar items, engines and simulation, batch processing | C, C++, Rust, C# (Unity DOTS), Java |
| [Functional](functional.md) | Business rules, data transformation, parsing and validation | Haskell, TypeScript, Rust, Python, Java |

Load only the language sections for the code at hand. For unlisted languages, use the language's official style guide or the [Google style guides](https://google.github.io/styleguide/) when the project sets none. Add a guide only for a recurring pattern family, and a language section only where that language is commonly used with the pattern.
