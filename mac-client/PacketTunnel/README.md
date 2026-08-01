# macOS Packet Tunnel target

`PacketTunnel.xcodeproj` is the minimal Network Extension target used by CI.
`PacketTunnelProvider.swift` is its lifecycle boundary. It creates a narrow
virtual route for Civ6's limited broadcast and the service virtual subnet,
then reads packets from `NEPacketTunnelFlow`.

`IPv4UDP.swift` owns bounded IPv4/UDP parsing and packet reconstruction,
including IPv4 and UDP checksums. `RelayEnvelope.swift` mirrors the shared
Rust envelope kinds and has a Swift smoke test under `Tests/ProtocolSmoke.swift`.
The smoke test is compile-and-run checked in the macOS CI job and in the Swift
Linux container used for local protocol validation.

`Civ6PacketRouter.swift` maps only the supported discovery/gameplay packets to
the envelope and back. It keeps discovery request IDs by source virtual IP so
multiple host sessions remain distinguishable; it does not forward arbitrary
IP traffic.

The provider is deliberately marked as a Phase 3 integration target: the
current Linux workspace can compile the shared Rust protocol and Tauri UI, but
cannot compile or sign Apple's Network Extension. The macOS CI job type-checks,
builds an unsigned `.appex`, and embeds it into the candidate Tauri App under
`Contents/PlugIns` through `tauri.ci.conf.json`. A production macOS target
must still embed the Rust transport sidecar, decode IPv4/UDP, and pass only UDP
`62900-62999` and `62056` datagrams into the shared relay envelope. It must
not become a generic proxy or silently forward all system traffic.

Release prerequisites are the Network Extension entitlement, Developer ID
signing, hardened runtime, notarization and staple. Test on Intel and Apple
Silicon with Mac↔Windows and Mac↔Mac games, including the 2K age-verification
precondition and repeated long games.
