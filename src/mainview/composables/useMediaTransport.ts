import { ref, onUnmounted } from "vue";
import {
  STREAM_AUDIO,
  STREAM_VIDEO,
  encodeMediaFrame,
  decodeMediaFrame,
} from "../../shared/types";
import { hexToBytes, bytesToHex } from "../lib/hex";

const SIDECAR_PORT = 9320;

export type OnMediaFrame = (peerId: string, data: Uint8Array, timestamp: number) => void;

export function useMediaTransport() {
  const connected = ref(false);
  let ws: WebSocket | null = null;
  let onAudio: OnMediaFrame | null = null;
  let onVideo: OnMediaFrame | null = null;

  // Cache peer ID bytes to avoid re-parsing on every frame
  const peerBytesCache = new Map<string, Uint8Array>();

  function getPeerBytes(peerIdHex: string): Uint8Array {
    let bytes = peerBytesCache.get(peerIdHex);
    if (!bytes) {
      bytes = hexToBytes(peerIdHex);
      peerBytesCache.set(peerIdHex, bytes);
    }
    return bytes;
  }

  function connect() {
    if (ws) return;
    ws = new WebSocket(`ws://localhost:${SIDECAR_PORT}`);
    ws.binaryType = "arraybuffer";

    ws.onopen = () => {
      console.log("[media-transport] Connected to sidecar");
      connected.value = true;
    };

    ws.onmessage = (event: MessageEvent) => {
      if (event.data instanceof ArrayBuffer) {
        handleBinaryFrame(new Uint8Array(event.data));
      }
    };

    ws.onclose = () => {
      console.log("[media-transport] Disconnected");
      connected.value = false;
      ws = null;
    };

    ws.onerror = (e) => {
      console.warn("[media-transport] WebSocket error:", e);
    };
  }

  function disconnect() {
    if (ws) { ws.close(); ws = null; }
    connected.value = false;
    peerBytesCache.clear();
  }

  function handleBinaryFrame(data: Uint8Array) {
    const frame = decodeMediaFrame(data);
    if (!frame) return;
    const peerId = bytesToHex(frame.peerId);
    const timestamp = Number(frame.timestampMs);
    if (frame.streamType === STREAM_AUDIO && onAudio) {
      onAudio(peerId, frame.payload, timestamp);
    } else if (frame.streamType === STREAM_VIDEO && onVideo) {
      onVideo(peerId, frame.payload, timestamp);
    }
  }

  function sendFrame(streamType: number, peerIdHex: string, data: Uint8Array) {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(encodeMediaFrame({
      streamType,
      peerId: getPeerBytes(peerIdHex),
      timestampMs: BigInt(Date.now()),
      payload: data,
    }));
  }

  function sendAudio(peerIdHex: string, data: Uint8Array) { sendFrame(STREAM_AUDIO, peerIdHex, data); }
  function sendVideo(peerIdHex: string, data: Uint8Array) { sendFrame(STREAM_VIDEO, peerIdHex, data); }

  function setOnAudio(handler: OnMediaFrame) { onAudio = handler; }
  function setOnVideo(handler: OnMediaFrame) { onVideo = handler; }

  onUnmounted(() => { disconnect(); });

  return { connected, connect, disconnect, sendAudio, sendVideo, setOnAudio, setOnVideo };
}
