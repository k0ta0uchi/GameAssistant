import type React from "react";
import {
  Search,
  CheckSquare,
  Square,
  Loader2,
  AlertTriangle,
  ChevronLeft,
  ChevronRight,
  Eye,
  RotateCcw,
  Copy,
  RefreshCw,
} from "lucide-react";
import {
  FACT_STATUSES,
  SUMMARY_STATUSES,
  statusBadgeClass,
  summaryReasonLabel,
  summaryRetryAllowed,
} from "../../../services/memoryService";
import { useMemoryV2 } from "../../../hooks/useMemoryV2";
import { MemoryContextMenu } from "./MemoryContextMenu";

export interface MemoryV2PanelProps {
  isOpen: boolean;
}

const SemanticBadge: React.FC<{ status: string }> = ({ status }) => (
  <span
    className={`inline-flex rounded border px-1.5 py-0.5 text-[10px] font-mono ${statusBadgeClass(status)}`}
  >
    {status}
  </span>
);

export const MemoryV2Panel: React.FC<MemoryV2PanelProps> = ({ isOpen }) => {
  const {
    subtab,
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
    cursorHistory,
    setCursor,
    setCursorHistory,
    loading,
    error,
    selectedFactIds,
    selectedSummaryIds,
    selectedCount,
    rows,
    activeFact,
    activeSummary,
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
    confirmDelete,
    setConfirmDelete,
    conflict,
    resolveConflict,
    mutating,
    contextMenu,
    setContextMenu,
    semanticContextActions,
    loadPage,
    selectAllVisible,
    copySelectedSemanticAsJson,
    toggleSelected,
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
  } = useMemoryV2({ isOpen });

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
          onClick={() => void loadPage()}
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
        <button
          type="button"
          disabled={selectedCount === 0}
          onClick={() => void copySelectedSemanticAsJson()}
          className="linear-btn-ghost px-2 py-1 flex items-center gap-1.5 text-xs disabled:opacity-40 hover:text-[#02b8cc]"
          title="Ctrl+C で選択項目を JSON としてコピー"
        >
          <Copy className="w-3 h-3 text-[#02b8cc]" />
          <span>Copy JSON ({selectedCount})</span>
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
