import React, { useState, useEffect, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
  X,
  Database,
  Search,
  Trash2,
  Save,
  Plus,
  RefreshCw,
  FileText,
  CheckSquare,
  Square,
  Sparkles,
  ArrowUpDown,
  User,
  Clock,
  Tag,
  Key,
  Layers,
  Loader2,
  Download,
  Archive,
} from 'lucide-react';
import { MemoryItem } from '../../types';

interface MemoryModalProps {
  isOpen: boolean;
  onClose: () => void;
}

type SortField = 'timestamp' | 'key' | 'type' | 'user' | 'content';
type SortOrder = 'asc' | 'desc';

export const MemoryModal: React.FC<MemoryModalProps> = ({ isOpen, onClose }) => {
  const [memories, setMemories] = useState<MemoryItem[]>([]);
  const [loading, setLoading] = useState<boolean>(false);
  const [searchQuery, setSearchQuery] = useState<string>('');

  // 選択状態 (複数選択 & Shift/Ctrl)
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const [activeItem, setActiveItem] = useState<MemoryItem | null>(null);
  const [lastAnchorIndex, setLastAnchorIndex] = useState<number | null>(null);

  // ソート状態
  const [sortField, setSortField] = useState<SortField>('timestamp');
  const [sortOrder, setSortOrder] = useState<SortOrder>('desc');

  // 編集フォーム状態 (単一)
  const [editKey, setEditKey] = useState<string>('');
  const [editType, setEditType] = useState<string>('memory');
  const [editUser, setEditUser] = useState<string>('User');
  const [editContent, setEditContent] = useState<string>('');
  const [isCreatingNew, setIsCreatingNew] = useState<boolean>(false);

  // 一括編集状態 (複数)
  const [bulkType, setBulkType] = useState<string>('');
  const [bulkUser, setBulkUser] = useState<string>('');

  // アクション通知・生成中状態
  const [actionMessage, setActionMessage] = useState<{ text: string; type: 'success' | 'error' } | null>(null);
  const [isGeneratingBlog, setIsGeneratingBlog] = useState<boolean>(false);
  const [blogResult, setBlogResult] = useState<{ filename: string; content: string } | null>(null);

  const fetchMemories = async () => {
    setLoading(true);
    try {
      // Tauri Native LanceDB を呼び出し (最新順で取得)
      const data: any = await invoke('list_lance_memories', { limit: 5000, offset: 0 });
      if (data && data.success) {
        const list: MemoryItem[] = (data.memories || []).map((m: any) => ({
          id: m.id,
          key: m.id,
          content: m.document,
          type: m.memory_type,
          source: m.source,
          user: m.user_id || m.source || 'User',
          timestamp: m.timestamp,
        }));
        setMemories(list);
        if (list.length > 0 && selectedIds.length === 0 && !activeItem) {
          setActiveItem(list[0]);
          setSelectedIds([list[0].id]);
          setLastAnchorIndex(0);
          populateEditForm(list[0]);
        }
      } else {
        showNotice('メモリーの取得に失敗しました', 'error');
      }
    } catch (err) {
      console.error('Failed to fetch memories from LanceDB:', err);
      showNotice('メモリーの取得に失敗しました', 'error');
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    if (isOpen) {
      fetchMemories();
    } else {
      setSelectedIds([]);
      setActiveItem(null);
      setLastAnchorIndex(null);
      setBlogResult(null);
      setIsCreatingNew(false);
    }
  }, [isOpen]);

  const populateEditForm = (item: MemoryItem) => {
    setIsCreatingNew(false);
    setEditKey(item.key || item.id);
    setEditType(item.type || 'memory');
    setEditUser(item.user || item.source || 'User');
    setEditContent(item.content || '');
  };

  const showNotice = (text: string, type: 'success' | 'error' = 'success') => {
    setActionMessage({ text, type });
    setTimeout(() => setActionMessage(null), 3500);
  };

  // フィルタ & ソート
  const filteredAndSortedMemories = useMemo(() => {
    let result = [...memories];
    if (searchQuery.trim()) {
      const q = searchQuery.toLowerCase();
      result = result.filter(
        (m) =>
          (m.content || '').toLowerCase().includes(q) ||
          (m.user || m.source || '').toLowerCase().includes(q) ||
          (m.type || '').toLowerCase().includes(q) ||
          (m.key || m.id || '').toLowerCase().includes(q)
      );
    }

    result.sort((a, b) => {
      let valA = '';
      let valB = '';

      switch (sortField) {
        case 'timestamp':
          valA = a.timestamp || '';
          valB = b.timestamp || '';
          break;
        case 'key':
          valA = a.key || a.id || '';
          valB = b.key || b.id || '';
          break;
        case 'type':
          valA = a.type || '';
          valB = b.type || '';
          break;
        case 'user':
          valA = a.user || a.source || '';
          valB = b.user || b.source || '';
          break;
        case 'content':
          valA = a.content || '';
          valB = b.content || '';
          break;
      }

      const cmp = valA.localeCompare(valB, undefined, { numeric: true });
      return sortOrder === 'asc' ? cmp : -cmp;
    });

    return result;
  }, [memories, searchQuery, sortField, sortOrder]);

  // Ctrl+A で全選択ショートカット
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (!isOpen) return;
      const target = e.target as HTMLElement;
      if (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA') return;

      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'a') {
        e.preventDefault();
        setSelectedIds(filteredAndSortedMemories.map((m) => m.id));
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [isOpen, filteredAndSortedMemories]);

  const handleSort = (field: SortField) => {
    if (sortField === field) {
      setSortOrder(sortOrder === 'asc' ? 'desc' : 'asc');
    } else {
      setSortField(field);
      setSortOrder('desc');
    }
  };

  // 選択操作 (Ctrl / Shift / 通常クリック対応)
  const handleRowClick = (item: MemoryItem, index: number, e: React.MouseEvent) => {
    const isCtrl = e.ctrlKey || e.metaKey;
    const isShift = e.shiftKey;

    if (isShift && lastAnchorIndex !== null) {
      // Shift + クリック: 範囲選択
      const from = Math.min(lastAnchorIndex, index);
      const to = Math.max(lastAnchorIndex, index);
      const rangeIds = filteredAndSortedMemories.slice(from, to + 1).map((m) => m.id);

      if (isCtrl) {
        // Ctrl + Shift: 既存選択に範囲を結合
        setSelectedIds((prev) => Array.from(new Set([...prev, ...rangeIds])));
      } else {
        // Shift のみ: 範囲を選択
        setSelectedIds(rangeIds);
      }
    } else if (isCtrl) {
      // Ctrl + クリック: トグル
      setSelectedIds((prev) => {
        if (prev.includes(item.id)) {
          return prev.filter((id) => id !== item.id);
        } else {
          return [...prev, item.id];
        }
      });
      setLastAnchorIndex(index);
    } else {
      // 通常クリック: 単一選択
      setSelectedIds([item.id]);
      setLastAnchorIndex(index);
    }

    setActiveItem(item);
    populateEditForm(item);
  };

  const handleToggleSelectId = (id: string, index: number, e: React.MouseEvent) => {
    e.stopPropagation();
    setSelectedIds((prev) => {
      if (prev.includes(id)) {
        return prev.filter((i) => i !== id);
      } else {
        return [...prev, id];
      }
    });
    setLastAnchorIndex(index);
  };

  const handleSelectAll = () => {
    if (selectedIds.length === filteredAndSortedMemories.length) {
      setSelectedIds([]);
    } else {
      setSelectedIds(filteredAndSortedMemories.map((m) => m.id));
    }
  };

  // 単一保存 / 新規作成
  const handleSaveSingle = async () => {
    if (!editKey.trim()) {
      showNotice('Key を指定してください', 'error');
      return;
    }
    if (!editContent.trim()) {
      showNotice('Content（内容）を入力してください', 'error');
      return;
    }

    try {
      const itemToSave = {
        id: editKey.trim(),
        document: editContent.trim(),
        memory_type: editType.trim() || 'memory',
        source: editUser.trim() || 'User',
        timestamp: new Date().toISOString(),
        user_id: editUser.trim() || 'User',
      };
      await invoke('import_memories_to_lance', { items: [itemToSave], vectors: null });
      showNotice(isCreatingNew ? '新規メモリーを作成しました！' : 'メモリーの変更を保存しました！');
      setIsCreatingNew(false);
      await fetchMemories();
    } catch (e) {
      showNotice(`保存エラー: ${e}`, 'error');
    }
  };

  // 単一 / 選択中アイテムの削除
  const handleDeleteSelected = async () => {
    const count = selectedIds.length;
    if (count === 0) return;

    if (!confirm(`選択した ${count} 件のメモリーを完全に削除しますか？`)) {
      return;
    }

    try {
      await invoke('delete_lance_memories_bulk', { ids: selectedIds });
      showNotice(`${count} 件のメモリーを LanceDB から削除しました`);
      setSelectedIds([]);
      setActiveItem(null);
      await fetchMemories();
    } catch (e) {
      showNotice(`削除エラー: ${e}`, 'error');
    }
  };

  // 一括メタデータ更新
  const handleBulkUpdate = async () => {
    if (selectedIds.length === 0) return;
    if (!bulkType && !bulkUser) {
      showNotice('一括適用する Type または User を指定してください', 'error');
      return;
    }

    try {
      const targetItems = memories.filter((m) => selectedIds.includes(m.id));
      const updatedItems = targetItems.map((m) => ({
        id: m.id,
        document: m.content,
        memory_type: bulkType.trim() || m.type || 'memory',
        source: bulkUser.trim() || m.source || 'User',
        timestamp: m.timestamp || new Date().toISOString(),
        user_id: bulkUser.trim() || m.user || 'User',
      }));

      await invoke('delete_lance_memories_bulk', { ids: selectedIds });
      await invoke('import_memories_to_lance', { items: updatedItems, vectors: null });
      showNotice(`${selectedIds.length} 件のメモリーを一括更新しました！`);
      setBulkType('');
      setBulkUser('');
      await fetchMemories();
    } catch (e) {
      showNotice(`一括更新エラー: ${e}`, 'error');
    }
  };

  // 選択メモリーからのブログ生成
  const handleGenerateBlog = async () => {
    if (selectedIds.length === 0) {
      showNotice('ブログを生成するメモリーを選択してください', 'error');
      return;
    }

    setIsGeneratingBlog(true);
    try {
      const res = await fetch('http://127.0.0.1:18080/api/memories/generate-blog', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ids: selectedIds }),
      });
      const data = await res.json();
      if (data.success) {
        setBlogResult({
          filename: data.filename,
          content: data.content,
        });
        showNotice('note プレイ日誌記事の生成が完了しました！');
      } else {
        showNotice(`ブログ生成エラー: ${data.error}`, 'error');
      }
    } catch (e) {
      showNotice(`通信エラー: ${e}`, 'error');
    } finally {
      setIsGeneratingBlog(false);
    }
  };

  const handleBackup = async () => {
    try {
      if (typeof window !== 'undefined' && (window as any).__TAURI_INTERNALS__) {
        const res: string = await invoke('lance_backup');
        showNotice(`バックアップを作成しました: ${res}`);
      }
    } catch (e) {
      showNotice(`バックアップ失敗: ${e}`, 'error');
    }
  };

  const handleExportJson = async () => {
    try {
      if (typeof window !== 'undefined' && (window as any).__TAURI_INTERNALS__) {
        const res: string = await invoke('lance_export_json', {});
        showNotice(res);
      }
    } catch (e) {
      showNotice(`JSONエクスポート失敗: ${e}`, 'error');
    }
  };

  const startCreateNew = () => {
    setIsCreatingNew(true);
    setActiveItem(null);
    setSelectedIds([]);
    setEditKey(`mem_${Date.now()}`);
    setEditType('memory');
    setEditUser('User');
    setEditContent('');
  };

  if (!isOpen) return null;

  const isBulkMode = selectedIds.length > 1;

  // Type バッジのカラーマップ
  const getTypeColor = (type?: string) => {
    switch (type) {
      case 'twitch_chat':
        return 'bg-[#8b5cf6]/15 text-[#a78bfa] border-[#8b5cf6]/30';
      case 'ai_response':
        return 'bg-[#27a644]/15 text-[#4ade80] border-[#27a644]/30';
      case 'auto_commentary':
        return 'bg-[#e4f222]/15 text-[#e4f222] border-[#e4f222]/30';
      case 'user_speech':
      case 'user_prompt':
        return 'bg-[#02b8cc]/15 text-[#38bdf8] border-[#02b8cc]/30';
      default:
        return 'bg-[#23252a] text-[#8a8f98] border-[#383b3f]';
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/75 backdrop-blur-md animate-fade-in select-none">
      <div className="w-[1060px] h-[86vh] max-h-[820px] bg-[#08090a] border border-[#23252a] rounded-[14px] shadow-2xl flex flex-col overflow-hidden text-[#d0d6e0]">
        {/* ヘッダー */}
        <div className="px-5 py-3.5 border-b border-[#23252a] flex items-center justify-between bg-[#161718]">
          <div className="flex items-center gap-2.5">
            <div className="p-1.5 rounded-[6px] bg-[#e4f222]/10 border border-[#e4f222]/20">
              <Database className="w-4 h-4 text-[#e4f222]" />
            </div>
            <div>
              <div className="flex items-center gap-2">
                <h2 className="text-sm font-semibold text-white tracking-wide">Memory Manager</h2>
                <span className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-[#e4f222]/10 text-[#e4f222] border border-[#e4f222]/30">
                  LanceDB (Rust Native)
                </span>
              </div>
              <p className="text-[11px] text-[#8a8f98]">
                長期記憶の検索、詳細編集、一括更新、選択メモリからの note ブログ自動生成
              </p>
            </div>
          </div>

          <div className="flex items-center gap-2">
            <button
              onClick={handleBackup}
              className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs font-medium rounded-[6px] bg-[#1a1b1e] hover:bg-[#23252a] text-[#d0d6e0] border border-[#2e3035] transition-all"
              title="LanceDB のスナップショットバックアップを作成"
            >
              <Archive className="w-3.5 h-3.5 text-[#38bdf8]" />
              <span>Backup</span>
            </button>
            <button
              onClick={handleExportJson}
              className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs font-medium rounded-[6px] bg-[#1a1b1e] hover:bg-[#23252a] text-[#d0d6e0] border border-[#2e3035] transition-all"
              title="全件を JSON ファイルに出力"
            >
              <Download className="w-3.5 h-3.5 text-[#4ade80]" />
              <span>Export</span>
            </button>
            <button
              onClick={startCreateNew}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-semibold rounded-[6px] bg-[#23252a] hover:bg-[#383b3f] text-white border border-[#383b3f] transition-all"
            >
              <Plus className="w-3.5 h-3.5 text-[#e4f222]" />
              <span>New Memory</span>
            </button>
            <button
              onClick={fetchMemories}
              className="p-1.5 text-[#8a8f98] hover:text-white hover:bg-[#23252a] rounded-[6px] transition-colors"
              title="データを再読込"
            >
              <RefreshCw className={`w-4 h-4 ${loading ? 'animate-spin' : ''}`} />
            </button>
            <button
              onClick={onClose}
              className="p-1.5 text-[#8a8f98] hover:text-white hover:bg-[#23252a] rounded-[6px] transition-colors"
            >
              <X className="w-4 h-4" />
            </button>
          </div>
        </div>

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
              {selectedIds.length > 0 && selectedIds.length === filteredAndSortedMemories.length ? (
                <CheckSquare className="w-3.5 h-3.5 text-[#e4f222]" />
              ) : (
                <Square className="w-3.5 h-3.5" />
              )}
              <span>Select All</span>
            </button>

            <span className="text-[#383b3f]">|</span>

            <span className="font-mono text-[11px] text-[#8a8f98]">
              Selected:{' '}
              <strong className="text-[#e4f222] font-semibold">{selectedIds.length}</strong> /{' '}
              {filteredAndSortedMemories.length}
            </span>

            {actionMessage && (
              <span
                className={`text-[11px] font-medium px-2 py-0.5 rounded border animate-fade-in ${
                  actionMessage.type === 'success'
                    ? 'bg-[#27a644]/15 text-[#4ade80] border-[#27a644]/30'
                    : 'bg-[#eb5757]/15 text-[#f87171] border-[#eb5757]/30'
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
                  {selectedIds.length > 0 && selectedIds.length === filteredAndSortedMemories.length ? (
                    <CheckSquare className="w-3.5 h-3.5 text-[#e4f222]" />
                  ) : (
                    <Square className="w-3.5 h-3.5" />
                  )}
                </button>
              </div>
              <button
                onClick={() => handleSort('timestamp')}
                className="flex items-center gap-1 hover:text-white text-left"
              >
                <span>Timestamp</span>
                <ArrowUpDown className="w-3 h-3" />
              </button>
              <button
                onClick={() => handleSort('key')}
                className="flex items-center gap-1 hover:text-white text-left"
              >
                <span>Key</span>
                <ArrowUpDown className="w-3 h-3" />
              </button>
              <button
                onClick={() => handleSort('type')}
                className="flex items-center gap-1 hover:text-white text-left"
              >
                <span>Type</span>
                <ArrowUpDown className="w-3 h-3" />
              </button>
              <button
                onClick={() => handleSort('user')}
                className="flex items-center gap-1 hover:text-white text-left"
              >
                <span>User</span>
                <ArrowUpDown className="w-3 h-3" />
              </button>
              <button
                onClick={() => handleSort('content')}
                className="flex items-center gap-1 hover:text-white text-left pl-2"
              >
                <span>Content</span>
                <ArrowUpDown className="w-3 h-3" />
              </button>
            </div>

            {/* テーブル行リスト */}
            <div className="flex-1 overflow-y-auto divide-y divide-[#1c1e22]">
              {loading ? (
                <div className="flex items-center justify-center py-16 gap-2 text-xs text-[#8a8f98]">
                  <Loader2 className="w-4 h-4 animate-spin text-[#e4f222]" />
                  <span>Loading LanceDB Memories...</span>
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
                      className={`grid grid-cols-[36px_140px_100px_100px_100px_1fr] items-center px-3 py-2 text-xs cursor-pointer select-none transition-colors ${
                        isActive
                          ? 'bg-[#1b1e24] border-l-2 border-l-[#e4f222]'
                          : isSelected
                          ? 'bg-[#131519]'
                          : 'hover:bg-[#111214]'
                      }`}
                    >
                      {/* チェックボックス */}
                      <div className="flex justify-center" onClick={(e) => handleToggleSelectId(m.id, index, e)}>
                        {isSelected ? (
                          <CheckSquare className="w-3.5 h-3.5 text-[#e4f222]" />
                        ) : (
                          <Square className="w-3.5 h-3.5 text-[#62666d] hover:text-[#8a8f98]" />
                        )}
                      </div>

                      {/* Timestamp */}
                      <div className="font-mono text-[11px] text-[#8a8f98] truncate pr-2">
                        {m.display_ts || m.timestamp || 'N/A'}
                      </div>

                      {/* Key */}
                      <div className="font-mono text-[11px] text-[#d0d6e0] truncate pr-2" title={m.key || m.id}>
                        {m.key || m.id}
                      </div>

                      {/* Type */}
                      <div className="pr-2">
                        <span
                          className={`inline-block text-[10px] font-mono px-1.5 py-0.5 rounded border truncate max-w-full ${getTypeColor(
                            m.type
                          )}`}
                        >
                          {m.type || 'memory'}
                        </span>
                      </div>

                      {/* User */}
                      <div className="text-[11px] text-[#d0d6e0] truncate pr-2" title={m.user || m.source}>
                        {m.user || m.source || 'User'}
                      </div>

                      {/* Content */}
                      <div className="text-xs text-[#8a8f98] truncate pl-2" title={m.content}>
                        {m.content}
                      </div>
                    </div>
                  );
                })
              )}
            </div>
          </div>

          {/* 右ペイン: 詳細エディタ / 一括エディタ / ブログ生成プレビュー */}
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
                    onClick={() => setBlogResult(null)}
                    className="text-[11px] text-[#8a8f98] hover:text-white underline"
                  >
                    Back to Editor
                  </button>
                </div>

                <div className="py-2">
                  <span className="text-[10px] font-mono text-[#8a8f98] block">Saved to:</span>
                  <span className="text-[11px] font-mono text-[#4ade80] break-all">{blogResult.filename}</span>
                </div>

                <div className="flex-1 overflow-y-auto border border-[#23252a] rounded-[6px] p-3 bg-[#08090a] text-xs font-sans text-[#d0d6e0] whitespace-pre-wrap leading-relaxed">
                  {blogResult.content}
                </div>

                <div className="pt-3 flex gap-2">
                  <button
                    onClick={() => {
                      navigator.clipboard.writeText(blogResult.content);
                      showNotice('記事をクリップボードにコピーしました！');
                    }}
                    className="flex-1 py-2 linear-btn-ghost text-xs font-medium"
                  >
                    Copy Markdown
                  </button>
                  <button
                    onClick={() => setBlogResult(null)}
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
                    <span className="text-xs font-semibold text-white">Bulk Editing</span>
                  </div>
                  <span className="text-xs font-mono px-2 py-0.5 rounded bg-[#e4f222]/15 text-[#e4f222] font-bold">
                    {selectedIds.length} items selected
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
                    onClick={handleBulkUpdate}
                    className="w-full py-2 linear-btn-ghost flex items-center justify-center gap-1.5 text-xs font-medium"
                  >
                    <Save className="w-3.5 h-3.5 text-[#27a644]" />
                    <span>Apply Metadata to {selectedIds.length} Items</span>
                  </button>

                  <button
                    onClick={handleDeleteSelected}
                    className="w-full py-2 linear-btn-ghost border-[#eb5757]/30 hover:border-[#eb5757] text-[#eb5757] flex items-center justify-center gap-1.5 text-xs font-medium"
                  >
                    <Trash2 className="w-3.5 h-3.5" />
                    <span>Delete {selectedIds.length} Selected</span>
                  </button>

                  {/* 🌟 選択したメモリーから note ブログ生成 */}
                  <button
                    onClick={handleGenerateBlog}
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
                        <span>Generate Blog from Selected ({selectedIds.length})</span>
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
                      {isCreatingNew ? 'Create New Memory' : 'Memory Details'}
                    </span>
                  </div>
                  {activeItem && !isCreatingNew && (
                    <span className="text-[10px] font-mono text-[#8a8f98]">ID: {activeItem.id.slice(0, 8)}...</span>
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
                      isCreatingNew ? 'bg-[#08090a] text-white' : 'bg-[#161718] text-[#8a8f98] cursor-not-allowed'
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
                    <span>Timestamp: {activeItem.display_ts || activeItem.timestamp || 'N/A'}</span>
                  </div>
                )}

                {/* Content (テキストエリア) */}
                <div className="flex-1 flex flex-col min-h-[160px]">
                  <div className="flex justify-between items-center mb-1">
                    <label className="text-[11px] text-[#8a8f98] font-medium">Content / Fact:</label>
                    <span className="text-[10px] font-mono text-[#62666d]">{editContent.length} chars</span>
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
                      onClick={handleSaveSingle}
                      className="flex-1 py-2 linear-btn-primary flex items-center justify-center gap-1.5 text-xs font-semibold"
                    >
                      <Save className="w-3.5 h-3.5 text-[#08090a]" />
                      <span>{isCreatingNew ? 'Create Memory' : 'Save Changes'}</span>
                    </button>

                    {!isCreatingNew && activeItem && (
                      <button
                        onClick={handleDeleteSelected}
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
                      onClick={handleGenerateBlog}
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
                </div>
              </div>
            )}
          </div>
        </div>

        {/* フッター */}
        <div className="px-5 py-2.5 border-t border-[#23252a] flex justify-between items-center bg-[#161718] text-xs text-[#8a8f98]">
          <div className="flex items-center gap-3">
            <span>Total Memories: {memories.length}</span>
            {selectedIds.length > 0 && (
              <span className="text-[#e4f222] font-semibold">({selectedIds.length} selected)</span>
            )}
          </div>
          <button onClick={onClose} className="px-4 py-1.5 linear-btn-ghost font-medium">
            Close
          </button>
        </div>
      </div>
    </div>
  );
};

