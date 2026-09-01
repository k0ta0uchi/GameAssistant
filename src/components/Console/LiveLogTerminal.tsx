import React, { useState, useRef, useEffect, useMemo } from 'react';
import {
  Terminal,
  Search,
  Trash2,
  ArrowDown,
  Copy,
  Check,
  ChevronUp,
  ChevronDown,
} from 'lucide-react';
import { LogEntry } from '../../types';

interface LiveLogTerminalProps {
  logs: LogEntry[];
  onClear: () => void;
}

const STORAGE_KEY_HEIGHT = 'gameassistant_console_height';
const DEFAULT_HEIGHT = 260;
const MIN_HEIGHT = 100;

export const LiveLogTerminal: React.FC<LiveLogTerminalProps> = ({ logs, onClear }) => {
  const [isExpanded, setIsExpanded] = useState<boolean>(true);
  const [searchQuery, setSearchQuery] = useState<string>('');
  const [levelFilter, setLevelFilter] = useState<string>('ALL');
  const [autoScroll, setAutoScroll] = useState<boolean>(true);
  const [copiedAll, setCopiedAll] = useState<boolean>(false);
  const [copiedIndex, setCopiedIndex] = useState<number | null>(null);

  // 高さ管理 (localStorage 永続化)
  const [height, setHeight] = useState<number>(() => {
    try {
      const saved = localStorage.getItem(STORAGE_KEY_HEIGHT);
      return saved ? Math.max(MIN_HEIGHT, parseInt(saved, 10)) : DEFAULT_HEIGHT;
    } catch {
      return DEFAULT_HEIGHT;
    }
  });

  const [isDragging, setIsDragging] = useState<boolean>(false);
  const startDragY = useRef<number>(0);
  const startHeight = useRef<number>(DEFAULT_HEIGHT);

  const scrollRef = useRef<HTMLDivElement>(null);

  // ドラッグリサイズ処理
  const handleMouseDown = (e: React.MouseEvent) => {
    if (!isExpanded) return;
    setIsDragging(true);
    startDragY.current = e.clientY;
    startHeight.current = height;
    e.preventDefault();
  };

  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      if (!isDragging) return;
      const deltaY = startDragY.current - e.clientY; // 上にドラッグで高く
      const maxH = Math.floor(window.innerHeight * 0.85);
      const newH = Math.min(maxH, Math.max(MIN_HEIGHT, startHeight.current + deltaY));
      setHeight(newH);
    };

    const handleMouseUp = () => {
      if (isDragging) {
        setIsDragging(false);
        try {
          localStorage.setItem(STORAGE_KEY_HEIGHT, height.toString());
        } catch {
          // ignore
        }
      }
    };

    if (isDragging) {
      window.addEventListener('mousemove', handleMouseMove);
      window.addEventListener('mouseup', handleMouseUp);
      document.body.style.cursor = 'ns-resize';
      document.body.style.userSelect = 'none';
    } else {
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    }

    return () => {
      window.removeEventListener('mousemove', handleMouseMove);
      window.removeEventListener('mouseup', handleMouseUp);
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    };
  }, [isDragging, height]);

  // 高さプリセット変更
  const setPresetHeight = (h: number) => {
    setIsExpanded(true);
    setHeight(h);
    try {
      localStorage.setItem(STORAGE_KEY_HEIGHT, h.toString());
    } catch {
      // ignore
    }
  };

  const filteredLogs = useMemo(() => {
    return logs.filter((log) => {
      if (levelFilter !== 'ALL' && log.level !== levelFilter) return false;
      if (searchQuery.trim() !== '') {
        const q = searchQuery.toLowerCase();
        const msg = (log.message || '').toLowerCase();
        const logger = (log.logger || '').toLowerCase();
        return msg.includes(q) || logger.includes(q);
      }
      return true;
    });
  }, [logs, levelFilter, searchQuery]);

  useEffect(() => {
    if (autoScroll && scrollRef.current && isExpanded) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [filteredLogs, autoScroll, isExpanded]);

  // 全ログ / フィルタログのコピー
  const handleCopyAllLogs = () => {
    const text = filteredLogs
      .map((l) => `[${l.timestamp}] [${l.level}] [${l.logger}] ${l.message}`)
      .join('\n');
    navigator.clipboard.writeText(text);
    setCopiedAll(true);
    setTimeout(() => setCopiedAll(false), 2000);
  };

  // 単一行のコピー
  const handleCopyLine = (log: LogEntry, idx: number, e: React.MouseEvent) => {
    e.stopPropagation();
    const lineText = `[${log.timestamp}] [${log.level}] [${log.logger}] ${log.message}`;
    navigator.clipboard.writeText(lineText);
    setCopiedIndex(idx);
    setTimeout(() => setCopiedIndex(null), 1800);
  };

  const getLevelBadgeClass = (level: string) => {
    switch (level) {
      case 'ERROR':
      case 'CRITICAL':
        return 'text-[#eb5757] bg-[rgba(235,87,87,0.15)] border-[#eb5757]';
      case 'WARNING':
        return 'text-[#e4f222] bg-[rgba(228,242,34,0.15)] border-[#e4f222]';
      case 'INFO':
        return 'text-[#02b8cc] bg-[rgba(2,184,204,0.15)] border-[#02b8cc]';
      case 'DEBUG':
      default:
        return 'text-[#8a8f98] bg-[rgba(138,143,152,0.1)] border-[#383b3f]';
    }
  };

  return (
    <div
      style={{ height: isExpanded ? `${height}px` : '36px' }}
      className="relative border-t border-[#23252a] bg-[#08090a] flex flex-col transition-[height] duration-75 shrink-0 select-none"
    >
      {/* 🖱️ ドラッグリサイズハンドル (上部ボーダー) */}
      {isExpanded && (
        <div
          onMouseDown={handleMouseDown}
          onDoubleClick={() => setPresetHeight(DEFAULT_HEIGHT)}
          title="ドラッグで高さを調整（ダブルクリックでリセット）"
          className="absolute -top-1 left-0 right-0 h-2.5 z-20 cursor-ns-resize group flex items-center justify-center hover:bg-[#e4f222]/20 transition-colors"
        >
          <div className="w-12 h-1 rounded-full bg-[#383b3f] group-hover:bg-[#e4f222] transition-colors" />
        </div>
      )}

      {/* ターミナルヘッダー */}
      <div className="h-9 px-4 flex items-center justify-between bg-[#0f1011] border-b border-[#23252a] select-none shrink-0">
        <div className="flex items-center gap-3">
          <button
            onClick={() => setIsExpanded(!isExpanded)}
            className="flex items-center gap-1.5 text-xs font-mono font-semibold text-[#d0d6e0] hover:text-white"
          >
            <Terminal className="w-3.5 h-3.5 text-[#e4f222]" />
            <span>CONSOLE LOGS ({logs.length})</span>
            {isExpanded ? (
              <ChevronDown className="w-3.5 h-3.5 text-[#8a8f98]" />
            ) : (
              <ChevronUp className="w-3.5 h-3.5 text-[#8a8f98]" />
            )}
          </button>

          {isExpanded && (
            <div className="flex items-center gap-1 text-[11px] font-mono">
              {['ALL', 'INFO', 'WARNING', 'ERROR', 'DEBUG'].map((lvl) => (
                <button
                  key={lvl}
                  onClick={() => setLevelFilter(lvl)}
                  className={`px-2 py-0.5 rounded-[4px] border transition-colors ${
                    levelFilter === lvl
                      ? 'bg-[#23252a] text-[#e4f222] border-[#383b3f]'
                      : 'text-[#8a8f98] border-transparent hover:text-[#d0d6e0]'
                  }`}
                >
                  {lvl}
                </button>
              ))}
            </div>
          )}
        </div>

        {isExpanded && (
          <div className="flex items-center gap-2">
            {/* 高さクイックプリセットボタン */}
            <div className="hidden sm:flex items-center gap-0.5 mr-1 font-mono text-[10px] text-[#8a8f98] bg-[#08090a] px-1 py-0.5 rounded border border-[#23252a]">
              <button
                onClick={() => setPresetHeight(160)}
                className={`px-1.5 py-0.5 rounded hover:text-white ${
                  height === 160 ? 'text-[#e4f222] font-bold bg-[#23252a]' : ''
                }`}
                title="高さ: 小 (160px)"
              >
                S
              </button>
              <button
                onClick={() => setPresetHeight(260)}
                className={`px-1.5 py-0.5 rounded hover:text-white ${
                  height === 260 ? 'text-[#e4f222] font-bold bg-[#23252a]' : ''
                }`}
                title="高さ: 中 (260px)"
              >
                M
              </button>
              <button
                onClick={() => setPresetHeight(460)}
                className={`px-1.5 py-0.5 rounded hover:text-white ${
                  height === 460 ? 'text-[#e4f222] font-bold bg-[#23252a]' : ''
                }`}
                title="高さ: 大 (460px)"
              >
                L
              </button>
            </div>

            {/* 検索ボックス */}
            <div className="relative flex items-center">
              <Search className="w-3 h-3 text-[#8a8f98] absolute left-2 pointer-events-none" />
              <input
                type="text"
                placeholder="Filter logs..."
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                className="text-[11px] font-mono bg-[#08090a] border border-[#23252a] rounded-[4px] pl-6 pr-2 py-0.5 text-[#d0d6e0] focus:outline-none focus:border-[#383b3f] w-36"
              />
            </div>

            {/* 自動スクロール */}
            <button
              onClick={() => setAutoScroll(!autoScroll)}
              className={`p-1 rounded transition-colors ${
                autoScroll ? 'text-[#e4f222] bg-[#23252a]' : 'text-[#8a8f98] hover:text-[#d0d6e0]'
              }`}
              title="自動スクロール切替"
            >
              <ArrowDown className="w-3.5 h-3.5" />
            </button>

            {/* 全ログコピー */}
            <button
              onClick={handleCopyAllLogs}
              className="flex items-center gap-1 px-2 py-0.5 text-[11px] font-mono text-[#8a8f98] hover:text-[#d0d6e0] hover:bg-[#23252a] rounded transition-colors"
              title="フィルタ中の全ログをクリップボードにコピー"
            >
              {copiedAll ? (
                <>
                  <Check className="w-3 h-3 text-[#27a644]" />
                  <span className="text-[#27a644]">Copied</span>
                </>
              ) : (
                <>
                  <Copy className="w-3 h-3" />
                  <span>Copy All</span>
                </>
              )}
            </button>

            {/* クリア */}
            <button
              onClick={onClear}
              className="p-1 text-[#8a8f98] hover:text-[#eb5757] hover:bg-[#23252a] rounded transition-colors"
              title="ログを消去"
            >
              <Trash2 className="w-3.5 h-3.5" />
            </button>
          </div>
        )}
      </div>

      {/* 📋 ログストリーム表示エリア (テキスト選択 select-text 解禁) */}
      {isExpanded && (
        <div
          ref={scrollRef}
          className="flex-1 p-3 overflow-y-auto font-mono text-[11px] leading-relaxed flex flex-col gap-0.5 bg-[#08090a] select-text cursor-text"
        >
          {filteredLogs.length === 0 ? (
            <div className="text-[#62666d] italic py-2 select-none">ログはありません</div>
          ) : (
            filteredLogs.map((log, idx) => (
              <div
                key={idx}
                className="group relative flex items-start gap-2 hover:bg-[#121417] px-1.5 py-0.5 rounded transition-colors"
              >
                <span className="text-[#62666d] shrink-0 font-mono">{log.timestamp}</span>
                <span
                  className={`px-1 rounded text-[10px] border shrink-0 font-mono ${getLevelBadgeClass(
                    log.level
                  )}`}
                >
                  {log.level}
                </span>
                <span className="text-[#8a8f98] shrink-0 font-mono">[{log.logger}]</span>
                <span className="text-[#d0d6e0] break-all whitespace-pre-wrap flex-1 leading-normal font-mono">
                  {log.message}
                </span>

                {/* 📋 個別行コピーボタン (ホバー時のみ右端に表示) */}
                <button
                  onClick={(e) => handleCopyLine(log, idx, e)}
                  className="opacity-0 group-hover:opacity-100 p-1 text-[#8a8f98] hover:text-white bg-[#1a1c20] hover:bg-[#23252a] rounded border border-[#383b3f] transition-opacity shrink-0 ml-1"
                  title="この行をコピー"
                >
                  {copiedIndex === idx ? (
                    <Check className="w-3 h-3 text-[#27a644]" />
                  ) : (
                    <Copy className="w-3 h-3" />
                  )}
                </button>
              </div>
            ))
          )}
        </div>
      )}
    </div>
  );
};

