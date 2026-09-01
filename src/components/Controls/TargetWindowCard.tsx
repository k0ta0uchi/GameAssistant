import React, { useState } from 'react';
import { Monitor, RefreshCw, Eye, Camera } from 'lucide-react';

interface TargetWindowCardProps {
  windows: string[];
  selectedWindow: string;
  onWindowChange: (window: string) => void;
  onRefresh: () => void;
  previewImage: string | null;
  onFetchPreview: () => void;
}

export const TargetWindowCard: React.FC<TargetWindowCardProps> = ({
  windows,
  selectedWindow,
  onWindowChange,
  onRefresh,
  previewImage,
  onFetchPreview,
}) => {
  const [isCapturing, setIsCapturing] = useState<boolean>(false);

  const handleCaptureClick = async () => {
    setIsCapturing(true);
    try {
      await onFetchPreview();
    } finally {
      setTimeout(() => setIsCapturing(false), 400);
    }
  };

  return (
    <div className="linear-card p-3.5 flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wider text-[#8a8f98]">
          <Monitor className="w-3.5 h-3.5 text-[#02b8cc]" />
          <span>Target Window</span>
        </div>
        <div className="flex items-center gap-1">
          <button
            onClick={handleCaptureClick}
            disabled={isCapturing || !selectedWindow}
            className="p-1 text-[#8a8f98] hover:text-[#d0d6e0] hover:bg-[#161718] rounded transition-colors disabled:opacity-40"
            title="現在の画面をスクリーンショット撮影"
          >
            <Camera className={`w-3.5 h-3.5 ${isCapturing ? 'text-[#e4f222] animate-pulse' : ''}`} />
          </button>
          <button
            onClick={onRefresh}
            className="p-1 text-[#8a8f98] hover:text-[#d0d6e0] hover:bg-[#161718] rounded transition-colors"
            title="ウィンドウ一覧を更新"
          >
            <RefreshCw className="w-3.5 h-3.5" />
          </button>
        </div>
      </div>

      <div>
        <select
          value={selectedWindow}
          onChange={(e) => onWindowChange(e.target.value)}
          className="w-full text-xs linear-input py-1.5 px-2 bg-[#0f1011] text-[#d0d6e0] cursor-pointer truncate"
        >
          {windows.length === 0 ? (
            <option value="" className="bg-[#161718] text-[#8a8f98]">(No active windows)</option>
          ) : (
            windows.map((win) => (
              <option key={win} value={win} className="bg-[#161718] text-[#d0d6e0]">
                {win}
              </option>
            ))
          )}
        </select>
      </div>

      {previewImage ? (
        <div className="group relative rounded-[6px] overflow-hidden border border-[#23252a] aspect-video bg-[#08090a] flex items-center justify-center">
          <img
            src={previewImage}
            alt="Window Preview"
            className="w-full h-full object-contain transition-transform duration-200 group-hover:scale-[1.02]"
          />
          <div className="absolute inset-0 bg-gradient-to-t from-black/60 via-transparent to-transparent opacity-0 group-hover:opacity-100 transition-opacity flex items-end p-2 pointer-events-none">
            <span className="text-[10px] font-mono text-white/90 truncate">
              {selectedWindow}
            </span>
          </div>
        </div>
      ) : (
        <div className="rounded-[6px] border border-[#23252a] border-dashed aspect-video bg-[#08090a]/50 flex flex-col items-center justify-center text-[#62666d] text-[11px] gap-1">
          <Eye className="w-4 h-4 opacity-50" />
          <span>No Preview Captured</span>
        </div>
      )}
    </div>
  );
};
