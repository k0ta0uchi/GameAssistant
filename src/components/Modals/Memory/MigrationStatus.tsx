import type React from "react";
import { Sparkles } from "lucide-react";
import type { MemoryBackfillProgress, MemoryMigrationStatus } from "../../../types";

export interface MigrationStatusProps {
  backfillProgress: MemoryBackfillProgress | null;
}

export const MigrationStatusBanner: React.FC<MigrationStatusProps> = ({
  backfillProgress,
}) => {
  if (!backfillProgress) return null;

  const backfillWarningReasons = Object.entries(
    backfillProgress.reasonCounts || {},
  ).filter(([, count]) => count > 0);

  const backfillHasWarnings = Boolean(
    backfillProgress.failed > 0 ||
      backfillWarningReasons.length > 0 ||
      Boolean(backfillProgress.lastErrorReason) ||
      Boolean(backfillProgress.reason),
  );

  const backfillWarningText =
    backfillWarningReasons.length > 0
      ? backfillWarningReasons
          .map(([reason, count]) => `${reason} (${count})`)
          .join(", ")
      : backfillProgress.reason ||
        backfillProgress.lastErrorReason ||
        "row failures";

  const backfillFatalCode =
    backfillProgress.fatalError?.code ||
    (backfillProgress.state === "error" ? "fatal_error" : null);

  return (
    <div className="border-b border-[#23252a] bg-[#0f1011] px-5 py-2 text-[11px]">
      <div className="flex items-center gap-3">
        <Sparkles
          className={`h-3.5 w-3.5 ${
            backfillProgress.state === "running"
              ? "animate-pulse text-[#e4f222]"
              : backfillProgress.state === "error"
                ? "text-[#f87171]"
                : "text-[#4ade80]"
          }`}
        />
        <span
          className={
            backfillProgress.state === "error"
              ? "text-[#f87171]"
              : "text-[#d0d6e0]"
          }
        >
          {backfillProgress.state === "running"
            ? "Processing all memories"
            : backfillProgress.state === "completed"
              ? backfillHasWarnings
                ? "All memories processed with warnings"
                : "All memories processed"
              : "Fatal error: all-memory processing stopped"}
        </span>
        <span className="font-mono text-[#8a8f98]">
          {backfillProgress.processed} / {backfillProgress.total}
        </span>
        <span className="text-[#62666d]">
          processed {backfillProgress.processed} · persisted{" "}
          {backfillProgress.persisted} · remaining{" "}
          {backfillProgress.remaining ??
            Math.max(
              0,
              backfillProgress.total - backfillProgress.processed,
            )}{" "}
          · queued {backfillProgress.queued} · skipped{" "}
          {backfillProgress.skipped} · failed {backfillProgress.failed} · retries{" "}
          {backfillProgress.retry_count ?? 0}
          {backfillProgress.attempt_id && (
            <> · attempt {backfillProgress.attempt_id}</>
          )}
        </span>
        {backfillProgress.state !== "running" && backfillProgress.final_counts && (
          <span className="truncate text-[#8a8f98]">
            Final rows: processed {backfillProgress.final_counts.processed} · persisted{" "}
            {backfillProgress.final_counts.persisted} · skipped{" "}
            {backfillProgress.final_counts.skipped} · failed{" "}
            {backfillProgress.final_counts.failed} · remaining{" "}
            {backfillProgress.final_counts.remaining}
          </span>
        )}
        {backfillHasWarnings && (
          <span className="truncate text-[#e4f222]">
            Warnings: {backfillWarningText}
          </span>
        )}
        {backfillFatalCode && (
          <span className="truncate text-[#f87171]">
            Fatal: {backfillFatalCode}
          </span>
        )}
      </div>
      <div className="mt-1.5 h-1 overflow-hidden rounded-full bg-[#23252a]">
        <div
          className={`h-full transition-[width] duration-300 ${
            backfillProgress.state === "error" ? "bg-[#f87171]" : "bg-[#e4f222]"
          }`}
          style={{
            width: `${
              backfillProgress.total > 0
                ? Math.min(
                    100,
                    Math.round(
                      (backfillProgress.processed / backfillProgress.total) * 100,
                    ),
                  )
                : backfillProgress.state === "completed"
                  ? 100
                  : 4
            }%`,
          }}
        />
      </div>
    </div>
  );
};

export interface LanceMigrationProgressProps {
  migrationStatus: MemoryMigrationStatus | null;
}

export const LanceMigrationProgress: React.FC<LanceMigrationProgressProps> = ({
  migrationStatus,
}) => {
  if (!migrationStatus) return null;

  const migrationPercent =
    migrationStatus.total && migrationStatus.total > 0
      ? migrationStatus.status === "completed"
        ? 100
        : Math.min(
            99,
            Math.floor((migrationStatus.processed / migrationStatus.total) * 100),
          )
      : null;

  if (migrationStatus.status !== "running") return null;

  return (
    <div className="w-full max-w-[360px] space-y-1.5">
      <div className="flex items-center justify-between text-[10px] font-mono text-[#8a8f98]">
        <span>
          {migrationStatus.processed.toLocaleString()} /{" "}
          {migrationStatus.total?.toLocaleString() ?? "?"} records
        </span>
        <span>{migrationPercent === null ? "..." : `${migrationPercent}%`}</span>
      </div>
      <div className="h-1.5 w-full overflow-hidden rounded-full bg-[#23252a]">
        <div
          className="h-full rounded-full bg-[#e4f222] transition-[width] duration-300"
          style={{
            width: migrationPercent === null ? "4%" : `${migrationPercent}%`,
          }}
        />
      </div>
      <p className="text-center text-[10px] text-[#62666d]">
        初回のみ実行されます。アプリを終了しないでください。
      </p>
    </div>
  );
};
