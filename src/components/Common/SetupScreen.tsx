import React, { useEffect, useRef, useState } from 'react';
import {
  AlertTriangle,
  Check,
  CheckCircle2,
  Circle,
  Download,
  FolderOpen,
  Loader2,
  PackageCheck,
  Play,
  RefreshCw,
  X,
  XCircle,
} from 'lucide-react';
import { hasValidGemmaTerms, SetupProgress, SetupStatus, SetupStepId, SetupStepState } from '../../types';
import { isTermsAcceptanceRequired } from '../../hooks/useAppState';

interface SetupScreenProps {
  status: SetupStatus | null;
  progress: SetupProgress | null;
  running: boolean;
  error: string | null;
  onStart: () => void;
  onRetry: () => void;
  onRequestElevation: () => void;
  elevationRequesting: boolean;
  onCancel: () => void;
  onAcceptTerms: () => void;
  /** Focus target owned by the caller, used after the mandatory view unmounts. */
  focusReturnRef?: React.RefObject<HTMLElement | null>;
}

const DEFAULT_STEPS: Array<{ id: SetupStepId; label: string; detail: string; icon: React.ElementType }> = [
  { id: 'python', label: 'Python runtime', detail: 'Python 3.12 を EXE と同じフォルダへ準備', icon: Download },
  { id: 'venv', label: 'Virtual environment', detail: 'アプリ専用の venv を作成', icon: FolderOpen },
  { id: 'packages', label: 'Python packages', detail: '固定された依存パッケージをインストール', icon: PackageCheck },
  { id: 'scripts', label: 'Embedded scripts', detail: '音声処理スクリプトを安全に展開', icon: FolderOpen },
  { id: 'models', label: 'Required models', detail: 'ASR / Embedding / Gemma モデルをダウンロード', icon: Download },
];

const MANUAL_MODEL_GUIDANCE: Array<{
  id: string;
  label: string;
  path: string;
  files: string[];
  url: string;
}> = [
  {
    id: 'kotoba-whisper-v2.0-faster',
    label: 'Kotoba-Whisper v2.0 (ASR)',
    path: './models/kotoba-whisper-v2.0-faster/',
    files: ['model.bin', 'config.json', 'preprocessor_config.json', 'tokenizer.json', 'vocabulary.json'],
    url: 'https://huggingface.co/kotoba-tech/kotoba-whisper-v2.0-faster/tree/main',
  },
  {
    id: 'GLuCoSE-base-ja',
    label: 'GLuCoSE-base-ja (Embedding)',
    path: './models/GLuCoSE-base-ja/',
    files: [
      '1_Pooling/config.json',
      'added_tokens.json',
      'config.json',
      'config_sentence_transformers.json',
      'entity_vocab.json',
      'modules.json',
      'pytorch_model.bin',
      'sentence_bert_config.json',
      'sentencepiece.bpe.model',
      'special_tokens_map.json',
      'tokenizer_config.json',
    ],
    url: 'https://huggingface.co/pkshatech/GLuCoSE-base-ja/tree/main',
  },
  {
    id: 'gemma-3-1b-it-Q4_K_S.gguf',
    label: 'Gemma 3 1B IT (GGUF)',
    path: './models/gemma-3-1b-it-Q4_K_S.gguf',
    files: ['gemma-3-1b-it-Q4_K_S.gguf'],
    url: 'https://huggingface.co/unsloth/gemma-3-1b-it-GGUF/tree/main',
  },
];

const cleanStage = (stage?: string | null): string => (stage || '').toLowerCase().replace(/[_\s-]/g, '');

const stageMatches = (left: string, right: string): boolean => {
  const a = cleanStage(left);
  const b = cleanStage(right);
  return a === b || a.includes(b) || b.includes(a);
};

const clamp = (value: number): number => Math.max(0, Math.min(100, Number.isFinite(value) ? value : 0));

const statusLabel = (state: SetupStepState): string => {
  switch (state) {
    case 'completed':
      return '完了';
    case 'running':
      return '実行中';
    case 'error':
      return 'エラー';
    case 'cancelled':
      return 'キャンセル';
    default:
      return '待機中';
  }
};

const StageIcon: React.FC<{ state: SetupStepState; Icon: React.ElementType }> = ({ state, Icon }) => {
  if (state === 'completed') return <Check className="h-4 w-4" strokeWidth={2.5} />;
  if (state === 'running') return <Loader2 className="h-4 w-4 animate-spin" />;
  if (state === 'error') return <XCircle className="h-4 w-4" />;
  if (state === 'cancelled') return <X className="h-4 w-4" />;
  return <Icon className="h-4 w-4" />;
};

/** Dedicated first-run setup view. It deliberately uses no browser-only APIs. */
export const SetupScreen: React.FC<SetupScreenProps> = ({
  status,
  progress,
  running,
  error,
  onStart,
  onRetry,
  onRequestElevation,
  elevationRequesting,
  onCancel,
  onAcceptTerms,
  focusReturnRef,
}) => {
  const dialogRef = useRef<HTMLDivElement>(null);
  const initialFocusRef = useRef<HTMLButtonElement>(null);
  const [manualInstallOpen, setManualInstallOpen] = useState(false);

  const handleDialogKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.key === 'Escape') {
      // Setup is mandatory; Escape cannot dismiss the incomplete flow.
      event.preventDefault();
      return;
    }
    if (event.key !== 'Tab' || !dialogRef.current) return;
    const focusable = Array.from(dialogRef.current.querySelectorAll<HTMLElement>(
      'button:not([disabled]), a[href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
    ));
    if (focusable.length === 0) {
      event.preventDefault();
      dialogRef.current.focus();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };
  // RuntimeStatus.progress is the overall setup progress. The event payload's
  // progress belongs only to its current stage (e.g. required model #2).
  const globalProgress = clamp(status?.progress ?? 0);
  const stageProgress = clamp(progress?.progress ?? 0);
  const currentStage = progress?.stage || status?.current_stage || null;
  const completedStages = status?.completed_stages || [];
  const stageStatus = (id: SetupStepId): SetupStepState => {
    const serverStage = status?.stages?.find((stage) => stageMatches(stage.id, id));
    const isCurrentEventStage = currentStage && stageMatches(currentStage, id) && Boolean(progress?.status);
    // Prefer the live event for its current stage. RuntimeStatus may still
    // contain the previous (pending) snapshot while a model event is arriving.
    if (isCurrentEventStage) {
      if (progress?.status === 'error') return 'error';
      if (progress?.status === 'cancelled' || status?.cancelled) return 'cancelled';
      if (progress?.status === 'completed') return 'completed';
      if (running) return 'running';
    }
    if (serverStage?.status) return serverStage.status;
    if (completedStages.some((stage) => stageMatches(stage, id))) return 'completed';
    if (currentStage && stageMatches(currentStage, id)) {
      if (progress?.status === 'error') return 'error';
      if (progress?.status === 'cancelled' || status?.cancelled) return 'cancelled';
      if (progress?.status === 'completed') return 'completed';
      return running ? 'running' : 'pending';
    }
    return 'pending';
  };

  const hasError = Boolean(error || status?.error);
  const elevationRequired = Boolean(status?.elevation_required);
  const isCancelled = Boolean(status?.cancelled) && !running;
  const termsAccepted = hasValidGemmaTerms(status);
  const isComplete = Boolean(status?.ready && status?.required_models_ready === true && termsAccepted) && !running;
  const termsAcceptanceRequired = isTermsAcceptanceRequired(status);
  const termsAcceptanceRunning = termsAcceptanceRequired && running;
  const showAcceptTerms = termsAcceptanceRequired && !running && !isComplete;
  const isExplicitlyPending = status?.running === false
    && (!currentStage || cleanStage(currentStage) === 'pending')
    && !progress?.status;
  const showStart = !running && !hasError && !isComplete && !isCancelled && !showAcceptTerms
    && (isExplicitlyPending || Boolean(currentStage));
  const startLabel = isExplicitlyPending ? 'セットアップを開始' : 'セットアップを続行';
  const missingModelIds = new Set(status?.required_models_missing || []);
  const missingManualModels = MANUAL_MODEL_GUIDANCE.filter((model) => missingModelIds.has(model.id));
  // If a newer backend reports an unknown model id, retain a useful fallback
  // instead of rendering an empty manual-install section.
  const manualModels = missingManualModels.length > 0 ? missingManualModels : MANUAL_MODEL_GUIDANCE;

  // The action button is replaced as setup moves between terms, retry,
  // cancel, and running states. Focus it after each replacement rather than
  // relying on a mount-time active-element snapshot.
  useEffect(() => {
    const action = initialFocusRef.current;
    if (action) {
      action.focus();
    } else {
      dialogRef.current?.focus();
    }
  }, [running, hasError, isCancelled, termsAcceptanceRunning, showAcceptTerms, showStart]);

  useEffect(() => () => {
    focusReturnRef?.current?.focus();
  }, [focusReturnRef]);

  return (
    <div
      ref={dialogRef}
      role="dialog"
      aria-modal="true"
      aria-labelledby="setup-title"
      aria-describedby="setup-description"
      aria-busy={running}
      tabIndex={-1}
      onKeyDown={handleDialogKeyDown}
      className="fixed inset-0 z-[100000] overflow-y-auto bg-[#08090a] text-[#d0d6e0]"
    >
      <div className="mx-auto flex min-h-screen w-full max-w-5xl flex-col px-6 py-8 sm:px-10 lg:py-12">
        <header className="flex items-start justify-between border-b border-[#23252a] pb-6">
          <div>
            <div className="mb-3 flex items-center gap-2 text-[10px] font-semibold uppercase tracking-[0.24em] text-[#e4f222]">
              <span className="h-1.5 w-1.5 rounded-full bg-[#e4f222] shadow-[0_0_10px_#e4f222]" />
              First launch / local runtime
            </div>
            <h1 id="setup-title" className="text-3xl font-semibold tracking-[-0.04em] text-white sm:text-4xl">GameAssistant を準備する</h1>
            <p id="setup-description" className="mt-3 max-w-xl text-sm leading-6 text-[#8a8f98]">
              Python、依存パッケージ、必要なモデルを EXE と同じフォルダ内にセットアップします。
              一度完了すれば、次回からすぐに起動できます。
            </p>
          </div>
          <div className="hidden rounded-full border border-[#383b3f] bg-[#0f1011] px-3 py-1.5 font-mono text-[10px] text-[#8a8f98] sm:block">
            PORTABLE MODE
          </div>
        </header>

        <div className="grid flex-1 gap-8 py-8 lg:grid-cols-[1fr_1.15fr] lg:items-start">
          <section className="relative overflow-hidden rounded-xl border border-[#2d3036] bg-[#0f1011] p-6 shadow-2xl shadow-black/20 sm:p-8">
            <div className="pointer-events-none absolute -right-24 -top-24 h-64 w-64 rounded-full bg-[#e4f222]/[0.04] blur-3xl" />
            <div className="relative">
              <div className="flex items-start justify-between gap-4">
                <div>
                  <p className="text-[11px] font-semibold uppercase tracking-[0.16em] text-[#62666d]">Setup progress</p>
                  <p className="mt-2 text-4xl font-semibold tabular-nums tracking-[-0.05em] text-white">{Math.round(globalProgress)}<span className="ml-1 text-xl text-[#62666d]">%</span></p>
                </div>
                <div className={`flex h-11 w-11 items-center justify-center rounded-lg border ${isComplete ? 'border-[#27a644]/40 bg-[#27a644]/10 text-[#27a644]' : hasError ? 'border-[#eb5757]/40 bg-[#eb5757]/10 text-[#eb5757]' : 'border-[#e4f222]/30 bg-[#e4f222]/10 text-[#e4f222]'}`}>
                  {isComplete ? <CheckCircle2 className="h-5 w-5" /> : hasError ? <AlertTriangle className="h-5 w-5" /> : <Loader2 className={`h-5 w-5 ${running ? 'animate-spin' : ''}`} />}
                </div>
              </div>

              <div
                className="mt-7 h-2 overflow-hidden rounded-full bg-[#23252a]"
                role="progressbar"
                aria-label="セットアップの進行状況"
                aria-valuenow={globalProgress}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuetext={`${Math.round(globalProgress)}%`}
              >
                <div className="h-full rounded-full bg-[#e4f222] transition-[width] duration-500 ease-out" style={{ width: `${globalProgress}%` }} />
              </div>
              <p className="mt-3 min-h-5 text-xs text-[#8a8f98]" role="status" aria-live="polite" aria-atomic="true">
                {progress?.message || status?.message || (
                  running
                    ? 'セットアップを実行しています…'
                    : isComplete
                    ? 'セットアップが完了しました。'
                    : showStart
                    ? `${startLabel}。`
                    : 'セットアップの状態を確認しています…'
                )}
              </p>

              {elevationRequired && (
                <div className="mt-5 rounded-lg border border-[#e4f222]/35 bg-[#e4f222]/[0.08] p-3 text-xs leading-5 text-[#d8dcaa]" role="status">
                  <div className="mb-1 flex items-center gap-2 font-semibold text-[#e4f222]"><AlertTriangle className="h-3.5 w-3.5" /> 管理者権限が必要です</div>
                  <div>{status?.elevation_message?.replace(/^elevation_required:\s*/i, '') || 'EXE と同じフォルダへ保存するため、UAC の確認が必要です。'}</div>
                </div>
              )}

              {hasError && (
                <div className="mt-5 rounded-lg border border-[#eb5757]/30 bg-[#eb5757]/[0.08] p-3 text-xs leading-5 text-[#f2a1a1]" role="alert">
                  <div className="mb-1 flex items-center gap-2 font-semibold text-[#eb5757]"><AlertTriangle className="h-3.5 w-3.5" /> セットアップに失敗しました</div>
                  <div className="break-words text-[#d88f8f]">{error || status?.error}</div>
                </div>
              )}

              {hasError && (
                <div className="mt-4 rounded-lg border border-[#383b3f] bg-[#121314] p-3 text-xs leading-5 text-[#b6bbc4]">
                  <button
                    type="button"
                    className="flex w-full items-center justify-between gap-3 text-left font-semibold text-[#d0d6e0] hover:text-white"
                    aria-expanded={manualInstallOpen}
                    onClick={() => setManualInstallOpen((open) => !open)}
                  >
                    <span>手動でモデルを設置する方法</span>
                    <span aria-hidden="true">{manualInstallOpen ? '−' : '+'}</span>
                  </button>
                  {manualInstallOpen && (
                    <div className="mt-3 border-t border-[#23252a] pt-3" role="region" aria-label="手動モデル設置手順">
                      <p>
                        URL変更や一時的な通信障害で自動取得できない場合は、公式リポジトリから必要なファイルをブラウザで取得し、次の相対パスへ配置してください。
                        README・.gitattributes は不要です。
                      </p>
                      <ol className="mt-3 list-decimal space-y-3 pl-5">
                        {manualModels.map((model) => (
                          <li key={model.id}>
                            <div className="font-semibold text-[#d0d6e0]">{model.label}</div>
                            <div className="mt-1">配置先: <code className="break-all rounded bg-[#08090a] px-1 py-0.5 text-[#e4f222]">{model.path}</code></div>
                            <div>必要ファイル: {model.files.join(', ')}</div>
                            <a className="mt-1 inline-flex items-center text-[#e4f222] underline hover:text-white" href={model.url} target="_blank" rel="noreferrer">
                              公式リポジトリを開く <span aria-hidden="true" className="ml-1">↗</span>
                            </a>
                          </li>
                        ))}
                      </ol>
                      <p className="mt-3 text-[#8a8f98]">
                        配置後に「再試行」を押すとファイルを検証してセットアップを再開します。対象モデルフォルダに古い <code>.gameassistant-install.json</code> が残っている場合は、手動配置前に削除してください。Gemma は NOTICE-GEMMA.txt と利用規約を確認し、同意を完了している必要があります。
                      </p>
                    </div>
                  )}
                </div>
              )}

              {isCancelled && !hasError && (
                <div className="mt-5 rounded-lg border border-[#383b3f] bg-[#161718] p-3 text-xs leading-5 text-[#8a8f98]">
                  セットアップをキャンセルしました。必要なモデルが揃うまでアプリを起動できません。
                </div>
              )}

              {showAcceptTerms && (
                <div className="mt-5 rounded-lg border border-[#e4f222]/30 bg-[#e4f222]/[0.06] p-3 text-xs leading-5 text-[#d8dcaa]" role="status">
                  <div className="mb-1 font-semibold text-[#e4f222]">Gemma Terms の確認が必要です</div>
                  <div>同梱の NOTICE-GEMMA.txt と <a className="underline" href="https://ai.google.dev/gemma/terms" target="_blank" rel="noreferrer">Gemma Terms</a> を確認してください。</div>
                </div>
              )}

              <div className="mt-7 flex flex-wrap gap-2">
                {elevationRequired && (
                  <button onClick={onRequestElevation} disabled={elevationRequesting} className="linear-btn-primary inline-flex items-center gap-2 px-4 py-2.5 text-xs disabled:cursor-wait disabled:opacity-60">
                    {elevationRequesting ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <AlertTriangle className="h-3.5 w-3.5" />}
                    {elevationRequesting ? 'UAC を起動中…' : '管理者としてセットアップ'}
                  </button>
                )}
                {termsAcceptanceRunning ? (
                  <button ref={initialFocusRef} disabled className="linear-btn-primary inline-flex cursor-wait items-center gap-2 px-4 py-2.5 text-xs opacity-60" aria-busy="true">
                    <Loader2 className="h-3.5 w-3.5 animate-spin" /> 規約の同意を保存中…
                  </button>
                ) : showAcceptTerms ? (
                  <button ref={initialFocusRef} onClick={onAcceptTerms} className="linear-btn-primary inline-flex items-center gap-2 px-4 py-2.5 text-xs">
                    規約を確認して同意 <Check className="h-3.5 w-3.5" />
                  </button>
                ) : hasError ? (
                  <>
                    <button ref={initialFocusRef} onClick={onRetry} disabled={running} className="linear-btn-primary inline-flex items-center gap-2 px-4 py-2.5 text-xs disabled:cursor-not-allowed disabled:opacity-50">
                      <RefreshCw className="h-3.5 w-3.5" /> 再試行
                    </button>
                  </>
                ) : isCancelled ? (
                  <>
                    <button ref={initialFocusRef} onClick={onRetry} className="linear-btn-primary inline-flex items-center gap-2 px-4 py-2.5 text-xs"><Play className="h-3.5 w-3.5" /> 再開する</button>
                  </>
                ) : showStart ? (
                  <button ref={initialFocusRef} onClick={onStart} className="linear-btn-primary inline-flex items-center gap-2 px-4 py-2.5 text-xs"><Play className="h-3.5 w-3.5" /> {startLabel}</button>
                ) : running ? (
                  <button ref={initialFocusRef} onClick={onCancel} className="linear-btn-ghost inline-flex items-center gap-2 px-4 py-2.5 text-xs text-[#eb5757] hover:border-[#eb5757]/50 hover:text-[#f2a1a1]"><X className="h-3.5 w-3.5" /> キャンセル</button>
                ) : (
                  <button disabled className="linear-btn-ghost inline-flex cursor-wait items-center gap-2 px-4 py-2.5 text-xs text-[#8a8f98]"><Loader2 className="h-3.5 w-3.5 animate-spin" /> 準備中…</button>
                )}
              </div>
            </div>
          </section>

          <section>
            <div className="mb-3 flex items-end justify-between">
              <div>
                <p className="text-[11px] font-semibold uppercase tracking-[0.16em] text-[#62666d]">What happens next</p>
                <h2 className="mt-1 text-lg font-semibold text-white">ローカル環境を構築</h2>
              </div>
              <span className="font-mono text-[10px] text-[#62666d]">./models · ./venv</span>
            </div>
            <div className="divide-y divide-[#23252a] overflow-hidden rounded-xl border border-[#23252a] bg-[#0f1011]">
              {DEFAULT_STEPS.map(({ id, label, detail, icon: Icon }) => {
                const state = stageStatus(id);
                const serverStage = status?.stages?.find((stage) => stageMatches(stage.id, id));
                const currentEvent = progress && currentStage && stageMatches(progress.stage, id) ? progress : null;
                // Live stage events win over a stale RuntimeStatus stage value.
                const stepProgress = clamp(currentEvent?.progress ?? serverStage?.progress ?? (state === 'completed' ? 100 : state === 'running' ? stageProgress : 0));
                const color = state === 'completed' ? 'text-[#27a644]' : state === 'running' ? 'text-[#e4f222]' : state === 'error' ? 'text-[#eb5757]' : 'text-[#62666d]';
                return (
                  <div key={id} className={`flex items-center gap-3 px-4 py-4 transition-colors ${state === 'running' ? 'bg-[#161718]' : ''}`}>
                    <div className={`flex h-8 w-8 shrink-0 items-center justify-center rounded-md border ${state === 'completed' ? 'border-[#27a644]/30 bg-[#27a644]/10' : state === 'running' ? 'border-[#e4f222]/30 bg-[#e4f222]/10' : state === 'error' ? 'border-[#eb5757]/30 bg-[#eb5757]/10' : 'border-[#23252a] bg-[#161718]'} ${color}`}>
                      <StageIcon state={state} Icon={Icon} />
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center justify-between gap-3">
                        <p className={`text-xs font-semibold ${state === 'pending' ? 'text-[#8a8f98]' : 'text-[#d0d6e0]'}`}>{serverStage?.label || label}</p>
                        <span className={`shrink-0 text-[10px] font-medium ${color}`}>{statusLabel(state)}</span>
                      </div>
                      <p className="mt-1 truncate text-[10px] text-[#62666d]">{serverStage?.message || detail}</p>
                      {state === 'running' && <div className="mt-2 h-1 overflow-hidden rounded-full bg-[#23252a]"><div className="h-full rounded-full bg-[#e4f222] transition-[width] duration-300" style={{ width: `${stepProgress}%` }} /></div>}
                    </div>
                  </div>
                );
              })}
              <div className="flex items-start gap-3 bg-[#e4f222]/[0.035] px-4 py-4">
                <Circle className="mt-0.5 h-4 w-4 shrink-0 text-[#e4f222]" />
                <div>
                  <p className="text-xs font-semibold text-[#d0d6e0]">保存場所は EXE の隣です</p>
                  <p className="mt-1 text-[10px] leading-5 text-[#8a8f98]">管理者権限が必要な場合は Windows が確認します。別の場所へ自動移動することはありません。</p>
                </div>
              </div>
            </div>
          </section>
        </div>

        <footer className="border-t border-[#23252a] pt-5 text-[10px] leading-5 text-[#62666d]">
          セットアップにはネットワーク接続と数 GB の空き容量が必要です。途中でキャンセルしても、次回起動時に続きから再開できます。
        </footer>
      </div>
    </div>
  );
};
