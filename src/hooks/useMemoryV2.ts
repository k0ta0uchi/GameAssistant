import { useState, useEffect, useMemo, useRef, useCallback } from "react";
import type React from "react";
import { listen } from "@tauri-apps/api/event";
import {
  type FactConflict,
  type FactEvidence,
  type FactMutationTarget,
  type FactRow,
  type MemoryFilters,
  type MemoryPageInfo,
  type MemoryPageRequest,
  type RawEventRow,
  type SummaryRetryResult,
  type SummaryRow,
  normalizeMemoryBackfillProgress,
} from "../types";
import {
  type SemanticTab,
  type SemanticError,
  EMPTY_MEMORY_FILTERS,
  listFactsApi,
  listSummariesApi,
  getFactEvidenceApi,
  getRawEventApi,
  confirmFactsApi,
  editFactApi,
  editFactsBulkApi,
  deleteFactsApi,
  undoMemoryManagerApi,
  retrySummaryApi,
  parseSemanticError,
  normalizeConflict,
  summaryRetryAllowed,
  writeClipboardText,
} from "../services/memoryService";
import type { ContextMenuAction } from "../components/Modals/Memory/MemoryContextMenu";

export interface UseMemoryV2Options {
  isOpen: boolean;
}

export function useMemoryV2({ isOpen }: UseMemoryV2Options) {
  const [subtab, setSubtab] = useState<SemanticTab>("facts");
  const [search, setSearch] = useState("");
  const [filters, setFilters] = useState<MemoryFilters>({
    ...EMPTY_MEMORY_FILTERS,
  });
  const [facts, setFacts] = useState<FactRow[]>([]);
  const [summaries, setSummaries] = useState<SummaryRow[]>([]);
  const [page, setPage] = useState<MemoryPageInfo | null>(null);
  const [cursor, setCursor] = useState<string | null>(null);
  const [cursorHistory, setCursorHistory] = useState<Array<string | null>>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<SemanticError | null>(null);
  const [selectedFactIds, setSelectedFactIds] = useState<string[]>([]);
  const [selectedSummaryIds, setSelectedSummaryIds] = useState<string[]>([]);
  const [activeFact, setActiveFact] = useState<FactRow | null>(null);
  const [activeSummary, setActiveSummary] = useState<SummaryRow | null>(null);
  const [evidence, setEvidence] = useState<FactEvidence[]>([]);
  const [evidenceError, setEvidenceError] = useState<string | null>(null);
  const [evidenceLoading, setEvidenceLoading] = useState(false);
  const [rawEvidence, setRawEvidence] = useState<RawEventRow | null>(null);
  const [predicate, setPredicate] = useState("");
  const [factValue, setFactValue] = useState("");
  const [bulkPredicate, setBulkPredicate] = useState("");
  const [bulkValue, setBulkValue] = useState("");
  const [notice, setNotice] = useState<{
    text: string;
    error?: boolean;
    undo?: string;
  } | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [conflict, setConflict] = useState<FactConflict | null>(null);
  const [mutating, setMutating] = useState(false);
  const [requestVersion, setRequestVersion] = useState(0);
  const [pendingSubtab, setPendingSubtab] = useState<SemanticTab | null>(null);
  const [contextMenu, setContextMenu] = useState<{
    x: number;
    y: number;
    kind: SemanticTab;
    id: string;
  } | null>(null);
  const [optimisticallyDeletedFactIds, setOptimisticallyDeletedFactIds] =
    useState<Set<string>>(() => new Set());
  const [deleteUndoIds, setDeleteUndoIds] = useState<Record<string, string[]>>(
    {},
  );
  const pageRequestId = useRef(0);

  const selectedCount =
    subtab === "facts" ? selectedFactIds.length : selectedSummaryIds.length;
  const rows = subtab === "facts" ? facts : summaries;

  const request: MemoryPageRequest = useMemo(
    () => ({
      page_size: 50,
      cursor,
      search: search.trim() || null,
      sort: "newest",
      filters,
    }),
    [cursor, filters, search],
  );

  const showNotice = useCallback(
    (text: string, errorNotice = false, undoToken?: string | null) => {
      setNotice({ text, error: errorNotice, undo: undoToken || undefined });
      window.setTimeout(() => setNotice(null), 6000);
    },
    [],
  );

  const loadPage = useCallback(
    async (overrides?: {
      factIds?: ReadonlySet<string>;
      request?: MemoryPageRequest;
    }) => {
      if (!isOpen) return;
      const requestId = ++pageRequestId.current;
      setLoading(true);
      setError(null);
      const requestToUse = overrides?.request || request;
      const hiddenFactIds = overrides?.factIds || optimisticallyDeletedFactIds;
      try {
        if (subtab === "facts") {
          const { rows: rawRows, page: rawPage } = await listFactsApi(requestToUse);
          if (requestId !== pageRequestId.current) return;
          const hiddenRowsOnPage = rawRows.filter((row) =>
            hiddenFactIds.has(row.fact_id),
          ).length;
          setFacts(rawRows.filter((row) => !hiddenFactIds.has(row.fact_id)));
          setPage(
            rawPage.total !== null
              ? {
                  ...rawPage,
                  total: Math.max(0, rawPage.total - hiddenRowsOnPage),
                }
              : rawPage,
          );
        } else {
          const { rows: rawRows, page: rawPage } = await listSummariesApi(requestToUse);
          if (requestId !== pageRequestId.current) return;
          setSummaries(rawRows);
          setPage(rawPage);
        }
      } catch (err) {
        if (requestId !== pageRequestId.current) return;
        setError(parseSemanticError(err));
      } finally {
        if (requestId === pageRequestId.current) setLoading(false);
      }
    },
    [isOpen, optimisticallyDeletedFactIds, request, subtab],
  );

  useEffect(() => {
    if (!isOpen) return;
    void loadPage();
  }, [
    isOpen,
    subtab,
    cursor,
    filters,
    search,
    requestVersion,
    optimisticallyDeletedFactIds,
    loadPage,
  ]);

  useEffect(() => {
    if (!isOpen) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let lastPersisted = -1;
    void listen(
      "memory-manager-summary-updated",
      (event: { payload: unknown }) => {
        if (disposed) return;
        const progress = normalizeMemoryBackfillProgress(event.payload);
        if (!progress) return;
        const persistedIncreased = progress.persisted > lastPersisted;
        lastPersisted = Math.max(lastPersisted, progress.persisted);
        if (persistedIncreased || progress.state !== "running")
          setRequestVersion((value) => value + 1);
      },
    )
      .then((dispose) => {
        if (disposed) dispose();
        else unlisten = dispose;
      })
      .catch(() => {
        // Event delivery is advisory; page reads remain authoritative.
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [isOpen]);

  useEffect(() => {
    if (activeFact) {
      setPredicate(activeFact.predicate);
      setFactValue(activeFact.value);
      setEvidence([]);
      setEvidenceError(null);
      setRawEvidence(null);
      setEvidenceLoading(true);
      void getFactEvidenceApi(activeFact.fact_id, { ...request, cursor: null })
        .then((result) => {
          setEvidence(result);
          setEvidenceError(null);
        })
        .catch((error: unknown) => {
          setEvidenceError(parseSemanticError(error).message);
        })
        .finally(() => setEvidenceLoading(false));
    }
  }, [activeFact, request]);

  const resetAndReload = useCallback(() => {
    setContextMenu(null);
    setCursor(null);
    setCursorHistory([]);
    setPage(null);
    setSelectedFactIds([]);
    setSelectedSummaryIds([]);
    setActiveFact(null);
    setActiveSummary(null);
  }, []);

  const switchSubtab = useCallback(
    (next: SemanticTab) => {
      if (next === subtab) return;
      if (
        activeFact &&
        (predicate !== activeFact.predicate || factValue !== activeFact.value)
      ) {
        setPendingSubtab(next);
        return;
      }
      setSubtab(next);
      resetAndReload();
    },
    [activeFact, factValue, predicate, resetAndReload, subtab],
  );

  const discardAndSwitchSubtab = useCallback(() => {
    if (!pendingSubtab) return;
    const next = pendingSubtab;
    setPendingSubtab(null);
    setSubtab(next);
    resetAndReload();
  }, [pendingSubtab, resetAndReload]);

  const mutationTargets = useCallback((): FactMutationTarget[] => {
    return selectedFactIds
      .map((factId) => {
        const fact = facts.find((item) => item.fact_id === factId);
        return fact
          ? {
              fact_id: fact.fact_id,
              expected_revision: fact.revision,
              expected_status: fact.status,
            }
          : null;
      })
      .filter((target): target is FactMutationTarget => target !== null);
  }, [facts, selectedFactIds]);

  const runMutation = useCallback(
    async (
      apiFn: () => Promise<{ changed: boolean; receipt: { undo_token?: string | null } }>,
      success: string,
      onCommitted?: (undoToken: string | null) => ReadonlySet<string> | undefined,
    ) => {
      setMutating(true);
      try {
        const result = await apiFn();
        const receipt = result.receipt;
        showNotice(success, false, receipt.undo_token);
        const hiddenFactIds = onCommitted?.(receipt.undo_token ?? null);
        resetAndReload();
        await loadPage({
          factIds: hiddenFactIds,
          request: { ...request, cursor: null },
        });
      } catch (err) {
        const parsed = parseSemanticError(err);
        const parsedConflict = normalizeConflict(err);
        if (parsedConflict) setConflict(parsedConflict);
        showNotice(parsed.message, true);
      } finally {
        setMutating(false);
      }
    },
    [loadPage, request, resetAndReload, showNotice],
  );

  const editFact = useCallback(async () => {
    if (!activeFact || !predicate.trim() || !factValue.trim()) return;
    await runMutation(
      () =>
        editFactApi({
          fact_id: activeFact.fact_id,
          expected_revision: activeFact.revision,
          expected_status: activeFact.status,
          predicate: predicate.trim(),
          value: factValue.trim(),
        }),
      "Fact updated",
    );
    setActiveFact(null);
  }, [activeFact, factValue, predicate, runMutation]);

  const saveAndSwitchSubtab = useCallback(async () => {
    if (!pendingSubtab) return;
    const next = pendingSubtab;
    await editFact();
    setPendingSubtab(null);
    setSubtab(next);
    resetAndReload();
  }, [editFact, pendingSubtab, resetAndReload]);

  const updateFilter = useCallback((key: keyof MemoryFilters, value: string) => {
    setFilters((previous) => ({ ...previous, [key]: value ? [value] : [] }));
    setCursor(null);
    setCursorHistory([]);
  }, []);

  const toggleSelected = useCallback(
    (id: string) => {
      if (subtab === "facts") {
        setSelectedFactIds((items) =>
          items.includes(id)
            ? items.filter((item) => item !== id)
            : [...items, id],
        );
      } else {
        setSelectedSummaryIds((items) =>
          items.includes(id)
            ? items.filter((item) => item !== id)
            : [...items, id],
        );
      }
    },
    [subtab],
  );

  const selectAllVisible = useCallback(() => {
    const ids = rows.map((row) =>
      subtab === "facts"
        ? (row as FactRow).fact_id
        : (row as SummaryRow).summary_id,
    );
    if (subtab === "facts") {
      setSelectedFactIds(selectedFactIds.length === ids.length ? [] : ids);
    } else {
      setSelectedSummaryIds(
        selectedSummaryIds.length === ids.length ? [] : ids,
      );
    }
  }, [rows, selectedFactIds.length, selectedSummaryIds.length, subtab]);

  const copySelectedSemanticAsJson = useCallback(
    async (overrideIds?: string[]) => {
      const isFacts = subtab === "facts";
      const ids = overrideIds ?? (isFacts ? selectedFactIds : selectedSummaryIds);
      if (ids.length === 0) return;

      const idSet = new Set(ids);
      const selectedItems = isFacts
        ? facts.filter((f) => idSet.has(f.fact_id))
        : summaries.filter((s) => idSet.has(s.summary_id));

      if (selectedItems.length === 0) return;

      try {
        const jsonText = JSON.stringify(selectedItems, null, 2);
        await writeClipboardText(jsonText);
        showNotice(
          `${selectedItems.length} 件の ${isFacts ? "Fact" : "Summary"} を JSON としてコピーしました`,
        );
      } catch (err) {
        console.error("Failed to copy semantic items as JSON:", err);
        showNotice("クリップボードへのコピーに失敗しました", true);
      }
    },
    [facts, selectedFactIds, selectedSummaryIds, showNotice, subtab, summaries],
  );

  useEffect(() => {
    if (!isOpen) return;
    const handleKeyDown = (e: KeyboardEvent) => {
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
        selectAllVisible();
        return;
      }

      if (isCtrl && key === "c") {
        const selection = window.getSelection();
        if (selection && selection.toString().length > 0) return;

        if (selectedCount > 0) {
          e.preventDefault();
          void copySelectedSemanticAsJson();
        }
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [isOpen, selectAllVisible, selectedCount, copySelectedSemanticAsJson]);

  const confirmFacts = useCallback(async () => {
    await runMutation(
      () => confirmFactsApi(mutationTargets()),
      `${selectedFactIds.length} Facts confirmed`,
    );
  }, [mutationTargets, runMutation, selectedFactIds.length]);

  const confirmActiveFact = useCallback(async () => {
    if (!activeFact || activeFact.status !== "auto") return;
    await runMutation(
      () =>
        confirmFactsApi([
          {
            fact_id: activeFact.fact_id,
            expected_revision: activeFact.revision,
            expected_status: activeFact.status,
          },
        ]),
      "Fact confirmed",
    );
  }, [activeFact, runMutation]);

  const editFactsBulk = useCallback(async () => {
    if (
      selectedFactIds.length === 0 ||
      (!bulkPredicate.trim() && !bulkValue.trim())
    )
      return;
    await runMutation(
      () =>
        editFactsBulkApi(
          mutationTargets(),
          bulkPredicate.trim() || null,
          bulkValue.trim() || null,
        ),
      `${selectedFactIds.length} Facts updated`,
    );
    setBulkPredicate("");
    setBulkValue("");
  }, [bulkPredicate, bulkValue, mutationTargets, runMutation, selectedFactIds.length]);

  const deleteFacts = useCallback(async () => {
    setConfirmDelete(false);
    const ids = [...selectedFactIds];
    const expectedRevisions: Record<string, number> = {};
    ids.forEach((id) => {
      const revision = facts.find((fact) => fact.fact_id === id)?.revision;
      if (typeof revision === "number") expectedRevisions[id] = revision;
    });
    await runMutation(
      () => deleteFactsApi(ids, expectedRevisions),
      `${ids.length} Facts deleted; source Raw events retained`,
      (undoToken) => {
        const hidden = new Set([...optimisticallyDeletedFactIds, ...ids]);
        setOptimisticallyDeletedFactIds(hidden);
        setFacts((current) =>
          current.filter((fact) => !ids.includes(fact.fact_id)),
        );
        setSelectedFactIds((current) =>
          current.filter((factId) => !ids.includes(factId)),
        );
        if (activeFact && ids.includes(activeFact.fact_id)) {
          setActiveFact(null);
          setEvidence([]);
          setEvidenceError(null);
          setRawEvidence(null);
        }
        if (undoToken) {
          setDeleteUndoIds((current) => ({ ...current, [undoToken]: ids }));
        }
        return hidden;
      },
    );
  }, [activeFact, facts, optimisticallyDeletedFactIds, runMutation, selectedFactIds]);

  const undo = useCallback(
    async (token: string) => {
      try {
        await undoMemoryManagerApi(token);
        const restoredIds = deleteUndoIds[token] || [];
        const visibleFactIds = new Set(optimisticallyDeletedFactIds);
        restoredIds.forEach((factId) => visibleFactIds.delete(factId));
        setOptimisticallyDeletedFactIds(visibleFactIds);
        setDeleteUndoIds((current) => {
          const next = { ...current };
          delete next[token];
          return next;
        });
        showNotice("Undo completed");
        resetAndReload();
        await loadPage({
          factIds: visibleFactIds,
          request: { ...request, cursor: null },
        });
      } catch (err) {
        showNotice(parseSemanticError(err).message || "Undo expired", true);
      }
    },
    [deleteUndoIds, loadPage, optimisticallyDeletedFactIds, request, resetAndReload, showNotice],
  );

  const retrySummary = useCallback(
    async (summary: SummaryRow) => {
      setMutating(true);
      try {
        const retry: SummaryRetryResult = await retrySummaryApi(
          summary.event_id,
          summary.status,
        );
        showNotice(`Summary retry queued (${retry.attempt_id})`);
        await loadPage();
      } catch (err) {
        showNotice(parseSemanticError(err).message, true);
      } finally {
        setMutating(false);
      }
    },
    [loadPage, showNotice],
  );

  const hydrateRawEvidence = useCallback(
    async (eventId: string) => {
      try {
        const event = await getRawEventApi(eventId);
        setRawEvidence(event);
      } catch (err) {
        showNotice(parseSemanticError(err).message, true);
      }
    },
    [showNotice],
  );

  const openFact = useCallback((fact: FactRow) => {
    setActiveFact(fact);
    setActiveSummary(null);
    setContextMenu(null);
  }, []);

  const openSummary = useCallback((summary: SummaryRow) => {
    setActiveSummary(summary);
    setActiveFact(null);
    setContextMenu(null);
  }, []);

  const openSemanticContextMenu = useCallback(
    (
      event: React.MouseEvent<HTMLElement> | React.KeyboardEvent<HTMLElement>,
      kind: SemanticTab,
      id: string,
    ) => {
      event.preventDefault();
      const row =
        kind === "facts"
          ? facts.find((fact) => fact.fact_id === id)
          : summaries.find((summary) => summary.summary_id === id);
      if (!row) return;
      if (kind === "facts") {
        const fact = row as FactRow;
        setSelectedFactIds([fact.fact_id]);
        openFact(fact);
      } else {
        const summary = row as SummaryRow;
        setSelectedSummaryIds([summary.summary_id]);
        openSummary(summary);
      }
      const target = event.currentTarget;
      const rect = target.getBoundingClientRect();
      const mouse = event as React.MouseEvent<HTMLElement>;
      setContextMenu({
        x: "clientX" in mouse && mouse.clientX ? mouse.clientX : rect.left + 12,
        y: "clientY" in mouse && mouse.clientY ? mouse.clientY : rect.bottom,
        kind,
        id,
      });
    },
    [facts, openFact, openSummary, summaries],
  );

  const semanticContextActions = useMemo<ContextMenuAction[]>(() => {
    if (!contextMenu) return [];
    if (contextMenu.kind === "facts") {
      const fact = facts.find((item) => item.fact_id === contextMenu.id);
      if (!fact) return [];
      return [
        { label: "Open details", onSelect: () => openFact(fact) },
        { label: "Edit fact", onSelect: () => openFact(fact) },
        {
          label: "Confirm fact",
          disabled: fact.status !== "auto" || mutating,
          onSelect: () => {
            setSelectedFactIds([fact.fact_id]);
            void confirmActiveFact();
          },
        },
        {
          label:
            selectedFactIds.length > 1 && selectedFactIds.includes(fact.fact_id)
              ? `Copy selected as JSON (${selectedFactIds.length})`
              : "Copy as JSON",
          onSelect: () => {
            if (selectedFactIds.includes(fact.fact_id)) {
              void copySelectedSemanticAsJson();
            } else {
              setSelectedFactIds([fact.fact_id]);
              void copySelectedSemanticAsJson([fact.fact_id]);
            }
          },
        },
        {
          label: "Delete fact",
          danger: true,
          disabled: mutating,
          onSelect: () => {
            setSelectedFactIds([fact.fact_id]);
            setConfirmDelete(true);
          },
        },
      ];
    }
    const summary = summaries.find(
      (item) => item.summary_id === contextMenu.id,
    );
    if (!summary) return [];
    return [
      { label: "Open details", onSelect: () => openSummary(summary) },
      {
        label:
          selectedSummaryIds.length > 1 &&
          selectedSummaryIds.includes(summary.summary_id)
            ? `Copy selected as JSON (${selectedSummaryIds.length})`
            : "Copy as JSON",
        onSelect: () => {
          if (selectedSummaryIds.includes(summary.summary_id)) {
            void copySelectedSemanticAsJson();
          } else {
            setSelectedSummaryIds([summary.summary_id]);
            void copySelectedSemanticAsJson([summary.summary_id]);
          }
        },
      },
      {
        label: "Retry summary",
        disabled: !summaryRetryAllowed(summary) || mutating,
        onSelect: () => void retrySummary(summary),
      },
      {
        label: "View raw evidence",
        onSelect: () => void hydrateRawEvidence(summary.event_id),
      },
    ];
  }, [
    confirmActiveFact,
    contextMenu,
    copySelectedSemanticAsJson,
    facts,
    hydrateRawEvidence,
    mutating,
    openFact,
    openSummary,
    retrySummary,
    selectedFactIds,
    selectedSummaryIds,
    summaries,
  ]);

  const resolveConflict = useCallback(
    (resolution: FactConflict["resolution"]) => {
      if (resolution === "keep_current" && conflict) {
        setActiveFact(conflict.current);
        setConflict(null);
      } else if (resolution === "cancel") {
        setConflict(null);
      } else if (conflict) {
        setActiveFact(conflict.current);
        setPredicate(conflict.attempted.predicate);
        setFactValue(conflict.attempted.value);
        setConflict(null);
        showNotice(
          "Review the current revision, then save your attempted values.",
          true,
        );
      }
    },
    [conflict, showNotice],
  );

  const handleNextPage = useCallback(() => {
    if (!page?.next_cursor) return;
    setCursorHistory((current) => [...current, cursor]);
    setCursor(page.next_cursor);
  }, [cursor, page?.next_cursor]);

  const handlePrevPage = useCallback(() => {
    const nextHistory = [...cursorHistory];
    const previousCursor = nextHistory.pop() ?? null;
    setCursorHistory(nextHistory);
    setCursor(previousCursor);
  }, [cursorHistory]);

  return {
    subtab,
    setSubtab,
    switchSubtab,
    pendingSubtab,
    setPendingSubtab,
    discardAndSwitchSubtab,
    saveAndSwitchSubtab,
    search,
    setSearch,
    filters,
    setFilters,
    updateFilter,
    facts,
    summaries,
    page,
    cursor,
    setCursor,
    cursorHistory,
    setCursorHistory,
    loading,
    error,
    selectedFactIds,
    setSelectedFactIds,
    selectedSummaryIds,
    setSelectedSummaryIds,
    selectedCount,
    rows,
    activeFact,
    setActiveFact,
    activeSummary,
    setActiveSummary,
    evidence,
    evidenceError,
    evidenceLoading,
    rawEvidence,
    setRawEvidence,
    predicate,
    setPredicate,
    factValue,
    setFactValue,
    bulkPredicate,
    setBulkPredicate,
    bulkValue,
    setBulkValue,
    notice,
    showNotice,
    confirmDelete,
    setConfirmDelete,
    conflict,
    setConflict,
    resolveConflict,
    mutating,
    contextMenu,
    setContextMenu,
    semanticContextActions,
    loadPage,
    resetAndReload,
    toggleSelected,
    selectAllVisible,
    copySelectedSemanticAsJson,
    confirmFacts,
    confirmActiveFact,
    editFact,
    editFactsBulk,
    deleteFacts,
    undo,
    retrySummary,
    hydrateRawEvidence,
    openFact,
    openSummary,
    openSemanticContextMenu,
    handleNextPage,
    handlePrevPage,
  };
}
