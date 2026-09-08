import assert from "node:assert/strict";
import { JSDOM } from "jsdom";
import React from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { createRoot, type Root } from "react-dom/client";
import {
  normalizeDownloadPercent,
  normalizeSetupStatus,
  normalizeLocalSummaryStatus,
  normalizeFactEntry,
  shouldBlockAppUntilSetupReady,
  shouldShowMainUiForSession,
  isSetupReadyForSession,
  isSetupActionAllowed,
  isTermsAcceptanceRequired,
  useAppState,
} from "./hooks/useAppState";
import {
  GEMMA_MODEL_ID,
  GEMMA_TERMS_MODEL_SHA256,
  GEMMA_TERMS_SOURCE,
  GEMMA_TERMS_VERSION,
  LOCAL_SUMMARY_TEST_TEXT,
  type LocalSummaryStatus,
  type SetupProgress,
  type SetupStatus,
  type MemoryPageRequest,
  type MemoryBackfillProgress,
  normalizeMemoryBackfillProgress,
  normalizeMemoryMigrationStatus,
} from "./types";
import { SetupScreen } from "./components/Common/SetupScreen";
import { SettingsModal } from "./components/Modals/SettingsModal";
import { MemoryModal } from "./components/Modals/MemoryModal";
import { dismissStartupLoader } from "./startupLoader";

(
  globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
console.warn = () => {};
Object.defineProperty(globalThis, "fetch", {
  configurable: true,
  writable: true,
  value: async () => ({ ok: true, json: async () => ({ success: true }) }),
});

const validSetup: SetupStatus = {
  ready: true,
  setup_required: false,
  running: false,
  cancelled: false,
  status: "ready",
  current_stage: "complete",
  progress: 100,
  stages: [],
  completed_stages: ["complete"],
  required_models_ready: true,
  required_models_missing: [],
  writable: true,
  elevation_required: false,
  uv_present: true,
  scripts_present: true,
  python_present: true,
  venv_present: true,
  lock_present: true,
  dependency_ready: true,
  python_import_ready: true,
  tokenizer_ready: true,
  embedding_ready: true,
  asr_websocket_ready: true,
  gemma_terms_accepted: true,
  gemma_terms_version: GEMMA_TERMS_VERSION,
  gemma_terms_model_sha256: GEMMA_TERMS_MODEL_SHA256,
  gemma_terms_source: GEMMA_TERMS_SOURCE,
};

const pendingSetup = (termsAccepted = false): SetupStatus => ({
  ...validSetup,
  ready: false,
  setup_required: true,
  status: "pending",
  current_stage: "python",
  progress: 0,
  completed_stages: [],
  required_models_ready: false,
  dependency_ready: false,
  python_import_ready: false,
  tokenizer_ready: false,
  embedding_ready: false,
  asr_websocket_ready: false,
  gemma_terms_accepted: termsAccepted,
  gemma_terms_version: termsAccepted ? GEMMA_TERMS_VERSION : "",
  gemma_terms_model_sha256: termsAccepted ? GEMMA_TERMS_MODEL_SHA256 : "",
  gemma_terms_source: termsAccepted ? GEMMA_TERMS_SOURCE : "",
});

assert.equal(normalizeDownloadPercent(1), 1);
assert.equal(normalizeDownloadPercent(42.5), 42.5);
assert.equal(normalizeDownloadPercent(-10), 0);
assert.equal(normalizeDownloadPercent(101), 100);

const migrationStatus = normalizeMemoryMigrationStatus({
  status: "running",
  processed: 12.9,
  total: 10,
  message: "migrating",
  error: null,
});
assert.equal(migrationStatus?.processed, 10);
assert.equal(migrationStatus?.total, 10);
assert.equal(normalizeMemoryMigrationStatus({ status: "unknown" }), null);

assert.equal(shouldBlockAppUntilSetupReady(true, null), true);
assert.equal(
  shouldBlockAppUntilSetupReady(true, { ...validSetup, ready: false }),
  true,
);
assert.equal(shouldBlockAppUntilSetupReady(true, validSetup), false);
assert.equal(
  shouldBlockAppUntilSetupReady(true, {
    ...validSetup,
    required_models_ready: false,
  }),
  true,
);
assert.equal(
  isSetupReadyForSession({ ...validSetup, required_models_ready: undefined }),
  false,
);
assert.equal(
  shouldBlockAppUntilSetupReady(true, {
    ...validSetup,
    gemma_terms_source: "https://example.invalid/terms",
  }),
  true,
);
// Runtime installation metadata alone must not authorize a session. Every
// startup validation layer is fail-closed until its concrete probe succeeds.
for (const layer of [
  "dependency_ready",
  "tokenizer_ready",
  "embedding_ready",
  "asr_websocket_ready",
] as const) {
  assert.equal(
    isSetupReadyForSession({ ...validSetup, [layer]: false }),
    false,
    `${layer} failure must block session startup`,
  );
}
assert.equal(shouldBlockAppUntilSetupReady(false, null), true);
assert.equal(shouldShowMainUiForSession(true, null), false);
assert.equal(shouldShowMainUiForSession(true, validSetup), true);
assert.equal(shouldShowMainUiForSession(false, null), true);
assert.equal(
  isTermsAcceptanceRequired({
    ...pendingSetup(false),
    status: "error",
    error:
      "Gemma Terms acknowledgement is required before setup can download the required model",
  }),
  true,
);
assert.equal(
  isTermsAcceptanceRequired({
    ...pendingSetup(false),
    status: "error",
    error: "uv failed unexpectedly",
  }),
  false,
);

// The static index.html loader must be dismissed even when App renders the
// first-run SetupScreen (which does not mount LoadingScreen).
{
  const dom = new JSDOM(
    '<!doctype html><div id="app-startup-loader"></div>',
  );
  const loader = dom.window.document.getElementById("app-startup-loader");
  dismissStartupLoader(dom.window.document);
  assert.equal(loader?.classList.contains("loaded"), true);
  await new Promise<void>((resolve) => setTimeout(resolve, 350));
  assert.equal(dom.window.document.getElementById("app-startup-loader"), null);
}

assert.equal(
  isSetupActionAllowed({ ...pendingSetup(true), error: "   " }),
  true,
);

// The setup command has a stable serialized shape. A random object must not
// be interpreted as a pending terms state or as a successful setup snapshot.
assert.equal(normalizeSetupStatus({}), null);
assert.equal(normalizeSetupStatus({ ready: false, progress: 1 }), null);
assert.equal(normalizeSetupStatus({ ...validSetup, progress: 1 })?.progress, 1);
assert.equal(
  normalizeSetupStatus({ ...validSetup, required_models_ready: false })
    ?.required_models_ready,
  false,
);
for (const runtimeStatus of [
  "ready",
  "pending",
  "running",
  "error",
  "cancelled",
] as const) {
  assert.equal(
    normalizeSetupStatus({ ...validSetup, status: runtimeStatus })?.status,
    runtimeStatus,
  );
}
assert.equal(normalizeSetupStatus({ ...validSetup, status: "unknown" }), null);

// The local summary status is advisory UI state: unknown states are rejected,
// scalars are clamped, and empty/absent messages collapse to null.
assert.equal(normalizeLocalSummaryStatus(null), null);
assert.equal(normalizeLocalSummaryStatus("ready"), null);
assert.equal(normalizeLocalSummaryStatus({}), null);
assert.equal(normalizeLocalSummaryStatus({ state: "destroyed" }), null);
for (const state of [
  "disabled",
  "model_missing",
  "starting",
  "ready",
  "busy",
  "error",
] as const) {
  assert.equal(
    normalizeLocalSummaryStatus({
      state,
      queueDepth: 2.7,
      fallbackActive: true,
      message: "m",
    })?.state,
    state,
  );
}
const clampedSummary = normalizeLocalSummaryStatus({
  state: "busy",
  queueDepth: -5,
  fallbackActive: "yes",
  message: "   ",
});
assert.equal(clampedSummary?.queueDepth, 0);
assert.equal(clampedSummary?.fallbackActive, false);
assert.equal(clampedSummary?.message, null);

const liveFact = normalizeFactEntry({
  fact_id: "fact:self:summary-1",
  source_event_id: "event-1",
  summary: "ユーザーは猫が好きです。",
  timestamp: "12:34:56",
  source: "User",
});
assert.deepEqual(liveFact, {
  id: "fact:self:summary-1",
  text: "ユーザーは猫が好きです。",
  timestamp: "12:34:56",
  source: "User",
  sourceEventId: "event-1",
});
assert.equal(normalizeFactEntry({ fact_id: "fact-only" }), null);

const additiveProgress = normalizeMemoryBackfillProgress({
  state: "error",
  processed: 3,
  total: 5,
  queued: 5,
  skipped: 0,
  failed: 1,
  persisted: 2,
  remaining: 2,
  reasonCounts: { invalid_model_output: 1 },
  lastErrorReason: "invalid_model_output",
  reason: "invalid_model_output",
  attempt_id: "attempt-2",
  retry_count: 1,
  final_counts: {
    processed: 3,
    persisted: 2,
    skipped: 0,
    failed: 1,
    remaining: 2,
  },
  fatalError: { code: "journal_unavailable", message: "journal unavailable" },
});
assert.ok(additiveProgress, "summary progress payload should be readable");
assert.equal(
  (additiveProgress as unknown as Record<string, unknown>).remaining,
  2,
);
assert.deepEqual(
  (additiveProgress as unknown as Record<string, unknown>).reasonCounts,
  { invalid_model_output: 1 },
);
assert.equal(
  (
    (additiveProgress as unknown as Record<string, unknown>)
      .fatalError as Record<string, unknown>
  ).code,
  "journal_unavailable",
);
assert.equal(
  (additiveProgress as unknown as Record<string, unknown>).attempt_id,
  "attempt-2",
);
assert.equal(
  (additiveProgress as unknown as Record<string, unknown>).retry_count,
  1,
);
assert.deepEqual(
  (additiveProgress as unknown as Record<string, unknown>).final_counts,
  { processed: 3, persisted: 2, skipped: 0, failed: 1, remaining: 2 },
);
const oldProgress = normalizeMemoryBackfillProgress({
  state: "running",
  processed: 1,
  total: 3,
});
assert.equal(oldProgress?.remaining, 2);
assert.deepEqual(oldProgress?.reasonCounts, {});
assert.equal(normalizeMemoryBackfillProgress(null), null);

type EventCallback = (event: { id: number; payload: unknown }) => void;

class TauriHarness {
  readonly callbacks = new Map<number, EventCallback>();
  readonly registrations: string[] = [];
  readonly unregistered: string[] = [];
  readonly invocations: Array<{ command: string; args: unknown }> = [];
  readonly successfulRegistrations: Array<{
    event: string;
    eventId: number;
    callbackId: number;
  }> = [];
  readonly unregisteredListeners: Array<{ event: string; eventId: number }> =
    [];
  private callbackId = 0;
  private eventId = 0;
  status: unknown = pendingSetup(true);
  settings: Record<string, unknown> = { gemma_terms_accepted: false };
  runSetupResult: unknown = pendingSetup(true);
  runSetupGate: Promise<unknown> | null = null;
  saveSettingGate: Promise<unknown> | null = null;
  failRegistrationAt: number | null = null;
  failRegistrationEvents = new Set<string>();
  failSaveSettingKeys = new Set<string>();
  modelsStatus: unknown[] = [];
  failGetSetupStatus = false;
  syncStatusOnSave = true;
  localSummaryStatus: unknown = null;
  failGetLocalSummaryStatus = false;
  testSummaryResult: unknown = null;
  testSummaryArgs: unknown = null;
  unloadSummaryCalls = 0;
  malformedSummaryResponse = false;
  summaryReason = "inference_failed";
  summaryStatus = "fallback";
  failSummaryRead = false;
  summaryRetryResult: unknown = {
    attempt_id: "attempt-2",
    event_id: "event-1",
    status: "pending",
    receipt: {
      operation_id: "retry-op",
      journal_sequence: 2,
      committed_at: "2026-09-04T00:00:00Z",
      undo_token: null,
      undo_expires_at: null,
    },
  };
  memoryItems: unknown[] = [];
  backfillResult: unknown = {
    accepted: true,
    progress: {
      state: "running",
      processed: 0,
      total: 1,
      queued: 1,
      skipped: 0,
      failed: 0,
      persisted: 0,
      excluded: 0,
      attempted: 0,
      remaining: 1,
      reasonCounts: {},
      lastErrorReason: null,
      fatalError: null,
      message: "queued",
      error: null,
    } satisfies MemoryBackfillProgress,
  };

  install(dom: JSDOM): void {
    const internals = {
      transformCallback: (callback: EventCallback) => {
        const id = ++this.callbackId;
        this.callbacks.set(id, callback);
        return id;
      },
      unregisterCallback: (id: number) => this.callbacks.delete(id),
      invoke: (command: string, args?: unknown) => this.invoke(command, args),
    };
    (
      dom.window as unknown as { __TAURI_INTERNALS__: unknown }
    ).__TAURI_INTERNALS__ = internals;
    (
      dom.window as unknown as { __TAURI_EVENT_PLUGIN_INTERNALS__: unknown }
    ).__TAURI_EVENT_PLUGIN_INTERNALS__ = {
      unregisterListener: () => {},
    };
    Object.defineProperty(globalThis, "window", {
      configurable: true,
      writable: true,
      value: dom.window,
    });
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      writable: true,
      value: dom.window.document,
    });
  }

  async invoke(command: string, args?: unknown): Promise<unknown> {
    this.invocations.push({ command, args });
    if (command === "plugin:event|listen") {
      const event = (args as { event: string }).event;
      this.registrations.push(event);
      if (
        this.failRegistrationEvents.has(event) ||
        (this.failRegistrationAt !== null &&
          this.registrations.length === this.failRegistrationAt)
      ) {
        throw new Error(`registration failed: ${event}`);
      }
      const eventId = ++this.eventId;
      this.successfulRegistrations.push({
        event,
        eventId,
        callbackId: (args as { handler: number }).handler,
      });
      return eventId;
    }
    if (command === "plugin:event|unlisten") {
      this.unregistered.push(command);
      const listener = args as { event: string; eventId: number };
      this.unregisteredListeners.push(listener);
      return undefined;
    }
    if (command === "get_setup_status") {
      if (this.failGetSetupStatus) throw new Error("setup status unavailable");
      return this.status;
    }
    if (command === "run_setup")
      return this.runSetupGate || this.runSetupResult;
    if (command === "load_settings") return this.settings;
    if (command === "get_models_status") return this.modelsStatus;
    if (command === "get_local_summary_status") {
      if (this.failGetLocalSummaryStatus)
        throw new Error("local summary status unavailable");
      return this.localSummaryStatus;
    }
    if (command === "unload_local_summary_model") {
      this.unloadSummaryCalls += 1;
      return undefined;
    }
    if (command === "test_local_summary") {
      this.testSummaryArgs = args;
      if (
        typeof this.testSummaryResult !== "object" ||
        this.testSummaryResult === null ||
        (this.testSummaryResult as { reject?: boolean }).reject === true
      ) {
        throw new Error("local summary test failed");
      }
      return this.testSummaryResult;
    }
    if (command === "list_audio_devices")
      return {
        input_devices: ["Test microphone"],
        default_device: "Test microphone",
      };
    if (command === "list_windows") return ["Test window"];
    if (command === "capture_window_preview")
      return "data:image/png;base64,test";
    if (command === "get_prompts") return [];
    if (command === "read_logs") return [];
    if (command === "twitch_get_status") return { connected: false };
    if (command === "list_lance_memories")
      return { success: true, memories: this.memoryItems };
    if (command === "memory_manager_process_all") return this.backfillResult;
    if (command === "memory_manager_retry_summary") return this.summaryRetryResult;
    if (command === "memory_manager_list_facts")
      return {
        rows: [
          {
            fact_id: "fact-1",
            subject: "self",
            predicate: "likes",
            key: "food",
            value: "curry",
            status: "auto",
            evidence_count: 1,
            latest_evidence_at: "2026-09-04T00:00:00Z",
            source_event_ids: ["event-1"],
            revision: 1,
            operation_id: "op-1",
          },
        ],
        page: {
          next_cursor: "offset:50",
          has_more: true,
          total: 51,
          snapshot_sequence: 1,
        },
      };
    if (command === "memory_manager_list_summaries" && this.failSummaryRead)
      throw new Error("summary read failed");
    if (command === "memory_manager_list_summaries" && this.malformedSummaryResponse)
      return {
        rows: [{}],
        page: {
          next_cursor: null,
          has_more: false,
          total: 1,
          snapshot_sequence: 1,
        },
      };
    if (command === "memory_manager_list_summaries")
      return {
        rows: [
          {
            summary_id: "summary-1",
            event_id: "event-1",
            summary: null,
            status: this.summaryStatus,
            error: "model unavailable",
            reason: this.summaryReason,
            model_id: null,
            prompt_version: null,
            attempt_id: "attempt-1",
            vector_source: "document",
            derived_fact_id: null,
            occurred_at: "2026-09-04T00:00:00Z",
            source: "test",
            event_type: "user_speech",
          },
        ],
        page: {
          next_cursor: null,
          has_more: false,
          total: 1,
          snapshot_sequence: 1,
        },
      };
    if (command === "memory_manager_get_fact_evidence")
      return {
        fact: {
          fact_id: "fact-1",
          subject: "self",
          predicate: "likes",
          key: "food",
          value: "curry",
          status: "auto",
          evidence_count: 1,
          latest_evidence_at: "2026-09-04T00:00:00Z",
          source_event_ids: ["event-1"],
          revision: 1,
          operation_id: "op-1",
        },
        evidence: [],
        page: { next_cursor: null, has_more: false, total: 0, snapshot_sequence: 1 },
      };
    if (command === "memory_manager_get_raw_event")
      return {
        event: {
          event_id: "event-1",
          legacy_id: null,
          subject: "self",
          event_type: "user_speech",
          source: "test",
          occurred_at: "2026-09-04T00:00:00Z",
          content_preview: "raw preview",
          content: "complete raw evidence",
          summary_status: "fallback",
          derived_fact_ids: [],
          vector_source: "document",
        },
        facts: [],
        summary: null,
      };
    if (command === "memory_manager_delete_facts")
      return {
        changed: true,
        items: [],
        receipt: {
          operation_id: "delete-op",
          journal_sequence: 2,
          committed_at: "2026-09-04T00:00:00Z",
          undo_token: "undo-1",
          undo_expires_at: "2026-09-04T00:10:00Z",
        },
      };
    if (command === "save_setting") {
      if (this.saveSettingGate) await this.saveSettingGate;
      const setting = args as { key: string; value: unknown };
      if (this.failSaveSettingKeys.has(setting.key))
        throw new Error(`save failed: ${setting.key}`);
      this.settings[setting.key] = setting.value;
      if (
        this.syncStatusOnSave &&
        this.status !== null &&
        typeof this.status === "object" &&
        setting.key in this.status
      ) {
        this.status = {
          ...(this.status as Record<string, unknown>),
          [setting.key]: setting.value,
        };
        if (setting.key === "gemma_terms_accepted" && setting.value === true) {
          this.status = {
            ...(this.status as Record<string, unknown>),
            error: null,
          };
        }
      }
      return this.settings;
    }
    if (command === "session_start" || command === "cancel_setup")
      return undefined;
    return undefined;
  }

  emit(eventName: string, payload: unknown): void {
    assert.ok(
      this.registrations.length > 0,
      "event listener should be registered before emitting",
    );
    const registration = this.successfulRegistrations.find(
      ({ event }) => event === eventName,
    );
    const callback = registration
      ? this.callbacks.get(registration.callbackId)
      : undefined;
    callback?.({ id: this.eventId, payload });
  }
}

const flush = async (): Promise<void> => {
  await act(async () => {
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
  });
};

const settleEffects = async (): Promise<void> => {
  await flush();
  await new Promise<void>((resolve) => setTimeout(resolve, 30));
  await flush();
};

interface HookSnapshot {
  current: ReturnType<typeof useAppState> | null;
}

const mountHook = async (
  harness: unknown,
): Promise<{ renderer: ReactTestRenderer; snapshot: HookSnapshot }> => {
  void harness;
  const snapshot: HookSnapshot = { current: null };
  const Probe = () => {
    snapshot.current = useAppState();
    return null;
  };
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(React.createElement(Probe));
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
  });
  await settleEffects();
  assert.ok(snapshot.current, "hook should expose a snapshot");
  return { renderer, snapshot };
};

const makeHarness = (): { dom: JSDOM; harness: TauriHarness } => {
  const dom = new JSDOM("<!doctype html><html><body></body></html>");
  const harness = new TauriHarness();
  harness.install(dom);
  return { dom, harness };
};

const makeBrowserHarness = (
  status: unknown,
): { dom: JSDOM; posts: string[]; restore: () => void } => {
  const dom = new JSDOM("<!doctype html><html><body></body></html>");
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: dom.window,
  });
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    writable: true,
    value: dom.window.document,
  });
  const previousWebSocket = (globalThis as { WebSocket?: unknown }).WebSocket;
  Object.defineProperty(globalThis, "WebSocket", {
    configurable: true,
    writable: true,
    value: class BrowserTestWebSocket {
      readyState = 0;
      close(): void {
        this.readyState = 3;
      }
    },
  });
  const posts: string[] = [];
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    writable: true,
    value: async (input: string, init?: RequestInit) => {
      const url = String(input);
      if (url.endsWith("/api/setup/status"))
        return { ok: true, json: async () => status };
      if (init?.method === "POST") posts.push(url);
      return { ok: true, json: async () => ({ success: true }) };
    },
  });
  return {
    dom,
    posts,
    restore: () => {
      Object.defineProperty(globalThis, "WebSocket", {
        configurable: true,
        writable: true,
        value: previousWebSocket,
      });
    },
  };
};

const model = (overrides: Partial<Record<string, unknown>> = {}) => ({
  id: GEMMA_MODEL_ID,
  name: "Gemma",
  description: "Gemma model",
  hf_repo: "google/gemma",
  category: "LLM",
  required: true,
  estimated_size_bytes: 100,
  is_installed: false,
  actual_size_bytes: 0,
  local_path: "",
  ...overrides,
});

const nodeText = (node: any): string => {
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(nodeText).join("");
  return node?.props ? nodeText(node.props.children) : "";
};

const restoreDefaultFetch = (): void => {
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    writable: true,
    value: async () => ({ ok: true, json: async () => ({ success: true }) }),
  });
};

const domForConfirm = (harness: TauriHarness): JSDOM => {
  const dom = new JSDOM("<!doctype html><html><body></body></html>");
  harness.install(dom);
  dom.window.confirm = () => true;
  return dom;
};

// Render the SettingsModal preferences tab into a real DOM so assertions run
// against rendered text and live buttons (not test-renderer internals).
const renderPreferencesModal = async (
  harness: TauriHarness,
  settings: Record<string, unknown> = { memory_summary_mode: "auto" },
) => {
  const dom = domForConfirm(harness);
  const host = dom.window.document.createElement("div");
  dom.window.document.body.appendChild(host);
  const root: Root = createRoot(host);
  await act(async () => {
    root.render(
      React.createElement(SettingsModal, {
        isOpen: true,
        onClose: () => {},
        settings,
        onUpdateSetting: async () => {},
        discordDevices: [],
        initialTab: "preferences" as const,
      }),
    );
    await new Promise<void>((resolve) => setTimeout(resolve, 20));
  });
  return { dom, host, root };
};

const findButton = (
  host: HTMLElement,
  label: string,
): HTMLButtonElement | undefined =>
  Array.from(host.querySelectorAll("button")).find((button) =>
    (button.textContent || "").includes(label),
  );

// Session startup is fail-closed for every transport and every malformed or
// incomplete setup state. Only a canonical, fully ready state may issue one
// transport call.
{
  const { harness } = makeHarness();
  harness.status = { ...validSetup, status: "unknown" };
  const { renderer, snapshot } = await mountHook(harness);
  await snapshot.current!.startSession();
  await snapshot.current!.runSetup();
  assert.equal(
    harness.invocations.filter(({ command }) => command === "session_start")
      .length,
    0,
  );
  assert.equal(
    harness.invocations.filter(({ command }) => command === "run_setup").length,
    0,
  );
  renderer.unmount();
  await settleEffects();
}

{
  const blockedStatuses: unknown[] = [
    null,
    { ready: false },
    { ...validSetup, ready: false, setup_required: true },
    { ...validSetup, required_models_ready: false },
    { ...validSetup, gemma_terms_accepted: false },
    { ...validSetup, gemma_terms_source: "https://example.invalid/terms" },
  ];
  for (const blocked of blockedStatuses) {
    const { harness } = makeHarness();
    harness.status = blocked;
    const { renderer, snapshot } = await mountHook(harness);
    await snapshot.current!.startSession();
    assert.equal(
      harness.invocations.filter(({ command }) => command === "session_start")
        .length,
      0,
    );
    renderer.unmount();
    await settleEffects();
  }
  const { harness } = makeHarness();
  harness.failGetSetupStatus = true;
  const { renderer, snapshot } = await mountHook(harness);
  await snapshot.current!.startSession();
  assert.equal(
    harness.invocations.filter(({ command }) => command === "session_start")
      .length,
    0,
  );
  renderer.unmount();
  await settleEffects();
}

{
  const { harness } = makeHarness();
  harness.status = validSetup;
  const { renderer, snapshot } = await mountHook(harness);
  await snapshot.current!.startSession();
  assert.equal(
    harness.invocations.filter(({ command }) => command === "session_start")
      .length,
    1,
  );
  renderer.unmount();
  await settleEffects();
}

// The native readiness event is the only asynchronous transition that may
// authorize the WebSocket layer after the initial setup snapshot.
{
  const { harness } = makeHarness();
  harness.status = { ...validSetup, asr_websocket_ready: false };
  const { renderer, snapshot } = await mountHook(harness);
  assert.equal(snapshot.current!.setupStatus?.asr_websocket_ready, false);
  harness.emit("asr_ready", { ready: true });
  await flush();
  assert.equal(snapshot.current!.setupStatus?.asr_websocket_ready, true);
  renderer.unmount();
  await settleEffects();
}

// A WebSocket readiness event must not promote an otherwise failed runtime to
// ready.  `asr_ready` authorizes only the ASR layer; dependency/model failures
// remain visible until their own setup retry succeeds.
{
  const { harness } = makeHarness();
  harness.status = {
    ...validSetup,
    ready: false,
    setup_required: true,
    status: "error",
    error: "Tokenizer initialization failed",
    tokenizer_ready: false,
    asr_websocket_ready: false,
  };
  const { renderer, snapshot } = await mountHook(harness);
  harness.emit("asr_ready", { ready: true });
  await flush();
  assert.equal(snapshot.current!.setupStatus?.ready, false);
  assert.equal(snapshot.current!.setupStatus?.setup_required, true);
  assert.equal(snapshot.current!.setupStatus?.tokenizer_ready, false);
  assert.equal(snapshot.current!.setupError, "Tokenizer initialization failed");
  renderer.unmount();
  await settleEffects();
}

// A failed startup warmup must remain visible while status polling observes
// the otherwise-healthy runtime. The next retry/`asr_ready` event is the only
// transition allowed to clear the WebSocket failure.
{
  const { harness } = makeHarness();
  harness.status = { ...validSetup, asr_websocket_ready: false };
  const { renderer, snapshot } = await mountHook(harness);
  const warmupError = "ASR WebSocket connection failed";
  harness.emit("asr_warmup_failed", { message: warmupError });
  await flush();
  assert.equal(snapshot.current!.setupError, warmupError);
  await snapshot.current!.fetchSetupStatus();
  assert.equal(snapshot.current!.setupError, warmupError);
  renderer.unmount();
  await settleEffects();
}

// Native ASR latency is retained alongside the finalized transcript so the
// first-utterance startup and inference timings remain visible to the user.
{
  const { harness } = makeHarness();
  const { renderer, snapshot } = await mountHook(harness);
  harness.emit("asr_result", {
    text: "ねえぐり、テスト",
    is_final: true,
    stream: "mic",
    is_prompt: true,
    latency_ms: 123.4,
  });
  await flush();
  assert.equal(snapshot.current!.asrHistory.length, 1);
  assert.equal(snapshot.current!.asrHistory[0].latencyMs, 123.4);
  renderer.unmount();
  await settleEffects();
}

// A durable Fact emitted by the live curation path appears in the conversation
// stream and duplicate deliveries remain idempotent.
{
  const { harness } = makeHarness();
  const { renderer, snapshot } = await mountHook(harness);
  harness.emit("memory-fact-created", {
    fact_id: "fact:self:summary-1",
    source_event_id: "event-1",
    summary: "ユーザーは猫が好きです。",
    timestamp: "12:34:56",
    source: "User",
  });
  await flush();
  assert.equal(snapshot.current!.factHistory.length, 1);
  assert.equal(snapshot.current!.factHistory[0].text, "ユーザーは猫が好きです。");
  harness.emit("memory-fact-created", {
    fact_id: "fact:self:summary-1",
    source_event_id: "event-1",
    summary: "ユーザーは猫が好きです。",
    timestamp: "12:34:56",
    source: "User",
  });
  await flush();
  assert.equal(snapshot.current!.factHistory.length, 1);
  renderer.unmount();
  await settleEffects();
}

// Settings must reject a boolean-only acknowledgement and must not download
// Gemma when the refreshed canonical status still has mismatched metadata.
{
  const { harness } = makeHarness();
  harness.modelsStatus = [model()];
  const settings: Record<string, unknown> = {
    gemma_terms_accepted: true,
    gemma_terms_version: "old",
  };
  const updates: string[] = [];
  const props = {
    isOpen: true,
    onClose: () => {},
    settings,
    onUpdateSetting: async (key: string, value: unknown) => {
      updates.push(key);
      settings[key] = value;
    },
    discordDevices: [],
    initialTab: "models" as const,
    setupStatus: {
      ...pendingSetup(),
      gemma_terms_accepted: true,
      gemma_terms_version: "old",
    },
    onRefreshSetupStatus: async () => ({
      ...pendingSetup(),
      gemma_terms_accepted: true,
      gemma_terms_version: "old",
    }),
  };
  harness.install(domForConfirm(harness));
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(React.createElement(SettingsModal, props as any));
    await new Promise<void>((resolve) => setTimeout(resolve, 20));
  });
  const button = renderer.root
    .findAllByType("button")
    .find((candidate) => nodeText(candidate).includes("Download"));
  assert.ok(button, "Gemma download button should render");
  await act(async () => {
    await button!.props.onClick();
  });
  assert.deepEqual(updates.slice(-4), [
    "gemma_terms_accepted",
    "gemma_terms_version",
    "gemma_terms_model_sha256",
    "gemma_terms_source",
  ]);
  assert.equal(
    harness.invocations.filter(({ command }) => command === "download_model")
      .length,
    0,
  );
  renderer.unmount();
  await settleEffects();
}

// The backend uses only the portable EXE-relative models root, so Settings
// exposes that location as read-only instead of offering an ignored path edit.
{
  const { harness } = makeHarness();
  harness.modelsStatus = [model()];
  const dom = domForConfirm(harness);
  const host = dom.window.document.createElement("div");
  dom.window.document.body.appendChild(host);
  const root = createRoot(host);
  await act(async () => {
    root.render(
      React.createElement(SettingsModal, {
        isOpen: true,
        onClose: () => {},
        settings: {},
        onUpdateSetting: async () => {},
        discordDevices: [],
        initialTab: "models" as const,
      }),
    );
    await new Promise<void>((resolve) => setTimeout(resolve, 20));
  });
  assert.equal(host.querySelector("input"), null);
  assert.equal(findButton(host, "Set Path"), undefined);
  assert.equal(findButton(host, "Reset"), undefined);
  assert.deepEqual(
    harness.invocations.find(({ command }) => command === "get_models_status")
      ?.args,
    { customDir: null },
  );
  assert.match(host.textContent || "", /EXE 隣の models/);
  await act(async () => {
    root.unmount();
  });
  await settleEffects();
}

{
  const { harness } = makeHarness();
  harness.modelsStatus = [model()];
  const settings = {
    gemma_terms_accepted: true,
    gemma_terms_version: GEMMA_TERMS_VERSION,
    gemma_terms_model_sha256: GEMMA_TERMS_MODEL_SHA256,
    gemma_terms_source: GEMMA_TERMS_SOURCE,
  };
  const props = {
    isOpen: true,
    onClose: () => {},
    settings,
    onUpdateSetting: async () => {},
    discordDevices: [],
    initialTab: "models" as const,
    setupStatus: null,
    onRefreshSetupStatus: async () => null,
  };
  harness.install(domForConfirm(harness));
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(React.createElement(SettingsModal, props as any));
    await new Promise<void>((resolve) => setTimeout(resolve, 20));
  });
  const button = renderer.root
    .findAllByType("button")
    .find((candidate) => nodeText(candidate).includes("Download"));
  assert.ok(button);
  await act(async () => {
    await button!.props.onClick();
  });
  assert.equal(
    harness.invocations.filter(({ command }) => command === "download_model")
      .length,
    0,
  );
  renderer.unmount();
  await settleEffects();
}

{
  const { harness } = makeHarness();
  harness.modelsStatus = [model()];
  const settings = {
    gemma_terms_accepted: true,
    gemma_terms_version: GEMMA_TERMS_VERSION,
    gemma_terms_model_sha256: GEMMA_TERMS_MODEL_SHA256,
    gemma_terms_source: GEMMA_TERMS_SOURCE,
  };
  const props = {
    isOpen: true,
    onClose: () => {},
    settings,
    onUpdateSetting: async () => {},
    discordDevices: [],
    initialTab: "models" as const,
    setupStatus: pendingSetup(),
    onRefreshSetupStatus: async () => ({
      ...pendingSetup(),
      gemma_terms_accepted: true,
      gemma_terms_version: GEMMA_TERMS_VERSION,
      gemma_terms_model_sha256: GEMMA_TERMS_MODEL_SHA256,
      gemma_terms_source: GEMMA_TERMS_SOURCE,
    }),
  };
  harness.install(domForConfirm(harness));
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(React.createElement(SettingsModal, props as any));
    await new Promise<void>((resolve) => setTimeout(resolve, 20));
  });
  const button = renderer.root
    .findAllByType("button")
    .find((candidate) => nodeText(candidate).includes("Download"));
  assert.ok(button);
  await act(async () => {
    await button!.props.onClick();
  });
  assert.equal(
    harness.invocations.filter(({ command }) => command === "download_model")
      .length,
    1,
  );
  renderer.unmount();
  await settleEffects();
}

// The HTTP transport consumes the same canonical status contract; there is no
// browser/null bypass, and a valid status authorizes exactly one POST.
{
  const { posts, restore } = makeBrowserHarness(validSetup);
  const { renderer, snapshot } = await mountHook({});
  await snapshot.current!.startSession();
  assert.equal(
    posts.filter((url) => url.endsWith("/api/session/start")).length,
    1,
  );
  renderer.unmount();
  await settleEffects();
  restoreDefaultFetch();
  restore();
}

{
  const blockedStatuses: unknown[] = [
    null,
    { ready: false },
    { ...validSetup, required_models_ready: false },
    { ...validSetup, gemma_terms_version: "old" },
  ];
  for (const blocked of blockedStatuses) {
    const { posts, restore } = makeBrowserHarness(blocked);
    const { renderer, snapshot } = await mountHook({});
    await snapshot.current!.startSession();
    assert.equal(
      posts.filter((url) => url.endsWith("/api/session/start")).length,
      0,
    );
    renderer.unmount();
    await settleEffects();
    restore();
  }
  restoreDefaultFetch();
}

// Terms acceptance requires exact metadata from both persisted settings and
// the refreshed canonical RuntimeStatus before run_setup is invoked.
{
  const { harness } = makeHarness();
  harness.syncStatusOnSave = false;
  harness.status = { ...pendingSetup(), gemma_terms_accepted: false };
  const { renderer, snapshot } = await mountHook(harness);
  await snapshot.current!.acceptTermsAndRunSetup();
  assert.equal(
    harness.invocations.filter(({ command }) => command === "run_setup").length,
    0,
  );
  renderer.unmount();
  await settleEffects();
}

{
  const { harness } = makeHarness();
  harness.syncStatusOnSave = false;
  harness.status = {
    ...pendingSetup(),
    gemma_terms_accepted: true,
    gemma_terms_version: "wrong",
  };
  const { renderer, snapshot } = await mountHook(harness);
  await snapshot.current!.acceptTermsAndRunSetup();
  assert.equal(
    harness.invocations.filter(({ command }) => command === "run_setup").length,
    0,
  );
  renderer.unmount();
  await settleEffects();
}

{
  const { harness } = makeHarness();
  harness.status = pendingSetup();
  harness.failSaveSettingKeys.add("gemma_terms_version");
  const { renderer, snapshot } = await mountHook(harness);
  await snapshot.current!.acceptTermsAndRunSetup();
  assert.equal(
    harness.invocations.filter(({ command }) => command === "run_setup").length,
    0,
  );
  renderer.unmount();
  await settleEffects();
}

// Cancellation must happen while run_setup is still pending and suppress both
// the rejected backend promise and any failure state it would otherwise set.
{
  const { harness } = makeHarness();
  let rejectRun!: (error: unknown) => void;
  harness.runSetupGate = new Promise((_resolve, reject) => {
    rejectRun = reject;
  });
  const { renderer, snapshot } = await mountHook(harness);
  const run = snapshot.current!.runSetup();
  await settleEffects();
  assert.equal(
    harness.invocations.filter(({ command }) => command === "run_setup").length,
    1,
  );
  assert.equal(snapshot.current!.isSetupRunning, true);
  await snapshot.current!.cancelSetup();
  rejectRun(new Error("late run_setup failure"));
  await run;
  await flush();
  assert.equal(snapshot.current!.isSetupRunning, false);
  assert.equal(snapshot.current!.setupError, null);
  assert.equal(snapshot.current!.setupStatus?.cancelled, true);
  assert.equal(snapshot.current!.setupStatus?.error, null);
  renderer.unmount();
  await settleEffects();
}

// Behavioral single-flight: two callers share one backend run.
{
  const { harness } = makeHarness();
  let release!: (value: unknown) => void;
  harness.runSetupGate = new Promise((resolve) => {
    release = resolve;
  });
  const { renderer, snapshot } = await mountHook(harness);
  const first = snapshot.current!.runSetup();
  const second = snapshot.current!.runSetup();
  await settleEffects();
  assert.equal(
    harness.invocations.filter(({ command }) => command === "run_setup").length,
    1,
  );
  release(validSetup);
  await Promise.all([first, second]);
  renderer.unmount();
  await settleEffects();
}

// A canonical failed setup snapshot is retryable: the prior error must not
// prevent exactly one new run_setup attempt.
{
  const { harness } = makeHarness();
  harness.status = { ...pendingSetup(true), error: "previous setup failure" };
  let release!: (value: unknown) => void;
  harness.runSetupGate = new Promise((resolve) => {
    release = resolve;
  });
  const { renderer, snapshot } = await mountHook(harness);
  assert.equal(snapshot.current!.setupError, "previous setup failure");
  const retry = snapshot.current!.runSetup();
  await settleEffects();
  assert.equal(
    harness.invocations.filter(({ command }) => command === "run_setup").length,
    1,
  );
  assert.equal(snapshot.current!.setupError, null);
  release(pendingSetup(true));
  await retry;
  renderer.unmount();
  await settleEffects();
}

// Accepting Gemma terms persists the exact metadata consumed by the backend;
// an acknowledgement without version/hash/source would remain unusable.
{
  const { harness } = makeHarness();
  const { renderer, snapshot } = await mountHook(harness);
  await snapshot.current!.acceptTermsAndRunSetup();
  const savedKeys = harness.invocations
    .filter(({ command }) => command === "save_setting")
    .map(({ args }) => (args as { key: string }).key);
  assert.deepEqual(savedKeys.slice(-4), [
    "gemma_terms_accepted",
    "gemma_terms_version",
    "gemma_terms_model_sha256",
    "gemma_terms_source",
  ]);
  assert.equal(harness.settings.gemma_terms_version, GEMMA_TERMS_VERSION);
  assert.equal(
    harness.settings.gemma_terms_model_sha256,
    GEMMA_TERMS_MODEL_SHA256,
  );
  assert.equal(harness.settings.gemma_terms_source, GEMMA_TERMS_SOURCE);
  renderer.unmount();
  await settleEffects();
}

// A successful local Tauri save remains authoritative when the legacy Python
// settings endpoint is unavailable during first-run Terms acceptance.
{
  const { harness } = makeHarness();
  const previousFetch = globalThis.fetch;
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    writable: true,
    value: async () => {
      throw new Error("Python server unavailable");
    },
  });
  try {
    const { renderer, snapshot } = await mountHook(harness);
    await snapshot.current!.acceptTermsAndRunSetup();
    assert.equal(
      harness.invocations.filter(({ command }) => command === "run_setup")
        .length,
      1,
    );
    renderer.unmount();
    await settleEffects();
  } finally {
    Object.defineProperty(globalThis, "fetch", {
      configurable: true,
      writable: true,
      value: previousFetch,
    });
  }
}

// Partial listener registration failures clean up every listener that did
// register, and unmounting after a complete registration remains idempotent.
{
  const { harness } = makeHarness();
  harness.failRegistrationEvents.add("setup_progress");
  const previousWarn = console.warn;
  console.warn = () => {};
  try {
    const { renderer } = await mountHook(harness);
    renderer.unmount();
    await settleEffects();
  } finally {
    console.warn = previousWarn;
  }
  const registered = harness.successfulRegistrations.filter(
    ({ event }) => event === "download_progress",
  );
  const unregistered = harness.unregisteredListeners.filter(
    ({ event }) => event === "download_progress",
  );
  assert.equal(registered.length, 1);
  assert.equal(unregistered.length, registered.length);
  assert.deepEqual(
    unregistered.map(({ eventId }) => eventId),
    registered.map(({ eventId }) => eventId),
  );
}

// Acceptance clears a stale pre-terms setup error and starts setup; an
// accepted-term retry with a later error remains handled by the retry path.
{
  const { harness } = makeHarness();
  harness.status = { ...pendingSetup(false), error: "previous setup failure" };
  const { renderer, snapshot } = await mountHook(harness);
  await snapshot.current!.acceptTermsAndRunSetup();
  assert.equal(
    harness.invocations.filter(({ command }) => command === "run_setup").length,
    1,
  );
  assert.equal(snapshot.current!.setupError, null);
  renderer.unmount();
  await settleEffects();
}

{
  const { harness } = makeHarness();
  const { renderer } = await mountHook(harness);
  const registered = harness.successfulRegistrations.filter(
    ({ event }) => event === "download_progress" || event === "setup_progress",
  );
  renderer.unmount();
  await settleEffects();
  const unregistered = harness.unregisteredListeners.filter(
    ({ event }) => event === "download_progress" || event === "setup_progress",
  );
  assert.equal(unregistered.length, registered.length);
  assert.deepEqual(
    unregistered.map(({ eventId }) => eventId).sort(),
    registered.map(({ eventId }) => eventId).sort(),
  );
}

// Disposal may win before either setup listener's dynamic registration
// resolves. Late successful registrations still have to be removed exactly
// once rather than being leaked.
{
  const { harness } = makeHarness();
  const snapshot: HookSnapshot = { current: null };
  const Probe = () => {
    snapshot.current = useAppState();
    return null;
  };
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(React.createElement(Probe));
  });
  renderer.unmount();
  await settleEffects();
  const registered = harness.successfulRegistrations.filter(
    ({ event }) => event === "download_progress" || event === "setup_progress",
  );
  const unregistered = harness.unregisteredListeners.filter(
    ({ event }) => event === "download_progress" || event === "setup_progress",
  );
  assert.equal(unregistered.length, registered.length);
  assert.deepEqual(
    unregistered.map(({ eventId }) => eventId).sort(),
    registered.map(({ eventId }) => eventId).sort(),
  );
}

// Only Gemma's setup download is projected into setup progress; unrelated
// model downloads must never move the first-run setup UI.
{
  const { harness } = makeHarness();
  const { renderer, snapshot } = await mountHook(harness);
  let release!: (value: unknown) => void;
  harness.runSetupGate = new Promise((resolve) => {
    release = resolve;
  });
  const run = snapshot.current!.runSetup();
  await settleEffects();
  const before = snapshot.current!.setupProgress;
  harness.emit("download_progress", {
    model_id: "kotoba-whisper",
    status: "downloading",
    percent: 30,
  });
  assert.strictEqual(snapshot.current!.setupProgress, before);
  harness.emit("download_progress", {
    model_id: GEMMA_MODEL_ID,
    status: "downloading",
    percent: 30,
  });
  await flush();
  assert.equal(snapshot.current!.setupProgress?.progress, 30);
  release(pendingSetup());
  await run;
  renderer.unmount();
  await settleEffects();
}

// Cancellation stays rendered as cancellation even if a stale completion event
// arrives after the cancel command.
{
  const { harness } = makeHarness();
  const { renderer, snapshot } = await mountHook(harness);
  await snapshot.current!.runSetup();
  await snapshot.current!.cancelSetup();
  harness.emit("setup_progress", {
    stage: "models",
    status: "completed",
    progress: 100,
  });
  await flush();
  assert.equal(snapshot.current!.setupStatus?.cancelled, true);
  assert.equal(snapshot.current!.setupStatus?.ready, false);
  renderer.unmount();
  await settleEffects();
}

// A malformed status is an actionable error state, not a terms prompt.
{
  const { harness } = makeHarness();
  harness.status = { ready: false };
  const { renderer, snapshot } = await mountHook(harness);
  assert.match(snapshot.current!.setupError || "", /状態|status|setup/i);
  assert.equal(snapshot.current!.setupStatus?.error !== null, true);
  renderer.unmount();
  await settleEffects();
}

// Terms acceptance is exposed only for a canonical, error-free status that
// explicitly requires setup and has not recorded the complete acknowledgement.
{
  const dom = new JSDOM(
    "<!doctype html><html><body><div id=\"root\"></div></body></html>",
  );
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: dom.window,
  });
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    writable: true,
    value: dom.window.document,
  });
  const host = dom.window.document.getElementById("root")!;
  const root: Root = createRoot(host);
  const props = {
    progress: null as SetupProgress | null,
    running: false,
    error: null as string | null,
    onStart: () => {},
    onRetry: () => {},
    onRequestElevation: () => {},
    elevationRequesting: false,
    onCancel: () => {},
    onAcceptTerms: () => {},
  };
  const render = async (status: unknown, error: string | null = null) => {
    await act(async () => {
      root.render(
        React.createElement(SetupScreen, {
          ...props,
          status: status as SetupStatus | null,
          error,
        }),
      );
    });
    assert.equal(
      Boolean(findButton(host, "規約を確認して同意")),
      false,
    );
  };
  await render(null);
  await render({ ...pendingSetup(), status: "unknown" });
  await render({ ...pendingSetup(), error: "setup status unavailable" }, "setup status unavailable");
  await render(
    { ...pendingSetup(), status: "error", error: "runtime setup failed" },
    "runtime setup failed",
  );
  await act(async () => {
    root.render(
      React.createElement(SetupScreen, {
        ...props,
        status: { ...pendingSetup(), error: null },
      }),
    );
  });
  assert.ok(findButton(host, "規約を確認して同意"));
  await act(async () => root.unmount());
}

// A model-transfer error must offer a manual installation path in addition to
// retry.  This keeps setup recoverable when a provider changes a URL or its
// API is temporarily unavailable.
{
  const dom = new JSDOM(
    "<!doctype html><html><body><div id=\"root\"></div></body></html>",
  );
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: dom.window,
  });
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    writable: true,
    value: dom.window.document,
  });
  const host = dom.window.document.getElementById("root")!;
  const root: Root = createRoot(host);
  await act(async () => {
    root.render(
      React.createElement(SetupScreen, {
        status: {
          ...pendingSetup(true),
          status: "error",
          error: "Failed to download 5 file(s): README.md",
          required_models_missing: ["kotoba-whisper-v2.0-faster"],
        },
        progress: null,
        running: false,
        error: "Failed to download 5 file(s): README.md",
        onStart: () => {},
        onRetry: () => {},
        onRequestElevation: () => {},
        elevationRequesting: false,
        onCancel: () => {},
        onAcceptTerms: () => {},
      }),
    );
  });
  assert.ok(findButton(host, "再試行"));
  const manualButton = findButton(host, "手動でモデルを設置");
  assert.ok(manualButton);
  await act(async () => manualButton!.click());
  assert.match(host.textContent || "", /公式リポジトリ/);
  assert.match(host.textContent || "", /models[\\/]kotoba-whisper-v2\.0-faster/);
  assert.match(host.textContent || "", /\.gameassistant-install\.json/);
  assert.ok(
    host.querySelector(
      'a[href="https://huggingface.co/kotoba-tech/kotoba-whisper-v2.0-faster/tree/main"]',
    ),
  );
  await act(async () => root.unmount());
}

// Cancellation cannot interrupt terms persistence or override its subsequent
// setup run, even when invoked between the first local save and its completion.
{
  const { harness } = makeHarness();
  harness.status = pendingSetup(false);
  let releaseSave!: () => void;
  let rejectRun!: (error: unknown) => void;
  harness.saveSettingGate = new Promise((resolve) => {
    releaseSave = () => resolve(undefined);
  });
  harness.runSetupGate = new Promise((_resolve, reject) => {
    rejectRun = reject;
  });
  const { renderer, snapshot } = await mountHook(harness);
  const acceptance = snapshot.current!.acceptTermsAndRunSetup();
  await flush();
  assert.equal(snapshot.current!.isSetupRunning, true);
  await snapshot.current!.cancelSetup();
  assert.equal(
    harness.invocations.filter(({ command }) => command === "cancel_setup")
      .length,
    0,
  );
  releaseSave();
  await settleEffects();
  assert.equal(
    harness.invocations.filter(({ command }) => command === "run_setup").length,
    1,
  );
  await snapshot.current!.cancelSetup();
  rejectRun(new Error("cancelled setup"));
  await acceptance;
  assert.equal(
    harness.invocations.filter(({ command }) => command === "cancel_setup")
      .length,
    1,
  );
  assert.equal(snapshot.current!.setupStatus?.cancelled, true);
  renderer.unmount();
  await settleEffects();
}

// Focus uses the caller-provided return target and follows action transitions.
{
  const dom = new JSDOM(
    '<!doctype html><html><body><button id="return">return</button><div id="root"></div></body></html>',
  );
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: dom.window,
  });
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    writable: true,
    value: dom.window.document,
  });
  const returnTarget = dom.window.document.getElementById(
    "return",
  ) as HTMLButtonElement;
  const host = dom.window.document.getElementById("root")!;
  const root: Root = createRoot(host);
  const props = {
    status: { ...pendingSetup(), error: "failed" },
    progress: null as SetupProgress | null,
    running: false,
    error: "failed",
    onStart: () => {},
    onRetry: () => {},
    onRequestElevation: () => {},
    elevationRequesting: false,
    onCancel: () => {},
    onAcceptTerms: () => {},
    focusReturnRef: { current: returnTarget },
  };
  await act(async () => {
    root.render(React.createElement(SetupScreen, props));
  });
  // A status carrying an error must not expose terms acceptance.
  assert.equal(
    dom.window.document.activeElement?.textContent?.includes("再試行"),
    true,
  );
  await act(async () => {
    root.render(
      React.createElement(SetupScreen, {
        ...props,
        status: { ...pendingSetup(), error: "previous setup cancellation", cancelled: true },
        error: "previous setup cancellation",
      }),
    );
  });
  assert.equal(
    dom.window.document.activeElement?.textContent?.includes("再試行"),
    true,
  );
  await act(async () => {
    root.render(
      React.createElement(SetupScreen, {
        ...props,
        status: { ...pendingSetup(true), error: "previous setup failure" },
      }),
    );
  });
  assert.equal(
    dom.window.document.activeElement?.textContent?.includes("再試行"),
    true,
  );
  await act(async () => {
    root.render(
      React.createElement(SetupScreen, {
        ...props,
        status: { ...pendingSetup(), error: null },
        running: true,
        error: null,
      }),
    );
  });
  assert.equal(Boolean(findButton(host, "キャンセル")), false);
  assert.equal(findButton(host, "規約の同意を保存中")?.disabled, true);
  await act(async () => {
    root.render(
      React.createElement(SetupScreen, {
        ...props,
        status: { ...pendingSetup(), cancelled: true },
        running: false,
        error: null,
      }),
    );
  });
  assert.equal(
    host.textContent?.includes("セットアップをキャンセルしました"),
    true,
  );
  await act(async () => {
    root.unmount();
  });
  assert.strictEqual(dom.window.document.activeElement, returnTarget);
}

// ---- Local memory summary settings (spec §4 UI states / §7 command & event) ----

// The preferences tab renders the runtime status verbatim: busy queue depth,
// fallback warning, the 800MB/CPU note, and both actions hit their commands.
// test_local_summary receives exactly { text } with the shared fixed text and
// its one-line verdict is displayed with the not-persisted annotation.
{
  const harness = new TauriHarness();
  harness.localSummaryStatus = {
    state: "busy",
    queueDepth: 3,
    fallbackActive: true,
    message: null,
  } satisfies LocalSummaryStatus;
  harness.testSummaryResult = {
    should_store: true,
    summary: "ユーザーは猫を2匹飼っている。",
  };
  const { host, root } = await renderPreferencesModal(harness);
  assert.match(host.textContent || "", /稼働状態:/);
  assert.match(host.textContent || "", /要約中（待ち 3 件）/);
  assert.match(host.textContent || "", /生テキストへフォールバック中/);
  assert.match(host.textContent || "", /約800MB/);
  assert.match(
    host.textContent || "",
    /モデルを自動ダウンロードすることはありません/,
  );

  const unloadButton = findButton(host, "要約モデルを停止");
  assert.ok(unloadButton, "unload button should render");
  await act(async () => {
    unloadButton!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
  });
  assert.equal(harness.unloadSummaryCalls, 1);

  const testButton = findButton(host, "テスト要約");
  assert.ok(testButton, "test summary button should render");
  await act(async () => {
    testButton!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
  });
  assert.deepEqual(harness.testSummaryArgs, { text: LOCAL_SUMMARY_TEST_TEXT });
  assert.match(
    host.textContent || "",
    /保存対象: ユーザーは猫を2匹飼っている。/,
  );
  assert.match(host.textContent || "", /記憶DBには保存されません/);

  // A should_store=false verdict is rendered as its own one-line outcome.
  harness.testSummaryResult = { should_store: false, summary: null };
  await act(async () => {
    testButton!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
  });
  assert.match(host.textContent || "", /保存不要と判定されました/);
  await act(async () => {
    root.unmount();
  });
  await settleEffects();
}

// The local-summary-status event drives live updates, and a malformed payload
// must never replace a valid status nor leak into the rendered text.
{
  const harness = new TauriHarness();
  harness.localSummaryStatus = {
    state: "starting",
    queueDepth: 0,
    fallbackActive: false,
    message: null,
  } satisfies LocalSummaryStatus;
  const { host, root } = await renderPreferencesModal(harness);
  assert.match(host.textContent || "", /起動中/);
  harness.emit("local-summary-status", {
    state: "ready",
    queueDepth: 0,
    fallbackActive: false,
    message: null,
  });
  await flush();
  assert.match(host.textContent || "", /待機中/);
  harness.emit("local-summary-status", {
    state: "destroyed",
    queueDepth: "many",
  });
  await flush();
  assert.match(host.textContent || "", /待機中/);
  assert.doesNotMatch(host.textContent || "", /destroyed/);
  await act(async () => {
    root.unmount();
  });
  const registered = harness.successfulRegistrations.filter(
    ({ event }) => event === "local-summary-status",
  );
  const unregistered = harness.unregisteredListeners.filter(
    ({ event }) => event === "local-summary-status",
  );
  assert.equal(registered.length, 1);
  assert.equal(unregistered.length, 1);
  await settleEffects();
}

// A missing/failed status command (integration phase pending) must not crash
// the section; a later model_missing report is informational, never red, and
// disables unload while the test action remains available.
{
  const harness = new TauriHarness();
  harness.failGetLocalSummaryStatus = true;
  const { host, root } = await renderPreferencesModal(harness);
  assert.match(
    host.textContent || "",
    /モデルを自動ダウンロードすることはありません/,
  );
  assert.doesNotMatch(host.textContent || "", /稼働状態:/);
  harness.emit("local-summary-status", {
    state: "model_missing",
    queueDepth: 0,
    fallbackActive: false,
    message: null,
  });
  await flush();
  const text = host.textContent || "";
  assert.match(text, /Gemma（任意）未導入 - 生テキストで記憶します/);
  const badge = Array.from(host.querySelectorAll("span")).find(
    (el) => (el.textContent || "") === "モデル未導入",
  );
  assert.ok(badge, "model_missing badge should render");
  assert.equal(
    /red/.test(badge!.className),
    false,
    "model_missing must not be styled as a red error",
  );
  const unloadButton = findButton(host, "要約モデルを停止");
  assert.equal(unloadButton!.disabled, true);
  const testButton = findButton(host, "テスト要約");
  assert.equal(testButton!.disabled, false);
  await act(async () => {
    root.unmount();
  });
  await settleEffects();
}

// Fact/Summary manager uses its dedicated server contract: navigation mounts
// Facts, the first request is bounded to 50 rows, cursors are opaque, and
// semantic deletion requires an exact-count confirmation while retaining raw.
{
  const harness = new TauriHarness();
  const dom = domForConfirm(harness);
  const host = dom.window.document.createElement("div");
  dom.window.document.body.appendChild(host);
  const root = createRoot(host);
  await act(async () => {
    root.render(React.createElement(MemoryModal, { isOpen: true, onClose: () => {} }));
    await new Promise<void>((resolve) => setTimeout(resolve, 20));
  });
  const semanticTab = findButton(host, "Fact / Summary");
  assert.ok(semanticTab, "semantic top tab should render");
  await act(async () => {
    semanticTab!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 30));
  });
  assert.match(host.textContent || "", /Facts/);
  const factRequest = harness.invocations.find(({ command }) => command === "memory_manager_list_facts");
  const factPageRequest = (factRequest?.args as { request: MemoryPageRequest }).request;
  assert.deepEqual(factPageRequest.page_size, 50);
  assert.equal(factPageRequest.sort, "newest");
  const next = findButton(host, "Next");
  assert.ok(next, "next page control should render");
  await act(async () => {
    next!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 20));
  });
  const factRequests = harness.invocations.filter(({ command }) => command === "memory_manager_list_facts");
  assert.equal((factRequests.at(-1)?.args as { request: MemoryPageRequest }).request.cursor, "offset:50");
  const requestFilters = factPageRequest.filters;
  assert.deepEqual(Object.keys(requestFilters).sort(), [
    "event_types",
    "has_summary",
    "occurred_from",
    "occurred_to",
    "sources",
    "statuses",
    "subjects",
  ]);
  const factRow = Array.from(host.querySelectorAll("button")).find((button) => (button.textContent || "").includes("self"));
  assert.ok(factRow, "normalized Fact row should render");
  const checkbox = factRow!.querySelector("span");
  assert.ok(checkbox);
  await act(async () => {
    checkbox!.dispatchEvent(new dom.window.MouseEvent("click", { bubbles: true }));
  });
  const deleteButton = findButton(host, "Delete (1)");
  assert.ok(deleteButton, "destructive action should show selected count");
  await act(async () => deleteButton!.click());
  assert.match(host.textContent || "", /Source Raw events.*retained/);
  assert.equal(harness.invocations.some(({ command }) => command === "memory_manager_delete_facts"), false);
  const confirmButton = findButton(host, "Delete 1");
  assert.ok(confirmButton, "delete confirmation should require explicit action");
  await act(async () => {
    confirmButton!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 20));
  });
  const deleteRequest = harness.invocations.find(({ command }) => command === "memory_manager_delete_facts");
  assert.deepEqual(
    (deleteRequest?.args as { request: { fact_ids: string[] } }).request.fact_ids,
    ["fact-1"],
  );
  const summariesTab = findButton(host, "Summaries");
  assert.ok(summariesTab);
  await act(async () => {
    summariesTab!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 20));
  });
  assert.match(host.textContent || "", /fallback/);
  const summaryRow = findButton(host, "fallback");
  assert.ok(summaryRow, "summary fallback row should render");
  await act(async () => summaryRow!.click());
  assert.match(host.textContent || "", /attempt-1/);
  assert.match(host.textContent || "", /要約推論に失敗/);
  assert.doesNotMatch(host.textContent || "", /complete raw evidence/);
  const retryButton = findButton(host, "Retry summary");
  assert.ok(retryButton, "retry affordance should be available for inference failure");
  await act(async () => {
    retryButton!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 20));
  });
  assert.equal(
    harness.invocations.filter(({ command }) => command === "memory_manager_retry_summary").length,
    1,
  );
  const rawEvidenceButton = findButton(host, "View raw evidence");
  assert.ok(rawEvidenceButton, "summary detail should expose raw fallback evidence");
  await act(async () => {
    rawEvidenceButton!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 20));
  });
  assert.match(host.textContent || "", /complete raw evidence/);
  await act(async () => root.unmount());
  await settleEffects();
}

// Each terminal reason has its own presentation label, and deterministic
// skipped rows are not offered an explicit retry. Unknown diagnostics use the
// compatibility inference_failed label rather than leaking free-form text.
for (const [reason, expectedLabel, retryable] of [
  ["metadata_echo", "メタデータだけの出力を拒否", true],
  ["ungrounded_summary", "未根拠の要約", true],
  ["invalid_model_output", "モデル出力が契約外", true],
  ["summary_runtime_timeout", "推論ランタイムのタイムアウト", true],
  ["summary_runtime_failed", "推論ランタイムに失敗", true],
  ["summary_queue_failed", "推論キューに失敗", true],
  ["inference_failed", "要約推論に失敗", true],
  ["model_declined", "モデルが保存不要と判定", false],
] as const) {
  const harness = new TauriHarness();
  harness.summaryReason = reason;
  harness.summaryStatus = retryable ? "fallback" : "skipped";
  const dom = domForConfirm(harness);
  const host = dom.window.document.createElement("div");
  dom.window.document.body.appendChild(host);
  const root = createRoot(host);
  await act(async () => {
    root.render(React.createElement(MemoryModal, { isOpen: true, onClose: () => {} }));
    await new Promise<void>((resolve) => setTimeout(resolve, 20));
  });
  const semanticTab = findButton(host, "Fact / Summary");
  assert.ok(semanticTab);
  await act(async () => {
    semanticTab!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 20));
  });
  const summariesTab = findButton(host, "Summaries");
  assert.ok(summariesTab);
  await act(async () => {
    summariesTab!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 20));
  });
  const summaryRow = findButton(host, harness.summaryStatus);
  assert.ok(summaryRow);
  await act(async () => summaryRow!.click());
  assert.match(host.textContent || "", new RegExp(expectedLabel));
  assert.equal(Boolean(findButton(host, "Retry summary")), retryable);
  await act(async () => root.unmount());
  await settleEffects();
}

// Malformed semantic IPC rows fail closed instead of becoming an empty list.
{
  const harness = new TauriHarness();
  harness.malformedSummaryResponse = true;
  const dom = domForConfirm(harness);
  const host = dom.window.document.createElement("div");
  dom.window.document.body.appendChild(host);
  const root = createRoot(host);
  await act(async () => {
    root.render(React.createElement(MemoryModal, { isOpen: true, onClose: () => {} }));
    await new Promise<void>((resolve) => setTimeout(resolve, 20));
  });
  const semanticTab = findButton(host, "Fact / Summary");
  assert.ok(semanticTab);
  await act(async () => {
    semanticTab!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 30));
  });
  const summariesTab = findButton(host, "Summaries");
  assert.ok(summariesTab);
  await act(async () => {
    summariesTab!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 30));
  });
  assert.match(host.textContent || "", /Invalid memory manager summary row/);
  assert.doesNotMatch(host.textContent || "", /No summaries found/);
  await act(async () => root.unmount());
  await settleEffects();
}

// Raw Memory view exposes an explicit all-memory semantic backfill action.
// Starting it is non-blocking and progress comes from the advisory event.
{
  const harness = new TauriHarness();
  harness.memoryItems = [
    {
      id: "event-1",
      document: "ユーザーは猫を飼っている",
      memory_type: "user_speech",
      source: "test",
      timestamp: "2026-09-04T00:00:00Z",
      user_id: "User",
    },
  ];
  const dom = domForConfirm(harness);
  const host = dom.window.document.createElement("div");
  dom.window.document.body.appendChild(host);
  const root = createRoot(host);
  await act(async () => {
    root.render(React.createElement(MemoryModal, { isOpen: true, onClose: () => {} }));
    await new Promise<void>((resolve) => setTimeout(resolve, 30));
  });
  const processButton = findButton(host, "Process all memories");
  assert.ok(processButton, "all-memory semantic backfill button should render");
  await act(async () => {
    processButton!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 10));
  });
  assert.equal(
    harness.invocations.filter(({ command }) => command === "memory_manager_process_all").length,
    1,
  );
  harness.emit("memory-manager-backfill-progress", {
    state: "running",
    processed: 1,
    total: 4,
    queued: 3,
    skipped: 1,
    failed: 0,
    message: "Processing",
    error: null,
    remaining: 2,
    reasonCounts: {},
    fatalError: null,
  });
  await flush();
  assert.match(host.textContent || "", /1 \/ 4/);
  assert.match(host.textContent || "", /persisted/);

  harness.emit("memory-manager-backfill-progress", {
    state: "completed",
    processed: 4,
    total: 4,
    queued: 3,
    skipped: 1,
    failed: 1,
    persisted: 2,
    remaining: 0,
    reason_counts: { invalid_model_output: 1 },
    last_error_reason: "invalid_model_output",
    fatalError: null,
    message: "Completed with warnings",
    error: null,
  });
  await flush();
  assert.match(host.textContent || "", /warning/i);
  assert.match(host.textContent || "", /invalid_model_output/);

  harness.emit("memory-manager-backfill-progress", {
    state: "error",
    processed: 2,
    total: 4,
    queued: 3,
    skipped: 1,
    failed: 0,
    persisted: 1,
    remaining: 2,
    reasonCounts: {},
    fatal_error: "journal_unavailable",
    message: "Stopped",
    error: "journal unavailable",
  });
  await flush();
  assert.match(host.textContent || "", /fatal/i);
  assert.match(host.textContent || "", /journal_unavailable/);
  await act(async () => root.unmount());
  await settleEffects();
}

// Let any deferred setup effects settle while the final Tauri harness is still
// installed, keeping npm test output deterministic.
makeHarness();
await new Promise<void>((resolve) => setTimeout(resolve, 550));

console.log("frontend behavioral contract checks passed");
