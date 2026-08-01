# Windows WFP adapter contract

This directory records the production interception contract. The Tauri
diagnostic app and the shared Rust client core are buildable now; the native
WFP callout is still a separate Windows SDK/driver deliverable and is not
pretended to be complete by the current probe UI.

## Required interception point

The adapter must register a signed WFP callout and filter at the outbound
transport layer for UDP. The filter is restricted to:

- the Civ VI executable's app identity;
- UDP;
- destination `255.255.255.255` and destination ports `62900-62999` for
  discovery;
- `62056/UDP` after a gameplay session has been authorized;
- outbound direction only for the broadcast rewrite path.

The callout must copy the original UDP payload, replace the destination with
the bridge's virtual/relay path, and inject the packet back into the stack with
loop-prevention metadata. It must never passively wait for a packet on the
virtual adapter: by then the limited broadcast has already been discarded by
the normal routing path. Inbound relay envelopes are decoded by the service,
their virtual source identity is checked, and a receive-side injection makes
the response visible to the original Civ VI socket.

The Windows service, not the Tauri UI, owns the callout lifecycle. Installation
must handle driver/service start, rollback, update, uninstall, signature
verification and coexistence with other WFP filters. `install.ps1` also adds
the smallest Domain/Private firewall rules and removes them on uninstall.

## Message boundary

The service converts intercepted Civ VI datagrams to the shared
`civ6-lan-protocol::relay::RelayMessage` envelope. The relay transport is
behind `DatagramTransport`; the first implementation is WireGuard/UDP, while
QUIC DATAGRAM or udp2raw may be evaluated later under the same datagram
semantics.

## Acceptance tests

1. With the virtual adapter listener disabled, Civ VI discovery still reaches
   the relay through the WFP outbound callout.
2. A room with two simultaneous hosts returns two distinguishable responses.
3. `62056/UDP` packets are delivered only to the selected gameplay peer.
4. Re-injected packets do not loop through the callout.
5. Windows Firewall rules are present after installation and absent after
   uninstall.
