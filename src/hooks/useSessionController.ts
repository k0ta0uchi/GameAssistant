import { useState, useCallback, useRef } from "react";
import {
  type SystemStatus,
  type ResourceInfo,
  type AsrEntry,
  type FactEntry,
  type SetupStatus,
} from "../types";
import { isSetupReadyForSession } from "./useSetupState";
import {
  startSessionApi,
  stopSessionApi,
  restartWhisperApi,
} from "../services/backendAdapter";

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

export interface UseSessionControllerOptions {
  fetchSetupStatus: () => Promise<SetupStatus | null>;
  onWhisperRestarted?: () => void;
  showToast: (message: string, type?: "success" | "info" | "warning") => void;
}

export interface UseSessionControllerResult {
  status: SystemStatus;
  sessionStarting: boolean;
  currentAsr: {
    text: string;
    isFinal: boolean;
    isPrompt?: boolean;
    latencyMs?: number | null;
  };
  asrHistory: AsrEntry[];
  factHistory: FactEntry[];
  geminiResponse: string;
  vram: ResourceInfo;
  ram: ResourceInfo;
  commentaryTimer: {
    progress: number;
    remaining: number;
  };
  levelMeter: number;
  discordLevelMeter: number;
  startSession: () => Promise<void>;
  stopSession: () => Promise<void>;
  restartWhisper: () => Promise<void>;
  setStatus: React.Dispatch<React.SetStateAction<SystemStatus>>;
  setSessionStarting: React.Dispatch<React.SetStateAction<boolean>>;
  setCurrentAsr: React.Dispatch<
    React.SetStateAction<{
      text: string;
      isFinal: boolean;
      isPrompt?: boolean;
      latencyMs?: number | null;
    }>
  >;
  setAsrHistory: React.Dispatch<React.SetStateAction<AsrEntry[]>>;
  setFactHistory: React.Dispatch<React.SetStateAction<FactEntry[]>>;
  setGeminiResponse: React.Dispatch<React.SetStateAction<string>>;
  setVram: React.Dispatch<React.SetStateAction<ResourceInfo>>;
  setRam: React.Dispatch<React.SetStateAction<ResourceInfo>>;
  setCommentaryTimer: React.Dispatch<
    React.SetStateAction<{
      progress: number;
      remaining: number;
    }>
  >;
  setLevelMeter: React.Dispatch<React.SetStateAction<number>>;
  setDiscordLevelMeter: React.Dispatch<React.SetStateAction<number>>;
  sessionStartInFlightRef: React.MutableRefObject<boolean>;
}

export function useSessionController(
  options: UseSessionControllerOptions,
): UseSessionControllerResult {
  const { fetchSetupStatus, onWhisperRestarted, showToast } = options;

  const [status, setStatus] = useState<SystemStatus>({
    asr: false,
    gemini: false,
    tts: false,
    twitch: false,
    session: false,
  });
  const [sessionStarting, setSessionStarting] = useState<boolean>(false);
  const sessionStartInFlightRef = useRef<boolean>(false);

  // 音声レベル
  const [levelMeter, setLevelMeter] = useState<number>(0);
  const [discordLevelMeter, setDiscordLevelMeter] = useState<number>(0);

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

  // Durable facts derived from live speech
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

  const startSession = useCallback(async () => {
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
      const res = await startSessionApi();
      if (res.success) {
        setStatus((prev) => ({ ...prev, session: true }));
      } else {
        setStatus((prev) => ({ ...prev, session: false }));
        if (res.error) {
          showToast(`セッションを開始できませんでした: ${res.error}`, "warning");
        }
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
  }, [fetchSetupStatus, showToast]);

  const stopSession = useCallback(async () => {
    if (sessionStartInFlightRef.current) return;
    setStatus((prev) => ({
      ...prev,
      session: false,
      gemini: false,
      tts: false,
    }));
    try {
      await stopSessionApi();
    } catch (e) {
      console.error("Failed to stop session:", e);
      const message = e instanceof Error ? e.message : String(e);
      showToast(`セッション停止に失敗しました: ${message}`, "warning");
    }
  }, [showToast]);

  const restartWhisper = useCallback(async () => {
    try {
      showToast("🔄 Whisper エンジンを再起動しています...", "info");
      await restartWhisperApi();
      onWhisperRestarted?.();
      showToast(
        "✅ Whisper エンジンの再起動とウォームアップが完了しました！",
        "success",
      );
    } catch (e) {
      console.error("Failed to restart whisper:", e);
      showToast(`Whisper 再起動エラー: ${e}`, "warning");
    }
  }, [onWhisperRestarted, showToast]);

  return {
    status,
    sessionStarting,
    currentAsr,
    asrHistory,
    factHistory,
    geminiResponse,
    vram,
    ram,
    commentaryTimer,
    levelMeter,
    discordLevelMeter,
    startSession,
    stopSession,
    restartWhisper,
    setStatus,
    setSessionStarting,
    setCurrentAsr,
    setAsrHistory,
    setFactHistory,
    setGeminiResponse,
    setVram,
    setRam,
    setCommentaryTimer,
    setLevelMeter,
    setDiscordLevelMeter,
    sessionStartInFlightRef,
  };
}
