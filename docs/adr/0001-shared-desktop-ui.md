# ADR 0001: Share the desktop UI package

状态：Accepted

## Context

Windows and macOS must expose the same room, relay and diagnostic workflow.
Keeping a full `main.ts`, markup, styles and validation implementation in both
Tauri projects would make every UI fix a two-file change and would allow the
platforms to drift silently.

## Decision

Use `clients/ui` as the single TypeScript/Vite UI package. The two desktop
projects keep only the platform shell:

- `src/main.ts` injects the platform label and Tauri `invoke` function;
- the platform Rust library exposes the same command names and response shapes;
- `tsconfig.json` extends the shared UI config;
- each platform `package.json` remains local because Tauri bundling and the
  platform package name are release concerns.

The native network boundary stays outside the shared UI: Windows WFP/service
integration and macOS Packet Tunnel integration must not leak into TypeScript.

## Consequences

UI behavior, validation, styles and control-plane workflow are changed once.
Platform-specific behavior is tested through the injected command contract and
the native Rust/Network Extension tests. A future Tauri workspace may reduce
manifest duplication further, but it is not required for the first release and
must not reintroduce platform logic into the shared UI.

## Verification

Both `win-client` and `mac-client` run the same `npm run build` path in CI. The
only expected differences in their frontend entrypoints are the platform
literal and the native Tauri project configuration.
