import { Channel } from "@tauri-apps/api/core";
import { handleIncomingPcm } from "./audioPlayback";
import { handleIncomingVideoFrame, handleRawNalu, supportsWebCodecsDecode } from "./receiveVideo";
import {
  corePromise,
  ensureReceiveRun,
  fromBase64,
  invokePromise,
  listenForRun,
  peerMediaStates,
  currentReceiveRun,
  sharedTextDecoder,
  toArrayBuffer,
  TransportStoppedError,
} from "./shared";

// Backend → WebView media bridge: binary Tauri channels where available,
// base64 events as the fallback, verified with a probe packet before use.

type MediaBridgeMode = "channel_binary" | "event_base64";

interface MediaSessionProfile {
  sessionId: string;
  receiveBridgeMode: MediaBridgeMode;
  receiveVideoMode: "decoded_jpeg" | "raw_h264_nalu";
}

interface MediaBridgeRegistration {
  sessionId: string;
  preferredBridgeModes: MediaBridgeMode[];
  webcodecsActive: boolean;
}

interface LegacyAudioEvent {
  peer_id: string;
  data: string;
  timestamp: number;
}

interface LegacyVideoEvent {
  peer_id: string;
  data: string;
  width: number;
  height: number;
  timestamp: number;
}

const BRIDGE_PROBE_TIMEOUT_MS = 1000;
const BRIDGE_PROBE_PEER_ID = "__bridge_probe__";

let bridgeSessionId: string | null = null;
let unlistenAudio: (() => void) | null = null;
let unlistenVideo: (() => void) | null = null;
let audioChannel: Channel<ArrayBuffer> | null = null;
let videoChannel: Channel<ArrayBuffer> | null = null;
let bridgeProbeResolver: (() => void) | null = null;
let bridgeProbeReceived = false;
let bridgeFallbackUsed = false;

function createSessionId() {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `media-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function unpackAudioChannelPacket(packet: ArrayBuffer) {
  const view = new DataView(packet);
  let offset = 0;
  const peerIdLen = view.getUint16(offset, true); offset += 2;
  const peerIdBytes = new Uint8Array(packet, offset, peerIdLen); offset += peerIdLen;
  const timestamp = Number(view.getBigUint64(offset, true)); offset += 8;
  const pcmLen = view.getUint32(offset, true); offset += 4;
  const pcmBytes = new Uint8Array(packet, offset, pcmLen);
  return {
    peerId: sharedTextDecoder.decode(peerIdBytes),
    timestamp,
    pcmBytes,
  };
}

function unpackVideoChannelPacket(packet: ArrayBuffer) {
  const view = new DataView(packet);
  let offset = 0;
  const peerIdLen = view.getUint16(offset, true); offset += 2;
  const peerIdBytes = new Uint8Array(packet, offset, peerIdLen); offset += peerIdLen;
  const timestamp = Number(view.getBigUint64(offset, true)); offset += 8;
  const width = view.getUint32(offset, true); offset += 4;
  const height = view.getUint32(offset, true); offset += 4;
  const jpegLen = view.getUint32(offset, true); offset += 4;
  const jpegBytes = new Uint8Array(packet, offset, jpegLen);
  return {
    peerId: sharedTextDecoder.decode(peerIdBytes),
    timestamp,
    width,
    height,
    jpegBytes,
  };
}

function parseRawNaluPacket(buf: ArrayBuffer) {
  const view = new DataView(buf);
  let offset = 0;
  const peerIdLen = view.getUint16(offset, true); offset += 2;
  const peerId = sharedTextDecoder.decode(new Uint8Array(buf, offset, peerIdLen)); offset += peerIdLen;
  const timestamp = Number(view.getBigUint64(offset, true)); offset += 8;
  const isKeyframe = view.getUint8(offset) === 1; offset += 1;
  const dataLen = view.getUint32(offset, true); offset += 4;
  const h264Data = new Uint8Array(buf, offset, dataLen);
  return { peerId, timestamp, isKeyframe, h264Data };
}

function resolveBridgeProbe() {
  bridgeProbeReceived = true;
  const resolve = bridgeProbeResolver;
  bridgeProbeResolver = null;
  resolve?.();
}

function waitForBridgeProbe(timeoutMs = BRIDGE_PROBE_TIMEOUT_MS) {
  if (bridgeProbeReceived) {
    return Promise.resolve();
  }
  return new Promise<void>((resolve, reject) => {
    const timeout = window.setTimeout(() => {
      bridgeProbeResolver = null;
      reject(new Error("Media bridge probe timed out"));
    }, timeoutMs);

    bridgeProbeResolver = () => {
      clearTimeout(timeout);
      resolve();
    };
  });
}

function handleBridgeAudio(peerId: string, pcmBytes: Uint8Array) {
  if (peerId === BRIDGE_PROBE_PEER_ID) {
    resolveBridgeProbe();
    return;
  }
  handleIncomingPcm(peerId, pcmBytes);
}

export function resetBridgeFallback() {
  bridgeFallbackUsed = false;
}

export async function setupReceiveBridge(token: number, forceEventMode = false) {
  const { invoke } = await corePromise;

  const sessionId = createSessionId();
  const preferredBridgeModes: MediaBridgeMode[] = forceEventMode
    ? ["event_base64"]
    : ["channel_binary", "event_base64"];

  const nextAudioChannel = new Channel<ArrayBuffer>();
  const nextVideoChannel = new Channel<ArrayBuffer>();

  const registration: MediaBridgeRegistration = {
    sessionId,
    preferredBridgeModes,
    // Raw NALUs only travel over the binary channel; in event mode the
    // backend has to decode to JPEG, or no video would arrive at all.
    webcodecsActive: !forceEventMode && await supportsWebCodecsDecode(),
  };

  const profile = await invoke<MediaSessionProfile>("register_media_bridge", {
    registration,
    audio: nextAudioChannel,
    video: nextVideoChannel,
  });
  if (token !== currentReceiveRun()) {
    // stop() ran while registering; it couldn't know this session id.
    await invoke("clear_media_bridge", { sessionId: profile.sessionId }).catch(() => {});
    throw new TransportStoppedError();
  }

  bridgeSessionId = profile.sessionId;
  bridgeProbeReceived = false;

  audioChannel = nextAudioChannel;
  videoChannel = nextVideoChannel;

  if (profile.receiveBridgeMode === "channel_binary") {
    audioChannel.onmessage = (raw) => {
      const packet = toArrayBuffer(raw);
      if (packet.byteLength === 0) return;
      const { peerId, pcmBytes } = unpackAudioChannelPacket(packet);
      handleBridgeAudio(peerId, pcmBytes);
    };
    unlistenAudio = () => {
      if (audioChannel) {
        audioChannel.onmessage = () => {};
      }
      audioChannel = null;
    };

    videoChannel.onmessage = (raw) => {
      const packet = toArrayBuffer(raw);
      if (packet.byteLength === 0) return;
      if (profile.receiveVideoMode === "raw_h264_nalu") {
        const { peerId, timestamp, isKeyframe, h264Data } = parseRawNaluPacket(packet);
        handleRawNalu(peerId, timestamp, isKeyframe, h264Data);
        return; // Skip JPEG path
      }
      const { peerId, timestamp, width, height, jpegBytes } = unpackVideoChannelPacket(packet);
      const peerState = peerMediaStates.get(peerId);
      if (!peerState || peerState.videoPaused) return;
      handleIncomingVideoFrame(peerId, timestamp, width, height, jpegBytes).catch(() => {});
    };
    unlistenVideo = () => {
      if (videoChannel) {
        videoChannel.onmessage = () => {};
      }
      videoChannel = null;
    };
  } else {
    unlistenAudio = await listenForRun<LegacyAudioEvent>(token, "audio-received", (event) => {
      const payload = event.payload;
      handleBridgeAudio(payload.peer_id, fromBase64(payload.data));
    });
    unlistenVideo = await listenForRun<LegacyVideoEvent>(token, "video-received", (event) => {
      const payload = event.payload;
      const peerState = peerMediaStates.get(payload.peer_id);
      if (peerState?.videoPaused) return;
      handleIncomingVideoFrame(
        payload.peer_id,
        payload.timestamp,
        payload.width,
        payload.height,
        fromBase64(payload.data),
      ).catch(() => {});
    });
  }

  const probeWait = waitForBridgeProbe();
  await invoke("probe_media_bridge", { sessionId: profile.sessionId });
  ensureReceiveRun(token);

  try {
    await probeWait;
  } catch (error) {
    // After stop() the bridge globals may already belong to a newer run.
    ensureReceiveRun(token);
    await teardownReceiveBridge(false);
    if (!bridgeFallbackUsed && profile.receiveBridgeMode === "channel_binary") {
      bridgeFallbackUsed = true;
      await invoke("clear_media_bridge", { sessionId: profile.sessionId }).catch(() => {});
      ensureReceiveRun(token);
      await setupReceiveBridge(token, true);
      return;
    }
    console.warn("[transport] Media bridge setup failed:", error);
    throw error;
  }
}

export async function teardownReceiveBridge(clearBackend = true) {
  const sessionId = bridgeSessionId;
  bridgeSessionId = null;

  unlistenAudio?.();
  unlistenVideo?.();
  unlistenAudio = null;
  unlistenVideo = null;
  audioChannel = null;
  videoChannel = null;
  bridgeProbeResolver = null;
  bridgeProbeReceived = false;

  if (clearBackend && sessionId) {
    const invoke = await invokePromise;
    await invoke("clear_media_bridge", { sessionId }).catch(() => {});
  }
}
