import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  type MemoryMigrationStatus,
  type MemoryBackfillProgress,
  normalizeMemoryBackfillProgress,
  normalizeMemoryMigrationStatus,
} from "../types";
import {
  getLanceMigrationStatusApi,
  memoryManagerProcessAllApi,
} from "../services/memoryService";

export interface UseMigrationStatusOptions {
  isOpen: boolean;
  onNotice?: (text: string, type: "success" | "error") => void;
}

export interface UseMigrationStatusResult {
  migrationStatus: MemoryMigrationStatus | null;
  backfillProgress: MemoryBackfillProgress | null;
  handleProcessAllMemories: () => Promise<void>;
  refreshMigrationStatus: () => Promise<void>;
}

export function useMigrationStatus({
  isOpen,
  onNotice,
}: UseMigrationStatusOptions): UseMigrationStatusResult {
  const [migrationStatus, setMigrationStatus] =
    useState<MemoryMigrationStatus | null>(null);
  const [backfillProgress, setBackfillProgress] =
    useState<MemoryBackfillProgress | null>(null);

  // Background backfill progress event subscription
  useEffect(() => {
    if (!isOpen) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen(
      "memory-manager-backfill-progress",
      (event: { payload: unknown }) => {
        if (disposed) return;
        const progress = normalizeMemoryBackfillProgress(event.payload);
        if (progress) setBackfillProgress(progress);
      },
    )
      .then((dispose) => {
        if (disposed) dispose();
        else unlisten = dispose;
      })
      .catch(() => {
        // Older portable builds do not expose this advisory event.
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [isOpen]);

  // Migration status event subscription and low-frequency fallback polling
  const pollTimerRef = useRef<number | null>(null);

  const stopFallbackPoll = useCallback(() => {
    if (pollTimerRef.current !== null) {
      window.clearInterval(pollTimerRef.current);
      pollTimerRef.current = null;
    }
  }, []);

  const applyStatus = useCallback(
    (status: MemoryMigrationStatus | null) => {
      if (status) {
        setMigrationStatus(status);
        if (status.status === "completed" || status.status === "error") {
          stopFallbackPoll();
        }
      }
    },
    [stopFallbackPoll],
  );

  const refreshMigrationStatus = useCallback(async () => {
    try {
      const status = await getLanceMigrationStatusApi();
      applyStatus(status);
    } catch {
      // Advisory command unavailable
    }
  }, [applyStatus]);

  useEffect(() => {
    if (!isOpen) {
      setMigrationStatus(null);
      stopFallbackPoll();
      return;
    }

    let disposed = false;
    let unlisten: (() => void) | undefined;

    // 1. Initial snapshot on modal open
    void refreshMigrationStatus();

    // 2. Primary event-driven progress updates
    void listen(
      "memory-migration-progress",
      (event: { payload: unknown }) => {
        if (disposed) return;
        const status = normalizeMemoryMigrationStatus(event.payload);
        applyStatus(status);
      },
    )
      .then((dispose) => {
        if (disposed) {
          dispose();
        } else {
          unlisten = dispose;
          // 3. Post-listen refresh to eliminate listener registration race
          void refreshMigrationStatus();
        }
      })
      .catch(() => {
        // Advisory event unavailable
      });

    // 4. Low-frequency fallback polling (3000ms)
    pollTimerRef.current = window.setInterval(refreshMigrationStatus, 3000);

    return () => {
      disposed = true;
      unlisten?.();
      stopFallbackPoll();
    };
  }, [isOpen, applyStatus, refreshMigrationStatus, stopFallbackPoll]);

  const handleProcessAllMemories = useCallback(async () => {
    if (backfillProgress?.state === "running") return;
    onNotice?.("全メモリのファクト・要約を開始しています...", "success");
    try {
      const { progress } = await memoryManagerProcessAllApi();
      setBackfillProgress(progress);
    } catch (e) {
      onNotice?.(`ファクト・要約処理を開始できませんでした: ${e}`, "error");
    }
  }, [backfillProgress?.state, onNotice]);

  return {
    migrationStatus,
    backfillProgress,
    handleProcessAllMemories,
    refreshMigrationStatus,
  };
}
