import type React from "react";
import {
  Sparkles,
  Layers,
  Tag,
  User,
  Save,
  Copy,
  Trash2,
  Loader2,
  FileText,
  Key,
  Clock,
} from "lucide-react";
import type { MemoryItem } from "../../../types";

export interface MemoryEditorProps {
  blogResult: { filename: string; content: string } | null;
  onClearBlogResult: () => void;
  onNotice: (text: string, type?: "success" | "error") => void;
  isBulkMode: boolean;
  selectedCount: number;
  bulkType: string;
  setBulkType: (val: string) => void;
  bulkUser: string;
  setBulkUser: (val: string) => void;
  onBulkUpdate: () => Promise<void>;
  onCopySelectedAsJson: (overrideIds?: string[]) => Promise<void>;
  onDeleteSelected: (idsOverride?: string[]) => Promise<void>;
  onGenerateBlog: (idsOverride?: string[]) => Promise<void>;
  isGeneratingBlog: boolean;
  isCreatingNew: boolean;
  activeItem: MemoryItem | null;
  editKey: string;
  setEditKey: (val: string) => void;
  editType: string;
  setEditType: (val: string) => void;
  editUser: string;
  setEditUser: (val: string) => void;
  editContent: string;
  setEditContent: (val: string) => void;
  onSaveSingle: () => Promise<void>;
}

export const MemoryEditor: React.FC<MemoryEditorProps> = ({
  blogResult,
  onClearBlogResult,
  onNotice,
  isBulkMode,
  selectedCount,
  bulkType,
  setBulkType,
  bulkUser,
  setBulkUser,
  onBulkUpdate,
  onCopySelectedAsJson,
  onDeleteSelected,
  onGenerateBlog,
  isGeneratingBlog,
  isCreatingNew,
  activeItem,
  editKey,
  setEditKey,
  editType,
  setEditType,
  editUser,
  setEditUser,
  editContent,
  setEditContent,
  onSaveSingle,
}) => {
  return (
    <div className="w-[360px] flex flex-col bg-[#0f1011] overflow-hidden">
      {blogResult ? (
        /* ブログ生成結果プレビュー表示 */
        <div className="flex-1 flex flex-col p-4 overflow-hidden animate-fade-in bg-[#0f1011]">
          <div className="flex items-center justify-between pb-3 border-b border-[#23252a]">
            <div className="flex items-center gap-1.5 text-xs font-semibold text-[#e4f222]">
              <Sparkles className="w-4 h-4" />
              <span>Generated Blog Article</span>
            </div>
            <button
              onClick={onClearBlogResult}
              className="text-[11px] text-[#8a8f98] hover:text-white underline"
            >
              Back to Editor
            </button>
          </div>

          <div className="py-2">
            <span className="text-[10px] font-mono text-[#8a8f98] block">
              Saved to:
            </span>
            <span className="text-[11px] font-mono text-[#4ade80] break-all">
              {blogResult.filename}
            </span>
          </div>

          <div className="flex-1 overflow-y-auto border border-[#23252a] rounded-[6px] p-3 bg-[#08090a] text-xs font-sans text-[#d0d6e0] whitespace-pre-wrap leading-relaxed">
            {blogResult.content}
          </div>

          <div className="pt-3 flex gap-2">
            <button
              onClick={() => {
                navigator.clipboard.writeText(blogResult.content);
                onNotice("記事をクリップボードにコピーしました！");
              }}
              className="flex-1 py-2 linear-btn-ghost text-xs font-medium"
            >
              Copy Markdown
            </button>
            <button
              onClick={onClearBlogResult}
              className="flex-1 py-2 linear-btn-primary text-xs font-semibold"
            >
              Done
            </button>
          </div>
        </div>
      ) : isBulkMode ? (
        /* 複数選択時の一括編集モード (Bulk Mode) */
        <div className="flex-1 flex flex-col p-4 overflow-y-auto gap-4 animate-fade-in">
          <div className="p-3 rounded-[8px] bg-[#161718] border border-[#23252a] flex items-center justify-between">
            <div className="flex items-center gap-2">
              <Layers className="w-4 h-4 text-[#e4f222]" />
              <span className="text-xs font-semibold text-white">
                Bulk Editing
              </span>
            </div>
            <span className="text-xs font-mono px-2 py-0.5 rounded bg-[#e4f222]/15 text-[#e4f222] font-bold">
              {selectedCount} items selected
            </span>
          </div>

          <div className="flex flex-col gap-3">
            <div>
              <label className="flex items-center gap-1 text-[11px] text-[#8a8f98] mb-1 font-medium">
                <Tag className="w-3 h-3 text-[#02b8cc]" />
                <span>Set Type for All Selected</span>
              </label>
              <input
                type="text"
                placeholder="e.g. user_speech, ai_response, note..."
                value={bulkType}
                onChange={(e) => setBulkType(e.target.value)}
                className="w-full text-xs font-mono linear-input px-2.5 py-1.5 bg-[#08090a] text-[#d0d6e0]"
              />
            </div>

            <div>
              <label className="flex items-center gap-1 text-[11px] text-[#8a8f98] mb-1 font-medium">
                <User className="w-3 h-3 text-[#e4f222]" />
                <span>Set User for All Selected</span>
              </label>
              <input
                type="text"
                placeholder="e.g. User, Streamer, Gemini..."
                value={bulkUser}
                onChange={(e) => setBulkUser(e.target.value)}
                className="w-full text-xs font-sans linear-input px-2.5 py-1.5 bg-[#08090a] text-[#d0d6e0]"
              />
            </div>
          </div>

          <div className="mt-auto flex flex-col gap-2.5 pt-4 border-t border-[#23252a]">
            <button
              onClick={() => void onBulkUpdate()}
              className="w-full py-2 linear-btn-ghost flex items-center justify-center gap-1.5 text-xs font-medium"
            >
              <Save className="w-3.5 h-3.5 text-[#27a644]" />
              <span>Apply Metadata to {selectedCount} Items</span>
            </button>

            <button
              type="button"
              onClick={() => void onCopySelectedAsJson()}
              className="w-full py-2 linear-btn-ghost flex items-center justify-center gap-1.5 text-xs font-medium hover:text-[#02b8cc] hover:border-[#02b8cc]/40"
              title="Ctrl+C で選択したメモリを JSON としてコピー"
            >
              <Copy className="w-3.5 h-3.5 text-[#02b8cc]" />
              <span>Copy {selectedCount} as JSON (Ctrl+C)</span>
            </button>

            <button
              onClick={() => void onDeleteSelected()}
              className="w-full py-2 linear-btn-ghost border-[#eb5757]/30 hover:border-[#eb5757] text-[#eb5757] flex items-center justify-center gap-1.5 text-xs font-medium"
            >
              <Trash2 className="w-3.5 h-3.5" />
              <span>Delete {selectedCount} Selected</span>
            </button>

            {/* 🌟 選択したメモリーから note ブログ生成 */}
            <button
              onClick={() => void onGenerateBlog()}
              disabled={isGeneratingBlog}
              className="w-full py-2.5 linear-btn-primary flex items-center justify-center gap-2 text-xs font-semibold shadow-lg"
            >
              {isGeneratingBlog ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin text-[#08090a]" />
                  <span>Generating note Blog...</span>
                </>
              ) : (
                <>
                  <Sparkles className="w-4 h-4 text-[#08090a]" />
                  <span>
                    Generate Blog from Selected ({selectedCount})
                  </span>
                </>
              )}
            </button>
          </div>
        </div>
      ) : (
        /* 単一選択 or 新規作成モード (Single Detail & Edit) */
        <div className="flex-1 flex flex-col p-4 overflow-y-auto gap-3.5 animate-fade-in">
          <div className="flex items-center justify-between pb-2 border-b border-[#23252a]">
            <div className="flex items-center gap-2">
              <FileText className="w-4 h-4 text-[#e4f222]" />
              <span className="text-xs font-semibold text-white">
                {isCreatingNew ? "Create New Memory" : "Memory Details"}
              </span>
            </div>
            {activeItem && !isCreatingNew && (
              <span className="text-[10px] font-mono text-[#8a8f98]">
                ID: {activeItem.id.slice(0, 8)}...
              </span>
            )}
          </div>

          {/* Key フィールド */}
          <div>
            <label className="flex items-center gap-1 text-[11px] text-[#8a8f98] mb-1 font-medium">
              <Key className="w-3 h-3 text-[#e4f222]" />
              <span>Key:</span>
            </label>
            <input
              type="text"
              value={editKey}
              readOnly={!isCreatingNew}
              onChange={(e) => setEditKey(e.target.value)}
              className={`w-full text-xs font-mono linear-input px-2.5 py-1.5 ${
                isCreatingNew
                  ? "bg-[#08090a] text-white"
                  : "bg-[#161718] text-[#8a8f98] cursor-not-allowed"
              }`}
              placeholder="e.g. user_fact_123"
            />
          </div>

          {/* Type & User 行 */}
          <div className="grid grid-cols-2 gap-2">
            <div>
              <label className="flex items-center gap-1 text-[11px] text-[#8a8f98] mb-1 font-medium">
                <Tag className="w-3 h-3 text-[#02b8cc]" />
                <span>Type:</span>
              </label>
              <input
                type="text"
                value={editType}
                onChange={(e) => setEditType(e.target.value)}
                className="w-full text-xs font-mono linear-input px-2.5 py-1.5 bg-[#08090a] text-[#d0d6e0]"
                placeholder="memory"
              />
            </div>

            <div>
              <label className="flex items-center gap-1 text-[11px] text-[#8a8f98] mb-1 font-medium">
                <User className="w-3 h-3 text-[#8b5cf6]" />
                <span>User / Source:</span>
              </label>
              <input
                type="text"
                value={editUser}
                onChange={(e) => setEditUser(e.target.value)}
                className="w-full text-xs font-sans linear-input px-2.5 py-1.5 bg-[#08090a] text-[#d0d6e0]"
                placeholder="User"
              />
            </div>
          </div>

          {/* Timestamp */}
          {!isCreatingNew && activeItem && (
            <div className="flex items-center gap-1.5 text-[11px] text-[#8a8f98] font-mono bg-[#161718] px-2.5 py-1.5 rounded-[6px] border border-[#23252a]">
              <Clock className="w-3 h-3 text-[#8a8f98]" />
              <span>
                Timestamp:{" "}
                {activeItem.display_ts || activeItem.timestamp || "N/A"}
              </span>
            </div>
          )}

          {/* Content (テキストエリア) */}
          <div className="flex-1 flex flex-col min-h-[160px]">
            <div className="flex justify-between items-center mb-1">
              <label className="text-[11px] text-[#8a8f98] font-medium">
                Content / Fact:
              </label>
              <span className="text-[10px] font-mono text-[#62666d]">
                {editContent.length} chars
              </span>
            </div>
            <textarea
              value={editContent}
              onChange={(e) => setEditContent(e.target.value)}
              placeholder="記憶内容を入力..."
              className="flex-1 w-full text-xs font-sans linear-input p-2.5 bg-[#08090a] text-white resize-none leading-relaxed focus:border-[#e4f222]"
            />
          </div>

          {/* アクションボタン */}
          <div className="flex flex-col gap-2 pt-2 border-t border-[#23252a]">
            <div className="flex gap-2">
              <button
                onClick={() => void onSaveSingle()}
                className="flex-1 py-2 linear-btn-primary flex items-center justify-center gap-1.5 text-xs font-semibold"
              >
                <Save className="w-3.5 h-3.5 text-[#08090a]" />
                <span>
                  {isCreatingNew ? "Create Memory" : "Save Changes"}
                </span>
              </button>

              {!isCreatingNew && activeItem && (
                <button
                  onClick={() => void onDeleteSelected()}
                  className="p-2 linear-btn-ghost border-[#eb5757]/30 hover:border-[#eb5757] text-[#eb5757] rounded-[6px]"
                  title="このメモリーを削除"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                </button>
              )}
            </div>

            {/* 1件選択時でもブログ生成が可能 */}
            {!isCreatingNew && activeItem && (
              <button
                onClick={() => void onGenerateBlog([activeItem.id])}
                disabled={isGeneratingBlog}
                className="w-full py-2 linear-btn-ghost border-[#e4f222]/30 hover:border-[#e4f222] text-[#e4f222] flex items-center justify-center gap-1.5 text-xs font-medium"
              >
                {isGeneratingBlog ? (
                  <>
                    <Loader2 className="w-3.5 h-3.5 animate-spin text-[#e4f222]" />
                    <span>Generating Blog...</span>
                  </>
                ) : (
                  <>
                    <Sparkles className="w-3.5 h-3.5 text-[#e4f222]" />
                    <span>Generate Blog from This Memory</span>
                  </>
                )}
              </button>
            )}

            {!isCreatingNew && activeItem && (
              <button
                type="button"
                onClick={() => void onCopySelectedAsJson([activeItem.id])}
                className="w-full py-2 linear-btn-ghost flex items-center justify-center gap-1.5 text-xs font-medium hover:text-[#02b8cc] hover:border-[#02b8cc]/40"
                title="Ctrl+C でこのメモリを JSON としてコピー"
              >
                <Copy className="w-3.5 h-3.5 text-[#02b8cc]" />
                <span>Copy as JSON (Ctrl+C)</span>
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
};
