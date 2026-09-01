import React from 'react';
import { Sparkles, MessageSquare } from 'lucide-react';

interface GeminiCardProps {
  response: string;
  isGenerating: boolean;
}

export const GeminiCard: React.FC<GeminiCardProps> = ({ response, isGenerating }) => {
  return (
    <div className="linear-card-elevated p-4 flex flex-col gap-3 flex-1 min-h-[140px] relative overflow-hidden">
      {/* アクセントグロー */}
      <div className="absolute top-0 right-0 w-32 h-32 bg-[radial-gradient(ellipse_at_top_right,_var(--tw-gradient-stops))] from-[rgba(228,242,34,0.06)] via-transparent to-transparent pointer-events-none" />

      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wider text-[#d0d6e0]">
          <Sparkles className="w-4 h-4 text-[#e4f222]" />
          <span>Gemini AI Response</span>
        </div>
        {isGenerating && (
          <span className="text-[11px] font-mono text-[#e4f222] animate-pulse">Generating...</span>
        )}
      </div>

      <div className="flex-1 overflow-y-auto pr-1">
        {response ? (
          <p className="text-sm text-[#e5e5e6] leading-relaxed whitespace-pre-wrap font-sans">
            {response}
          </p>
        ) : (
          <div className="h-full flex items-center justify-center text-xs text-[#62666d] italic gap-2 py-4">
            <MessageSquare className="w-4 h-4" />
            <span>AI の応答待機中 (発話または Discord 音声を検知するとここに回答が表示されます)</span>
          </div>
        )}
      </div>
    </div>
  );
};
