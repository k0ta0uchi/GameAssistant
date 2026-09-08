import { useEffect, useRef, useState } from "react";
import { shouldShowMainUiForSession, useAppState } from "./hooks/useAppState";
import { dismissStartupLoader } from "./startupLoader";
import { Sidebar } from "./components/Layout/Sidebar";
import { MainDashboard } from "./components/Dashboard/MainDashboard";
import { LiveLogTerminal } from "./components/Console/LiveLogTerminal";
import { SettingsModal } from "./components/Modals/SettingsModal";
import { MemoryModal } from "./components/Modals/MemoryModal";
import { LoadingScreen } from "./components/Common/LoadingScreen";
import { Toast } from "./components/Common/Toast";
import { SetupScreen } from "./components/Common/SetupScreen";

export function App() {
  const {
    isConnected,
    status,
    levelMeter,
    currentAsr,
    asrHistory,
    factHistory,
    geminiResponse,
    vram,
    ram,
    commentaryTimer,
    sessionStarting,
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
    setupStatus,
    setupProgress,
    isSetupRunning,
    setupError,
    isElevationRequesting,
    fetchSetupStatus,
    runSetup,
    acceptTermsAndRunSetup,
    requestSetupElevation,
    cancelSetup,
  } = useAppState();

  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const [settingsInitialTab, setSettingsInitialTab] = useState<
    "engines" | "models" | "prompts" | "twitch" | "preferences" | "blog_skills"
  >("engines");
  const [isMemoryOpen, setIsMemoryOpen] = useState(false);

  const tauriEnvironment =
    typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
  const setupStatusPending = tauriEnvironment && setupStatus === null;
  // Keep the static loader above React until the first setup snapshot is
  // known. This avoids flashing SetupScreen on healthy portable installs and
  // also prevents the main UI/effects from mounting before the runtime gate.
  useEffect(() => {
    if (!tauriEnvironment || !setupStatusPending) dismissStartupLoader();
  }, [setupStatusPending, tauriEnvironment]);

  // Browser/dev fallback has no setup manager. Tauri fails closed while the
  // first status read is unknown, unhealthy, or not ready.
  const showMainUi = shouldShowMainUiForSession(tauriEnvironment, setupStatus);
  const showSetupScreen =
    tauriEnvironment && !setupStatusPending && !showMainUi;
  const termsFlowRef = useRef(false);
  const setupFocusReturnRef = useRef<HTMLDivElement>(null);

  const handleOpenSettings = (
    tab:
      | "engines"
      | "models"
      | "prompts"
      | "twitch"
      | "preferences"
      | "blog_skills" = "engines",
  ) => {
    setSettingsInitialTab(tab);
    setIsSettingsOpen(true);
  };

  return (
    <div
      ref={setupFocusReturnRef}
      tabIndex={-1}
      className="flex h-screen w-screen overflow-hidden bg-[#08090a] text-[#d0d6e0]"
    >
      {/* トースト通知 */}
      <Toast toast={toast} />

      {showMainUi && (
        <>
          {/* 左サイドバー */}
          <Sidebar
            sessionRunning={status.session}
            sessionStarting={sessionStarting}
            onStartSession={startSession}
            onStopSession={stopSession}
            onRestartWhisper={restartWhisper}
            inputDevices={inputDevices}
            selectedDevice={selectedDevice}
            onDeviceChange={(dev) => updateSetting("audio_device", dev)}
            levelMeter={levelMeter}
            enableDiscordCapture={enableDiscordCapture}
            onToggleDiscordCapture={(enabled) =>
              updateSetting("enable_discord_capture", enabled)
            }
            discordDevices={discordDevices}
            selectedDiscordDevice={selectedDiscordDevice}
            onDiscordDeviceChange={(dev) =>
              updateSetting("discord_audio_device", dev)
            }
            windows={windows}
            selectedWindow={selectedWindow}
            onWindowChange={(win) => updateSetting("window", win)}
            onRefreshWindows={fetchWindows}
            previewImage={previewImage}
            onFetchPreview={fetchPreview}
            vram={vram}
            ram={ram}
            onOpenSettings={() => handleOpenSettings("engines")}
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
                  <span>
                    ⚠️ 必須モデル（Kotoba-Whisper / GLuCoSE /
                    Gemma）が未ダウンロードまたは検証待ちです。
                  </span>
                </div>
                <button
                  onClick={() => handleOpenSettings("models")}
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
              factHistory={factHistory}
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
            setupStatus={setupStatus}
            onRefreshSetupStatus={fetchSetupStatus}
            discordDevices={discordDevices}
            prompts={prompts}
            onSavePrompt={savePrompt}
            onResetPrompt={resetPrompt}
            initialTab={settingsInitialTab}
          />

          {/* 記憶管理モーダル */}
          <MemoryModal
            isOpen={isMemoryOpen}
            onClose={() => setIsMemoryOpen(false)}
            logs={logs}
            onClearLogs={clearLogs}
          />

          {/* 起動時ローディング画面 */}
          <LoadingScreen isConnected={isConnected} />
        </>
      )}

      {/* 初回ランタイムセットアップ画面（Tauri ビルドのみ） */}
      {showSetupScreen && (
        <SetupScreen
          status={setupStatus}
          progress={setupProgress}
          running={isSetupRunning}
          error={setupError}
          onStart={() => {
            void runSetup();
          }}
          onRetry={() => {
            void fetchSetupStatus().then((latest) => {
              if (latest?.ready) return;
              void runSetup();
            });
          }}
          onAcceptTerms={() => {
            if (termsFlowRef.current) return;
            const accepted = window.confirm(
              "NOTICE-GEMMA.txt と Gemma Terms を確認しました。Gemma のダウンロードと使用に同意しますか？\n\nhttps://ai.google.dev/gemma/terms",
            );
            if (!accepted) return;
            termsFlowRef.current = true;
            void acceptTermsAndRunSetup().finally(() => {
              termsFlowRef.current = false;
            });
          }}
          onRequestElevation={() => {
            void requestSetupElevation();
          }}
          elevationRequesting={isElevationRequesting}
          onCancel={async () => {
            await cancelSetup();
          }}
          focusReturnRef={setupFocusReturnRef}
        />
      )}
    </div>
  );
}

export default App;
