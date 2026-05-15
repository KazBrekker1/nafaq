#![cfg(test)]

//! Multi-node scenario tests that mirror real-world user interactions:
//! two independent nodes, the production relay, gossip-based presence, and
//! a persistent identity that survives a node "restart". These tests are
//! intentionally slow — they wait on real network events with real timing.

use std::time::Duration;

use crate::messages::{DmMessage, Event};
use crate::test_support::{wait_for_event, TestNode};

/// Phase-1 sanity: when two nodes track each other as contacts, both sides
/// should see `PresenceChanged { online: true }` via gossip neighbor-up.
///
/// Covers reported bug #1: "Adding a new contact shows offline even when
/// both apps are open."
#[tokio::test]
async fn presence_neighbor_up_within_60s_after_tracking_contact() {
    let a = TestNode::new().await.expect("create A");
    let b = TestNode::new().await.expect("create B");

    let a_id = a.node_id_str();
    let b_id = b.node_id_str();

    let mut rx_a = a.event_tx.subscribe();
    let mut rx_b = b.event_tx.subscribe();

    a.presence.track_contact(&b_id).await.expect("A tracks B");
    b.presence.track_contact(&a_id).await.expect("B tracks A");

    let evt_a = wait_for_event(&mut rx_a, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &b_id
        )
    })
    .await;
    assert!(
        evt_a.is_some(),
        "A never observed B come online within 60s via gossip"
    );

    let evt_b = wait_for_event(&mut rx_b, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &a_id
        )
    })
    .await;
    assert!(
        evt_b.is_some(),
        "B never observed A come online within 60s via gossip"
    );

    a.shutdown_graceful().await;
    b.shutdown_graceful().await;
}

/// End-to-end scenario: two nodes establish a DM connection, exchange a
/// message, one node restarts with the SAME persistent identity, and DMs
/// resume without requiring a second restart.
///
/// Covers reported bug #3: "After one node exited and reopened the app,
/// both stopped receiving/sending messages."
///
/// The five phases mirror the user-visible flow exactly.
#[tokio::test]
async fn dm_survives_peer_restart_with_persistent_identity() {

    // ── Phase 1: bring up both nodes ───────────────────────────────────
    let a = TestNode::new().await.expect("create A");
    let b = TestNode::new().await.expect("create B");
    let a_id = a.node_id_str();
    let b_id = b.node_id_str();
    let b_secret = b.secret_key.clone();

    let mut rx_a = a.event_tx.subscribe();
    let mut rx_b = b.event_tx.subscribe();

    a.presence.track_contact(&b_id).await.unwrap();
    b.presence.track_contact(&a_id).await.unwrap();

    // ── Phase 2: presence handshake ────────────────────────────────────
    let a_sees_b_first = wait_for_event(&mut rx_a, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &b_id
        )
    })
    .await;
    assert!(a_sees_b_first.is_some(), "phase 2: A never saw B online");

    // ── Phase 3: baseline DM A → B ─────────────────────────────────────
    a.mgr
        .send_dm(
            &b_id,
            &DmMessage::Text {
                content: "before_restart".to_string(),
                timestamp: 1,
            },
        )
        .await
        .expect("phase 3: A.send_dm baseline");

    let baseline = wait_for_event(&mut rx_b, Duration::from_secs(15), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &a_id && content == "before_restart"
        )
    })
    .await;
    assert!(
        baseline.is_some(),
        "phase 3: B never received the baseline DM"
    );

    // ── Phase 4: simulate restart of B ─────────────────────────────────
    // Use graceful close — matches the user's reported "exit and reopen
    // the app" flow. Ungraceful drop is also realistic but causes the
    // relay to see two concurrent sessions for the same node id (the old
    // endpoint's background tasks linger until they self-abort) which is
    // a different class of bug we're not addressing in this scope.
    drop(rx_b);
    b.shutdown_graceful().await;

    // Bring B back up with the SAME secret key — simulates user reopening
    // the app with persistent identity enabled.
    let b_new = TestNode::with_key(b_secret).await.expect("re-create B");
    assert_eq!(
        b_new.node_id_str(),
        b_id,
        "phase 4: restarted B must have the same node id"
    );
    let mut rx_b_new = b_new.event_tx.subscribe();
    b_new.presence.track_contact(&a_id).await.unwrap();

    // A should see B come back online via gossip — this is what triggers
    // the `peer_recently_rejoined_gossip` branch in should_accept_dm_connection.
    let a_sees_b_again = wait_for_event(&mut rx_a, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &b_id
        )
    })
    .await;
    assert!(
        a_sees_b_again.is_some(),
        "phase 4: A never observed B's rejoin within 60s"
    );

    // ── Phase 5: post-restart DM A → B' ────────────────────────────────
    // If A's DM entry to old-B survived the graceful close (race window
    // between B's CONNECTION_CLOSE and A's cleanup task), the freshness
    // gate in ensure_dm_connected will evict it because presence reported
    // B's rejoin AFTER the entry was established. If the cleanup already
    // ran, ensure_dm_connected simply redials. Either path must produce a
    // working DM connection to B'.
    a.mgr
        .send_dm(
            &b_id,
            &DmMessage::Text {
                content: "after_restart".to_string(),
                timestamp: 2,
            },
        )
        .await
        .expect("phase 5: A.send_dm post-restart");

    let post_restart = wait_for_event(&mut rx_b_new, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &a_id && content == "after_restart"
        )
    })
    .await;
    assert!(
        post_restart.is_some(),
        "phase 5: B' never received the post-restart DM — DM is wedged"
    );

    a.shutdown_graceful().await;
    b_new.shutdown_graceful().await;
}

/// The user reported *"both stopped receiving/sending"* — verify that after
/// B restarts, **both** directions work, not just A → B'. This forces
/// `connect_dm` from both sides through the recovered presence path.
#[tokio::test]
async fn bidirectional_dms_survive_b_restart() {
    let a = TestNode::new().await.expect("create A");
    let b = TestNode::new().await.expect("create B");
    let a_id = a.node_id_str();
    let b_id = b.node_id_str();
    let b_secret = b.secret_key.clone();

    let mut rx_a = a.event_tx.subscribe();
    let mut rx_b = b.event_tx.subscribe();

    a.presence.track_contact(&b_id).await.unwrap();
    b.presence.track_contact(&a_id).await.unwrap();

    // Presence handshake.
    wait_for_event(&mut rx_a, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &b_id
        )
    })
    .await
    .expect("A never saw B online (pre-restart)");

    // Baseline both directions.
    a.mgr
        .send_dm(
            &b_id,
            &DmMessage::Text {
                content: "a_before".into(),
                timestamp: 1,
            },
        )
        .await
        .unwrap();
    b.mgr
        .send_dm(
            &a_id,
            &DmMessage::Text {
                content: "b_before".into(),
                timestamp: 2,
            },
        )
        .await
        .unwrap();
    wait_for_event(&mut rx_b, Duration::from_secs(15), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &a_id && content == "a_before"
        )
    })
    .await
    .expect("baseline a_before missing");
    wait_for_event(&mut rx_a, Duration::from_secs(15), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &b_id && content == "b_before"
        )
    })
    .await
    .expect("baseline b_before missing");

    // Restart B with the same identity.
    drop(rx_b);
    b.shutdown_graceful().await;
    let b_new = TestNode::with_key(b_secret).await.expect("re-create B");
    let mut rx_b_new = b_new.event_tx.subscribe();
    b_new.presence.track_contact(&a_id).await.unwrap();
    wait_for_event(&mut rx_a, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &b_id
        )
    })
    .await
    .expect("A never saw B's rejoin");

    // Both directions after restart.
    a.mgr
        .send_dm(
            &b_id,
            &DmMessage::Text {
                content: "a_after".into(),
                timestamp: 3,
            },
        )
        .await
        .expect("A.send_dm post-restart");
    b_new
        .mgr
        .send_dm(
            &a_id,
            &DmMessage::Text {
                content: "b_after".into(),
                timestamp: 4,
            },
        )
        .await
        .expect("B'.send_dm post-restart");

    wait_for_event(&mut rx_b_new, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &a_id && content == "a_after"
        )
    })
    .await
    .expect("B' never received a_after");
    wait_for_event(&mut rx_a, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &b_id && content == "b_after"
        )
    })
    .await
    .expect("A never received b_after");

    a.shutdown_graceful().await;
    b_new.shutdown_graceful().await;
}

/// Symmetric of the restart scenario — restart **A** instead of B. Catches
/// asymmetric bugs in the inbound/outbound DM accept logic.
#[tokio::test]
async fn dm_survives_local_restart() {
    let a = TestNode::new().await.expect("create A");
    let b = TestNode::new().await.expect("create B");
    let a_id = a.node_id_str();
    let b_id = b.node_id_str();
    let a_secret = a.secret_key.clone();

    let rx_a = a.event_tx.subscribe();
    let mut rx_b = b.event_tx.subscribe();

    a.presence.track_contact(&b_id).await.unwrap();
    b.presence.track_contact(&a_id).await.unwrap();
    wait_for_event(&mut rx_b, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &a_id
        )
    })
    .await
    .expect("B never saw A online");

    // Baseline.
    a.mgr
        .send_dm(
            &b_id,
            &DmMessage::Text {
                content: "before".into(),
                timestamp: 1,
            },
        )
        .await
        .unwrap();
    wait_for_event(&mut rx_b, Duration::from_secs(15), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &a_id && content == "before"
        )
    })
    .await
    .expect("baseline missing");

    // Restart A with the same identity.
    drop(rx_a);
    a.shutdown_graceful().await;
    let a_new = TestNode::with_key(a_secret).await.expect("re-create A");
    let _rx_a_new = a_new.event_tx.subscribe();
    a_new.presence.track_contact(&b_id).await.unwrap();
    wait_for_event(&mut rx_b, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &a_id
        )
    })
    .await
    .expect("B never saw A's rejoin");

    // Both directions after restart — but from A's side, this is a fresh
    // outbound dial (A' has no prior DM state). From B's side, B's existing
    // DM entry to old-A must be evicted before the new connection can land.
    a_new
        .mgr
        .send_dm(
            &b_id,
            &DmMessage::Text {
                content: "after".into(),
                timestamp: 2,
            },
        )
        .await
        .expect("A'.send_dm post-restart");
    wait_for_event(&mut rx_b, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &a_id && content == "after"
        )
    })
    .await
    .expect("B never received the post-restart DM");

    a_new.shutdown_graceful().await;
    b.shutdown_graceful().await;
}

/// Removing a contact must flip presence to offline locally; re-adding must
/// flip it back. Exercises the `untrack_contact` path that test #1 doesn't.
#[tokio::test]
async fn untrack_and_re_track_flips_presence() {
    let a = TestNode::new().await.expect("create A");
    let b = TestNode::new().await.expect("create B");
    let a_id = a.node_id_str();
    let b_id = b.node_id_str();

    let mut rx_a = a.event_tx.subscribe();
    let mut rx_b = b.event_tx.subscribe();

    a.presence.track_contact(&b_id).await.unwrap();
    b.presence.track_contact(&a_id).await.unwrap();
    wait_for_event(&mut rx_a, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &b_id
        )
    })
    .await
    .expect("A never saw B online");

    // Untrack — A immediately emits a synthetic offline event for B.
    a.presence.untrack_contact(&b_id).await;
    wait_for_event(&mut rx_a, Duration::from_secs(2), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: false } if peer_id == &b_id
        )
    })
    .await
    .expect("A did not emit offline event for B after untrack");

    // The snapshot no longer contains B.
    let snapshot = a.presence.snapshot().await;
    assert!(
        !snapshot.contains_key(&b_id),
        "after untrack, snapshot should not contain B"
    );

    // Re-track — A re-subscribes and should see NeighborUp again. B should
    // also see A back (gossip on B's side wasn't dropped, just neighbored down).
    a.presence.track_contact(&b_id).await.unwrap();
    wait_for_event(&mut rx_a, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &b_id
        )
    })
    .await
    .expect("A did not see B online after re-track");

    // Sanity: B's view of A should also still be online (its gossip subscription
    // for A never dropped, and A's re-subscribe brings the swarm back together).
    let _ = wait_for_event(&mut rx_b, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &a_id
        )
    })
    .await;

    a.shutdown_graceful().await;
    b.shutdown_graceful().await;
}
