import { useState, useEffect, useMemo, useRef, useCallback } from "react";
import type React from "react";
import type { MemoryItem } from "../types";
import type { ContextMenuAction } from "../components/Modals/Memory/MemoryContextMenu";
import {
  listLanceMemoriesApi,
  importMemoriesToLanceApi,
  deleteLanceMemoriesBulkApi,
  updateLanceMemoriesBulkApi,
  generateBlogFromMemoriesApi,
  lanceBackupApi,
  lanceExportJsonApi,
  writeClipboardText,
} from "../services/memoryService";

export type SortField = "timestamp" | "key" | "type" | "user" | "content";
export type SortOrder = "asc" | "desc";

export interface UseLegacyLanceMemoriesOptions {
  isOpen: boolean;
  isActiveTab: boolean;
}

export function useLegacyLanceMemories({
  isOpen,
  isActiveTab,
}: UseLegacyLanceMemoriesOptions) {
  const [memories, setMemories] = useState<MemoryItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");

  // 選択状態 (複数選択 & Shift/Ctrl)
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const [activeItem, setActiveItem] = useState<MemoryItem | null>(null);
  const [lastAnchorIndex, setLastAnchorIndex] = useState<number | null>(null);
  const [contextMenu, setContextMenu] = useState<{
    x: number;
    y: number;
    id: string;
  } | null>(null);
  const [optimisticallyDeletedIds, setOptimisticallyDeletedIds] = useState<
    Set<string>
  >(() => new Set());
  const fetchRequestId = useRef(0);

  // ソート状態
  const [sortField, setSortField] = useState<SortField>("timestamp");
  const [sortOrder, setSortOrder] = useState<SortOrder>("desc");

  // 編集フォーム状態 (単一)
  const [editKey, setEditKey] = useState("");
  const [editType, setEditType] = useState("memory");
  const [editUser, setEditUser] = useState("User");
  const [editContent, setEditContent] = useState("");
  const [isCreatingNew, setIsCreatingNew] = useState(false);

  // 一括編集状態 (複数)
  const [bulkType, setBulkType] = useState("");
  const [bulkUser, setBulkUser] = useState("");

  // アクション通知・生成中状態
  const [actionMessage, setActionMessage] = useState<{
    text: string;
    type: "success" | "error";
  } | null>(null);
  const isGeneratingBlogRef = useRef(false);
  const [isGeneratingBlog, setIsGeneratingBlog] = useState(false);
  const [blogResult, setBlogResult] = useState<{
    filename: string;
    content: string;
  } | null>(null);

  const showNotice = useCallback(
    (text: string, type: "success" | "error" = "success") => {
      setActionMessage({ text, type });
      window.setTimeout(() => setActionMessage(null), 3500);
    },
    [],
  );

  const optimisticallyDeletedIdsRef = useRef(optimisticallyDeletedIds);
  optimisticallyDeletedIdsRef.current = optimisticallyDeletedIds;
  const selectedIdsRef = useRef(selectedIds);
  selectedIdsRef.current = selectedIds;
  const activeItemRef = useRef(activeItem);
  activeItemRef.current = activeItem;

  const populateEditForm = useCallback((item: MemoryItem) => {
    setIsCreatingNew(false);
    setEditKey(item.key || item.id);
    setEditType(item.type || "memory");
    setEditUser(item.user || item.source || "User");
    setEditContent(item.content || "");
  }, []);

  const fetchMemories = useCallback(
    async (hiddenIds?: ReadonlySet<string>) => {
      const idsToHide = hiddenIds ?? optimisticallyDeletedIdsRef.current;
      const requestId = ++fetchRequestId.current;
      setLoading(true);
      try {
        const list = await listLanceMemoriesApi(5000, 0);
        // A successful delete is reflected locally before LanceDB's next
        // snapshot is observable. Keep the tombstone overlay while this read
        // catches up so a stale snapshot cannot make the row reappear.
        const visibleList = list.filter((memory) => !idsToHide.has(memory.id));
        if (requestId !== fetchRequestId.current) return;
        setMemories(visibleList);
        if (
          visibleList.length > 0 &&
          selectedIdsRef.current.length === 0 &&
          !activeItemRef.current
        ) {
          setActiveItem(visibleList[0]);
          setSelectedIds([visibleList[0].id]);
          setLastAnchorIndex(0);
          populateEditForm(visibleList[0]);
        }
      } catch (err) {
        if (requestId !== fetchRequestId.current) return;
        console.error("Failed to fetch memories from LanceDB:", err);
        showNotice("メモリーの取得に失敗しました", "error");
      } finally {
        if (requestId === fetchRequestId.current) setLoading(false);
      }
    },
    [populateEditForm, showNotice],
  );

  useEffect(() => {
    if (isOpen) {
      void fetchMemories();
    } else {
      setSelectedIds([]);
      setActiveItem(null);
      setContextMenu(null);
      setOptimisticallyDeletedIds(new Set());
      setLastAnchorIndex(null);
      setBlogResult(null);
      setIsCreatingNew(false);
    }
  }, [isOpen, fetchMemories]);

  // フィルタ & ソート
  const filteredAndSortedMemories = useMemo(() => {
    let result = [...memories];
    if (searchQuery.trim()) {
      const q = searchQuery.toLowerCase();
      result = result.filter(
        (m) =>
          (m.content || "").toLowerCase().includes(q) ||
          (m.user || m.source || "").toLowerCase().includes(q) ||
          (m.type || "").toLowerCase().includes(q) ||
          (m.key || m.id || "").toLowerCase().includes(q),
      );
    }

    result.sort((a, b) => {
      let valA = "";
      let valB = "";

      switch (sortField) {
        case "timestamp":
          valA = a.timestamp || "";
          valB = b.timestamp || "";
          break;
        case "key":
          valA = a.key || a.id || "";
          valB = b.key || b.id || "";
          break;
        case "type":
          valA = a.type || "";
          valB = b.type || "";
          break;
        case "user":
          valA = a.user || a.source || "";
          valB = b.user || b.source || "";
          break;
        case "content":
          valA = a.content || "";
          valB = b.content || "";
          break;
      }

      const cmp = valA.localeCompare(valB, undefined, { numeric: true });
      return sortOrder === "asc" ? cmp : -cmp;
    });

    return result;
  }, [memories, searchQuery, sortField, sortOrder]);

  const copySelectedAsJson = useCallback(
    async (overrideIds?: string[]) => {
      const ids = overrideIds ?? selectedIds;
      if (ids.length === 0) return;

      const idSet = new Set(ids);
      const selectedItems = filteredAndSortedMemories.filter((m) =>
        idSet.has(m.id),
      );
      if (selectedItems.length === 0) return;

      try {
        const jsonText = JSON.stringify(selectedItems, null, 2);
        await writeClipboardText(jsonText);
        showNotice(
          `${selectedItems.length} 件のメモリを JSON としてコピーしました`,
          "success",
        );
      } catch (err) {
        console.error("Failed to copy memories as JSON:", err);
        showNotice("クリップボードへのコピーに失敗しました", "error");
      }
    },
    [filteredAndSortedMemories, selectedIds, showNotice],
  );

  // Ctrl+A で全選択, Ctrl+C で選択メモリを JSON コピー
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (!isOpen || !isActiveTab) return;
      const target = e.target as HTMLElement;
      if (
        target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA" ||
        target.isContentEditable
      ) {
        return;
      }

      const isCtrl = e.ctrlKey || e.metaKey;
      const key = e.key.toLowerCase();

      if (isCtrl && key === "a") {
        e.preventDefault();
        setSelectedIds(filteredAndSortedMemories.map((m) => m.id));
        return;
      }

      if (isCtrl && key === "c") {
        const selection = window.getSelection();
        if (selection && selection.toString().length > 0) return;

        if (selectedIds.length > 0) {
          e.preventDefault();
          void copySelectedAsJson();
        }
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [isOpen, isActiveTab, filteredAndSortedMemories, selectedIds, copySelectedAsJson]);

  const handleSort = useCallback(
    (field: SortField) => {
      if (sortField === field) {
        setSortOrder(sortOrder === "asc" ? "desc" : "asc");
      } else {
        setSortField(field);
        setSortOrder("desc");
      }
    },
    [sortField, sortOrder],
  );

  // 選択操作 (Ctrl / Shift / 通常クリック対応)
  const handleRowClick = useCallback(
    (item: MemoryItem, index: number, e: React.MouseEvent) => {
      setContextMenu(null);
      const isCtrl = e.ctrlKey || e.metaKey;
      const isShift = e.shiftKey;

      if (isShift && lastAnchorIndex !== null) {
        const from = Math.min(lastAnchorIndex, index);
        const to = Math.max(lastAnchorIndex, index);
        const rangeIds = filteredAndSortedMemories
          .slice(from, to + 1)
          .map((m) => m.id);

        if (isCtrl) {
          setSelectedIds((prev) => Array.from(new Set([...prev, ...rangeIds])));
        } else {
          setSelectedIds(rangeIds);
        }
      } else if (isCtrl) {
        setSelectedIds((prev) => {
          if (prev.includes(item.id)) {
            return prev.filter((id) => id !== item.id);
          } else {
            return [...prev, item.id];
          }
        });
        setLastAnchorIndex(index);
      } else {
        setSelectedIds([item.id]);
        setLastAnchorIndex(index);
      }

      setActiveItem(item);
      populateEditForm(item);
    },
    [filteredAndSortedMemories, lastAnchorIndex, populateEditForm],
  );

  const handleToggleSelectId = useCallback(
    (id: string, index: number, e: React.MouseEvent) => {
      e.stopPropagation();
      setSelectedIds((prev) => {
        if (prev.includes(id)) {
          return prev.filter((i) => i !== id);
        } else {
          return [...prev, id];
        }
      });
      setLastAnchorIndex(index);
    },
    [],
  );

  const handleSelectAll = useCallback(() => {
    if (selectedIds.length === filteredAndSortedMemories.length) {
      setSelectedIds([]);
    } else {
      setSelectedIds(filteredAndSortedMemories.map((m) => m.id));
    }
  }, [filteredAndSortedMemories, selectedIds.length]);

  // 単一保存 / 新規作成
  const handleSaveSingle = useCallback(async () => {
    if (!editKey.trim()) {
      showNotice("Key を指定してください", "error");
      return;
    }
    if (!editContent.trim()) {
      showNotice("Content（内容）を入力してください", "error");
      return;
    }

    try {
      const itemToSave = {
        id: editKey.trim(),
        document: editContent.trim(),
        memory_type: editType.trim() || "memory",
        source: editUser.trim() || "User",
        timestamp: new Date().toISOString(),
        user_id: editUser.trim() || "User",
      };
      await importMemoriesToLanceApi([itemToSave], null);
      showNotice(
        isCreatingNew
          ? "新規メモリーを作成しました！"
          : "メモリーの変更を保存しました！",
      );
      setIsCreatingNew(false);
      await fetchMemories();
    } catch (e) {
      showNotice(`保存エラー: ${e}`, "error");
    }
  }, [editContent, editKey, editType, editUser, fetchMemories, isCreatingNew, showNotice]);

  // 単一 / 選択中アイテムの削除
  const handleDeleteSelected = useCallback(
    async (idsOverride?: string[]) => {
      const idsToDelete = idsOverride ? [...idsOverride] : [...selectedIds];
      const count = idsToDelete.length;
      if (count === 0) return;

      if (!confirm(`選択した ${count} 件のメモリーを完全に削除しますか？`)) {
        return;
      }

      try {
        await deleteLanceMemoriesBulkApi(idsToDelete);
        const nextHiddenIds = new Set([
          ...optimisticallyDeletedIds,
          ...idsToDelete,
        ]);
        setOptimisticallyDeletedIds(nextHiddenIds);
        // Update the rendered snapshot immediately after the backend confirms
        // the deletion. The follow-up read is still needed for total/count and
        // to reconcile any rows changed by another window.
        setMemories((current) =>
          current.filter((memory) => !idsToDelete.includes(memory.id)),
        );
        showNotice(`${count} 件のメモリーを LanceDB から削除しました`);
        setSelectedIds([]);
        setActiveItem(null);
        setContextMenu(null);
        await fetchMemories(nextHiddenIds);
      } catch (e) {
        showNotice(`削除エラー: ${e}`, "error");
      }
    },
    [fetchMemories, optimisticallyDeletedIds, selectedIds, showNotice],
  );

  // 一括メタデータ更新
  const handleBulkUpdate = useCallback(async () => {
    if (selectedIds.length === 0) return;
    if (!bulkType && !bulkUser) {
      showNotice("一括適用する Type または User を指定してください", "error");
      return;
    }

    try {
      const targetItems = memories.filter((m) => selectedIds.includes(m.id));
      const updatedItems = targetItems.map((m) => ({
        id: m.id,
        document: m.content,
        memory_type: bulkType.trim() || m.type || "memory",
        source: bulkUser.trim() || m.source || "User",
        timestamp: m.timestamp || new Date().toISOString(),
        user_id: bulkUser.trim() || m.user || "User",
      }));

      await updateLanceMemoriesBulkApi(updatedItems);
      showNotice(`${selectedIds.length} 件のメモリーを一括更新しました！`);
      setBulkType("");
      setBulkUser("");
      await fetchMemories();
    } catch (e) {
      showNotice(`一括更新エラー: ${e}`, "error");
    }
  }, [bulkType, bulkUser, fetchMemories, memories, selectedIds, showNotice]);

  // 選択メモリーからのブログ生成
  const handleGenerateBlog = useCallback(
    async (idsOverride?: string[]) => {
      const idsToGenerate = idsOverride ? [...idsOverride] : [...selectedIds];
      if (idsToGenerate.length === 0) {
        showNotice("ブログを生成するメモリーを選択してください", "error");
        return;
      }

      if (isGeneratingBlogRef.current) return;
      isGeneratingBlogRef.current = true;
      setIsGeneratingBlog(true);
      // 失敗時に以前の成功記事を今回の結果と誤認させないため、先に結果を消す。
      setBlogResult(null);
      try {
        const result = await generateBlogFromMemoriesApi(idsToGenerate);
        setBlogResult({ filename: result.filename, content: result.content });
        showNotice("note プレイ日誌記事の生成・保存が完了しました！");
      } catch (e) {
        const reason =
          typeof e === "string"
            ? e
            : e instanceof Error
              ? e.message
              : JSON.stringify(e);
        showNotice(
          /__TAURI_INTERNALS__/.test(reason)
            ? "ブログ生成はTauriアプリ内で実行してください (ブラウザー単体では利用できません)"
            : `ブログ生成エラー: ${reason}`,
          "error",
        );
      } finally {
        isGeneratingBlogRef.current = false;
        setIsGeneratingBlog(false);
      }
    },
    [selectedIds, showNotice],
  );

  const openRawContextMenu = useCallback(
    (
      event: React.MouseEvent<HTMLElement> | React.KeyboardEvent<HTMLElement>,
      item: MemoryItem,
    ) => {
      event.preventDefault();
      if (!selectedIds.includes(item.id)) {
        setSelectedIds([item.id]);
      }
      setActiveItem(item);
      populateEditForm(item);
      const target = event.currentTarget;
      const rect = target.getBoundingClientRect();
      const mouse = event as React.MouseEvent<HTMLElement>;
      setContextMenu({
        x: "clientX" in mouse && mouse.clientX ? mouse.clientX : rect.left + 12,
        y: "clientY" in mouse && mouse.clientY ? mouse.clientY : rect.bottom,
        id: item.id,
      });
    },
    [populateEditForm, selectedIds],
  );

  const rawContextActions = useMemo<ContextMenuAction[]>(() => {
    if (!contextMenu) return [];
    const item = memories.find((memory) => memory.id === contextMenu.id);
    if (!item) return [];
    return [
      {
        label: "Open details",
        onSelect: () => {
          setActiveItem(item);
          populateEditForm(item);
        },
      },
      {
        label: "Edit memory",
        onSelect: () => {
          setActiveItem(item);
          populateEditForm(item);
        },
      },
      {
        label:
          selectedIds.length > 1 && selectedIds.includes(item.id)
            ? `Copy selected as JSON (${selectedIds.length})`
            : "Copy as JSON",
        onSelect: () => {
          if (selectedIds.includes(item.id)) {
            void copySelectedAsJson();
          } else {
            setSelectedIds([item.id]);
            void copySelectedAsJson([item.id]);
          }
        },
      },
      {
        label: "Edit selected metadata",
        disabled: selectedIds.length < 2,
        onSelect: () => setSelectedIds((current) => [...current]),
      },
      {
        label: "Generate blog",
        onSelect: () => void handleGenerateBlog([item.id]),
      },
      {
        label: "Delete memory",
        danger: true,
        onSelect: () => void handleDeleteSelected([item.id]),
      },
    ];
  }, [
    contextMenu,
    memories,
    selectedIds,
    populateEditForm,
    copySelectedAsJson,
    handleGenerateBlog,
    handleDeleteSelected,
  ]);

  const handleBackup = useCallback(async () => {
    try {
      if (
        typeof window !== "undefined" &&
        (window as any).__TAURI_INTERNALS__
      ) {
        const res = await lanceBackupApi();
        showNotice(`バックアップを作成しました: ${res}`);
      }
    } catch (e) {
      showNotice(`バックアップ失敗: ${e}`, "error");
    }
  }, [showNotice]);

  const handleExportJson = useCallback(async () => {
    try {
      if (
        typeof window !== "undefined" &&
        (window as any).__TAURI_INTERNALS__
      ) {
        const res = await lanceExportJsonApi();
        showNotice(res);
      }
    } catch (e) {
      showNotice(`JSONエクスポート失敗: ${e}`, "error");
    }
  }, [showNotice]);

  const startCreateNew = useCallback(() => {
    setContextMenu(null);
    setIsCreatingNew(true);
    setActiveItem(null);
    setSelectedIds([]);
    setEditKey(`mem_${Date.now()}`);
    setEditType("memory");
    setEditUser("User");
    setEditContent("");
  }, []);

  const isBulkMode = selectedIds.length > 1;

  return {
    memories,
    loading,
    searchQuery,
    setSearchQuery,
    selectedIds,
    setSelectedIds,
    activeItem,
    setActiveItem,
    lastAnchorIndex,
    setLastAnchorIndex,
    contextMenu,
    setContextMenu,
    sortField,
    sortOrder,
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
    rawContextActions,
    fetchMemories,
    populateEditForm,
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
    handleBackup,
    handleExportJson,
    startCreateNew,
  };
}
