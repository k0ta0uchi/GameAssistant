import { useState, useCallback, useRef, useEffect } from "react";
import {
  listAudioDevicesApi,
  listWindowsApi,
  captureWindowPreviewApi,
  startAudioPreviewApi,
  isTauriEnv,
} from "../services/backendAdapter";

export interface UseAudioDevicesOptions {
  isSessionActive?: boolean;
}

export interface UseAudioDevicesResult {
  inputDevices: string[];
  discordDevices: string[];
  selectedDevice: string;
  selectedDiscordDevice: string;
  enableDiscordCapture: boolean;
  windows: string[];
  selectedWindow: string;
  previewImage: string;
  levelMeter: number;
  discordLevelMeter: number;
  fetchDevices: () => Promise<void>;
  fetchWindows: () => Promise<void>;
  fetchPreview: (targetWindowName?: string) => Promise<void>;
  setSelectedDevice: React.Dispatch<React.SetStateAction<string>>;
  setSelectedDiscordDevice: React.Dispatch<React.SetStateAction<string>>;
  setEnableDiscordCapture: React.Dispatch<React.SetStateAction<boolean>>;
  setSelectedWindow: React.Dispatch<React.SetStateAction<string>>;
  setLevelMeter: React.Dispatch<React.SetStateAction<number>>;
  setDiscordLevelMeter: React.Dispatch<React.SetStateAction<number>>;
  setPreviewImage: React.Dispatch<React.SetStateAction<string>>;
}

export function useAudioDevices(
  options: UseAudioDevicesOptions = {},
): UseAudioDevicesResult {
  const { isSessionActive = false } = options;

  const [inputDevices, setInputDevices] = useState<string[]>([
    "Default (System Default)",
  ]);
  const [discordDevices, setDiscordDevices] = useState<string[]>([
    "Auto (Discord App / System Loopback)",
  ]);
  const [selectedDevice, setSelectedDevice] = useState<string>(
    "Default (System Default)",
  );
  const [selectedDiscordDevice, setSelectedDiscordDevice] = useState<string>(
    "Auto (Discord App / System Loopback)",
  );
  const [enableDiscordCapture, setEnableDiscordCapture] =
    useState<boolean>(false);

  const [windows, setWindows] = useState<string[]>([]);
  const [selectedWindow, setSelectedWindow] = useState<string>("");
  const [previewImage, setPreviewImage] = useState<string>("");

  const [levelMeter, setLevelMeter] = useState<number>(0);
  const [discordLevelMeter, setDiscordLevelMeter] = useState<number>(0);

  const selectedWindowRef = useRef(selectedWindow);
  useEffect(() => {
    selectedWindowRef.current = selectedWindow;
  }, [selectedWindow]);

  const isFetchingPreviewRef = useRef(false);
  const fetchPreview = useCallback(async (targetWindowName?: string) => {
    const win = targetWindowName || selectedWindowRef.current;
    if (!win || isFetchingPreviewRef.current) return;
    isFetchingPreviewRef.current = true;
    try {
      const preview = await captureWindowPreviewApi(win);
      if (preview) {
        setPreviewImage(preview);
      }
    } catch (e) {
      console.error("Failed to fetch preview:", e);
    } finally {
      isFetchingPreviewRef.current = false;
    }
  }, []);

  const fetchDevices = useCallback(async () => {
    try {
      const audioData = await listAudioDevicesApi();
      if (audioData.inputDevices.length > 0) {
        setInputDevices(audioData.inputDevices);
        setSelectedDevice((prev) => {
          if (prev) return prev;
          return audioData.defaultDevice || audioData.inputDevices[0];
        });
      }
      if (audioData.discordDevices.length > 0) {
        setDiscordDevices(audioData.discordDevices);
        setSelectedDiscordDevice((prev) => {
          if (prev && audioData.discordDevices.includes(prev)) return prev;
          return audioData.defaultDiscordDevice || audioData.discordDevices[0];
        });
      }
    } catch (e) {
      console.error("Failed to fetch devices:", e);
    }
  }, []);

  const fetchWindows = useCallback(async () => {
    try {
      const winList = await listWindowsApi();
      setWindows(winList);
      setSelectedWindow((prev) => {
        const target = prev && winList.includes(prev) ? prev : winList[0] || "";
        return target;
      });
    } catch (e) {
      console.error("Failed to fetch windows:", e);
    }
  }, []);

  // 選択中ウィンドウのプレビュー初回取得
  const lastFetchedWinRef = useRef<string>("");
  useEffect(() => {
    if (selectedWindow && selectedWindow !== lastFetchedWinRef.current) {
      lastFetchedWinRef.current = selectedWindow;
      void fetchPreview(selectedWindow);
    }
  }, [selectedWindow, fetchPreview]);

  // 常時オーディオプレビュー (セッション開始前でもメーターを動かす)
  useEffect(() => {
    if (!isSessionActive && isTauriEnv()) {
      void startAudioPreviewApi({
        micDevice: selectedDevice,
        discordDevice: selectedDiscordDevice,
        enableDiscord: enableDiscordCapture,
      }).catch((err) => console.warn("Failed to start audio preview:", err));
    }
  }, [
    isSessionActive,
    selectedDevice,
    selectedDiscordDevice,
    enableDiscordCapture,
  ]);

  return {
    inputDevices,
    discordDevices,
    selectedDevice,
    selectedDiscordDevice,
    enableDiscordCapture,
    windows,
    selectedWindow,
    previewImage,
    levelMeter,
    discordLevelMeter,
    fetchDevices,
    fetchWindows,
    fetchPreview,
    setSelectedDevice,
    setSelectedDiscordDevice,
    setEnableDiscordCapture,
    setSelectedWindow,
    setLevelMeter,
    setDiscordLevelMeter,
    setPreviewImage,
  };
}
