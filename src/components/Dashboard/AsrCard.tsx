import React, { useRef, useEffect } from 'react';
import {
  Mic,
  Clock,
  CheckCircle2,
  Radio,
  MessageSquare,
  Sparkles,
  Headphones,
  BrainCircuit,
} from 'lucide-react';
import { AsrEntry, FactEntry } from '../../types';

export interface AsrData {
  text: string;
  isFinal: boolean;
  isPrompt?: boolean;
  latencyMs?: number | null;
}

interface AsrCardProps {
  currentAsr: AsrData | string;
  asrHistory?: AsrEntry[] | string[];
  factHistory?: FactEntry[];
  commentaryProgress: number; // 0 - 100
  commentaryRemaining: number; // 秒
}

export const AsrCard: React.FC<AsrCardProps> = ({
  currentAsr,
  asrHistory = [],
  factHistory = [],
  commentaryProgress,
  commentaryRemaining,
}) => {
  const scrollRef = useRef<HTMLDivElement>(null);

  // 文字列またはオブジェクトの両方に対応
  const asrText = typeof currentAsr === 'string' ? currentAsr : currentAsr?.text || '';
  const isFinal = typeof currentAsr === 'string' ? true : currentAsr?.isFinal ?? true;
  const isPromptActive = typeof currentAsr === 'object' ? !!currentAsr?.isPrompt : false;
  const currentLatency = typeof currentAsr === 'object' ? currentAsr?.latencyMs : null;

  // 履歴更新時や発話更新時に最下部へ自動スクロール
  useEffect(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [asrHistory, asrText, factHistory]);

  return (
    <div className="linear-card p-4 flex flex-col gap-3 flex-1 min-h-[220px]">
      {/* ヘッダー */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wider text-[#8a8f98]">
          <Mic className="w-3.5 h-3.5 text-[#27a644]" />
          <span>Live Conversation &amp; Memory</span>
        </div>

        {/* 自動ツッコミタイマー */}
        <div className="flex items-center gap-2 text-[11px] font-mono text-[#8a8f98]">
          <Clock className="w-3 h-3 text-[#02b8cc]" />
          <span>Auto Commentary: {commentaryRemaining > 0 ? `${commentaryRemaining}s` : 'Idle'}</span>
        </div>
      </div>

      {/* 自動ツッコミプログレスバー */}
      <div className="w-full bg-[#161718] h-1 rounded-full overflow-hidden border border-[#23252a]">
        <div
          className="h-full bg-[#02b8cc] transition-all duration-200"
          style={{ width: `${Math.min(100, Math.max(0, commentaryProgress))}%` }}
        />
      </div>

      {/* 文字起こしメイン領域 (上部に確定履歴、下部に現在のアクティブ発話) */}
      <div
        ref={scrollRef}
        className="flex-1 overflow-y-auto pr-1 flex flex-col gap-2 font-sans text-xs scroll-smooth"
      >
        {asrHistory.length === 0 && factHistory.length === 0 && !asrText && (
          <div className="h-full flex flex-col items-center justify-center text-[#525866] italic py-8 gap-2">
            <MessageSquare className="w-5 h-5 opacity-40" />
            <span>マイクまたは Discord 音声の文字起こしがここにリアルタイム表示されます</span>
          </div>
        )}

        {/* 1. 過去の確定ログ一覧（上方向に残る） */}
        {asrHistory.map((item, idx) => {
          const isObj = typeof item === 'object' && item !== null;
          const text = isObj ? (item as AsrEntry).text : (item as string);
          const time = isObj ? (item as AsrEntry).timestamp : '';
          const key = isObj ? (item as AsrEntry).id : `asr_${idx}`;
          const isDiscord = isObj ? (item as AsrEntry).isDiscord : text.startsWith('[Discord]');
          const isPrompt = isObj ? (item as AsrEntry).isPrompt : false;
          const latencyMs = isObj ? (item as AsrEntry).latencyMs : null;

          return (
            <div
              key={key}
              className={`flex items-start gap-2.5 px-2.5 py-1.5 rounded-[6px] transition-colors ${
                isPrompt
                  ? 'bg-[#1e1528]/80 text-[#f5f3ff] border border-[#a855f7]/40 shadow-sm'
                  : 'text-[#b0b8c4] bg-[#0c0d0e]/60 border border-[#1a1c20] hover:border-[#2a2e36]'
              }`}
            >
              {/* 左側バッジ領域 */}
              <div className="flex items-center gap-1.5 shrink-0 mt-0.5 select-none">
                {isPrompt ? (
                  <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium font-mono bg-[#3b1754] text-[#c084fc] border border-[#a855f7]/50 shadow-sm animate-pulse">
                    <Sparkles className="w-2.5 h-2.5 text-[#e879f9]" />
                    プロンプト
                  </span>
                ) : isDiscord ? (
                  <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[9px] font-mono bg-[#0c2838] text-[#38bdf8] border border-[#0284c7]/30">
                    <Headphones className="w-2.5 h-2.5" />
                    Discord
                  </span>
                ) : (
                  <span className="inline-flex items-center gap-1 px-1 py-0.5 rounded text-[9px] font-mono bg-[#162d1e] text-[#4ade80] border border-[#22c55e]/20">
                    <CheckCircle2 className="w-2.5 h-2.5" />
                    マイク
                  </span>
                )}
                {time && <span className="text-[10px] font-mono text-[#525866]">{time}</span>}
                {typeof latencyMs === 'number' && Number.isFinite(latencyMs) && (
                  <span className="text-[10px] font-mono text-[#525866]">{Math.round(latencyMs)}ms</span>
                )}
              </div>
              <span className={`leading-relaxed break-words ${isPrompt ? 'font-medium text-[#faf5ff]' : ''}`}>
                {text}
              </span>
            </div>
          );
        })}

        {/* 2. Gemmaが会話から抽出して永続化したFact */}
        {factHistory.length > 0 && (
          <section
            className="mt-1 pt-2 border-t border-[#252a2f]"
            aria-labelledby="live-facts-heading"
          >
            <div
              id="live-facts-heading"
              className="flex items-center gap-1.5 px-1 pb-1 text-[10px] font-semibold uppercase tracking-wider text-[#a78bfa]"
            >
              <BrainCircuit className="w-3 h-3" aria-hidden="true" />
              <span>Auto-extracted FACT</span>
              <span className="font-mono text-[#6d5db3]">{factHistory.length}</span>
            </div>
            <ul className="flex flex-col gap-1.5" aria-live="polite">
              {factHistory.map((fact) => (
                <li
                  key={fact.id}
                  className="flex items-start gap-2.5 px-2.5 py-2 rounded-[7px] bg-[#171326]/90 border border-[#7c5cc6]/45 shadow-[0_0_14px_rgba(124,92,198,0.08)]"
                >
                  <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-semibold font-mono bg-[#2a1b4a] text-[#c4b5fd] border border-[#8b5cf6]/55 shrink-0 mt-0.5">
                    <Sparkles className="w-2.5 h-2.5 text-[#e879f9]" aria-hidden="true" />
                    FACT
                  </span>
                  <div className="min-w-0 flex-1">
                    <div className="text-xs leading-relaxed text-[#f0eaff] break-words">
                      {fact.text}
                    </div>
                    {(fact.timestamp || fact.source) && (
                      <div className="mt-1 flex items-center gap-2 text-[10px] font-mono text-[#8277a3]">
                        {fact.timestamp && <span>{fact.timestamp}</span>}
                        {fact.source && <span>{fact.source}</span>}
                      </div>
                    )}
                  </div>
                </li>
              ))}
            </ul>
          </section>
        )}

        {/* 3. 最下部: 現在のアクティブ発話行（リアルタイムインプレース色分け） */}
        {asrText && (
          <div
            className={`sticky bottom-0 mt-1 flex items-start gap-2.5 px-3 py-2 rounded-[8px] border shadow-lg backdrop-blur ${
              isPromptActive
                ? 'bg-[#181024] border-[#a855f7]/60'
                : 'bg-[#101214] border-[#2a2e37]'
            }`}
          >
            {/* ステータスバッジ色分け */}
            {isPromptActive ? (
              <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium font-mono bg-[#3b1754] text-[#c084fc] border border-[#a855f7]/60 shrink-0 mt-0.5 animate-pulse">
                <Sparkles className="w-2.5 h-2.5 text-[#e879f9]" />
                プロンプト
              </span>
            ) : isFinal ? (
              <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium font-mono bg-[#162d1e] text-[#4ade80] border border-[#22c55e]/20 shrink-0 mt-0.5">
                <CheckCircle2 className="w-2.5 h-2.5" />
                確定
              </span>
            ) : (
              <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium font-mono bg-[#2e260e] text-[#e4f222] border border-[#e4f222]/30 shrink-0 mt-0.5 animate-pulse">
                <Radio className="w-2.5 h-2.5" />
                認識中
              </span>
            )}

            {/* 文字列色分け */}
            <div
              className={`text-xs leading-relaxed transition-colors duration-150 break-words flex-1 ${
                isPromptActive
                  ? 'text-[#f5f3ff] font-medium'
                  : isFinal
                  ? 'text-[#f3f4f6] font-normal'
                  : 'text-[#e4f222] font-medium'
              }`}
            >
              <span>{asrText}</span>
              {isFinal && typeof currentLatency === 'number' && Number.isFinite(currentLatency) && (
                <span className="ml-2 shrink-0 text-[10px] font-mono text-[#62666d]">
                  {Math.round(currentLatency)}ms
                </span>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
};
