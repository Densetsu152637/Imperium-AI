# Imperium-AI design documents

These documents describe what Imperium will be and how it will be built. They are the
source of truth until code exists; once code lands, each document is updated to describe the
implemented behaviour and to mark anything still aspirational.

| #   | Document                                          | Covers                                                        |
| --- | ------------------------------------------------- | ------------------------------------------------------------- |
| 01  | [Vision and scope](01-vision.md)                  | Goals, non-goals, platforms, principles                       |
| 02  | [Architecture](02-architecture.md)                | Processes, crates, protocol, repository layout, data paths    |
| 03  | [Agent engine](03-agent-engine.md)                | Sessions, the agent loop, reactive delegation, budgets        |
| 04  | [Models and routing](04-models-and-routing.md)    | Endpoints, local runtimes, model discovery, hardware, routing |
| 05  | [Tools and isolation](05-tools-and-isolation.md)  | Tools, MCP, web, code execution, isolation tiers, SSH         |
| 06  | [Extensibility](06-extensibility.md)              | Plugins, skills, hooks, UI mods, telemetry API, permissions   |
| 07  | [Desktop interface](07-desktop-ui.md)             | Layout, scaling, ultrawide, theming                           |
| 08  | [Agent policy](08-agent-policy.md)                | Agent-Custom-Workflow integration and overrides               |
| 09  | [Media generation](09-media-generation.md)        | Image, video, and audio models                                |
| 10  | [Configuration](10-configuration.md)              | Config layers, schema, secrets                                |
| 11  | [CI/CD and releases](11-ci-cd.md)                 | Checks, release channels, bundles, runtime packs              |
| 12  | [Roadmap](12-roadmap.md)                          | Phased delivery and exit criteria                             |
| 13  | [Decisions](13-decisions.md)                      | Decision log and open questions                               |
| 14  | [Projects and automation](14-projects-and-automation.md) | Projects, project agents, schedules                    |
| 15  | [Mobile and remote](15-mobile-and-remote.md)      | Phone app, pairing, connectivity                              |
| 16  | [Pets](16-pets.md)                                | Pixel companions driven by agent state                        |
| 17  | [Memory](17-memory.md)                            | Global and environment memory: storage, recall, writing, privacy  |
| 18  | [Workflow contract](18-workflow-contract.md)      | Public contract for custom workflows; context retrieval (RAG) |
| 19  | [Sharing and sync](19-sharing-and-sync.md)        | Session bundles, redaction, import; team sync design (designed) |

## Conventions

- **Must / should / may** carry their usual RFC 2119 meaning.
- Performance and cost figures are estimates until measured.
- A change to a contract (protocol, config schema, mod API) updates the owning document in the
  same pull request.
