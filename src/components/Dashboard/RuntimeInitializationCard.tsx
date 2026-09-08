import React, { useEffect, useState } from "react";
import {
  AlertTriangle,
  CheckCircle2,
  Circle,
  Loader2,
  RotateCcw,
  Timer,
} from "lucide-react";
import type {
  RuntimeInitializationStage,
  RuntimeInitializationStatus,
} from "../../types";
import "./RuntimeInitializationCard.css";

interface RuntimeInitializationCardProps {
  status: RuntimeInitializationStatus;
  onRetry: () => void;
}

type RuntimeCardVisibility = "visible" | "exiting" | "hidden";

const STAGE_ORDER = ["asr", "embedding", "memory_v2"] as const;

const STAGE_DETAILS: Record<
  (typeof STAGE_ORDER)[number],
  { label: string; detail: string }
> = {
  asr: {
    label: "ASR / Faster-Whisper",
    detail: "音声 WebSocket と Whisper モデル",
  },
  embedding: {
    label: "GLuCoSE-base-ja",
    detail: "日本語埋め込みモデルと実ベクトル生成",
  },
  memory_v2: {
    label: "memory-v2",
    detail: "ジャーナル復旧と LanceDB テーブル",
  },
};

const formatDuration = (milliseconds: number | undefined): string => {
  if (!Number.isFinite(milliseconds) || !milliseconds || milliseconds < 0) {
    return "—";
  }
  const seconds = milliseconds / 1000;
  return seconds >= 60
    ? `${Math.floor(seconds / 60)}分 ${(seconds % 60).toFixed(1)}秒`
    : `${seconds.toFixed(1)}秒`;
};

const stageById = (
  stages: RuntimeInitializationStage[],
  id: string,
): RuntimeInitializationStage | undefined =>
  stages.find((candidate) => candidate.id === id);

const stageLabel = (stage: RuntimeInitializationStage | undefined, id: string) =>
  stage?.label || STAGE_DETAILS[id as (typeof STAGE_ORDER)[number]]?.label || id;

const stageStateLabel = (state: string): string => {
  switch (state) {
    case "completed":
      return "完了";
    case "running":
      return "実行中";
    case "error":
      return "エラー";
    default:
      return "待機中";
  }
};

export const RuntimeInitializationCard: React.FC<
  RuntimeInitializationCardProps
> = ({ status, onRetry }) => {
  const [visibility, setVisibility] = useState<RuntimeCardVisibility>("visible");
  const isRunning = status.status === "running";
  const isComplete = status.status === "completed";
  const hasError = status.status === "error";
  const progress = Math.max(0, Math.min(100, status.progress));

  // Keep the successful state visible long enough to be perceived, then let
  // CSS collapse the card before removing it from the accessibility tree.
  // A retry/error transition cancels the pending dismissal and restores it.
  useEffect(() => {
    if (!isComplete) {
      setVisibility("visible");
      return;
    }

    setVisibility("visible");
    const exitTimer = window.setTimeout(() => {
      setVisibility("exiting");
    }, 900);
    const hiddenTimer = window.setTimeout(() => {
      setVisibility("hidden");
    }, 1_650);

    return () => {
      window.clearTimeout(exitTimer);
      window.clearTimeout(hiddenTimer);
    };
  }, [isComplete]);

  if (visibility === "hidden") return null;

  const isExiting = visibility === "exiting";
  const statusText = isComplete
    ? "すべてのローカルエンジンが利用可能です。"
    : hasError
      ? status.error || "初期化に失敗しました。"
      : status.message || "初期化を開始しています…";

  return (
    <section
      className={`runtime-init-card shrink-0 overflow-hidden rounded-xl border ${
        isComplete
          ? "border-[#27a644]/35 bg-[#0c1710]"
          : hasError
            ? "border-[#eb5757]/35 bg-[#180f10]"
            : "border-[#e4f222]/25 bg-[#11140d]"
      } ${isComplete ? "runtime-init-card--complete" : ""} ${isExiting ? "runtime-init-card--exiting" : ""}`}
      aria-labelledby="runtime-init-title"
      aria-busy={isRunning}
      aria-hidden={isExiting}
      data-visibility={visibility}
    >
      <div className="flex items-start justify-between gap-4 border-b border-white/[0.07] px-4 py-3 sm:px-5">
        <div className="flex min-w-0 items-start gap-3">
          <div
            className={`mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border ${
              isComplete
                ? "border-[#27a644]/35 bg-[#27a644]/10 text-[#27a644]"
                : hasError
                  ? "border-[#eb5757]/35 bg-[#eb5757]/10 text-[#eb5757]"
                  : "border-[#e4f222]/35 bg-[#e4f222]/10 text-[#e4f222]"
            }`}
          >
            {isComplete ? (
              <CheckCircle2 className="h-4 w-4" aria-hidden="true" />
            ) : hasError ? (
              <AlertTriangle className="h-4 w-4" aria-hidden="true" />
            ) : (
              <Loader2
                className={`h-4 w-4 ${isRunning ? "animate-spin" : ""}`}
                aria-hidden="true"
              />
            )}
          </div>
          <div className="min-w-0">
            <p className="text-[10px] font-semibold uppercase tracking-[0.18em] text-[#62666d]">
              Engine bootstrap
            </p>
            <h2 id="runtime-init-title" className="mt-1 truncate text-sm font-semibold text-white">
              ローカルエンジン初期化
            </h2>
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-2 text-right">
          <div>
            <p className="font-mono text-lg font-semibold tabular-nums text-white">
              {Math.round(progress)}%
            </p>
            <p className="text-[10px] text-[#8a8f98]">
              {isComplete ? "準備完了" : hasError ? "要再試行" : "準備中"}
            </p>
          </div>
          {hasError && (
            <button
              type="button"
              onClick={onRetry}
              className="linear-btn-ghost inline-flex items-center gap-1.5 px-2.5 py-1.5 text-[10px]"
            >
              <RotateCcw className="h-3 w-3" aria-hidden="true" />
              再試行
            </button>
          )}
        </div>
      </div>

      <div className="px-4 py-3 sm:px-5">
        <progress
          className={`runtime-init-progress ${isComplete ? "is-complete" : hasError ? "has-error" : ""}`}
          value={progress}
          max={100}
          aria-label="ローカルエンジン初期化の進行状況"
          aria-valuetext={`${Math.round(progress)}%`}
        />
        <p
          className={`mt-2 min-h-4 truncate text-[11px] ${hasError ? "text-[#f2a1a1]" : isComplete ? "text-[#a7d7ae]" : "text-[#b9bf9b]"}`}
          role="status"
          aria-live="polite"
        >
          {statusText}
        </p>

        <div className="mt-3 grid gap-2 md:grid-cols-3">
          {STAGE_ORDER.map((id) => {
            const stage = stageById(status.stages, id);
            const state = stage?.status || "pending";
            const complete = state === "completed";
            const error = state === "error";
            const running = state === "running";
            return (
              <div
                key={id}
                className={`rounded-lg border px-3 py-2 transition-colors ${
                  complete
                    ? "border-[#27a644]/25 bg-[#27a644]/[0.06]"
                    : error
                      ? "border-[#eb5757]/25 bg-[#eb5757]/[0.06]"
                      : running
                        ? "border-[#e4f222]/25 bg-[#e4f222]/[0.06]"
                        : "border-white/[0.07] bg-black/10"
                }`}
              >
                <div className="flex items-center justify-between gap-2">
                  <div className="flex min-w-0 items-center gap-2">
                    {complete ? (
                      <CheckCircle2 className="h-3.5 w-3.5 shrink-0 text-[#27a644]" aria-hidden="true" />
                    ) : error ? (
                      <AlertTriangle className="h-3.5 w-3.5 shrink-0 text-[#eb5757]" aria-hidden="true" />
                    ) : running ? (
                      <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin text-[#e4f222]" aria-hidden="true" />
                    ) : (
                      <Circle className="h-3.5 w-3.5 shrink-0 text-[#62666d]" aria-hidden="true" />
                    )}
                    <span className="truncate text-[11px] font-semibold text-[#d0d6e0]">
                      {stageLabel(stage, id)}
                    </span>
                  </div>
                  <span className={`shrink-0 text-[10px] ${complete ? "text-[#27a644]" : error ? "text-[#eb5757]" : running ? "text-[#e4f222]" : "text-[#62666d]"}`}>
                    {stageStateLabel(state)}
                  </span>
                </div>
                <p className="mt-1 truncate pl-5 text-[10px] text-[#62666d]">
                  {error
                    ? stage?.error || "初期化エラー"
                    : STAGE_DETAILS[id].detail}
                </p>
                {complete && (
                  <p className="mt-1 flex items-center gap-1 pl-5 font-mono text-[10px] tabular-nums text-[#8a8f98]">
                    <Timer className="h-3 w-3" aria-hidden="true" />
                    {formatDuration(stage?.elapsed_ms)}
                  </p>
                )}
              </div>
            );
          })}
        </div>

        {isComplete && (
          <div className="mt-3 flex items-center justify-between gap-3 border-t border-[#27a644]/15 pt-3 text-[10px] text-[#8a8f98]">
            <span>実測初期化時間</span>
            <span className="font-mono font-semibold tabular-nums text-[#a7d7ae]">
              {formatDuration(status.elapsed_ms)}
            </span>
          </div>
        )}
      </div>
    </section>
  );
};
