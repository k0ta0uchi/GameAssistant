import { useState, useCallback } from "react";
import type {
  PromptItem,
  LocalSummaryState,
  LocalSummaryStatus,
} from "../types";
import { LOCAL_SUMMARY_STATES } from "../types";
import { saveSettingAdapter } from "../services/settingsAdapter";
import {
  API_BASE,
  isTauriEnv,
  loadSettingsApi,
  getPromptsApi,
  savePromptApi,
  resetPromptApi,
} from "../services/backendAdapter";

const asRecord = (value: unknown): Record<string, unknown> | null => {
  return value !== null && typeof value === "object"
    ? (value as Record<string, unknown>)
    : null;
};

const firstString = (...values: unknown[]): string | null => {
  const value = values.find(
    (candidate) => typeof candidate === "string" && candidate.trim().length > 0,
  );
  return typeof value === "string" ? value : null;
};

const isLocalSummaryState = (value: unknown): value is LocalSummaryState =>
  typeof value === "string" &&
  LOCAL_SUMMARY_STATES.some((state) => state === value);

/**
 * Local summary status is advisory UI state: a malformed or partially drifted
 * payload must never crash the settings screen. Anything without a known
 * `state` is rejected outright; scalar fields fall back to neutral defaults.
 */
export const normalizeLocalSummaryStatus = (
  value: unknown,
): LocalSummaryStatus | null => {
  const record = asRecord(value);
  if (!record || !isLocalSummaryState(record.state)) return null;
  const queueDepth =
    typeof record.queueDepth === "number" && Number.isFinite(record.queueDepth)
      ? Math.max(0, Math.floor(record.queueDepth))
      : 0;
  return {
    state: record.state,
    queueDepth,
    fallbackActive: record.fallbackActive === true,
    message: firstString(record.message),
  };
};

export type SettingUpdateOptions = { throwOnError?: boolean };

export interface UseSettingsOptions {
  onSettingSynchronized?: (key: string, value: any) => void;
  onSettingRollback?: (key: string, prevValue: any) => void;
  showToast?: (message: string, type: "success" | "info" | "warning") => void;
}

export interface UseSettingsResult {
  settings: Record<string, unknown>;
  prompts: PromptItem[];
  fetchSettings: () => Promise<Record<string, unknown> | null>;
  updateSetting: (
    key: string,
    value: any,
    options?: SettingUpdateOptions,
  ) => Promise<void>;
  fetchPrompts: () => Promise<PromptItem[]>;
  savePrompt: (id: string, value: string) => Promise<boolean>;
  resetPrompt: (id: string) => Promise<boolean>;
  setSettings: React.Dispatch<React.SetStateAction<Record<string, unknown>>>;
  setPrompts: React.Dispatch<React.SetStateAction<PromptItem[]>>;
}

export function useSettings(options: UseSettingsOptions = {}): UseSettingsResult {
  const { onSettingSynchronized, onSettingRollback, showToast } = options;

  const [settings, setSettings] = useState<Record<string, unknown>>({});
  const [prompts, setPrompts] = useState<PromptItem[]>([]);

  const fetchSettings = useCallback(async (): Promise<Record<string, unknown> | null> => {
    try {
      const loaded = await loadSettingsApi();
      if (loaded && Object.keys(loaded).length > 0) {
        setSettings(loaded);
        return loaded;
      }
    } catch (e) {
      console.error("Failed to fetch settings:", e);
    }
    return null;
  }, []);

  const updateSetting = useCallback(
    async (
      key: string,
      value: any,
      opt: SettingUpdateOptions = {},
    ): Promise<void> => {
      let prevValue: any;

      // 1. ローカルステート即時更新（Optimistic update）
      setSettings((prev) => {
        prevValue = prev[key];
        return { ...prev, [key]: value };
      });
      onSettingSynchronized?.(key, value);

      // 2. 単一の永続化経路
      try {
        const res = await saveSettingAdapter(
          key,
          value,
          isTauriEnv(),
          API_BASE,
        );
        if (res.warning) {
          console.warn(`Setting ${key} saved with worker warning:`, res.warning);
          showToast?.(
            `設定は保存されましたが、ワーカー反映警告: ${res.warning}`,
            "warning",
          );
        }
        if (res.settings && typeof res.settings === "object") {
          setSettings(res.settings);
        }
      } catch (e) {
        console.error(`Failed to update setting ${key}:`, e);

        // 3. 永続化失敗時: ロールバック
        setSettings((prev) => ({ ...prev, [key]: prevValue }));
        onSettingRollback?.(key, prevValue);

        const errText = e instanceof Error ? e.message : String(e);
        showToast?.(`設定「${key}」の保存に失敗しました: ${errText}`, "warning");

        if (opt.throwOnError) throw e;
      }
    },
    [onSettingRollback, onSettingSynchronized, showToast],
  );

  const fetchPrompts = useCallback(async (): Promise<PromptItem[]> => {
    try {
      const promptList = await getPromptsApi();
      setPrompts(promptList);
      return promptList;
    } catch (e) {
      console.error("Failed to fetch prompts:", e);
      return [];
    }
  }, []);

  const savePrompt = useCallback(
    async (id: string, value: string): Promise<boolean> => {
      try {
        const result = await savePromptApi(id, value);
        if (result.success && result.prompts) {
          setPrompts(result.prompts);
          showToast?.("✅ プロンプト設定を保存しました", "success");
          return true;
        }
        return false;
      } catch (e) {
        console.error(`Failed to save prompt ${id}:`, e);
        return false;
      }
    },
    [showToast],
  );

  const resetPrompt = useCallback(
    async (id: string): Promise<boolean> => {
      try {
        const result = await resetPromptApi(id);
        if (result.success && result.prompts) {
          setPrompts(result.prompts);
          showToast?.("🔄 プロンプトを初期デフォルトに戻しました", "info");
          return true;
        }
        return false;
      } catch (e) {
        console.error(`Failed to reset prompt ${id}:`, e);
        return false;
      }
    },
    [showToast],
  );

  return {
    settings,
    prompts,
    fetchSettings,
    updateSetting,
    fetchPrompts,
    savePrompt,
    resetPrompt,
    setSettings,
    setPrompts,
  };
}
