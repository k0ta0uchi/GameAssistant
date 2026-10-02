import React, { useRef, useState } from 'react';
import { Play, Square, RotateCcw } from 'lucide-react';

interface ActionButtonsProps {
  sessionRunning: boolean;
  sessionStarting?: boolean;
  enginesReady?: boolean;
  enginesInitializing?: boolean;
  onStart: () => void;
  onStop: () => void;
  onRestartWhisper: () => void;
  autoCommentaryEnabled?: boolean;
  onToggleAutoCommentary?: (enabled: boolean) => Promise<void>;
}

export const ActionButtons: React.FC<ActionButtonsProps> = ({
  sessionRunning,
  sessionStarting = false,
  enginesReady = true,
  enginesInitializing = false,
  onStart,
  onStop,
  onRestartWhisper,
  autoCommentaryEnabled = false,
  onToggleAutoCommentary,
}) => {
  const saving = useRef(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const toggle = async () => {
    if (saving.current || !onToggleAutoCommentary) return;
    saving.current = true;
    setBusy(true);
    setError("");
    try { await onToggleAutoCommentary(!autoCommentaryEnabled); }
    catch (e) { setError(`自動ツッコミの保存に失敗しました: ${String(e)}`); }
    finally { saving.current = false; setBusy(false); }
  };
  return (
    <div className="flex flex-col gap-2">
      {sessionRunning ? (
        <button
          onClick={onStop}
          className="w-full flex items-center justify-center gap-2 py-2.5 px-4 bg-[#eb5757] hover:bg-[#d64545] text-white font-medium rounded-[6px] transition-all shadow-sm active:scale-[0.98]"
        >
          <Square className="w-4 h-4 fill-current" />
          <span>Stop Session</span>
        </button>
      ) : (
        <button
          onClick={onStart}
          disabled={sessionStarting || !enginesReady}
          aria-busy={sessionStarting || enginesInitializing}
          className="w-full flex items-center justify-center gap-2 py-2.5 px-4 linear-btn-primary active:scale-[0.98] disabled:cursor-wait disabled:opacity-60"
        >
          <Play className="w-4 h-4 fill-current" />
          <span>
            {sessionStarting
              ? "Preparing ASR..."
              : enginesInitializing
                ? "Initializing engines..."
                : !enginesReady
                  ? "Engines unavailable"
                  : "Start Session"}
          </span>
        </button>
      )}

      <button
        aria-label="自動ツッコミ"
        aria-pressed={autoCommentaryEnabled}
        aria-busy={busy}
        disabled={busy || !onToggleAutoCommentary}
        onClick={toggle}
        className={`w-full py-2 px-3 rounded-[6px] text-xs font-medium disabled:opacity-60 ${autoCommentaryEnabled ? 'linear-btn-primary' : 'linear-btn-ghost'}`}
      >
        自動ツッコミ: {autoCommentaryEnabled ? 'ON' : 'OFF'}{busy ? ' (保存中...)' : ''}
      </button>
      {error && <p role="alert" className="text-xs text-red-400">{error}</p>}

      <button
        onClick={onRestartWhisper}
        disabled={sessionStarting || enginesInitializing}
        className="w-full flex items-center justify-center gap-2 py-1.5 px-3 linear-btn-ghost text-xs text-[#8a8f98] hover:text-[#d0d6e0] disabled:cursor-wait disabled:opacity-40"
        title="音声認識エンジン (Whisper) を再起動します"
      >
        <RotateCcw className="w-3.5 h-3.5" />
        <span>Restart Whisper</span>
      </button>
    </div>
  );
};
