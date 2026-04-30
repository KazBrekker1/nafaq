# Standalone Relay Resilience Implementation Plan

> **REQUIRED SUB-SKILL:** Use the executing-plans skill to implement this plan task-by-task.

**Goal:** Rebuild Nafaq's node identity, relay readiness, ticket, DM, presence, and connection lifecycle around a robust standalone architecture that uses only the project-controlled relay.

**Architecture:** Introduce a backend-owned node/connectivity service with durable local identity, explicit relay state, refreshable tickets, bounded dial operations, peer connection state machines, reconnect queues, and frontend status derived from backend events. Nafaq remains standalone: no default relay fallback, no central signaling/rendezvous server, no accounts, and no server-stored app data.

**Tech Stack:** Tauri 2, Rust, Tokio, Iroh 0.97, Nuxt 4, Vue 3, Tauri Store, project relay `https://iroh-relay.sanad.ink`.

---

## Product Constraints

These constraints are now canonical and are documented in `docs/GOALS.md`:

1. Use only `https://iroh-relay.sanad.ink` for relay-assisted connectivity.
2. Do not add default/public n0 relay fallback.
3. Do not add a central signaling/rendezvous service.
4. Keep contacts, tickets, messages, and peer metadata local or peer-to-peer.
5. Persistent node identity is the default, not an optional reliability toggle.
6. Never silently replace a persisted node identity with a new one.

---

## Target State Machine

### Relay lifecycle

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelayStatusKind {
    Starting,
    Connecting,
    Online,
    Degraded,
    Offline,
}
```

### Peer lifecycle

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PeerConnectionKind {
    Idle,
    Connecting,
    Connected,
    Suspect,
    Reconnecting,
    Disconnected,
    Failed,
}
```

A stalled peer should move `connected -> suspect -> reconnecting -> disconnected/failed`, not directly to disconnected after a short mobile/laptop pause.

---

## Task 1: Lock in standalone/own-relay goals

**Files:**
- Created: `docs/GOALS.md`
- Modified: `README.md`
- Optional follow-up modify: `docs/superpowers/specs/2026-03-24-nafaq-design.md`

**Steps:**
1. Verify `docs/GOALS.md` says Nafaq is standalone and does not use default relay fallback.
2. Verify README no longer says the app has no infrastructure at all; it should say no accounts/signaling/app backend and project-controlled relay only.
3. Optional: archive or update the old Electrobun-era spec so it does not override the current Tauri/Iroh goals.

**Verification:**

```bash
rg -n "default relay|standalone|signaling|iroh-relay.sanad.ink|no accounts" README.md docs/GOALS.md
```

Expected: README and goals clearly describe standalone app + own relay only.

---

## Task 2: Add identity load result instead of `Option<SecretKey>`

**Problem:** `src-tauri/src/lib.rs:117-143` silently returns `None` when persistence is enabled but the key is missing/corrupt.

**Files:**
- Create: `src-tauri/src/identity.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/messages.rs`

**Step 1: Add identity types**

Create `src-tauri/src/identity.rs`:

```rust
use anyhow::{Context, Result};
use iroh::SecretKey;
use tauri_plugin_store::StoreExt;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityStatus {
    LoadedPersistent,
    CreatedPersistent,
    ResetRequired,
}

#[derive(Debug, Clone)]
pub struct LoadedIdentity {
    pub secret_key: SecretKey,
    pub status: IdentityStatus,
}

const SETTINGS_FILE: &str = "settings.json";
const SECRET_KEY_KEY: &str = "secret_key";
const PERSISTENT_KEY: &str = "persistent_identity";

pub fn load_or_create_persistent_identity(app: &tauri::AppHandle) -> Result<LoadedIdentity> {
    let store = app.store(SETTINGS_FILE)?;
    let persistent = store
        .get(PERSISTENT_KEY)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    match store.get(SECRET_KEY_KEY).and_then(|v| v.as_str().map(String::from)) {
        Some(raw) => {
            let key = parse_secret_key(&raw).context("stored node identity is invalid")?;
            if !persistent {
                store.set(PERSISTENT_KEY, serde_json::Value::Bool(true));
                store.save()?;
            }
            Ok(LoadedIdentity { secret_key: key, status: IdentityStatus::LoadedPersistent })
        }
        None if persistent => anyhow::bail!(
            "persistent node identity is enabled but no secret key was found; explicit reset required"
        ),
        None => {
            let mut rng = rand::rng();
            let key = SecretKey::generate(&mut rng);
            persist_secret_key(&store, &key)?;
            Ok(LoadedIdentity { secret_key: key, status: IdentityStatus::CreatedPersistent })
        }
    }
}

fn parse_secret_key(raw: &str) -> Result<SecretKey> {
    raw.parse::<SecretKey>().or_else(|_| {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD.decode(raw)?;
        let arr: [u8; 32] = bytes.as_slice().try_into()?;
        Ok(SecretKey::from_bytes(&arr))
    })
}

pub fn persist_secret_key(store: &tauri_plugin_store::Store<tauri::Wry>, key: &SecretKey) -> Result<()> {
    let hex: String = key.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    store.set(SECRET_KEY_KEY, serde_json::Value::String(hex));
    store.set(PERSISTENT_KEY, serde_json::Value::Bool(true));
    store.save()?;
    Ok(())
}
```

Note: If the `rand` re-export is unavailable from current dependencies, use the same RNG pattern Iroh examples use, or add a direct compatible `rand` dependency.

**Step 2: Wire module**

In `src-tauri/src/lib.rs`, add:

```rust
mod identity;
```

Replace the current `secret_key` block with:

```rust
let loaded_identity = identity::load_or_create_persistent_identity(app.handle())?;
let identity_status = loaded_identity.status.clone();
let secret_key = loaded_identity.secret_key;
```

Then call:

```rust
node::create_endpoint_with_key(secret_key)
```

Task 3 updates `create_endpoint_with_key` to require a key.

**Step 3: Update settings command**

`get_settings` should return:

```json
{
  "persistentIdentity": true,
  "identityStatus": "loaded_persistent"
}
```

Remove UI semantics that imply persistence is optional. The old toggle should become a reset/export/debug action later, not a reliability setting.

**Step 4: Add tests**

Add tests in `identity.rs` for:

- missing key + no flag creates and stores a persistent key,
- existing key loads same public node ID,
- flag true + missing key errors,
- invalid key errors.

**Verification:**

```bash
cd src-tauri
cargo test identity --lib
```

Expected: identity tests pass.

---

## Task 3: Make endpoint creation require a stable key

**Problem:** `node::create_endpoint_with_key(None)` permits accidental ephemeral identities.

**Files:**
- Modify: `src-tauri/src/node.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify tests using `create_endpoint()`

**Step 1: Replace optional API**

Change:

```rust
pub async fn create_endpoint_with_key(secret_key: Option<SecretKey>) -> Result<Endpoint>
```

to:

```rust
pub async fn create_endpoint_with_key(secret_key: SecretKey) -> Result<Endpoint>
```

Always apply:

```rust
let builder = Endpoint::builder(presets::N0)
    .alpns(vec![NAFAQ_ALPN.to_vec(), NAFAQ_DM_ALPN.to_vec()])
    .transport_config(transport_config)
    .relay_mode(RelayMode::custom([relay_url]))
    .secret_key(secret_key);
```

**Step 2: Test helper only**

Keep a test-only helper:

```rust
#[cfg(test)]
pub async fn create_test_endpoint() -> Result<Endpoint> {
    let mut rng = rand::rng();
    create_endpoint_with_key(SecretKey::generate(&mut rng)).await
}
```

Update tests to call `create_test_endpoint()`.

**Verification:**

```bash
cd src-tauri
cargo test node --lib
```

Expected: no production code can create an endpoint without an explicit stable key.

---

## Task 4: Add backend relay health service and events

**Problem:** Relay online state is only checked during startup and ticket generation.

**Files:**
- Create: `src-tauri/src/relay.rs`
- Modify: `src-tauri/src/messages.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/commands.rs`

**Step 1: Add events**

In `messages.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayStatusKind {
    Starting,
    Connecting,
    Online,
    Degraded,
    Offline,
}
```

Add to `Event`:

```rust
RelayStatusChanged {
    status: RelayStatusKind,
    relay_url: String,
    node_id: String,
    ticket_available: bool,
    message: Option<String>,
},
TicketRefreshed {
    ticket: String,
},
```

Update event forwarder in `lib.rs` to emit names:

- `relay-status-changed`
- `ticket-refreshed`

**Step 2: Implement relay watcher**

Create `relay.rs` with a loop that:

1. emits `Connecting`,
2. waits for `endpoint.online()` with timeout,
3. checks `endpoint.addr().addrs` is non-empty,
4. emits `Online` + current ticket,
5. repeats every 15s or after failures with exponential backoff,
6. emits `Degraded`/`Offline` when consecutive checks fail.

Important: this is not adding another relay. It only monitors `node::RELAY_URL`.

**Step 3: Store latest ticket centrally**

Add to `AppState` or a new `NodeRuntimeState`:

```rust
pub latest_ticket: Arc<Mutex<Option<String>>>,
pub relay_status: Arc<Mutex<RelayStatusKind>>,
```

`get_node_info` should return node ID immediately plus relay status. If ticket is unavailable, return `ticket: null` rather than blocking/hanging.

**Step 4: Tests**

Unit-test ticket availability logic by extracting a pure helper:

```rust
fn ticket_available(addr: &iroh::EndpointAddr) -> bool {
    !addr.addrs.is_empty()
}
```

**Verification:**

```bash
cd src-tauri
cargo test relay --lib
```

Expected: relay status/ticket helpers pass.

---

## Task 5: Replace unsafe ticket announcement with refreshable ticket book

**Problem:** `setup_connection` announces `generate_ticket(endpoint)` immediately and `handle_peer_announce` ignores changed tickets forever.

**Files:**
- Modify: `src-tauri/src/connection.rs`
- Modify: `src-tauri/src/messages.rs`
- Add tests in `connection.rs`

**Step 1: Introduce peer ticket record**

Replace `HashMap<String, String>` with:

```rust
#[derive(Debug, Clone)]
struct PeerTicketRecord {
    ticket: String,
    last_updated_ms: u64,
    last_dial_failed_ms: Option<u64>,
    dial_failures: u32,
}
```

**Step 2: Update ticket on change**

Add method:

```rust
async fn upsert_peer_ticket(&self, peer_id: &str, ticket: &str) -> bool {
    let mut tickets = self.peer_tickets.lock().await;
    match tickets.get_mut(peer_id) {
        Some(existing) if existing.ticket == ticket => false,
        Some(existing) => {
            existing.ticket = ticket.to_string();
            existing.last_updated_ms = Self::current_timestamp_ms();
            existing.last_dial_failed_ms = None;
            existing.dial_failures = 0;
            true
        }
        None => {
            tickets.insert(peer_id.to_string(), PeerTicketRecord {
                ticket: ticket.to_string(),
                last_updated_ms: Self::current_timestamp_ms(),
                last_dial_failed_ms: None,
                dial_failures: 0,
            });
            true
        }
    }
}
```

**Step 3: Announce only latest valid own ticket**

ConnectionManager should receive a `latest_ticket: Arc<Mutex<Option<String>>>` or a callback from runtime state. In `setup_connection`, replace:

```rust
let own_ticket = crate::node::generate_ticket(endpoint);
```

with:

```rust
let own_ticket = latest_ticket.lock().await.clone();
if let Some(ticket) = own_ticket {
    self.send_control(&peer_id, &ControlAction::PeerAnnounce { peer_id: own_id, ticket }).await.ok();
}
```

If there is no ticket, skip announce and rely on `TicketRefreshed` handler to announce later.

**Step 4: Broadcast ticket refresh to peers**

When relay service emits `TicketRefreshed`, ConnectionManager should send a fresh `PeerAnnounce` to all connected peers.

**Step 5: Do not block gossip on dial**

In `handle_peer_announce`, relay the announcement to existing peers even if dialing the announced peer hangs/fails. Start the dial in a spawned task with timeout.

**Step 6: Tests**

Add tests:

- changed ticket updates the ticket book,
- identical ticket does not reset failure counters,
- failed dial does not prevent relay target selection,
- missing own ticket does not send an empty/stale self announce.

**Verification:**

```bash
cd src-tauri
cargo test connection::tests::ticket --lib
```

Expected: ticket book tests pass.

---

## Task 6: Add bounded dial/open-stream helpers

**Problem:** call/DM/mesh dials and `open_bi` can await forever.

**Files:**
- Modify: `src-tauri/src/connection.rs`
- Modify: `src-tauri/src/commands.rs`

**Step 1: Add constants**

```rust
const CALL_DIAL_TIMEOUT: Duration = Duration::from_secs(20);
const DM_DIAL_TIMEOUT: Duration = Duration::from_secs(12);
const STREAM_OPEN_TIMEOUT: Duration = Duration::from_secs(8);
```

**Step 2: Wrap call dial**

```rust
let connection = tokio::time::timeout(
    CALL_DIAL_TIMEOUT,
    endpoint.connect(addr, crate::node::NAFAQ_ALPN),
)
.await
.map_err(|_| anyhow::anyhow!("timed out dialing peer"))??;
```

**Step 3: Wrap stream opens and stream type writes**

```rust
let (mut chat_send, _) = tokio::time::timeout(STREAM_OPEN_TIMEOUT, connection.open_bi())
    .await
    .map_err(|_| anyhow::anyhow!("timed out opening chat stream"))??;
```

Do this for call chat/control streams and DM stream.

**Step 4: Tests**

Use a test helper or mocked future for timeout wrappers if direct Iroh timeout tests are flaky. At minimum add unit tests around pure helper error formatting and keep existing relay integration tests.

**Verification:**

```bash
cd src-tauri
cargo test connection --lib
```

Expected: existing connection tests still pass; user-facing invoke cannot hang indefinitely.

---

## Task 7: Add per-peer connection state and in-flight guards

**Problem:** duplicate call/DM connection races can overwrite state and leak tasks.

**Files:**
- Modify: `src-tauri/src/connection.rs`
- Modify: `src-tauri/src/messages.rs`
- Modify: `src-tauri/src/lib.rs` event forwarder

**Step 1: Add peer state events**

In `messages.rs`, add peer lifecycle enum and event:

```rust
PeerConnectionStatusChanged {
    peer_id: String,
    status: PeerConnectionKind,
    reason: Option<String>,
},
```

Forward as `peer-connection-status-changed`.

**Step 2: Add in-flight maps**

In `ConnectionManager`:

```rust
call_connecting: Arc<Mutex<HashSet<String>>>,
dm_connecting: Arc<Mutex<HashSet<String>>>,
```

Before dialing, reserve the peer ID. If already connected/connecting, return or await existing state rather than starting a second connection.

**Step 3: Deterministic tie-break for simultaneous dials**

If inbound and outbound connections appear for the same peer, keep exactly one. Deterministic rule:

- Compare local node ID string and remote peer ID string.
- Higher node ID keeps outbound; lower node ID keeps inbound.

Close the loser connection explicitly.

**Step 4: Insert safely**

Replace blind `peers.insert(peer_id.clone(), peer_conn)` with a helper that closes replaced connections and prevents leaked tasks.

**Step 5: Tests**

Add tests for:

- duplicate connect reservation prevents two dials,
- replacing a peer closes the old connection,
- simultaneous tie-break returns the same winner on both peers.

**Verification:**

```bash
cd src-tauri
cargo test connection::tests::duplicate --lib
```

Expected: duplicate connection tests pass.

---

## Task 8: Replace aggressive pruning with suspect/reconnect lifecycle

**Problem:** `prune_stale_peers(15_000)` is too aggressive for mobile/laptop sleep and backgrounding.

**Files:**
- Modify: `src-tauri/src/connection.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: frontend call/DM status handling later

**Step 1: Change thresholds**

```rust
const SUSPECT_AFTER_MS: u64 = 20_000;
const RECONNECT_AFTER_MS: u64 = 35_000;
const DISCONNECT_AFTER_MS: u64 = 120_000;
```

Tune after device testing.

**Step 2: Replace prune method**

Replace `prune_stale_peers` with:

```rust
pub async fn maintain_peer_liveness(&self) {
    // connected -> suspect when idle > SUSPECT_AFTER_MS
    // suspect -> reconnecting when idle > RECONNECT_AFTER_MS
    // reconnecting -> disconnected only after DISCONNECT_AFTER_MS or explicit close
}
```

**Step 3: Reconnect using latest ticket**

When moving to `reconnecting`, use latest cached `PeerTicketRecord`. If no ticket exists, keep `suspect` and emit a status asking for fresh ticket/peer activity rather than deleting state.

**Step 4: Keep media cleanup for true disconnect only**

Only emit current `PeerDisconnected` on final disconnect. For suspect/reconnecting, emit `PeerConnectionStatusChanged` and keep UI peer identity present.

**Step 5: Tests**

Use manually set `last_activity_ms` to test state transitions without waiting real time.

**Verification:**

```bash
cd src-tauri
cargo test liveness --lib
```

Expected: peers are not removed at 15s; they move through suspect/reconnecting first.

---

## Task 9: Make DM send ensure/reconnect/queue

**Problem:** frontend `sendText` calls `send_dm` directly and fails if the backend stream is stale.

**Files:**
- Modify: `src-tauri/src/connection.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `app/composables/useDM.ts`

**Step 1: Backend ensure method**

Add:

```rust
pub async fn ensure_dm_connected(&self, peer_id: &str) -> Result<()> {
    if self.dm_peers.lock().await.contains_key(peer_id) {
        return Ok(());
    }
    self.connect_dm(peer_id).await
}
```

Then in `send_dm`:

```rust
if !self.dm_peers.lock().await.contains_key(peer_id) {
    self.ensure_dm_connected(peer_id).await?;
}
```

**Step 2: On write failure, reconnect once**

If writing to the DM stream fails:

1. remove stale DM connection,
2. reconnect with timeout,
3. retry the same message once,
4. return a clear error if it still fails.

**Step 3: Frontend optimistic message state**

Extend `DmTextMessage` with:

```ts
status: "sending" | "sent" | "failed";
```

`sendText` should push `sending`, then update to `sent` or `failed`.

**Step 4: Tests**

Add backend tests for reconnect-on-write failure if feasible. Add frontend unit/testable helper for message state transition if no full test harness exists.

**Verification:**

```bash
cd src-tauri
cargo test dm --lib
bun run generate
```

Expected: DM send no longer depends on a page-level stale `connectedPeers` set.

---

## Task 10: Move frontend node readiness to backend-derived state

**Problem:** `useCall.ts` retries `get_node_info` and then gives up; it does not subscribe to relay recovery.

**Files:**
- Modify: `app/composables/useCall.ts`
- Modify: `app/composables/useSettings.ts`
- Modify: `app/pages/index.vue`
- Modify: `app/pages/settings.vue`
- Possibly create: `app/composables/useNodeRuntime.ts`

**Step 1: Create runtime composable**

Create `useNodeRuntime.ts`:

```ts
export type RelayStatus = "starting" | "connecting" | "online" | "degraded" | "offline";

const nodeId = ref<string | null>(null);
const relayStatus = ref<RelayStatus>("starting");
const ticket = ref<string | null>(null);
const nodeError = ref<string | null>(null);
```

Listen for:

- `relay-status-changed`,
- `ticket-refreshed`,
- `peer-connection-status-changed`.

**Step 2: Stop polling as readiness source**

`get_node_info` should initialize state once, but relay updates come from events. Do not permanently give up after 30s; show offline/degraded until backend reports recovery.

**Step 3: Update UI language**

Show:

- Node ID always once identity is loaded.
- Relay status separately.
- Ticket unavailable until relay online.
- Clear recovery messages.

**Verification:**

```bash
bun run generate
```

Expected: frontend builds and state types compile.

---

## Task 11: Fix presence probing lifecycle and make it non-invasive

**Problem:** presence currently opens DM ALPN connections every 30s for each contact, which can create transport churn and misleading online state.

**Files:**
- Modify: `src-tauri/src/commands.rs`
- Modify: `app/composables/usePresence.ts`
- Modify pages using `startProbing`

**Step 1: Backend bounded presence remains**

Keep timeout, but emit less churn:

- Limit concurrent probes.
- Back off failed peers.
- Do not probe while relay offline.

**Step 2: Frontend watches contact list changes**

`startProbing` should watch the provided ref so loaded contacts trigger immediate probe after `get_contacts` completes.

**Step 3: Prefer connection state when available**

If a peer has active DM/call connection state, use that over a fresh presence probe.

**Verification:**

```bash
bun run generate
cd src-tauri && cargo test check_presence --lib
```

Expected: presence does not race contact loading and does not flood mobile devices.

---

## Task 12: Fix relay deployment health and production posture

**Problem:** relay healthcheck always exits success and relay image is unpinned/dev.

**Files:**
- Modify: `deploy/iroh-relay/docker-compose.yml`
- Add: `deploy/iroh-relay/README.md`

**Step 1: Fix healthcheck**

Replace:

```yaml
test: ["CMD-SHELL", "wget -qO- http://127.0.0.1:3340/generate_204 || exit 0"]
```

with:

```yaml
test: ["CMD-SHELL", "wget -qO- http://127.0.0.1:3340/generate_204 >/dev/null"]
```

or an equivalent command that fails on HTTP failure.

**Step 2: Pin image**

Replace `latest` with the tested relay version matching Iroh 0.97 compatibility.

**Step 3: Remove dev mode if appropriate**

Investigate the correct production command for `n0computer/iroh-relay`. Document why any remaining flags are used.

**Verification:**

```bash
docker compose -f deploy/iroh-relay/docker-compose.yml config
```

Expected: valid compose config; healthcheck fails when local HTTP probe fails.

---

## Task 13: Add integration tests for mobile/laptop failure modes

**Files:**
- Create: `src-tauri/tests/resilience.rs` or add to `src-tauri/src/connection.rs` tests
- Optional create: `scripts/test-resilience.sh`

**Scenarios:**

1. Stable identity survives restart.
2. Missing persisted key produces explicit reset-required error.
3. Relay-unready ticket is not announced.
4. Changed peer ticket replaces old ticket.
5. Dial timeout returns in bounded time.
6. DM send reconnects after stale stream.
7. Peer idle moves to suspect, not disconnected.
8. Reconnect failure eventually becomes disconnected with reason.

**Verification:**

```bash
cd src-tauri
cargo test resilience -- --nocapture
```

Expected: all resilience scenarios pass or are marked ignored with a clear reason if they require live relay.

---

## Task 14: Final verification matrix

Run:

```bash
bun run generate
cd src-tauri
cargo test --lib
cargo test --tests
```

Manual tests:

1. Fresh install starts with stable node ID and persistent identity.
2. Restart app: node ID unchanged.
3. Temporarily block relay: app shows relay offline/degraded, node ID remains visible, ticket unavailable.
4. Restore relay: app emits relay online and ticket refresh without restart.
5. Start call between laptop and phone.
6. Lock phone for 20-40s: laptop shows suspect/reconnecting, not permanent disconnect.
7. Unlock phone: connection recovers or offers retry with reason.
8. Send DM after route navigation and after short offline period: message queues/reconnects or fails visibly.
9. Group call: stale/changed ticket does not poison mesh; later fresh ticket works.
10. Relay healthcheck actually fails when relay HTTP endpoint is down.

---

## Implementation Order

1. Goals/docs lock-in.
2. Persistent identity by default and explicit identity failure.
3. Endpoint requires stable key.
4. Relay status service + ticket cache.
5. Refreshable peer ticket book.
6. Bounded dials/stream opens.
7. Connection state machine and duplicate guards.
8. Suspect/reconnect lifecycle.
9. DM ensure/reconnect/queue.
10. Frontend runtime state.
11. Presence lifecycle.
12. Relay deployment health.
13. Resilience tests and device verification.

This is intentionally not a small patch. It turns the networking layer into a resilient standalone runtime while preserving the core product constraint: own relay only, no central app backend.
