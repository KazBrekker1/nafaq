//! Per-frame video transport.
//!
//! Every encoded frame travels on its own QUIC uni-stream:
//! `[STREAM_VIDEO][seq: u32 BE][timestamp_ms: u64 BE][annex-b payload]`.
//!
//! One stream per frame gives the sender a delivery signal (`stopped()`
//! resolves once the peer acknowledged every byte) and a way to abandon a
//! frame that can no longer arrive in time (`reset()`), so video latency stays
//! bounded instead of queueing megabytes behind a slow path. The cost is that
//! frames can complete out of order, and an abandoned frame leaves a hole —
//! [`VideoReorderBuffer`] on the receiver restores order and only ever hands
//! the decoder an unbroken reference chain (anything after a hole waits for
//! the next keyframe).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use iroh::endpoint::Connection;
use tokio::sync::Notify;

use crate::messages::STREAM_VIDEO;

pub const VIDEO_FRAME_HEADER_LEN: usize = 4 + 8;
/// Upper bound for one encoded frame's payload; anything larger is refused.
/// Generous for our profiles (a 640x360 keyframe is tens of KiB).
pub const MAX_VIDEO_FRAME_BYTES: usize = 1024 * 1024;

/// Frames written but not yet acknowledged. At 12 fps and a 300 ms RTT about
/// four frames are legitimately in flight; beyond this the path can't keep up
/// and newer frames are skipped (the quality controller then lowers bitrate).
const MAX_IN_FLIGHT_FRAMES: usize = 6;
/// A frame not acknowledged within this window is abandoned (stream reset).
const DELTA_DELIVERY_DEADLINE: Duration = Duration::from_millis(1200);
/// Keyframes are several times larger than deltas; give them longer.
const KEYFRAME_DELIVERY_DEADLINE: Duration = Duration::from_millis(2500);
/// Receiver: a frame stream must complete within this long. The sender
/// abandons (resets) any frame past its delivery deadline, so this only
/// bites a peer that stalls a stream deliberately to pin memory.
pub const VIDEO_FRAME_READ_TIMEOUT: Duration =
    KEYFRAME_DELIVERY_DEADLINE.saturating_add(Duration::from_millis(1500));
/// A frame that waited this long for in-flight capacity is too stale to send.
const MAX_QUEUED_FRAME_AGE: Duration = Duration::from_millis(500);
const STREAM_OPEN_TIMEOUT: Duration = Duration::from_secs(3);

/// Receiver: how long a later frame may wait for a missing earlier one before
/// the hole is declared permanent.
const REORDER_GAP_TIMEOUT: Duration = Duration::from_millis(300);
/// Receiver: cap on frames held back while waiting for a hole to fill.
const REORDER_MAX_HELD: usize = 8;

pub fn encode_video_frame(seq: u32, timestamp_ms: u64, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + VIDEO_FRAME_HEADER_LEN + payload.len());
    buf.push(STREAM_VIDEO);
    buf.extend_from_slice(&seq.to_be_bytes());
    buf.extend_from_slice(&timestamp_ms.to_be_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// Parses a frame body (everything after the stream-type byte).
pub fn decode_video_frame(body: &[u8]) -> Option<(u32, u64, &[u8])> {
    if body.len() <= VIDEO_FRAME_HEADER_LEN {
        return None;
    }
    let seq = u32::from_be_bytes(body[..4].try_into().ok()?);
    let timestamp_ms = u64::from_be_bytes(body[4..12].try_into().ok()?);
    Some((seq, timestamp_ms, &body[VIDEO_FRAME_HEADER_LEN..]))
}

// ── Receiver ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedVideoFrame {
    pub seq: u32,
    pub timestamp_ms: u64,
    pub is_keyframe: bool,
    pub payload: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct ReorderOutput {
    /// Frames safe to decode, in order.
    pub ready: Vec<ReceivedVideoFrame>,
    /// The chain is broken; the sender should be asked for a keyframe.
    pub need_keyframe: bool,
}

pub struct VideoReorderBuffer {
    /// Next sequence number that continues the current reference chain.
    expected: Option<u32>,
    held: BTreeMap<u32, ReceivedVideoFrame>,
    gap_since: Option<Instant>,
}

impl Default for VideoReorderBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoReorderBuffer {
    pub fn new() -> Self {
        Self {
            expected: None,
            held: BTreeMap::new(),
            gap_since: None,
        }
    }

    pub fn push(&mut self, frame: ReceivedVideoFrame, now: Instant) -> ReorderOutput {
        let mut out = ReorderOutput::default();
        match self.expected {
            // No chain yet (fresh, or after a hole): only a keyframe can start one.
            None => {
                if frame.is_keyframe {
                    self.start_chain(frame, now, &mut out);
                } else {
                    // Kept in case its keyframe completes later; bounded by
                    // evicting the oldest, which is least likely to matter.
                    self.held.insert(frame.seq, frame);
                    while self.held.len() > REORDER_MAX_HELD {
                        self.held.pop_first();
                    }
                    out.need_keyframe = true;
                }
            }
            Some(expected) => {
                if frame.seq < expected {
                    // Late duplicate or a frame we already skipped past.
                } else if frame.seq == expected || frame.is_keyframe {
                    // A keyframe ahead of a hole resynchronises immediately.
                    self.start_chain(frame, now, &mut out);
                } else {
                    // Not trimmed here: exceeding the cap means the hole
                    // isn't going to fill, which the check below turns into
                    // a reset + keyframe request. (Trimming would silently
                    // drop the frames right after the hole instead.)
                    self.held.insert(frame.seq, frame);
                    self.gap_since.get_or_insert(now);
                }
            }
        }

        let gap_expired = self
            .gap_since
            .is_some_and(|since| now.duration_since(since) >= REORDER_GAP_TIMEOUT);
        if self.expected.is_some() && (gap_expired || self.held.len() > REORDER_MAX_HELD) {
            // The missing frame isn't coming (the sender abandoned it). Every
            // held delta references it, so drop them and wait for a keyframe.
            self.expected = None;
            self.held.clear();
            self.gap_since = None;
            out.need_keyframe = true;
        }
        out
    }

    fn start_chain(&mut self, frame: ReceivedVideoFrame, now: Instant, out: &mut ReorderOutput) {
        let seq = frame.seq;
        self.held.retain(|&held_seq, _| held_seq > seq);
        out.ready.push(frame);
        let mut next = seq.wrapping_add(1);
        while let Some(frame) = self.held.remove(&next) {
            out.ready.push(frame);
            next = next.wrapping_add(1);
        }
        self.expected = Some(next);
        self.gap_since = if self.held.is_empty() { None } else { Some(now) };
    }
}

// ── Sender ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct PendingVideoFrame {
    pub timestamp_ms: u64,
    pub payload: Arc<Vec<u8>>,
    pub is_keyframe: bool,
    pub queued_at: Instant,
}

struct WriterSlot {
    pending: Option<PendingVideoFrame>,
    /// The peer's reference chain is broken (a frame was skipped or lost);
    /// deltas are useless until the next keyframe.
    awaiting_keyframe: bool,
}

/// Per-peer outbound video queue: a single "next frame" slot plus in-flight
/// accounting for the writer task.
#[derive(Clone)]
pub struct PeerVideoWriter {
    slot: Arc<StdMutex<WriterSlot>>,
    notify: Arc<Notify>,
    capacity_freed: Arc<Notify>,
    in_flight: Arc<AtomicUsize>,
    /// Shared with the peer entry; set to ask the encoder for a keyframe.
    request_keyframe: Arc<AtomicBool>,
    dropped_frames: Arc<AtomicU32>,
}

impl PeerVideoWriter {
    pub fn new(request_keyframe: Arc<AtomicBool>) -> Self {
        Self {
            slot: Arc::new(StdMutex::new(WriterSlot {
                pending: None,
                // A new connection starts with no reference chain.
                awaiting_keyframe: true,
            })),
            notify: Arc::new(Notify::new()),
            capacity_freed: Arc::new(Notify::new()),
            in_flight: Arc::new(AtomicUsize::new(0)),
            request_keyframe,
            dropped_frames: Arc::new(AtomicU32::new(0)),
        }
    }

    fn lock_slot(&self) -> std::sync::MutexGuard<'_, WriterSlot> {
        self.slot.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// Offer a freshly encoded frame. Deltas are only accepted while the
    /// chain is intact and the writer has room; skipping one breaks the chain,
    /// so the writer then waits for (and asks for) a keyframe.
    pub fn enqueue(&self, frame: PendingVideoFrame) {
        let mut slot = self.lock_slot();
        if frame.is_keyframe {
            slot.awaiting_keyframe = false;
            slot.pending = Some(frame);
        } else if slot.awaiting_keyframe {
            self.request_keyframe.store(true, Ordering::Relaxed);
            return;
        } else if slot.pending.is_some() {
            slot.awaiting_keyframe = true;
            self.request_keyframe.store(true, Ordering::Relaxed);
            self.dropped_frames.fetch_add(1, Ordering::Relaxed);
            return;
        } else {
            slot.pending = Some(frame);
        }
        drop(slot);
        self.notify.notify_one();
    }

    /// Frames stopped flowing to this peer (paused, suspect, lost frame): the
    /// next frame it gets must be a keyframe.
    pub fn mark_gap(&self) {
        let mut slot = self.lock_slot();
        slot.awaiting_keyframe = true;
        if slot.pending.as_ref().is_some_and(|f| !f.is_keyframe) {
            slot.pending = None;
        }
    }

    /// Frames skipped or abandoned since the last call (congestion signal).
    pub fn take_dropped_frames(&self) -> u32 {
        self.dropped_frames.swap(0, Ordering::Relaxed)
    }

    fn frame_lost(&self) {
        self.mark_gap();
        self.request_keyframe.store(true, Ordering::Relaxed);
        self.dropped_frames.fetch_add(1, Ordering::Relaxed);
    }

    fn take_next(&self) -> Option<PendingVideoFrame> {
        let mut slot = self.lock_slot();
        let frame = slot.pending.take()?;
        if frame.queued_at.elapsed() > MAX_QUEUED_FRAME_AGE {
            drop(slot);
            if !frame.is_keyframe {
                self.frame_lost();
                return None;
            }
            // A stale keyframe is still the only way to restart the chain.
        }
        Some(frame)
    }

    pub fn spawn(self, peer_id: String, connection: Connection) {
        tokio::spawn(async move {
            let mut seq: u32 = 0;
            loop {
                tokio::select! {
                    _ = self.notify.notified() => {}
                    _ = connection.closed() => break,
                }
                loop {
                    while self.in_flight.load(Ordering::Acquire) >= MAX_IN_FLIGHT_FRAMES {
                        tokio::select! {
                            _ = self.capacity_freed.notified() => {}
                            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                            _ = connection.closed() => return,
                        }
                    }
                    let Some(frame) = self.take_next() else {
                        break;
                    };
                    seq = seq.wrapping_add(1);
                    self.in_flight.fetch_add(1, Ordering::AcqRel);
                    let writer = self.clone();
                    let connection = connection.clone();
                    let peer_id = peer_id.clone();
                    tokio::spawn(async move {
                        let delivered = send_frame(&connection, seq, &frame).await;
                        if let Err(reason) = delivered {
                            tracing::debug!("Video frame {seq} to {peer_id} not delivered: {reason}");
                            writer.frame_lost();
                        }
                        writer.in_flight.fetch_sub(1, Ordering::AcqRel);
                        writer.capacity_freed.notify_one();
                    });
                }
            }
            tracing::info!("Video writer closed for peer {peer_id}");
        });
    }
}

async fn send_frame(
    connection: &Connection,
    seq: u32,
    frame: &PendingVideoFrame,
) -> Result<(), String> {
    let deadline = if frame.is_keyframe {
        KEYFRAME_DELIVERY_DEADLINE
    } else {
        DELTA_DELIVERY_DEADLINE
    };
    let started = Instant::now();
    let mut stream = tokio::time::timeout(STREAM_OPEN_TIMEOUT, connection.open_uni())
        .await
        .map_err(|_| "timed out opening stream".to_string())?
        .map_err(|e| e.to_string())?;
    let _ = stream.set_priority(if frame.is_keyframe { 50 } else { 30 });
    let bytes = encode_video_frame(seq, frame.timestamp_ms, &frame.payload);
    let remaining = |started: Instant| deadline.saturating_sub(started.elapsed());

    match tokio::time::timeout(remaining(started), stream.write_all(&bytes)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e.to_string()),
        Err(_) => {
            let _ = stream.reset(0u32.into());
            return Err("write deadline exceeded".into());
        }
    }
    stream.finish().map_err(|e| e.to_string())?;

    match tokio::time::timeout(remaining(started), stream.stopped()).await {
        Ok(Ok(None)) => Ok(()),
        Ok(Ok(Some(code))) => Err(format!("stopped by peer ({code})")),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => {
            let _ = stream.reset(0u32.into());
            Err("delivery deadline exceeded".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u32, key: bool) -> ReceivedVideoFrame {
        ReceivedVideoFrame {
            seq,
            timestamp_ms: seq as u64 * 83,
            is_keyframe: key,
            payload: vec![seq as u8],
        }
    }

    fn seqs(out: &ReorderOutput) -> Vec<u32> {
        out.ready.iter().map(|f| f.seq).collect()
    }

    #[test]
    fn header_round_trips() {
        let bytes = encode_video_frame(7, 1234, &[1, 2, 3]);
        assert_eq!(bytes[0], STREAM_VIDEO);
        let (seq, ts, payload) = decode_video_frame(&bytes[1..]).unwrap();
        assert_eq!((seq, ts, payload), (7, 1234, &[1u8, 2, 3][..]));
        assert!(decode_video_frame(&bytes[1..13]).is_none());
    }

    #[test]
    fn deltas_before_first_keyframe_are_withheld() {
        let mut buf = VideoReorderBuffer::new();
        let now = Instant::now();
        let out = buf.push(frame(1, false), now);
        assert!(out.ready.is_empty());
        assert!(out.need_keyframe);
        let out = buf.push(frame(2, true), now);
        assert_eq!(seqs(&out), vec![2]);
        assert!(!out.need_keyframe);
    }

    #[test]
    fn reordered_frames_are_released_in_order() {
        let mut buf = VideoReorderBuffer::new();
        let now = Instant::now();
        buf.push(frame(1, true), now);
        assert!(buf.push(frame(3, false), now).ready.is_empty());
        let out = buf.push(frame(2, false), now);
        assert_eq!(seqs(&out), vec![2, 3]);
    }

    #[test]
    fn delta_arriving_before_its_keyframe_is_kept() {
        let mut buf = VideoReorderBuffer::new();
        let now = Instant::now();
        buf.push(frame(1, true), now);
        buf.push(frame(2, false), now);
        // Big keyframe 3 finishes after delta 4.
        assert!(buf.push(frame(4, false), now).ready.is_empty());
        let out = buf.push(frame(3, true), now);
        assert_eq!(seqs(&out), vec![3, 4]);
    }

    #[test]
    fn permanent_hole_drops_dependents_and_requests_keyframe() {
        let mut buf = VideoReorderBuffer::new();
        let start = Instant::now();
        buf.push(frame(1, true), start);
        buf.push(frame(3, false), start);
        let out = buf.push(frame(4, false), start + REORDER_GAP_TIMEOUT);
        assert!(out.ready.is_empty());
        assert!(out.need_keyframe);
        // Late arrival of the missing frame no longer resurrects the chain.
        let out = buf.push(frame(2, false), start + REORDER_GAP_TIMEOUT);
        assert!(out.ready.is_empty());
        let out = buf.push(frame(5, true), start + REORDER_GAP_TIMEOUT);
        assert_eq!(seqs(&out), vec![5]);
    }

    #[test]
    fn overflowing_the_hold_while_chained_resets_and_requests_keyframe() {
        let mut buf = VideoReorderBuffer::new();
        let now = Instant::now();
        buf.push(frame(1, true), now);
        // Frame 2 is missing; 3.. pile up before the gap timeout.
        for seq in 3..(3 + REORDER_MAX_HELD as u32) {
            let out = buf.push(frame(seq, false), now);
            assert!(out.ready.is_empty());
            assert!(!out.need_keyframe, "held {seq} within the cap");
        }
        let out = buf.push(frame(3 + REORDER_MAX_HELD as u32, false), now);
        assert!(out.ready.is_empty());
        assert!(out.need_keyframe, "overflow must reset and ask for a keyframe");
        // The chain restarts only at a keyframe.
        assert!(buf.push(frame(2, false), now).ready.is_empty());
        let key = 4 + REORDER_MAX_HELD as u32;
        assert_eq!(seqs(&buf.push(frame(key, true), now)), vec![key]);
    }

    #[test]
    fn keyframe_after_hole_resyncs_immediately() {
        let mut buf = VideoReorderBuffer::new();
        let now = Instant::now();
        buf.push(frame(1, true), now);
        buf.push(frame(3, false), now);
        let out = buf.push(frame(4, true), now);
        assert_eq!(seqs(&out), vec![4]);
        assert!(buf.push(frame(3, false), now).ready.is_empty());
    }

    #[test]
    fn duplicates_and_late_frames_are_ignored() {
        let mut buf = VideoReorderBuffer::new();
        let now = Instant::now();
        buf.push(frame(1, true), now);
        buf.push(frame(2, false), now);
        assert!(buf.push(frame(2, false), now).ready.is_empty());
        assert!(buf.push(frame(1, true), now).ready.is_empty());
    }

    fn pending(key: bool) -> PendingVideoFrame {
        PendingVideoFrame {
            timestamp_ms: 0,
            payload: Arc::new(vec![0]),
            is_keyframe: key,
            queued_at: Instant::now(),
        }
    }

    #[test]
    fn writer_waits_for_keyframe_on_a_new_connection() {
        let request = Arc::new(AtomicBool::new(false));
        let writer = PeerVideoWriter::new(request.clone());
        writer.enqueue(pending(false));
        assert!(writer.lock_slot().pending.is_none());
        assert!(request.load(Ordering::Relaxed));
        writer.enqueue(pending(true));
        assert!(writer.lock_slot().pending.is_some());
    }

    #[test]
    fn skipping_a_delta_breaks_the_chain_until_the_next_keyframe() {
        let request = Arc::new(AtomicBool::new(false));
        let writer = PeerVideoWriter::new(request.clone());
        writer.enqueue(pending(true));
        request.store(false, Ordering::Relaxed);
        // Slot still busy with the keyframe: this delta must be skipped...
        writer.enqueue(pending(false));
        assert!(writer.lock_slot().awaiting_keyframe);
        assert!(request.load(Ordering::Relaxed));
        assert_eq!(writer.take_dropped_frames(), 1);
        // ...and so must every delta after it, even once the slot drains.
        assert!(writer.take_next().is_some_and(|f| f.is_keyframe));
        writer.enqueue(pending(false));
        assert!(writer.lock_slot().pending.is_none());
        writer.enqueue(pending(true));
        assert!(!writer.lock_slot().awaiting_keyframe);
    }

    #[test]
    fn mark_gap_discards_a_queued_delta() {
        let writer = PeerVideoWriter::new(Arc::new(AtomicBool::new(false)));
        writer.enqueue(pending(true));
        writer.take_next();
        writer.enqueue(pending(false));
        writer.mark_gap();
        assert!(writer.lock_slot().pending.is_none());
        assert!(writer.lock_slot().awaiting_keyframe);
    }
}
