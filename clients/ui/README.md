# Shared desktop UI

This package contains the common TypeScript UI used by both Tauri shells. It
owns the user-facing registration, room list, create/join flow, room member
view and relay latency display. Admin-only transport diagnostics and server
addresses are intentionally not rendered in this client UI. The native
projects retain only a tiny `src/main.ts` that injects the platform name and
Tauri `invoke` function.

The package intentionally does not depend directly on `@tauri-apps/api`.
Keeping `invoke` injected makes the package easy to type-check and prevents
local `file:` package installs from creating different nested Tauri API
dependency trees on Windows and macOS.

The platform shells must expose the same command names and JSON shapes. Native
differences belong in the Windows WFP service or macOS Network Extension, not
in a second copy of the UI. The current control-plane command contract is still
being wired to account-backed user authentication; until then, the room actions
use the native shell's hidden connection configuration rather than exposing
admin settings to end users.
