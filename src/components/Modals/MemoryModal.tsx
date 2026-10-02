import type React from "react";
import { useState, useEffect } from "react";
import {
  X,
  Database,
  Archive,
  Sparkles,
  Download,
  Plus,
  RefreshCw,
} from "lucide-react";
import { LiveLogTerminal } from "../Console/LiveLogTerminal";
import type { LogEntry } from "../../types";
import { useMigrationStatus } from "../../hooks/useMigrationStatus";
import { useLegacyLanceMemories } from "../../hooks/useLegacyLanceMemories";
import { MigrationStatusBanner } from "./Memory/MigrationStatus";
import { LegacyLancePanel } from "./Memory/LegacyLancePanel";
import { MemoryV2Panel } from "./Memory/MemoryV2Panel";

export interface MemoryModalProps {
  isOpen: boolean;
  onClose: () => void;
  /** Main-window console stream, shared so the modal shows the same logs. */
  logs?: LogEntry[];
  onClearLogs?: () => void;
}

export const MemoryModal: React.FC<MemoryModalProps> = ({
  isOpen,
  onClose,
  logs = [],
  onClearLogs,
}) => {
  const [managerTab, setManagerTab] = useState<"raw" | "semantic">("raw");

  useEffect(() => {
    if (!isOpen && managerTab !== "raw") {
      setManagerTab("raw");
    }
  }, [isOpen, managerTab]);

  const legacyState = useLegacyLanceMemories({
    isOpen,
    isActiveTab: managerTab === "raw",
  });

  const {
    migrationStatus,
    backfillProgress,
    handleProcessAllMemories,
  } = useMigrationStatus({
    isOpen,
    onNotice: legacyState.showNotice,
  });

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex flex-col bg-black/75 backdrop-blur-md animate-fade-in select-none">
      <div className="flex-1 min-h-0 flex items-center justify-center p-4">
        <div className="w-[1060px] h-full max-h-[820px] bg-[#08090a] border border-[#23252a] rounded-[14px] shadow-2xl flex flex-col overflow-hidden text-[#d0d6e0]">
          {/* ヘッダー */}
          <div className="px-5 py-3.5 border-b border-[#23252a] flex items-center justify-between bg-[#161718]">
            <div className="flex items-center gap-2.5">
              <div className="p-1.5 rounded-[6px] bg-[#e4f222]/10 border border-[#e4f222]/20">
                <Database className="w-4 h-4 text-[#e4f222]" />
              </div>
              <div>
                <div className="flex items-center gap-2">
                  <h2 className="text-sm font-semibold text-white tracking-wide">
                    Memory Manager
                  </h2>
                  <span className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-[#e4f222]/10 text-[#e4f222] border border-[#e4f222]/30">
                    LanceDB (Rust Native)
                  </span>
                </div>
                <p className="text-[11px] text-[#8a8f98]">
                  長期記憶の検索、詳細編集、一括更新、選択メモリからの note
                  ブログ自動生成
                </p>
              </div>
            </div>

            <div className="flex items-center gap-2">
              <button
                onClick={legacyState.handleBackup}
                className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs font-medium rounded-[6px] bg-[#1a1b1e] hover:bg-[#23252a] text-[#d0d6e0] border border-[#2e3035] transition-all"
                title="LanceDB のスナップショットバックアップを作成"
              >
                <Archive className="w-3.5 h-3.5 text-[#38bdf8]" />
                <span>Backup</span>
              </button>
              <button
                type="button"
                onClick={() => void handleProcessAllMemories()}
                disabled={
                  legacyState.loading || backfillProgress?.state === "running"
                }
                className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs font-medium rounded-[6px] bg-[#e4f222]/10 hover:bg-[#e4f222]/20 text-[#e4f222] border border-[#e4f222]/30 transition-all disabled:cursor-not-allowed disabled:opacity-40"
                title="全メモリからGemmaで要約・ファクトを作成します。完了済みはスキップします。"
              >
                <Sparkles className="w-3.5 h-3.5" />
                <span>Process all memories</span>
              </button>
              <button
                onClick={legacyState.handleExportJson}
                className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs font-medium rounded-[6px] bg-[#1a1b1e] hover:bg-[#23252a] text-[#d0d6e0] border border-[#2e3035] transition-all"
                title="全件を JSON ファイルに出力"
              >
                <Download className="w-3.5 h-3.5 text-[#4ade80]" />
                <span>Export</span>
              </button>
              <button
                onClick={legacyState.startCreateNew}
                className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-semibold rounded-[6px] bg-[#23252a] hover:bg-[#383b3f] text-white border border-[#383b3f] transition-all"
              >
                <Plus className="w-3.5 h-3.5 text-[#e4f222]" />
                <span>New Memory</span>
              </button>
              <button
                onClick={() => void legacyState.fetchMemories()}
                className="p-1.5 text-[#8a8f98] hover:text-white hover:bg-[#23252a] rounded-[6px] transition-colors"
                title="データを再読込"
              >
                <RefreshCw
                  className={`w-4 h-4 ${legacyState.loading ? "animate-spin" : ""}`}
                />
              </button>
              <button
                onClick={onClose}
                className="p-1.5 text-[#8a8f98] hover:text-white hover:bg-[#23252a] rounded-[6px] transition-colors"
              >
                <X className="w-4 h-4" />
              </button>
            </div>
          </div>

          {/* タブ切り替え */}
          <nav
            aria-label="Memory views"
            className="flex items-center gap-1 border-b border-[#23252a] bg-[#161718] px-5 py-2"
          >
            <button
              type="button"
              aria-selected={managerTab === "raw"}
              onClick={() => {
                legacyState.setContextMenu(null);
                setManagerTab("raw");
              }}
              className={`px-3 py-1 text-xs font-medium rounded ${managerTab === "raw" ? "bg-[#e4f222]/15 text-[#e4f222]" : "text-[#8a8f98] hover:text-white"}`}
            >
              Raw
            </button>
            <button
              type="button"
              aria-selected={managerTab === "semantic"}
              onClick={() => {
                legacyState.setContextMenu(null);
                setManagerTab("semantic");
              }}
              className={`px-3 py-1 text-xs font-medium rounded ${managerTab === "semantic" ? "bg-[#e4f222]/15 text-[#e4f222]" : "text-[#8a8f98] hover:text-white"}`}
            >
              Fact / Summary
            </button>
          </nav>

          {/* バックフィル進行状況バー */}
          <MigrationStatusBanner backfillProgress={backfillProgress} />

          {/* パネルコンテンツ */}
          {managerTab === "semantic" ? (
            <MemoryV2Panel isOpen={isOpen} />
          ) : (
            <LegacyLancePanel
              legacyState={legacyState}
              migrationStatus={migrationStatus}
            />
          )}

          {/* フッター */}
          <div className="px-5 py-2.5 border-t border-[#23252a] flex justify-between items-center bg-[#161718] text-xs text-[#8a8f98]">
            <div className="flex items-center gap-3">
              <span>Total Memories: {legacyState.memories.length}</span>
              {legacyState.selectedIds.length > 0 && (
                <span className="text-[#e4f222] font-semibold">
                  ({legacyState.selectedIds.length} selected)
                </span>
              )}
            </div>
            <button
              onClick={onClose}
              className="px-4 py-1.5 linear-btn-ghost font-medium"
            >
              Close
            </button>
          </div>
        </div>
      </div>
      {/* メイン画面と共通のコンソール（変換ログを含む全動作ログ） */}
      <LiveLogTerminal logs={logs} onClear={onClearLogs ?? (() => {})} />
    </div>
  );
};
