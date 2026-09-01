import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
  X,
  Sliders,
  Radio,
  Cpu,
  BookOpen,
  MessageSquareCode,
  Copy,
  Check,
  Key,
  ExternalLink,
  Power,
  RefreshCw,
} from 'lucide-react';
import { SkillsResponse, PromptItem } from '../../types';
import { PromptsTab } from './PromptsTab';

interface SettingsModalProps {
  isOpen: boolean;
  onClose: () => void;
  settings: Record<string, any>;
  onUpdateSetting: (key: string, value: any) => Promise<void>;
  discordDevices: string[];
  prompts?: PromptItem[];
  onSavePrompt?: (id: string, value: string) => Promise<boolean>;
  onResetPrompt?: (id: string) => Promise<boolean>;
}

export const SettingsModal: React.FC<SettingsModalProps> = ({
  isOpen,
  onClose,
  settings,
  onUpdateSetting,
  discordDevices,
  prompts = [],
  onSavePrompt = async () => true,
  onResetPrompt = async () => true,
}) => {
  const [activeTab, setActiveTab] = useState<'engines' | 'prompts' | 'twitch' | 'preferences' | 'blog_skills'>('engines');
  const [skillsData, setSkillsData] = useState<SkillsResponse | null>(null);
  const [editingSkillId, setEditingSkillId] = useState<string | null>(null);
  const [editingSkillContent, setEditingSkillContent] = useState<string>('');
  const [isSavingSkill, setIsSavingSkill] = useState<boolean>(false);

  useEffect(() => {
    if (isOpen) {
      invoke<SkillsResponse>('list_skills')
        .then((data) => setSkillsData(data))
        .catch((err) => console.error('Failed to fetch skills:', err));
    }
  }, [isOpen]);

  if (!isOpen) return null;

  const handleToggleSkill = async (skillId: string, enabled: boolean) => {
    if (!skillsData) return;
    const currentEnabled = skillsData.enabled_skills || [];
    const nextEnabled = enabled
      ? [...currentEnabled, skillId]
      : currentEnabled.filter((id) => id !== skillId);

    setSkillsData({ ...skillsData, enabled_skills: nextEnabled });
    await onUpdateSetting('enabled_blog_skills', nextEnabled);
  };

  const handleStartEditSkill = async (skillId: string) => {
    try {
      const content = await invoke<string>('get_skill_content', { id: skillId });
      setEditingSkillId(skillId);
      setEditingSkillContent(content);
    } catch (e) {
      console.error('Failed to get skill content:', e);
    }
  };

  const handleSaveSkillContent = async () => {
    if (!editingSkillId) return;
    setIsSavingSkill(true);
    try {
      const updated = await invoke<SkillsResponse>('save_skill_content', {
        id: editingSkillId,
        content: editingSkillContent,
      });
      setSkillsData(updated);
      setEditingSkillId(null);
    } catch (e) {
      console.error('Failed to save skill content:', e);
    } finally {
      setIsSavingSkill(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-sm animate-fade-in select-none">
      <div className="w-[840px] max-h-[88vh] bg-[#0f1011] border border-[#23252a] rounded-[12px] shadow-2xl flex flex-col overflow-hidden">
        {/* ヘッダー */}
        <div className="px-5 py-3.5 border-b border-[#23252a] flex items-center justify-between bg-[#161718]">
          <div className="flex items-center gap-2">
            <Sliders className="w-4 h-4 text-[#e4f222]" />
            <h2 className="text-sm font-semibold text-white">Settings</h2>
          </div>
          <button
            onClick={onClose}
            className="p-1 text-[#8a8f98] hover:text-white hover:bg-[#23252a] rounded transition-colors"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        {/* タブバー */}
        <div className="px-5 border-b border-[#23252a] flex gap-4 bg-[#0f1011]">
          {[
            { id: 'engines', label: 'Engines', icon: Cpu },
            { id: 'prompts', label: 'System Prompts', icon: MessageSquareCode },
            { id: 'twitch', label: 'Twitch', icon: Radio },
            { id: 'preferences', label: 'Preferences', icon: Sliders },
            { id: 'blog_skills', label: 'Blog & Skills', icon: BookOpen },
          ].map((tab) => {
            const Icon = tab.icon;
            const isActive = activeTab === tab.id;
            return (
              <button
                key={tab.id}
                onClick={() => setActiveTab(tab.id as any)}
                className={`flex items-center gap-1.5 py-2.5 text-xs font-medium border-b-2 transition-all ${
                  isActive
                    ? 'border-[#e4f222] text-[#e4f222]'
                    : 'border-transparent text-[#8a8f98] hover:text-[#d0d6e0]'
                }`}
              >
                <Icon className="w-3.5 h-3.5" />
                <span>{tab.label}</span>
              </button>
            );
          })}
        </div>

        {/* コンテンツエリア */}
        <div className="flex-1 p-5 overflow-y-auto flex flex-col gap-4 text-xs">
          {/* 1. Engines タブ */}
          {activeTab === 'engines' && (
            <div className="flex flex-col gap-4">
              {/* TTS Engine */}
              <div className="flex flex-col gap-1.5">
                <label className="text-[#8a8f98] font-medium">TTS Engine (音声合成)</label>
                <select
                  value={settings.tts_engine || 'voicevox'}
                  onChange={(e) => onUpdateSetting('tts_engine', e.target.value)}
                  className="linear-input py-1.5 px-2 bg-[#0f1011] text-[#d0d6e0]"
                >
                  <option value="voicevox" className="bg-[#161718] text-[#d0d6e0]">VOICEVOX (Local)</option>
                  <option value="gemini" className="bg-[#161718] text-[#d0d6e0]">Gemini Live TTS (Cloud)</option>
                  <option value="style_bert_vits2" className="bg-[#161718] text-[#d0d6e0]">Style-Bert-VITS2 (Local Server)</option>
                </select>
              </div>

              {/* ASR Engine */}
              <div className="flex flex-col gap-1.5">
                <label className="text-[#8a8f98] font-medium">ASR Engine (Whisper Model)</label>
                <select
                  value={settings.asr_engine || 'kotoba'}
                  onChange={(e) => onUpdateSetting('asr_engine', e.target.value)}
                  className="linear-input py-1.5 px-2 bg-[#0f1011] text-[#d0d6e0]"
                >
                  <option value="kotoba" className="bg-[#161718] text-[#d0d6e0]">Kotoba-Whisper-v2.0-faster (High Precision Japanese ASR)</option>
                </select>
              </div>

              {/* Preload Whisper on Startup Option */}
              <div className="flex items-center justify-between p-2.5 rounded-[6px] bg-[#161718] border border-[#23252a]">
                <div className="flex flex-col gap-0.5">
                  <span className="text-xs text-white font-medium">起動時にWhisperモデルを事前ロード (高速ウォームアップ)</span>
                  <span className="text-[10px] text-[#8a8f98]">GUI起動直後にバックグラウンドで重みをロードし、最初の発話からゼロ遅延で認識</span>
                </div>
                <input
                  type="checkbox"
                  checked={settings.preload_whisper_on_startup ?? true}
                  onChange={(e) => onUpdateSetting('preload_whisper_on_startup', e.target.checked)}
                  className="w-4 h-4 rounded border-[#383b3f] bg-[#0f1011] text-[#e4f222] focus:ring-0 cursor-pointer"
                />
              </div>

              {/* Wake Word Engine */}
              <div className="flex flex-col gap-1.5">
                <label className="text-[#8a8f98] font-medium">Wake Word Engine</label>
                <select
                  value={settings.wake_word_engine || 'whisper_vad'}
                  onChange={(e) => onUpdateSetting('wake_word_engine', e.target.value)}
                  className="linear-input py-1.5 px-2 bg-[#0f1011] text-[#d0d6e0]"
                >
                  <option value="whisper_vad" className="bg-[#161718] text-[#d0d6e0]">Whisper VAD (無音検出)</option>
                  <option value="openwakeword" className="bg-[#161718] text-[#d0d6e0]">openWakeWord (ねえぐり - neeguri.onnx)</option>
                </select>
              </div>

              {/* Whisper VAD Wake Words 設定 (カンマ区切り編集) */}
              <div className="flex flex-col gap-1.5 bg-[#0f1011] p-3 rounded-[6px] border border-[#23252a]">
                <div className="flex items-center justify-between">
                  <label className="text-[#8a8f98] font-medium">Whisper VAD Wake Words (カンマ区切り)</label>
                  <span className="text-[10px] text-[#525866]">複数単語指定可能</span>
                </div>
                <input
                  type="text"
                  value={settings.custom_wake_words ?? "ねえぐり, ねぐり, ネグリ, ねーぐり, ねぇぐり, ね〜ぐり, neguri"}
                  onChange={(e) => onUpdateSetting('custom_wake_words', e.target.value)}
                  placeholder="例: ねえぐり, ねぐり, ネグリ, neguri, アシスタント"
                  className="linear-input py-1.5 px-2 bg-[#161718] text-[#d0d6e0] font-mono text-[11px] border border-[#2a2e37] focus:border-[#e4f222]"
                />
                <span className="text-[10px] text-[#62666d]">
                  Whisper VADモードで呼びかけとして検知する単語をカンマ（, または 、）区切りで追加・編集できます。
                </span>
              </div>

              {/* Wake Word Threshold スライダー */}
              <div className="flex flex-col gap-1.5 bg-[#0f1011] p-3 rounded-[6px] border border-[#23252a]">
                <div className="flex justify-between font-mono">
                  <span className="text-[#8a8f98]">Wake Word Detection Threshold</span>
                  <span className="text-[#e4f222] font-semibold">
                    {(settings.wake_word_threshold ?? 0.25).toFixed(2)}
                  </span>
                </div>
                <input
                  type="range"
                  min="0.05"
                  max="0.95"
                  step="0.01"
                  value={settings.wake_word_threshold ?? 0.25}
                  onChange={(e) => onUpdateSetting('wake_word_threshold', parseFloat(e.target.value))}
                  className="w-full accent-[#e4f222] cursor-pointer"
                />
                <span className="text-[10px] text-[#62666d]">
                  低い値ほど敏感に検知し、高い値ほど誤検知を防ぎます（推奨: 0.20 〜 0.35）
                </span>
              </div>

              {/* Discord Audio Device */}
              <div className="flex flex-col gap-1.5">
                <label className="text-[#8a8f98] font-medium">Discord Capture Device</label>
                <select
                  value={settings.discord_audio_device || 'Auto (Discord App / System Loopback)'}
                  onChange={(e) => onUpdateSetting('discord_audio_device', e.target.value)}
                  className="linear-input py-1.5 px-2 bg-[#0f1011] text-[#d0d6e0]"
                >
                  {discordDevices.map((dev) => (
                    <option key={dev} value={dev} className="bg-[#161718] text-[#d0d6e0]">
                      {dev}
                    </option>
                  ))}
                </select>
              </div>
            </div>
          )}

          {/* 2. System Prompts タブ */}
          {activeTab === 'prompts' && (
            <PromptsTab
              prompts={prompts}
              onSavePrompt={onSavePrompt}
              onResetPrompt={onResetPrompt}
            />
          )}

          {/* 3. Twitch タブ */}
          {activeTab === 'twitch' && (
            <TwitchTab
              settings={settings}
              onUpdateSetting={onUpdateSetting}
            />
          )}

          {/* 3. Preferences タブ */}
          {activeTab === 'preferences' && (
            <div className="flex flex-col gap-3">
              {/* User Name / ID 設定 */}
              <div className="flex flex-col gap-1.5 p-3 rounded-[6px] bg-[#08090a] border border-[#23252a]">
                <label className="text-white font-medium flex items-center justify-between">
                  <span>User Name / User ID</span>
                  <span className="text-[10px] font-mono text-[#8a8f98]">LanceDB 記録・プロンプト識別名</span>
                </label>
                <input
                  type="text"
                  value={settings.user_name || 'User'}
                  onChange={(e) => onUpdateSetting('user_name', e.target.value)}
                  className="w-full linear-input py-1.5 px-2 bg-[#0f1011] text-[#d0d6e0] font-sans text-xs"
                  placeholder="e.g. User"
                />
                <span className="text-[10px] text-[#62666d]">
                  発話記憶（LanceDB）や AI 応答時に記録されるユーザー名です（デフォルト: User）
                </span>
              </div>

              <label className="flex items-center justify-between p-2.5 rounded-[6px] bg-[#08090a] border border-[#23252a] cursor-pointer">
                <div>
                  <div className="text-white font-medium">Disable Thinking Mode</div>
                  <div className="text-[11px] text-[#62666d]">
                    Gemini の思考プロセスをスキップし、超高速に応答を生成します
                  </div>
                </div>
                <input
                  type="checkbox"
                  checked={Boolean(settings.disable_thinking_mode)}
                  onChange={(e) => onUpdateSetting('disable_thinking_mode', e.target.checked)}
                  className="w-4 h-4 rounded accent-[#e4f222]"
                />
              </label>

              <label className="flex items-center justify-between p-2.5 rounded-[6px] bg-[#08090a] border border-[#23252a] cursor-pointer">
                <div>
                  <div className="text-white font-medium">Preallocate VRAM</div>
                  <div className="text-[11px] text-[#62666d]">
                    PyTorch の VRAM を事前割り当てしてメモリ断片化を抑制します
                  </div>
                </div>
                <input
                  type="checkbox"
                  checked={Boolean(settings.preallocate_vram)}
                  onChange={(e) => onUpdateSetting('preallocate_vram', e.target.checked)}
                  className="w-4 h-4 rounded accent-[#e4f222]"
                />
              </label>

              <label className="flex items-center justify-between p-2.5 rounded-[6px] bg-[#08090a] border border-[#23252a] cursor-pointer">
                <div>
                  <div className="text-white font-medium">Auto-Restart Slow Whisper (遅延自動検知＆再起動)</div>
                  <div className="text-[11px] text-[#62666d]">
                    VRAM 蓄積による Whisper の推論遅延（&gt; 2.5秒）を検知した際、GPU ワーカーを自動再起動して VRAM と速度を回復します（手動/自動切り替え可能）
                  </div>
                </div>
                <input
                  type="checkbox"
                  checked={settings.auto_restart_whisper !== false}
                  onChange={(e) => onUpdateSetting('auto_restart_whisper', e.target.checked)}
                  className="w-4 h-4 rounded accent-[#e4f222]"
                />
              </label>

              {/* Auto Commentary 設定グループ */}
              <div className="flex flex-col gap-2.5 p-3 rounded-[6px] bg-[#08090a] border border-[#23252a]">
                <label className="flex items-center justify-between cursor-pointer">
                  <div>
                    <div className="text-white font-medium">Enable Auto Commentary (自立型実況・ツッコミ)</div>
                    <div className="text-[11px] text-[#62666d]">
                      沈黙時間が続いた際に AI がゲーム画面を見て自動で配信ツッコミ・コメントを入れます
                    </div>
                  </div>
                  <input
                    type="checkbox"
                    checked={Boolean(settings.enable_auto_commentary)}
                    onChange={(e) => onUpdateSetting('enable_auto_commentary', e.target.checked)}
                    className="w-4 h-4 rounded accent-[#e4f222]"
                  />
                </label>

                {Boolean(settings.enable_auto_commentary) && (
                  <div className="mt-1 pt-2.5 border-t border-[#23252a] grid grid-cols-3 gap-3">
                    <div className="flex flex-col gap-1">
                      <label className="text-[11px] text-[#8a8f98] font-medium">
                        Min Interval (最短間隔)
                      </label>
                      <div className="flex items-center gap-1.5">
                        <input
                          type="number"
                          min="10"
                          max="3600"
                          value={settings.auto_commentary_min ?? 200}
                          onChange={(e) => onUpdateSetting('auto_commentary_min', parseInt(e.target.value) || 200)}
                          className="linear-input py-1 px-2 bg-[#0f1011] text-[#d0d6e0] font-mono text-xs w-full"
                        />
                        <span className="text-[11px] text-[#62666d] shrink-0">秒</span>
                      </div>
                    </div>

                    <div className="flex flex-col gap-1">
                      <label className="text-[11px] text-[#8a8f98] font-medium">
                        Max Interval (最長間隔)
                      </label>
                      <div className="flex items-center gap-1.5">
                        <input
                          type="number"
                          min="10"
                          max="3600"
                          value={settings.auto_commentary_max ?? 400}
                          onChange={(e) => onUpdateSetting('auto_commentary_max', parseInt(e.target.value) || 400)}
                          className="linear-input py-1 px-2 bg-[#0f1011] text-[#d0d6e0] font-mono text-xs w-full"
                        />
                        <span className="text-[11px] text-[#62666d] shrink-0">秒</span>
                      </div>
                    </div>

                    <div className="flex flex-col gap-1">
                      <label className="text-[11px] text-[#8a8f98] font-medium">
                        Silence Avoid (被り回避)
                      </label>
                      <div className="flex items-center gap-1.5">
                        <input
                          type="number"
                          min="1"
                          max="60"
                          value={settings.auto_commentary_avoid_duration ?? 5}
                          onChange={(e) => onUpdateSetting('auto_commentary_avoid_duration', parseInt(e.target.value) || 5)}
                          className="linear-input py-1 px-2 bg-[#0f1011] text-[#d0d6e0] font-mono text-xs w-full"
                        />
                        <span className="text-[11px] text-[#62666d] shrink-0">秒</span>
                      </div>
                    </div>
                  </div>
                )}
              </div>
            </div>
          )}

          {/* 4. Blog & Skills タブ */}
          {activeTab === 'blog_skills' && (
            <div className="flex flex-col gap-3.5">
              {/* ブログ生成の自動実行 */}
              <label className="flex items-center justify-between p-2.5 rounded-[6px] bg-[#08090a] border border-[#23252a] cursor-pointer hover:border-[#383b3f]">
                <div>
                  <div className="text-white font-medium">Generate Blog Post on Stop (ブログ自動生成)</div>
                  <div className="text-[11px] text-[#62666d]">
                    セッション終了時にプレイログ・会話履歴からnoteブログ記事を自動生成して blogs/ ディレクトリに保存します
                  </div>
                </div>
                <input
                  type="checkbox"
                  checked={Boolean(settings.create_blog_post)}
                  onChange={(e) => onUpdateSetting('create_blog_post', e.target.checked)}
                  className="w-4 h-4 rounded accent-[#e4f222]"
                />
              </label>

              {/* ブログ生成時の Thinking モード切り替え */}
              <label className="flex items-center justify-between p-2.5 rounded-[6px] bg-[#08090a] border border-[#23252a] cursor-pointer hover:border-[#383b3f]">
                <div>
                  <div className="text-white font-medium">Blog Thinking Mode (ブログ思考モード)</div>
                  <div className="text-[11px] text-[#62666d]">
                    Gemini 2.0 Flash の Thinking（思考）機能を有効化し、より長文で論理的なプレイ日記記事を執筆します
                  </div>
                </div>
                <input
                  type="checkbox"
                  checked={settings.blog_use_thinking !== false}
                  onChange={(e) => onUpdateSetting('blog_use_thinking', e.target.checked)}
                  className="w-4 h-4 rounded accent-[#e4f222]"
                />
              </label>

              {/* スキル適用マスタースイッチ */}
              <label className="flex items-center justify-between p-2.5 rounded-[6px] bg-[#08090a] border border-[#23252a] cursor-pointer hover:border-[#383b3f]">
                <div>
                  <div className="text-white font-medium">Enable Blog Skills (執筆スキルの適用)</div>
                  <div className="text-[11px] text-[#62666d]">
                    記事生成時に以下の文体・ペルソナ・構成ガイドラインスキルをシステムプロンプトに注入します
                  </div>
                </div>
                <input
                  type="checkbox"
                  checked={settings.enable_blog_skills !== false}
                  onChange={(e) => onUpdateSetting('enable_blog_skills', e.target.checked)}
                  className="w-4 h-4 rounded accent-[#e4f222]"
                />
              </label>

              {/* 利用可能なスキル一覧 */}
              <div className="pt-2">
                <div className="flex items-center justify-between mb-2">
                  <div className="text-[#8a8f98] font-medium">Available Skills (利用可能なスキル一覧)</div>
                  <span className="text-[11px] text-[#62666d]">skills/ ディレクトリ配下を自動検出</span>
                </div>

                {skillsData?.skills && skillsData.skills.length > 0 ? (
                  <div className="flex flex-col gap-2.5">
                    {skillsData.skills.map((skill) => {
                      const isChecked = (skillsData.enabled_skills || []).includes(skill.id);
                      const isEditing = editingSkillId === skill.id;

                      return (
                        <div
                          key={skill.id}
                          className={`flex flex-col p-3 rounded-[6px] bg-[#08090a] border transition-all ${
                            isChecked ? 'border-[#e4f222]/40 ring-1 ring-[#e4f222]/20' : 'border-[#23252a]'
                          }`}
                        >
                          <div className="flex items-start justify-between gap-3">
                            <div className="flex items-start gap-2.5 flex-1">
                              <input
                                type="checkbox"
                                checked={isChecked}
                                onChange={(e) => handleToggleSkill(skill.id, e.target.checked)}
                                className="w-4 h-4 mt-0.5 rounded accent-[#e4f222] shrink-0 cursor-pointer"
                              />
                              <div className="flex-1">
                                <div className="flex items-center gap-2">
                                  <span className="text-[#d0d6e0] font-semibold text-xs">{skill.name}</span>
                                  <span className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-[#161718] border border-[#23252a] text-[#8a8f98]">
                                    {skill.id}
                                  </span>
                                </div>
                                <div className="text-[11px] text-[#8a8f98] mt-1 leading-relaxed">{skill.description}</div>
                              </div>
                            </div>

                            <button
                              onClick={() => {
                                if (isEditing) {
                                  setEditingSkillId(null);
                                } else {
                                  handleStartEditSkill(skill.id);
                                }
                              }}
                              className="px-2.5 py-1 linear-btn-ghost text-xs text-[#8a8f98] hover:text-[#d0d6e0] shrink-0"
                            >
                              {isEditing ? 'Close' : 'Edit Skill'}
                            </button>
                          </div>

                          {/* スキル編集エリア */}
                          {isEditing && (
                            <div className="mt-3 pt-3 border-t border-[#23252a] flex flex-col gap-2">
                              <label className="text-[11px] font-mono text-[#8a8f98]">
                                Edit SKILL.md ({skill.id}):
                              </label>
                              <textarea
                                value={editingSkillContent}
                                onChange={(e) => setEditingSkillContent(e.target.value)}
                                className="w-full h-56 p-2.5 bg-[#0f1011] border border-[#23252a] rounded-[6px] font-mono text-xs text-[#d0d6e0] focus:border-[#e4f222] focus:outline-none resize-y leading-relaxed"
                                placeholder="Skill content in Markdown..."
                              />
                              <div className="flex justify-end gap-2">
                                <button
                                  onClick={() => setEditingSkillId(null)}
                                  className="px-3 py-1 text-xs text-[#8a8f98] hover:text-white"
                                >
                                  Cancel
                                </button>
                                <button
                                  onClick={handleSaveSkillContent}
                                  disabled={isSavingSkill}
                                  className="px-3.5 py-1 linear-btn-primary text-xs font-semibold"
                                >
                                  {isSavingSkill ? 'Saving...' : 'Save Skill'}
                                </button>
                              </div>
                            </div>
                          )}
                        </div>
                      );
                    })}
                  </div>
                ) : (
                  <div className="p-4 rounded-[6px] bg-[#08090a] border border-[#23252a] text-center text-[#62666d] italic">
                    利用可能なスキルが見つかりません (skills/ ディレクトリを確認してください)
                  </div>
                )}
              </div>
            </div>
          )}
        </div>

        {/* フッター */}
        <div className="px-5 py-3 border-t border-[#23252a] flex justify-end bg-[#161718]">
          <button onClick={onClose} className="px-4 py-1.5 linear-btn-primary text-xs font-semibold">
            Done
          </button>
        </div>
      </div>
    </div>
  );
};

// =====================================================================
// Twitch 設定 & 認証 & 接続管理タブ
// =====================================================================
interface TwitchTabProps {
  settings: Record<string, any>;
  onUpdateSetting: (key: string, value: any) => Promise<void>;
}

const TwitchTab: React.FC<TwitchTabProps> = ({ settings, onUpdateSetting }) => {
  const [authCode, setAuthCode] = useState<string>('');
  const [copiedUrl, setCopiedUrl] = useState<boolean>(false);
  const [isRegistering, setIsRegistering] = useState<boolean>(false);
  const [isToggling, setIsToggling] = useState<boolean>(false);
  const [twitchStatus, setTwitchStatus] = useState<{
    connected: boolean;
    bot_username: string;
    bot_id: string;
    has_client_id: boolean;
    has_client_secret: boolean;
  }>({
    connected: false,
    bot_username: settings.twitch_bot_username || '',
    bot_id: settings.twitch_bot_id || '',
    has_client_id: !!settings.twitch_client_id,
    has_client_secret: !!settings.twitch_client_secret,
  });
  const [message, setMessage] = useState<{ text: string; type: 'success' | 'error' | 'info' } | null>(null);

  const fetchStatus = async () => {
    try {
      const data: any = await invoke('twitch_get_status');
      setTwitchStatus((prev) => ({
        ...prev,
        connected: !!data?.connected,
        bot_username: settings.twitch_bot_username || '',
        bot_id: settings.twitch_bot_id || '',
        has_client_id: !!settings.twitch_client_id,
        has_client_secret: !!settings.twitch_client_secret,
      }));
    } catch {
      // ignore
    }
  };

  useEffect(() => {
    fetchStatus();
    const interval = setInterval(fetchStatus, 3000);
    return () => clearInterval(interval);
  }, []);

  const handleCopyAuthUrl = async () => {
    const clientId = (settings.twitch_client_id || '').trim();
    if (!clientId) {
      setMessage({ text: 'Client ID を入力してください。', type: 'error' });
      return;
    }
    try {
      const authUrl: string = await invoke('twitch_get_auth_url', { clientId });
      if (authUrl) {
        await navigator.clipboard.writeText(authUrl);
        setCopiedUrl(true);
        setMessage({ text: '認可URLをクリップボードにコピーしました！ブラウザで開いて認可を完了してください。', type: 'success' });
        setTimeout(() => setCopiedUrl(false), 3000);
      } else {
        setMessage({ text: '認可URLの生成に失敗しました。Client ID を確認してください。', type: 'error' });
      }
    } catch (e) {
      setMessage({ text: `認可URL生成エラー: ${e}`, type: 'error' });
    }
  };

  const handleOpenAuthUrl = async () => {
    const clientId = (settings.twitch_client_id || '').trim();
    if (!clientId) {
      setMessage({ text: 'Client ID を入力してください。', type: 'error' });
      return;
    }
    try {
      const authUrl: string = await invoke('twitch_get_auth_url', { clientId });
      if (authUrl) {
        window.open(authUrl, '_blank');
        setMessage({ text: 'ブラウザで Twitch 認可ページを開きました。認可後に表示されるコードをコピーしてください。', type: 'info' });
      }
    } catch (e) {
      setMessage({ text: `認可ページオープンエラー: ${e}`, type: 'error' });
    }
  };

  const handleRegisterCode = async () => {
    if (!authCode.trim()) {
      setMessage({ text: '認証コードを入力してください。', type: 'error' });
      return;
    }
    const clientId = (settings.twitch_client_id || '').trim();
    const clientSecret = (settings.twitch_client_secret || '').trim();
    if (!clientId || !clientSecret) {
      setMessage({ text: 'Client ID と Client Secret の両方を入力してください。', type: 'error' });
      return;
    }

    setIsRegistering(true);
    setMessage(null);
    try {
      const tokenRes: any = await invoke('twitch_register_code', {
        clientId,
        clientSecret,
        code: authCode.trim(),
      });
      if (tokenRes && tokenRes.access_token) {
        // トークン検証
        const valRes: any = await invoke('twitch_validate_token', { accessToken: tokenRes.access_token });
        if (valRes && valRes.user_id) {
          await onUpdateSetting('twitch_bot_id', valRes.user_id);
          if (valRes.login) {
            await onUpdateSetting('twitch_bot_username', valRes.login);
          }
        }
        await onUpdateSetting('twitch_access_token', tokenRes.access_token);
        if (tokenRes.refresh_token) {
          await onUpdateSetting('twitch_refresh_token', tokenRes.refresh_token);
        }
        setMessage({ text: '✅ トークンの登録と検証に成功しました！', type: 'success' });
        setAuthCode('');
        await fetchStatus();
      } else {
        setMessage({ text: 'トークンの取得に失敗しました。認証コードを確認してください。', type: 'error' });
      }
    } catch (e) {
      setMessage({ text: `トークン登録エラー: ${e}`, type: 'error' });
    } finally {
      setIsRegistering(false);
    }
  };

  const handleTestConnection = async () => {
    setIsToggling(true);
    setMessage(null);
    try {
      const channel = (settings.twitch_channel || settings.user_name || 'Kota').trim();
      const botNick = (settings.twitch_bot_username || 'justinfan12345').trim();
      const oauthToken = (settings.twitch_access_token || '').trim();

      await invoke('twitch_connect', {
        settings: {
          channel,
          bot_nick: botNick,
          oauth_token: oauthToken,
        }
      });
      setMessage({
        text: `✅ Twitch チャンネル '#${channel}' への接続テストに成功しました！（※実際の配信セッション開始時に自動接続されます）`,
        type: 'success'
      });
      await fetchStatus();
    } catch (e) {
      setMessage({ text: `Twitch 接続テスト失敗: ${e}`, type: 'error' });
    } finally {
      setIsToggling(false);
    }
  };

  return (
    <div className="flex flex-col gap-4 font-sans text-xs">
      {/* 接続ステータスバー */}
      <div className="flex items-center justify-between p-3 rounded-[8px] bg-[#08090a] border border-[#23252a]">
        <div className="flex items-center gap-2.5">
          <div
            className={`w-2.5 h-2.5 rounded-full ${
              twitchStatus.connected
                ? 'bg-[#27a644] shadow-[0_0_8px_#27a644]'
                : 'bg-[#62666d]'
            }`}
          />
          <div>
            <div className="text-white font-medium flex items-center gap-2">
              <span>Twitch Bot Status:</span>
              <span
                className={`font-mono text-[11px] px-1.5 py-0.5 rounded ${
                  twitchStatus.connected
                    ? 'bg-[#27a644]/20 text-[#27a644] border border-[#27a644]/40'
                    : 'bg-[#23252a] text-[#8a8f98]'
                }`}
              >
                {twitchStatus.connected ? 'CONNECTED (ONLINE)' : 'READY (Auto-connects on Session Start)'}
              </span>
            </div>
            {settings.twitch_bot_username && (
              <div className="text-[11px] text-[#8a8f98] mt-0.5">
                Bot Username: <span className="text-[#d0d6e0] font-mono">{settings.twitch_bot_username}</span>
                {settings.twitch_bot_id && ` (ID: ${settings.twitch_bot_id})`}
              </div>
            )}
          </div>
        </div>

        {/* 接続テストボタン */}
        <button
          onClick={handleTestConnection}
          disabled={isToggling}
          className="flex items-center gap-1.5 px-3 py-1.5 rounded-[6px] font-semibold bg-[#e4f222] text-[#08090a] hover:bg-[#e4f222]/90 disabled:opacity-50 transition-all shadow-sm"
        >
          {isToggling ? <RefreshCw className="w-3.5 h-3.5 animate-spin" /> : <Power className="w-3.5 h-3.5" />}
          <span>{isToggling ? 'Testing...' : 'Test Connection'}</span>
        </button>
      </div>

      {/* メッセージ通知 */}
      {message && (
        <div
          className={`p-2.5 rounded-[6px] text-[11px] border leading-relaxed ${
            message.type === 'success'
              ? 'bg-[#27a644]/10 text-[#27a644] border-[#27a644]/30'
              : message.type === 'error'
              ? 'bg-[#eb5757]/10 text-[#eb5757] border-[#eb5757]/30'
              : 'bg-[#02b8cc]/10 text-[#02b8cc] border-[#02b8cc]/30'
          }`}
        >
          {message.text}
        </div>
      )}

      {/* 1. API 認証クレデンシャル */}
      <div className="flex flex-col gap-3 p-3.5 rounded-[8px] bg-[#08090a] border border-[#23252a]">
        <div className="text-[#d0d6e0] font-semibold flex items-center gap-1.5">
          <Key className="w-3.5 h-3.5 text-[#e4f222]" />
          <span>Twitch API Credentials</span>
        </div>

        <div className="grid grid-cols-2 gap-3">
          <div>
            <label className="block text-[#8a8f98] mb-1 font-medium">Bot Username</label>
            <input
              type="text"
              value={settings.twitch_bot_username || ''}
              onChange={(e) => onUpdateSetting('twitch_bot_username', e.target.value)}
              className="w-full linear-input py-1.5 px-2 bg-[#0f1011] text-[#d0d6e0]"
              placeholder="e.g. MyTwitchBot"
            />
          </div>

          <div>
            <label className="block text-[#8a8f98] mb-1 font-medium">
              Bot ID <span className="text-[#62666d] text-[10px]">(Token登録時に自動設定)</span>
            </label>
            <input
              type="text"
              value={settings.twitch_bot_id || ''}
              onChange={(e) => onUpdateSetting('twitch_bot_id', e.target.value)}
              className="w-full linear-input py-1.5 px-2 bg-[#0f1011] text-[#d0d6e0] font-mono"
              placeholder="e.g. 123456789"
            />
          </div>

          <div>
            <label className="block text-[#8a8f98] mb-1 font-medium">Client ID</label>
            <input
              type="password"
              value={settings.twitch_client_id || ''}
              onChange={(e) => onUpdateSetting('twitch_client_id', e.target.value)}
              className="w-full linear-input py-1.5 px-2 bg-[#0f1011] text-[#d0d6e0] font-mono"
              placeholder="Twitch Developer Console Client ID"
            />
          </div>

          <div>
            <label className="block text-[#8a8f98] mb-1 font-medium">Client Secret</label>
            <input
              type="password"
              value={settings.twitch_client_secret || ''}
              onChange={(e) => onUpdateSetting('twitch_client_secret', e.target.value)}
              className="w-full linear-input py-1.5 px-2 bg-[#0f1011] text-[#d0d6e0] font-mono"
              placeholder="Twitch Developer Console Secret"
            />
          </div>
        </div>
      </div>

      {/* 2. OAuth 認証 & トークン登録手順 */}
      <div className="flex flex-col gap-3 p-3.5 rounded-[8px] bg-[#08090a] border border-[#23252a]">
        <div className="text-[#d0d6e0] font-semibold flex items-center gap-1.5">
          <ExternalLink className="w-3.5 h-3.5 text-[#02b8cc]" />
          <span>OAuth 2.0 Token Setup</span>
        </div>

        <div className="flex flex-col gap-2.5 text-[11px] text-[#8a8f98]">
          <div className="flex items-center justify-between">
            <span>Step 1: Twitch 認可ページを開いてアカウントを認可します。</span>
            <div className="flex gap-1.5">
              <button
                onClick={handleOpenAuthUrl}
                className="flex items-center gap-1.5 px-2.5 py-1 bg-[#02b8cc]/20 hover:bg-[#02b8cc]/30 text-[#02b8cc] border border-[#02b8cc]/40 rounded-[4px] font-semibold transition-colors"
              >
                <ExternalLink className="w-3 h-3" />
                <span>Open Auth Page</span>
              </button>
              <button
                onClick={handleCopyAuthUrl}
                className="flex items-center gap-1.5 px-2.5 py-1 bg-[#161718] hover:bg-[#23252a] text-[#d0d6e0] border border-[#383b3f] rounded-[4px] transition-colors"
              >
                {copiedUrl ? <Check className="w-3 h-3 text-[#27a644]" /> : <Copy className="w-3 h-3" />}
                <span>{copiedUrl ? 'Copied URL!' : 'Copy URL'}</span>
              </button>
            </div>
          </div>

          <div>
            <span>Step 2: 認可後にリダイレクトされたページに表示される認証コード（code）を貼り付けて登録します。</span>
            <div className="flex gap-2 mt-1.5">
              <input
                type="text"
                placeholder="Paste authorization code here..."
                value={authCode}
                onChange={(e) => setAuthCode(e.target.value)}
                className="flex-1 linear-input py-1.5 px-2 bg-[#0f1011] text-[#d0d6e0] font-mono text-xs"
              />
              <button
                onClick={handleRegisterCode}
                disabled={isRegistering || !authCode.trim()}
                className="flex items-center gap-1.5 px-3 py-1.5 bg-[#27a644] hover:bg-[#27a644]/90 disabled:opacity-50 text-white font-semibold rounded-[4px] transition-all"
              >
                {isRegistering ? <RefreshCw className="w-3 h-3 animate-spin" /> : <Key className="w-3 h-3" />}
                <span>Register Token</span>
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
};
