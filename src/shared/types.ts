// ============================================================
// IPC types shared between Bun main process and webview.
// These MUST match the Rust sidecar's message protocol exactly.
// See: sidecar/src/messages.rs
// ============================================================

/** Stream type identifiers for binary media frames */
export const STREAM_AUDIO = 0x01;
export const STREAM_VIDEO = 0x02;
export const STREAM_CHAT = 0x03;
export const STREAM_CONTROL = 0x04;

/** Control actions sent between peers */
export type ControlAction =
  | { action: "mute"; muted: boolean }
  | { action: "video_off"; off: boolean }
  | { action: "peer_announce"; peer_id: string; ticket: string };

/** Commands sent from Bun -> Sidecar (JSON text frames) */
export type Command =
  | { type: "get_node_info" }
  | { type: "create_call" }
  | { type: "join_call"; ticket: string }
  | { type: "end_call"; peer_id: string }
  | { type: "send_chat"; peer_id: string; message: string }
  | { type: "send_control"; peer_id: string; action: ControlAction };

/** Events sent from Sidecar -> Bun (JSON text frames) */
export type Event =
  | { type: "node_info"; id: string; ticket: string }
  | { type: "call_created"; ticket: string }
  | { type: "peer_connected"; peer_id: string }
  | { type: "peer_disconnected"; peer_id: string }
  | { type: "chat_received"; peer_id: string; message: string }
  | { type: "control_received"; peer_id: string; action: ControlAction }
  | { type: "connection_status"; peer_id: string; status: "direct" | "relayed" | "connecting" }
  | { type: "error"; message: string };

/** Media frame binary header (41 bytes) */
export const MEDIA_FRAME_HEADER_SIZE = 41; // 1 + 32 + 8

export interface MediaFrame {
  streamType: number;
  peerId: Uint8Array; // 32 bytes
  timestampMs: bigint;
  payload: Uint8Array;
}

export function encodeMediaFrame(frame: MediaFrame): Uint8Array {
  const buf = new Uint8Array(MEDIA_FRAME_HEADER_SIZE + frame.payload.length);
  buf[0] = frame.streamType;
  buf.set(frame.peerId, 1);
  const view = new DataView(buf.buffer);
  view.setBigUint64(33, frame.timestampMs, false); // big-endian
  buf.set(frame.payload, MEDIA_FRAME_HEADER_SIZE);
  return buf;
}

export function decodeMediaFrame(data: Uint8Array): MediaFrame | null {
  if (data.length < MEDIA_FRAME_HEADER_SIZE) return null;
  const view = new DataView(data.buffer, data.byteOffset);
  return {
    streamType: data[0],
    peerId: data.slice(1, 33),
    timestampMs: view.getBigUint64(33, false),
    payload: data.slice(MEDIA_FRAME_HEADER_SIZE),
  };
}
