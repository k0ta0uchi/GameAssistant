import type React from "react";
import {
  Search,
  CheckSquare,
  Square,
  ArrowUpDown,
  Loader2,
  Database,
} from "lucide-react";
import type { MemoryMigrationStatus } from "../../../types";
import { MemoryEditor } from "./MemoryEditor";
import { MemoryContextMenu } from "./MemoryContextMenu";
import type { useLegacyLanceMemories } from "../../../hooks/useLegacyLanceMemories";

export interface LegacyLancePanelProps {
  legacyState: ReturnType<typeof useLegacyLanceMemories>;
  migrationStatus: MemoryMigrationStatus | null;
}

const getTypeColor = (type?: string) => {
  switch (type) {
    case "twitch_chat":
      return "bg-[#8b5cf6]/15 text-[#a78bfa] border-[#8b5cf6]/30";
    case "ai_response":
      return "bg-[#27a644]/15 text-[#4ade80] border-[#27a644]/30";
    case "auto_commentary":
      return "bg-[#e4f222]/15 text-[#e4f222] border-[#e4f222]/30";
    case "user_speech":
    case "user_prompt":
      return "bg-[#02b8cc]/15 text-[#38bdf8] border-[#02b8cc]/30";
    default:
      return "bg-[#23252a] text-[#8a8f98] border-[#383b3f]";
  }
};

export const LegacyLancePanel: React.FC<LegacyLancePanelProps> = ({
  legacyState,
  migrationStatus,
}) => {
  const {
    memories,
    loading,
    searchQuery,
    setSearchQuery,
    selectedIds,
    activeItem,
    contextMenu,
    setContextMenu,
    rawContextActions,
    editKey,
    setEditKey,
    editType,
    setEditType,
    editUser,
    setEditUser,
    editContent,
    setEditContent,
    isCreatingNew,
    bulkType,
    setBulkType,
    bulkUser,
    setBulkUser,
    actionMessage,
    showNotice,
    isGeneratingBlog,
    blogResult,
    setBlogResult,
    filteredAndSortedMemories,
    isBulkMode,
    fetchMemories,
    handleSort,
    handleRowClick,
    handleToggleSelectId,
    handleSelectAll,
    handleSaveSingle,
    handleDeleteSelected,
    handleBulkUpdate,
    handleGenerateBlog,
    copySelectedAsJson,
    openRawContextMenu,
  } = legacyState;

  const migrationPercent =
    migrationStatus?.total && migrationStatus.total > 0
      ? migrationStatus.status === "completed"
        ? 100
        : Math.min(
            99,
            Math.floor(
              (migrationStatus.processed / migrationStatus.total) * 100,
            ),
          )
      : null;

  const migrationFinishing =
    migrationStatus?.status === "running" &&
    migrationStatus.total !== null &&
    migrationStatus.total !== undefined &&
    migrationStatus.processed >= migrationStatus.total;

  return (
    <>
      {/* ツールバー & 検索 */}
      <div className="px-5 py-2.5 border-b border-[#23252a] bg-[#0f1011] flex items-center justify-between gap-4">
        <div className="relative flex-1 max-w-[420px]">
          <Search className="w-3.5 h-3.5 text-[#8a8f98] absolute left-3 top-2.5 pointer-events-none" />
          <input
            type="text"
            placeholder="Search by Key, Content, User, Type..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="w-full text-xs font-sans linear-input pl-8 pr-3 py-1.5 bg-[#08090a] text-[#d0d6e0]"
          />
        </div>

        <div className="flex items-center gap-3 text-xs">
          <button
            onClick={handleSelectAll}
            className="flex items-center gap-1.5 text-[#8a8f98] hover:text-white transition-colors"
          >
            {selectedIds.length > 0 &&
            selectedIds.length === filteredAndSortedMemories.length ? (
              <CheckSquare className="w-3.5 h-3.5 text-[#e4f222]" />
            ) : (
              <Square className="w-3.5 h-3.5" />
            )}
            <span>Select All</span>
          </button>

          <span className="text-[#383b3f]">|</span>

          <span className="font-mono text-[11px] text-[#8a8f98]">
            Selected:{" "}
            <strong className="text-[#e4f222] font-semibold">
              {selectedIds.length}
            </strong>{" "}
            / {filteredAndSortedMemories.length}
          </span>

          {actionMessage && (
            <span
              className={`text-[11px] font-medium px-2 py-0.5 rounded border animate-fade-in ${
                actionMessage.type === "success"
                  ? "bg-[#27a644]/15 text-[#4ade80] border-[#27a644]/30"
                  : "bg-[#eb5757]/15 text-[#f87171] border-[#eb5757]/30"
              }`}
            >
              {actionMessage.text}
            </span>
          )}
        </div>
      </div>

      {/* メイン 2 ペインコンテンツ */}
      <div className="flex-1 flex overflow-hidden">
        {/* 左ペイン: テーブルリスト */}
        <div className="flex-1 flex flex-col border-r border-[#23252a] overflow-hidden bg-[#08090a]">
          {/* テーブルカラムヘッダー */}
          <div className="grid grid-cols-[36px_140px_100px_100px_100px_1fr] items-center px-3 py-2 border-b border-[#23252a] bg-[#161718] text-[11px] font-mono text-[#8a8f98]">
            <div className="flex justify-center">
              <button onClick={handleSelectAll} className="hover:text-white">
                {selectedIds.length > 0 &&
                selectedIds.length === filteredAndSortedMemories.length ? (
                  <CheckSquare className="w-3.5 h-3.5 text-[#e4f222]" />
                ) : (
                  <Square className="w-3.5 h-3.5" />
                )}
              </button>
            </div>
            <button
              onClick={() => handleSort("timestamp")}
              className="flex items-center gap-1 hover:text-white text-left"
            >
              <span>Timestamp</span>
              <ArrowUpDown className="w-3 h-3" />
            </button>
            <button
              onClick={() => handleSort("key")}
              className="flex items-center gap-1 hover:text-white text-left"
            >
              <span>Key</span>
              <ArrowUpDown className="w-3 h-3" />
            </button>
            <button
              onClick={() => handleSort("type")}
              className="flex items-center gap-1 hover:text-white text-left"
            >
              <span>Type</span>
              <ArrowUpDown className="w-3 h-3" />
            </button>
            <button
              onClick={() => handleSort("user")}
              className="flex items-center gap-1 hover:text-white text-left"
            >
              <span>User</span>
              <ArrowUpDown className="w-3 h-3" />
            </button>
            <button
              onClick={() => handleSort("content")}
              className="flex items-center gap-1 hover:text-white text-left pl-2"
            >
              <span>Content</span>
              <ArrowUpDown className="w-3 h-3" />
            </button>
          </div>

          {/* テーブル行リスト */}
          <div className="flex-1 overflow-y-auto divide-y divide-[#1c1e22]">
            {loading ? (
              <div className="flex flex-col items-center justify-center py-16 px-8 gap-3 text-xs text-[#8a8f98]">
                <div className="flex items-center gap-2">
                  <Loader2 className="w-4 h-4 animate-spin text-[#e4f222]" />
                  <span>
                    {migrationStatus?.status === "running"
                      ? migrationFinishing
                        ? "移行データを反映中..."
                        : migrationStatus.message ||
                          "Migrating legacy LanceDB memories..."
                      : "Loading LanceDB Memories..."}
                  </span>
                </div>
                {migrationStatus?.status === "running" && (
                  <div className="w-full max-w-[360px] space-y-1.5">
                    <div className="flex items-center justify-between text-[10px] font-mono text-[#8a8f98]">
                      <span>
                        {migrationStatus.processed.toLocaleString()} /{" "}
                        {migrationStatus.total?.toLocaleString() ?? "?"} records
                      </span>
                      <span>
                        {migrationPercent === null
                          ? "..."
                          : `${migrationPercent}%`}
                      </span>
                    </div>
                    <div className="h-1.5 w-full overflow-hidden rounded-full bg-[#23252a]">
                      <div
                        className="h-full rounded-full bg-[#e4f222] transition-[width] duration-300"
                        style={{
                          width:
                            migrationPercent === null
                              ? "4%"
                              : `${migrationPercent}%`,
                        }}
                      />
                    </div>
                    <p className="text-center text-[10px] text-[#62666d]">
                      初回のみ実行されます。アプリを終了しないでください。
                    </p>
                  </div>
                )}
              </div>
            ) : migrationStatus?.status === "error" &&
              memories.length === 0 ? (
              <div className="flex flex-col items-center justify-center py-16 px-8 gap-3 text-xs text-[#f87171]">
                <span>メモリー移行に失敗しました</span>
                <span className="max-w-[420px] text-center text-[10px] text-[#8a8f98]">
                  {migrationStatus.error || "詳細はログを確認してください。"}
                </span>
                <button
                  type="button"
                  onClick={() => void fetchMemories()}
                  className="rounded border border-[#e4f222]/40 px-3 py-1.5 text-[11px] text-[#e4f222] hover:bg-[#e4f222]/10"
                >
                  再試行
                </button>
              </div>
            ) : filteredAndSortedMemories.length === 0 ? (
              <div className="flex flex-col items-center justify-center py-16 text-xs text-[#62666d]">
                <Database className="w-8 h-8 stroke-[1.5] mb-2 opacity-40" />
                <span>No memories found</span>
              </div>
            ) : (
              filteredAndSortedMemories.map((m, index) => {
                const isSelected = selectedIds.includes(m.id);
                const isActive = activeItem?.id === m.id;

                return (
                  <div
                    key={m.id}
                    onClick={(e) => handleRowClick(m, index, e)}
                    onContextMenu={(event) => openRawContextMenu(event, m)}
                    onKeyDown={(event) => {
                      if (
                        event.key === "ContextMenu" ||
                        (event.shiftKey && event.key === "F10")
                      ) {
                        openRawContextMenu(event, m);
                      }
                    }}
                    role="button"
                    tabIndex={0}
                    aria-haspopup="menu"
                    aria-label={`Memory ${m.key || m.id}`}
                    className={`grid grid-cols-[36px_140px_100px_100px_100px_1fr] items-center px-3 py-2 text-xs cursor-pointer select-none transition-colors ${
                      isActive
                        ? "bg-[#1b1e24] border-l-2 border-l-[#e4f222]"
                        : isSelected
                          ? "bg-[#131519]"
                          : "hover:bg-[#111214]"
                    }`}
                  >
                    {/* チェックボックス */}
                    <div
                      className="flex justify-center"
                      onClick={(e) => handleToggleSelectId(m.id, index, e)}
                    >
                      {isSelected ? (
                        <CheckSquare className="w-3.5 h-3.5 text-[#e4f222]" />
                      ) : (
                        <Square className="w-3.5 h-3.5 text-[#62666d] hover:text-[#8a8f98]" />
                      )}
                    </div>

                    {/* Timestamp */}
                    <div className="font-mono text-[11px] text-[#8a8f98] truncate pr-2">
                      {m.display_ts || m.timestamp || "N/A"}
                    </div>

                    {/* Key */}
                    <div
                      className="font-mono text-[11px] text-[#d0d6e0] truncate pr-2"
                      title={m.key || m.id}
                    >
                      {m.key || m.id}
                    </div>

                    {/* Type */}
                    <div className="pr-2">
                      <span
                        className={`inline-block text-[10px] font-mono px-1.5 py-0.5 rounded border truncate max-w-full ${getTypeColor(
                          m.type,
                        )}`}
                      >
                        {m.type || "memory"}
                      </span>
                    </div>

                    {/* User */}
                    <div
                      className="text-[11px] text-[#d0d6e0] truncate pr-2"
                      title={m.user || m.source}
                    >
                      {m.user || m.source || "User"}
                    </div>

                    {/* Content */}
                    <div
                      className="text-xs text-[#8a8f98] truncate pl-2"
                      title={m.content}
                    >
                      {m.content}
                    </div>
                  </div>
                );
              })
            )}
          </div>
        </div>

        {/* 右ペイン: 詳細エディタ / 一括エディタ / ブログ生成プレビュー */}
        <MemoryEditor
          blogResult={blogResult}
          onClearBlogResult={() => setBlogResult(null)}
          onNotice={showNotice}
          isBulkMode={isBulkMode}
          selectedCount={selectedIds.length}
          bulkType={bulkType}
          setBulkType={setBulkType}
          bulkUser={bulkUser}
          setBulkUser={setBulkUser}
          onBulkUpdate={handleBulkUpdate}
          onCopySelectedAsJson={copySelectedAsJson}
          onDeleteSelected={handleDeleteSelected}
          onGenerateBlog={handleGenerateBlog}
          isGeneratingBlog={isGeneratingBlog}
          isCreatingNew={isCreatingNew}
          activeItem={activeItem}
          editKey={editKey}
          setEditKey={setEditKey}
          editType={editType}
          setEditType={setEditType}
          editUser={editUser}
          setEditUser={setEditUser}
          editContent={editContent}
          setEditContent={setEditContent}
          onSaveSingle={handleSaveSingle}
        />
      </div>

      {contextMenu && rawContextActions.length > 0 && (
        <MemoryContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          label="Memory actions"
          actions={rawContextActions}
          onClose={() => setContextMenu(null)}
        />
      )}
    </>
  );
};
