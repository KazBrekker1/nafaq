#![cfg(test)]

//! Multi-node scenario tests that mirror real-world user interactions:
//! two independent nodes, the production relay, gossip-based presence, and
//! a persistent identity that survives a node "restart". These tests are
//! intentionally slow — they wait on real network events with real timing.

use std::collections::HashSet;
use std::time::Duration;

use crate::messages::{ControlAction, DmMessage, Event};
use crate::test_support::{TestNode, wait_for_event};

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
                id: None,
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
                id: None,
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
                id: None,
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
    b.mgr
        .send_dm(
            &a_id,
            &DmMessage::Text {
                content: "b_before".into(),
                timestamp: 2,
                id: None,
            },
        )
        .await
        .unwrap();
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
                id: None,
            },
        )
        .await
        .expect("A.send_dm post-restart");
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
    b_new
        .mgr
        .send_dm(
            &a_id,
            &DmMessage::Text {
                content: "b_after".into(),
                timestamp: 4,
                id: None,
            },
        )
        .await
        .expect("B'.send_dm post-restart");
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
                id: None,
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
                id: None,
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

#[tokio::test]
async fn simultaneous_first_dms_are_delivered_from_blank_state() {
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

    wait_for_event(&mut rx_b, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &a_id
        )
    })
    .await
    .expect("B never saw A online");

    let (a_send, b_send) = tokio::join!(
        async {
            a.mgr
                .send_dm(
                    &b_id,
                    &DmMessage::Text {
                        content: "a_first".into(),
                        timestamp: 1,
                        id: None,
                    },
                )
                .await
        },
        async {
            b.mgr
                .send_dm(
                    &a_id,
                    &DmMessage::Text {
                        content: "b_first".into(),
                        timestamp: 2,
                        id: None,
                    },
                )
                .await
        },
    );

    a_send.expect("A first send failed");
    b_send.expect("B first send failed");

    wait_for_event(&mut rx_b, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &a_id && content == "a_first"
        )
    })
    .await
    .expect("B never received A's simultaneous first DM");

    wait_for_event(&mut rx_a, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &b_id && content == "b_first"
        )
    })
    .await
    .expect("A never received B's simultaneous first DM");

    a.shutdown_graceful().await;
    b.shutdown_graceful().await;
}

#[tokio::test]
async fn multiple_sequential_dms_preserve_order_over_existing_dm_connection() {
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

    a.mgr
        .send_dm(
            &b_id,
            &DmMessage::Text {
                content: "warmup".into(),
                timestamp: 1,
                id: None,
            },
        )
        .await
        .expect("A warmup send failed");
    wait_for_event(&mut rx_b, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &a_id && content == "warmup"
        )
    })
    .await
    .expect("B never received warmup DM");
    assert!(a.mgr.dm_peer_connected(&b_id).await);
    assert!(b.mgr.dm_peer_connected(&a_id).await);

    for (content, timestamp) in [("first", 2), ("second", 3), ("third", 4)] {
        a.mgr
            .send_dm(
                &b_id,
                &DmMessage::Text {
                    content: content.into(),
                    timestamp,
                    id: None,
                },
            )
            .await
            .expect("A sequential send failed");
        wait_for_event(&mut rx_b, Duration::from_secs(30), |e| {
            matches!(e,
                Event::DmReceived {
                    peer_id,
                    message: DmMessage::Text { content: received, .. },
                } if peer_id == &a_id && received == content
            )
        })
        .await
        .unwrap_or_else(|| panic!("B never received {content} in order"));
    }

    b.mgr
        .send_dm(
            &a_id,
            &DmMessage::Text {
                content: "ack".into(),
                timestamp: 5,
                id: None,
            },
        )
        .await
        .expect("B ack send failed");
    wait_for_event(&mut rx_a, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &b_id && content == "ack"
        )
    })
    .await
    .expect("A never received B ack");

    a.shutdown_graceful().await;
    b.shutdown_graceful().await;
}

#[tokio::test]
async fn simultaneous_first_dm_burst_continues_after_duplicate_resolution() {
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
    wait_for_event(&mut rx_b, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &a_id
        )
    })
    .await
    .expect("B never saw A online");

    let (a_first, b_first) = tokio::join!(
        async {
            a.mgr
                .send_dm(
                    &b_id,
                    &DmMessage::Text {
                        content: "a_first_burst".into(),
                        timestamp: 1,
                        id: None,
                    },
                )
                .await
        },
        async {
            b.mgr
                .send_dm(
                    &a_id,
                    &DmMessage::Text {
                        content: "b_first_burst".into(),
                        timestamp: 2,
                        id: None,
                    },
                )
                .await
        },
    );
    a_first.expect("A first burst send failed");
    b_first.expect("B first burst send failed");

    wait_for_event(&mut rx_b, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &a_id && content == "a_first_burst"
        )
    })
    .await
    .expect("B never received A's first burst DM");
    wait_for_event(&mut rx_a, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &b_id && content == "b_first_burst"
        )
    })
    .await
    .expect("A never received B's first burst DM");

    let (a_follow_up, b_follow_up) = tokio::join!(
        async {
            a.mgr
                .send_dm(
                    &b_id,
                    &DmMessage::Text {
                        content: "a_follow_up".into(),
                        timestamp: 3,
                        id: None,
                    },
                )
                .await
        },
        async {
            b.mgr
                .send_dm(
                    &a_id,
                    &DmMessage::Text {
                        content: "b_follow_up".into(),
                        timestamp: 4,
                        id: None,
                    },
                )
                .await
        },
    );
    a_follow_up.expect("A follow-up send failed");
    b_follow_up.expect("B follow-up send failed");

    wait_for_event(&mut rx_b, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &a_id && content == "a_follow_up"
        )
    })
    .await
    .expect("B never received A's follow-up DM");
    wait_for_event(&mut rx_a, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &b_id && content == "b_follow_up"
        )
    })
    .await
    .expect("A never received B's follow-up DM");
    assert!(a.mgr.dm_peer_connected(&b_id).await);
    assert!(b.mgr.dm_peer_connected(&a_id).await);

    a.shutdown_graceful().await;
    b.shutdown_graceful().await;
}

#[tokio::test]
async fn peer_restart_emits_presence_cycle_and_dms_resume() {
    let a = TestNode::new().await.expect("create A");
    let b = TestNode::new().await.expect("create B");
    let a_id = a.node_id_str();
    let b_id = b.node_id_str();
    let b_secret = b.secret_key.clone();

    let mut rx_a = a.event_tx.subscribe();
    let rx_b = b.event_tx.subscribe();

    a.presence.track_contact(&b_id).await.unwrap();
    b.presence.track_contact(&a_id).await.unwrap();

    wait_for_event(&mut rx_a, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &b_id
        )
    })
    .await
    .expect("A never saw B online before restart");

    drop(rx_b);
    b.shutdown_graceful().await;
    wait_for_event(&mut rx_a, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: false } if peer_id == &b_id
        )
    })
    .await
    .expect("A never saw B offline after shutdown");

    let b_new = TestNode::with_key(b_secret).await.expect("re-create B");
    assert_eq!(b_new.node_id_str(), b_id);
    let mut rx_b_new = b_new.event_tx.subscribe();
    b_new.presence.track_contact(&a_id).await.unwrap();

    wait_for_event(&mut rx_a, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &b_id
        )
    })
    .await
    .expect("A never saw B online after restart");

    a.mgr
        .send_dm(
            &b_id,
            &DmMessage::Text {
                content: "a_after_presence_cycle".into(),
                timestamp: 1,
                id: None,
            },
        )
        .await
        .expect("A post-cycle send failed");
    wait_for_event(&mut rx_b_new, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &a_id && content == "a_after_presence_cycle"
        )
    })
    .await
    .expect("B' never received A's post-cycle DM");

    b_new
        .mgr
        .send_dm(
            &a_id,
            &DmMessage::Text {
                content: "b_after_presence_cycle".into(),
                timestamp: 2,
                id: None,
            },
        )
        .await
        .expect("B post-cycle send failed");
    wait_for_event(&mut rx_a, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &b_id && content == "b_after_presence_cycle"
        )
    })
    .await
    .expect("A never received B's post-cycle DM");

    a.shutdown_graceful().await;
    b_new.shutdown_graceful().await;
}

#[tokio::test]
async fn failed_dm_while_peer_offline_recovers_after_rejoin() {
    let a = TestNode::new().await.expect("create A");
    let b = TestNode::new().await.expect("create B");
    let a_id = a.node_id_str();
    let b_id = b.node_id_str();
    let b_secret = b.secret_key.clone();

    let mut rx_a = a.event_tx.subscribe();
    let rx_b = b.event_tx.subscribe();

    a.presence.track_contact(&b_id).await.unwrap();
    b.presence.track_contact(&a_id).await.unwrap();
    wait_for_event(&mut rx_a, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &b_id
        )
    })
    .await
    .expect("A never saw B online before offline send test");

    drop(rx_b);
    b.shutdown_graceful().await;
    wait_for_event(&mut rx_a, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: false } if peer_id == &b_id
        )
    })
    .await
    .expect("A never saw B offline before failed send");

    let offline_send = a
        .mgr
        .send_dm(
            &b_id,
            &DmMessage::Text {
                content: "sent_while_offline".into(),
                timestamp: 1,
                id: None,
            },
        )
        .await;
    assert!(
        offline_send.is_err(),
        "DM send should fail while the peer is offline"
    );
    assert!(
        !a.mgr.dm_peer_connected(&b_id).await,
        "failed offline DM must not leave a stale connected DM entry"
    );

    let b_new = TestNode::with_key(b_secret).await.expect("re-create B");
    assert_eq!(b_new.node_id_str(), b_id);
    let mut rx_b_new = b_new.event_tx.subscribe();
    b_new.presence.track_contact(&a_id).await.unwrap();
    wait_for_event(&mut rx_a, Duration::from_secs(60), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: true } if peer_id == &b_id
        )
    })
    .await
    .expect("A never saw B online after failed-send rejoin");

    a.mgr
        .send_dm(
            &b_id,
            &DmMessage::Text {
                content: "sent_after_rejoin".into(),
                timestamp: 2,
                id: None,
            },
        )
        .await
        .expect("A send after B rejoined failed");
    wait_for_event(&mut rx_b_new, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &a_id && content == "sent_after_rejoin"
        )
    })
    .await
    .expect("B' never received DM after failed-send recovery");

    a.shutdown_graceful().await;
    b_new.shutdown_graceful().await;
}

#[tokio::test]
async fn untracking_contact_during_active_dm_keeps_dm_usable() {
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
    .expect("A never saw B online before active DM untrack");

    a.mgr
        .send_dm(
            &b_id,
            &DmMessage::Text {
                content: "before_untrack".into(),
                timestamp: 1,
                id: None,
            },
        )
        .await
        .expect("A initial DM failed");
    wait_for_event(&mut rx_b, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &a_id && content == "before_untrack"
        )
    })
    .await
    .expect("B never received initial DM before untrack");
    assert!(a.mgr.dm_peer_connected(&b_id).await);
    assert!(b.mgr.dm_peer_connected(&a_id).await);

    a.presence.untrack_contact(&b_id).await;
    wait_for_event(&mut rx_a, Duration::from_secs(2), |e| {
        matches!(e,
            Event::PresenceChanged { peer_id, online: false } if peer_id == &b_id
        )
    })
    .await
    .expect("A did not emit offline event after active DM untrack");
    assert!(
        !a.presence.snapshot().await.contains_key(&b_id),
        "untracked contact should be absent from A's presence snapshot"
    );

    a.mgr
        .send_dm(
            &b_id,
            &DmMessage::Text {
                content: "after_untrack".into(),
                timestamp: 2,
                id: None,
            },
        )
        .await
        .expect("A DM after untrack failed");
    wait_for_event(&mut rx_b, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &a_id && content == "after_untrack"
        )
    })
    .await
    .expect("B never received DM after A untracked contact");

    b.mgr
        .send_dm(
            &a_id,
            &DmMessage::Text {
                content: "reply_after_untrack".into(),
                timestamp: 3,
                id: None,
            },
        )
        .await
        .expect("B reply after A untrack failed");
    wait_for_event(&mut rx_a, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &b_id && content == "reply_after_untrack"
        )
    })
    .await
    .expect("A never received reply after untracking contact");

    a.shutdown_graceful().await;
    b.shutdown_graceful().await;
}

#[tokio::test]
async fn interrupted_file_transfer_cleans_up_and_later_dms_work() {
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
    .expect("A never saw B online before interrupted transfer");

    let file_id = "interrupted-large-transfer".to_string();
    let chunk = vec![7u8; 128 * 1024];
    a.mgr
        .ensure_dm_connected(&b_id)
        .await
        .expect("A could not establish DM before interrupted transfer");
    a.mgr
        .send_dm_frame_strict(
            &b_id,
            &DmMessage::FileStart {
                name: "interrupted.bin".into(),
                size: (chunk.len() * 2) as u64,
                id: file_id.clone(),
            },
        )
        .await
        .expect("FileStart send failed");
    a.mgr
        .send_dm_frame_strict(
            &b_id,
            &DmMessage::FileChunk {
                id: file_id.clone(),
                offset: 0,
                data: chunk,
            },
        )
        .await
        .expect("FileChunk send failed");

    wait_for_event(&mut rx_b, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmFileProgress { peer_id, file_id: seen_id, received }
                if peer_id == &a_id && seen_id == &file_id && *received == 128 * 1024
        )
    })
    .await
    .expect("B never observed partial file progress before interruption");

    a.mgr.disconnect_dm(&b_id).await;
    wait_for_event(&mut rx_b, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmFileTransferFailed { peer_id, file_id: seen_id }
                if peer_id == &a_id && seen_id == &file_id
        )
    })
    .await
    .expect("B never emitted file-transfer failure after interrupted DM stream");

    a.mgr
        .send_dm(
            &b_id,
            &DmMessage::Text {
                content: "after_interrupted_file".into(),
                timestamp: 1,
                id: None,
            },
        )
        .await
        .expect("A text DM after interrupted file transfer failed");
    wait_for_event(&mut rx_b, Duration::from_secs(30), |e| {
        matches!(e,
            Event::DmReceived {
                peer_id,
                message: DmMessage::Text { content, .. },
            } if peer_id == &a_id && content == "after_interrupted_file"
        )
    })
    .await
    .expect("B never received text DM after interrupted file transfer");

    a.shutdown_graceful().await;
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

// ── Group-call mesh scenarios ───────────────────────────────────────────
//
// The mesh self-connects via `ConnectionManager::handle_peer_announce`
// (connection.rs ~1762): when a node accepts a new call connection, it
// relays every OTHER peer ticket it already knows to the newcomer
// (connection.rs ~1670-1684), and the newcomer's control dispatcher reacts
// to that relayed `PeerAnnounce` by auto-dialing the announced peer. Every
// call participant must first publish its own ticket (mirrors
// `commands::create_call`) or it has nothing to be announced *as* — a
// joiner that never creates a ticket is only ever reachable by the peer it
// directly dialed.

/// Brings up a two-node call where BOTH sides have published their own call
/// ticket, B has joined A, and — critically — A has already ingested B's
/// self-announce. That last wait is a hard synchronization point: A only
/// relays its *already-known* peer tickets to a newcomer at the moment that
/// newcomer's connection is set up; there's no later re-broadcast. Joining a
/// third peer before A had actually processed B's announce would mean the
/// B<->C auto-connect never fires — not because the mesh is broken, but
/// because the relay snapshot was taken too early.
async fn two_node_call_ready_for_mesh() -> (TestNode, TestNode, String, String, String) {
    let a = TestNode::new().await.expect("create A");
    let b = TestNode::new().await.expect("create B");
    let a_id = a.node_id_str();
    let b_id = b.node_id_str();

    let mut rx_a = a.event_tx.subscribe();
    let mut rx_b = b.event_tx.subscribe();

    let a_ticket = a
        .create_call_ticket()
        .await
        .expect("A create_call_ticket");
    // B must be independently reachable *before* it joins so A learns B's
    // address (via B's self-announce on connect) and can relay it onward.
    b.create_call_ticket()
        .await
        .expect("B create_call_ticket");

    b.mgr
        .connect_to_peer_with_ticket(&b.endpoint, &a_ticket)
        .await
        .expect("B joins A");

    wait_for_event(&mut rx_a, Duration::from_secs(30), |e| {
        matches!(e, Event::PeerConnected { peer_id } if peer_id == &b_id)
    })
    .await
    .expect("A never saw B connect");
    wait_for_event(&mut rx_b, Duration::from_secs(30), |e| {
        matches!(e, Event::PeerConnected { peer_id } if peer_id == &a_id)
    })
    .await
    .expect("B never saw A connect");

    wait_for_event(&mut rx_a, Duration::from_secs(15), |e| {
        matches!(e,
            Event::ControlReceived {
                peer_id,
                action: ControlAction::PeerAnnounce { peer_id: announced, .. },
            } if peer_id == &b_id && announced == &b_id
        )
    })
    .await
    .expect("A never received B's self-announce before proceeding");

    (a, b, a_id, b_id, a_ticket)
}

/// Full 3-node mesh: A hosts, B joins first (publishing its own ticket), then
/// C joins directly via A's ticket. Via `handle_peer_announce`'s relay of
/// already-known peer tickets, B and C should additionally auto-connect to
/// EACH OTHER without ever being told about each other by the test. Returns
/// once all three report exactly 2 peers.
async fn three_node_mesh() -> (TestNode, TestNode, TestNode, String, String, String) {
    let (a, b, a_id, b_id, a_ticket) = two_node_call_ready_for_mesh().await;

    let c = TestNode::new().await.expect("create C");
    let c_id = c.node_id_str();

    let mut rx_a = a.event_tx.subscribe();
    let mut rx_b = b.event_tx.subscribe();
    let mut rx_c = c.event_tx.subscribe();

    // Mirrors commands::join_call, which arms the call session before dialing.
    c.mgr.set_call_session_active(true);
    c.mgr
        .connect_to_peer_with_ticket(&c.endpoint, &a_ticket)
        .await
        .expect("C joins A");

    wait_for_event(&mut rx_a, Duration::from_secs(30), |e| {
        matches!(e, Event::PeerConnected { peer_id } if peer_id == &c_id)
    })
    .await
    .expect("A never saw C connect");
    wait_for_event(&mut rx_c, Duration::from_secs(30), |e| {
        matches!(e, Event::PeerConnected { peer_id } if peer_id == &a_id)
    })
    .await
    .expect("C never saw A connect");

    // The mesh auto-connect: B and C should dial each other, triggered by
    // A relaying B's ticket to C at C's join (handle_peer_announce).
    wait_for_event(&mut rx_b, Duration::from_secs(30), |e| {
        matches!(e, Event::PeerConnected { peer_id } if peer_id == &c_id)
    })
    .await
    .expect("B never auto-connected to C via the peer-announce mesh relay");
    wait_for_event(&mut rx_c, Duration::from_secs(30), |e| {
        matches!(e, Event::PeerConnected { peer_id } if peer_id == &b_id)
    })
    .await
    .expect("C never auto-connected to B via the peer-announce mesh relay");

    assert_eq!(a.mgr.peer_count().await, 2, "A should see both B and C");
    assert_eq!(b.mgr.peer_count().await, 2, "B should see both A and C");
    assert_eq!(c.mgr.peer_count().await, 2, "C should see both A and B");

    (a, b, c, a_id, b_id, c_id)
}

async fn connected_peer_ids(node: &TestNode) -> HashSet<String> {
    node.mgr
        .snapshot_network_stats()
        .await
        .into_iter()
        .map(|stats| stats.peer_id)
        .collect()
}

/// Covers the mesh-formation gap identified in review: every existing
/// scenario test uses exactly two nodes, so `handle_peer_announce` and its
/// PeerAnnounce relay had zero automated coverage. A creates a call, B
/// joins, C joins — B and C must end up connected to EACH OTHER via the
/// auto-dial mesh relay, not just to A.
#[tokio::test]
async fn mesh_formation_connects_all_three_peers_via_peer_announce() {
    let (a, b, c, a_id, b_id, c_id) = three_node_mesh().await;

    assert_eq!(
        connected_peer_ids(&a).await,
        HashSet::from([b_id.clone(), c_id.clone()]),
        "A should be connected to exactly {{B, C}}"
    );
    assert_eq!(
        connected_peer_ids(&b).await,
        HashSet::from([a_id.clone(), c_id.clone()]),
        "B should be connected to exactly {{A, C}}"
    );
    assert_eq!(
        connected_peer_ids(&c).await,
        HashSet::from([a_id.clone(), b_id.clone()]),
        "C should be connected to exactly {{A, B}}"
    );

    a.shutdown_graceful().await;
    b.shutdown_graceful().await;
    c.shutdown_graceful().await;
}

/// C leaving a fully-meshed 3-node call must be observed by both remaining
/// peers exactly once (no ghost/duplicate PeerDisconnected), and A<->B must
/// remain healthy afterward.
#[tokio::test]
async fn leave_propagation_disconnects_cleanly_without_ghost_entries() {
    let (a, b, c, a_id, b_id, c_id) = three_node_mesh().await;

    let mut rx_a = a.event_tx.subscribe();
    let mut rx_b = b.event_tx.subscribe();

    c.mgr
        .disconnect_peer(&a_id)
        .await
        .expect("C disconnects from A");
    c.mgr
        .disconnect_peer(&b_id)
        .await
        .expect("C disconnects from B");

    wait_for_event(&mut rx_a, Duration::from_secs(15), |e| {
        matches!(e, Event::PeerDisconnected { peer_id } if peer_id == &c_id)
    })
    .await
    .expect("A never observed C's departure");
    wait_for_event(&mut rx_b, Duration::from_secs(15), |e| {
        matches!(e, Event::PeerDisconnected { peer_id } if peer_id == &c_id)
    })
    .await
    .expect("B never observed C's departure");

    // No ghost duplicate disconnect events for C on either side.
    let a_ghost = wait_for_event(&mut rx_a, Duration::from_millis(750), |e| {
        matches!(e, Event::PeerDisconnected { peer_id } if peer_id == &c_id)
    })
    .await;
    assert!(
        a_ghost.is_none(),
        "A observed a duplicate PeerDisconnected for C"
    );
    let b_ghost = wait_for_event(&mut rx_b, Duration::from_millis(750), |e| {
        matches!(e, Event::PeerDisconnected { peer_id } if peer_id == &c_id)
    })
    .await;
    assert!(
        b_ghost.is_none(),
        "B observed a duplicate PeerDisconnected for C"
    );

    assert_eq!(a.mgr.peer_count().await, 1, "A should only have B left");
    assert_eq!(b.mgr.peer_count().await, 1, "B should only have A left");
    assert_eq!(c.mgr.peer_count().await, 0, "C should have no peers left");

    // A<->B stays healthy after C leaves.
    a.mgr
        .send_chat(&b_id, "still-alive")
        .await
        .expect("A->B chat after C left");
    wait_for_event(&mut rx_b, Duration::from_secs(10), |e| {
        matches!(e,
            Event::ChatReceived { peer_id, message } if peer_id == &a_id && message == "still-alive"
        )
    })
    .await
    .expect("B never received chat from A after C left — A<->B was disrupted");

    a.shutdown_graceful().await;
    b.shutdown_graceful().await;
    c.shutdown_graceful().await;
}

/// A late joiner (C) arriving after A<->B is already established and
/// exchanging chat must end up connected to both without disrupting the
/// existing A<->B pairing.
#[tokio::test]
async fn late_joiner_connects_to_both_without_disrupting_existing_pair() {
    let (a, b, a_id, b_id, a_ticket) = two_node_call_ready_for_mesh().await;

    let mut rx_a = a.event_tx.subscribe();
    let mut rx_b = b.event_tx.subscribe();

    // Media/control flowing before the late joiner arrives.
    a.mgr
        .send_chat(&b_id, "pre-join-chat")
        .await
        .expect("A->B chat before C joins");
    wait_for_event(&mut rx_b, Duration::from_secs(10), |e| {
        matches!(e,
            Event::ChatReceived { peer_id, message } if peer_id == &a_id && message == "pre-join-chat"
        )
    })
    .await
    .expect("B never received pre-join chat from A");

    let c = TestNode::new().await.expect("create C");
    let c_id = c.node_id_str();
    let mut rx_c = c.event_tx.subscribe();

    // Mirrors commands::join_call, which arms the call session before dialing.
    c.mgr.set_call_session_active(true);
    c.mgr
        .connect_to_peer_with_ticket(&c.endpoint, &a_ticket)
        .await
        .expect("C joins A");

    wait_for_event(&mut rx_a, Duration::from_secs(30), |e| {
        matches!(e, Event::PeerConnected { peer_id } if peer_id == &c_id)
    })
    .await
    .expect("A never saw C connect");
    wait_for_event(&mut rx_c, Duration::from_secs(30), |e| {
        matches!(e, Event::PeerConnected { peer_id } if peer_id == &a_id)
    })
    .await
    .expect("C never saw A connect");

    wait_for_event(&mut rx_b, Duration::from_secs(30), |e| {
        matches!(e, Event::PeerConnected { peer_id } if peer_id == &c_id)
    })
    .await
    .expect("B never auto-connected to late joiner C via peer-announce mesh relay");
    wait_for_event(&mut rx_c, Duration::from_secs(30), |e| {
        matches!(e, Event::PeerConnected { peer_id } if peer_id == &b_id)
    })
    .await
    .expect("C never auto-connected to B via peer-announce mesh relay");

    assert_eq!(a.mgr.peer_count().await, 2, "A should see both B and C");
    assert_eq!(b.mgr.peer_count().await, 2, "B should see both A and C");
    assert_eq!(c.mgr.peer_count().await, 2, "C should see both A and B");

    // A<->B pairing must still be healthy after C's late arrival.
    b.mgr
        .send_chat(&a_id, "post-join-chat")
        .await
        .expect("B->A chat after C joins");
    wait_for_event(&mut rx_a, Duration::from_secs(10), |e| {
        matches!(e,
            Event::ChatReceived { peer_id, message } if peer_id == &b_id && message == "post-join-chat"
        )
    })
    .await
    .expect("A never received post-join chat from B — A<->B was disrupted by C joining");

    a.shutdown_graceful().await;
    b.shutdown_graceful().await;
    c.shutdown_graceful().await;
}
