import { useEffect, useCallback, useRef } from "react";
import { useWebSocket } from "./useWebSocket";
import { useModelStatus } from "./useModelStatus";
import {
  useSettings,
  normalizeLocalSummaryStatus,
} from "./useSettings";
import { useAudioDevices } from "./useAudioDevices";
import {
  useSetupState,
  normalizeDownloadPercent,
  isExpectedGemmaDownload,
  shouldBlockAppUntilSetupReady,
  shouldShowMainUiForSession,
  isPortableRuntimeReadyForMainUi,
  isSetupReadyForSession,
  normalizeSetupStatus,
  isSetupActionAllowed,
  isTermsAcceptanceRequired,
} from "./useSetupState";
import {
  useRuntimeInitialization,
  normalizeRuntimeInitializationStatus,
} from "./useRuntimeInitialization";
import {
  useSessionController,
  normalizeFactEntry,
} from "./useSessionController";
import { useTauriEvents } from "./useTauriEvents";

// Re-export domain helpers for backward compatibility and test contracts
export {
  normalizeDownloadPercent,
  isExpectedGemmaDownload,
  shouldBlockAppUntilSetupReady,
  shouldShowMainUiForSession,
  isPortableRuntimeReadyForMainUi,
  isSetupReadyForSession,
  normalizeFactEntry,
  normalizeLocalSummaryStatus,
  normalizeSetupStatus,
  isSetupActionAllowed,
  isTermsAcceptanceRequired,
  normalizeRuntimeInitializationStatus,
};

export function useAppState() {
  const { isConnected, addListener } = useWebSocket();

  // ShowToast ref to break circular dependency between settings/session and events
  const showToastRef = useRef<
    (message: string, type?: "success" | "info" | "warning") => void
  >(() => {});

  const handleShowToast = useCallback(
    (message: string, type: "success" | "info" | "warning" = "success") => {
      showToastRef.current(message, type);
    },
    [],
  );

  // 1. Model Status
  const modelStatus = useModelStatus();

  // 2. Settings
  const settingsState = useSettings({
    showToast: handleShowToast,
  });

  // 3. Setup State
  const setupState = useSetupState({
    onModelsNeedRefresh: modelStatus.fetchModels,
    updateSetting: settingsState.updateSetting,
    fetchSettings: settingsState.fetchSettings,
  });

  // 4. Runtime Initialization
  const runtimeInit = useRuntimeInitialization({
    setupStatus: setupState.setupStatus,
    isReadyForMainUi: isPortableRuntimeReadyForMainUi,
    onInitializationCompleted: modelStatus.fetchModels,
    showToast: handleShowToast,
  });

  // 5. Session Controller
  const sessionController = useSessionController({
    fetchSetupStatus: setupState.fetchSetupStatus,
    onWhisperRestarted: setupState.handleAsrReady,
    showToast: handleShowToast,
  });

  // 6. Audio & Windows
  const audioDevices = useAudioDevices({
    isSessionActive: sessionController.status.session,
  });

  // 7. Tauri & WebSocket Events, Logs, Toasts
  const tauriEvents = useTauriEvents({
    setStatus: sessionController.setStatus,
    setLevelMeter: sessionController.setLevelMeter,
    setDiscordLevelMeter: sessionController.setDiscordLevelMeter,
    setCurrentAsr: sessionController.setCurrentAsr,
    setAsrHistory: sessionController.setAsrHistory,
    setFactHistory: sessionController.setFactHistory,
    setGeminiResponse: sessionController.setGeminiResponse,
    setVram: sessionController.setVram,
    setRam: sessionController.setRam,
    setCommentaryTimer: sessionController.setCommentaryTimer,
    setPreviewImage: audioDevices.setPreviewImage,
    handleAsrReady: setupState.handleAsrReady,
    handleAsrWarmupFailed: setupState.handleAsrWarmupFailed,
    setRuntimeInitialization: runtimeInit.setRuntimeInitialization,
    addListener,
  });

  // Wire actual showToast back into the ref
  useEffect(() => {
    showToastRef.current = tauriEvents.showToast;
  }, [tauriEvents.showToast]);

  const { fetchSettings, fetchPrompts } = settingsState;
  const {
    fetchDevices,
    fetchWindows,
    setEnableDiscordCapture,
    setSelectedDevice,
    setSelectedDiscordDevice,
    setSelectedWindow,
  } = audioDevices;
  const { fetchModels } = modelStatus;
  const { fetchLogs } = tauriEvents;

  // Initial full data load
  const fetchAllData = useCallback(async () => {
    const loadedSettings = await fetchSettings();
    if (loadedSettings) {
      if (loadedSettings.enable_discord_capture !== undefined) {
        setEnableDiscordCapture(
          Boolean(loadedSettings.enable_discord_capture),
        );
      }
      if (loadedSettings.audio_device) {
        setSelectedDevice(String(loadedSettings.audio_device));
      }
      if (loadedSettings.discord_audio_device) {
        setSelectedDiscordDevice(
          String(loadedSettings.discord_audio_device),
        );
      }
      if (loadedSettings.window) {
        setSelectedWindow(String(loadedSettings.window));
      }
    }
    await Promise.all([
      fetchDevices(),
      fetchWindows(),
      fetchPrompts(),
      fetchModels(),
    ]);
    await fetchLogs();
  }, [
    fetchSettings,
    fetchPrompts,
    fetchDevices,
    fetchWindows,
    fetchModels,
    fetchLogs,
    setEnableDiscordCapture,
    setSelectedDevice,
    setSelectedDiscordDevice,
    setSelectedWindow,
  ]);

  useEffect(() => {
    void fetchAllData();
  }, [fetchAllData]);

  return {
    // WebSocket
    isConnected,

    // Session status & meters
    status: sessionController.status,
    levelMeter: sessionController.levelMeter,
    discordLevelMeter: sessionController.discordLevelMeter,
    currentAsr: sessionController.currentAsr,
    asrHistory: sessionController.asrHistory,
    factHistory: sessionController.factHistory,
    geminiResponse: sessionController.geminiResponse,
    vram: sessionController.vram,
    ram: sessionController.ram,
    commentaryTimer: sessionController.commentaryTimer,
    sessionStarting: sessionController.sessionStarting,

    // Logs & Toast
    logs: tauriEvents.logs,
    toast: tauriEvents.toast,
    showToast: tauriEvents.showToast,
    clearLogs: tauriEvents.clearLogs,

    // Audio & Window devices
    inputDevices: audioDevices.inputDevices,
    discordDevices: audioDevices.discordDevices,
    selectedDevice: audioDevices.selectedDevice,
    selectedDiscordDevice: audioDevices.selectedDiscordDevice,
    enableDiscordCapture: audioDevices.enableDiscordCapture,
    windows: audioDevices.windows,
    selectedWindow: audioDevices.selectedWindow,
    previewImage: audioDevices.previewImage,
    fetchWindows: audioDevices.fetchWindows,
    fetchPreview: audioDevices.fetchPreview,

    // Settings & Prompts
    settings: settingsState.settings,
    prompts: settingsState.prompts,
    updateSetting: settingsState.updateSetting,
    fetchSettings: settingsState.fetchSettings,
    fetchPrompts: settingsState.fetchPrompts,
    savePrompt: settingsState.savePrompt,
    resetPrompt: settingsState.resetPrompt,

    // Session controls
    startSession: sessionController.startSession,
    stopSession: sessionController.stopSession,
    restartWhisper: sessionController.restartWhisper,

    // Model status
    modelsStatus: modelStatus.modelsStatus,
    missingRequiredModels: modelStatus.missingRequiredModels,
    fetchModelsStatus: modelStatus.fetchModels,

    // Setup state
    setupStatus: setupState.setupStatus,
    setupProgress: setupState.setupProgress,
    isSetupRunning: setupState.isSetupRunning,
    setupError: setupState.setupError,
    isElevationRequesting: setupState.isElevationRequesting,
    fetchSetupStatus: setupState.fetchSetupStatus,
    runSetup: setupState.runSetup,
    acceptTermsAndRunSetup: setupState.acceptTermsAndRunSetup,
    requestSetupElevation: setupState.requestSetupElevation,
    cancelSetup: setupState.cancelSetup,

    // Runtime initialization
    runtimeInitialization: runtimeInit.runtimeInitialization,
    initializeRuntime: runtimeInit.initializeRuntime,
    fetchRuntimeInitializationStatus:
      runtimeInit.fetchRuntimeInitializationStatus,
  };
}
