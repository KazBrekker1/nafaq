import { mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import AppUpdateModal from "./AppUpdateModal.vue";
import type { UpdateStatus } from "~/composables/useAppUpdate";

type TestRef<T> = {
  readonly __v_isRef: true;
  value: T;
};

function buttonWithText(wrapper: ReturnType<typeof mount>, text: string) {
  const button = wrapper.findAll("button").find((candidate) => candidate.text() === text);
  if (!button) {
    throw new Error(`Could not find button with text: ${text}`);
  }
  return button;
}

function testRef<T>(value: T): TestRef<T> {
  return { __v_isRef: true, value };
}

const updateState = vi.hoisted(() => ({
  status: testRef("idle" as UpdateStatus),
  latestVersion: testRef<string | null>(null),
  releaseNotes: testRef<string | null>(null),
  downloadProgress: testRef(0),
  progressKnown: testRef(false),
  errorMessage: testRef<string | null>(null),
  downloadAndInstall: vi.fn(),
  checkForUpdate: vi.fn(),
}));

function readonlyTestRef<T>(source: TestRef<T>): Readonly<TestRef<T>> {
  return source;
}

vi.mock("~/composables/useAppUpdate", () => ({
  useAppUpdate: () => ({
    status: readonlyTestRef(updateState.status),
    latestVersion: readonlyTestRef(updateState.latestVersion),
    releaseNotes: readonlyTestRef(updateState.releaseNotes),
    downloadProgress: readonlyTestRef(updateState.downloadProgress),
    progressKnown: readonlyTestRef(updateState.progressKnown),
    errorMessage: readonlyTestRef(updateState.errorMessage),
    isUpdateAvailable: {
      __v_isRef: true,
      get value() {
        return updateState.status.value === "available";
      },
    },
    downloadAndInstall: updateState.downloadAndInstall,
    checkForUpdate: updateState.checkForUpdate,
    checkForUpdateOnce: updateState.checkForUpdate,
  }),
}));

describe("AppUpdateModal", () => {
  beforeEach(() => {
    updateState.status.value = "idle";
    updateState.latestVersion.value = null;
    updateState.releaseNotes.value = null;
    updateState.downloadProgress.value = 0;
    updateState.progressKnown.value = false;
    updateState.errorMessage.value = null;
    vi.clearAllMocks();
  });

  it("renders available update details and starts installation", async () => {
    updateState.status.value = "available";
    updateState.latestVersion.value = "0.9.0";
    updateState.releaseNotes.value = "Signed updater release";

    const wrapper = mount(AppUpdateModal, {
      props: { open: true },
      global: { stubs: updateModalStubs },
    });

    expect(wrapper.text()).toContain("UPDATE AVAILABLE");
    expect(wrapper.text()).toContain("Nafaq v0.9.0 is ready to install.");
    expect(wrapper.text()).toContain("Signed updater release");
    await buttonWithText(wrapper, "Update & Restart").trigger("click");
    expect(updateState.downloadAndInstall).toHaveBeenCalledOnce();
  });

  it("renders download progress while busy", async () => {
    updateState.status.value = "downloading";
    updateState.progressKnown.value = true;
    updateState.downloadProgress.value = 64;

    const wrapper = mount(AppUpdateModal, {
      props: { open: true },
      global: { stubs: updateModalStubs },
    });

    expect(wrapper.text()).toContain("Downloading");
    expect(wrapper.text()).toContain("64%");
    expect(wrapper.text()).toContain("Please wait");
  });

  it("renders errors and retries update checks", async () => {
    updateState.status.value = "error";
    updateState.errorMessage.value = "signature rejected";

    const wrapper = mount(AppUpdateModal, {
      props: { open: true },
      global: { stubs: updateModalStubs },
    });

    expect(wrapper.text()).toContain("UPDATE CHECK FAILED");
    expect(wrapper.text()).toContain("signature rejected");
    await buttonWithText(wrapper, "Retry").trigger("click");
    expect(updateState.checkForUpdate).toHaveBeenCalledOnce();
  });
});

const updateModalStubs = {
  UModal: {
    props: ["title", "description"],
    template: "<div><h1>{{ title }}</h1><p>{{ description }}</p><slot name=\"body\" /><slot name=\"footer\" /></div>",
  },
  UButton: {
    template: "<button v-bind=\"$attrs\"><slot /></button>",
  },
  UIcon: {
    template: "<span />",
  },
  UProgress: {
    template: "<div />",
  },
} as const;
