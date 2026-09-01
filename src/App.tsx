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
  } = useAppState();

  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const [isMemoryOpen, setIsMemoryOpen] = useState(false);

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
        onOpenSettings={() => setIsSettingsOpen(true)}
        onOpenMemory={() => setIsMemoryOpen(true)}
        isConnected={isConnected}
      />

      {/* メインエリア (右側: 上部ダッシュボード + 下部ログコンソール) */}
      <main className="flex-1 flex flex-col h-screen overflow-hidden">
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
      />

      {/* 記憶管理モーダル */}
      <MemoryModal isOpen={isMemoryOpen} onClose={() => setIsMemoryOpen(false)} />

      {/* 起動時ローディング画面 */}
      <LoadingScreen isConnected={isConnected} />
    </div>
  );
}

export default App;
