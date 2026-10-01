export interface SaveSettingResult {
  success: boolean;
  settings?: Record<string, any>;
  warning?: string;
  error?: string;
}

/**
 * 設定保存アダプター
 * - Tauri 環境: invoke("save_setting") を canonical authority として唯一の write 経路とする
 * - Browser/Dev 環境: HTTP POST /api/settings を fallback として利用
 */
export async function saveSettingAdapter(
  key: string,
  value: any,
  isTauri: boolean,
  apiBase: string,
): Promise<SaveSettingResult> {
  if (isTauri) {
    const { invoke } = await import("@tauri-apps/api/core");
    const res: any = await invoke("save_setting", { key, value });
    if (res && typeof res === "object") {
      if ("settings" in res) {
        return {
          success: true,
          settings: res.settings,
          warning: res.warning || undefined,
        };
      }
      return {
        success: true,
        settings: res,
      };
    }
    return { success: true };
  }

  // Browser/Dev fallback (HTTP)
  const response = await fetch(`${apiBase}/api/settings`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ key, value }),
  });

  if (!response.ok) {
    throw new Error(`settings request failed: ${response.status}`);
  }

  const result = await response.json();
  if (result && result.success === false) {
    throw new Error(result.error || `failed to save setting ${key}`);
  }

  return {
    success: true,
    settings: result?.settings,
  };
}
