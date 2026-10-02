import { useState, useEffect, useCallback, useRef } from "react";
import {
  type SystemStatus,
  type ResourceInfo,
  type WsMessage,
  type LogEntry,
  type AsrEntry,
  type FactEntry,
  type RuntimeInitializationStatus,
} from "../types";
import { isTauriEnv, getAppLogsApi, clearAppLogsApi } from "../services/backendAdapter";
import { normalizeFactEntry } from "./useSessionController";
import { normalizeRuntimeInitializationStatus } from "./useRuntimeInitialization";

/** Drain collected unlisten functions exactly once each, in order. */
const runUnlisteners = (unlisteners: Array<() => void>): void => {
  for (const unlisten of unlisteners.splice(0)) unlisten();
};

export interface UseTauriEventsOptions {
  // SessionController setters
  setStatus: React.Dispatch<React.SetStateAction<SystemStatus>>;
  setLevelMeter: React.Dispatch<React.SetStateAction<number>>;
  setDiscordLevelMeter: React.Dispatch<React.SetStateAction<number>>;
  setCurrentAsr: React.Dispatch<
    React.SetStateAction<{
      text: string;
      isFinal: boolean;
      isPrompt?: boolean;
      latencyMs?: number | null;
    }>
  >;
  setAsrHistory: React.Dispatch<React.SetStateAction<AsrEntry[]>>;
  setFactHistory: React.Dispatch<React.SetStateAction<FactEntry[]>>;
  setGeminiResponse: React.Dispatch<React.SetStateAction<string>>;
  setVram: React.Dispatch<React.SetStateAction<ResourceInfo>>;
  setRam: React.Dispatch<React.SetStateAction<ResourceInfo>>;
  setCommentaryTimer: React.Dispatch<
    React.SetStateAction<{
      progress: number;
      remaining: number;
    }>
  >;
  // Audio setters
  selectedWindow?: string;
  setPreviewImage: React.Dispatch<React.SetStateAction<string>>;
  // Setup handlers
  handleAsrReady: () => void;
  handleAsrWarmupFailed: (message?: string) => void;
  // Runtime handlers
  setRuntimeInitialization: React.Dispatch<
    React.SetStateAction<RuntimeInitializationStatus>
  >;
  // WebSocket hook listener
  addListener: (listener: (msg: WsMessage) => void) => () => void;
}

export interface UseTauriEventsResult {
  logs: LogEntry[];
  toast: {
    id: string;
    message: string;
    type: "success" | "info" | "warning";
  } | null;
  showToast: (message: string, type?: "success" | "info" | "warning") => void;
  fetchLogs: () => Promise<void>;
  clearLogs: () => void;
  setLogs: React.Dispatch<React.SetStateAction<LogEntry[]>>;
}

export function useTauriEvents(
  options: UseTauriEventsOptions,
): UseTauriEventsResult {
  const {
    setStatus,
    setLevelMeter,
    setDiscordLevelMeter,
    setCurrentAsr,
    setAsrHistory,
    setFactHistory,
    setGeminiResponse,
    setVram,
    setRam,
    setCommentaryTimer,
    selectedWindow,
    setPreviewImage,
    handleAsrReady,
    handleAsrWarmupFailed,
    setRuntimeInitialization,
    addListener,
  } = options;

  // トースト通知状態
  const captureTargetRef = useRef(selectedWindow);
  captureTargetRef.current = selectedWindow;
  const [toast, setToast] = useState<{
    id: string;
    message: string;
    type: "success" | "info" | "warning";
  } | null>(null);
  const toastTimerRef = useRef<NodeJS.Timeout | null>(null);

  const showToast = useCallback(
    (message: string, type: "success" | "info" | "warning" = "success") => {
      if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
      setToast({
        id: Math.random().toString(36).substring(2, 9),
        message,
        type,
      });
      toastTimerRef.current = setTimeout(() => {
        setToast(null);
      }, 4000);
    },
    [],
  );

  // リアルタイムログ
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const micActiveTimerRef = useRef<NodeJS.Timeout | null>(null);

  const fetchLogs = useCallback(async () => {
    try {
      const logEntries = await getAppLogsApi();
      if (Array.isArray(logEntries)) {
        setLogs((prev) => {
          const existingKeys = new Set(
            prev.map((l) => `${l.timestamp}_${l.message}`),
          );
          const newEntries = logEntries.filter(
            (l) => !existingKeys.has(`${l.timestamp}_${l.message}`),
          );
          return [...prev, ...newEntries].slice(-500);
        });
      }
    } catch {
      // ignore
    }
  }, []);

  const clearLogs = useCallback(() => {
    setLogs([]);
    void clearAppLogsApi().catch(() => {});
  }, []);

  // -------------------------------------------------------------
  // Tauri イベントリスナー初期化 (Rust Native イベント)
  // -------------------------------------------------------------
  useEffect(() => {
    let unlistenAll: (() => void) | null = null;
    let isCancelled = false;
    const unlistenFns: (() => void)[] = [];

    void import("@tauri-apps/api/event")
      .then(async ({ listen }) => {
        if (isCancelled) return;

        const once = (unlisten: () => void): (() => void) => {
          let called = false;
          return () => {
            if (called) return;
            called = true;
            unlisten();
          };
        };
        const register = async <T>(
          eventName: string,
          handler: (event: { payload: T }) => void,
        ) => {
          const remove = once(
            await listen<T>(eventName, (event) => {
              if (!isCancelled) handler(event);
            }),
          );
          if (isCancelled) remove();
          else unlistenFns.push(remove);
        };

        try {
          await register<{ ram: ResourceInfo; vram: ResourceInfo }>(
            "resource_status",
            (event) => {
              if (event.payload) {
                setRam(event.payload.ram);
                setVram(event.payload.vram);
              }
            },
          );

          await register<number>("level_meter", (event) => {
            if (typeof event.payload === "number") {
              const lvl = event.payload;
              setLevelMeter(lvl);
              if (lvl > 0.012) {
                setStatus((prev) => (prev.asr ? prev : { ...prev, asr: true }));
                if (micActiveTimerRef.current)
                  clearTimeout(micActiveTimerRef.current);
                micActiveTimerRef.current = setTimeout(() => {
                  setStatus((prev) => ({ ...prev, asr: false }));
                }, 450);
              }
            }
          });

          await register<number>("discord_level_meter", (event) => {
            if (typeof event.payload === "number") {
              setDiscordLevelMeter(event.payload);
            }
          });

          await register<{
            text: string;
            is_final: boolean;
            is_prompt?: boolean;
            stream?: string;
            latency_ms?: number | null;
            event_id?: string | null;
          }>("asr_result", (event) => {
            if (event.payload && event.payload.text) {
              const rawText = event.payload.text.trim();
              const isPrompt = !!event.payload.is_prompt;
              const isDiscord =
                event.payload.stream === "discord" ||
                rawText.startsWith("[Discord]");

              setStatus((prev) => (prev.asr ? prev : { ...prev, asr: true }));
              if (micActiveTimerRef.current)
                clearTimeout(micActiveTimerRef.current);
              micActiveTimerRef.current = setTimeout(
                () => {
                  setStatus((prev) => ({ ...prev, asr: false }));
                },
                event.payload.is_final ? 350 : 800,
              );

              if (rawText) {
                if (event.payload.is_final) {
                  setAsrHistory((prev) => {
                    const lastIndex = prev.length - 1;
                    const last = prev[lastIndex];
                    if (
                      last &&
                      (last.text === rawText ||
                        rawText.includes(last.text) ||
                        last.text.includes(rawText))
                    ) {
                      const updated = [...prev];
                      updated[lastIndex] = {
                        ...last,
                        text: rawText,
                        isPrompt: isPrompt || last.isPrompt,
                        isDiscord: isDiscord || last.isDiscord,
                        latencyMs:
                          event.payload.latency_ms ?? last.latencyMs ?? null,
                      };
                      return updated;
                    }
                    return [
                      ...prev.slice(-29),
                      {
                        id: Math.random().toString(36).substring(2, 9),
                        text: rawText,
                        timestamp: new Date().toLocaleTimeString(),
                        isDiscord,
                        isPrompt,
                        latencyMs: event.payload.latency_ms ?? null,
                        seq: Date.now(),
                      },
                    ];
                  });
                  setCurrentAsr({ text: "", isFinal: true, isPrompt: false });
                } else {
                  setCurrentAsr({
                    text: rawText,
                    isFinal: false,
                    isPrompt,
                    latencyMs: event.payload.latency_ms ?? null,
                  });
                }
              }
            }
          });

          await register<unknown>("memory-fact-created", (event) => {
            const fact = normalizeFactEntry(event.payload);
            if (!fact) return;
            setFactHistory((prev) => {
              const duplicate = prev.some(
                (item) =>
                  item.id === fact.id ||
                  (fact.sourceEventId !== undefined &&
                    item.sourceEventId === fact.sourceEventId),
              );
              if (duplicate) return prev;
              return [...prev.slice(-29), { ...fact, seq: Date.now() }];
            });
          });

          await register<{
            is_running: boolean;
            remaining_sec: number;
            total_sec: number;
          }>("auto_commentary_status", (event) => {
            if (event.payload) {
              const total = event.payload.total_sec || 1;
              const remaining = event.payload.remaining_sec || 0;
              const progress = Math.min(
                100,
                Math.max(0, ((total - remaining) / total) * 100),
              );
              setCommentaryTimer({
                progress,
                remaining,
              });
            }
          });

          await register<{ image?: string; source?: string; target?: string }>(
            "window_preview_updated",
            (event) => {
              if (typeof event.payload?.image === "string" && event.payload.target === captureTargetRef.current) {
                setPreviewImage(event.payload.image);
              }
            },
          );

          await register<{
            type: string;
            author: string;
            content: string;
            timestamp: string;
          }>("session-event", (event) => {
            if (event.payload) {
              const { type, content } = event.payload;
              if (type === "ai_response" || type === "auto_commentary") {
                setGeminiResponse(content);
              }
            }
          });

          await register<{ is_generating: boolean }>(
            "gemini_status",
            (event) => {
              if (event.payload) {
                setStatus((prev) => ({
                  ...prev,
                  gemini: !!event.payload.is_generating,
                }));
              }
            },
          );

          await register<{ is_playing: boolean }>("tts_status", (event) => {
            if (event.payload) {
              setStatus((prev) => ({
                ...prev,
                tts: !!event.payload.is_playing,
              }));
            }
          });

          await register<{ connected: boolean }>("twitch_status", (event) => {
            if (event.payload) {
              setStatus((prev) => ({
                ...prev,
                twitch: !!event.payload.connected,
              }));
            }
          });

          await register<{
            message: string;
            type?: "success" | "info" | "warning";
          }>("toast_notice", (event) => {
            if (event.payload?.message) {
              showToast(event.payload.message, event.payload.type || "info");
            }
          });

          await register<{ ready?: boolean; timestamp?: string }>(
            "asr_ready",
            (event) => {
              if (event.payload?.ready !== true) return;
              handleAsrReady();
            },
          );

          await register<{ message?: string }>("asr_warmup_failed", (event) => {
            handleAsrWarmupFailed(event.payload?.message);
          });

          await register<unknown>("runtime_initialization", (event) => {
            const snapshot = normalizeRuntimeInitializationStatus(
              event.payload,
            );
            if (!snapshot) return;
            setRuntimeInitialization(snapshot);
          });

          if (!isCancelled) {
            import("@tauri-apps/api/core")
              .then(({ invoke }) =>
                invoke<unknown>("get_runtime_initialization_status"),
              )
              .then((raw) => {
                if (!isCancelled) {
                  const snap = normalizeRuntimeInitializationStatus(raw);
                  if (snap) setRuntimeInitialization(snap);
                }
              })
              .catch(() => {});
          }

          await register<LogEntry>("app_log", (event) => {
            if (event.payload) {
              setLogs((prev) => [...prev.slice(-999), event.payload]);
            }
          });

          // Twitch 初期接続状態チェック
          import("@tauri-apps/api/core").then(({ invoke }) => {
            invoke<{ connected: boolean }>("twitch_get_status")
              .then((res) => {
                if (!isCancelled && res && res.connected) {
                  setStatus((prev) => ({ ...prev, twitch: true }));
                }
              })
              .catch(() => {});
          });

          if (isCancelled) {
            runUnlisteners(unlistenFns);
          } else {
            unlistenAll = () => runUnlisteners(unlistenFns);
          }
        } catch (err) {
          console.warn("Event listener registration error:", err);
          runUnlisteners(unlistenFns);
        }
      })
      .catch((error) => {
        if (!isCancelled)
          console.warn("Event listener module unavailable:", error);
        runUnlisteners(unlistenFns);
      });

    return () => {
      isCancelled = true;
      if (micActiveTimerRef.current) {
        clearTimeout(micActiveTimerRef.current);
      }
      if (unlistenAll) {
        unlistenAll();
      } else {
        runUnlisteners(unlistenFns);
      }
    };
  }, [
    handleAsrReady,
    handleAsrWarmupFailed,
    setCurrentAsr,
    setAsrHistory,
    setFactHistory,
    setCommentaryTimer,
    setDiscordLevelMeter,
    setGeminiResponse,
    setLevelMeter,
    setPreviewImage,
    setRam,
    setRuntimeInitialization,
    setStatus,
    setVram,
    showToast,
  ]);

  // WebSocket メッセージ受信ハンドラ
  useEffect(() => {
    const removeListener = addListener((msg: WsMessage) => {
      switch (msg.type) {
        case "status":
          setStatus(msg.status);
          break;
        case "level_meter":
          setLevelMeter(msg.level);
          break;
        case "asr": {
          const isFinal = Boolean(msg.is_final);
          const rawText = (msg.text || "").trim();
          if (!rawText) break;
          const stream =
            typeof msg.stream === "string" && msg.stream.trim()
              ? msg.stream.trim()
              : rawText.startsWith("[Discord]")
                ? "discord"
                : "mic";
          const isPrompt = Boolean(msg.is_prompt);
          const latencyMs =
            typeof msg.latency_ms === "number" &&
            Number.isFinite(msg.latency_ms)
              ? msg.latency_ms
              : null;

          setCurrentAsr({
            text: rawText,
            isFinal,
            isPrompt,
            latencyMs,
          });

          if (isFinal) {
            setAsrHistory((prev) => {
              if (prev.length > 0) {
                const last = prev[prev.length - 1];
                if (last.text === rawText) {
                  return prev;
                }
              }
              const recent = prev.slice(-3);
              if (recent.some((item) => item.text === rawText)) {
                return prev;
              }
              if (rawText.length < 2) {
                return prev;
              }

              const newEntry: AsrEntry = {
                id: `${Date.now()}_${Math.random().toString(36).substring(2, 7)}`,
                text: rawText,
                timestamp: new Date().toLocaleTimeString("ja-JP", {
                  hour: "2-digit",
                  minute: "2-digit",
                  second: "2-digit",
                }),
                isDiscord: stream === "discord",
                isPrompt,
                latencyMs,
              };
              return [...prev.slice(-49), newEntry];
            });
          }
          break;
        }
        case "gemini_response":
          setGeminiResponse(msg.text);
          break;
        case "resource_status":
          if (!isTauriEnv()) {
            setVram(msg.vram);
            setRam(msg.ram);
          }
          break;
        case "commentary_timer":
          setCommentaryTimer({
            progress: msg.progress,
            remaining: msg.remaining,
          });
          break;
        case "log":
          setLogs((prev) => {
            if (prev.length > 0) {
              const last = prev[prev.length - 1];
              if (
                last.timestamp === msg.timestamp &&
                last.message === msg.message &&
                last.logger === msg.logger
              ) {
                return prev;
              }
              const recent = prev.slice(-10);
              if (
                recent.some(
                  (l) =>
                    l.timestamp === msg.timestamp && l.message === msg.message,
                )
              ) {
                return prev;
              }
            }
            return [...prev.slice(-500), msg];
          });
          break;
        case "log_history":
          if (Array.isArray(msg.logs)) {
            setLogs((prev) => {
              const existingKeys = new Set(
                prev.map((l) => `${l.timestamp}_${l.logger}_${l.message}`),
              );
              const newEntries = msg.logs.filter(
                (l) =>
                  !existingKeys.has(`${l.timestamp}_${l.logger}_${l.message}`),
              );
              return [...prev, ...newEntries].slice(-500);
            });
          }
          break;
      }
    });

    // Tauri 起動初期ログの購読
    let disposed = false;
    const unlistenTauriLogs: Array<() => void> = [];
    const once = (unlisten: () => void): (() => void) => {
      let called = false;
      return () => {
        if (called) return;
        called = true;
        unlisten();
      };
    };
    if (isTauriEnv()) {
      void (async () => {
        try {
          const { listen } = await import("@tauri-apps/api/event");
          const remove = once(
            await listen<LogEntry>("python_startup_log", (event) => {
              if (disposed || !event.payload) return;
              setLogs((prev) => {
                const msg = event.payload;
                if (
                  prev.length > 0 &&
                  prev[prev.length - 1].message === msg.message
                )
                  return prev;
                return [...prev.slice(-500), msg];
              });
            }),
          );
          if (disposed) remove();
          else unlistenTauriLogs.push(remove);
        } catch {
          // ignore
        }
      })();
    }

    return () => {
      disposed = true;
      removeListener();
      runUnlisteners(unlistenTauriLogs);
    };
  }, [
    addListener,
    setCurrentAsr,
    setAsrHistory,
    setCommentaryTimer,
    setGeminiResponse,
    setLevelMeter,
    setRam,
    setStatus,
    setVram,
  ]);

  return {
    logs,
    toast,
    showToast,
    fetchLogs,
    clearLogs,
    setLogs,
  };
}
