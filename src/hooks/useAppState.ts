import { useState, useEffect, useCallback, useRef } from "react";
import {
  type SystemStatus,
  type ResourceInfo,
  type WsMessage,
  type LogEntry,
  type PromptItem,
  type AsrEntry,
  type FactEntry,
  type ModelStatus,
  type SetupProgress,
  type SetupStatus,
  type SetupStepStatus,
  type DownloadProgressEvent,
  GEMMA_MODEL_ID,
  GEMMA_TERMS_VERSION,
  GEMMA_TERMS_MODEL_SHA256,
  GEMMA_TERMS_SOURCE,
  RUNTIME_STATUS_VALUES,
  type RuntimeStatus,
  LOCAL_SUMMARY_STATES,
  type LocalSummaryState,
  type LocalSummaryStatus,
  hasValidGemmaTerms,
} from "../types";
import { useWebSocket } from "./useWebSocket";

const API_BASE = "http://127.0.0.1:18080";

// Tauri 環境かどうかの判定
const isTauriEnv = () =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Download events report a literal percentage (1 means 1%), never a ratio. */
export const normalizeDownloadPercent = (value: unknown): number => {
  if (typeof value !== "number" || !Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(100, value));
};

/** Setup/model progress payloads use literal percentages (1 means 1%). */
export const isExpectedGemmaDownload = (modelId: unknown): modelId is string =>
  modelId === GEMMA_MODEL_ID;

/** App startup must remain blocked until a known, healthy setup status is available. */
export const shouldBlockAppUntilSetupReady = (
  _tauri: boolean,
  setup:
    | (Pick<SetupStatus, "ready" | "required_models_ready"> &
        Partial<
          Pick<
            SetupStatus,
            | "gemma_terms_accepted"
            | "gemma_terms_version"
            | "gemma_terms_model_sha256"
            | "gemma_terms_source"
            | "dependency_ready"
            | "python_import_ready"
            | "tokenizer_ready"
            | "embedding_ready"
            | "asr_websocket_ready"
          >
        > & { error?: string | null })
    | null,
): boolean => {
  return !isSetupReadyForSession(setup);
};

/**
 * The main UI is allowed to mount only after the first canonical setup
 * snapshot has arrived.  Keeping the unknown state separate from the
 * fail-closed setup screen prevents a ready portable install from briefly
 * flashing the first-run setup view while the IPC request is in flight.
 */
export const shouldShowMainUiForSession = (
  tauri: boolean,
  setup: Parameters<typeof shouldBlockAppUntilSetupReady>[1],
): boolean =>
  !tauri || (setup !== null && !shouldBlockAppUntilSetupReady(tauri, setup));

/** Every session transport must use the same fail-closed bootstrap contract. */
export const isSetupReadyForSession = (
  setup:
    | (Pick<SetupStatus, "ready" | "required_models_ready"> &
        Partial<
          Pick<
            SetupStatus,
            | "gemma_terms_accepted"
            | "gemma_terms_version"
            | "gemma_terms_model_sha256"
            | "gemma_terms_source"
            | "dependency_ready"
            | "python_import_ready"
            | "tokenizer_ready"
            | "embedding_ready"
            | "asr_websocket_ready"
          >
        > & { status?: unknown; error?: string | null })
    | null,
): boolean =>
  Boolean(
    setup &&
      isRuntimeStatus(setup.status) &&
      setup.ready === true &&
      setup.required_models_ready === true &&
      (setup.error === undefined ||
        setup.error === null ||
        setup.error === "") &&
      setup.dependency_ready === true &&
      setup.python_import_ready === true &&
      setup.tokenizer_ready === true &&
      setup.embedding_ready === true &&
      setup.asr_websocket_ready === true &&
      hasValidGemmaTerms(setup),
  );

const pendingSetupStatus = (): SetupStatus => ({
  ready: false,
  setup_required: true,
  status: "pending",
  gemma_terms_accepted: false,
  gemma_terms_version: "",
  gemma_terms_model_sha256: "",
  gemma_terms_source: "",
  dependency_ready: false,
  python_import_ready: false,
  tokenizer_ready: false,
  embedding_ready: false,
  asr_websocket_ready: false,
});

const asRecord = (value: unknown): Record<string, unknown> | null => {
  return value !== null && typeof value === "object"
    ? (value as Record<string, unknown>)
    : null;
};

const firstString = (...values: unknown[]): string | null => {
  const value = values.find(
    (candidate) => typeof candidate === "string" && candidate.trim().length > 0,
  );
  return typeof value === "string" ? value : null;
};

/**
 * Normalize the live Fact event before it enters the dashboard state. The
 * backend has used both domain-oriented (`summary`, `fact_id`) and generic
 * event-oriented (`content`, `id`) names in different transports, so the UI
 * accepts either shape while still rejecting an incomplete payload.
 */
export const normalizeFactEntry = (value: unknown): FactEntry | null => {
  const record = asRecord(value);
  if (!record) return null;
  const id = firstString(
    record.fact_id,
    record.id,
    record.source_event_id,
    record.event_id,
  );
  const text = firstString(
    record.summary,
    record.value,
    record.text,
    record.content,
  );
  if (!id || !text) return null;
  return {
    id,
    text,
    timestamp: firstString(record.timestamp, record.occurred_at) || "",
    source: firstString(record.source, record.author) || undefined,
    sourceEventId:
      firstString(record.source_event_id, record.event_id) || undefined,
  };
};

const isRuntimeStatus = (value: unknown): value is RuntimeStatus =>
  typeof value === "string" &&
  RUNTIME_STATUS_VALUES.some((status) => status === value);

const isLocalSummaryState = (value: unknown): value is LocalSummaryState =>
  typeof value === "string" &&
  LOCAL_SUMMARY_STATES.some((state) => state === value);

/**
 * Local summary status is advisory UI state: a malformed or partially drifted
 * payload must never crash the settings screen. Anything without a known
 * `state` is rejected outright; scalar fields fall back to neutral defaults.
 */
export const normalizeLocalSummaryStatus = (
  value: unknown,
): LocalSummaryStatus | null => {
  const record = asRecord(value);
  if (!record || !isLocalSummaryState(record.state)) return null;
  const queueDepth =
    typeof record.queueDepth === "number" && Number.isFinite(record.queueDepth)
      ? Math.max(0, Math.floor(record.queueDepth))
      : 0;
  return {
    state: record.state,
    queueDepth,
    fallbackActive: record.fallbackActive === true,
    message: firstString(record.message),
  };
};

const normalizeProgress = (value: unknown): number => {
  if (typeof value !== "number" || !Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(100, value));
};

/** Drain collected unlisten functions exactly once each, in order. */
const runUnlisteners = (unlisteners: Array<() => void>): void => {
  for (const unlisten of unlisteners.splice(0)) unlisten();
};

const normalizeSetupStep = (value: unknown): SetupStepStatus | null => {
  const record = asRecord(value);
  if (!record) return null;
  const id = firstString(record.id, record.stage, record.step, record.name);
  if (!id) return null;
  const status = firstString(record.status, record.state) || "pending";
  const error = firstString(record.error, record.error_message);
  return {
    id,
    label: firstString(record.label, record.title, record.name) || undefined,
    status,
    progress: normalizeProgress(record.progress),
    message: firstString(record.message, record.detail) || undefined,
    error,
  };
};

/** Derive the setup-running flag from a live stage event (no nested ternaries). */
const resolveSetupRunning = (
  stageActive: boolean,
  stageTerminal: boolean,
  stageCompleted: boolean,
  previous: SetupStatus | null,
): boolean => {
  if (stageActive) return true;
  if (stageTerminal) return false;
  if (stageCompleted && !previous?.ready) return true;
  return Boolean(previous?.running);
};

export const normalizeSetupStatus = (value: unknown): SetupStatus | null => {
  const record = asRecord(value);
  if (!record) return null;

  // RuntimeStatus is serialized by Rust with these fields. Rejecting a
  // partial object is important: treating `{ ready: false }` as a terms state
  // hides a broken IPC/backend contract behind an irrelevant prompt.
  const requiredBooleanFields = [
    "ready",
    "setup_required",
    "running",
    "cancelled",
    "required_models_ready",
    "writable",
    "elevation_required",
    "uv_present",
    "scripts_present",
    "python_present",
    "venv_present",
    "lock_present",
    "gemma_terms_accepted",
    "dependency_ready",
    "python_import_ready",
    "tokenizer_ready",
    "embedding_ready",
    "asr_websocket_ready",
  ];
  const requiredArrayFields = [
    "stages",
    "completed_stages",
    "required_models_missing",
  ];
  const validBooleans = requiredBooleanFields.every(
    (field) => typeof record[field] === "boolean",
  );
  const validArrays = requiredArrayFields.every((field) =>
    Array.isArray(record[field]),
  );
  const validStrings =
    isRuntimeStatus(record.status) &&
    typeof record.progress === "number" &&
    (typeof record.current_stage === "string" ||
      record.current_stage === null) &&
    typeof record.gemma_terms_version === "string" &&
    typeof record.gemma_terms_model_sha256 === "string" &&
    typeof record.gemma_terms_source === "string";
  const validOptionalStrings = ["message", "error", "elevation_message"].every(
    (field) =>
      record[field] === undefined ||
      record[field] === null ||
      typeof record[field] === "string",
  );
  const validStages =
    Array.isArray(record.stages) &&
    record.stages.every((step) => {
      const stage = asRecord(step);
      return (
        Boolean(stage) &&
        typeof stage!.id === "string" &&
        typeof stage!.status === "string" &&
        typeof stage!.progress === "number"
      );
    });
  const validCompletedStages =
    Array.isArray(record.completed_stages) &&
    record.completed_stages.every((stage) => typeof stage === "string");
  const validMissingModels =
    Array.isArray(record.required_models_missing) &&
    record.required_models_missing.every((model) => typeof model === "string");
  if (
    !validBooleans ||
    !validArrays ||
    !validStrings ||
    !validOptionalStrings ||
    !validStages ||
    !validCompletedStages ||
    !validMissingModels
  )
    return null;

  // RuntimeStatus is the source of truth. Keep the canonical flat fields and
  // avoid inventing defaults that could authorize a drifted terms record.
  const explicitReady = record.ready as boolean;
  const explicitRequired = record.setup_required as boolean;
  const ready = explicitReady;
  const stage = firstString(
    record.current_stage,
    record.currentStage,
    record.stage,
    record.current_step,
    record.step,
  );
  const rawStages = Array.isArray(record.stages) ? record.stages : [];
  const stages = rawStages
    .map(normalizeSetupStep)
    .filter((step): step is SetupStepStatus => step !== null);
  const rawCompleted = Array.isArray(record.completed_stages)
    ? record.completed_stages.filter(
        (stage): stage is string => typeof stage === "string",
      )
    : [];

  return {
    ...record,
    status: record.status as RuntimeStatus,
    ready,
    setup_required: explicitRequired,
    running: record.running as boolean,
    cancelled: record.cancelled as boolean,
    current_stage: stage,
    progress: normalizeProgress(record.progress ?? record.percent),
    message: firstString(record.message, record.detail),
    error: firstString(record.error, record.error_message),
    stages,
    completed_stages: rawCompleted,
    required_models_ready: record.required_models_ready as boolean,
    required_models_missing: record.required_models_missing as string[],
    writable: record.writable as boolean,
    elevation_required: record.elevation_required as boolean,
    elevation_message: firstString(
      record.elevation_message,
      record.elevationMessage,
    ),
    uv_present: record.uv_present as boolean,
    scripts_present: record.scripts_present as boolean,
    python_present: record.python_present as boolean,
    venv_present: record.venv_present as boolean,
    lock_present: record.lock_present as boolean,
    dependency_ready: record.dependency_ready as boolean,
    python_import_ready: record.python_import_ready as boolean,
    tokenizer_ready: record.tokenizer_ready as boolean,
    embedding_ready: record.embedding_ready as boolean,
    asr_websocket_ready: record.asr_websocket_ready as boolean,
    gemma_terms_accepted: record.gemma_terms_accepted as boolean,
    gemma_terms_version: record.gemma_terms_version as string,
    gemma_terms_model_sha256: record.gemma_terms_model_sha256 as string,
    gemma_terms_source: record.gemma_terms_source as string,
  };
};

const isCanonicalSetupShape = (setup: SetupStatus): boolean => {
  const requiredBooleanFields = [
    "ready",
    "setup_required",
    "running",
    "cancelled",
    "required_models_ready",
    "writable",
    "elevation_required",
    "uv_present",
    "scripts_present",
    "python_present",
    "venv_present",
    "lock_present",
    "gemma_terms_accepted",
    "dependency_ready",
    "python_import_ready",
    "tokenizer_ready",
    "embedding_ready",
    "asr_websocket_ready",
  ] as const;
  const requiredArrayFields = [
    "stages",
    "completed_stages",
    "required_models_missing",
  ] as const;
  const validBooleans = requiredBooleanFields.every(
    (field) => typeof setup[field] === "boolean",
  );
  const validArrays = requiredArrayFields.every((field) =>
    Array.isArray(setup[field]),
  );
  const validStrings =
    isRuntimeStatus(setup.status) &&
    typeof setup.progress === "number" &&
    (typeof setup.current_stage === "string" || setup.current_stage === null) &&
    typeof setup.gemma_terms_version === "string" &&
    typeof setup.gemma_terms_model_sha256 === "string" &&
    typeof setup.gemma_terms_source === "string";
  const validOptionalStrings = ["message", "error", "elevation_message"].every(
    (field) =>
      setup[field] === undefined ||
      setup[field] === null ||
      typeof setup[field] === "string",
  );
  if (!validBooleans || !validArrays || !validStrings || !validOptionalStrings)
    return false;
  const validStages = setup.stages!.every(
    (stage) =>
      stage !== null &&
      typeof stage === "object" &&
      typeof stage.id === "string" &&
      typeof stage.status === "string" &&
      typeof stage.progress === "number",
  );
  const validCompletedStages = setup.completed_stages!.every(
    (stage) => typeof stage === "string",
  );
  const validMissingModels = setup.required_models_missing!.every(
    (model) => typeof model === "string",
  );
  return validStages && validCompletedStages && validMissingModels;
};

/** A canonical, acknowledged but incomplete status may begin setup/download. */
export const isSetupActionAllowed = (
  setup: SetupStatus | null | undefined,
): boolean =>
  Boolean(
    setup &&
      isCanonicalSetupShape(setup) &&
      setup.ready === false &&
      setup.setup_required === true &&
      hasValidGemmaTerms(setup),
  );

/**
 * Terms may be requested only by a complete canonical runtime snapshot. A
 * malformed or unrelated error snapshot stays non-actionable; the exact gate
 * error from older releases is treated as recoverable Terms state.
 */
export const isTermsAcceptanceRequired = (
  setup: SetupStatus | null | undefined,
): boolean => {
  if (
    !setup ||
    !isCanonicalSetupShape(setup) ||
    setup.ready !== false ||
    setup.setup_required !== true ||
    setup.gemma_terms_accepted !== false
  ) {
    return false;
  }
  const error = setup.error?.trim().toLowerCase() || "";
  // Older releases attempted setup before Terms were accepted and persisted
  // this exact gate error. Treat it as the Terms state it represents so the
  // user can recover without a manual state-file edit.
  const termsGateError =
    error ===
    "gemma terms acknowledgement is required before setup can download the required model";
  return (setup.status !== "error" && error === "") || termsGateError;
};

const normalizeSetupProgress = (value: unknown): SetupProgress | null => {
  const record = asRecord(value);
  if (!record) return null;
  const stage = firstString(
    record.stage,
    record.current_stage,
    record.step,
    record.id,
  );
  if (!stage) return null;
  return {
    stage,
    status: firstString(record.status, record.state) || undefined,
    progress: normalizeProgress(record.progress ?? record.percent),
    message: firstString(record.message, record.detail),
    error: firstString(record.error, record.error_message),
    current: typeof record.current === "number" ? record.current : undefined,
    total: typeof record.total === "number" ? record.total : undefined,
  };
};

const setupStagesMatch = (
  left?: string | null,
  right?: string | null,
): boolean => {
  if (!left || !right) return false;
  const normalize = (value: string) =>
    value.toLowerCase().replace(/[_\s-]/g, "");
  const a = normalize(left);
  const b = normalize(right);
  return a === b || a.includes(b) || b.includes(a);
};

export function useAppState() {
  const { isConnected, addListener } = useWebSocket();

  // プロンプト設定
  const [prompts, setPrompts] = useState<PromptItem[]>([]);

  // モデル状態
  const [modelsStatus, setModelsStatus] = useState<ModelStatus[]>([]);
  const [missingRequiredModels, setMissingRequiredModels] =
    useState<boolean>(false);

  // 初回ランタイムセットアップ状態
  const [setupStatus, setSetupStatus] = useState<SetupStatus | null>(null);
  const [setupProgress, setSetupProgress] = useState<SetupProgress | null>(
    null,
  );
  const [isSetupRunning, setIsSetupRunning] = useState<boolean>(false);
  const [setupError, setSetupError] = useState<string | null>(null);
  const [isElevationRequesting, setIsElevationRequesting] =
    useState<boolean>(false);
  // Keep live stage events ahead of a stale state-file snapshot returned by
  // polling. It also prevents a delayed completed event from undoing cancel.
  const setupEventRef = useRef<SetupProgress | null>(null);
  // Cancellation is a separate generation guard because a cancel command can
  // race with queued progress events and an older runtime-status snapshot.
  const cancelRequestedRef = useRef(false);
  // Manual model downloads also emit download_progress. Only a currently
  // running setup may project those events into the setup screen.
  const setupDownloadActiveRef = useRef(false);
  const setupRunInFlightRef = useRef<Promise<SetupStatus | null> | null>(null);
  const setupStatusRef = useRef<SetupStatus | null>(null);
  const setupAttemptErrorRef = useRef<string | null>(null);
  // The native warmup failure is transient and is not persisted in the
  // bootstrap state file. Keep it across status polls so a healthy portable
  // runtime cannot erase the actionable WebSocket error before the user can
  // retry it.
  const asrWarmupErrorRef = useRef<string | null>(null);
  const termsAcceptanceInFlightRef = useRef<Promise<SetupStatus | null> | null>(
    null,
  );

  // システム状態
  const [status, setStatus] = useState<SystemStatus>({
    asr: false,
    gemini: false,
    tts: false,
    twitch: false,
    session: false,
  });
  const [sessionStarting, setSessionStarting] = useState(false);
  const sessionStartInFlightRef = useRef(false);

  // 音声レベル
  const [levelMeter, setLevelMeter] = useState<number>(0);

  // 音声認識 (ASR)
  const [currentAsr, setCurrentAsr] = useState<{
    text: string;
    isFinal: boolean;
    isPrompt?: boolean;
    latencyMs?: number | null;
  }>({
    text: "",
    isFinal: true,
    isPrompt: false,
  });
  const [asrHistory, setAsrHistory] = useState<AsrEntry[]>([]);

  // Durable facts derived from live speech are kept separately from the raw
  // transcript so the dashboard can distinguish what was said from what the
  // memory curator decided to retain.
  const [factHistory, setFactHistory] = useState<FactEntry[]>([]);

  // Gemini 回答
  const [geminiResponse, setGeminiResponse] = useState<string>("");

  // リソースモニター
  const [vram, setVram] = useState<ResourceInfo>({
    used: 0,
    total: 0,
    percent: 0,
  });
  const [ram, setRam] = useState<ResourceInfo>({
    used: 0,
    total: 0,
    percent: 0,
  });

  // 自動ツッコミタイマー
  const [commentaryTimer, setCommentaryTimer] = useState<{
    progress: number;
    remaining: number;
  }>({
    progress: 0,
    remaining: 0,
  });

  // トースト通知状態
  const [toast, setToast] = useState<{
    id: string;
    message: string;
    type: "success" | "info" | "warning";
  } | null>(null);
  const toastTimerRef = useRef<NodeJS.Timeout | null>(null);

  const showToast = useCallback(
    (message: string, type: "success" | "info" | "warning" = "success") => {
      if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
      setToast({
        id: Math.random().toString(36).substring(2, 9),
        message,
        type,
      });
      toastTimerRef.current = setTimeout(() => {
        setToast(null);
      }, 4000);
    },
    [],
  );

  // リアルタイムログ
  const [logs, setLogs] = useState<LogEntry[]>([]);

  // デバイス & ウィンドウ設定
  const [inputDevices, setInputDevices] = useState<string[]>([
    "Default (System Default)",
  ]);
  const [discordDevices, setDiscordDevices] = useState<string[]>([
    "Auto (Discord App / System Loopback)",
  ]);
  const [selectedDevice, setSelectedDevice] = useState<string>(
    "Default (System Default)",
  );
  const [selectedDiscordDevice, setSelectedDiscordDevice] = useState<string>(
    "Auto (Discord App / System Loopback)",
  );
  const [enableDiscordCapture, setEnableDiscordCapture] =
    useState<boolean>(false);

  const [windows, setWindows] = useState<string[]>([]);
  const [selectedWindow, setSelectedWindow] = useState<string>("");
  const [previewImage, setPreviewImage] = useState<string>("");

  // 設定オブジェクト
  const [settings, setSettings] = useState<Record<string, unknown>>({});

  const micActiveTimerRef = useRef<NodeJS.Timeout | null>(null);

  useEffect(() => {
    setupStatusRef.current = setupStatus;
  }, [setupStatus]);

  // -------------------------------------------------------------
  // Tauri イベントリスナー初期化 (Rust Native イベント)
  // -------------------------------------------------------------
  useEffect(() => {
    let unlistenAll: (() => void) | null = null;
    let isCancelled = false;
    const unlistenFns: (() => void)[] = [];

    void import("@tauri-apps/api/event")
      .then(async ({ listen }) => {
        if (isCancelled) return;

        const once = (unlisten: () => void): (() => void) => {
          let called = false;
          return () => {
            if (called) return;
            called = true;
            unlisten();
          };
        };
        const register = async <T>(
          eventName: string,
          handler: (event: { payload: T }) => void,
        ) => {
          const remove = once(
            await listen<T>(eventName, (event) => {
              if (!isCancelled) handler(event);
            }),
          );
          if (isCancelled) remove();
          else unlistenFns.push(remove);
        };

        try {
          await register<{ ram: ResourceInfo; vram: ResourceInfo }>(
            "resource_status",
            (event) => {
              if (event.payload) {
                setRam(event.payload.ram);
                setVram(event.payload.vram);
              }
            },
          );

          await register<number>("level_meter", (event) => {
            if (typeof event.payload === "number") {
              const lvl = event.payload;
              setLevelMeter(lvl);
              if (lvl > 0.012) {
                setStatus((prev) => (prev.asr ? prev : { ...prev, asr: true }));
                if (micActiveTimerRef.current)
                  clearTimeout(micActiveTimerRef.current);
                micActiveTimerRef.current = setTimeout(() => {
                  setStatus((prev) => ({ ...prev, asr: false }));
                }, 450);
              }
            }
          });

          await register<{
            text: string;
            is_final: boolean;
            is_prompt?: boolean;
            stream?: string;
            latency_ms?: number | null;
            event_id?: string | null;
          }>("asr_result", (event) => {
            if (event.payload && event.payload.text) {
              const rawText = event.payload.text.trim();
              const isPrompt = !!event.payload.is_prompt;
              const isDiscord =
                event.payload.stream === "discord" ||
                rawText.startsWith("[Discord]");

              setStatus((prev) => (prev.asr ? prev : { ...prev, asr: true }));
              if (micActiveTimerRef.current)
                clearTimeout(micActiveTimerRef.current);
              micActiveTimerRef.current = setTimeout(
                () => {
                  setStatus((prev) => ({ ...prev, asr: false }));
                },
                event.payload.is_final ? 350 : 800,
              );

              if (rawText) {
                if (event.payload.is_final) {
                  setAsrHistory((prev) => {
                    const lastIndex = prev.length - 1;
                    const last = prev[lastIndex];
                    if (
                      last &&
                      (last.text === rawText ||
                        rawText.includes(last.text) ||
                        last.text.includes(rawText))
                    ) {
                      const updated = [...prev];
                      updated[lastIndex] = {
                        ...last,
                        text: rawText,
                        isPrompt: isPrompt || last.isPrompt,
                        isDiscord: isDiscord || last.isDiscord,
                        latencyMs:
                          event.payload.latency_ms ?? last.latencyMs ?? null,
                      };
                      return updated;
                    }
                    return [
                      ...prev.slice(-29),
                      {
                        id: Math.random().toString(36).substring(2, 9),
                        text: rawText,
                        timestamp: new Date().toLocaleTimeString(),
                        isDiscord,
                        isPrompt,
                        latencyMs: event.payload.latency_ms ?? null,
                      },
                    ];
                  });
                  setCurrentAsr({ text: "", isFinal: true, isPrompt: false });
                } else {
                  setCurrentAsr({
                    text: rawText,
                    isFinal: false,
                    isPrompt,
                    latencyMs: event.payload.latency_ms ?? null,
                  });
                }
              }
            }
          });

          await register<unknown>("memory-fact-created", (event) => {
            const fact = normalizeFactEntry(event.payload);
            if (!fact) return;
            setFactHistory((prev) => {
              const duplicate = prev.some(
                (item) =>
                  item.id === fact.id ||
                  (fact.sourceEventId !== undefined &&
                    item.sourceEventId === fact.sourceEventId),
              );
              if (duplicate) return prev;
              return [...prev.slice(-29), fact];
            });
          });

          await register<{
            is_running: boolean;
            remaining_sec: number;
            total_sec: number;
          }>("auto_commentary_status", (event) => {
            if (event.payload) {
              const total = event.payload.total_sec || 1;
              const remaining = event.payload.remaining_sec || 0;
              const progress = Math.min(
                100,
                Math.max(0, ((total - remaining) / total) * 100),
              );
              setCommentaryTimer({
                progress,
                remaining,
              });
            }
          });

          await register<{
            type: string;
            author: string;
            content: string;
            timestamp: string;
          }>("session-event", (event) => {
            if (event.payload) {
              const { type, content } = event.payload;
              if (type === "ai_response" || type === "auto_commentary") {
                setGeminiResponse(content);
              }
            }
          });

          await register<{ is_generating: boolean }>(
            "gemini_status",
            (event) => {
              if (event.payload) {
                setStatus((prev) => ({
                  ...prev,
                  gemini: !!event.payload.is_generating,
                }));
              }
            },
          );

          await register<{ is_playing: boolean }>("tts_status", (event) => {
            if (event.payload) {
              setStatus((prev) => ({
                ...prev,
                tts: !!event.payload.is_playing,
              }));
            }
          });

          await register<{ connected: boolean }>("twitch_status", (event) => {
            if (event.payload) {
              setStatus((prev) => ({
                ...prev,
                twitch: !!event.payload.connected,
              }));
            }
          });

          await register<{
            message: string;
            type?: "success" | "info" | "warning";
          }>("toast_notice", (event) => {
            if (event.payload?.message) {
              showToast(event.payload.message, event.payload.type || "info");
            }
          });

          // `asr_ready` is emitted only by the native layer after the same
          // WebSocket used for audio/commands has completed its handshake.
          // Never infer this state from process startup or a toast message.
          await register<{ ready?: boolean; timestamp?: string }>(
            "asr_ready",
            (event) => {
              if (event.payload?.ready !== true) return;
              const previousWarmupError = asrWarmupErrorRef.current;
              asrWarmupErrorRef.current = null;
              const previous = setupStatusRef.current;
              if (!previous) return;
              // This event authorizes only the ASR transport. Keep any
              // dependency/tokenizer/embedding failure fail-closed even if
              // a late socket event arrives while that setup error is shown.
              const runtimeReadyWithoutAsr =
                previous.required_models_ready === true &&
                previous.dependency_ready === true &&
                previous.python_import_ready === true &&
                previous.tokenizer_ready === true &&
                previous.embedding_ready === true &&
                hasValidGemmaTerms(previous) &&
                (!previous.error || previous.error === previousWarmupError);
              const updated = {
                ...previous,
                ready: runtimeReadyWithoutAsr,
                setup_required: runtimeReadyWithoutAsr
                  ? false
                  : previous.setup_required,
                running: false,
                cancelled: false,
                status: runtimeReadyWithoutAsr
                  ? ("ready" as const)
                  : previous.status,
                error: runtimeReadyWithoutAsr ? null : previous.error,
                asr_websocket_ready: true,
                current_stage: runtimeReadyWithoutAsr
                  ? "complete"
                  : previous.current_stage,
                progress: runtimeReadyWithoutAsr
                  ? Math.max(previous.progress ?? 0, 100)
                  : previous.progress,
                message: runtimeReadyWithoutAsr
                  ? "ASR WebSocketの準備が完了しました。"
                  : previous.message,
              };
              setupStatusRef.current = updated;
              setSetupStatus(updated);
              setSetupError(updated.error || null);
            },
          );

          await register<{ message?: string }>("asr_warmup_failed", (event) => {
            const message =
              event.payload?.message ||
              "ASR WebSocketの準備に失敗しました。再試行してください。";
            asrWarmupErrorRef.current = message;
            setSetupStatus((previous) => {
              if (!previous) return previous;
              const updated = {
                ...previous,
                ready: false,
                setup_required: true,
                status: "error" as const,
                asr_websocket_ready: false,
                error: message,
                running: false,
                cancelled: false,
              };
              setupStatusRef.current = updated;
              return updated;
            });
            setSetupError(message);
          });

          await register<LogEntry>("app_log", (event) => {
            if (event.payload) {
              setLogs((prev) => [...prev.slice(-999), event.payload]);
            }
          });

          // Twitch 初期接続状態チェック
          import("@tauri-apps/api/core").then(({ invoke }) => {
            invoke<{ connected: boolean }>("twitch_get_status")
              .then((res) => {
                if (!isCancelled && res && res.connected) {
                  setStatus((prev) => ({ ...prev, twitch: true }));
                }
              })
              .catch(() => {});
          });

          if (isCancelled) {
            runUnlisteners(unlistenFns);
          } else {
            unlistenAll = () => runUnlisteners(unlistenFns);
          }
        } catch (err) {
          console.warn("Event listener registration error:", err);
          runUnlisteners(unlistenFns);
        }
      })
      .catch((error) => {
        if (!isCancelled)
          console.warn("Event listener module unavailable:", error);
        runUnlisteners(unlistenFns);
      });

    return () => {
      isCancelled = true;
      if (micActiveTimerRef.current) {
        clearTimeout(micActiveTimerRef.current);
      }
      if (unlistenAll) {
        unlistenAll();
      } else {
        runUnlisteners(unlistenFns);
      }
    };
  }, []);

  // WebSocket メッセージ受信ハンドラ
  useEffect(() => {
    const removeListener = addListener((msg: WsMessage) => {
      switch (msg.type) {
        case "status":
          setStatus(msg.status);
          break;
        case "level_meter":
          setLevelMeter(msg.level);
          break;
        case "asr": {
          const isFinal = Boolean(msg.is_final);
          const rawText = (msg.text || "").trim();
          if (!rawText) break;
          const stream =
            typeof msg.stream === "string" && msg.stream.trim()
              ? msg.stream.trim()
              : rawText.startsWith("[Discord]")
                ? "discord"
                : "mic";
          const isPrompt = Boolean(msg.is_prompt);
          const latencyMs =
            typeof msg.latency_ms === "number" && Number.isFinite(msg.latency_ms)
              ? msg.latency_ms
              : null;

          setCurrentAsr({
            text: rawText,
            isFinal,
            isPrompt,
            latencyMs,
          });

          if (isFinal) {
            setAsrHistory((prev) => {
              // 1. 直前の履歴と完全一致する場合は重複として追加しない
              if (prev.length > 0) {
                const last = prev[prev.length - 1];
                if (last.text === rawText) {
                  return prev;
                }
              }
              // 2. 直近3件に同一テキストがある場合も重複防止
              const recent = prev.slice(-3);
              if (recent.some((item) => item.text === rawText)) {
                return prev;
              }
              // 3. 1文字以下の極小ノイズは除外
              if (rawText.length < 2) {
                return prev;
              }

              const newEntry: AsrEntry = {
                id: `${Date.now()}_${Math.random().toString(36).substring(2, 7)}`,
                text: rawText,
                timestamp: new Date().toLocaleTimeString("ja-JP", {
                  hour: "2-digit",
                  minute: "2-digit",
                  second: "2-digit",
                }),
                isDiscord: stream === "discord",
                isPrompt,
                latencyMs,
              };
              return [...prev.slice(-49), newEntry];
            });
          }
          break;
        }
        case "gemini_response":
          setGeminiResponse(msg.text);
          break;
        case "resource_status":
          if (!isTauriEnv()) {
            setVram(msg.vram);
            setRam(msg.ram);
          }
          break;
        case "commentary_timer":
          setCommentaryTimer({
            progress: msg.progress,
            remaining: msg.remaining,
          });
          break;
        case "log":
          setLogs((prev) => {
            if (prev.length > 0) {
              const last = prev[prev.length - 1];
              // 直前のログと同一（タイムスタンプ、メッセージ、ロガーが同一）の場合は重複として無視
              if (
                last.timestamp === msg.timestamp &&
                last.message === msg.message &&
                last.logger === msg.logger
              ) {
                return prev;
              }
              // 直近10件内に同一タイムスタンプ＋同一メッセージがあれば無視
              const recent = prev.slice(-10);
              if (
                recent.some(
                  (l) =>
                    l.timestamp === msg.timestamp && l.message === msg.message,
                )
              ) {
                return prev;
              }
            }
            return [...prev.slice(-500), msg];
          });
          break;
        case "log_history":
          if (Array.isArray(msg.logs)) {
            setLogs((prev) => {
              const existingKeys = new Set(
                prev.map((l) => `${l.timestamp}_${l.logger}_${l.message}`),
              );
              const newEntries = msg.logs.filter(
                (l) =>
                  !existingKeys.has(`${l.timestamp}_${l.logger}_${l.message}`),
              );
              return [...prev, ...newEntries].slice(-500);
            });
          }
          break;
      }
    });

    // Tauri 起動初期ログの購読
    let disposed = false;
    const unlistenTauriLogs: Array<() => void> = [];
    const once = (unlisten: () => void): (() => void) => {
      let called = false;
      return () => {
        if (called) return;
        called = true;
        unlisten();
      };
    };
    if (isTauriEnv()) {
      void (async () => {
        try {
          const { listen } = await import("@tauri-apps/api/event");
          const remove = once(
            await listen<LogEntry>("python_startup_log", (event) => {
              if (disposed || !event.payload) return;
              setLogs((prev) => {
                const msg = event.payload;
                if (
                  prev.length > 0 &&
                  prev[prev.length - 1].message === msg.message
                )
                  return prev;
                return [...prev.slice(-500), msg];
              });
            }),
          );
          if (disposed) remove();
          else unlistenTauriLogs.push(remove);
        } catch (error) {
          console.warn("Startup log listener unavailable:", error);
        }
      })();
    }

    return () => {
      disposed = true;
      removeListener();
      runUnlisteners(unlistenTauriLogs);
    };
  }, [addListener]);

  // 1. プレビュー取得（重複・連打防止ガード付き）
  const isFetchingPreviewRef = useRef(false);
  const fetchPreview = useCallback(async (targetWindowName?: string) => {
    const win = targetWindowName || selectedWindowRef.current;
    if (!win || isFetchingPreviewRef.current) return;
    isFetchingPreviewRef.current = true;
    try {
      if (isTauriEnv()) {
        const { invoke } = await import("@tauri-apps/api/core");
        const preview = await invoke<string | null>("capture_window_preview", {
          title: win,
        });
        if (preview) {
          setPreviewImage(preview);
          return;
        }
      }
      const res = await fetch(`${API_BASE}/api/capture/preview`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ window: win }),
      });
      const data = await res.json();
      if (data.success && data.image) {
        setPreviewImage(data.image);
      }
    } catch (e) {
      console.error("Failed to fetch preview:", e);
    } finally {
      isFetchingPreviewRef.current = false;
    }
  }, []);

  const selectedWindowRef = useRef(selectedWindow);
  useEffect(() => {
    selectedWindowRef.current = selectedWindow;
  }, [selectedWindow]);

  // 2. 設定取得 & 復元（最優先）
  const fetchSettings = useCallback(async () => {
    try {
      let loaded: Record<string, unknown> | null = null;
      if (isTauriEnv()) {
        try {
          const { invoke } = await import("@tauri-apps/api/core");
          loaded = await invoke<Record<string, unknown>>("load_settings");
        } catch (e) {
          console.error("Tauri load_settings error:", e);
        }
      }

      if (!loaded || Object.keys(loaded).length === 0) {
        const res = await fetch(`${API_BASE}/api/settings`);
        loaded = await res.json();
      }

      if (loaded && Object.keys(loaded).length > 0) {
        setSettings(loaded);
        if (loaded.enable_discord_capture !== undefined) {
          setEnableDiscordCapture(Boolean(loaded.enable_discord_capture));
        }
        if (loaded.audio_device) {
          setSelectedDevice(String(loaded.audio_device));
        }
        if (loaded.discord_audio_device) {
          setSelectedDiscordDevice(String(loaded.discord_audio_device));
        }
        if (loaded.window) {
          setSelectedWindow(String(loaded.window));
        }

        return loaded;
      }
    } catch (e) {
      console.error("Failed to fetch settings:", e);
    }
    return null;
  }, [fetchPreview]);

  // 3. デバイス一覧取得（既存の設定値を保護）
  const fetchDevices = useCallback(async () => {
    try {
      if (isTauriEnv()) {
        const { invoke } = await import("@tauri-apps/api/core");
        const audioData = await invoke<{
          input_devices: string[];
          default_device: string | null;
        }>("list_audio_devices");
        if (audioData && audioData.input_devices.length > 0) {
          setInputDevices(audioData.input_devices);
          setDiscordDevices([
            "Auto (Discord App / System Loopback)",
            ...audioData.input_devices,
          ]);
          setSelectedDevice((prev) => {
            if (prev) return prev;
            return audioData.default_device || audioData.input_devices[0];
          });
          setSelectedDiscordDevice(
            (prev) => prev || "Auto (Discord App / System Loopback)",
          );
          return;
        }
      }

      const res = await fetch(`${API_BASE}/api/devices`);
      const data = await res.json();
      const inputs = data.input_devices || [];
      setInputDevices(inputs);
      setDiscordDevices(data.discord_devices || []);
      setSelectedDevice((prev) => {
        if (prev) return prev;
        return data.selected_device || (inputs.length > 0 ? inputs[0] : "");
      });
      setSelectedDiscordDevice(
        (prev) =>
          prev ||
          data.selected_discord_device ||
          "Auto (Discord App / System Loopback)",
      );
    } catch (e) {
      console.error("Failed to fetch devices:", e);
    }
  }, []);

  // 4. ウィンドウ一覧取得
  const fetchWindows = useCallback(async () => {
    try {
      if (isTauriEnv()) {
        const { invoke } = await import("@tauri-apps/api/core");
        const winList = await invoke<string[]>("list_windows");
        if (winList && winList.length > 0) {
          setWindows(winList);
          setSelectedWindow((prev) => {
            const target = prev && winList.includes(prev) ? prev : winList[0];
            return target;
          });
          return;
        }
      }
      const res = await fetch(`${API_BASE}/api/windows`);
      const data = await res.json();
      const winList = data.windows || [];
      setWindows(winList);
      setSelectedWindow((prev) => {
        const target =
          prev ||
          data.selected_window ||
          (winList.length > 0 ? winList[0] : "");
        return target;
      });
    } catch (e) {
      console.error("Failed to fetch windows:", e);
    }
  }, []);

  const fetchPrompts = useCallback(async () => {
    try {
      if (isTauriEnv()) {
        const { invoke } = await import("@tauri-apps/api/core");
        const promptList = await invoke<PromptItem[]>("get_prompts");
        if (promptList && Array.isArray(promptList)) {
          setPrompts(promptList);
          return;
        }
      }
      const res = await fetch(`${API_BASE}/api/prompts`);
      const data = await res.json();
      if (data.prompts) setPrompts(data.prompts);
    } catch (e) {
      console.error("Failed to fetch prompts:", e);
    }
  }, []);

  const fetchLogs = useCallback(async () => {
    try {
      if (isTauriEnv()) {
        const { invoke } = await import("@tauri-apps/api/core");
        const rustLogs = await invoke<LogEntry[]>("get_app_logs");
        if (rustLogs && Array.isArray(rustLogs)) {
          setLogs(rustLogs.slice(-500));
          return;
        }
      }
      const res = await fetch(`${API_BASE}/api/logs`);
      const data = await res.json();
      if (data.success && Array.isArray(data.logs)) {
        setLogs((prev) => {
          const existingKeys = new Set(
            prev.map((l) => `${l.timestamp}_${l.message}`),
          );
          const newEntries = (data.logs as LogEntry[]).filter(
            (l) => !existingKeys.has(`${l.timestamp}_${l.message}`),
          );
          return [...prev, ...newEntries].slice(-500);
        });
      }
    } catch {
      // ignore
    }
  }, []);

  const fetchModelsStatus = useCallback(async () => {
    try {
      if (isTauriEnv()) {
        const { invoke } = await import("@tauri-apps/api/core");
        const list = await invoke<ModelStatus[]>("get_models_status", {
          customDir: null,
        });
        setModelsStatus(list);
        const hasMissing = list.some((m) => m.required && !m.is_installed);
        setMissingRequiredModels(hasMissing);
        return list;
      }
    } catch (e) {
      console.warn("Failed to fetch models status:", e);
    }
    return [];
  }, []);

  /** Read the canonical setup manager state over the active transport. */
  const fetchSetupStatus =
    useCallback(async (): Promise<SetupStatus | null> => {
      try {
        let rawStatus: unknown;
        if (isTauriEnv()) {
          const { invoke } = await import("@tauri-apps/api/core");
          rawStatus = await invoke<unknown>("get_setup_status");
        } else {
          const response = await fetch(`${API_BASE}/api/setup/status`);
          if (!response.ok)
            throw new Error(`setup status request failed: ${response.status}`);
          rawStatus = await response.json();
        }
        const normalized = normalizeSetupStatus(rawStatus);
        if (!normalized) {
          const unavailable = {
            ...pendingSetupStatus(),
            error: "セットアップ状態を読み取れませんでした。",
          };
          setupStatusRef.current = unavailable;
          setSetupStatus(unavailable);
          setSetupError(unavailable.error || null);
          return unavailable;
        }

        // RuntimeStatus intentionally does not persist an ASR process failure:
        // the next launch may recover it. Preserve the live failure locally so
        // polling a still-healthy dependency/model snapshot cannot make the
        // Setup screen look ready again before the retry succeeds.
        if (normalized.asr_websocket_ready === true) {
          asrWarmupErrorRef.current = null;
        }

        const liveEvent = setupEventRef.current;
        const sameStage = setupStagesMatch(
          liveEvent?.stage,
          normalized.current_stage,
        );
        const cancellationRequested = cancelRequestedRef.current;
        const setupAttemptActive = Boolean(setupRunInFlightRef.current);
        const liveCancelled = Boolean(
          liveEvent && sameStage && liveEvent.status === "cancelled",
        );
        const liveStageActive = Boolean(
          liveEvent &&
            !normalized.ready &&
            (liveEvent.status === "running" ||
              liveEvent.status === "downloading" ||
              liveEvent.status === "completed"),
        );
        const staleAttemptError = setupAttemptErrorRef.current;
        let effectiveStatus: SetupStatus = normalized;
        const asrWarmupError = asrWarmupErrorRef.current;
        if (
          asrWarmupError &&
          normalized.asr_websocket_ready !== true &&
          !cancellationRequested &&
          !liveCancelled
        ) {
          effectiveStatus = {
            ...normalized,
            ready: false,
            setup_required: true,
            running: false,
            cancelled: false,
            status: "error",
            error: asrWarmupError,
          };
        } else if (cancellationRequested || liveCancelled) {
          effectiveStatus = {
            ...normalized,
            ready: false,
            setup_required: true,
            running: false,
            cancelled: true,
            message: "セットアップをキャンセルしました。",
            error: null,
            current_stage: liveEvent?.stage || normalized.current_stage,
          };
        } else if (
          liveStageActive ||
          (setupAttemptActive && !staleAttemptError && !normalized.ready)
        ) {
          effectiveStatus = {
            ...normalized,
            // A completed/running stage event must remain in preparation until
            // a later RuntimeStatus explicitly confirms ready=true.
            ready: false,
            setup_required: true,
            running: true,
            current_stage: liveEvent?.stage || normalized.current_stage,
            error: null,
          };
        }

        setupStatusRef.current = effectiveStatus;
        setSetupStatus(effectiveStatus);
        setIsSetupRunning(
          Boolean(effectiveStatus.running && !effectiveStatus.ready),
        );
        setSetupError(effectiveStatus.error || null);
        if (effectiveStatus.current_stage || effectiveStatus.ready) {
          setSetupProgress((previous) => {
            const stage =
              effectiveStatus.current_stage || previous?.stage || "python";
            const eventIsCurrentStage = Boolean(
              liveEvent && setupStagesMatch(liveEvent.stage, stage),
            );
            const serverStage = effectiveStatus.stages?.find((candidate) =>
              setupStagesMatch(candidate.id, stage),
            );

            // Do not copy RuntimeStatus.progress here: it is the overall setup
            // percentage. Preserve the current-stage event value when present.
            if (eventIsCurrentStage && liveEvent && !cancellationRequested)
              return liveEvent;
            if (cancellationRequested) {
              return {
                ...(previous || { stage, progress: 0 }),
                status: "cancelled",
                message: "セットアップをキャンセルしました。",
                error: null,
              };
            }
            return {
              stage,
              status: effectiveStatus.ready
                ? "completed"
                : serverStage?.status ||
                  (effectiveStatus.running ? "running" : undefined),
              progress: serverStage?.progress ?? 0,
              message: serverStage?.message || effectiveStatus.message || null,
              error: serverStage?.error || effectiveStatus.error || null,
            };
          });
        }
        if (effectiveStatus.ready) setupEventRef.current = null;
        return effectiveStatus;
      } catch (error) {
        console.warn("Setup status unavailable:", error);
        const unavailable = {
          ...pendingSetupStatus(),
          error: String(error),
        };
        setupStatusRef.current = unavailable;
        setSetupStatus(unavailable);
        setSetupError(unavailable.error || null);
        return unavailable;
      }
    }, []);

  // セットアップイベントはポーリングの補助として購読する。イベントが欠けても画面は更新される。
  useEffect(() => {
    if (!isTauriEnv()) return;
    let disposed = false;
    const unlistenFns: Array<() => void> = [];
    const once = (unlisten: () => void): (() => void) => {
      let called = false;
      return () => {
        if (called) return;
        called = true;
        unlisten();
      };
    };

    const register = async <T>(
      eventName: string,
      handler: (event: { payload: T }) => void,
    ) => {
      try {
        const { listen } = await import("@tauri-apps/api/event");
        const remove = once(await listen<T>(eventName, handler));
        if (disposed) remove();
        else unlistenFns.push(remove);
      } catch (error) {
        console.warn(`Setup listener unavailable (${eventName}):`, error);
      }
    };

    void register<DownloadProgressEvent>("download_progress", (event) => {
      if (disposed || !setupDownloadActiveRef.current) return;
      const payload = event.payload;
      if (!payload || !isExpectedGemmaDownload(payload.model_id)) return;
      const status =
        payload.status === "completed" ? "completed" : payload.status;
      const percent = normalizeDownloadPercent(payload.percent);
      const progress = {
        stage: "models" as const,
        status,
        progress: percent,
        current: payload.current_bytes,
        total: payload.total_bytes,
        message: `${payload.model_id}: ${Math.round(percent)}%`,
        error: payload.error_message || null,
      };
      setupEventRef.current = progress;
      setSetupProgress(progress);
    });

    void register<unknown>("setup_progress", (event) => {
      if (disposed) return;
      const progress = normalizeSetupProgress(event.payload);
      if (!progress) return;
      const progressWithMessage =
        progress.status === "cancelled" && !progress.message
          ? { ...progress, message: "セットアップをキャンセルしました。" }
          : progress;

      const previousEvent = setupEventRef.current;
      const cancellationRequested = cancelRequestedRef.current;
      const cancelledEvent = Boolean(
        previousEvent &&
          previousEvent.status === "cancelled" &&
          setupStagesMatch(previousEvent.stage, progress.stage),
      );
      // Once cancellation was requested, queued events from the old run
      // must not resurrect setup or replace the cancellation error.
      if (cancellationRequested && progress.status !== "cancelled") return;
      // A model downloader may flush a queued completed event after the
      // user cancelled. Keep the cancelled state authoritative until a
      // new run_setup call explicitly resets it.
      if (cancelledEvent && progress.status === "completed") {
        setIsSetupRunning(false);
        setSetupStatus((previous) =>
          previous
            ? {
                ...previous,
                ready: false,
                setup_required: true,
                running: false,
                cancelled: true,
                message: "セットアップをキャンセルしました。",
                error: null,
              }
            : previous,
        );
        return;
      }
      setupEventRef.current = progressWithMessage;
      setSetupProgress(progressWithMessage);
      // setup_progress.progress is the progress of the current stage (for
      // example, the second required model), not the overall runtime
      // progress returned by get_setup_status. Keep the setup manager
      // active through a stage's completed event; get_setup_status is the
      // authority for the final ready/running transition.
      if (progress.status === "running" || progress.status === "downloading") {
        setIsSetupRunning(true);
      } else if (
        progress.status === "error" ||
        progress.status === "cancelled"
      ) {
        setIsSetupRunning(false);
      } else if (progress.status === "completed") {
        // Wait for get_setup_status to confirm the final Complete stage.
        setIsSetupRunning(true);
      }
      setSetupError(progressWithMessage.error || null);
      setSetupStatus((previous) => {
        const stageActive =
          progressWithMessage.status === "running" ||
          progressWithMessage.status === "downloading";
        const stageTerminal =
          progressWithMessage.status === "error" ||
          progressWithMessage.status === "cancelled";
        const stageCompleted = progressWithMessage.status === "completed";
        return {
          ...(previous || pendingSetupStatus()),
          // Do not infer overall completion from a single stage event.
          // In particular, a completed models event can be followed by
          // the final state-file write and Complete stage.
          ready: Boolean(previous?.ready),
          setup_required: previous?.setup_required ?? true,
          cancelled:
            progressWithMessage.status === "cancelled"
              ? true
              : Boolean(previous?.cancelled),
          running: resolveSetupRunning(
            stageActive,
            stageTerminal,
            stageCompleted,
            previous,
          ),
          current_stage: progressWithMessage.stage,
          // Keep RuntimeStatus.progress (overall progress) untouched.
          message: progressWithMessage.message,
          error: progressWithMessage.error || null,
          stages: previous?.stages,
        };
      });
    });

    return () => {
      if (disposed) return;
      disposed = true;
      runUnlisteners(unlistenFns);
    };
  }, []);

  // setup manager はバックグラウンド処理中も状態ファイルを更新するため、短い間隔で再取得する。
  useEffect(() => {
    if (
      !isTauriEnv() ||
      !setupStatus ||
      (!setupStatus.setup_required &&
        !isSetupRunning &&
        setupStatus.asr_websocket_ready === true)
    )
      return;
    const timer = window.setInterval(() => {
      fetchSetupStatus();
    }, 1200);
    return () => window.clearInterval(timer);
  }, [fetchSetupStatus, isSetupRunning, setupStatus]);

  const runSetup = useCallback(async (): Promise<SetupStatus | null> => {
    if (!isTauriEnv()) return null;
    if (setupRunInFlightRef.current) return setupRunInFlightRef.current;
    const currentSetup = setupStatusRef.current;
    if (!isSetupActionAllowed(currentSetup)) {
      const message =
        "Gemma Terms またはセットアップ状態を確認できないため、セットアップを開始できません。";
      setSetupError(message);
      setSetupStatus((previous) =>
        previous ? { ...previous, error: message, running: false } : previous,
      );
      return null;
    }

    // A retry starts a fresh event sequence; otherwise a queued completion from
    // the previous cancelled run would be mistaken for the new run's result.
    asrWarmupErrorRef.current = null;
    cancelRequestedRef.current = false;
    setupEventRef.current = null;
    setupAttemptErrorRef.current = null;
    setupDownloadActiveRef.current = true;
    setIsSetupRunning(true);
    setSetupError(null);
    const pending = {
      ...(setupStatusRef.current || pendingSetupStatus()),
      ready: false,
      setup_required: true,
      running: true,
      cancelled: false,
      error: null,
    };
    setupStatusRef.current = pending;
    setSetupStatus(pending);

    const operation = (async (): Promise<SetupStatus | null> => {
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const result = await invoke<unknown>("run_setup");
        const normalized = normalizeSetupStatus(result);
        if (normalized && !cancelRequestedRef.current) {
          setupStatusRef.current = normalized;
          setSetupStatus(normalized);
          setSetupError(normalized.error || null);
        }
        return normalized;
      } catch (error) {
        if (cancelRequestedRef.current) return null;
        const message = String(error);
        setupAttemptErrorRef.current = message;
        setSetupError(message);
        const failed = {
          ...(setupStatusRef.current || pendingSetupStatus()),
          running: false,
          ready: false,
          setup_required: true,
          cancelled: false,
          error: message,
        };
        setupStatusRef.current = failed;
        setSetupStatus(failed);
        return null;
      } finally {
        await fetchSetupStatus();
        await fetchModelsStatus();
        setupDownloadActiveRef.current = false;
        setIsSetupRunning(false);
      }
    })();
    setupRunInFlightRef.current = operation;
    operation.then(
      () => {
        if (setupRunInFlightRef.current === operation)
          setupRunInFlightRef.current = null;
      },
      () => {
        if (setupRunInFlightRef.current === operation)
          setupRunInFlightRef.current = null;
      },
    );
    return operation;
  }, [fetchModelsStatus, fetchSetupStatus]);

  const requestSetupElevation = useCallback(async (): Promise<boolean> => {
    if (!isTauriEnv()) return false;
    if (setupRunInFlightRef.current) return false;
    setIsElevationRequesting(true);
    setSetupError(null);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const result = await invoke<{ launched?: boolean; message?: string }>(
        "request_setup_elevation",
      );
      if (!result?.launched) {
        setSetupError(
          result?.message || "管理者権限の要求を開始できませんでした。",
        );
        return false;
      }
      setSetupStatus((previous) =>
        previous
          ? {
              ...previous,
              message:
                result.message ||
                "管理者権限でセットアップを開始しました。UAC の確認を完了してください。",
            }
          : previous,
      );
      return true;
    } catch (error) {
      const message = String(error);
      setSetupError(message);
      setSetupStatus((previous) =>
        previous ? { ...previous, error: message } : previous,
      );
      return false;
    } finally {
      setIsElevationRequesting(false);
      await fetchSetupStatus();
    }
  }, [fetchSetupStatus]);

  const cancelSetup = useCallback(async () => {
    if (!isTauriEnv()) return;
    // Terms persistence is a single authoritative acknowledgement transaction.
    // Do not mark it cancelled or send cancel_setup while any save/validation
    // step is in flight; doing so would let cancellation override acceptance.
    if (termsAcceptanceInFlightRef.current) return;
    cancelRequestedRef.current = true;
    setSetupError(null);
    setSetupProgress((previous) =>
      previous
        ? {
            ...previous,
            status: "cancelled",
            message: "セットアップをキャンセルしました。",
            error: null,
          }
        : previous,
    );
    const cancelledStatus: SetupStatus = {
      ...(setupStatusRef.current || pendingSetupStatus()),
      ready: false,
      setup_required: true,
      running: false,
      cancelled: true,
      message: "セットアップをキャンセルしました。",
      error: null,
    };
    setupStatusRef.current = cancelledStatus;
    setSetupStatus(cancelledStatus);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      // The cancel command was added with the setup manager. Keep this optional
      // so older development binaries can still dismiss the setup screen.
      await invoke("cancel_setup");
    } catch (error) {
      console.warn("Setup cancellation unavailable:", error);
    } finally {
      if (setupEventRef.current) {
        setupEventRef.current = {
          ...setupEventRef.current,
          status: "cancelled",
          message: "セットアップをキャンセルしました。",
        };
      }
      setIsSetupRunning(false);
      setSetupStatus((previous) =>
        previous
          ? {
              ...previous,
              running: false,
              cancelled: true,
              ready: false,
              setup_required: true,
              message: "セットアップをキャンセルしました。",
              error: null,
            }
          : previous,
      );
    }
  }, []);

  // 初回起動時に一度だけ状態を読み込み、必要な場合だけ App が SetupScreen を表示する。
  useEffect(() => {
    fetchSetupStatus();
  }, [fetchSetupStatus]);

  const fetchAllData = useCallback(async () => {
    await fetchSettings();
    await Promise.all([
      fetchDevices(),
      fetchWindows(),
      fetchPrompts(),
      fetchModelsStatus(),
    ]);
    fetchLogs();
  }, [
    fetchSettings,
    fetchDevices,
    fetchWindows,
    fetchPrompts,
    fetchModelsStatus,
    fetchLogs,
  ]);

  useEffect(() => {
    fetchAllData();
  }, []);

  // 選択中ウィンドウのプレビュー初回取得
  const lastFetchedWinRef = useRef<string>("");
  useEffect(() => {
    if (selectedWindow && selectedWindow !== lastFetchedWinRef.current) {
      lastFetchedWinRef.current = selectedWindow;
      fetchPreview(selectedWindow);
    }
  }, [selectedWindow, fetchPreview]);

  // アクション
  const startSession = async () => {
    if (sessionStartInFlightRef.current) return;
    sessionStartInFlightRef.current = true;
    setSessionStarting(true);
    try {
      const currentSetup = await fetchSetupStatus();
      if (!isSetupReadyForSession(currentSetup)) {
        throw new Error(
          "セットアップが完了していないため、セッションを開始できません。",
        );
      }
      if (isTauriEnv()) {
        const { invoke } = await import("@tauri-apps/api/core");
        await invoke("session_start");
        // The native command waits for the startup-warmed ASR WebSocket. Do
        // not show an active session until that readiness gate has passed.
        setStatus((prev) => ({ ...prev, session: true }));
        return;
      }
      const res = await fetch(`${API_BASE}/api/session/start`, {
        method: "POST",
      });
      const data = await res.json();
      if (data.success) {
        setStatus((prev) => ({ ...prev, session: true }));
      } else {
        setStatus((prev) => ({ ...prev, session: false }));
      }
    } catch (e) {
      console.error("Failed to start session:", e);
      const message = e instanceof Error ? e.message : String(e);
      showToast(`セッションを開始できませんでした: ${message}`, "warning");
      setStatus((prev) => ({ ...prev, session: false }));
    } finally {
      sessionStartInFlightRef.current = false;
      setSessionStarting(false);
    }
  };

  const stopSession = async () => {
    // A native start may be waiting for the startup ASR warmup. Do not race
    // it with a stop command; the controls remain disabled until it resolves.
    if (sessionStartInFlightRef.current) return;
    setStatus((prev) => ({
      ...prev,
      session: false,
      gemini: false,
      tts: false,
    }));
    try {
      if (isTauriEnv()) {
        const { invoke } = await import("@tauri-apps/api/core");
        await invoke("session_stop");
        return;
      }
      await fetch(`${API_BASE}/api/session/stop`, { method: "POST" });
    } catch (e) {
      console.error("Failed to stop session:", e);
      const message = e instanceof Error ? e.message : String(e);
      showToast(`セッション停止に失敗しました: ${message}`, "warning");
    }
  };

  const restartWhisper = async () => {
    try {
      showToast("🔄 Whisper エンジンを再起動しています...", "info");
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("restart_whisper");
      asrWarmupErrorRef.current = null;
      setSetupStatus((previous) => {
        if (!previous) return previous;
        const updated = {
          ...previous,
          ready: true,
          setup_required: false,
          running: false,
          cancelled: false,
          status: "ready" as const,
          error: null,
          asr_websocket_ready: true,
          current_stage: "complete",
          progress: Math.max(previous.progress ?? 0, 100),
          message: "ASR WebSocketの準備が完了しました。",
        };
        setupStatusRef.current = updated;
        return updated;
      });
      setSetupError(null);
      showToast(
        "✅ Whisper エンジンの再起動とウォームアップが完了しました！",
        "success",
      );
    } catch (e) {
      console.error("Failed to restart whisper:", e);
      showToast(`Whisper 再起動エラー: ${e}`, "warning");
    }
  };

  type SettingUpdateOptions = { throwOnError?: boolean };
  const updateSetting = async (
    key: string,
    value: any,
    options: SettingUpdateOptions = {},
  ) => {
    // 1. ローカルステート即時更新（UIの遅延ゼロ）
    setSettings((prev) => ({ ...prev, [key]: value }));
    if (key === "enable_discord_capture") setEnableDiscordCapture(value);
    if (key === "audio_device") setSelectedDevice(value);
    if (key === "discord_audio_device") setSelectedDiscordDevice(value);
    if (key === "window") {
      setSelectedWindow(value);
    }

    // 2. Tauri Rust 経由で settings.json へ即時書き込み
    let tauriSaveSucceeded = false;
    if (isTauriEnv()) {
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        await invoke("save_setting", { key, value });
        tauriSaveSucceeded = true;
      } catch (te) {
        console.error("Tauri save_setting error:", te);
        if (options.throwOnError) throw te;
      }
    }

    // 3. Python サーバー経由で settings.json へ即時書き込み & サービス反映
    try {
      const response = await fetch(`${API_BASE}/api/settings`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ key, value }),
      });
      if (!response.ok)
        throw new Error(`settings request failed: ${response.status}`);
      const result = await response.json();
      if (result && result.success === false)
        throw new Error(result.error || `failed to save setting ${key}`);
    } catch (e) {
      console.error(`Failed to update setting ${key}:`, e);
      // The Rust command is authoritative in Tauri. Keep the HTTP endpoint as
      // a compatibility/service-sync best effort, but never let its absence
      // invalidate a successful local save (notably during first-run setup).
      if (options.throwOnError && !tauriSaveSucceeded) throw e;
    }
  };

  const savePrompt = async (id: string, value: string): Promise<boolean> => {
    try {
      if (isTauriEnv()) {
        const { invoke } = await import("@tauri-apps/api/core");
        const updatedList = await invoke<PromptItem[]>("save_prompt", {
          id,
          value,
        });
        if (updatedList && Array.isArray(updatedList)) {
          setPrompts(updatedList);
          showToast("✅ プロンプト設定を保存しました", "success");
          return true;
        }
      }
      const res = await fetch(`${API_BASE}/api/prompts`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ id, value }),
      });
      const data = await res.json();
      if (data.success && data.prompts) {
        setPrompts(data.prompts);
        return true;
      }
      return false;
    } catch (e) {
      console.error(`Failed to save prompt ${id}:`, e);
      return false;
    }
  };

  const resetPrompt = async (id: string): Promise<boolean> => {
    try {
      if (isTauriEnv()) {
        const { invoke } = await import("@tauri-apps/api/core");
        const updatedList = await invoke<PromptItem[]>("reset_prompt", { id });
        if (updatedList && Array.isArray(updatedList)) {
          setPrompts(updatedList);
          showToast("🔄 プロンプトを初期デフォルトに戻しました", "info");
          return true;
        }
      }
      const res = await fetch(`${API_BASE}/api/prompts/reset`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ id }),
      });
      const data = await res.json();
      if (data.success && data.prompts) {
        setPrompts(data.prompts);
        return true;
      }
      return false;
    } catch (e) {
      console.error(`Failed to reset prompt ${id}:`, e);
      return false;
    }
  };

  const acceptTermsAndRunSetup =
    useCallback(async (): Promise<SetupStatus | null> => {
      if (!isTauriEnv()) return null;
      if (termsAcceptanceInFlightRef.current)
        return termsAcceptanceInFlightRef.current;

      // Mark the flow active before the first await so a second click cannot
      // start another acceptance/run sequence.
      setIsSetupRunning(true);
      let operation: Promise<SetupStatus | null> = Promise.resolve(null);
      operation = (async (): Promise<SetupStatus | null> => {
        try {
          await updateSetting("gemma_terms_accepted", true, {
            throwOnError: true,
          });
          // Persist the complete acknowledgement record. The Rust command also
          // fills these fields, while the HTTP fallback accepts individual
          // setting payloads and otherwise would leave Gemma blocked.
          await updateSetting("gemma_terms_version", GEMMA_TERMS_VERSION, {
            throwOnError: true,
          });
          await updateSetting(
            "gemma_terms_model_sha256",
            GEMMA_TERMS_MODEL_SHA256,
            { throwOnError: true },
          );
          await updateSetting("gemma_terms_source", GEMMA_TERMS_SOURCE, {
            throwOnError: true,
          });
          const refreshedSettings = await fetchSettings();
          if (!hasValidGemmaTerms(refreshedSettings)) {
            throw new Error("Gemma Terms の同意を保存できませんでした。");
          }
          const refreshedStatus = await fetchSetupStatus();
          if (!isSetupActionAllowed(refreshedStatus)) {
            throw new Error(
              "Gemma Terms の同意状態をランタイムで確認できませんでした。",
            );
          }
          // Terms persistence is complete before setup starts. Release the
          // cancellation guard at this handoff so a user can still cancel the
          // actual setup/download operation.
          const setupOperation = runSetup();
          termsAcceptanceInFlightRef.current = null;
          return await setupOperation;
        } catch (error) {
          const message = String(error);
          setIsSetupRunning(false);
          setSetupError(message);
          setSetupStatus((previous) => ({
            ...(previous || pendingSetupStatus()),
            ready: false,
            setup_required: true,
            running: false,
            error: message,
          }));
          return null;
        } finally {
          await fetchSettings();
          await fetchSetupStatus();
          if (termsAcceptanceInFlightRef.current === operation) {
            termsAcceptanceInFlightRef.current = null;
          }
        }
      })();
      termsAcceptanceInFlightRef.current = operation;
      return operation;
    }, [fetchSettings, fetchSetupStatus, runSetup, updateSetting]);

  const clearLogs = () => {
    setLogs([]);
    if (isTauriEnv()) {
      import("@tauri-apps/api/core")
        .then(({ invoke }) => {
          invoke("clear_app_logs").catch(() => {});
        })
        .catch(() => {});
    }
  };

  return {
    isConnected,
    status,
    levelMeter,
    currentAsr,
    asrHistory,
    factHistory,
    geminiResponse,
    vram,
    ram,
    commentaryTimer,
    sessionStarting,
    logs,
    inputDevices,
    discordDevices,
    selectedDevice,
    selectedDiscordDevice,
    enableDiscordCapture,
    windows,
    selectedWindow,
    previewImage,
    settings,
    prompts,
    startSession,
    stopSession,
    restartWhisper,
    updateSetting,
    fetchWindows,
    fetchPreview,
    fetchSettings,
    fetchPrompts,
    savePrompt,
    resetPrompt,
    clearLogs,
    toast,
    showToast,
    modelsStatus,
    missingRequiredModels,
    fetchModelsStatus,
    setupStatus,
    setupProgress,
    isSetupRunning,
    setupError,
    isElevationRequesting,
    fetchSetupStatus,
    runSetup,
    acceptTermsAndRunSetup,
    requestSetupElevation,
    cancelSetup,
  };
}
