# Nafaq Product Goals

## Core Goal

Nafaq is a standalone peer-to-peer communication app. The app should not require user accounts, a hosted signaling service, a central contact registry, a message broker, TURN infrastructure, or any app-specific backend to operate.

The app may use the project-controlled Iroh relay at `https://iroh-relay.sanad.ink` for NAT traversal and relay-assisted connectivity. That relay is infrastructure, not application state: it must not store user accounts, contact lists, conversations, media, files, or call metadata beyond what Iroh relay operation requires.

## Connectivity Goal

Connectivity must be robust on mobile phones and laptops, including slow startup, network transitions, Wi-Fi/cellular changes, laptop sleep/wake, app foreground/background transitions, temporary relay outage, and peer restarts.

The correct behavior is not to immediately drop peer state when a transport stalls. The app should move connections through explicit states:

1. `starting`
2. `relay_connecting`
3. `relay_online`
4. `ready`
5. `connecting_peer`
6. `connected`
7. `suspect`
8. `reconnecting`
9. `disconnected`

Users should see understandable status and recovery attempts instead of silent hangs, stale node IDs, or sudden disconnects.

## Identity Goal

A node identity is a durable part of the standalone app install. It should be generated once, persisted by default, and reused across restarts. Contacts and DMs rely on the node ID staying stable.

Persistent identity should not be an optional reliability toggle. A user may explicitly reset identity, but the default behavior is stable identity.

If a persisted key is missing, corrupt, or unavailable, the app must not silently create a new identity while claiming persistence is enabled. It must surface a clear error or perform an explicit reset flow.

## Relay Goal

For now Nafaq must use only the project-controlled relay at `https://iroh-relay.sanad.ink`. Do not fall back to the public/default n0 relay network unless that product decision is explicitly changed later.

Because there is no fallback relay, the app and deployment must treat relay health as a first-class dependency:

- The app should continuously monitor relay readiness.
- Tickets should be created and announced only after relay-published addresses are available.
- Tickets should refresh after relay/network changes.
- The relay deployment must have a real failing healthcheck and pinned production configuration.

## Discovery Goal

Nafaq remains standalone: there is no central rendezvous API or hosted contact directory. Peer discovery is based on:

- out-of-band ticket sharing,
- QR/copy workflows,
- locally persisted contacts,
- locally cached last-known peer tickets,
- peer-to-peer gossip over active encrypted connections.

Any stored peer reachability information lives locally on the user device and is refreshed through P2P interaction, not through a central server.

## Messaging Goal

DMs should behave like message delivery, not like a fragile UI-bound stream. Sending a DM should ensure a connection, reconnect if needed, queue briefly while reconnecting, and report delivery failure clearly when a peer cannot be reached.

Navigating between pages must not accidentally destroy transport state or create duplicate connections. Transport lifecycle belongs in the backend connection manager, not in individual pages.

## Call Goal

Calls should survive transient transport problems when possible. Short stalls should enter `suspect` / `reconnecting`; they should not immediately become permanent disconnects. When reconnection fails, the UI should explain what happened and offer a retry.

Group mesh formation should not poison peer tickets permanently. Stale tickets must be replaceable, failed dials must not block gossip, and duplicate simultaneous dials must be resolved deterministically.

## Non-Goals

- No default/public relay fallback for now.
- No central signaling/rendezvous server.
- No accounts or cloud identity.
- No server-stored contact list.
- No server-stored messages, files, or media.
- No TURN-style media server.
