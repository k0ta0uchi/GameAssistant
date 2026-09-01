import React from 'react';
import { Play, Square, RotateCcw } from 'lucide-react';

interface ActionButtonsProps {
  sessionRunning: boolean;
  onStart: () => void;
  onStop: () => void;
  onRestartWhisper: () => void;
}

export const ActionButtons: React.FC<ActionButtonsProps> = ({
  sessionRunning,
  onStart,
  onStop,
  onRestartWhisper,
}) => {
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
          className="w-full flex items-center justify-center gap-2 py-2.5 px-4 linear-btn-primary active:scale-[0.98]"
        >
          <Play className="w-4 h-4 fill-current" />
          <span>Start Session</span>
        </button>
      )}

      <button
        onClick={onRestartWhisper}
        className="w-full flex items-center justify-center gap-2 py-1.5 px-3 linear-btn-ghost text-xs text-[#8a8f98] hover:text-[#d0d6e0]"
        title="音声認識エンジン (Whisper) を再起動します"
      >
        <RotateCcw className="w-3.5 h-3.5" />
        <span>Restart Whisper</span>
      </button>
    </div>
  );
};
