# Nafaq Frontend Implementation Plan (Plan 3 of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Vue frontend with all 5 screens (home, ticket exchange, lobby, in-call, group grid), call state management, camera/mic preview, and in-call chat — all using the brutalist design aesthetic.

**Architecture:** Vue 3 SPA with vue-router for page navigation, Tailwind CSS for styling, and three composables (`useCall`, `useMedia`, `useChat`) that wrap the `nafaq` API from Plan 2. Pages are state-driven — the call composable manages transitions between home → lobby → call → home.

**Tech Stack:** Vue 3, vue-router 4, Tailwind CSS 4, TypeScript

**Prerequisites:** Plans 1+2 complete. The `nafaq` API is available via `inject("nafaq")` in any Vue component.

**Scope note:** This plan covers UI, call lifecycle, camera preview, and chat. Remote video/audio streaming (WebCodecs encoding, binary frame transport) is deferred to a future plan — the call page shows local video preview and a placeholder for remote video.

---

## File Structure

```
src/mainview/
├── index.html                  # (exists) — add Tailwind + font
├── index.ts                    # (exists) — add router
├── App.vue                     # (exists) — replace with router-view
├── main.css                    # Global brutalist styles + Tailwind import
├── router.ts                   # Vue Router config
├── pages/
│   ├── HomePage.vue            # Home: New Call / Join Call
│   ├── LobbyPage.vue          # Pre-call: camera preview + device selection
│   └── CallPage.vue            # In-call: video grid + controls + chat
├── components/
│   ├── TicketCreate.vue        # Generate + display ticket with copy/QR
│   ├── TicketJoin.vue          # Paste ticket + connect
│   ├── CallControls.vue        # Mic | Cam | Chat | End buttons
│   ├── ChatSidebar.vue         # In-call chat messages + input
│   └── VideoGrid.vue           # Adaptive video tile layout
└── composables/
    ├── useCall.ts              # Call state machine + event handling
    ├── useMedia.ts             # getUserMedia + device enumeration
    └── useChat.ts              # Chat message history + send
```

## Design System

All components follow the brutalist aesthetic:
- **Font:** `'JetBrains Mono', monospace` (loaded via Google Fonts)
- **Colors:** `--bg: #000`, `--fg: #e2e8f0`, `--accent: #8B5CF6`, `--border: #e2e8f0`, `--muted: #666`, `--danger: #ff0000`
- **Borders:** `2px solid` with sharp corners (no border-radius)
- **Labels:** Uppercase + `letter-spacing: 3px` + `font-size: 10px`
- **Buttons:** Monospace, `font-weight: 700`, no border-radius
- **Inputs:** Black bg, white border, monospace

---

### Task 1: Tailwind CSS + Router + App Shell

**Files:**
- Modify: `package.json` (add deps)
- Create: `src/mainview/main.css`
- Modify: `src/mainview/index.html` (add font + CSS link)
- Create: `src/mainview/router.ts`
- Modify: `src/mainview/index.ts` (add router)
- Modify: `src/mainview/App.vue` (replace with router-view shell)
- Modify: `vite.config.ts` (add Tailwind)

- [ ] **Step 1: Install dependencies**

Run: `bun add vue-router@4 && bun add -d tailwindcss @tailwindcss/vite`

- [ ] **Step 2: Update vite.config.ts to add Tailwind**

`vite.config.ts`:
```typescript
import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [vue(), tailwindcss()],
  root: "src/mainview",
  build: {
    outDir: "../../dist",
    emptyOutDir: true,
  },
  server: {
    port: 5173,
    strictPort: true,
  },
});
```

- [ ] **Step 3: Create main.css with Tailwind + brutalist base styles**

`src/mainview/main.css`:
```css
@import "tailwindcss";

@theme {
  --font-mono: 'JetBrains Mono', 'SF Mono', 'Fira Code', ui-monospace, monospace;
  --color-accent: #8B5CF6;
  --color-surface: #000000;
  --color-surface-alt: #0a0a0a;
  --color-border: #e2e8f0;
  --color-border-muted: #333333;
  --color-muted: #666666;
  --color-danger: #ff0000;
}

/* ── Base ─────────────────────────────────────────────── */

body {
  margin: 0;
  background: var(--color-surface);
  color: var(--color-border);
  font-family: var(--font-mono);
  font-size: 14px;
  line-height: 1.6;
}

/* ── Brutalist Label ──────────────────────────────────── */

.label {
  font-size: 10px;
  text-transform: uppercase;
  letter-spacing: 3px;
  font-weight: 700;
  color: var(--color-muted);
}

/* ── Brutalist Button ─────────────────────────────────── */

.btn {
  font-family: var(--font-mono);
  font-weight: 700;
  font-size: 13px;
  padding: 12px 28px;
  border: 2px solid var(--color-border);
  cursor: pointer;
  transition: background 0.1s, color 0.1s;
}

.btn-primary {
  background: var(--color-border);
  color: #000;
}
.btn-primary:hover {
  background: #000;
  color: var(--color-border);
}

.btn-outline {
  background: transparent;
  color: var(--color-border);
}
.btn-outline:hover {
  background: var(--color-border);
  color: #000;
}

.btn-danger {
  background: var(--color-danger);
  color: #fff;
  border-color: var(--color-danger);
}

/* ── Brutalist Input ──────────────────────────────────── */

.input {
  font-family: var(--font-mono);
  font-size: 13px;
  padding: 12px 16px;
  background: #000;
  color: var(--color-border);
  border: 2px solid var(--color-border);
  width: 100%;
  box-sizing: border-box;
  outline: none;
}
.input:focus {
  border-color: var(--color-accent);
}
.input::placeholder {
  color: var(--color-muted);
}
```

- [ ] **Step 4: Update index.html to load font and CSS**

`src/mainview/index.html`:
```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>Nafaq</title>
  <link rel="preconnect" href="https://fonts.googleapis.com" />
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin />
  <link href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;500;700;900&display=swap" rel="stylesheet" />
</head>
<body>
  <div id="app"></div>
  <script type="module" src="./index.ts"></script>
</body>
</html>
```

- [ ] **Step 5: Create router.ts**

`src/mainview/router.ts`:
```typescript
import { createRouter, createWebHashHistory } from "vue-router";

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    {
      path: "/",
      name: "home",
      component: () => import("./pages/HomePage.vue"),
    },
    {
      path: "/lobby",
      name: "lobby",
      component: () => import("./pages/LobbyPage.vue"),
    },
    {
      path: "/call",
      name: "call",
      component: () => import("./pages/CallPage.vue"),
    },
  ],
});

export default router;
```

- [ ] **Step 6: Update index.ts to use router and CSS**

`src/mainview/index.ts` — add router and CSS import at the top, keep existing nafaq API code. Replace the mount section:

Add `import "./main.css";` at the top (line 1).
Add `import router from "./router";` after the vue import.
Change `app.mount("#app");` to `app.use(router).mount("#app");`.

- [ ] **Step 7: Replace App.vue with router shell**

`src/mainview/App.vue`:
```vue
<template>
  <router-view />
</template>
```

- [ ] **Step 8: Create placeholder pages**

`src/mainview/pages/HomePage.vue`:
```vue
<template>
  <div class="min-h-screen flex items-center justify-center">
    <div class="text-center">
      <h1 class="text-5xl font-black tracking-[8px]">NAFAQ</h1>
      <p class="label mt-2">P2P Encrypted Calls</p>
    </div>
  </div>
</template>
```

`src/mainview/pages/LobbyPage.vue`:
```vue
<template>
  <div class="min-h-screen flex items-center justify-center">
    <p class="label">Lobby — coming next</p>
  </div>
</template>
```

`src/mainview/pages/CallPage.vue`:
```vue
<template>
  <div class="min-h-screen flex items-center justify-center">
    <p class="label">Call — coming next</p>
  </div>
</template>
```

- [ ] **Step 9: Verify Vite builds**

Run: `bunx vite build --config vite.config.ts`
Expected: Build succeeds

- [ ] **Step 10: Commit**

```bash
git add package.json bun.lock vite.config.ts src/mainview/
git commit -m "feat(ui): add Tailwind CSS, vue-router, and brutalist design system"
```

---

### Task 2: Call State Composable

**Files:**
- Create: `src/mainview/composables/useCall.ts`

- [ ] **Step 1: Implement call state machine**

`src/mainview/composables/useCall.ts`:
```typescript
import { ref, inject, onMounted, onUnmounted, computed } from "vue";
import { useRouter } from "vue-router";
import type { Event } from "../../shared/types";

export type CallState = "idle" | "creating" | "joining" | "waiting" | "connected";

export function useCall() {
  const nafaq = inject<any>("nafaq");
  const router = useRouter();

  const state = ref<CallState>("idle");
  const ticket = ref<string | null>(null);
  const peerId = ref<string | null>(null);
  const nodeId = ref<string | null>(null);
  const sidecarConnected = ref(false);
  const error = ref<string | null>(null);

  // All connected peer IDs (for group calls)
  const peers = ref<string[]>([]);

  let unsubEvent: (() => void) | undefined;
  let unsubStatus: (() => void) | undefined;

  function handleEvent(event: Event) {
    switch (event.type) {
      case "node_info":
        nodeId.value = event.id;
        break;

      case "call_created":
        ticket.value = event.ticket;
        state.value = "waiting";
        break;

      case "peer_connected":
        if (!peers.value.includes(event.peer_id)) {
          peers.value.push(event.peer_id);
        }
        peerId.value = event.peer_id;
        state.value = "connected";
        router.push("/call");
        break;

      case "peer_disconnected": {
        const idx = peers.value.indexOf(event.peer_id);
        if (idx >= 0) peers.value.splice(idx, 1);
        if (peers.value.length === 0) {
          state.value = "idle";
          peerId.value = null;
          ticket.value = null;
          router.push("/");
        }
        break;
      }

      case "error":
        error.value = event.message;
        break;
    }
  }

  onMounted(() => {
    unsubEvent = nafaq?.onEvent(handleEvent);
    unsubStatus = nafaq?.onStatus((s: { connected: boolean }) => {
      sidecarConnected.value = s.connected;
    });
    nafaq?.getStatus().then((s: any) => {
      sidecarConnected.value = s.connected;
      nodeId.value = s.nodeId;
    });
  });

  onUnmounted(() => {
    unsubEvent?.();
    unsubStatus?.();
  });

  async function createCall() {
    error.value = null;
    state.value = "creating";
    await nafaq?.createCall();
  }

  async function joinCall(t: string) {
    error.value = null;
    state.value = "joining";
    ticket.value = t;
    await nafaq?.joinCall(t);
  }

  async function endCall() {
    for (const p of peers.value) {
      await nafaq?.endCall(p);
    }
    state.value = "idle";
    peerId.value = null;
    peers.value = [];
    ticket.value = null;
    router.push("/");
  }

  async function sendControl(action: any) {
    if (!peerId.value) return;
    await nafaq?.sendCommand({
      type: "send_control",
      peer_id: peerId.value,
      action,
    });
  }

  return {
    state,
    ticket,
    peerId,
    nodeId,
    peers,
    sidecarConnected,
    error,
    createCall,
    joinCall,
    endCall,
    sendControl,
  };
}
```

- [ ] **Step 2: Verify it compiles**

Run: `bunx tsc --noEmit --skipLibCheck`
Expected: No errors

- [ ] **Step 3: Commit**

```bash
git add src/mainview/composables/useCall.ts
git commit -m "feat(ui): call state composable with lifecycle management"
```

---

### Task 3: Home Page (New Call + Join Call)

**Files:**
- Create: `src/mainview/components/TicketCreate.vue`
- Create: `src/mainview/components/TicketJoin.vue`
- Modify: `src/mainview/pages/HomePage.vue`

- [ ] **Step 1: Create TicketCreate component**

`src/mainview/components/TicketCreate.vue`:
```vue
<script setup lang="ts">
import { ref } from "vue";

const props = defineProps<{
  ticket: string | null;
  state: string;
}>();

const emit = defineEmits<{
  create: [];
}>();

const copied = ref(false);

function copyTicket() {
  if (!props.ticket) return;
  navigator.clipboard.writeText(props.ticket);
  copied.value = true;
  setTimeout(() => (copied.value = false), 2000);
}
</script>

<template>
  <div>
    <p class="label mb-4">SHARE THIS TICKET</p>

    <div v-if="!ticket && state === 'idle'">
      <button class="btn btn-primary w-full" @click="emit('create')">
        New Call
      </button>
    </div>

    <div v-else-if="state === 'creating'">
      <p class="text-[var(--color-muted)] text-xs tracking-widest">Creating...</p>
    </div>

    <div v-else-if="ticket" class="space-y-4">
      <div class="border-2 border-[var(--color-accent)] p-4 text-xs break-all text-[var(--color-border)] bg-[#111]">
        {{ ticket }}
      </div>

      <button class="btn btn-primary w-full" @click="copyTicket">
        {{ copied ? "Copied!" : "Copy Ticket" }}
      </button>

      <p class="text-[var(--color-muted)] text-xs tracking-widest text-center">
        Waiting for peer<span class="text-[var(--color-accent)]">_</span>
      </p>
    </div>
  </div>
</template>
```

- [ ] **Step 2: Create TicketJoin component**

`src/mainview/components/TicketJoin.vue`:
```vue
<script setup lang="ts">
import { ref } from "vue";

const props = defineProps<{
  state: string;
}>();

const emit = defineEmits<{
  join: [ticket: string];
}>();

const ticketInput = ref("");

function submit() {
  const t = ticketInput.value.trim();
  if (!t) return;
  emit("join", t);
}
</script>

<template>
  <div>
    <p class="label mb-4">ENTER TICKET</p>

    <input
      v-model="ticketInput"
      class="input mb-4"
      placeholder="Paste ticket..."
      @keyup.enter="submit"
      :disabled="state === 'joining'"
    />

    <button
      class="btn btn-primary w-full"
      @click="submit"
      :disabled="!ticketInput.trim() || state === 'joining'"
    >
      {{ state === "joining" ? "Connecting..." : "Connect" }}
    </button>
  </div>
</template>
```

- [ ] **Step 3: Build the Home page**

`src/mainview/pages/HomePage.vue`:
```vue
<script setup lang="ts">
import { useCall } from "../composables/useCall";
import TicketCreate from "../components/TicketCreate.vue";
import TicketJoin from "../components/TicketJoin.vue";

const { state, ticket, nodeId, sidecarConnected, error, createCall, joinCall } = useCall();
</script>

<template>
  <div class="min-h-screen flex items-center justify-center p-8">
    <div class="w-full max-w-xl">
      <!-- Header -->
      <div class="text-center mb-12">
        <h1 class="text-5xl font-black tracking-[8px]">NAFAQ</h1>
        <p class="label mt-2">P2P Encrypted Calls</p>
      </div>

      <!-- Sidecar status -->
      <div class="flex items-center justify-center gap-2 mb-8">
        <div
          class="w-2 h-2"
          :style="{ background: sidecarConnected ? '#8B5CF6' : '#ff0000' }"
        ></div>
        <span class="text-xs text-[var(--color-muted)]">
          {{ sidecarConnected ? "Connected" : "Disconnected" }}
        </span>
        <span v-if="nodeId" class="text-xs text-[var(--color-muted)]">
          · {{ nodeId.slice(0, 12) }}...
        </span>
      </div>

      <!-- Error -->
      <div
        v-if="error"
        class="border-2 border-[var(--color-danger)] p-3 mb-6 text-xs text-[var(--color-danger)]"
      >
        {{ error }}
      </div>

      <!-- Action panels -->
      <div class="grid grid-cols-2 gap-0">
        <div class="border-2 border-[var(--color-border)] p-6 border-r-0">
          <TicketCreate
            :ticket="ticket"
            :state="state"
            @create="createCall"
          />
        </div>
        <div class="border-2 border-[var(--color-border)] p-6">
          <TicketJoin
            :state="state"
            @join="joinCall"
          />
        </div>
      </div>

      <!-- Node ID -->
      <div class="border-t border-[var(--color-border-muted)] mt-8 pt-4 text-center">
        <p class="label">YOUR NODE</p>
        <p class="text-xs text-[var(--color-muted)] mt-1">
          {{ nodeId || "Loading..." }}
        </p>
      </div>
    </div>
  </div>
</template>
```

- [ ] **Step 4: Verify build**

Run: `bunx vite build --config vite.config.ts`
Expected: Build succeeds

- [ ] **Step 5: Commit**

```bash
git add src/mainview/components/TicketCreate.vue src/mainview/components/TicketJoin.vue src/mainview/pages/HomePage.vue
git commit -m "feat(ui): home page with ticket create/join panels"
```

---

### Task 4: Media Composable (Camera + Mic)

**Files:**
- Create: `src/mainview/composables/useMedia.ts`

- [ ] **Step 1: Implement getUserMedia wrapper**

`src/mainview/composables/useMedia.ts`:
```typescript
import { ref, onUnmounted } from "vue";

export interface MediaDevice {
  deviceId: string;
  label: string;
}

export function useMedia() {
  const localStream = ref<MediaStream | null>(null);
  const cameras = ref<MediaDevice[]>([]);
  const microphones = ref<MediaDevice[]>([]);
  const selectedCamera = ref<string>("");
  const selectedMic = ref<string>("");
  const micLevel = ref(0);
  const audioMuted = ref(false);
  const videoMuted = ref(false);
  const error = ref<string | null>(null);

  let analyserInterval: ReturnType<typeof setInterval> | null = null;
  let audioContext: AudioContext | null = null;

  async function enumerateDevices() {
    try {
      const devices = await navigator.mediaDevices.enumerateDevices();
      cameras.value = devices
        .filter((d) => d.kind === "videoinput")
        .map((d) => ({ deviceId: d.deviceId, label: d.label || `Camera ${d.deviceId.slice(0, 8)}` }));
      microphones.value = devices
        .filter((d) => d.kind === "audioinput")
        .map((d) => ({ deviceId: d.deviceId, label: d.label || `Mic ${d.deviceId.slice(0, 8)}` }));

      if (!selectedCamera.value && cameras.value.length > 0) {
        selectedCamera.value = cameras.value[0].deviceId;
      }
      if (!selectedMic.value && microphones.value.length > 0) {
        selectedMic.value = microphones.value[0].deviceId;
      }
    } catch (e: any) {
      error.value = `Device enumeration failed: ${e.message}`;
    }
  }

  async function startPreview() {
    error.value = null;
    try {
      // Request permissions first (labels are empty until granted)
      const stream = await navigator.mediaDevices.getUserMedia({
        video: selectedCamera.value
          ? { deviceId: { exact: selectedCamera.value } }
          : true,
        audio: selectedMic.value
          ? { deviceId: { exact: selectedMic.value } }
          : true,
      });

      // Stop any existing stream
      stopPreview();

      localStream.value = stream;

      // Re-enumerate to get labels
      await enumerateDevices();

      // Set up mic level monitoring
      startMicLevelMonitor(stream);
    } catch (e: any) {
      error.value = `Camera/mic access failed: ${e.message}`;
    }
  }

  function startMicLevelMonitor(stream: MediaStream) {
    try {
      audioContext = new AudioContext();
      const source = audioContext.createMediaStreamSource(stream);
      const analyser = audioContext.createAnalyser();
      analyser.fftSize = 256;
      source.connect(analyser);

      const dataArray = new Uint8Array(analyser.frequencyBinCount);
      analyserInterval = setInterval(() => {
        analyser.getByteFrequencyData(dataArray);
        const avg = dataArray.reduce((a, b) => a + b, 0) / dataArray.length;
        micLevel.value = Math.min(100, Math.round((avg / 128) * 100));
      }, 100);
    } catch {
      // Audio context may not be available
    }
  }

  function stopPreview() {
    if (localStream.value) {
      localStream.value.getTracks().forEach((t) => t.stop());
      localStream.value = null;
    }
    if (analyserInterval) {
      clearInterval(analyserInterval);
      analyserInterval = null;
    }
    if (audioContext) {
      audioContext.close();
      audioContext = null;
    }
    micLevel.value = 0;
  }

  function toggleAudio() {
    audioMuted.value = !audioMuted.value;
    localStream.value?.getAudioTracks().forEach((t) => {
      t.enabled = !audioMuted.value;
    });
  }

  function toggleVideo() {
    videoMuted.value = !videoMuted.value;
    localStream.value?.getVideoTracks().forEach((t) => {
      t.enabled = !videoMuted.value;
    });
  }

  async function switchCamera(deviceId: string) {
    selectedCamera.value = deviceId;
    if (localStream.value) await startPreview();
  }

  async function switchMic(deviceId: string) {
    selectedMic.value = deviceId;
    if (localStream.value) await startPreview();
  }

  onUnmounted(() => {
    stopPreview();
  });

  return {
    localStream,
    cameras,
    microphones,
    selectedCamera,
    selectedMic,
    micLevel,
    audioMuted,
    videoMuted,
    error,
    enumerateDevices,
    startPreview,
    stopPreview,
    toggleAudio,
    toggleVideo,
    switchCamera,
    switchMic,
  };
}
```

- [ ] **Step 2: Verify it compiles**

Run: `bunx tsc --noEmit --skipLibCheck`
Expected: No errors

- [ ] **Step 3: Commit**

```bash
git add src/mainview/composables/useMedia.ts
git commit -m "feat(ui): media composable with camera/mic preview and level monitoring"
```

---

### Task 5: Lobby Page

**Files:**
- Modify: `src/mainview/pages/LobbyPage.vue`

- [ ] **Step 1: Build the lobby with camera preview**

`src/mainview/pages/LobbyPage.vue`:
```vue
<script setup lang="ts">
import { ref, onMounted, watch } from "vue";
import { useRouter } from "vue-router";
import { useMedia } from "../composables/useMedia";
import { useCall } from "../composables/useCall";

const router = useRouter();
const call = useCall();
const media = useMedia();

const videoEl = ref<HTMLVideoElement | null>(null);

onMounted(async () => {
  if (call.state.value === "idle") {
    router.push("/");
    return;
  }
  await media.startPreview();
});

// Bind stream to video element
watch(
  () => media.localStream.value,
  (stream) => {
    if (videoEl.value && stream) {
      videoEl.value.srcObject = stream;
    }
  },
);

function joinCall() {
  // Navigate is handled by useCall when peer_connected fires
  // The lobby just keeps preview running until connection happens
}

function cancel() {
  media.stopPreview();
  call.endCall();
}
</script>

<template>
  <div class="min-h-screen flex items-center justify-center p-8">
    <div class="border-2 border-[var(--color-border)] flex max-w-4xl w-full">
      <!-- Camera Preview -->
      <div class="flex-[1.3] bg-[#111] relative border-r-2 border-[var(--color-border)] min-h-[400px] flex items-center justify-center">
        <video
          ref="videoEl"
          autoplay
          muted
          playsinline
          class="w-full h-full object-cover absolute inset-0"
        ></video>
        <p v-if="!media.localStream.value" class="text-[var(--color-muted)] text-sm tracking-widest relative z-10">
          {{ media.error.value || "Starting camera..." }}
        </p>
        <div class="absolute top-3 left-4 z-10">
          <span class="text-[var(--color-accent)] text-xs font-bold tracking-widest">● Live</span>
        </div>
      </div>

      <!-- Controls -->
      <div class="flex-1 p-8 flex flex-col justify-center gap-5">
        <div>
          <p class="label mb-2">CAMERA</p>
          <select
            class="input text-xs p-2"
            :value="media.selectedCamera.value"
            @change="media.switchCamera(($event.target as HTMLSelectElement).value)"
          >
            <option v-for="cam in media.cameras.value" :key="cam.deviceId" :value="cam.deviceId">
              {{ cam.label }}
            </option>
          </select>
        </div>

        <div>
          <p class="label mb-2">MICROPHONE</p>
          <select
            class="input text-xs p-2"
            :value="media.selectedMic.value"
            @change="media.switchMic(($event.target as HTMLSelectElement).value)"
          >
            <option v-for="mic in media.microphones.value" :key="mic.deviceId" :value="mic.deviceId">
              {{ mic.label }}
            </option>
          </select>
        </div>

        <div>
          <p class="label mb-2">MIC LEVEL</p>
          <div class="flex gap-[3px] h-4 items-end">
            <div
              v-for="i in 10"
              :key="i"
              class="w-1"
              :style="{
                height: `${4 + (i <= media.micLevel.value / 10 ? (media.micLevel.value / 10) * 1.2 : 0)}px`,
                background: i <= media.micLevel.value / 10 ? 'var(--color-accent)' : 'var(--color-border-muted)',
              }"
            ></div>
          </div>
        </div>

        <div class="flex gap-0 mt-2">
          <button
            class="btn btn-outline text-xs px-4 py-2.5 border-r-0"
            :class="{ 'bg-[var(--color-danger)] border-[var(--color-danger)] text-white': media.audioMuted.value }"
            @click="media.toggleAudio"
          >
            {{ media.audioMuted.value ? "Mic Off" : "Mic On" }}
          </button>
          <button
            class="btn btn-outline text-xs px-4 py-2.5"
            :class="{ 'bg-[var(--color-danger)] border-[var(--color-danger)] text-white': media.videoMuted.value }"
            @click="media.toggleVideo"
          >
            {{ media.videoMuted.value ? "Cam Off" : "Cam On" }}
          </button>
        </div>

        <div class="flex gap-0 mt-2">
          <button class="btn btn-outline text-xs flex-1 border-r-0" @click="cancel">
            Cancel
          </button>
          <button class="btn btn-primary text-xs flex-1" disabled>
            {{ call.state.value === "waiting" ? "Waiting..." : call.state.value === "joining" ? "Connecting..." : "Ready" }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>
```

- [ ] **Step 2: Update useCall to navigate to lobby**

In `src/mainview/composables/useCall.ts`, update `createCall` and `joinCall` to navigate to lobby:

After `await nafaq?.createCall();` add: `router.push("/lobby");`
After `await nafaq?.joinCall(t);` add: `router.push("/lobby");`

- [ ] **Step 3: Verify build**

Run: `bunx vite build --config vite.config.ts`
Expected: Build succeeds

- [ ] **Step 4: Commit**

```bash
git add src/mainview/pages/LobbyPage.vue src/mainview/composables/useCall.ts
git commit -m "feat(ui): lobby page with camera preview and device selection"
```

---

### Task 6: Chat Composable + Sidebar

**Files:**
- Create: `src/mainview/composables/useChat.ts`
- Create: `src/mainview/components/ChatSidebar.vue`

- [ ] **Step 1: Implement chat composable**

`src/mainview/composables/useChat.ts`:
```typescript
import { ref, inject, onMounted, onUnmounted } from "vue";
import type { Event } from "../../shared/types";

export interface ChatMessage {
  id: string;
  sender: "you" | "peer";
  peerId?: string;
  text: string;
  timestamp: number;
}

export function useChat() {
  const nafaq = inject<any>("nafaq");
  const messages = ref<ChatMessage[]>([]);

  let unsubEvent: (() => void) | undefined;

  onMounted(() => {
    unsubEvent = nafaq?.onEvent((event: Event) => {
      if (event.type === "chat_received") {
        messages.value.push({
          id: crypto.randomUUID(),
          sender: "peer",
          peerId: event.peer_id,
          text: event.message,
          timestamp: Date.now(),
        });
      }
    });
  });

  onUnmounted(() => {
    unsubEvent?.();
  });

  async function sendMessage(peerId: string, text: string) {
    if (!text.trim()) return;
    await nafaq?.sendChat(peerId, text);
    messages.value.push({
      id: crypto.randomUUID(),
      sender: "you",
      text,
      timestamp: Date.now(),
    });
  }

  function clearMessages() {
    messages.value = [];
  }

  return { messages, sendMessage, clearMessages };
}
```

- [ ] **Step 2: Create ChatSidebar component**

`src/mainview/components/ChatSidebar.vue`:
```vue
<script setup lang="ts">
import { ref, nextTick, watch } from "vue";
import type { ChatMessage } from "../composables/useChat";

const props = defineProps<{
  messages: ChatMessage[];
  peerId: string;
}>();

const emit = defineEmits<{
  send: [text: string];
}>();

const input = ref("");
const messagesEl = ref<HTMLElement | null>(null);

function submit() {
  const text = input.value.trim();
  if (!text) return;
  emit("send", text);
  input.value = "";
}

// Auto-scroll to bottom
watch(
  () => props.messages.length,
  async () => {
    await nextTick();
    if (messagesEl.value) {
      messagesEl.value.scrollTop = messagesEl.value.scrollHeight;
    }
  },
);
</script>

<template>
  <div class="w-[260px] bg-black border-l-2 border-[var(--color-border)] flex flex-col">
    <!-- Header -->
    <div class="p-3 border-b-2 border-[var(--color-border-muted)]">
      <span class="label">MESSAGES</span>
    </div>

    <!-- Messages -->
    <div ref="messagesEl" class="flex-1 overflow-y-auto">
      <div
        v-for="msg in messages"
        :key="msg.id"
        class="px-4 py-2.5 border-b border-[#1a1a1a]"
        :class="msg.sender === 'you' ? 'bg-[var(--color-surface-alt)]' : ''"
      >
        <span
          class="text-[9px] tracking-widest"
          :style="{ color: msg.sender === 'you' ? 'var(--color-accent)' : 'var(--color-muted)' }"
        >
          {{ msg.sender === "you" ? "You" : "Peer" }} · {{ new Date(msg.timestamp).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) }}
        </span>
        <br />
        <span class="text-xs">{{ msg.text }}</span>
      </div>

      <div v-if="messages.length === 0" class="p-4 text-center text-[var(--color-muted)] text-xs">
        No messages yet
      </div>
    </div>

    <!-- Input -->
    <div class="border-t-2 border-[var(--color-border-muted)]">
      <input
        v-model="input"
        class="input border-0 text-xs py-3.5 px-4"
        placeholder="Type a message..."
        @keyup.enter="submit"
      />
    </div>
  </div>
</template>
```

- [ ] **Step 3: Verify build**

Run: `bunx vite build --config vite.config.ts`
Expected: Build succeeds

- [ ] **Step 4: Commit**

```bash
git add src/mainview/composables/useChat.ts src/mainview/components/ChatSidebar.vue
git commit -m "feat(ui): chat composable and sidebar component"
```

---

### Task 7: Call Controls + Video Grid

**Files:**
- Create: `src/mainview/components/CallControls.vue`
- Create: `src/mainview/components/VideoGrid.vue`

- [ ] **Step 1: Create CallControls component**

`src/mainview/components/CallControls.vue`:
```vue
<script setup lang="ts">
defineProps<{
  audioMuted: boolean;
  videoMuted: boolean;
  chatOpen: boolean;
}>();

const emit = defineEmits<{
  toggleAudio: [];
  toggleVideo: [];
  toggleChat: [];
  endCall: [];
}>();
</script>

<template>
  <div class="flex justify-center gap-0">
    <button
      class="w-[52px] h-12 bg-transparent border-2 border-[var(--color-border)] text-[var(--color-border)] text-xs font-bold cursor-pointer font-mono"
      :class="{ 'bg-[var(--color-danger)] border-[var(--color-danger)]': audioMuted }"
      @click="emit('toggleAudio')"
    >
      Mic
    </button>
    <button
      class="w-[52px] h-12 bg-transparent border-2 border-[var(--color-border)] border-l-0 text-[var(--color-border)] text-xs font-bold cursor-pointer font-mono"
      :class="{ 'bg-[var(--color-danger)] border-[var(--color-danger)]': videoMuted }"
      @click="emit('toggleVideo')"
    >
      Cam
    </button>
    <button
      class="w-[52px] h-12 bg-transparent border-2 border-[var(--color-border)] border-l-0 text-[var(--color-border)] text-xs font-bold cursor-pointer font-mono"
      :class="{ 'bg-[var(--color-accent)] border-[var(--color-accent)]': chatOpen }"
      @click="emit('toggleChat')"
    >
      Chat
    </button>
    <button
      class="w-[52px] h-12 bg-[var(--color-danger)] border-2 border-[var(--color-danger)] border-l-0 text-white text-xs font-bold cursor-pointer font-mono"
      @click="emit('endCall')"
    >
      End
    </button>
  </div>
</template>
```

- [ ] **Step 2: Create VideoGrid component**

`src/mainview/components/VideoGrid.vue`:
```vue
<script setup lang="ts">
import { computed, ref, watch } from "vue";

const props = defineProps<{
  localStream: MediaStream | null;
  peers: string[];
}>();

const localVideoEl = ref<HTMLVideoElement | null>(null);

watch(
  () => props.localStream,
  (stream) => {
    if (localVideoEl.value && stream) {
      localVideoEl.value.srcObject = stream;
    }
  },
);

const gridCols = computed(() => {
  const total = props.peers.length + 1; // +1 for self
  if (total <= 1) return 1;
  if (total <= 4) return 2;
  return 3;
});
</script>

<template>
  <!-- 1-on-1: full screen remote + PiP self -->
  <div v-if="peers.length === 1" class="relative w-full h-full bg-[var(--color-surface-alt)]">
    <!-- Remote video placeholder -->
    <div class="w-full h-full flex items-center justify-center">
      <span class="text-[var(--color-border-muted)] text-sm font-bold tracking-widest">Remote Video</span>
    </div>

    <!-- Self PiP -->
    <div class="absolute bottom-20 right-4 w-[180px] h-[110px] bg-[#111] border-2 border-[var(--color-border)] overflow-hidden">
      <video
        ref="localVideoEl"
        autoplay
        muted
        playsinline
        class="w-full h-full object-cover"
      ></video>
      <span v-if="!localStream" class="absolute inset-0 flex items-center justify-center text-[var(--color-muted)] text-[10px] tracking-widest">
        You
      </span>
    </div>
  </div>

  <!-- Group: grid layout -->
  <div
    v-else
    class="w-full h-full grid gap-[2px] bg-[var(--color-border)] p-[2px]"
    :style="{ gridTemplateColumns: `repeat(${gridCols}, 1fr)` }"
  >
    <!-- Self tile -->
    <div class="bg-[#111] relative min-h-[140px] flex items-center justify-center">
      <video
        ref="localVideoEl"
        autoplay
        muted
        playsinline
        class="w-full h-full object-cover absolute inset-0"
      ></video>
      <span class="absolute bottom-2 left-2.5 text-[10px] text-[var(--color-accent)] bg-black px-2 py-0.5 font-bold tracking-wider z-10">
        You
      </span>
      <div class="absolute top-2 right-2.5 w-1.5 h-1.5 bg-[var(--color-accent)] z-10"></div>
    </div>

    <!-- Peer tiles (placeholder) -->
    <div
      v-for="peer in peers"
      :key="peer"
      class="bg-[#111] relative min-h-[140px] flex items-center justify-center"
    >
      <span class="text-[var(--color-border-muted)] text-sm font-bold tracking-widest">Peer</span>
      <span class="absolute bottom-2 left-2.5 text-[10px] text-white bg-black px-2 py-0.5 font-bold tracking-wider">
        {{ peer.slice(0, 8) }}...
      </span>
      <div class="absolute top-2 right-2.5 w-1.5 h-1.5 bg-[var(--color-accent)]"></div>
    </div>
  </div>
</template>
```

- [ ] **Step 3: Verify build**

Run: `bunx vite build --config vite.config.ts`
Expected: Build succeeds

- [ ] **Step 4: Commit**

```bash
git add src/mainview/components/CallControls.vue src/mainview/components/VideoGrid.vue
git commit -m "feat(ui): call controls and video grid components"
```

---

### Task 8: Call Page (Wire Everything Together)

**Files:**
- Modify: `src/mainview/pages/CallPage.vue`

- [ ] **Step 1: Build the in-call page**

`src/mainview/pages/CallPage.vue`:
```vue
<script setup lang="ts">
import { ref, onMounted } from "vue";
import { useRouter } from "vue-router";
import { useCall } from "../composables/useCall";
import { useMedia } from "../composables/useMedia";
import { useChat } from "../composables/useChat";
import CallControls from "../components/CallControls.vue";
import ChatSidebar from "../components/ChatSidebar.vue";
import VideoGrid from "../components/VideoGrid.vue";

const router = useRouter();
const call = useCall();
const media = useMedia();
const chat = useChat();

const chatOpen = ref(true);
const callDuration = ref("0:00");
let durationInterval: ReturnType<typeof setInterval> | null = null;
let startTime = Date.now();

onMounted(() => {
  if (call.state.value !== "connected") {
    router.push("/");
    return;
  }

  // Start camera if not already running
  if (!media.localStream.value) {
    media.startPreview();
  }

  // Start call duration timer
  startTime = Date.now();
  durationInterval = setInterval(() => {
    const elapsed = Math.floor((Date.now() - startTime) / 1000);
    const mins = Math.floor(elapsed / 60);
    const secs = elapsed % 60;
    callDuration.value = `${mins}:${secs.toString().padStart(2, "0")}`;
  }, 1000);
});

function handleEndCall() {
  if (durationInterval) clearInterval(durationInterval);
  media.stopPreview();
  chat.clearMessages();
  call.endCall();
}

function handleSendChat(text: string) {
  if (call.peerId.value) {
    chat.sendMessage(call.peerId.value, text);
  }
}
</script>

<template>
  <div class="h-screen flex">
    <!-- Video area -->
    <div class="flex-1 bg-[var(--color-surface-alt)] relative flex flex-col">
      <!-- Top bar -->
      <div class="absolute top-0 left-0 right-0 flex justify-between px-4 py-3 z-20 bg-gradient-to-b from-black/80 to-transparent">
        <span class="text-sm font-black tracking-widest">{{ callDuration }}</span>
        <div class="flex items-center gap-2">
          <div class="w-2 h-2 bg-[var(--color-accent)]"></div>
          <span class="text-[10px] text-[var(--color-accent)] tracking-widest font-bold">P2P Direct</span>
        </div>
      </div>

      <!-- Video grid -->
      <div class="flex-1">
        <VideoGrid
          :localStream="media.localStream.value"
          :peers="call.peers.value"
        />
      </div>

      <!-- Bottom controls -->
      <div class="absolute bottom-0 left-0 right-0 py-3.5 z-20 bg-gradient-to-t from-black/80 to-transparent"
        :class="chatOpen ? 'right-[260px]' : 'right-0'"
      >
        <CallControls
          :audioMuted="media.audioMuted.value"
          :videoMuted="media.videoMuted.value"
          :chatOpen="chatOpen"
          @toggleAudio="media.toggleAudio"
          @toggleVideo="media.toggleVideo"
          @toggleChat="chatOpen = !chatOpen"
          @endCall="handleEndCall"
        />
      </div>
    </div>

    <!-- Chat sidebar -->
    <ChatSidebar
      v-if="chatOpen"
      :messages="chat.messages.value"
      :peerId="call.peerId.value || ''"
      @send="handleSendChat"
    />
  </div>
</template>
```

- [ ] **Step 2: Verify build**

Run: `bunx vite build --config vite.config.ts`
Expected: Build succeeds

- [ ] **Step 3: Commit**

```bash
git add src/mainview/pages/CallPage.vue
git commit -m "feat(ui): call page with video grid, controls, and chat"
```

---

## Verification Checklist

After completing all tasks:

- [ ] `bunx vite build --config vite.config.ts` produces `dist/`
- [ ] `bunx vite dev --config vite.config.ts` starts dev server on http://localhost:5173
- [ ] Home page (`/`) shows NAFAQ header, New Call and Join Call panels, node ID
- [ ] Clicking "New Call" navigates to lobby (`/lobby`), shows ticket
- [ ] Lobby shows camera preview, device dropdowns, mic level bars
- [ ] When a peer connects, auto-navigates to call page (`/call`)
- [ ] Call page shows video grid (self PiP), call controls, chat sidebar
- [ ] Chat messages send and receive
- [ ] "End" button returns to home
- [ ] All transitions smooth, brutalist aesthetic consistent

## What's Next

This completes the UI layer. Remaining work for full functionality:
- **Media streaming pipeline** — WebCodecs encoding/decoding, binary frame transport to sidecar for remote audio/video
- **QR code display/scanning** — for ticket exchange (add a QR library)
- **Group call mesh** — peer announce control messages for mesh formation
- **Settings page** — persistent device preferences
