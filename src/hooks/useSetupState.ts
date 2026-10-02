import { useState, useEffect, useCallback, useRef } from "react";
import {
  type SetupProgress,
  type SetupStatus,
  type SetupStepStatus,
  type DownloadProgressEvent,
  type RuntimeStatus,
  RUNTIME_STATUS_VALUES,
  GEMMA_MODEL_ID,
  GEMMA_TERMS_VERSION,
  GEMMA_TERMS_MODEL_SHA256,
  GEMMA_TERMS_SOURCE,
  hasValidGemmaTerms,
} from "../types";
import {
  getSetupStatusApi,
  runSetupApi,
  cancelSetupApi,
  requestSetupElevationApi,
  isTauriEnv,
} from "../services/backendAdapter";

/** Download events report a literal percentage (1 means 1%), never a ratio. */
export const normalizeDownloadPercent = (value: unknown): number => {
  if (typeof value !== "number" || !Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(100, value));
};

/** Setup/model progress payloads use literal percentages (1 means 1%). */
export const isExpectedGemmaDownload = (modelId: unknown): modelId is string =>
  modelId === GEMMA_MODEL_ID;

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

const normalizeProgress = (value: unknown): number => {
  if (typeof value !== "number" || !Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(100, value));
};

const isRuntimeStatus = (value: unknown): value is RuntimeStatus =>
  typeof value === "string" &&
  RUNTIME_STATUS_VALUES.some((status) => status === value);

export const pendingSetupStatus = (): SetupStatus => ({
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

export const setupStagesMatch = (
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

export const normalizeSetupStep = (value: unknown): SetupStepStatus | null => {
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

export const resolveSetupRunning = (
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
        (st): st is string => typeof st === "string",
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

export const isCanonicalSetupShape = (setup: SetupStatus): boolean => {
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
 * snapshot has arrived. Keeping the unknown state separate from the
 * fail-closed setup screen prevents a ready portable install from briefly
 * flashing the first-run setup view while the IPC request is in flight.
 */
export const shouldShowMainUiForSession = (
  tauri: boolean,
  setup: Parameters<typeof shouldBlockAppUntilSetupReady>[1],
): boolean => !tauri || isPortableRuntimeReadyForMainUi(setup);

/**
 * First-run setup and live engine initialization are separate gates. Once
 * Python/dependencies/models/Gemma have been verified, the main screen must
 * mount so it can show the real ASR/GLuCoSE/memory-v2 initialization progress.
 * The session action still uses the stricter `isSetupReadyForSession` gate,
 * which includes the live ASR WebSocket bit.
 */
export const isPortableRuntimeReadyForMainUi = (
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
      hasValidGemmaTerms(setup),
  );

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

export interface UseSetupStateOptions {
  onModelsNeedRefresh?: () => void;
  updateSetting?: (
    key: string,
    value: any,
    options?: { throwOnError?: boolean },
  ) => Promise<void>;
  fetchSettings?: () => Promise<Record<string, unknown> | null>;
}

export interface UseSetupStateResult {
  setupStatus: SetupStatus | null;
  setupProgress: SetupProgress | null;
  isSetupRunning: boolean;
  setupError: string | null;
  isElevationRequesting: boolean;
  fetchSetupStatus: () => Promise<SetupStatus | null>;
  runSetup: () => Promise<SetupStatus | null>;
  cancelSetup: () => Promise<void>;
  requestSetupElevation: () => Promise<boolean>;
  acceptTermsAndRunSetup: () => Promise<SetupStatus | null>;
  handleAsrReady: () => void;
  handleAsrWarmupFailed: (message?: string) => void;
  setSetupStatus: React.Dispatch<React.SetStateAction<SetupStatus | null>>;
  setSetupProgress: React.Dispatch<React.SetStateAction<SetupProgress | null>>;
  setIsSetupRunning: React.Dispatch<React.SetStateAction<boolean>>;
  setSetupError: React.Dispatch<React.SetStateAction<string | null>>;
  setupStatusRef: React.MutableRefObject<SetupStatus | null>;
  setupRunInFlightRef: React.MutableRefObject<Promise<SetupStatus | null> | null>;
  asrWarmupErrorRef: React.MutableRefObject<string | null>;
}

export function useSetupState(
  options: UseSetupStateOptions = {},
): UseSetupStateResult {
  const { onModelsNeedRefresh, updateSetting, fetchSettings } = options;

  const [setupStatus, setSetupStatus] = useState<SetupStatus | null>(null);
  const [setupProgress, setSetupProgress] = useState<SetupProgress | null>(null);
  const [isSetupRunning, setIsSetupRunning] = useState<boolean>(false);
  const [setupError, setSetupError] = useState<string | null>(null);
  const [isElevationRequesting, setIsElevationRequesting] =
    useState<boolean>(false);

  const setupEventRef = useRef<SetupProgress | null>(null);
  const cancelRequestedRef = useRef(false);
  const setupDownloadActiveRef = useRef(false);
  const setupRunInFlightRef = useRef<Promise<SetupStatus | null> | null>(null);
  const setupStatusRef = useRef<SetupStatus | null>(null);
  const setupAttemptErrorRef = useRef<string | null>(null);
  const asrWarmupErrorRef = useRef<string | null>(null);
  const termsAcceptanceInFlightRef = useRef<Promise<SetupStatus | null> | null>(
    null,
  );

  useEffect(() => {
    setupStatusRef.current = setupStatus;
  }, [setupStatus]);

  /** Read the canonical setup manager state over the active transport. */
  const fetchSetupStatus = useCallback(async (): Promise<SetupStatus | null> => {
    try {
      const rawStatus = await getSetupStatusApi();
      const normalized = normalizeSetupStatus(rawStatus);
      if (!normalized) {
        const unavailable: SetupStatus = {
          ...pendingSetupStatus(),
          error: "セットアップ状態を読み取れませんでした。",
        };
        setupStatusRef.current = unavailable;
        setSetupStatus(unavailable);
        setSetupError(unavailable.error || null);
        return unavailable;
      }

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
      const unavailable: SetupStatus = {
        ...pendingSetupStatus(),
        error: String(error),
      };
      setupStatusRef.current = unavailable;
      setSetupStatus(unavailable);
      setSetupError(unavailable.error || null);
      return unavailable;
    }
  }, []);

  // Tauri setup events subscription
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
      if (disposed) return;
      const payload = event.payload;
      if (payload?.status === "completed") {
        onModelsNeedRefresh?.();
      }
      if (!setupDownloadActiveRef.current) return;
      if (!payload || !isExpectedGemmaDownload(payload.model_id)) return;
      const status =
        payload.status === "completed" ? "completed" : payload.status;
      const percent = normalizeDownloadPercent(payload.percent);
      const progress: SetupProgress = {
        stage: "models",
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
      const record = asRecord(event.payload);
      if (!record || typeof record.stage !== "string") return;
      const progress: SetupProgress = {
        stage: record.stage,
        status: firstString(record.status, record.state) || "pending",
        progress: normalizeProgress(record.progress),
        current:
          typeof record.current === "number" && Number.isFinite(record.current)
            ? record.current
            : undefined,
        total:
          typeof record.total === "number" && Number.isFinite(record.total)
            ? record.total
            : undefined,
        message: firstString(record.message, record.detail) || null,
        error: firstString(record.error, record.error_message) || null,
      };
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
      if (cancellationRequested && progress.status !== "cancelled") return;
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

      if (progress.status === "running" || progress.status === "downloading") {
        setIsSetupRunning(true);
      } else if (
        progress.status === "error" ||
        progress.status === "cancelled"
      ) {
        setIsSetupRunning(false);
      } else if (progress.status === "completed") {
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
          message: progressWithMessage.message,
          error: progressWithMessage.error || null,
          stages: previous?.stages,
        };
      });
    });

    return () => {
      if (disposed) return;
      disposed = true;
      for (const unlisten of unlistenFns) unlisten();
    };
  }, [onModelsNeedRefresh]);

  // Polling while setup is required or actively running
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

  const handleAsrReady = useCallback(() => {
    const previousWarmupError = asrWarmupErrorRef.current;
    asrWarmupErrorRef.current = null;
    const previous = setupStatusRef.current;
    if (!previous) return;
    const runtimeReadyWithoutAsr =
      previous.required_models_ready === true &&
      previous.dependency_ready === true &&
      previous.python_import_ready === true &&
      previous.tokenizer_ready === true &&
      previous.embedding_ready === true &&
      hasValidGemmaTerms(previous) &&
      (!previous.error || previous.error === previousWarmupError);
    const updated: SetupStatus = {
      ...previous,
      ready: runtimeReadyWithoutAsr,
      setup_required: runtimeReadyWithoutAsr ? false : previous.setup_required,
      running: false,
      cancelled: false,
      status: runtimeReadyWithoutAsr ? ("ready" as const) : previous.status,
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
  }, []);

  const handleAsrWarmupFailed = useCallback((message?: string) => {
    const errorMsg =
      message || "ASR WebSocketの準備に失敗しました。再試行してください。";
    asrWarmupErrorRef.current = errorMsg;
    setSetupStatus((previous) => {
      if (!previous) return previous;
      const updated: SetupStatus = {
        ...previous,
        ready: false,
        setup_required: true,
        status: "error" as const,
        asr_websocket_ready: false,
        error: errorMsg,
        running: false,
        cancelled: false,
      };
      setupStatusRef.current = updated;
      return updated;
    });
    setSetupError(errorMsg);
  }, []);

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

    asrWarmupErrorRef.current = null;
    cancelRequestedRef.current = false;
    setupEventRef.current = null;
    setupAttemptErrorRef.current = null;
    setupDownloadActiveRef.current = true;
    setIsSetupRunning(true);
    setSetupError(null);
    const pending: SetupStatus = {
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
        const result = await runSetupApi();
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
        const failed: SetupStatus = {
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
        onModelsNeedRefresh?.();
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
  }, [fetchSetupStatus, onModelsNeedRefresh]);

  const requestSetupElevation = useCallback(async (): Promise<boolean> => {
    if (!isTauriEnv()) return false;
    if (setupRunInFlightRef.current) return false;
    setIsElevationRequesting(true);
    setSetupError(null);
    try {
      const raw = await requestSetupElevationApi();
      const result = raw as { launched?: boolean; message?: string } | undefined;
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
      await cancelSetupApi();
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

  const acceptTermsAndRunSetup = useCallback(async (): Promise<SetupStatus | null> => {
    if (!isTauriEnv() || !updateSetting || !fetchSettings) return null;
    if (setupRunInFlightRef.current || termsAcceptanceInFlightRef.current)
      return null;

    setIsSetupRunning(true);
    let operation: Promise<SetupStatus | null> = Promise.resolve(null);
    operation = (async (): Promise<SetupStatus | null> => {
      try {
        await updateSetting("gemma_terms_accepted", true, {
          throwOnError: true,
        });
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

  // Initial status read on mount
  useEffect(() => {
    void fetchSetupStatus();
  }, [fetchSetupStatus]);

  return {
    setupStatus,
    setupProgress,
    isSetupRunning,
    setupError,
    isElevationRequesting,
    fetchSetupStatus,
    runSetup,
    cancelSetup,
    requestSetupElevation,
    acceptTermsAndRunSetup,
    handleAsrReady,
    handleAsrWarmupFailed,
    setSetupStatus,
    setSetupProgress,
    setIsSetupRunning,
    setSetupError,
    setupStatusRef,
    setupRunInFlightRef,
    asrWarmupErrorRef,
  };
}
