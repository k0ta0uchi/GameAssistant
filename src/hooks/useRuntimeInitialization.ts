import { useState, useEffect, useCallback, useRef } from "react";
import type {
  RuntimeInitializationStatus,
  RuntimeInitializationStage,
  SetupStatus,
} from "../types";
import {
  getRuntimeInitializationStatusApi,
  initializeRuntimeApi,
  isTauriEnv,
} from "../services/backendAdapter";

export const idleRuntimeInitializationStatus = (): RuntimeInitializationStatus => ({
  status: "idle",
  progress: 0,
  current_stage: null,
  message: "初期化を開始する準備をしています。",
  elapsed_ms: 0,
  started_at: null,
  completed_at: null,
  stages: [
    {
      id: "asr",
      label: "ASR / Faster-Whisper",
      status: "pending",
      progress: 0,
    },
    {
      id: "embedding",
      label: "GLuCoSE-base-ja",
      status: "pending",
      progress: 0,
    },
    { id: "memory_v2", label: "memory-v2", status: "pending", progress: 0 },
  ],
  asr_ready: false,
  embedding_ready: false,
  memory_v2_ready: false,
  error: null,
});

export const normalizeRuntimeInitializationStatus = (
  value: unknown,
): RuntimeInitializationStatus | null => {
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;

  const status = record.status;
  if (
    status !== "idle" &&
    status !== "running" &&
    status !== "completed" &&
    status !== "error"
  ) {
    return null;
  }

  const progress =
    typeof record.progress === "number" && Number.isFinite(record.progress)
      ? Math.max(0, Math.min(100, record.progress))
      : 0;

  const currentStage =
    record.current_stage === "asr" ||
    record.current_stage === "embedding" ||
    record.current_stage === "memory_v2"
      ? record.current_stage
      : null;

  const message =
    typeof record.message === "string" ? record.message : "";

  const elapsedMs =
    typeof record.elapsed_ms === "number" && Number.isFinite(record.elapsed_ms)
      ? Math.max(0, record.elapsed_ms)
      : 0;

  const optionalString = (key: string): string | null =>
    typeof record[key] === "string" && record[key] ? String(record[key]) : null;

  const stages: RuntimeInitializationStage[] = Array.isArray(record.stages)
    ? record.stages
        .map((stage): RuntimeInitializationStage | null => {
          if (!stage || typeof stage !== "object") return null;
          const s = stage as Record<string, unknown>;
          const id =
            s.id === "asr" || s.id === "embedding" || s.id === "memory_v2"
              ? s.id
              : null;
          const stageStatus =
            s.status === "pending" ||
            s.status === "running" ||
            s.status === "completed" ||
            s.status === "error"
              ? s.status
              : null;
          if (!id || !stageStatus) return null;
          return {
            id,
            label: typeof s.label === "string" ? s.label : id,
            status: stageStatus,
            progress:
              typeof s.progress === "number" && Number.isFinite(s.progress)
                ? Math.max(0, Math.min(100, s.progress))
                : 0,
            elapsed_ms:
              typeof s.elapsed_ms === "number" && Number.isFinite(s.elapsed_ms)
                ? Math.max(0, s.elapsed_ms)
                : undefined,
            error:
              typeof s.error === "string" && s.error ? s.error : undefined,
          };
        })
        .filter((s): s is RuntimeInitializationStage => s !== null)
    : [];

  return {
    status,
    progress,
    current_stage: currentStage,
    message,
    elapsed_ms: elapsedMs,
    started_at: optionalString("started_at"),
    completed_at: optionalString("completed_at"),
    stages: stages.length > 0 ? stages : idleRuntimeInitializationStatus().stages,
    asr_ready: Boolean(record.asr_ready),
    embedding_ready: Boolean(record.embedding_ready),
    memory_v2_ready: Boolean(record.memory_v2_ready),
    error: optionalString("error"),
  };
};

export interface UseRuntimeInitializationOptions {
  setupStatus: SetupStatus | null;
  isReadyForMainUi: (setup: SetupStatus | null) => boolean;
  onInitializationCompleted?: () => void;
  showToast?: (message: string, type: "success" | "info" | "warning") => void;
}

export interface UseRuntimeInitializationResult {
  runtimeInitialization: RuntimeInitializationStatus;
  initializeRuntime: () => Promise<RuntimeInitializationStatus | null>;
  fetchRuntimeInitializationStatus: () => Promise<RuntimeInitializationStatus | null>;
  setRuntimeInitialization: React.Dispatch<
    React.SetStateAction<RuntimeInitializationStatus>
  >;
}

export function useRuntimeInitialization({
  setupStatus,
  isReadyForMainUi,
  onInitializationCompleted,
  showToast,
}: UseRuntimeInitializationOptions): UseRuntimeInitializationResult {
  const [runtimeInitialization, setRuntimeInitialization] =
    useState<RuntimeInitializationStatus>(idleRuntimeInitializationStatus);
  const runtimeInitializationRequestedRef = useRef(false);
  const runtimeInitializationPollRef = useRef<number | null>(null);

  /** Read the measured live engine-initialization snapshot from native code. */
  const fetchRuntimeInitializationStatus = useCallback(async () => {
    if (!isTauriEnv()) return null;
    try {
      const raw = await getRuntimeInitializationStatusApi();
      const normalized = normalizeRuntimeInitializationStatus(raw);
      if (normalized) setRuntimeInitialization(normalized);
      return normalized;
    } catch (error) {
      console.warn("Runtime initialization status unavailable:", error);
      return null;
    }
  }, []);

  /** Start the one-shot ASR → GLuCoSE → memory-v2 initialization pipeline. */
  const initializeRuntime = useCallback(async () => {
    if (!isTauriEnv()) return null;
    setRuntimeInitialization((previous) => {
      if (previous.status === "completed") return previous;
      const next = idleRuntimeInitializationStatus();
      next.status = "running";
      next.message = "ASR、GLuCoSE、memory-v2を初期化しています。";
      return next;
    });
    try {
      const raw = await initializeRuntimeApi();
      const normalized = normalizeRuntimeInitializationStatus(raw);
      if (!normalized) {
        throw new Error("初期化結果を読み取れませんでした。");
      }
      setRuntimeInitialization(normalized);
      if (normalized.status === "completed") {
        onInitializationCompleted?.();
      }
      return normalized;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setRuntimeInitialization((previous) => ({
        ...previous,
        status: "error",
        message: "エンジン初期化に失敗しました。",
        error: message,
      }));
      showToast?.(`エンジン初期化エラー: ${message}`, "warning");
      return null;
    }
  }, [onInitializationCompleted, showToast]);

  // Mounting the main screen is the explicit initialization boundary.
  useEffect(() => {
    if (!isTauriEnv() || !isReadyForMainUi(setupStatus)) return;
    if (
      runtimeInitializationRequestedRef.current ||
      runtimeInitialization.status !== "idle"
    )
      return;
    runtimeInitializationRequestedRef.current = true;
    void initializeRuntime().then((result) => {
      if (!result || result.status === "error") {
        runtimeInitializationRequestedRef.current = false;
      }
    });
  }, [initializeRuntime, isReadyForMainUi, runtimeInitialization.status, setupStatus]);

  // Events are the primary channel for runtime initialization progress.
  // Low-frequency fallback polling (3000ms) only runs while actively in-flight.
  useEffect(() => {
    if (
      !isTauriEnv() ||
      !isReadyForMainUi(setupStatus) ||
      runtimeInitialization.status === "completed" ||
      runtimeInitialization.status === "error"
    ) {
      if (runtimeInitializationPollRef.current !== null) {
        window.clearInterval(runtimeInitializationPollRef.current);
        runtimeInitializationPollRef.current = null;
      }
      return;
    }

    // Always fetch latest snapshot upon readiness transition
    void fetchRuntimeInitializationStatus();

    // Only run fallback polling while actively in-flight
    if (runtimeInitialization.status !== "running") {
      if (runtimeInitializationPollRef.current !== null) {
        window.clearInterval(runtimeInitializationPollRef.current);
        runtimeInitializationPollRef.current = null;
      }
      return;
    }

    const timer = window.setInterval(() => {
      void fetchRuntimeInitializationStatus();
    }, 3000);
    runtimeInitializationPollRef.current = timer;
    return () => {
      window.clearInterval(timer);
      if (runtimeInitializationPollRef.current === timer) {
        runtimeInitializationPollRef.current = null;
      }
    };
  }, [
    fetchRuntimeInitializationStatus,
    isReadyForMainUi,
    runtimeInitialization.status,
    setupStatus,
  ]);

  return {
    runtimeInitialization,
    initializeRuntime,
    fetchRuntimeInitializationStatus,
    setRuntimeInitialization,
  };
}
