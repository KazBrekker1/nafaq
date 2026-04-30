# Standalone relay resilience regression inventory

This inventory tracks the mobile/laptop failure-mode regressions from
`docs/plans/2026-04-30-standalone-relay-resilience.md` Task 13. Prefer adding new
coverage here and to the focused module tests below before introducing long live-relay tests.

Run the fast resilience subset with:

```bash
cd src-tauri
cargo test resilience -- --nocapture
```

Some lifecycle coverage uses real local Iroh endpoints and may wait for relay readiness through the
existing test helper. Those tests are intentionally kept under their focused module names rather than
being duplicated in the fast resilience subset.

| Scenario | Coverage | Notes |
| --- | --- | --- |
| Stable identity survives restart/load twice | `identity::tests::resilience_stable_identity_survives_restart_and_two_loads` | Reopens the same store through fresh mock apps and asserts the node id is stable across two reloads. |
| Missing persisted key with persistent flag errors | `identity::tests::resilience_missing_persisted_key_with_persistent_flag_errors` | Guards against silently generating a replacement identity when persistence is enabled. |
| Relay-unready/missing ticket is not announced | `connection::tests::resilience_missing_ticket_or_endpoint_is_not_announced`, `connection::tests::resilience_ticket_latest_self_announce_uses_latest_ticket_only` | Covers no-ticket/no-endpoint readiness and latest-ticket self announcement behavior. |
| Changed peer ticket replaces old ticket | `connection::tests::resilience_ticket_upsert_changed_ticket_updates_record` | Also verifies stale dial-failure counters are reset for a changed ticket. |
| Dial timeout returns bounded error | `connection::tests::resilience_timeout_helper_returns_clear_error` | Helper-level timeout coverage avoids flaky live relay dials. |
| DM send reconnects after stale stream | `connection::tests::send_dm_reconnects_once_after_unavailable_stream` | Uses real Iroh endpoints; keep focused rather than duplicating. |
| Peer idle moves to suspect, not disconnected | `connection::tests::liveness_marks_connected_peer_suspect_without_removing_it` and `connection::tests::liveness_does_not_disconnect_after_legacy_fifteen_second_idle` | Validates the mobile/laptop pause path does not jump directly to disconnected. |
| Reconnect failure/final timeout eventually disconnected with reason | `connection::tests::liveness_reconnect_attempt_uses_latest_cached_ticket_record`, `connection::tests::liveness_final_timeout_disconnects_and_removes_peer_with_reason` | Final timeout now asserts the frontend status event carries the `peer timeout` reason. |
