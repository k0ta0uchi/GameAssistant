import { useState, useEffect, useCallback } from "react";
import type { ModelStatus } from "../types";
import { getModelsStatusApi, isTauriEnv } from "../services/backendAdapter";

export interface UseModelStatusResult {
  modelsStatus: ModelStatus[];
  missingRequiredModels: boolean;
  fetchModelsStatus: () => Promise<ModelStatus[]>;
  fetchModels: () => Promise<ModelStatus[]>;
  setModelsStatus: React.Dispatch<React.SetStateAction<ModelStatus[]>>;
  setMissingRequiredModels: React.Dispatch<React.SetStateAction<boolean>>;
}

export function useModelStatus(): UseModelStatusResult {
  const [modelsStatus, setModelsStatus] = useState<ModelStatus[]>([]);
  const [missingRequiredModels, setMissingRequiredModels] =
    useState<boolean>(false);

  const fetchModelsStatus = useCallback(async (): Promise<ModelStatus[]> => {
    try {
      const list = await getModelsStatusApi();
      const validList = list.filter((m) => {
        const key = `${m.id} ${m.name} ${m.hf_repo}`.toLowerCase();
        return !key.includes("sup-simcse") && !key.includes("sup_simcse");
      });
      setModelsStatus(validList);
      const hasMissing = validList.some((m) => m.required && !m.is_installed);
      setMissingRequiredModels(hasMissing);
      return validList;
    } catch (e) {
      console.warn("Failed to fetch models status:", e);
      return [];
    }
  }, []);

  // 必須モデル不足の警告が出ている間、ダウンロード完了や配置を検知して自動解消するポーリング
  useEffect(() => {
    if (!isTauriEnv() || !missingRequiredModels) return;
    const interval = window.setInterval(() => {
      void fetchModelsStatus();
    }, 4000);
    return () => window.clearInterval(interval);
  }, [missingRequiredModels, fetchModelsStatus]);

  return {
    modelsStatus,
    missingRequiredModels,
    fetchModelsStatus,
    fetchModels: fetchModelsStatus,
    setModelsStatus,
    setMissingRequiredModels,
  };
}
