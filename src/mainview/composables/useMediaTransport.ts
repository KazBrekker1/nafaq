import { ref, onUnmounted } from "vue";
import {
  STREAM_AUDIO,
  STREAM_VIDEO,
  encodeMediaFrame,
  decodeMediaFrame,
} from "../../shared/types";
import { hexToBytes, bytesToHex } from "../lib/hex";

const SIDECAR_PORT = 9320;

export type OnAudioFrame = (peerId: string, data: Uint8Array, timestamp: number) => void;
export type OnVideoFrame = (peerId: string, data: Uint8Array, timestamp: number) => void;

export function useMediaTransport() {
  const connected = ref(false);
  let ws: WebSocket | null = null;
  let onAudio: OnAudioFrame | null = null;
  let onVideo: OnVideoFrame | null = null;

  function connect() {
    if (ws) return;
    ws = new WebSocket(`ws://127.0.0.1:${SIDECAR_PORT}`);
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

    ws.onerror = () => {};
  }

  function disconnect() {
    if (ws) { ws.close(); ws = null; }
    connected.value = false;
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

  function sendAudio(peerIdHex: string, data: Uint8Array) {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(encodeMediaFrame({
      streamType: STREAM_AUDIO,
      peerId: hexToBytes(peerIdHex),
      timestampMs: BigInt(Date.now()),
      payload: data,
    }));
  }

  function sendVideo(peerIdHex: string, data: Uint8Array) {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(encodeMediaFrame({
      streamType: STREAM_VIDEO,
      peerId: hexToBytes(peerIdHex),
      timestampMs: BigInt(Date.now()),
      payload: data,
    }));
  }

  function setOnAudio(handler: OnAudioFrame) { onAudio = handler; }
  function setOnVideo(handler: OnVideoFrame) { onVideo = handler; }

  onUnmounted(() => { disconnect(); });

  return { connected, connect, disconnect, sendAudio, sendVideo, setOnAudio, setOnVideo };
}
