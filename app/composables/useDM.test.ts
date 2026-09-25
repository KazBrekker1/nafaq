import { beforeEach, describe, expect, it, vi } from "vitest";
import { ref } from "vue";

type Handler = (event: { payload: unknown }) => void;

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  handlers: new Map<string, Handler>(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: Handler) => {
    mocks.handlers.set(name, handler);
    return () => mocks.handlers.delete(name);
  }),
}));
const onlineStatus = ref<Record<string, boolean>>({});
vi.mock("./usePresence", () => ({
  usePresence: () => ({ onlineStatus, isOnline: () => false }),
}));

function deferred() {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<void>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

async function freshDM() {
  vi.resetModules();
  mocks.handlers.clear();
  const { useDM } = await import("./useDM");
  const dm = useDM();
  await vi.waitFor(() => expect(mocks.handlers.has("dm-ack-received")).toBe(true));
  return dm;
}

function emit(name: string, payload: unknown) {
  mocks.handlers.get(name)!({ payload });
}

describe("useDM", () => {
  beforeEach(() => {
    mocks.invoke.mockReset();
    onlineStatus.value = {};
    vi.useRealTimers();
    vi.spyOn(console, "warn").mockImplementation(() => {});
  });

  it("sends texts to one peer strictly in order", async () => {
    const sent: string[] = [];
    const first = deferred();
    mocks.invoke.mockImplementation(async (_cmd: string, args: { message: { content: string } }) => {
      if (args.message.content === "one") await first.promise;
      sent.push(args.message.content);
    });
    const dm = await freshDM();

    const a = dm.sendText("peer", "one");
    const b = dm.sendText("peer", "two");
    await Promise.resolve();
    expect(mocks.invoke).toHaveBeenCalledTimes(1);
    first.resolve();
    await Promise.all([a, b]);

    expect(sent).toEqual(["one", "two"]);
    expect(dm.conversations.value.peer!.map(m => m.type === "text" && m.status)).toEqual(["sent", "sent"]);
  });

  it("does not downgrade an early delivery ack to sent", async () => {
    const send = deferred();
    mocks.invoke.mockImplementation(() => send.promise);
    const dm = await freshDM();

    const sending = dm.sendText("peer", "hi");
    await vi.waitFor(() => expect(mocks.invoke).toHaveBeenCalled());
    const msg = dm.conversations.value.peer![0]!;
    emit("dm-ack-received", { peer_id: "peer", id: msg.type === "text" ? msg.clientId : "" });
    send.resolve();
    await sending;

    expect(msg.type === "text" && msg.status).toBe("delivered");
  });

  it("retries failed messages once when the DM connection comes back", async () => {
    mocks.invoke.mockRejectedValueOnce(new Error("unreachable"));
    const dm = await freshDM();

    await dm.sendText("peer", "hi");
    const msg = dm.conversations.value.peer![0]!;
    expect(msg.type === "text" && msg.status).toBe("failed");

    mocks.invoke.mockResolvedValue(undefined);
    emit("dm-connected", { peer_id: "peer" });
    await dm.resend("peer", msg as never);
    await vi.waitFor(() => expect(msg.type === "text" && msg.status).toBe("sent"));

    expect(mocks.invoke).toHaveBeenCalledTimes(2);
  });

  it("fails queued messages fast once a send to that peer fails", async () => {
    const firstSend = deferred();
    mocks.invoke.mockImplementationOnce(() => firstSend.promise);
    const dm = await freshDM();

    const first = dm.sendText("peer", "one");
    const second = dm.sendText("peer", "two");
    await vi.waitFor(() => expect(mocks.invoke).toHaveBeenCalledTimes(1));
    firstSend.reject(new Error("unreachable"));
    await Promise.all([first, second]);

    const statuses = dm.conversations.value.peer!.map(m => m.type === "text" && m.status);
    expect(statuses).toEqual(["failed", "failed"]);
    // Only the first message paid for a dial attempt.
    expect(mocks.invoke).toHaveBeenCalledTimes(1);
  });

  it("counts unread messages outside the open conversation", async () => {
    mocks.invoke.mockResolvedValue(undefined);
    const dm = await freshDM();

    emit("dm-received", { peer_id: "a", message: { type: "text", content: "x", timestamp: 1 } });
    emit("dm-received", { peer_id: "b", message: { type: "text", content: "y", timestamp: 2 } });
    expect(dm.totalUnread.value).toBe(2);

    dm.markRead("a");
    expect(dm.totalUnread.value).toBe(1);
  });

  it("dials a peer with failed messages when presence sees it come online", async () => {
    mocks.invoke.mockRejectedValueOnce(new Error("unreachable"));
    const dm = await freshDM();
    await dm.sendText("peer", "hi");
    vi.useFakeTimers();
    mocks.invoke.mockResolvedValue(undefined);

    onlineStatus.value = { peer: true };
    await vi.advanceTimersByTimeAsync(5000);

    expect(mocks.invoke).toHaveBeenCalledWith("connect_dm", { nodeId: "peer" });
  });
});
