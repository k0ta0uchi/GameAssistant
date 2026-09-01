import React, { useState, useEffect } from 'react';
import { PromptItem } from '../../types';
import {
  Bot,
  Sparkles,
  BookOpen,
  Brain,
  FileText,
  Volume2,
  RotateCcw,
  Save,
  Copy,
  Check,
  AlertCircle,
  Code2,
} from 'lucide-react';

interface PromptsTabProps {
  prompts: PromptItem[];
  onSavePrompt: (id: string, value: string) => Promise<boolean>;
  onResetPrompt: (id: string) => Promise<boolean>;
}

export const PromptsTab: React.FC<PromptsTabProps> = ({
  prompts,
  onSavePrompt,
  onResetPrompt,
}) => {
  const [selectedPromptId, setSelectedPromptId] = useState<string>(
    prompts.length > 0 ? prompts[0].id : 'system_instruction_character'
  );
  const [draftValues, setDraftValues] = useState<Record<string, string>>({});
  const [isSaving, setIsSaving] = useState<boolean>(false);
  const [saveSuccess, setSaveSuccess] = useState<boolean>(false);
  const [copied, setCopied] = useState<boolean>(false);

  // 初期ロード・更新時に draftValues を初期化
  useEffect(() => {
    const drafts: Record<string, string> = {};
    prompts.forEach((p) => {
      drafts[p.id] = p.value;
    });
    setDraftValues(drafts);
    if (!selectedPromptId && prompts.length > 0) {
      setSelectedPromptId(prompts[0].id);
    }
  }, [prompts]);

  const currentPrompt = prompts.find((p) => p.id === selectedPromptId) || prompts[0];
  const currentValue = draftValues[selectedPromptId] ?? currentPrompt?.value ?? '';
  const isModifiedFromDefault = currentValue.trim() !== (currentPrompt?.default ?? '').trim();
  const hasUnsavedChanges = currentValue !== (currentPrompt?.value ?? '');

  const getPromptIcon = (iconName: string) => {
    switch (iconName) {
      case 'Bot':
        return <Bot size={16} className="text-lime-400" />;
      case 'Sparkles':
        return <Sparkles size={16} className="text-teal-400" />;
      case 'BookOpen':
        return <BookOpen size={16} className="text-purple-400" />;
      case 'Brain':
        return <Brain size={16} className="text-pink-400" />;
      case 'FileText':
        return <FileText size={16} className="text-blue-400" />;
      case 'Volume2':
        return <Volume2 size={16} className="text-amber-400" />;
      default:
        return <Code2 size={16} className="text-zinc-400" />;
    }
  };

  const handleValueChange = (newVal: string) => {
    setDraftValues((prev) => ({
      ...prev,
      [selectedPromptId]: newVal,
    }));
    setSaveSuccess(false);
  };

  const handleSave = async () => {
    if (!currentPrompt) return;
    setIsSaving(true);
    const success = await onSavePrompt(currentPrompt.id, currentValue);
    setIsSaving(false);
    if (success) {
      setSaveSuccess(true);
      setTimeout(() => setSaveSuccess(false), 2500);
    }
  };

  const handleResetToDefault = async () => {
    if (!currentPrompt) return;
    if (window.confirm(`「${currentPrompt.title}」を初期デフォルトのプロンプトに戻しますか？`)) {
      setIsSaving(true);
      await onResetPrompt(currentPrompt.id);
      setDraftValues((prev) => ({
        ...prev,
        [currentPrompt.id]: currentPrompt.default,
      }));
      setIsSaving(false);
      setSaveSuccess(true);
      setTimeout(() => setSaveSuccess(false), 2000);
    }
  };

  const handleCopy = () => {
    navigator.clipboard.writeText(currentValue);
    setCopied(true);
    setTimeout(() => setCopied(false), 1800);
  };

  if (!prompts || prompts.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center p-12 text-zinc-500 font-mono text-xs">
        <AlertCircle size={24} className="mb-2 text-zinc-600" />
        プロンプト設定をロード中...
      </div>
    );
  }

  const lineCount = (currentValue.match(/\n/g) || []).length + 1;
  const charCount = currentValue.length;

  return (
    <div className="flex flex-col lg:flex-row h-[560px] gap-4">
      {/* 左ペイン: プロンプト選択リスト */}
      <div className="w-full lg:w-64 flex flex-col gap-1.5 overflow-y-auto pr-1 border-r border-zinc-800/80">
        <div className="px-2 py-1 text-[10px] font-mono tracking-wider text-zinc-500 uppercase">
          System Prompts ({prompts.length})
        </div>
        {prompts.map((p) => {
          const isSelected = p.id === selectedPromptId;
          const draftVal = draftValues[p.id] ?? p.value;
          const isDraftUnsaved = draftVal !== p.value;
          const isCustom = draftVal.trim() !== p.default.trim();

          return (
            <button
              key={p.id}
              onClick={() => {
                setSelectedPromptId(p.id);
                setSaveSuccess(false);
              }}
              className={`flex flex-col text-left p-2.5 rounded-lg transition-all duration-150 relative group ${
                isSelected
                  ? 'bg-zinc-800/90 border border-lime-400/40 shadow-sm'
                  : 'bg-zinc-900/40 hover:bg-zinc-800/50 border border-transparent'
              }`}
            >
              <div className="flex items-center justify-between w-full mb-1">
                <div className="flex items-center gap-2">
                  {getPromptIcon(p.icon)}
                  <span
                    className={`text-xs font-medium tracking-tight truncate max-w-[140px] ${
                      isSelected ? 'text-zinc-100' : 'text-zinc-300'
                    }`}
                  >
                    {p.title.split('(')[0].trim()}
                  </span>
                </div>
                {isDraftUnsaved && (
                  <span className="w-2 h-2 rounded-full bg-amber-400 animate-pulse" title="未保存の変更があります" />
                )}
              </div>

              <div className="flex items-center gap-1.5 mt-0.5">
                <span className="text-[9px] font-mono px-1.5 py-0.5 rounded bg-zinc-800/90 text-zinc-400 border border-zinc-700/50">
                  {p.category}
                </span>
                {isCustom ? (
                  <span className="text-[9px] font-mono px-1.5 py-0.5 rounded bg-lime-950/60 text-lime-400 border border-lime-800/40">
                    Customized
                  </span>
                ) : (
                  <span className="text-[9px] font-mono px-1.5 py-0.5 rounded bg-zinc-900 text-zinc-500">
                    Default
                  </span>
                )}
              </div>
            </button>
          );
        })}
      </div>

      {/* 右ペイン: リッチプロンプトエディタ */}
      {currentPrompt && (
        <div className="flex-1 flex flex-col min-w-0 bg-zinc-950/60 rounded-xl border border-zinc-800/80 p-4 relative">
          {/* ヘッダー */}
          <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-2 pb-3 mb-3 border-b border-zinc-800/80">
            <div className="min-w-0">
              <div className="flex items-center gap-2">
                {getPromptIcon(currentPrompt.icon)}
                <h3 className="text-sm font-semibold text-zinc-100 truncate">{currentPrompt.title}</h3>
                {hasUnsavedChanges && (
                  <span className="text-[10px] font-mono px-2 py-0.5 rounded-full bg-amber-950/80 text-amber-300 border border-amber-800/50 flex items-center gap-1">
                    <span className="w-1.5 h-1.5 rounded-full bg-amber-400 animate-ping" />
                    Unsaved
                  </span>
                )}
              </div>
              <p className="text-[11px] text-zinc-400 mt-1 line-clamp-1">{currentPrompt.description}</p>
            </div>

            {/* ツールバーボタン */}
            <div className="flex items-center gap-1.5 shrink-0">
              <button
                onClick={handleCopy}
                className="flex items-center gap-1 px-2.5 py-1.5 rounded-md bg-zinc-800/80 hover:bg-zinc-700/80 text-zinc-300 text-xs font-mono border border-zinc-700/50 transition-colors"
                title="プロンプトをクリップボードにコピー"
              >
                {copied ? <Check size={13} className="text-lime-400" /> : <Copy size={13} />}
                <span>{copied ? 'Copied' : 'Copy'}</span>
              </button>

              <button
                onClick={handleResetToDefault}
                disabled={!isModifiedFromDefault || isSaving}
                className={`flex items-center gap-1 px-2.5 py-1.5 rounded-md text-xs font-mono border transition-all ${
                  isModifiedFromDefault
                    ? 'bg-zinc-800/80 hover:bg-red-950/50 hover:text-red-400 hover:border-red-800/50 text-zinc-300 border-zinc-700/50 cursor-pointer'
                    : 'bg-zinc-900/40 text-zinc-600 border-transparent cursor-not-allowed opacity-50'
                }`}
                title="初期デフォルトのプロンプトに戻す"
              >
                <RotateCcw size={13} />
                <span>Reset</span>
              </button>

              <button
                onClick={handleSave}
                disabled={isSaving || !hasUnsavedChanges}
                className={`flex items-center gap-1.5 px-3.5 py-1.5 rounded-md text-xs font-mono font-medium transition-all ${
                  saveSuccess
                    ? 'bg-emerald-600 text-white border border-emerald-500'
                    : hasUnsavedChanges
                    ? 'bg-lime-400 hover:bg-lime-300 text-black shadow-md shadow-lime-950/50 cursor-pointer animate-pulse'
                    : 'bg-zinc-800/60 text-zinc-500 border border-zinc-700/40 cursor-not-allowed'
                }`}
              >
                {saveSuccess ? (
                  <>
                    <Check size={14} className="stroke-[2.5]" />
                    <span>Saved!</span>
                  </>
                ) : (
                  <>
                    <Save size={14} />
                    <span>{isSaving ? 'Saving...' : 'Save Prompt'}</span>
                  </>
                )}
              </button>
            </div>
          </div>

          {/* 特殊プレースホルダーの警告（Memory Fact Extraction 用） */}
          {currentPrompt.id === 'memory_summarize_prompt' && !currentValue.includes('{text}') && (
            <div className="flex items-center gap-2 mb-2 p-2 rounded-lg bg-amber-950/40 border border-amber-800/50 text-amber-300 text-xs font-mono">
              <AlertCircle size={14} className="shrink-0 text-amber-400" />
              <span>ヒント: このプロンプトには発話文が代入されるプレースホルダー <code>{'{text}'}</code> を含めてください。</span>
            </div>
          )}

          {/* テキストエディタ */}
          <div className="flex-1 flex flex-col relative min-h-0">
            <textarea
              value={currentValue}
              onChange={(e) => handleValueChange(e.target.value)}
              placeholder="システムプロンプトを入力..."
              className="w-full flex-1 p-3.5 bg-zinc-900/80 border border-zinc-800 rounded-lg text-zinc-100 font-mono text-[11.5px] leading-relaxed resize-none focus:outline-none focus:border-lime-400/50 focus:ring-1 focus:ring-lime-400/20 transition-all selection:bg-lime-400/30 selection:text-white"
              spellCheck={false}
            />
          </div>

          {/* フッター情報 */}
          <div className="flex items-center justify-between pt-2.5 mt-2 border-t border-zinc-800/80 text-[10.5px] font-mono text-zinc-500">
            <div className="flex items-center gap-3">
              <span>{lineCount} lines</span>
              <span>•</span>
              <span>{charCount} characters</span>
            </div>
            <div className="flex items-center gap-2">
              <span className="text-zinc-500">
                {isModifiedFromDefault ? '✏️ Custom configuration active' : '🔒 Using default instruction'}
              </span>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};
