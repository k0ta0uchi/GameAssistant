import { useState } from 'react';
import { useAppState } from './hooks/useAppState';
import { Sidebar } from './components/Layout/Sidebar';
import { MainDashboard } from './components/Dashboard/MainDashboard';
import { LiveLogTerminal } from './components/Console/LiveLogTerminal';
import { SettingsModal } from './components/Modals/SettingsModal';
import { MemoryModal } from './components/Modals/MemoryModal';
import { LoadingScreen } from './components/Common/LoadingScreen';
import { Toast } from './components/Common/Toast';

export function App() {
  const {
    isConnected,
    status,
    levelMeter,
    currentAsr,
    asrHistory,
    geminiResponse,
    vram,
    ram,
    commentaryTimer,
    logs,
    inputDevices,
    discordDevices,
    selectedDevice,
    selectedDiscordDevice,
    enableDiscordCapture,
    windows,
    selectedWindow,
    previewImage,
    settings,
    prompts,
    startSession,
    stopSession,
    restartWhisper,
    updateSetting,
    savePrompt,
    resetPrompt,
    fetchWindows,
    fetchPreview,
    clearLogs,
    toast,
    missingRequiredModels,
  } = useAppState();

  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const [settingsInitialTab, setSettingsInitialTab] = useState<'engines' | 'models' | 'prompts' | 'twitch' | 'preferences' | 'blog_skills'>('engines');
  const [isMemoryOpen, setIsMemoryOpen] = useState(false);

  const handleOpenSettings = (tab: 'engines' | 'models' | 'prompts' | 'twitch' | 'preferences' | 'blog_skills' = 'engines') => {
    setSettingsInitialTab(tab);
    setIsSettingsOpen(true);
  };

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-[#08090a] text-[#d0d6e0]">
      {/* トースト通知 */}
      <Toast toast={toast} />

      {/* 左サイドバー */}
      <Sidebar
        sessionRunning={status.session}
        onStartSession={startSession}
        onStopSession={stopSession}
        onRestartWhisper={restartWhisper}
        inputDevices={inputDevices}
        selectedDevice={selectedDevice}
        onDeviceChange={(dev) => updateSetting('audio_device', dev)}
        levelMeter={levelMeter}
        enableDiscordCapture={enableDiscordCapture}
        onToggleDiscordCapture={(enabled) => updateSetting('enable_discord_capture', enabled)}
        discordDevices={discordDevices}
        selectedDiscordDevice={selectedDiscordDevice}
        onDiscordDeviceChange={(dev) => updateSetting('discord_audio_device', dev)}
        windows={windows}
        selectedWindow={selectedWindow}
        onWindowChange={(win) => updateSetting('window', win)}
        onRefreshWindows={fetchWindows}
        previewImage={previewImage}
        onFetchPreview={fetchPreview}
        vram={vram}
        ram={ram}
        onOpenSettings={() => handleOpenSettings('engines')}
        onOpenMemory={() => setIsMemoryOpen(true)}
        isConnected={isConnected}
      />

      {/* メインエリア (右側: 上部ダッシュボード + 下部ログコンソール) */}
      <main className="flex-1 flex flex-col h-screen overflow-hidden">
        {/* 必須モデル未ダウンロード時の警告バナー */}
        {missingRequiredModels && (
          <div className="bg-red-950/40 border-b border-red-500/40 px-4 py-2 flex items-center justify-between text-xs text-red-200 animate-fade-in z-20">
            <div className="flex items-center gap-2 font-medium">
              <span className="flex h-2 w-2 relative">
                <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-red-400 opacity-75"></span>
                <span className="relative inline-flex rounded-full h-2 w-2 bg-red-500"></span>
              </span>
              <span>⚠️ 音声認識・埋め込みモデル（Kotoba-Whisper / GLuCoSE）が未ダウンロードです。</span>
            </div>
            <button
              onClick={() => handleOpenSettings('models')}
              className="px-3 py-1 bg-red-500/20 hover:bg-red-500/30 text-red-300 hover:text-white border border-red-500/40 rounded text-xs font-semibold flex items-center gap-1.5 transition-all shadow-sm"
            >
              <span>モデル設定を開いてダウンロード</span>
              <span>→</span>
            </button>
          </div>
        )}

        <MainDashboard
          status={status}
          geminiResponse={geminiResponse}
          currentAsr={currentAsr}
          asrHistory={asrHistory}
          commentaryProgress={commentaryTimer.progress}
          commentaryRemaining={commentaryTimer.remaining}
        />
        <LiveLogTerminal logs={logs} onClear={clearLogs} />
      </main>

      {/* 設定モーダル */}
      <SettingsModal
        isOpen={isSettingsOpen}
        onClose={() => setIsSettingsOpen(false)}
        settings={settings}
        onUpdateSetting={updateSetting}
        discordDevices={discordDevices}
        prompts={prompts}
        onSavePrompt={savePrompt}
        onResetPrompt={resetPrompt}
        initialTab={settingsInitialTab}
      />

      {/* 記憶管理モーダル */}
      <MemoryModal isOpen={isMemoryOpen} onClose={() => setIsMemoryOpen(false)} />

      {/* 起動時ローディング画面 */}
      <LoadingScreen isConnected={isConnected} />
    </div>
  );
}

export default App;
