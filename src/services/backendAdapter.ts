import type {
  LogEntry,
  ModelStatus,
  PromptItem,
} from "../types";

export const API_BASE = "http://127.0.0.1:18080";

export const isTauriEnv = (): boolean =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Setup APIs */
export async function getSetupStatusApi(): Promise<unknown> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    return await invoke<unknown>("get_setup_status");
  }
  const response = await fetch(`${API_BASE}/api/setup/status`);
  if (!response.ok) {
    throw new Error(`setup status request failed: ${response.status}`);
  }
  return await response.json();
}

export async function runSetupApi(): Promise<unknown> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    return await invoke<unknown>("run_setup");
  }
  const response = await fetch(`${API_BASE}/api/setup/run`, {
    method: "POST",
  });
  if (!response.ok) {
    throw new Error(`setup run request failed: ${response.status}`);
  }
  return await response.json();
}

export async function cancelSetupApi(): Promise<unknown> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    return await invoke<unknown>("cancel_setup");
  }
  const response = await fetch(`${API_BASE}/api/setup/cancel`, {
    method: "POST",
  });
  if (!response.ok) {
    throw new Error(`setup cancel request failed: ${response.status}`);
  }
  return await response.json();
}

export async function requestSetupElevationApi(): Promise<unknown> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    return await invoke<unknown>("request_setup_elevation");
  }
  const response = await fetch(`${API_BASE}/api/setup/elevation`, {
    method: "POST",
  });
  if (!response.ok) {
    throw new Error(`setup elevation request failed: ${response.status}`);
  }
  return await response.json();
}

/** Runtime initialization APIs */
export async function getRuntimeInitializationStatusApi(): Promise<unknown> {
  if (!isTauriEnv()) return null;
  const { invoke } = await import("@tauri-apps/api/core");
  return await invoke<unknown>("get_runtime_initialization_status");
}

export async function initializeRuntimeApi(): Promise<unknown> {
  if (!isTauriEnv()) return null;
  const { invoke } = await import("@tauri-apps/api/core");
  return await invoke<unknown>("initialize_runtime");
}

/** Model APIs */
export async function getModelsStatusApi(): Promise<ModelStatus[]> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    return await invoke<ModelStatus[]>("get_models_status", {
      customDir: null,
    });
  }
  return [];
}

/** Settings & Prompts APIs */
export async function loadSettingsApi(): Promise<Record<string, unknown> | null> {
  if (isTauriEnv()) {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const loaded = await invoke<Record<string, unknown>>("load_settings");
      if (loaded && Object.keys(loaded).length > 0) return loaded;
    } catch (e) {
      console.error("Tauri load_settings error:", e);
    }
  }
  try {
    const res = await fetch(`${API_BASE}/api/settings`);
    return await res.json();
  } catch (e) {
    console.error("Browser load_settings error:", e);
    return null;
  }
}

export async function getPromptsApi(): Promise<PromptItem[]> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    const promptList = await invoke<PromptItem[]>("get_prompts");
    if (promptList && Array.isArray(promptList)) return promptList;
  }
  const res = await fetch(`${API_BASE}/api/prompts`);
  const data = await res.json();
  return data.prompts || [];
}

export async function savePromptApi(
  id: string,
  text: string,
): Promise<{ success: boolean; error?: string }> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("save_prompt", { promptId: id, text });
    return { success: true };
  }
  const res = await fetch(`${API_BASE}/api/prompts/${id}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ text }),
  });
  return await res.json();
}

export async function resetPromptApi(
  id: string,
): Promise<{ success: boolean; text?: string; error?: string }> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    const defaultText = await invoke<string>("reset_prompt", { promptId: id });
    return { success: true, text: defaultText };
  }
  const res = await fetch(`${API_BASE}/api/prompts/${id}/reset`, {
    method: "POST",
  });
  return await res.json();
}

/** Audio & Window devices APIs */
export interface AudioDevicesResult {
  inputDevices: string[];
  discordDevices: string[];
  defaultDevice: string | null;
  defaultDiscordDevice: string | null;
}

export async function listAudioDevicesApi(): Promise<AudioDevicesResult> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    const audioData = await invoke<{
      input_devices: string[];
      default_device: string | null;
      output_devices?: string[];
      default_output_device?: string | null;
    }>("list_audio_devices");
    if (audioData) {
      const inputs = audioData.input_devices || [];
      const outputs =
        audioData.output_devices && audioData.output_devices.length > 0
          ? audioData.output_devices
          : ["Default (System Playback Loopback)"];
      return {
        inputDevices: inputs,
        discordDevices: outputs,
        defaultDevice: audioData.default_device || inputs[0] || null,
        defaultDiscordDevice: audioData.default_output_device || outputs[0] || null,
      };
    }
  }

  const res = await fetch(`${API_BASE}/api/devices`);
  const data = await res.json();
  const inputs = data.input_devices || [];
  const outputs = data.discord_devices || [];
  return {
    inputDevices: inputs,
    discordDevices: outputs,
    defaultDevice: data.selected_device || inputs[0] || null,
    defaultDiscordDevice:
      data.selected_discord_device ||
      outputs[0] ||
      "Auto (Discord App / System Loopback)",
  };
}

export async function listWindowsApi(): Promise<string[]> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    const winList = await invoke<string[]>("list_windows");
    if (winList && winList.length > 0) return winList;
  }
  const res = await fetch(`${API_BASE}/api/windows`);
  const data = await res.json();
  return data.windows || [];
}

export async function captureWindowPreviewApi(
  title: string,
): Promise<string | null> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    const preview = await invoke<string | null>("capture_window_preview", {
      title,
    });
    if (preview) return preview;
  }
  const res = await fetch(`${API_BASE}/api/capture/preview`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ window: title }),
  });
  const data = await res.json();
  return data.success && data.image ? data.image : null;
}

export async function startAudioPreviewApi(params: {
  micDevice: string;
  discordDevice: string;
  enableDiscord: boolean;
}): Promise<void> {
  if (!isTauriEnv()) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("start_audio_preview", {
    micDevice: params.micDevice,
    discordDevice: params.discordDevice,
    enableDiscord: params.enableDiscord,
  });
}

/** Logs APIs */
export async function getAppLogsApi(): Promise<LogEntry[]> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    const rustLogs = await invoke<LogEntry[]>("get_app_logs");
    if (rustLogs && Array.isArray(rustLogs)) return rustLogs;
  }
  const res = await fetch(`${API_BASE}/api/logs`);
  const data = await res.json();
  return data.success && Array.isArray(data.logs) ? data.logs : [];
}

export async function clearAppLogsApi(): Promise<void> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("clear_app_logs");
  }
}

/** Session APIs */
export async function startSessionApi(params?: {
  window?: string;
  audio_device?: string;
  discord_audio_device?: string;
  enable_discord_audio?: boolean;
}): Promise<{ success: boolean; error?: string }> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke(
      "session_start",
      params
        ? {
            targetWindow: params.window,
            micDevice: params.audio_device,
            discordDevice: params.discord_audio_device,
            enableDiscord: params.enable_discord_audio,
          }
        : {},
    );
    return { success: true };
  }
  const res = await fetch(`${API_BASE}/api/session/start`, {
    method: "POST",
    headers: params ? { "Content-Type": "application/json" } : undefined,
    body: params ? JSON.stringify(params) : undefined,
  });
  return await res.json();
}

export async function stopSessionApi(): Promise<{ success: boolean }> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("session_stop");
    return { success: true };
  }
  const res = await fetch(`${API_BASE}/api/session/stop`, {
    method: "POST",
  });
  return await res.json();
}

export async function restartWhisperApi(): Promise<{ success: boolean }> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("restart_whisper");
    return { success: true };
  }
  const res = await fetch(`${API_BASE}/api/whisper/restart`, {
    method: "POST",
  });
  return await res.json();
}
