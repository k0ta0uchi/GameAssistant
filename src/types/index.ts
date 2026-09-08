// TypeScript 型定義

export interface SystemStatus {
  asr: boolean;
  gemini: boolean;
  tts: boolean;
  twitch: boolean;
  session: boolean;
}

export interface LogEntry {
  type: "log";
  timestamp: string;
  level: "DEBUG" | "INFO" | "WARNING" | "ERROR" | "CRITICAL";
  logger: string;
  message: string;
}

export interface AsrEvent {
  type: "asr";
  text: string;
  is_final: boolean;
  /** Canonical source stream used by the native and WebSocket transports. */
  stream?: string;
  /** Native wake-word/prompt decision; transports must not re-derive it. */
  is_prompt?: boolean;
  latency_ms?: number | null;
  /** Final events carry their durable identity; partials may be null/absent. */
  event_id?: string | null;
}

export interface AsrEntry {
  id: string;
  text: string;
  timestamp: string;
  isDiscord?: boolean;
  isPrompt?: boolean;
  /** Native Whisper inference latency for this finalized utterance. */
  latencyMs?: number | null;
}

/** A durable Fact derived from a live session utterance. */
export interface FactEntry {
  id: string;
  text: string;
  timestamp: string;
  source?: string;
  sourceEventId?: string;
}

export interface LevelMeterEvent {
  type: "level_meter";
  level: number;
}

export interface GeminiResponseEvent {
  type: "gemini_response";
  text: string;
}

export interface ResourceInfo {
  used: number; // MB
  total: number; // MB
  percent: number; // %
}

export interface ResourceStatusEvent {
  type: "resource_status";
  vram: ResourceInfo;
  ram: ResourceInfo;
}

export interface CommentaryTimerEvent {
  type: "commentary_timer";
  progress: number;
  remaining: number;
}

export interface StatusEvent {
  type: "status";
  status: SystemStatus;
}

export interface LogHistoryEvent {
  type: "log_history";
  logs: LogEntry[];
}

export type WsMessage =
  | LogEntry
  | LogHistoryEvent
  | AsrEvent
  | LevelMeterEvent
  | GeminiResponseEvent
  | ResourceStatusEvent
  | CommentaryTimerEvent
  | StatusEvent;

export interface SkillItem {
  id: string;
  name: string;
  description: string;
  guidelines?: string;
  file_path?: string;
}

export interface SkillsResponse {
  skills: SkillItem[];
  enabled_skills: string[];
  master_enabled: boolean;
}

export interface MemoryItem {
  id: string;
  key?: string;
  content: string;
  source?: string;
  user?: string;
  type?: string;
  timestamp?: string;
  display_ts?: string;
}

/** DTOs for the additive memory-v2 manager seam. Raw compatibility DTOs above
 * intentionally remain unchanged for the legacy Raw view. */
export type FactStatus = "auto" | "confirmed" | "edited";
export type SummaryStatus =
  | "pending"
  | "completed"
  | "skipped"
  | "fallback"
  | "error"
  | "legacy"
  | "deleted";
export type MemorySort = "newest" | "oldest" | "relevance";
export type VectorSource = "document" | "summary" | "none";

export interface MemoryFilters {
  statuses: string[];
  sources: string[];
  event_types: string[];
  subjects: string[];
  occurred_from: string | null;
  occurred_to: string | null;
  has_summary: boolean | null;
}

export interface MemoryPageRequest {
  page_size: number;
  cursor: string | null;
  search: string | null;
  sort: MemorySort;
  filters: MemoryFilters;
}

export interface MemoryPageInfo {
  next_cursor: string | null;
  has_more: boolean;
  total: number | null;
  snapshot_sequence: number;
}

// Contract names retained as aliases for consumers that mirror the backend
// DTO names directly.
export type PageRequest = MemoryPageRequest;
export type PageInfo = MemoryPageInfo;

export interface RawEventRow {
  event_id: string;
  legacy_id: string | null;
  subject: string;
  event_type: string;
  source: string;
  occurred_at: string;
  content_preview: string;
  content: string | null;
  summary_status: SummaryStatus | null;
  derived_fact_ids: string[];
  vector_source: VectorSource | null;
}

export interface FactRow {
  fact_id: string;
  subject: string;
  predicate: string;
  key: string;
  value: string;
  status: FactStatus;
  evidence_count: number;
  latest_evidence_at: string | null;
  source_event_ids: string[];
  revision: number;
  operation_id: string;
}

export interface SummaryRow {
  summary_id: string;
  event_id: string;
  summary: string | null;
  status: SummaryStatus;
  error: string | null;
  /** Additive reason alias; older payloads may provide only `error`. */
  reason?: string | null;
  model_id: string | null;
  prompt_version: string | null;
  attempt_id?: string | null;
  /** Number of re-attempts after the first durable attempt. */
  retry_count?: number;
  vector_source: VectorSource | null;
  derived_fact_id: string | null;
  occurred_at: string;
  source: string;
  event_type: string;
}

export interface FactEvidence {
  event_id: string;
  subject: string;
  event_type: string;
  source: string;
  occurred_at: string;
  content: string;
  relation: "source" | "supporting" | "conflicting";
  match_score: number | null;
}

export interface JournalMutationReceipt {
  operation_id: string;
  journal_sequence: number;
  committed_at: string;
  undo_token: string | null;
  undo_expires_at: string | null;
}

export interface MemoryMutationResult {
  changed: boolean;
  items: Array<FactRow | SummaryRow>;
  receipt: JournalMutationReceipt;
}
export type MutationResult = MemoryMutationResult;

export interface FactMutationTarget {
  fact_id: string;
  expected_revision: number;
  expected_status: FactStatus;
}

export interface FactEdit extends FactMutationTarget {
  predicate: string;
  value: string;
}

export interface FactConflict {
  conflict_id: string;
  fact_id: string;
  current: FactRow;
  attempted: {
    predicate: string;
    value: string;
    expected_revision: number;
    expected_status: FactStatus;
  };
  evidence: FactEvidence[];
  resolution: "keep_current" | "apply_attempted" | "cancel";
}

export interface SummaryRetryRequest {
  event_id: string;
  expected_status: Extract<SummaryStatus, "fallback" | "error" | "skipped">;
}

export interface SummaryRetryResult {
  attempt_id: string;
  event_id: string;
  status: "pending";
  receipt: JournalMutationReceipt;
}

/** Progress emitted by the explicit all-memory semantic backfill action. */
export type MemoryBackfillState = "running" | "completed" | "error";

export interface MemoryBackfillFinalCounts {
  processed: number;
  persisted: number;
  skipped: number;
  failed: number;
  remaining: number;
}

export interface MemoryBackfillProgress {
  state: MemoryBackfillState;
  processed: number;
  total: number;
  queued: number;
  skipped: number;
  failed: number;
  persisted: number;
  excluded?: number;
  attempted?: number;
  message: string;
  error: string | null;
  /** Latest row-level reason; `lastErrorReason` remains a compatibility alias. */
  reason?: string | null;
  /** Latest attempt identity when a producer supplies one. */
  attempt_id?: string | null;
  /** Number of retries, excluding the first attempt. */
  retry_count?: number;
  remaining?: number;
  reasonCounts?: Record<string, number>;
  lastErrorReason?: string | null;
  fatalError?: { code: string; message?: string } | null;
  /** Terminal snapshot of row counts; populated for all normalized payloads. */
  final_counts?: MemoryBackfillFinalCounts;
}

export interface MemoryBackfillStart {
  accepted: boolean;
  progress: MemoryBackfillProgress;
}

export const normalizeMemoryBackfillProgress = (
  value: unknown,
): MemoryBackfillProgress | null => {
  if (!value || typeof value !== "object") return null;
  const raw = value as Record<string, unknown>;
  const state = raw.state;
  if (state !== "running" && state !== "completed" && state !== "error") {
    return null;
  }
  const numberValue = (candidate: unknown): number =>
    typeof candidate === "number" && Number.isFinite(candidate)
      ? Math.max(0, Math.floor(candidate))
      : 0;
  const total = numberValue(raw.total);
  const processed = Math.min(numberValue(raw.processed), total);
  const excluded = numberValue(raw.excluded);
  const attempted = numberValue(raw.attempted);
  const suppliedRemaining = numberValue(
    raw.remaining === undefined ? raw.remaining_count : raw.remaining,
  );
  const suppliedRetryCount = numberValue(
    raw.retry_count === undefined ? raw.retryCount : raw.retry_count,
  );
  const attemptSource =
    raw.attempt_id === undefined ? raw.attemptId : raw.attempt_id;
  const attemptId =
    typeof attemptSource === "string" && attemptSource.trim()
      ? attemptSource.trim()
      : null;
  const canonicalReason = (candidate: unknown): string | null => {
    if (typeof candidate !== "string" || !candidate.trim()) return null;
    const value = candidate.trim().toLowerCase();
    const known = [
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
    ];
    return known.find((reason) => value === reason || value.includes(reason)) ||
      "inference_failed";
  };
  const reasonSource =
    raw.reasonCounts === undefined ? raw.reason_counts : raw.reasonCounts;
  const reasonCounts: Record<string, number> = {};
  if (reasonSource && typeof reasonSource === "object") {
    for (const [rawReason, count] of Object.entries(
      reasonSource as Record<string, unknown>,
    )) {
      const reason = canonicalReason(rawReason);
      if (!reason) continue;
      const normalized = numberValue(count);
      if (normalized > 0)
        reasonCounts[reason] = (reasonCounts[reason] || 0) + normalized;
    }
  }
  const fatalSource =
    raw.fatalError === undefined ? raw.fatal_error : raw.fatalError;
  const fatalError =
    typeof fatalSource === "string" && fatalSource.trim()
      ? { code: fatalSource.trim() }
      : fatalSource && typeof fatalSource === "object"
        ? (() => {
          const candidate = fatalSource as Record<string, unknown>;
          const code =
            typeof candidate.code === "string" && candidate.code.trim()
              ? candidate.code
              : null;
          if (!code) return null;
          const message =
            typeof candidate.message === "string" && candidate.message.trim()
              ? candidate.message
              : undefined;
          return { code, ...(message ? { message } : {}) };
          })()
        : null;
  const reason = canonicalReason(
    raw.reason === undefined
      ? raw.lastErrorReason === undefined
        ? raw.last_error_reason
        : raw.lastErrorReason
      : raw.reason,
  );
  const remaining =
    raw.remaining !== undefined || raw.remaining_count !== undefined
      ? Math.min(suppliedRemaining, total)
      : Math.max(0, total - processed);
  const finalSource =
    raw.final_counts === undefined
      ? raw.finalCounts === undefined
        ? raw.final_row_counts
        : raw.finalCounts
      : raw.final_counts;
  const finalRecord =
    finalSource && typeof finalSource === "object"
      ? (finalSource as Record<string, unknown>)
      : null;
  const finalCounts: MemoryBackfillFinalCounts = {
    processed: numberValue(finalRecord?.processed ?? processed),
    persisted: numberValue(finalRecord?.persisted ?? raw.persisted),
    skipped: numberValue(finalRecord?.skipped ?? raw.skipped),
    failed: numberValue(finalRecord?.failed ?? raw.failed),
    remaining: numberValue(finalRecord?.remaining ?? remaining),
  };
  return {
    state,
    processed,
    total,
    queued: numberValue(raw.queued),
    skipped: numberValue(raw.skipped),
    failed: numberValue(raw.failed),
    persisted: numberValue(raw.persisted),
    excluded,
    attempted,
    message: typeof raw.message === "string" ? raw.message : "",
    error: typeof raw.error === "string" && raw.error.trim() ? raw.error : null,
    reason,
    attempt_id: attemptId,
    retry_count: suppliedRetryCount,
    remaining,
    reasonCounts,
    lastErrorReason: canonicalReason(
      raw.lastErrorReason === undefined
        ? raw.last_error_reason
        : raw.lastErrorReason,
    ),
    fatalError,
    final_counts: finalCounts,
  };
};

export type MemoryMigrationState = "idle" | "running" | "completed" | "error";

export interface MemoryMigrationStatus {
  status: MemoryMigrationState;
  processed: number;
  total: number | null;
  message: string;
  error: string | null;
}

export const normalizeMemoryMigrationStatus = (
  value: unknown,
): MemoryMigrationStatus | null => {
  if (!value || typeof value !== "object") return null;
  const raw = value as Record<string, unknown>;
  const states: MemoryMigrationState[] = [
    "idle",
    "running",
    "completed",
    "error",
  ];
  if (!states.includes(raw.status as MemoryMigrationState)) return null;
  const numeric = (candidate: unknown, fallback: number): number =>
    typeof candidate === "number" && Number.isFinite(candidate)
      ? Math.max(0, Math.floor(candidate))
      : fallback;
  const total =
    raw.total === null || raw.total === undefined
      ? null
      : numeric(raw.total, 0);
  const processed = numeric(raw.processed, 0);
  return {
    status: raw.status as MemoryMigrationState,
    processed: total === null ? processed : Math.min(processed, total),
    total,
    message: typeof raw.message === "string" ? raw.message : "",
    error: typeof raw.error === "string" && raw.error.trim() ? raw.error : null,
  };
};

export interface PromptItem {
  id: string;
  title: string;
  category: "Character" | "Commentary" | "Blog" | "Memory" | "Voice";
  icon: string;
  description: string;
  default: string;
  value: string;
  is_modified: boolean;
}

export interface ModelStatus {
  id: string;
  name: string;
  description: string;
  hf_repo: string;
  category: "ASR" | "Embedding" | "LLM" | "Other";
  required: boolean;
  estimated_size_bytes: number;
  is_installed: boolean;
  actual_size_bytes: number;
  local_path: string;
}

export interface DownloadProgressEvent {
  model_id: string;
  current_bytes: number;
  total_bytes: number;
  speed_mbps: number;
  percent: number;
  status: "downloading" | "completed" | "error" | "cancelled";
  error_message?: string;
}

/** Canonical local memory summary runtime states emitted by the Rust backend. */
export const LOCAL_SUMMARY_STATES = [
  "disabled",
  "model_missing",
  "starting",
  "ready",
  "busy",
  "error",
] as const;
export type LocalSummaryState = (typeof LOCAL_SUMMARY_STATES)[number];

/** Payload of `get_local_summary_status` and the `local-summary-status` event. */
export interface LocalSummaryStatus {
  state: LocalSummaryState;
  queueDepth: number;
  fallbackActive: boolean;
  message: string | null;
}

/** Verdict returned by `test_local_summary`. Mirrors Rust SummaryDecision. */
export interface SummaryDecision {
  should_store: boolean;
  summary: string | null;
}

/** Fixed text used by the SettingsModal test-summary probe (never persisted). */
export const LOCAL_SUMMARY_TEST_TEXT =
  "ユーザーは猫を2匹飼っています。好きな食べ物はカレーで、名前は蒼です。" as const;

export const GEMMA_MODEL_ID = "gemma-3-1b-it-Q4_K_S.gguf" as const;
export const GEMMA_TERMS_VERSION = "gemma-terms-v1" as const;
export const GEMMA_TERMS_MODEL_SHA256 =
  "f1536b0b60e53ffd98c945a3295f51154db3ffdd95329d86971c83c86c56899f" as const;
export const GEMMA_TERMS_SOURCE = "https://ai.google.dev/gemma/terms" as const;

export interface GemmaTermsMetadata {
  gemma_terms_accepted: boolean;
  gemma_terms_version: string;
  gemma_terms_model_sha256: string;
  gemma_terms_source: string;
}

export const hasValidGemmaTerms = (
  metadata: Partial<GemmaTermsMetadata> | null | undefined,
): boolean =>
  metadata?.gemma_terms_accepted === true &&
  metadata.gemma_terms_version === GEMMA_TERMS_VERSION &&
  metadata.gemma_terms_model_sha256 === GEMMA_TERMS_MODEL_SHA256 &&
  metadata.gemma_terms_source === GEMMA_TERMS_SOURCE;

/** Finite discriminator emitted by Rust's bootstrap RuntimeStatus. */
export const RUNTIME_STATUS_VALUES = [
  "ready",
  "pending",
  "running",
  "error",
  "cancelled",
] as const;
export type RuntimeStatus = (typeof RUNTIME_STATUS_VALUES)[number];

/** First-run runtime setup state returned by the Rust setup manager. */
export type SetupStepId =
  | "python"
  | "venv"
  | "packages"
  | "scripts"
  | "models"
  | "complete"
  | string;

export type SetupStepState =
  | "pending"
  | "running"
  | "completed"
  | "error"
  | "cancelled"
  | string;

export interface SetupStepStatus {
  id: SetupStepId;
  label?: string;
  status: SetupStepState;
  progress?: number;
  message?: string;
  error?: string | null;
}

/** Canonical setup status shape emitted by the Rust runtime setup manager. */
export interface SetupStatus extends GemmaTermsMetadata {
  ready: boolean;
  setup_required?: boolean;
  running?: boolean;
  cancelled?: boolean;
  status?: RuntimeStatus;
  current_stage?: SetupStepId | null;
  progress?: number;
  message?: string | null;
  error?: string | null;
  stages?: SetupStepStatus[];
  completed_stages?: SetupStepId[];
  required_models_ready?: boolean;
  required_models_missing?: string[];
  writable?: boolean;
  elevation_required?: boolean;
  elevation_message?: string | null;
  uv_present?: boolean;
  scripts_present?: boolean;
  python_present?: boolean;
  venv_present?: boolean;
  lock_present?: boolean;
  llama_server_present?: boolean;
  /** Optional readiness diagnostics emitted by newer portable runtimes. */
  dependency_ready?: boolean;
  python_import_ready?: boolean;
  tokenizer_ready?: boolean;
  embedding_ready?: boolean;
  asr_websocket_ready?: boolean;
  diagnostics?: Record<string, unknown>;
  [key: string]: unknown;
}

/** Progress payload emitted by the setup manager during run_setup. */
export interface SetupProgress {
  stage: SetupStepId;
  status?: SetupStepState;
  progress: number;
  message?: string | null;
  error?: string | null;
  current?: number;
  total?: number;
}

/**
 * Startup engine initialization is deliberately separate from first-run
 * setup.  Setup verifies the portable runtime and model files; this status
 * reports the live ASR/GLuCoSE/memory-v2 work performed from the main screen.
 */
export type RuntimeInitializationState =
  | "idle"
  | "running"
  | "completed"
  | "error";

export type RuntimeInitializationStageId =
  | "asr"
  | "embedding"
  | "memory_v2"
  | "complete"
  | string;

export interface RuntimeInitializationStage {
  id: RuntimeInitializationStageId;
  label?: string;
  status: "pending" | "running" | "completed" | "error" | string;
  progress: number;
  elapsed_ms?: number;
  error?: string | null;
}

/** Live startup initialization snapshot emitted by the native runtime. */
export interface RuntimeInitializationStatus {
  status: RuntimeInitializationState;
  progress: number;
  current_stage?: RuntimeInitializationStageId | null;
  message?: string | null;
  elapsed_ms?: number;
  started_at?: string | null;
  completed_at?: string | null;
  stages: RuntimeInitializationStage[];
  asr_ready: boolean;
  embedding_ready: boolean;
  memory_v2_ready: boolean;
  error?: string | null;
}
