import React from 'react';
import { Mic, Brain, Volume2, Radio } from 'lucide-react';
import { SystemStatus } from '../../types';

interface StatusBadgesProps {
  status: SystemStatus;
}

export const StatusBadges: React.FC<StatusBadgesProps> = ({ status }) => {
  return (
    <div className="flex items-center gap-2 flex-wrap select-none">
      {/* MIC (ASR: 音声入力検知で点滅) */}
      <div
        className={`flex items-center gap-1.5 px-2.5 py-1 rounded-[5px] border text-xs font-mono font-semibold transition-all duration-200 ${
          status.asr
            ? 'bg-[#27a644]/20 border-[#27a644] text-[#27a644] shadow-[0_0_12px_rgba(39,166,68,0.45)] ring-1 ring-[#27a644]/50'
            : 'bg-[#0f1011] border-[#23252a] text-[#8a8f98]'
        }`}
      >
        <Mic className={`w-3.5 h-3.5 transition-transform ${status.asr ? 'animate-pulse scale-110 text-[#27a644]' : 'text-[#8a8f98]'}`} />
        <span>MIC</span>
      </div>

      {/* GEMINI (AI思考・レスポンス生成中に点灯) */}
      <div
        className={`flex items-center gap-1.5 px-2.5 py-1 rounded-[5px] border text-xs font-mono font-semibold transition-all duration-200 ${
          status.gemini
            ? 'bg-[#e4f222]/20 border-[#e4f222] text-[#e4f222] shadow-[0_0_12px_rgba(228,242,34,0.45)] ring-1 ring-[#e4f222]/50 animate-pulse'
            : 'bg-[#0f1011] border-[#23252a] text-[#8a8f98]'
        }`}
      >
        <Brain className={`w-3.5 h-3.5 ${status.gemini ? 'animate-spin text-[#e4f222]' : 'text-[#8a8f98]'}`} />
        <span>GEMINI</span>
      </div>

      {/* VOICE (VOICEVOX 音声読み上げ中に点灯) */}
      <div
        className={`flex items-center gap-1.5 px-2.5 py-1 rounded-[5px] border text-xs font-mono font-semibold transition-all duration-200 ${
          status.tts
            ? 'bg-[#02b8cc]/20 border-[#02b8cc] text-[#02b8cc] shadow-[0_0_12px_rgba(2,184,204,0.45)] ring-1 ring-[#02b8cc]/50 animate-pulse'
            : 'bg-[#0f1011] border-[#23252a] text-[#8a8f98]'
        }`}
      >
        <Volume2 className={`w-3.5 h-3.5 ${status.tts ? 'animate-bounce text-[#02b8cc]' : 'text-[#8a8f98]'}`} />
        <span>VOICE</span>
      </div>

      {/* TWITCH (ログイン・接続完了で点灯) */}
      <div
        className={`flex items-center gap-1.5 px-2.5 py-1 rounded-[5px] border text-xs font-mono font-semibold transition-all duration-200 ${
          status.twitch
            ? 'bg-[#8b5cf6]/20 border-[#8b5cf6] text-[#a78bfa] shadow-[0_0_12px_rgba(139,92,246,0.4)] ring-1 ring-[#8b5cf6]/50'
            : 'bg-[#0f1011] border-[#23252a] text-[#8a8f98]'
        }`}
      >
        <Radio className={`w-3.5 h-3.5 ${status.twitch ? 'text-[#a78bfa]' : 'text-[#8a8f98]'}`} />
        <span>TWITCH</span>
        {status.twitch && <span className="w-1.5 h-1.5 rounded-full bg-[#a78bfa] shadow-[0_0_6px_#a78bfa]" />}
      </div>
    </div>
  );
};
