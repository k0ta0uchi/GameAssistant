import React from 'react';
import { StatusBadges } from './StatusBadges';
import { GeminiCard } from './GeminiCard';
import { AsrCard, AsrData } from './AsrCard';
import { SystemStatus, AsrEntry } from '../../types';

interface MainDashboardProps {
  status: SystemStatus;
  geminiResponse: string;
  currentAsr: AsrData | string;
  asrHistory: AsrEntry[] | string[];
  commentaryProgress: number;
  commentaryRemaining: number;
}

export const MainDashboard: React.FC<MainDashboardProps> = ({
  status,
  geminiResponse,
  currentAsr,
  asrHistory,
  commentaryProgress,
  commentaryRemaining,
}) => {
  return (
    <div className="flex-1 flex flex-col p-4 gap-3 overflow-hidden bg-[#08090a]">
      {/* 上部ヘッダー & ステータスインジケーター */}
      <div className="flex items-center justify-between pb-2 border-b border-[#23252a] shrink-0">
        <div className="text-xs font-semibold uppercase tracking-wider text-[#8a8f98]">
          Live Interaction & Diagnostics
        </div>
        <StatusBadges status={status} />
      </div>

      {/* メインエリア (Gemini AI 回答 & ASR 文字起こし) */}
      <div className="flex-1 flex flex-col gap-3 min-h-0">
        <GeminiCard response={geminiResponse} isGenerating={status.gemini} />
        <AsrCard
          currentAsr={currentAsr}
          asrHistory={asrHistory}
          commentaryProgress={commentaryProgress}
          commentaryRemaining={commentaryRemaining}
        />
      </div>
    </div>
  );
};
