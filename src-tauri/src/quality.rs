//! Outbound video quality control.
//!
//! One controller owns the "how hard can we push video" decision. It runs on
//! the 1 s stats tick and looks, per peer, only at signals that mean the path
//! is actually congested:
//! - video frames the writer had to skip or abandon (see `video_transport`),
//! - packet loss during the tick,
//! - queueing delay: RTT above that peer's recent minimum. Absolute RTT is not
//!   a congestion signal — a relayed call across regions sits at 150 ms+ and
//!   is perfectly healthy.
//!
//! It steps down quickly and recovers slowly (hysteresis) so the encoder isn't
//! reconfigured every tick. The shared encoder follows the worst peer.

use std::collections::{HashMap, VecDeque};

pub const MAX_LEVEL: u8 = 3;

/// Consecutive good ticks before stepping one level back up.
const RECOVER_AFTER_GOOD_TICKS: u32 = 8;
/// Minimum ticks between two step-downs, so one burst isn't counted twice.
const STEP_DOWN_COOLDOWN_TICKS: u32 = 2;
const RTT_WINDOW_TICKS: usize = 30;
const QUEUE_DELAY_BAD_MS: u64 = 200;
const LOST_PACKETS_BAD_PER_TICK: u64 = 25;
const DROPPED_FRAMES_BAD_PER_TICK: u32 = 2;

#[derive(Debug, Clone, Copy, Default)]
pub struct PeerSignal {
    pub rtt_ms: u64,
    pub lost_packets: u64,
    pub dropped_video_frames: u32,
}

#[derive(Debug, Default)]
struct PeerState {
    level: u8,
    good_ticks: u32,
    ticks_since_step_down: u32,
    rtt_samples: VecDeque<u64>,
}

#[derive(Debug, Default)]
pub struct SendQualityController {
    peers: HashMap<String, PeerState>,
}

impl SendQualityController {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one tick of per-peer signals; returns the level for the shared
    /// encoder (0 = full quality … `MAX_LEVEL` = most constrained).
    pub fn tick(&mut self, signals: &[(String, PeerSignal)]) -> u8 {
        self.peers
            .retain(|peer_id, _| signals.iter().any(|(id, _)| id == peer_id));

        let mut worst = 0;
        for (peer_id, signal) in signals {
            let state = self.peers.entry(peer_id.clone()).or_default();
            state.ticks_since_step_down = state.ticks_since_step_down.saturating_add(1);

            let min_rtt = state.rtt_samples.iter().copied().min();
            if signal.rtt_ms > 0 {
                state.rtt_samples.push_back(signal.rtt_ms);
                if state.rtt_samples.len() > RTT_WINDOW_TICKS {
                    state.rtt_samples.pop_front();
                }
            }
            let queue_delay_ms = min_rtt
                .map(|min| signal.rtt_ms.saturating_sub(min))
                .unwrap_or(0);

            let congested = signal.dropped_video_frames >= DROPPED_FRAMES_BAD_PER_TICK
                || signal.lost_packets >= LOST_PACKETS_BAD_PER_TICK
                || queue_delay_ms >= QUEUE_DELAY_BAD_MS;

            if congested {
                state.good_ticks = 0;
                if state.level < MAX_LEVEL
                    && state.ticks_since_step_down >= STEP_DOWN_COOLDOWN_TICKS
                {
                    state.level += 1;
                    state.ticks_since_step_down = 0;
                }
            } else {
                state.good_ticks += 1;
                if state.good_ticks >= RECOVER_AFTER_GOOD_TICKS && state.level > 0 {
                    state.level -= 1;
                    state.good_ticks = 0;
                }
            }
            worst = worst.max(state.level);
        }
        worst
    }
}

/// Base encoding profile chosen for the current call size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoProfile {
    pub bitrate_bps: u32,
    pub fps: u32,
    pub max_width: u32,
    pub max_height: u32,
}

/// What a level means for the encoder and capture. Mirrored by
/// `sendLevelCapture` in the frontend's useMediaTransport.
pub fn apply_level(base: VideoProfile, level: u8) -> VideoProfile {
    let (bitrate_pct, fps_cap, small) = match level.min(MAX_LEVEL) {
        0 => (100, u32::MAX, false),
        1 => (60, 8, false),
        2 => (35, 6, true),
        _ => (20, 5, true),
    };
    let (max_width, max_height) = if small {
        (base.max_width.min(320), base.max_height.min(180))
    } else {
        (base.max_width, base.max_height)
    };
    VideoProfile {
        bitrate_bps: (base.bitrate_bps / 100 * bitrate_pct).max(80_000).min(base.bitrate_bps),
        fps: base.fps.min(fps_cap),
        max_width,
        max_height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(ctrl: &mut SendQualityController, signal: PeerSignal) -> u8 {
        ctrl.tick(&[("a".to_string(), signal)])
    }

    fn healthy(rtt_ms: u64) -> PeerSignal {
        PeerSignal {
            rtt_ms,
            ..Default::default()
        }
    }

    #[test]
    fn high_but_stable_rtt_is_not_congestion() {
        let mut ctrl = SendQualityController::new();
        for _ in 0..20 {
            assert_eq!(tick(&mut ctrl, healthy(260)), 0);
        }
    }

    #[test]
    fn rising_rtt_is_congestion() {
        let mut ctrl = SendQualityController::new();
        tick(&mut ctrl, healthy(80));
        tick(&mut ctrl, healthy(80));
        assert_eq!(tick(&mut ctrl, healthy(400)), 1);
    }

    #[test]
    fn steps_down_with_cooldown_and_recovers_slowly() {
        let mut ctrl = SendQualityController::new();
        let bad = PeerSignal {
            rtt_ms: 50,
            dropped_video_frames: 5,
            ..Default::default()
        };
        tick(&mut ctrl, healthy(50));
        assert_eq!(tick(&mut ctrl, bad), 1);
        // Cooldown: an immediately following bad tick doesn't step again.
        assert_eq!(tick(&mut ctrl, bad), 1);
        assert_eq!(tick(&mut ctrl, bad), 2);
        for _ in 0..RECOVER_AFTER_GOOD_TICKS - 1 {
            assert_eq!(tick(&mut ctrl, healthy(50)), 2);
        }
        assert_eq!(tick(&mut ctrl, healthy(50)), 1);
    }

    #[test]
    fn worst_peer_wins_and_departed_peers_are_forgotten() {
        let mut ctrl = SendQualityController::new();
        let bad = PeerSignal {
            lost_packets: 100,
            ..Default::default()
        };
        ctrl.tick(&[("a".into(), healthy(40)), ("b".into(), healthy(40))]);
        let level = ctrl.tick(&[("a".into(), healthy(40)), ("b".into(), bad)]);
        assert_eq!(level, 1);
        assert_eq!(ctrl.tick(&[("a".into(), healthy(40))]), 0);
    }

    #[test]
    fn levels_scale_the_base_profile() {
        let base = VideoProfile {
            bitrate_bps: 400_000,
            fps: 12,
            max_width: 640,
            max_height: 360,
        };
        assert_eq!(apply_level(base, 0), base);
        let l1 = apply_level(base, 1);
        assert_eq!((l1.bitrate_bps, l1.fps, l1.max_width), (240_000, 8, 640));
        let l3 = apply_level(base, 3);
        assert_eq!((l3.bitrate_bps, l3.fps, l3.max_width, l3.max_height), (80_000, 5, 320, 180));
    }
}
