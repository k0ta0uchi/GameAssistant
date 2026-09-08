import type React from "react";
import { useState, useEffect, useMemo, useLayoutEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
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
  AlertTriangle,
  ChevronLeft,
  ChevronRight,
  Eye,
  RotateCcw,
} from "lucide-react";
import { LiveLogTerminal } from "../Console/LiveLogTerminal";
import {
  type FactConflict,
  type FactEvidence,
  type FactMutationTarget,
  type FactRow,
  type FactStatus,
  type MemoryFilters,
  type MemoryPageInfo,
  type MemoryPageRequest,
  type RawEventRow,
  type SummaryRetryResult,
  type SummaryRow,
  type SummaryStatus,
  type MemoryItem,
  type MemoryMigrationStatus,
  type MemoryBackfillProgress,
  type LogEntry,
  normalizeMemoryBackfillProgress,
  normalizeMemoryMigrationStatus,
} from "../../types";

type SemanticTab = "facts" | "summaries";
type SemanticError = { message: string; retryable: boolean };

interface ContextMenuAction {
  label: string;
  onSelect: () => void;
  disabled?: boolean;
  danger?: boolean;
}

interface ContextMenuProps {
  x: number;
  y: number;
  label: string;
  actions: ContextMenuAction[];
  onClose: () => void;
}

/**
 * A small, keyboard-complete context menu shared by the raw and semantic
 * memory views.  The native context menu is suppressed only for the memory
 * row itself; Escape, outside click, and the ContextMenu/Shift+F10 keys all
 * dismiss it again.
 */
const MemoryContextMenu: React.FC<ContextMenuProps> = ({
  x,
  y,
  label,
  actions,
  onClose,
}) => {
  const menuRef = useRef<HTMLDivElement | null>(null);
  const actionRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const [position, setPosition] = useState({ x, y });

  useLayoutEffect(() => {
    const viewportWidth =
      typeof window === "undefined" ? 1024 : window.innerWidth;
    const viewportHeight =
      typeof window === "undefined" ? 768 : window.innerHeight;
    const menuWidth = 236;
    const menuHeight = Math.min(360, Math.max(64, actions.length * 38 + 20));
    setPosition({
      x: Math.max(8, Math.min(x, viewportWidth - menuWidth - 8)),
      y: Math.max(8, Math.min(y, viewportHeight - menuHeight - 8)),
    });
  }, [actions.length, x, y]);

  useEffect(() => {
    const firstEnabled = actions.findIndex((action) => !action.disabled);
    if (firstEnabled >= 0) actionRefs.current[firstEnabled]?.focus();

    const handlePointerDown = (event: PointerEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) onClose();
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
        return;
      }
      const enabledIndexes = actions
        .map((action, index) => (action.disabled ? -1 : index))
        .filter((index) => index >= 0);
      if (
        enabledIndexes.length === 0 ||
        !["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)
      )
        return;
      event.preventDefault();
      const activeIndex = enabledIndexes.indexOf(
        actionRefs.current.findIndex(
          (button) => button === document.activeElement,
        ),
      );
      const current = activeIndex < 0 ? 0 : activeIndex;
      const next =
        event.key === "Home"
          ? 0
          : event.key === "End"
            ? enabledIndexes.length - 1
            : (current +
                (event.key === "ArrowUp" ? -1 : 1) +
                enabledIndexes.length) %
              enabledIndexes.length;
      actionRefs.current[enabledIndexes[next]]?.focus();
    };
    document.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [actions, onClose]);

  return (
    <div
      ref={menuRef}
      role="menu"
      aria-label={label}
      className="fixed z-[80] min-w-[236px] max-w-[280px] overflow-y-auto rounded-[7px] border border-[#383b3f] bg-[#161718] p-1.5 shadow-2xl shadow-black/50"
      style={{
        left: position.x,
        top: position.y,
        maxHeight: "min(360px, calc(100vh - 16px))",
      }}
      onPointerDown={(event) => event.stopPropagation()}
    >
      {actions.map((action, index) => (
        <button
          key={action.label}
          ref={(button) => {
            actionRefs.current[index] = button;
          }}
          type="button"
          role="menuitem"
          disabled={action.disabled}
          onClick={() => {
            onClose();
            if (!action.disabled) action.onSelect();
          }}
          className={`flex w-full items-center rounded-[5px] px-2.5 py-2 text-left text-xs transition-colors disabled:cursor-not-allowed disabled:opacity-35 ${
            action.danger
              ? "text-[#f87171] hover:bg-[#eb5757]/10"
              : "text-[#d0d6e0] hover:bg-[#23252a] hover:text-white"
          }`}
        >
          {action.label}
        </button>
      ))}
    </div>
  );
};

const EMPTY_MEMORY_FILTERS: MemoryFilters = {
  statuses: [],
  sources: [],
  event_types: [],
  subjects: [],
  occurred_from: null,
  occurred_to: null,
  has_summary: null,
};

const FACT_STATUSES: FactStatus[] = ["auto", "confirmed", "edited"];
const SUMMARY_STATUSES: SummaryStatus[] = [
  "pending",
  "completed",
  "skipped",
  "fallback",
  "error",
  "legacy",
  "deleted",
];

const isRecord = (value: unknown): value is Record<string, unknown> =>
  Boolean(value) && typeof value === "object";

const stringValue = (value: unknown): string | null =>
  typeof value === "string" ? value : null;

const numberValue = (value: unknown): number | null =>
  typeof value === "number" && Number.isFinite(value) ? value : null;

const arrayOfStrings = (value: unknown): string[] =>
  Array.isArray(value)
    ? value.filter((entry): entry is string => typeof entry === "string")
    : [];

const SUMMARY_REASON_CODES = [
  "policy_excluded",
  "model_declined",
  "metadata_echo",
  "ungrounded_summary",
  "invalid_model_output",
  "empty_source",
  "empty_summary",
  "source_too_long",
  "summary_runtime_timeout",
  "summary_runtime_failed",
  "summary_queue_failed",
  "embedding_failed",
  "invalid_embedding",
  "fact_validation_failed",
  "subject_unresolved",
  "journal_commit_failed",
  "not_candidate",
  "candidate_not_eligible",
  "source_not_allowed",
  "projection_resync_failed",
  "concurrent_state_changed",
  "durable_summary_exists",
  "terminal_status",
  "delete_tombstone",
  "inference_failed",
] as const;

const normalizeSummaryReason = (value: unknown): string | null => {
  if (typeof value !== "string" || !value.trim()) return null;
  const normalized = value.trim().toLowerCase();
  return (
    SUMMARY_REASON_CODES.find(
      (reason) => normalized === reason || normalized.includes(reason),
    ) || "inference_failed"
  );
};

const normalizePage = (value: unknown): MemoryPageInfo | null => {
  if (!isRecord(value)) return null;
  const sequence = numberValue(value.snapshot_sequence);
  const hasMore = typeof value.has_more === "boolean" ? value.has_more : null;
  if (sequence === null || hasMore === null) return null;
  return {
    next_cursor: stringValue(value.next_cursor),
    has_more: hasMore,
    total: numberValue(value.total),
    snapshot_sequence: sequence,
  };
};

const normalizeRawRow = (value: unknown): RawEventRow | null => {
  if (!isRecord(value)) return null;
  const eventId = stringValue(value.event_id);
  const occurredAt = stringValue(value.occurred_at);
  if (!eventId || !occurredAt) return null;
  const status = stringValue(value.summary_status);
  const vectorSource = stringValue(value.vector_source);
  return {
    event_id: eventId,
    legacy_id: stringValue(value.legacy_id),
    subject: stringValue(value.subject) || "",
    event_type: stringValue(value.event_type) || "",
    source: stringValue(value.source) || "",
    occurred_at: occurredAt,
    content_preview: stringValue(value.content_preview) || "",
    content: stringValue(value.content),
    summary_status:
      status && SUMMARY_STATUSES.includes(status as SummaryStatus)
        ? (status as SummaryStatus)
        : null,
    derived_fact_ids: arrayOfStrings(value.derived_fact_ids),
    vector_source:
      vectorSource === "document" ||
      vectorSource === "summary" ||
      vectorSource === "none"
        ? vectorSource
        : null,
  };
};

const normalizeFactRow = (value: unknown): FactRow | null => {
  if (!isRecord(value)) return null;
  const factId = stringValue(value.fact_id);
  const status = stringValue(value.status);
  const revision = numberValue(value.revision);
  if (
    !factId ||
    revision === null ||
    revision < 0 ||
    !status ||
    !FACT_STATUSES.includes(status as FactStatus)
  )
    return null;
  return {
    fact_id: factId,
    subject: stringValue(value.subject) || "",
    predicate: stringValue(value.predicate) || "",
    key: stringValue(value.key) || "",
    value: stringValue(value.value) || "",
    status: status as FactStatus,
    evidence_count: Math.max(
      0,
      Math.floor(numberValue(value.evidence_count) || 0),
    ),
    latest_evidence_at: stringValue(value.latest_evidence_at),
    source_event_ids: arrayOfStrings(value.source_event_ids),
    revision,
    operation_id: stringValue(value.operation_id) || "",
  };
};

const normalizeSummaryRow = (value: unknown): SummaryRow | null => {
  if (!isRecord(value)) return null;
  const summaryId = stringValue(value.summary_id);
  const eventId = stringValue(value.event_id);
  const occurredAt = stringValue(value.occurred_at);
  const status = stringValue(value.status);
  if (
    !summaryId ||
    !eventId ||
    !occurredAt ||
    !status ||
    !SUMMARY_STATUSES.includes(status as SummaryStatus)
  )
    return null;
  const vectorSource = stringValue(value.vector_source);
  const rawReason = stringValue(value.reason) || stringValue(value.error);
  const reason = normalizeSummaryReason(rawReason);
  const retryCount = numberValue(
    value.retry_count === undefined ? value.retryCount : value.retry_count,
  );
  const attemptId =
    stringValue(value.attempt_id) || stringValue(value.attemptId);
  return {
    summary_id: summaryId,
    event_id: eventId,
    summary: stringValue(value.summary),
    status: status as SummaryStatus,
    error: stringValue(value.error) || reason,
    reason,
    model_id: stringValue(value.model_id),
    prompt_version: stringValue(value.prompt_version),
    attempt_id: attemptId,
    retry_count: retryCount === null ? 0 : Math.max(0, Math.floor(retryCount)),
    vector_source:
      vectorSource === "document" ||
      vectorSource === "summary" ||
      vectorSource === "none"
        ? vectorSource
        : null,
    derived_fact_id: stringValue(value.derived_fact_id),
    occurred_at: occurredAt,
    source: stringValue(value.source) || "",
    event_type: stringValue(value.event_type) || "",
  };
};

const parseSemanticError = (error: unknown): SemanticError => {
  const fallback =
    error instanceof Error
      ? error.message
      : String(error || "Memory manager request failed");
  let parsed: Record<string, unknown> | null = null;
  if (typeof error === "string") {
    try {
      const candidate = JSON.parse(error) as unknown;
      parsed = isRecord(candidate) ? candidate : null;
    } catch {
      parsed = null;
    }
  } else if (isRecord(error)) {
    parsed = error;
  }
  return {
    message: stringValue(parsed?.message) || fallback,
    retryable:
      parsed?.retryable === true ||
      parsed?.code === "storage_unavailable" ||
      parsed?.code === "summary_unavailable",
  };
};

const normalizeConflict = (error: unknown): FactConflict | null => {
  if (!isRecord(error)) return null;
  const details = isRecord(error.details) ? error.details : error;
  const current = normalizeFactRow(details.current);
  const attempted = isRecord(details.attempted) ? details.attempted : null;
  const conflictId = stringValue(details.conflict_id);
  if (!current || !attempted || !conflictId) return null;
  const expectedRevision = numberValue(attempted.expected_revision);
  const expectedStatus = stringValue(attempted.expected_status);
  if (
    expectedRevision === null ||
    !expectedStatus ||
    !FACT_STATUSES.includes(expectedStatus as FactStatus)
  )
    return null;
  const rawEvidence = Array.isArray(details.evidence) ? details.evidence : [];
  return {
    conflict_id: conflictId,
    fact_id: current.fact_id,
    current,
    attempted: {
      predicate: stringValue(attempted.predicate) || "",
      value: stringValue(attempted.value) || "",
      expected_revision: expectedRevision,
      expected_status: expectedStatus as FactStatus,
    },
    evidence: rawEvidence.filter(isRecord).map((entry) => ({
      event_id: stringValue(entry.event_id) || "",
      subject: stringValue(entry.subject) || "",
      event_type: stringValue(entry.event_type) || "",
      source: stringValue(entry.source) || "",
      occurred_at: stringValue(entry.occurred_at) || "",
      content: stringValue(entry.content) || "",
      relation: (stringValue(entry.relation) ||
        "supporting") as FactEvidence["relation"],
      match_score: numberValue(entry.match_score),
    })),
    resolution: "cancel",
  };
};

const hasMutationReceipt = (
  value: unknown,
): value is { undo_token?: unknown } => {
  if (!isRecord(value)) return false;
  return (
    typeof value.operation_id === "string" &&
    numberValue(value.journal_sequence) !== null &&
    typeof value.committed_at === "string"
  );
};

const statusBadgeClass = (status: string): string =>
  status === "confirmed" || status === "completed"
    ? "bg-[#27a644]/15 text-[#4ade80] border-[#27a644]/30"
    : status === "error"
      ? "bg-[#eb5757]/15 text-[#f87171] border-[#eb5757]/30"
      : status === "fallback" || status === "pending"
        ? "bg-[#e4f222]/15 text-[#e4f222] border-[#e4f222]/30"
        : "bg-[#23252a] text-[#8a8f98] border-[#383b3f]";

const summaryReasonLabel = (reason: string | null): string => {
  switch (reason) {
    case "policy_excluded":
      return "Fact化はプライバシーポリシーにより除外";
    case "model_declined":
      return "モデルが保存不要と判定";
    case "metadata_echo":
      return "メタデータだけの出力を拒否（再試行可能）";
    case "ungrounded_summary":
      return "未根拠の要約を拒否（再試行可能）";
    case "invalid_model_output":
      return "モデル出力が契約外（再試行可能）";
    case "summary_runtime_timeout":
      return "推論ランタイムのタイムアウト（再試行可能）";
    case "summary_runtime_failed":
      return "推論ランタイムに失敗（再試行可能）";
    case "summary_queue_failed":
      return "推論キューに失敗（再試行可能）";
    case "inference_failed":
      return "要約推論に失敗（原因不明、Raw は保持）";
    case "empty_summary":
      return "要約が空のため保存できません";
    case "journal_commit_failed":
      return "ジャーナルの永続化に失敗（致命的エラー）";
    case "invalid_embedding":
      return "要約ベクトルが不正のため保存できません";
    case "embedding_failed":
      return "要約ベクトルの取得に失敗";
    case "fact_validation_failed":
      return "Fact の検証に失敗";
    case "subject_unresolved":
      return "話者の識別情報がないため Fact 化を保留";
    case "not_candidate":
    case "candidate_not_eligible":
    case "source_not_allowed":
      return "候補外のため自動処理対象外";
    default:
      return reason || "";
  }
};

const summaryRetryAllowed = (summary: SummaryRow): boolean =>
  (summary.status === "fallback" || summary.status === "error") &&
  [
    "metadata_echo",
    "ungrounded_summary",
    "invalid_model_output",
    "summary_runtime_timeout",
    "summary_runtime_failed",
    "summary_queue_failed",
    "embedding_failed",
    "invalid_embedding",
    "fact_validation_failed",
    "inference_failed",
  ].includes(summary.reason || summary.error || "");

const SemanticBadge: React.FC<{ status: string }> = ({ status }) => (
  <span
    className={`inline-flex rounded border px-1.5 py-0.5 text-[10px] font-mono ${statusBadgeClass(status)}`}
  >
    {status}
  </span>
);

interface FactSummaryManagerProps {
  isOpen: boolean;
}

const FactSummaryManager: React.FC<FactSummaryManagerProps> = ({ isOpen }) => {
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

  const showNotice = (
    text: string,
    errorNotice = false,
    undoToken?: string | null,
  ) => {
    setNotice({ text, error: errorNotice, undo: undoToken || undefined });
    window.setTimeout(() => setNotice(null), 6000);
  };

  const loadPage = async (overrides?: {
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
      const command =
        subtab === "facts"
          ? "memory_manager_list_facts"
          : "memory_manager_list_summaries";
      const result = await invoke(command, { request: requestToUse });
      if (requestId !== pageRequestId.current) return;
      if (!isRecord(result) || !Array.isArray(result.rows))
        throw new Error("Invalid memory manager response");
      const normalizedPage = normalizePage(result.page);
      if (!normalizedPage)
        throw new Error("Invalid memory manager page response");
      let hiddenRowsOnPage = 0;
      if (subtab === "facts") {
        const normalizedRows = result.rows.map(normalizeFactRow);
        if (normalizedRows.some((row) => row === null))
          throw new Error("Invalid memory manager fact row");
        hiddenRowsOnPage = (normalizedRows as FactRow[]).filter((row) =>
          hiddenFactIds.has(row.fact_id),
        ).length;
        setFacts(
          (normalizedRows as FactRow[]).filter(
            (row) => !hiddenFactIds.has(row.fact_id),
          ),
        );
      } else {
        const normalizedRows = result.rows.map(normalizeSummaryRow);
        if (normalizedRows.some((row) => row === null))
          throw new Error("Invalid memory manager summary row");
        setSummaries(normalizedRows as SummaryRow[]);
      }
      setPage(
        subtab === "facts" && normalizedPage.total !== null
          ? {
              ...normalizedPage,
              total: Math.max(0, normalizedPage.total - hiddenRowsOnPage),
            }
          : normalizedPage,
      );
    } catch (err) {
      if (requestId !== pageRequestId.current) return;
      setError(parseSemanticError(err));
      // Keep the last authoritative page visible behind the error state. A
      // failed read is not an empty result and must never erase rows that the
      // user may still need to inspect or retry.
    } finally {
      if (requestId === pageRequestId.current) setLoading(false);
    }
  };

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
        // Backfill emits this advisory event after each durable chunk. Refresh
        // only when the persisted count advances (or at terminal state) so
        // status reads do not race every inference progress update.
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
      void invoke("memory_manager_get_fact_evidence", {
        // Tauri exposes scalar command parameters in camelCase.  The old
        // snake_case key is rejected before the command body runs.
        factId: activeFact.fact_id,
        page: { ...request, cursor: null },
      })
        .then((result: unknown) => {
          if (!isRecord(result) || !Array.isArray(result.evidence))
            throw new Error("Invalid evidence response");
          setEvidence(
            result.evidence.filter(isRecord).map((entry) => ({
              event_id: stringValue(entry.event_id) || "",
              subject: stringValue(entry.subject) || "",
              event_type: stringValue(entry.event_type) || "",
              source: stringValue(entry.source) || "",
              occurred_at: stringValue(entry.occurred_at) || "",
              content: stringValue(entry.content) || "",
              relation: (stringValue(entry.relation) ||
                "supporting") as FactEvidence["relation"],
              match_score: numberValue(entry.match_score),
            })),
          );
          setEvidenceError(null);
        })
        .catch((error: unknown) => {
          // A failed evidence read is not an empty evidence set. Preserve the
          // last authoritative rows and show the read failure explicitly.
          setEvidenceError(parseSemanticError(error).message);
        })
        .finally(() => setEvidenceLoading(false));
    }
  }, [activeFact?.fact_id]);

  const resetAndReload = () => {
    setContextMenu(null);
    setCursor(null);
    setCursorHistory([]);
    setPage(null);
    setSelectedFactIds([]);
    setSelectedSummaryIds([]);
    setActiveFact(null);
    setActiveSummary(null);
  };

  const switchSubtab = (next: SemanticTab) => {
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
  };

  const discardAndSwitchSubtab = () => {
    if (!pendingSubtab) return;
    setPendingSubtab(null);
    setSubtab(pendingSubtab);
    resetAndReload();
  };

  const saveAndSwitchSubtab = async () => {
    if (!pendingSubtab) return;
    const next = pendingSubtab;
    await editFact();
    setPendingSubtab(null);
    setSubtab(next);
    resetAndReload();
  };

  const updateFilter = (key: keyof MemoryFilters, value: string) => {
    setFilters((previous) => ({ ...previous, [key]: value ? [value] : [] }));
    setCursor(null);
    setCursorHistory([]);
  };

  const toggleSelected = (id: string) => {
    if (subtab === "facts")
      setSelectedFactIds((items) =>
        items.includes(id)
          ? items.filter((item) => item !== id)
          : [...items, id],
      );
    else
      setSelectedSummaryIds((items) =>
        items.includes(id)
          ? items.filter((item) => item !== id)
          : [...items, id],
      );
  };

  const selectAllVisible = () => {
    const ids = rows.map((row) =>
      subtab === "facts"
        ? (row as FactRow).fact_id
        : (row as SummaryRow).summary_id,
    );
    if (subtab === "facts")
      setSelectedFactIds(selectedFactIds.length === ids.length ? [] : ids);
    else
      setSelectedSummaryIds(
        selectedSummaryIds.length === ids.length ? [] : ids,
      );
  };

  const mutationTargets = (): FactMutationTarget[] =>
    selectedFactIds
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

  const runMutation = async (
    command: string,
    args: unknown,
    success: string,
    onCommitted?: (undoToken: string | null) => ReadonlySet<string> | undefined,
  ) => {
    setMutating(true);
    try {
      const result = await invoke(command, args as Record<string, unknown>);
      if (
        !isRecord(result) ||
        result.changed !== true ||
        !hasMutationReceipt(result.receipt)
      )
        throw new Error("Invalid mutation receipt");
      const receipt = result.receipt;
      showNotice(success, false, stringValue(receipt.undo_token));
      const hiddenFactIds = onCommitted?.(stringValue(receipt.undo_token));
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
  };

  const confirmFacts = async () => {
    await runMutation(
      "memory_manager_confirm_facts",
      { targets: mutationTargets() },
      `${selectedFactIds.length} Facts confirmed`,
    );
  };

  const confirmActiveFact = async () => {
    if (!activeFact || activeFact.status !== "auto") return;
    await runMutation(
      "memory_manager_confirm_facts",
      {
        targets: [
          {
            fact_id: activeFact.fact_id,
            expected_revision: activeFact.revision,
            expected_status: activeFact.status,
          },
        ],
      },
      "Fact confirmed",
    );
  };

  const editFact = async () => {
    if (!activeFact || !predicate.trim() || !factValue.trim()) return;
    await runMutation(
      "memory_manager_edit_fact",
      {
        edit: {
          fact_id: activeFact.fact_id,
          expected_revision: activeFact.revision,
          expected_status: activeFact.status,
          predicate: predicate.trim(),
          value: factValue.trim(),
        },
      },
      "Fact updated",
    );
    setActiveFact(null);
  };

  const editFactsBulk = async () => {
    if (
      selectedFactIds.length === 0 ||
      (!bulkPredicate.trim() && !bulkValue.trim())
    )
      return;
    await runMutation(
      "memory_manager_edit_facts_bulk",
      {
        edit: {
          targets: mutationTargets(),
          predicate: bulkPredicate.trim() || null,
          value: bulkValue.trim() || null,
        },
      },
      `${selectedFactIds.length} Facts updated`,
    );
    setBulkPredicate("");
    setBulkValue("");
  };

  const deleteFacts = async () => {
    setConfirmDelete(false);
    const ids = [...selectedFactIds];
    const expectedRevisions: Record<string, number> = {};
    ids.forEach((id) => {
      const revision = facts.find((fact) => fact.fact_id === id)?.revision;
      if (typeof revision === "number") expectedRevisions[id] = revision;
    });
    await runMutation(
      "memory_manager_delete_facts",
      {
        request: {
          fact_ids: ids,
          expected_revisions: expectedRevisions,
        },
      },
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
  };

  const undo = async (token: string) => {
    try {
      await invoke("memory_manager_undo", { request: { undo_token: token } });
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
  };

  const retrySummary = async (summary: SummaryRow) => {
    setMutating(true);
    try {
      const result = await invoke("memory_manager_retry_summary", {
        request: {
          event_id: summary.event_id,
          expected_status: summary.status,
        },
      });
      // SAFETY: invoke returns an untyped payload. The guards in this
      // expression (isRecord + string attempt_id + status === 'pending')
      // establish the SummaryRetryResult receipt shape that TypeScript
      // cannot infer from the command boundary.
      const retry =
        isRecord(result) &&
        stringValue(result.attempt_id) &&
        result.status === "pending" &&
        hasMutationReceipt(result.receipt)
          ? (result as unknown as SummaryRetryResult)
          : null;
      if (!retry) throw new Error("Invalid summary retry response");
      showNotice(`Summary retry queued (${retry.attempt_id})`);
      await loadPage();
    } catch (err) {
      showNotice(parseSemanticError(err).message, true);
    } finally {
      setMutating(false);
    }
  };

  const hydrateRawEvidence = async (eventId: string) => {
    try {
      const result = await invoke("memory_manager_get_raw_event", {
        // Tauri exposes scalar command parameters in camelCase.
        eventId,
      });
      if (!isRecord(result)) throw new Error("Invalid raw event response");
      const event = normalizeRawRow(result.event);
      if (!event) throw new Error("Invalid raw event");
      setRawEvidence(event);
    } catch (err) {
      showNotice(parseSemanticError(err).message, true);
    }
  };

  const openFact = (fact: FactRow) => {
    setActiveFact(fact);
    setActiveSummary(null);
    setContextMenu(null);
  };

  const openSummary = (summary: SummaryRow) => {
    setActiveSummary(summary);
    setActiveFact(null);
    setContextMenu(null);
  };

  const openSemanticContextMenu = (
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
  };

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
    contextMenu,
    facts,
    summaries,
    mutating,
    confirmActiveFact,
    retrySummary,
  ]);

  const resolveConflict = (resolution: FactConflict["resolution"]) => {
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
  };

  if (!isOpen) return null;
  return (
    <div className="flex-1 flex flex-col overflow-hidden bg-[#08090a]">
      <div className="px-5 py-2 border-b border-[#23252a] bg-[#0f1011] flex items-center gap-2">
        <button
          type="button"
          onClick={() => switchSubtab("facts")}
          className="linear-btn-ghost px-3 py-1 text-xs"
        >
          Facts
        </button>
        <button
          type="button"
          onClick={() => switchSubtab("summaries")}
          className="linear-btn-ghost px-3 py-1 text-xs"
        >
          Summaries
        </button>
        <span className="ml-auto text-[11px] text-[#8a8f98]">
          {page?.total === null || page?.total === undefined
            ? "many"
            : page.total}{" "}
          results · newest first
        </span>
      </div>
      <search className="px-5 py-2 border-b border-[#23252a] bg-[#0f1011] flex items-center gap-2">
        <Search className="w-3.5 h-3.5 text-[#8a8f98]" aria-hidden="true" />
        <input
          aria-label="Search facts and summaries"
          type="search"
          value={search}
          onChange={(event) => {
            setSearch(event.target.value);
            setCursor(null);
            setCursorHistory([]);
          }}
          placeholder="Search IDs, subject, key, predicate, source..."
          className="flex-1 text-xs linear-input px-2.5 py-1.5 bg-[#08090a]"
        />
        <select
          aria-label="Status filter"
          value={filters.statuses[0] || ""}
          onChange={(event) => updateFilter("statuses", event.target.value)}
          className="text-xs linear-input bg-[#08090a] px-2 py-1.5"
        >
          <option value="">All statuses</option>
          {(subtab === "facts" ? FACT_STATUSES : SUMMARY_STATUSES).map(
            (status) => (
              <option key={status} value={status}>
                {status}
              </option>
            ),
          )}
        </select>
        <input
          aria-label="Source filter"
          value={filters.sources[0] || ""}
          onChange={(event) => updateFilter("sources", event.target.value)}
          placeholder="Source"
          className="w-24 text-xs linear-input px-2 py-1.5 bg-[#08090a]"
        />
        <input
          aria-label="Event type filter"
          value={filters.event_types[0] || ""}
          onChange={(event) => updateFilter("event_types", event.target.value)}
          placeholder="Event type"
          className="w-24 text-xs linear-input px-2 py-1.5 bg-[#08090a]"
        />
        <input
          aria-label="Subject filter"
          value={filters.subjects[0] || ""}
          onChange={(event) => updateFilter("subjects", event.target.value)}
          placeholder="Subject"
          className="w-20 text-xs linear-input px-2 py-1.5 bg-[#08090a]"
        />
        <select
          aria-label="Has summary filter"
          value={
            filters.has_summary === null ? "" : String(filters.has_summary)
          }
          onChange={(event) => {
            const value = event.target.value;
            setFilters((previous) => ({
              ...previous,
              has_summary: value === "" ? null : value === "true",
            }));
            setCursor(null);
            setCursorHistory([]);
          }}
          className="w-20 text-xs linear-input bg-[#08090a] px-1 py-1.5"
        >
          <option value="">Summary: all</option>
          <option value="true">Has summary</option>
          <option value="false">No summary</option>
        </select>
        <input
          aria-label="Occurred from filter"
          type="datetime-local"
          value={filters.occurred_from || ""}
          onChange={(event) => {
            setFilters((previous) => ({
              ...previous,
              occurred_from: event.target.value || null,
            }));
            setCursor(null);
            setCursorHistory([]);
          }}
          className="w-28 text-[10px] linear-input bg-[#08090a] px-1 py-1.5"
        />
        <input
          aria-label="Occurred to filter"
          type="datetime-local"
          value={filters.occurred_to || ""}
          onChange={(event) => {
            setFilters((previous) => ({
              ...previous,
              occurred_to: event.target.value || null,
            }));
            setCursor(null);
            setCursorHistory([]);
          }}
          className="w-28 text-[10px] linear-input bg-[#08090a] px-1 py-1.5"
        />
        <button
          type="button"
          aria-label="Refresh semantic list"
          onClick={() => setRequestVersion((value) => value + 1)}
          className="p-1.5 text-[#8a8f98] hover:text-white"
        >
          <RefreshCw
            className={`w-3.5 h-3.5 ${loading ? "animate-spin" : ""}`}
          />
        </button>
      </search>
      <div className="px-5 py-2 border-b border-[#23252a] flex items-center gap-3 text-xs">
        <button
          type="button"
          onClick={selectAllVisible}
          className="text-[#8a8f98] hover:text-white"
        >
          {selectedCount === rows.length && rows.length > 0
            ? "Clear visible"
            : "Select all visible"}
        </button>
        {subtab === "facts" && (
          <>
            <button
              type="button"
              disabled={selectedCount === 0 || mutating}
              onClick={() => void confirmFacts()}
              className="linear-btn-ghost px-2 py-1 disabled:opacity-40"
            >
              Confirm
            </button>
            <button
              type="button"
              disabled={
                selectedCount === 0 ||
                mutating ||
                (!bulkPredicate.trim() && !bulkValue.trim())
              }
              onClick={() => void editFactsBulk()}
              className="linear-btn-ghost px-2 py-1 disabled:opacity-40"
            >
              Bulk edit
            </button>
            <button
              type="button"
              disabled={selectedCount === 0 || mutating}
              onClick={() => setConfirmDelete(true)}
              className="linear-btn-ghost px-2 py-1 text-[#f87171] disabled:opacity-40"
            >
              Delete ({selectedCount})
            </button>
            <input
              aria-label="Bulk predicate"
              value={bulkPredicate}
              onChange={(event) => setBulkPredicate(event.target.value)}
              placeholder="predicate"
              className="w-20 text-xs linear-input bg-[#08090a] px-1.5 py-1"
            />
            <input
              aria-label="Bulk value"
              value={bulkValue}
              onChange={(event) => setBulkValue(event.target.value)}
              placeholder="value"
              className="w-20 text-xs linear-input bg-[#08090a] px-1.5 py-1"
            />
          </>
        )}
        {notice && (
          <span
            role={notice.error ? "alert" : "status"}
            className={notice.error ? "text-[#f87171]" : "text-[#4ade80]"}
          >
            {notice.text}
            {notice.undo && (
              <button
                type="button"
                onClick={() => void undo(notice.undo!)}
                className="ml-2 underline"
              >
                Undo
              </button>
            )}
          </span>
        )}
      </div>
      <div className="flex-1 flex overflow-hidden">
        <div className="flex-1 overflow-y-auto border-r border-[#23252a]">
          {subtab === "facts" ? (
            <div className="grid grid-cols-[28px_80px_1fr_1fr_1.2fr_70px_120px] gap-2 px-3 py-2 bg-[#161718] text-[10px] font-mono text-[#8a8f98]">
              <span /> <span>Status</span>
              <span>Subject</span>
              <span>Key</span>
              <span>Predicate / Value</span>
              <span>Evidence</span>
              <span>Latest evidence</span>
            </div>
          ) : (
            <div className="grid grid-cols-[28px_88px_120px_100px_1fr_100px] gap-2 px-3 py-2 bg-[#161718] text-[10px] font-mono text-[#8a8f98]">
              <span /> <span>Status</span>
              <span>Event time</span>
              <span>Source / type</span>
              <span>Summary preview</span>
              <span>Derived Fact ID</span>
            </div>
          )}
          {loading ? (
            <div className="p-12 text-center text-xs text-[#8a8f98]">
              <Loader2 className="mx-auto mb-2 h-5 w-5 animate-spin text-[#e4f222]" />
              Loading...
            </div>
          ) : error ? (
            <div className="p-12 text-center text-xs text-[#f87171]">
              <AlertTriangle className="mx-auto mb-2 h-5 w-5" />
              {error.message}
              <button
                type="button"
                onClick={() => void loadPage()}
                className="ml-2 underline"
              >
                Retry
              </button>
            </div>
          ) : rows.length === 0 ? (
            <div className="p-12 text-center text-xs text-[#62666d]">
              No {subtab} found
            </div>
          ) : subtab === "facts" ? (
            facts.map((fact) => (
              <button
                type="button"
                key={fact.fact_id}
                aria-haspopup="menu"
                onClick={() => openFact(fact)}
                onContextMenu={(event) =>
                  openSemanticContextMenu(event, "facts", fact.fact_id)
                }
                onKeyDown={(event) => {
                  if (
                    event.key === "ContextMenu" ||
                    (event.shiftKey && event.key === "F10")
                  ) {
                    openSemanticContextMenu(event, "facts", fact.fact_id);
                  }
                }}
                className={`w-full text-left grid grid-cols-[28px_80px_1fr_1fr_1.2fr_70px_120px] gap-2 items-center px-3 py-2 border-b border-[#1c1e22] text-[11px] hover:bg-[#111214] ${activeFact?.fact_id === fact.fact_id ? "bg-[#1b1e24] border-l-2 border-l-[#e4f222]" : ""}`}
              >
                <span
                  onClick={(event) => {
                    event.stopPropagation();
                    toggleSelected(fact.fact_id);
                  }}
                >
                  {selectedFactIds.includes(fact.fact_id) ? (
                    <CheckSquare className="h-3.5 w-3.5 text-[#e4f222]" />
                  ) : (
                    <Square className="h-3.5 w-3.5 text-[#62666d]" />
                  )}
                </span>
                <SemanticBadge status={fact.status} />
                <span className="truncate">{fact.subject}</span>
                <span className="truncate font-mono">{fact.key}</span>
                <span className="truncate">
                  {fact.predicate}: {fact.value}
                </span>
                <span>{fact.evidence_count} evidence</span>
                <span className="truncate text-[#8a8f98]">
                  {fact.latest_evidence_at || "—"}
                </span>
              </button>
            ))
          ) : (
            summaries.map((summary) => (
              <button
                type="button"
                key={summary.summary_id}
                aria-haspopup="menu"
                onClick={() => openSummary(summary)}
                onContextMenu={(event) =>
                  openSemanticContextMenu(
                    event,
                    "summaries",
                    summary.summary_id,
                  )
                }
                onKeyDown={(event) => {
                  if (
                    event.key === "ContextMenu" ||
                    (event.shiftKey && event.key === "F10")
                  ) {
                    openSemanticContextMenu(
                      event,
                      "summaries",
                      summary.summary_id,
                    );
                  }
                }}
                className={`w-full text-left grid grid-cols-[28px_88px_120px_100px_1fr_100px] gap-2 items-center px-3 py-2 border-b border-[#1c1e22] text-[11px] hover:bg-[#111214] ${activeSummary?.summary_id === summary.summary_id ? "bg-[#1b1e24] border-l-2 border-l-[#e4f222]" : ""}`}
              >
                <span
                  onClick={(event) => {
                    event.stopPropagation();
                    toggleSelected(summary.summary_id);
                  }}
                >
                  {selectedSummaryIds.includes(summary.summary_id) ? (
                    <CheckSquare className="h-3.5 w-3.5 text-[#e4f222]" />
                  ) : (
                    <Square className="h-3.5 w-3.5 text-[#62666d]" />
                  )}
                </span>
                <SemanticBadge status={summary.status} />
                <span className="truncate font-mono">
                  {summary.occurred_at}
                </span>
                <span className="truncate">
                  {summary.source}/{summary.event_type}
                </span>
                <span className="truncate">
                  {summary.status === "completed" && summary.summary
                    ? summary.summary
                    : summaryReasonLabel(summary.reason || summary.error) ||
                      (summary.status === "pending" ||
                      summary.status === "fallback" ||
                      summary.status === "error"
                        ? "Raw fallback available"
                        : "—")}
                </span>
                <span className="truncate font-mono">
                  {summary.derived_fact_id || "—"}
                </span>
              </button>
            ))
          )}
          <div className="flex items-center justify-between px-3 py-2 text-[11px] text-[#8a8f98]">
            <button
              type="button"
              disabled={cursorHistory.length === 0 || loading}
              onClick={() => {
                const history = [...cursorHistory];
                const previous = history.pop() ?? null;
                setCursorHistory(history);
                setCursor(previous);
              }}
              className="flex items-center gap-1 disabled:opacity-30"
            >
              <ChevronLeft className="h-3.5 w-3.5" />
              Previous
            </button>
            <span>50 per page</span>
            <button
              type="button"
              disabled={!page?.has_more || loading || !page?.next_cursor}
              onClick={() => {
                setCursorHistory((history) => [...history, cursor]);
                setCursor(page?.next_cursor || null);
              }}
              className="flex items-center gap-1 disabled:opacity-30"
            >
              Next
              <ChevronRight className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>
        <aside className="w-[360px] flex flex-col overflow-y-auto bg-[#0f1011] p-4 text-xs">
          {activeFact ? (
            <>
              <div className="flex items-center justify-between border-b border-[#23252a] pb-2">
                <h3 className="font-semibold text-white">Fact detail</h3>
                <SemanticBadge status={activeFact.status} />
              </div>
              <label className="mt-3 text-[#8a8f98]">
                Subject (read-only)
                <input
                  readOnly
                  value={activeFact.subject}
                  className="mt-1 w-full linear-input bg-[#161718] px-2 py-1.5 text-[#8a8f98]"
                />
              </label>
              <label className="mt-2 text-[#8a8f98]">
                Key (read-only)
                <input
                  readOnly
                  value={activeFact.key}
                  className="mt-1 w-full linear-input bg-[#161718] px-2 py-1.5 text-[#8a8f98]"
                />
              </label>
              <label className="mt-2 text-[#8a8f98]">
                Predicate
                <input
                  value={predicate}
                  onChange={(event) => setPredicate(event.target.value)}
                  className="mt-1 w-full linear-input bg-[#08090a] px-2 py-1.5 text-white"
                />
              </label>
              <label className="mt-2 text-[#8a8f98]">
                Value
                <textarea
                  value={factValue}
                  onChange={(event) => setFactValue(event.target.value)}
                  className="mt-1 w-full linear-input bg-[#08090a] px-2 py-1.5 text-white"
                  rows={3}
                />
              </label>
              <button
                type="button"
                disabled={
                  mutating ||
                  (predicate === activeFact.predicate &&
                    factValue === activeFact.value)
                }
                onClick={() => void editFact()}
                className="mt-3 linear-btn-primary px-3 py-1.5 disabled:opacity-40"
              >
                Save edit
              </button>
              {activeFact.status === "auto" && (
                <button
                  type="button"
                  disabled={mutating}
                  onClick={() => void confirmActiveFact()}
                  className="mt-2 linear-btn-ghost px-3 py-1.5 disabled:opacity-40"
                >
                  Confirm Fact
                </button>
              )}
              <dl className="mt-4 space-y-1 border-t border-[#23252a] pt-3 text-[10px] text-[#8a8f98]">
                <div>
                  <dt className="inline">Fact ID: </dt>
                  <dd className="inline font-mono text-[#d0d6e0]">
                    {activeFact.fact_id}
                  </dd>
                </div>
                <div>
                  <dt className="inline">Source event IDs: </dt>
                  <dd className="inline font-mono text-[#d0d6e0]">
                    {activeFact.source_event_ids.join(", ") || "—"}
                  </dd>
                </div>
                <div>
                  <dt className="inline">Revision: </dt>
                  <dd className="inline">{activeFact.revision}</dd>
                </div>
                <div>
                  <dt className="inline">Operation: </dt>
                  <dd className="inline font-mono">
                    {activeFact.operation_id}
                  </dd>
                </div>
                <div>
                  <dt className="inline">Latest evidence: </dt>
                  <dd className="inline">
                    {activeFact.latest_evidence_at || "—"}
                  </dd>
                </div>
              </dl>
              <div className="mt-4 border-t border-[#23252a] pt-3">
                <h4 className="mb-2 font-semibold text-white">
                  Evidence ({activeFact.evidence_count})
                </h4>
                {evidenceLoading ? (
                  <span className="text-[#8a8f98]">Loading evidence...</span>
                ) : evidenceError ? (
                  <span className="text-[#f87171]">{evidenceError}</span>
                ) : (
                  evidence.map((item) => (
                    <details
                      key={item.event_id}
                      className="mb-2 rounded border border-[#23252a] p-2"
                    >
                      <summary className="cursor-pointer text-[#d0d6e0]">
                        {item.relation} · {item.occurred_at} · {item.source}
                      </summary>
                      <p className="mt-2 whitespace-pre-wrap text-[#8a8f98]">
                        {item.content}
                      </p>
                      <button
                        type="button"
                        onClick={() => void hydrateRawEvidence(item.event_id)}
                        className="mt-2 flex items-center gap-1 text-[#38bdf8] underline"
                      >
                        <Eye className="h-3 w-3" />
                        Open raw evidence
                      </button>
                    </details>
                  ))
                )}
              </div>
              {rawEvidence && (
                <div className="mt-2 rounded border border-[#02b8cc]/30 bg-[#02b8cc]/5 p-2">
                  <div className="flex items-center justify-between text-[#38bdf8]">
                    <span>Raw evidence (read-only)</span>
                    <button type="button" onClick={() => setRawEvidence(null)}>
                      ×
                    </button>
                  </div>
                  <p className="mt-1 whitespace-pre-wrap text-[#d0d6e0]">
                    {rawEvidence.content || rawEvidence.content_preview}
                  </p>
                </div>
              )}
            </>
          ) : activeSummary ? (
            <>
              <div className="flex items-center justify-between border-b border-[#23252a] pb-2">
                <h3 className="font-semibold text-white">Summary detail</h3>
                <SemanticBadge status={activeSummary.status} />
              </div>
              <dl className="mt-3 space-y-2 text-[11px] text-[#8a8f98]">
                <div>
                  <dt>Summary ID (read-only)</dt>
                  <dd className="font-mono text-[#d0d6e0]">
                    {activeSummary.summary_id}
                  </dd>
                </div>
                <div>
                  <dt>Event ID (read-only)</dt>
                  <dd className="font-mono text-[#d0d6e0]">
                    {activeSummary.event_id}
                  </dd>
                </div>
                <div>
                  <dt>Occurred</dt>
                  <dd>{activeSummary.occurred_at}</dd>
                </div>
                <div>
                  <dt>Source / type</dt>
                  <dd>
                    {activeSummary.source} / {activeSummary.event_type}
                  </dd>
                </div>
                <div>
                  <dt>Summary</dt>
                  <dd className="mt-1 whitespace-pre-wrap text-[#d0d6e0]">
                    {activeSummary.status === "completed"
                      ? activeSummary.summary || "—"
                      : "Raw fallback available"}
                  </dd>
                </div>
                {(activeSummary.reason || activeSummary.error) && (
                  <div className="text-[#f87171]">
                    <dt>Reason</dt>
                    <dd>
                      {summaryReasonLabel(
                        activeSummary.reason || activeSummary.error,
                      )}
                    </dd>
                  </div>
                )}
                <div>
                  <dt>Model / prompt</dt>
                  <dd>
                    {activeSummary.model_id || "—"} /{" "}
                    {activeSummary.prompt_version || "—"}
                  </dd>
                </div>
                <div>
                  <dt>Vector source</dt>
                  <dd>{activeSummary.vector_source || "—"}</dd>
                </div>
                <div>
                  <dt>Derived Fact</dt>
                  <dd className="font-mono">
                    {activeSummary.derived_fact_id || "—"}
                  </dd>
                </div>
                <div>
                  <dt>Attempt</dt>
                  <dd className="font-mono">
                    {activeSummary.attempt_id || "—"}
                  </dd>
                </div>
                <div>
                  <dt>Retry count</dt>
                  <dd>{activeSummary.retry_count ?? 0}</dd>
                </div>
              </dl>
              {summaryRetryAllowed(activeSummary) && (
                <button
                  type="button"
                  disabled={mutating}
                  onClick={() => void retrySummary(activeSummary)}
                  className="mt-3 flex items-center justify-center gap-1 linear-btn-ghost px-3 py-1.5 disabled:opacity-40"
                >
                  <RotateCcw className="h-3.5 w-3.5" />
                  Retry summary
                </button>
              )}
              {(activeSummary.reason || activeSummary.error) ===
                "policy_excluded" && (
                <div className="mt-3 rounded border border-[#62666d]/40 bg-[#23252a]/40 p-2 text-[#8a8f98]">
                  この行は設定されたプライバシーポリシーにより Fact
                  化していません。Raw event は保持されています。
                </div>
              )}
              {(activeSummary.status === "pending" ||
                activeSummary.status === "fallback" ||
                activeSummary.status === "error") && (
                <div className="mt-3 rounded border border-[#e4f222]/30 bg-[#e4f222]/5 p-2 text-[#e4f222]">
                  Raw fallback remains available and searchable.
                </div>
              )}
              <button
                type="button"
                onClick={() => void hydrateRawEvidence(activeSummary.event_id)}
                className="mt-3 flex items-center gap-1 text-[#38bdf8] underline"
              >
                <Eye className="h-3 w-3" />
                View raw evidence
              </button>
              {rawEvidence && (
                <p className="mt-2 whitespace-pre-wrap rounded border border-[#23252a] p-2 text-[#d0d6e0]">
                  {rawEvidence.content || rawEvidence.content_preview}
                </p>
              )}
            </>
          ) : (
            <div className="flex h-full items-center justify-center text-center text-[#62666d]">
              Select a row to inspect details.
            </div>
          )}
        </aside>
      </div>
      {contextMenu && semanticContextActions.length > 0 && (
        <MemoryContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          label={
            contextMenu.kind === "facts" ? "Fact actions" : "Summary actions"
          }
          actions={semanticContextActions}
          onClose={() => setContextMenu(null)}
        />
      )}
      {confirmDelete && (
        <div
          role="dialog"
          aria-modal="true"
          className="absolute inset-0 z-10 flex items-center justify-center bg-black/70"
        >
          <div className="w-[360px] rounded border border-[#eb5757]/50 bg-[#161718] p-4">
            <h3 className="font-semibold text-white">
              Delete {selectedFactIds.length} Facts?
            </h3>
            <p className="mt-2 text-xs text-[#8a8f98]">
              Only semantic Facts will be deleted. Source Raw events, content,
              and embeddings are retained.
            </p>
            <div className="mt-4 flex justify-end gap-2">
              <button
                type="button"
                onClick={() => setConfirmDelete(false)}
                className="linear-btn-ghost px-3 py-1.5"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() => void deleteFacts()}
                className="linear-btn-ghost px-3 py-1.5 text-[#f87171]"
              >
                Delete {selectedFactIds.length}
              </button>
            </div>
          </div>
        </div>
      )}
      {pendingSubtab && (
        <div
          role="dialog"
          aria-modal="true"
          className="absolute inset-0 z-10 flex items-center justify-center bg-black/70"
        >
          <div className="w-[420px] rounded border border-[#e4f222]/50 bg-[#161718] p-4">
            <h3 className="font-semibold text-white">Unsaved Fact edit</h3>
            <p className="mt-2 text-xs text-[#8a8f98]">
              Save or explicitly discard the current detail-panel edit before
              changing tabs.
            </p>
            <div className="mt-4 flex justify-end gap-2">
              <button
                type="button"
                onClick={() => setPendingSubtab(null)}
                className="linear-btn-ghost px-3 py-1.5"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={discardAndSwitchSubtab}
                className="linear-btn-ghost px-3 py-1.5"
              >
                Discard edit
              </button>
              <button
                type="button"
                onClick={() => void saveAndSwitchSubtab()}
                className="linear-btn-primary px-3 py-1.5"
              >
                Save edit
              </button>
            </div>
          </div>
        </div>
      )}
      {conflict && (
        <div
          role="dialog"
          aria-modal="true"
          className="absolute inset-0 z-10 flex items-center justify-center bg-black/70"
        >
          <div className="w-[620px] rounded border border-[#e4f222]/50 bg-[#161718] p-4">
            <h3 className="font-semibold text-white">Fact changed elsewhere</h3>
            <div className="mt-3 grid grid-cols-2 gap-3 text-xs">
              <div>
                <h4 className="text-[#8a8f98]">Current</h4>
                <p className="mt-1 border border-[#23252a] p-2">
                  {conflict.current.predicate}: {conflict.current.value}
                  <br />
                  status {conflict.current.status} · revision{" "}
                  {conflict.current.revision}
                </p>
              </div>
              <div>
                <h4 className="text-[#8a8f98]">Your change</h4>
                <p className="mt-1 border border-[#23252a] p-2">
                  {conflict.attempted.predicate}: {conflict.attempted.value}
                  <br />
                  expected {conflict.attempted.expected_status} · revision{" "}
                  {conflict.attempted.expected_revision}
                </p>
              </div>
            </div>
            <h4 className="mt-3 text-[#8a8f98]">Evidence</h4>
            <p className="mt-1 text-xs text-[#d0d6e0]">
              {conflict.evidence.length} evidence items remain attached.
            </p>
            <div className="mt-4 flex justify-end gap-2">
              <button
                type="button"
                onClick={() => resolveConflict("cancel")}
                className="linear-btn-ghost px-3 py-1.5"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() => resolveConflict("keep_current")}
                className="linear-btn-ghost px-3 py-1.5"
              >
                Keep current
              </button>
              <button
                type="button"
                onClick={() => resolveConflict("apply_attempted")}
                className="linear-btn-primary px-3 py-1.5"
              >
                Apply my change
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};

interface MemoryModalProps {
  isOpen: boolean;
  onClose: () => void;
  /** Main-window console stream, shared so the modal shows the same logs. */
  logs?: LogEntry[];
  onClearLogs?: () => void;
}

type SortField = "timestamp" | "key" | "type" | "user" | "content";
type SortOrder = "asc" | "desc";

export const MemoryModal: React.FC<MemoryModalProps> = ({
  isOpen,
  onClose,
  logs = [],
  onClearLogs,
}) => {
  const [managerTab, setManagerTab] = useState<"raw" | "semantic">("raw");
  const [memories, setMemories] = useState<MemoryItem[]>([]);
  const [loading, setLoading] = useState<boolean>(false);
  const [migrationStatus, setMigrationStatus] =
    useState<MemoryMigrationStatus | null>(null);
  const [searchQuery, setSearchQuery] = useState<string>("");

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
  const [editKey, setEditKey] = useState<string>("");
  const [editType, setEditType] = useState<string>("memory");
  const [editUser, setEditUser] = useState<string>("User");
  const [editContent, setEditContent] = useState<string>("");
  const [isCreatingNew, setIsCreatingNew] = useState<boolean>(false);

  // 一括編集状態 (複数)
  const [bulkType, setBulkType] = useState<string>("");
  const [bulkUser, setBulkUser] = useState<string>("");

  // アクション通知・生成中状態
  const [actionMessage, setActionMessage] = useState<{
    text: string;
    type: "success" | "error";
  } | null>(null);
  const [backfillProgress, setBackfillProgress] =
    useState<MemoryBackfillProgress | null>(null);
  const [isGeneratingBlog, setIsGeneratingBlog] = useState<boolean>(false);
  const [blogResult, setBlogResult] = useState<{
    filename: string;
    content: string;
  } | null>(null);

  const fetchMemories = async (
    hiddenIds: ReadonlySet<string> = optimisticallyDeletedIds,
  ) => {
    const requestId = ++fetchRequestId.current;
    setLoading(true);
    try {
      // Tauri Native LanceDB を呼び出し (最新順で取得)
      const data: unknown = await invoke("list_lance_memories", {
        limit: 5000,
        offset: 0,
      });
      if (
        !isRecord(data) ||
        data.success !== true ||
        !Array.isArray(data.memories)
      ) {
        throw new Error("Memory read returned an invalid result");
      }
      const rawMemories = data.memories;
      if (
        rawMemories.some(
          (memory) =>
            !isRecord(memory) ||
            !stringValue(memory.id) ||
            !stringValue(memory.document),
        )
      ) {
        throw new Error("Memory read returned an invalid row");
      }
      const list: MemoryItem[] = rawMemories.map((memory) => {
        const row = memory as Record<string, unknown>;
        return {
          id: stringValue(row.id)!,
          key: stringValue(row.id)!,
          content: stringValue(row.document)!,
          type: stringValue(row.memory_type) || undefined,
          source: stringValue(row.source) || undefined,
          user: stringValue(row.user_id) || stringValue(row.source) || "User",
          timestamp: stringValue(row.timestamp) || undefined,
        };
      });
      // A successful delete is reflected locally before LanceDB's next
      // snapshot is observable. Keep the tombstone overlay while this read
      // catches up so a stale snapshot cannot make the row reappear.
      const visibleList = list.filter((memory) => !hiddenIds.has(memory.id));
      if (requestId !== fetchRequestId.current) return;
      setMemories(visibleList);
      if (visibleList.length > 0 && selectedIds.length === 0 && !activeItem) {
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
  };

  useEffect(() => {
    if (isOpen) {
      fetchMemories();
    } else {
      setManagerTab("raw");
      setSelectedIds([]);
      setActiveItem(null);
      setContextMenu(null);
      setOptimisticallyDeletedIds(new Set());
      setLastAnchorIndex(null);
      setBlogResult(null);
      setIsCreatingNew(false);
      setBackfillProgress(null);
    }
  }, [isOpen]);

  // The all-memory semantic pass is detached from the command invocation;
  // progress events keep the operation visible while Gemma processes rows.
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

  // progress snapshot so a multi-minute import is visible instead of looking
  // like a hung memory manager.
  useEffect(() => {
    if (!isOpen) {
      setMigrationStatus(null);
      return;
    }

    let cancelled = false;
    const refreshMigrationStatus = async () => {
      try {
        const raw = await invoke("get_lance_migration_status");
        const status = normalizeMemoryMigrationStatus(raw);
        if (!cancelled && status) {
          setMigrationStatus(status);
        }
      } catch {
        // Older portable builds do not expose this advisory command. The
        // regular memory query still determines success or failure.
      }
    };

    void refreshMigrationStatus();
    const timer = window.setInterval(refreshMigrationStatus, 500);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [isOpen]);

  const populateEditForm = (item: MemoryItem) => {
    setIsCreatingNew(false);
    setEditKey(item.key || item.id);
    setEditType(item.type || "memory");
    setEditUser(item.user || item.source || "User");
    setEditContent(item.content || "");
  };

  const showNotice = (text: string, type: "success" | "error" = "success") => {
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

  // Ctrl+A で全選択ショートカット
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (!isOpen) return;
      const target = e.target as HTMLElement;
      if (target.tagName === "INPUT" || target.tagName === "TEXTAREA") return;

      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
        e.preventDefault();
        setSelectedIds(filteredAndSortedMemories.map((m) => m.id));
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [isOpen, filteredAndSortedMemories]);

  const handleSort = (field: SortField) => {
    if (sortField === field) {
      setSortOrder(sortOrder === "asc" ? "desc" : "asc");
    } else {
      setSortField(field);
      setSortOrder("desc");
    }
  };

  // 選択操作 (Ctrl / Shift / 通常クリック対応)
  const handleRowClick = (
    item: MemoryItem,
    index: number,
    e: React.MouseEvent,
  ) => {
    setContextMenu(null);
    const isCtrl = e.ctrlKey || e.metaKey;
    const isShift = e.shiftKey;

    if (isShift && lastAnchorIndex !== null) {
      // Shift + クリック: 範囲選択
      const from = Math.min(lastAnchorIndex, index);
      const to = Math.max(lastAnchorIndex, index);
      const rangeIds = filteredAndSortedMemories
        .slice(from, to + 1)
        .map((m) => m.id);

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

  const handleToggleSelectId = (
    id: string,
    index: number,
    e: React.MouseEvent,
  ) => {
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
      await invoke("import_memories_to_lance", {
        items: [itemToSave],
        vectors: null,
      });
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
  };

  // 単一 / 選択中アイテムの削除
  const handleDeleteSelected = async (idsOverride?: string[]) => {
    const idsToDelete = idsOverride ? [...idsOverride] : [...selectedIds];
    const count = idsToDelete.length;
    if (count === 0) return;

    if (!confirm(`選択した ${count} 件のメモリーを完全に削除しますか？`)) {
      return;
    }

    try {
      await invoke("delete_lance_memories_bulk", { ids: idsToDelete });
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
  };

  // 一括メタデータ更新
  const handleBulkUpdate = async () => {
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

      await invoke("delete_lance_memories_bulk", { ids: selectedIds });
      await invoke("import_memories_to_lance", {
        items: updatedItems,
        vectors: null,
      });
      showNotice(`${selectedIds.length} 件のメモリーを一括更新しました！`);
      setBulkType("");
      setBulkUser("");
      await fetchMemories();
    } catch (e) {
      showNotice(`一括更新エラー: ${e}`, "error");
    }
  };

  // 選択メモリーからのブログ生成
  const handleGenerateBlog = async (idsOverride?: string[]) => {
    const idsToGenerate = idsOverride ? [...idsOverride] : [...selectedIds];
    if (idsToGenerate.length === 0) {
      showNotice("ブログを生成するメモリーを選択してください", "error");
      return;
    }

    setIsGeneratingBlog(true);
    try {
      const res = await fetch(
        "http://127.0.0.1:18080/api/memories/generate-blog",
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ ids: idsToGenerate }),
        },
      );
      const data = await res.json();
      if (data.success) {
        setBlogResult({
          filename: data.filename,
          content: data.content,
        });
        showNotice("note プレイ日誌記事の生成が完了しました！");
      } else {
        showNotice(`ブログ生成エラー: ${data.error}`, "error");
      }
    } catch (e) {
      showNotice(`通信エラー: ${e}`, "error");
    } finally {
      setIsGeneratingBlog(false);
    }
  };

  const openRawContextMenu = (
    event: React.MouseEvent<HTMLElement> | React.KeyboardEvent<HTMLElement>,
    item: MemoryItem,
  ) => {
    event.preventDefault();
    // A context action always has an unambiguous target. Existing multi-row
    // selection is kept only when the user right-clicks one of those rows.
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
  };

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
  }, [contextMenu, memories, selectedIds.length]);

  const handleBackup = async () => {
    try {
      if (
        typeof window !== "undefined" &&
        (window as any).__TAURI_INTERNALS__
      ) {
        const res: string = await invoke("lance_backup");
        showNotice(`バックアップを作成しました: ${res}`);
      }
    } catch (e) {
      showNotice(`バックアップ失敗: ${e}`, "error");
    }
  };

  const handleProcessAllMemories = async () => {
    if (backfillProgress?.state === "running") return;
    setActionMessage({
      text: "全メモリのファクト・要約を開始しています...",
      type: "success",
    });
    try {
      const result = await invoke("memory_manager_process_all");
      if (!isRecord(result))
        throw new Error("Invalid all-memory processing response");
      const progress = normalizeMemoryBackfillProgress(result.progress);
      if (!progress) throw new Error("Invalid all-memory processing progress");
      setBackfillProgress(progress);
      if (result.accepted === false) {
        setActionMessage({
          text: "全メモリの処理は既に実行中です",
          type: "success",
        });
      }
    } catch (e) {
      setActionMessage({
        text: `ファクト・要約処理を開始できませんでした: ${e}`,
        type: "error",
      });
    }
  };

  const handleExportJson = async () => {
    try {
      if (
        typeof window !== "undefined" &&
        (window as any).__TAURI_INTERNALS__
      ) {
        const res: string = await invoke("lance_export_json", {});
        showNotice(res);
      }
    } catch (e) {
      showNotice(`JSONエクスポート失敗: ${e}`, "error");
    }
  };

  const startCreateNew = () => {
    setContextMenu(null);
    setIsCreatingNew(true);
    setActiveItem(null);
    setSelectedIds([]);
    setEditKey(`mem_${Date.now()}`);
    setEditType("memory");
    setEditUser("User");
    setEditContent("");
  };

  if (!isOpen) return null;

  const isBulkMode = selectedIds.length > 1;
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

  const backfillWarningReasons = backfillProgress
    ? Object.entries(backfillProgress.reasonCounts || {}).filter(
        ([, count]) => count > 0,
      )
    : [];
  const backfillHasWarnings = Boolean(
    backfillProgress &&
      (backfillProgress.failed > 0 ||
        backfillWarningReasons.length > 0 ||
        Boolean(backfillProgress.lastErrorReason) ||
        Boolean(backfillProgress.reason)),
  );
  const backfillWarningText = backfillProgress
      ? backfillWarningReasons.length > 0
        ? backfillWarningReasons
            .map(([reason, count]) => `${reason} (${count})`)
            .join(", ")
      : backfillProgress.reason ||
        backfillProgress.lastErrorReason ||
        "row failures"
    : "";
  const backfillFatalCode =
    backfillProgress?.fatalError?.code ||
    (backfillProgress?.state === "error" ? "fatal_error" : null);

  // Type バッジのカラーマップ
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
                onClick={handleBackup}
                className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs font-medium rounded-[6px] bg-[#1a1b1e] hover:bg-[#23252a] text-[#d0d6e0] border border-[#2e3035] transition-all"
                title="LanceDB のスナップショットバックアップを作成"
              >
                <Archive className="w-3.5 h-3.5 text-[#38bdf8]" />
                <span>Backup</span>
              </button>
              <button
                type="button"
                onClick={() => void handleProcessAllMemories()}
                disabled={loading || backfillProgress?.state === "running"}
                className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs font-medium rounded-[6px] bg-[#e4f222]/10 hover:bg-[#e4f222]/20 text-[#e4f222] border border-[#e4f222]/30 transition-all disabled:cursor-not-allowed disabled:opacity-40"
                title="全メモリからGemmaで要約・ファクトを作成します。完了済みはスキップします。"
              >
                <Sparkles className="w-3.5 h-3.5" />
                <span>Process all memories</span>
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
                onClick={() => void fetchMemories()}
                className="p-1.5 text-[#8a8f98] hover:text-white hover:bg-[#23252a] rounded-[6px] transition-colors"
                title="データを再読込"
              >
                <RefreshCw
                  className={`w-4 h-4 ${loading ? "animate-spin" : ""}`}
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

          <nav
            aria-label="Memory views"
            className="flex items-center gap-1 border-b border-[#23252a] bg-[#161718] px-5 py-2"
          >
            <button
              type="button"
              aria-selected={managerTab === "raw"}
              onClick={() => {
                setContextMenu(null);
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
                setContextMenu(null);
                setManagerTab("semantic");
              }}
              className={`px-3 py-1 text-xs font-medium rounded ${managerTab === "semantic" ? "bg-[#e4f222]/15 text-[#e4f222]" : "text-[#8a8f98] hover:text-white"}`}
            >
              Fact / Summary
            </button>
          </nav>

          {backfillProgress && (
            <div className="border-b border-[#23252a] bg-[#0f1011] px-5 py-2 text-[11px]">
              <div className="flex items-center gap-3">
                <Sparkles
                  className={`h-3.5 w-3.5 ${backfillProgress.state === "running" ? "animate-pulse text-[#e4f222]" : backfillProgress.state === "error" ? "text-[#f87171]" : "text-[#4ade80]"}`}
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
                  {backfillProgress.skipped} · failed {backfillProgress.failed}{" "}
                  · retries {backfillProgress.retry_count ?? 0}
                  {backfillProgress.attempt_id && (
                    <> · attempt {backfillProgress.attempt_id}</>
                  )}
                </span>
                {backfillProgress.state !== "running" &&
                  backfillProgress.final_counts && (
                    <span className="truncate text-[#8a8f98]">
                      Final rows: processed{" "}
                      {backfillProgress.final_counts.processed} · persisted{" "}
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
                  className={`h-full transition-[width] duration-300 ${backfillProgress.state === "error" ? "bg-[#f87171]" : "bg-[#e4f222]"}`}
                  style={{
                    width: `${backfillProgress.total > 0 ? Math.min(100, Math.round((backfillProgress.processed / backfillProgress.total) * 100)) : backfillProgress.state === "completed" ? 100 : 4}%`,
                  }}
                />
              </div>
            </div>
          )}

          {managerTab === "semantic" ? (
            <FactSummaryManager isOpen={isOpen} />
          ) : (
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
                      <button
                        onClick={handleSelectAll}
                        className="hover:text-white"
                      >
                        {selectedIds.length > 0 &&
                        selectedIds.length ===
                          filteredAndSortedMemories.length ? (
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
                                {migrationStatus.total?.toLocaleString() ?? "?"}{" "}
                                records
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
                          {migrationStatus.error ||
                            "詳細はログを確認してください。"}
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
                            onContextMenu={(event) =>
                              openRawContextMenu(event, m)
                            }
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
                              onClick={(e) =>
                                handleToggleSelectId(m.id, index, e)
                              }
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
                            showNotice(
                              "記事をクリップボードにコピーしました！",
                            );
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
                          <span className="text-xs font-semibold text-white">
                            Bulk Editing
                          </span>
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
                          <span>
                            Apply Metadata to {selectedIds.length} Items
                          </span>
                        </button>

                        <button
                          onClick={() => void handleDeleteSelected()}
                          className="w-full py-2 linear-btn-ghost border-[#eb5757]/30 hover:border-[#eb5757] text-[#eb5757] flex items-center justify-center gap-1.5 text-xs font-medium"
                        >
                          <Trash2 className="w-3.5 h-3.5" />
                          <span>Delete {selectedIds.length} Selected</span>
                        </button>

                        {/* 🌟 選択したメモリーから note ブログ生成 */}
                        <button
                          onClick={() => void handleGenerateBlog()}
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
                                Generate Blog from Selected (
                                {selectedIds.length})
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
                            {isCreatingNew
                              ? "Create New Memory"
                              : "Memory Details"}
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
                            {activeItem.display_ts ||
                              activeItem.timestamp ||
                              "N/A"}
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
                            onClick={handleSaveSingle}
                            className="flex-1 py-2 linear-btn-primary flex items-center justify-center gap-1.5 text-xs font-semibold"
                          >
                            <Save className="w-3.5 h-3.5 text-[#08090a]" />
                            <span>
                              {isCreatingNew ? "Create Memory" : "Save Changes"}
                            </span>
                          </button>

                          {!isCreatingNew && activeItem && (
                            <button
                              onClick={() => void handleDeleteSelected()}
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
                            onClick={() => void handleGenerateBlog()}
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
            </>
          )}

          {contextMenu && rawContextActions.length > 0 && (
            <MemoryContextMenu
              x={contextMenu.x}
              y={contextMenu.y}
              label="Memory actions"
              actions={rawContextActions}
              onClose={() => setContextMenu(null)}
            />
          )}

          {/* フッター */}
          <div className="px-5 py-2.5 border-t border-[#23252a] flex justify-between items-center bg-[#161718] text-xs text-[#8a8f98]">
            <div className="flex items-center gap-3">
              <span>Total Memories: {memories.length}</span>
              {selectedIds.length > 0 && (
                <span className="text-[#e4f222] font-semibold">
                  ({selectedIds.length} selected)
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
