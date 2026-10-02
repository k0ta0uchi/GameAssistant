import { invoke } from "@tauri-apps/api/core";
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
  normalizeMemoryBackfillProgress,
  normalizeMemoryMigrationStatus,
} from "../types";

export type SemanticTab = "facts" | "summaries";
export type SemanticError = { message: string; retryable: boolean };

export const EMPTY_MEMORY_FILTERS: MemoryFilters = {
  statuses: [],
  sources: [],
  event_types: [],
  subjects: [],
  occurred_from: null,
  occurred_to: null,
  has_summary: null,
};

export const FACT_STATUSES: FactStatus[] = ["auto", "confirmed", "edited"];
export const SUMMARY_STATUSES: SummaryStatus[] = [
  "pending",
  "completed",
  "skipped",
  "fallback",
  "error",
  "legacy",
  "deleted",
];

export const isRecord = (value: unknown): value is Record<string, unknown> =>
  Boolean(value) && typeof value === "object";

export const stringValue = (value: unknown): string | null =>
  typeof value === "string" ? value : null;

export const numberValue = (value: unknown): number | null =>
  typeof value === "number" && Number.isFinite(value) ? value : null;

export const arrayOfStrings = (value: unknown): string[] =>
  Array.isArray(value)
    ? value.filter((entry): entry is string => typeof entry === "string")
    : [];

export const SUMMARY_REASON_CODES = [
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

export const normalizeSummaryReason = (value: unknown): string | null => {
  if (typeof value !== "string" || !value.trim()) return null;
  const normalized = value.trim().toLowerCase();
  return (
    SUMMARY_REASON_CODES.find(
      (reason) => normalized === reason || normalized.includes(reason),
    ) || "inference_failed"
  );
};

export const normalizePage = (value: unknown): MemoryPageInfo | null => {
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

export const normalizeRawRow = (value: unknown): RawEventRow | null => {
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

export const normalizeFactRow = (value: unknown): FactRow | null => {
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

export const normalizeSummaryRow = (value: unknown): SummaryRow | null => {
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

export const parseSemanticError = (error: unknown): SemanticError => {
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

export const normalizeConflict = (error: unknown): FactConflict | null => {
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

export const hasMutationReceipt = (
  value: unknown,
): value is { undo_token?: unknown } => {
  if (!isRecord(value)) return false;
  return (
    typeof value.operation_id === "string" &&
    numberValue(value.journal_sequence) !== null &&
    typeof value.committed_at === "string"
  );
};

export const statusBadgeClass = (status: string): string =>
  status === "confirmed" || status === "completed"
    ? "bg-[#27a644]/15 text-[#4ade80] border-[#27a644]/30"
    : status === "error"
      ? "bg-[#eb5757]/15 text-[#f87171] border-[#eb5757]/30"
      : status === "fallback" || status === "pending"
        ? "bg-[#e4f222]/15 text-[#e4f222] border-[#e4f222]/30"
        : "bg-[#23252a] text-[#8a8f98] border-[#383b3f]";

export const summaryReasonLabel = (reason: string | null): string => {
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

export const summaryRetryAllowed = (summary: SummaryRow): boolean =>
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

export const writeClipboardText = async (text: string): Promise<void> => {
  const clip =
    typeof window !== "undefined" && window.navigator?.clipboard
      ? window.navigator.clipboard
      : typeof navigator !== "undefined" && navigator.clipboard
        ? navigator.clipboard
        : undefined;
  if (!clip?.writeText) {
    throw new Error("Clipboard API unavailable");
  }
  await clip.writeText(text);
};

// ==========================================
// Memory V2 IPC APIs
// ==========================================

export interface MutationReceiptResponse {
  changed: boolean;
  receipt: {
    operation_id: string;
    journal_sequence: number;
    committed_at: string;
    undo_token?: string | null;
  };
}

export async function listFactsApi(
  request: MemoryPageRequest,
): Promise<{ rows: FactRow[]; page: MemoryPageInfo }> {
  const result = await invoke("memory_manager_list_facts", { request });
  if (!isRecord(result) || !Array.isArray(result.rows)) {
    throw new Error("Invalid memory manager response");
  }
  const page = normalizePage(result.page);
  if (!page) {
    throw new Error("Invalid memory manager page response");
  }
  const normalizedRows = result.rows.map(normalizeFactRow);
  if (normalizedRows.some((row) => row === null)) {
    throw new Error("Invalid memory manager fact row");
  }
  return { rows: normalizedRows as FactRow[], page };
}

export async function listSummariesApi(
  request: MemoryPageRequest,
): Promise<{ rows: SummaryRow[]; page: MemoryPageInfo }> {
  const result = await invoke("memory_manager_list_summaries", { request });
  if (!isRecord(result) || !Array.isArray(result.rows)) {
    throw new Error("Invalid memory manager response");
  }
  const page = normalizePage(result.page);
  if (!page) {
    throw new Error("Invalid memory manager page response");
  }
  const normalizedRows = result.rows.map(normalizeSummaryRow);
  if (normalizedRows.some((row) => row === null)) {
    throw new Error("Invalid memory manager summary row");
  }
  return { rows: normalizedRows as SummaryRow[], page };
}

export async function getFactEvidenceApi(
  factId: string,
  page: MemoryPageRequest,
): Promise<FactEvidence[]> {
  const result = await invoke("memory_manager_get_fact_evidence", {
    factId,
    page,
  });
  if (!isRecord(result) || !Array.isArray(result.evidence)) {
    throw new Error("Invalid evidence response");
  }
  return result.evidence.filter(isRecord).map((entry) => ({
    event_id: stringValue(entry.event_id) || "",
    subject: stringValue(entry.subject) || "",
    event_type: stringValue(entry.event_type) || "",
    source: stringValue(entry.source) || "",
    occurred_at: stringValue(entry.occurred_at) || "",
    content: stringValue(entry.content) || "",
    relation: (stringValue(entry.relation) || "supporting") as FactEvidence["relation"],
    match_score: numberValue(entry.match_score),
  }));
}

export async function getRawEventApi(eventId: string): Promise<RawEventRow | null> {
  const result = await invoke("memory_manager_get_raw_event", { eventId });
  if (!isRecord(result)) throw new Error("Invalid raw event response");
  const event = normalizeRawRow(result.event);
  if (!event) throw new Error("Invalid raw event");
  return event;
}

export async function confirmFactsApi(
  targets: FactMutationTarget[],
): Promise<MutationReceiptResponse> {
  const result = await invoke("memory_manager_confirm_facts", { targets });
  if (
    !isRecord(result) ||
    result.changed !== true ||
    !hasMutationReceipt(result.receipt)
  ) {
    throw new Error("Invalid mutation receipt");
  }
  return result as unknown as MutationReceiptResponse;
}

export async function editFactApi(edit: {
  fact_id: string;
  expected_revision: number;
  expected_status: FactStatus;
  predicate: string;
  value: string;
}): Promise<MutationReceiptResponse> {
  const result = await invoke("memory_manager_edit_fact", { edit });
  if (
    !isRecord(result) ||
    result.changed !== true ||
    !hasMutationReceipt(result.receipt)
  ) {
    throw new Error("Invalid mutation receipt");
  }
  return result as unknown as MutationReceiptResponse;
}

export async function editFactsBulkApi(
  targets: FactMutationTarget[],
  predicate: string | null,
  value: string | null,
): Promise<MutationReceiptResponse> {
  const result = await invoke("memory_manager_edit_facts_bulk", {
    edit: { targets, predicate, value },
  });
  if (
    !isRecord(result) ||
    result.changed !== true ||
    !hasMutationReceipt(result.receipt)
  ) {
    throw new Error("Invalid mutation receipt");
  }
  return result as unknown as MutationReceiptResponse;
}

export async function deleteFactsApi(
  fact_ids: string[],
  expected_revisions: Record<string, number>,
): Promise<MutationReceiptResponse> {
  const result = await invoke("memory_manager_delete_facts", {
    request: { fact_ids, expected_revisions },
  });
  if (
    !isRecord(result) ||
    result.changed !== true ||
    !hasMutationReceipt(result.receipt)
  ) {
    throw new Error("Invalid mutation receipt");
  }
  return result as unknown as MutationReceiptResponse;
}

export async function undoMemoryManagerApi(token: string): Promise<void> {
  await invoke("memory_manager_undo", { request: { undo_token: token } });
}

export async function retrySummaryApi(
  eventId: string,
  expectedStatus: SummaryStatus,
): Promise<SummaryRetryResult> {
  const result = await invoke("memory_manager_retry_summary", {
    request: {
      event_id: eventId,
      expected_status: expectedStatus,
    },
  });
  const retry =
    isRecord(result) &&
    stringValue(result.attempt_id) &&
    result.status === "pending" &&
    hasMutationReceipt(result.receipt)
      ? (result as unknown as SummaryRetryResult)
      : null;
  if (!retry) throw new Error("Invalid summary retry response");
  return retry;
}


// ==========================================
// Legacy LanceDB IPC APIs
// ==========================================

export async function listLanceMemoriesApi(
  limit = 5000,
  offset = 0,
): Promise<MemoryItem[]> {
  const data: unknown = await invoke("list_lance_memories", { limit, offset });
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
  return rawMemories.map((memory) => {
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
}

export async function importMemoriesToLanceApi(
  items: Array<{
    id: string;
    document: string;
    memory_type: string;
    source: string;
    timestamp: string;
    user_id: string;
  }>,
  vectors: null = null,
): Promise<void> {
  await invoke("import_memories_to_lance", { items, vectors });
}

export async function deleteLanceMemoriesBulkApi(ids: string[]): Promise<void> {
  await invoke("delete_lance_memories_bulk", { ids });
}

export async function updateLanceMemoriesBulkApi(
  items: Array<{
    id: string;
    document: string;
    memory_type: string;
    source: string;
    timestamp: string;
    user_id: string;
  }>,
): Promise<void> {
  await invoke("update_lance_memories_bulk", { items });
}

export async function generateBlogFromMemoriesApi(
  ids: string[],
): Promise<{ filename: string; content: string }> {
  return await invoke<{ filename: string; content: string }>(
    "generate_blog_from_memories",
    { ids },
  );
}

export async function lanceBackupApi(): Promise<string> {
  return await invoke<string>("lance_backup");
}

export async function lanceExportJsonApi(): Promise<string> {
  return await invoke<string>("lance_export_json", {});
}

export async function memoryManagerProcessAllApi(): Promise<{
  accepted?: boolean;
  progress: MemoryBackfillProgress;
}> {
  const result = await invoke("memory_manager_process_all");
  if (!isRecord(result)) {
    throw new Error("Invalid all-memory processing response");
  }
  const progress = normalizeMemoryBackfillProgress(result.progress);
  if (!progress) {
    throw new Error("Invalid all-memory processing progress");
  }
  return {
    accepted: typeof result.accepted === "boolean" ? result.accepted : undefined,
    progress,
  };
}

export async function getLanceMigrationStatusApi(): Promise<MemoryMigrationStatus | null> {
  const raw = await invoke("get_lance_migration_status");
  return normalizeMemoryMigrationStatus(raw);
}

