import { useState, useEffect, useCallback, useRef } from 'react';
import { SystemStatus, ResourceInfo, WsMessage, LogEntry, PromptItem, AsrEntry } from '../types';
import { useWebSocket } from './useWebSocket';

const API_BASE = 'http://127.0.0.1:18080';

// Tauri 環境かどうかの判定
const isTauriEnv = () => typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

// アプリ起動中の一意実行フラグ (React StrictMode の二重マウント防止)
let globalWarmupTriggered = false;

export function useAppState() {
  const { isConnected, addListener } = useWebSocket();

  // プロンプト設定
  const [prompts, setPrompts] = useState<PromptItem[]>([]);

  // システム状態
  const [status, setStatus] = useState<SystemStatus>({
    asr: false,
    gemini: false,
    tts: false,
    twitch: false,
    session: false,
  });

  // 音声レベル
  const [levelMeter, setLevelMeter] = useState<number>(0);

  // 音声認識 (ASR)
  const [currentAsr, setCurrentAsr] = useState<{ text: string; isFinal: boolean; isPrompt?: boolean }>({
    text: '',
    isFinal: true,
    isPrompt: false,
  });
  const [asrHistory, setAsrHistory] = useState<AsrEntry[]>([]);

  // Gemini 回答
  const [geminiResponse, setGeminiResponse] = useState<string>('');

  // リソースモニター
  const [vram, setVram] = useState<ResourceInfo>({ used: 0, total: 0, percent: 0 });
  const [ram, setRam] = useState<ResourceInfo>({ used: 0, total: 0, percent: 0 });

  // 自動ツッコミタイマー
  const [commentaryTimer, setCommentaryTimer] = useState<{ progress: number; remaining: number }>({
    progress: 0,
    remaining: 0,
  });

  // トースト通知状態
  const [toast, setToast] = useState<{ id: string; message: string; type: 'success' | 'info' | 'warning' } | null>(null);
  const toastTimerRef = useRef<NodeJS.Timeout | null>(null);

  const showToast = useCallback((message: string, type: 'success' | 'info' | 'warning' = 'success') => {
    if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
    setToast({ id: Math.random().toString(36).substring(2, 9), message, type });
    toastTimerRef.current = setTimeout(() => {
      setToast(null);
    }, 4000);
  }, []);

  // リアルタイムログ
  const [logs, setLogs] = useState<LogEntry[]>([]);

  // デバイス & ウィンドウ設定
  const [inputDevices, setInputDevices] = useState<string[]>(['Default (System Default)']);
  const [discordDevices, setDiscordDevices] = useState<string[]>(['Auto (Discord App / System Loopback)']);
  const [selectedDevice, setSelectedDevice] = useState<string>('Default (System Default)');
  const [selectedDiscordDevice, setSelectedDiscordDevice] = useState<string>('Auto (Discord App / System Loopback)');
  const [enableDiscordCapture, setEnableDiscordCapture] = useState<boolean>(false);

  const [windows, setWindows] = useState<string[]>([]);
  const [selectedWindow, setSelectedWindow] = useState<string>('');
  const [previewImage, setPreviewImage] = useState<string>('');

  // 設定オブジェクト
  const [settings, setSettings] = useState<Record<string, any>>({});

  const micActiveTimerRef = useRef<NodeJS.Timeout | null>(null);

  // -------------------------------------------------------------
  // Tauri イベントリスナー初期化 (Rust Native イベント)
  // -------------------------------------------------------------
  useEffect(() => {
    let unlistenAll: (() => void) | null = null;
    let isCancelled = false;

    import('@tauri-apps/api/event').then(async ({ listen }) => {
      if (isCancelled) return;

      const unlistenFns: (() => void)[] = [];

      try {
        const u1 = await listen<{ ram: ResourceInfo; vram: ResourceInfo }>('resource_status', (event) => {
          if (event.payload) {
            setRam(event.payload.ram);
            setVram(event.payload.vram);
          }
        });
        unlistenFns.push(u1);

        const u2 = await listen<number>('level_meter', (event) => {
          if (typeof event.payload === 'number') {
            const lvl = event.payload;
            setLevelMeter(lvl);
            if (lvl > 0.012) {
              setStatus((prev) => (prev.asr ? prev : { ...prev, asr: true }));
              if (micActiveTimerRef.current) clearTimeout(micActiveTimerRef.current);
              micActiveTimerRef.current = setTimeout(() => {
                setStatus((prev) => ({ ...prev, asr: false }));
              }, 450);
            }
          }
        });
        unlistenFns.push(u2);

        const u3 = await listen<{ text: string; is_final: boolean; is_prompt?: boolean; stream?: string }>('asr_result', (event) => {
          if (event.payload && event.payload.text) {
            const rawText = event.payload.text.trim();
            const isPrompt = !!event.payload.is_prompt;
            const isDiscord = event.payload.stream === 'discord' || rawText.startsWith('[Discord]');

            setStatus((prev) => (prev.asr ? prev : { ...prev, asr: true }));
            if (micActiveTimerRef.current) clearTimeout(micActiveTimerRef.current);
            micActiveTimerRef.current = setTimeout(() => {
              setStatus((prev) => ({ ...prev, asr: false }));
            }, event.payload.is_final ? 350 : 800);

            if (rawText) {
              if (event.payload.is_final) {
                setAsrHistory((prev) => {
                  const lastIndex = prev.length - 1;
                  const last = prev[lastIndex];
                  if (last && (last.text === rawText || rawText.includes(last.text) || last.text.includes(rawText))) {
                    const updated = [...prev];
                    updated[lastIndex] = {
                      ...last,
                      text: rawText,
                      isPrompt: isPrompt || last.isPrompt,
                      isDiscord: isDiscord || last.isDiscord,
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
                    },
                  ];
                });
                setCurrentAsr({ text: '', isFinal: true, isPrompt: false });
              } else {
                setCurrentAsr({ text: rawText, isFinal: false, isPrompt });
              }
            }
          }
        });
        unlistenFns.push(u3);

        const u4 = await listen<{ is_running: boolean; remaining_sec: number; total_sec: number }>('auto_commentary_status', (event) => {
          if (event.payload) {
            const total = event.payload.total_sec || 1;
            const remaining = event.payload.remaining_sec || 0;
            const progress = Math.min(100, Math.max(0, ((total - remaining) / total) * 100));
            setCommentaryTimer({
              progress,
              remaining,
            });
          }
        });
        unlistenFns.push(u4);

        const u5 = await listen<{ type: string; author: string; content: string; timestamp: string }>('session-event', (event) => {
          if (event.payload) {
            const { type, content } = event.payload;
            if (type === 'ai_response' || type === 'auto_commentary') {
              setGeminiResponse(content);
            }
          }
        });
        unlistenFns.push(u5);

        const uGemini = await listen<{ is_generating: boolean }>('gemini_status', (event) => {
          if (event.payload) {
            setStatus((prev) => ({ ...prev, gemini: !!event.payload.is_generating }));
          }
        });
        unlistenFns.push(uGemini);

        const uTts = await listen<{ is_playing: boolean }>('tts_status', (event) => {
          if (event.payload) {
            setStatus((prev) => ({ ...prev, tts: !!event.payload.is_playing }));
          }
        });
        unlistenFns.push(uTts);

        const uTwitch = await listen<{ connected: boolean }>('twitch_status', (event) => {
          if (event.payload) {
            setStatus((prev) => ({ ...prev, twitch: !!event.payload.connected }));
          }
        });
        unlistenFns.push(uTwitch);

        const uToast = await listen<{ message: string; type?: 'success' | 'info' | 'warning' }>('toast_notice', (event) => {
          if (event.payload?.message) {
            showToast(event.payload.message, event.payload.type || 'info');
          }
        });
        unlistenFns.push(uToast);

        const u6 = await listen<LogEntry>('app_log', (event) => {
          if (event.payload) {
            setLogs((prev) => [...prev.slice(-999), event.payload]);
          }
        });
        unlistenFns.push(u6);

        // Twitch 初期接続状態チェック
        import('@tauri-apps/api/core').then(({ invoke }) => {
          invoke<{ connected: boolean }>('twitch_get_status').then((res) => {
            if (res && res.connected) {
              setStatus((prev) => ({ ...prev, twitch: true }));
            }
          }).catch(() => {});
        });

        if (isCancelled) {
          unlistenFns.forEach((fn) => fn());
        } else {
          unlistenAll = () => unlistenFns.forEach((fn) => fn());
        }
      } catch (err) {
        console.warn('Event listener registration error:', err);
      }
    });

    return () => {
      isCancelled = true;
      if (micActiveTimerRef.current) {
        clearTimeout(micActiveTimerRef.current);
      }
      if (unlistenAll) {
        unlistenAll();
      }
    };
  }, []);

  // WebSocket メッセージ受信ハンドラ
  useEffect(() => {
    const removeListener = addListener((msg: WsMessage) => {
      switch (msg.type) {
        case 'status':
          setStatus(msg.status);
          break;
        case 'level_meter':
          setLevelMeter(msg.level);
          break;
        case 'asr': {
          const isFinal = Boolean(msg.is_final);
          const rawText = (msg.text || '').trim();
          if (!rawText) break;

          setCurrentAsr({ text: rawText, isFinal });

          if (isFinal) {
            setAsrHistory((prev) => {
              // 1. 直前の履歴と完全一致する場合は重複として追加しない
              if (prev.length > 0) {
                const last = prev[prev.length - 1];
                if (last.text === rawText) {
                  return prev;
                }
              }
              // 2. 直近3件に同一テキストがある場合も重複防止
              const recent = prev.slice(-3);
              if (recent.some((item) => item.text === rawText)) {
                return prev;
              }
              // 3. 1文字以下の極小ノイズは除外
              if (rawText.length < 2) {
                return prev;
              }

              const newEntry: AsrEntry = {
                id: `${Date.now()}_${Math.random().toString(36).substring(2, 7)}`,
                text: rawText,
                timestamp: new Date().toLocaleTimeString('ja-JP', { hour: '2-digit', minute: '2-digit', second: '2-digit' }),
                isDiscord: rawText.startsWith('[Discord]'),
              };
              return [...prev.slice(-49), newEntry];
            });
          }
          break;
        }
        case 'gemini_response':
          setGeminiResponse(msg.text);
          break;
        case 'resource_status':
          if (!isTauriEnv()) {
            setVram(msg.vram);
            setRam(msg.ram);
          }
          break;
        case 'commentary_timer':
          setCommentaryTimer({ progress: msg.progress, remaining: msg.remaining });
          break;
        case 'log':
          setLogs((prev) => {
            if (prev.length > 0) {
              const last = prev[prev.length - 1];
              // 直前のログと同一（タイムスタンプ、メッセージ、ロガーが同一）の場合は重複として無視
              if (last.timestamp === msg.timestamp && last.message === msg.message && last.logger === msg.logger) {
                return prev;
              }
              // 直近10件内に同一タイムスタンプ＋同一メッセージがあれば無視
              const recent = prev.slice(-10);
              if (recent.some((l) => l.timestamp === msg.timestamp && l.message === msg.message)) {
                return prev;
              }
            }
            return [...prev.slice(-500), msg];
          });
          break;
        case 'log_history':
          if (Array.isArray(msg.logs)) {
            setLogs((prev) => {
              const existingKeys = new Set(prev.map((l) => `${l.timestamp}_${l.logger}_${l.message}`));
              const newEntries = msg.logs.filter((l) => !existingKeys.has(`${l.timestamp}_${l.logger}_${l.message}`));
              return [...prev, ...newEntries].slice(-500);
            });
          }
          break;
      }
    });

    // Tauri 起動初期ログの購読
    let unlistenTauriLogs: (() => void) | null = null;
    if (isTauriEnv()) {
      import('@tauri-apps/api/event').then(({ listen }) => {
        listen<LogEntry>('python_startup_log', (event) => {
          if (event.payload) {
            setLogs((prev) => {
              const msg = event.payload;
              if (prev.length > 0) {
                const last = prev[prev.length - 1];
                if (last.message === msg.message) {
                  return prev;
                }
              }
              return [...prev.slice(-500), msg];
            });
          }
        }).then((unlisten) => {
          unlistenTauriLogs = unlisten;
        });
      });
    }

    return () => {
      removeListener();
      if (unlistenTauriLogs) unlistenTauriLogs();
    };
  }, [addListener]);

  // 1. プレビュー取得（重複・連打防止ガード付き）
  const isFetchingPreviewRef = useRef(false);
  const fetchPreview = useCallback(async (targetWindowName?: string) => {
    const win = targetWindowName || selectedWindowRef.current;
    if (!win || isFetchingPreviewRef.current) return;
    isFetchingPreviewRef.current = true;
    try {
      if (isTauriEnv()) {
        const { invoke } = await import('@tauri-apps/api/core');
        const preview = await invoke<string | null>('capture_window_preview', { title: win });
        if (preview) {
          setPreviewImage(preview);
          return;
        }
      }
      const res = await fetch(`${API_BASE}/api/capture/preview`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ window: win }),
      });
      const data = await res.json();
      if (data.success && data.image) {
        setPreviewImage(data.image);
      }
    } catch (e) {
      console.error('Failed to fetch preview:', e);
    } finally {
      isFetchingPreviewRef.current = false;
    }
  }, []);

  const selectedWindowRef = useRef(selectedWindow);
  useEffect(() => {
    selectedWindowRef.current = selectedWindow;
  }, [selectedWindow]);

  // 2. 設定取得 & 復元（最優先）
  const fetchSettings = useCallback(async () => {
    try {
      let loaded: Record<string, any> | null = null;
      if (isTauriEnv()) {
        try {
          const { invoke } = await import('@tauri-apps/api/core');
          loaded = await invoke<Record<string, any>>('load_settings');
        } catch (e) {
          console.error('Tauri load_settings error:', e);
        }
      }

      if (!loaded || Object.keys(loaded).length === 0) {
        const res = await fetch(`${API_BASE}/api/settings`);
        loaded = await res.json();
      }

      if (loaded && Object.keys(loaded).length > 0) {
        setSettings(loaded);
        if (loaded.enable_discord_capture !== undefined) {
          setEnableDiscordCapture(Boolean(loaded.enable_discord_capture));
        }
        if (loaded.audio_device) {
          setSelectedDevice(loaded.audio_device);
        }
        if (loaded.discord_audio_device) {
          setSelectedDiscordDevice(loaded.discord_audio_device);
        }
        if (loaded.window) {
          setSelectedWindow(loaded.window);
        }

        return loaded;
      }
    } catch (e) {
      console.error('Failed to fetch settings:', e);
    }
    return null;
  }, [fetchPreview]);

  // 3. デバイス一覧取得（既存の設定値を保護）
  const fetchDevices = useCallback(async () => {
    try {
      if (isTauriEnv()) {
        const { invoke } = await import('@tauri-apps/api/core');
        const audioData = await invoke<{ input_devices: string[]; default_device: string | null }>('list_audio_devices');
        if (audioData && audioData.input_devices.length > 0) {
          setInputDevices(audioData.input_devices);
          setDiscordDevices(['Auto (Discord App / System Loopback)', ...audioData.input_devices]);
          setSelectedDevice((prev) => {
            if (prev) return prev;
            return audioData.default_device || audioData.input_devices[0];
          });
          setSelectedDiscordDevice((prev) => prev || 'Auto (Discord App / System Loopback)');
          return;
        }
      }

      const res = await fetch(`${API_BASE}/api/devices`);
      const data = await res.json();
      const inputs = data.input_devices || [];
      setInputDevices(inputs);
      setDiscordDevices(data.discord_devices || []);
      setSelectedDevice((prev) => {
        if (prev) return prev;
        return data.selected_device || (inputs.length > 0 ? inputs[0] : '');
      });
      setSelectedDiscordDevice((prev) => prev || data.selected_discord_device || 'Auto (Discord App / System Loopback)');
    } catch (e) {
      console.error('Failed to fetch devices:', e);
    }
  }, []);

  // 4. ウィンドウ一覧取得
  const fetchWindows = useCallback(async () => {
    try {
      if (isTauriEnv()) {
        const { invoke } = await import('@tauri-apps/api/core');
        const winList = await invoke<string[]>('list_windows');
        if (winList && winList.length > 0) {
          setWindows(winList);
          setSelectedWindow((prev) => {
            const target = prev && winList.includes(prev) ? prev : winList[0];
            return target;
          });
          return;
        }
      }
      const res = await fetch(`${API_BASE}/api/windows`);
      const data = await res.json();
      const winList = data.windows || [];
      setWindows(winList);
      setSelectedWindow((prev) => {
        const target = prev || data.selected_window || (winList.length > 0 ? winList[0] : '');
        return target;
      });
    } catch (e) {
      console.error('Failed to fetch windows:', e);
    }
  }, []);

  const fetchPrompts = useCallback(async () => {
    try {
      if (isTauriEnv()) {
        const { invoke } = await import('@tauri-apps/api/core');
        const promptList = await invoke<PromptItem[]>('get_prompts');
        if (promptList && Array.isArray(promptList)) {
          setPrompts(promptList);
          return;
        }
      }
      const res = await fetch(`${API_BASE}/api/prompts`);
      const data = await res.json();
      if (data.prompts) setPrompts(data.prompts);
    } catch (e) {
      console.error('Failed to fetch prompts:', e);
    }
  }, []);

  const fetchLogs = useCallback(async () => {
    try {
      if (isTauriEnv()) {
        const { invoke } = await import('@tauri-apps/api/core');
        const rustLogs = await invoke<LogEntry[]>('get_app_logs');
        if (rustLogs && Array.isArray(rustLogs)) {
          setLogs(rustLogs.slice(-500));
          return;
        }
      }
      const res = await fetch(`${API_BASE}/api/logs`);
      const data = await res.json();
      if (data.success && Array.isArray(data.logs)) {
        setLogs((prev) => {
          const existingKeys = new Set(prev.map((l) => `${l.timestamp}_${l.message}`));
          const newEntries = (data.logs as LogEntry[]).filter(
            (l) => !existingKeys.has(`${l.timestamp}_${l.message}`)
          );
          return [...prev, ...newEntries].slice(-500);
        });
      }
    } catch (e) {
      // ignore
    }
  }, []);

  const fetchAllData = useCallback(async () => {
    await fetchSettings();
    await Promise.all([fetchDevices(), fetchWindows(), fetchPrompts()]);
    fetchLogs();

    // GUI 画面の初期描画完了後に、バックグラウンドで ASR ウォームアップを非同期トリガー（完全一意実行）
    if (isTauriEnv() && !globalWarmupTriggered) {
      globalWarmupTriggered = true;
      setTimeout(() => {
        import('@tauri-apps/api/core').then(({ invoke }) => {
          invoke<string>('warmup_asr')
            .then(() => {
              showToast('⚡ Kotoba-Whisper GPU (CUDA INT8) のウォームアップが完了しました！', 'success');
            })
            .catch((err) => console.warn('Warmup trigger error:', err));
        });
      }, 500);
    }
  }, [fetchSettings, fetchDevices, fetchWindows, fetchPrompts, fetchLogs, showToast]);

  useEffect(() => {
    fetchAllData();
  }, []);

  // 選択中ウィンドウのプレビュー初回取得
  const lastFetchedWinRef = useRef<string>('');
  useEffect(() => {
    if (selectedWindow && selectedWindow !== lastFetchedWinRef.current) {
      lastFetchedWinRef.current = selectedWindow;
      fetchPreview(selectedWindow);
    }
  }, [selectedWindow, fetchPreview]);

  // アクション
  const startSession = async () => {
    setStatus((prev) => ({ ...prev, session: true }));
    try {
      if (isTauriEnv()) {
        const { invoke } = await import('@tauri-apps/api/core');
        await invoke('session_start');
        return;
      }
      const res = await fetch(`${API_BASE}/api/session/start`, { method: 'POST' });
      const data = await res.json();
      if (!data.success) {
        setStatus((prev) => ({ ...prev, session: false }));
      }
    } catch (e) {
      console.error('Failed to start session:', e);
      setStatus((prev) => ({ ...prev, session: false }));
    }
  };

  const stopSession = async () => {
    setStatus((prev) => ({ ...prev, session: false, gemini: false, tts: false }));
    try {
      if (isTauriEnv()) {
        const { invoke } = await import('@tauri-apps/api/core');
        await invoke('session_stop');
        return;
      }
      await fetch(`${API_BASE}/api/session/stop`, { method: 'POST' });
    } catch (e) {
      console.error('Failed to stop session:', e);
    }
  };

  const restartWhisper = async () => {
    try {
      showToast('🔄 Whisper エンジンを再起動しています...', 'info');
      const { invoke } = await import('@tauri-apps/api/core');
      await invoke('restart_whisper');
      showToast('✅ Whisper エンジンの再起動とウォームアップが完了しました！', 'success');
    } catch (e) {
      console.error('Failed to restart whisper:', e);
      showToast(`Whisper 再起動エラー: ${e}`, 'warning');
    }
  };

  const updateSetting = async (key: string, value: any) => {
    // 1. ローカルステート即時更新（UIの遅延ゼロ）
    setSettings((prev) => ({ ...prev, [key]: value }));
    if (key === 'enable_discord_capture') setEnableDiscordCapture(value);
    if (key === 'audio_device') setSelectedDevice(value);
    if (key === 'discord_audio_device') setSelectedDiscordDevice(value);
    if (key === 'window') {
      setSelectedWindow(value);
    }

    // 2. Tauri Rust 経由で settings.json へ即時書き込み
    if (isTauriEnv()) {
      try {
        const { invoke } = await import('@tauri-apps/api/core');
        await invoke('save_setting', { key, value });
      } catch (te) {
        console.error('Tauri save_setting error:', te);
      }
    }

    // 3. Python サーバー経由で settings.json へ即時書き込み & サービス反映
    try {
      await fetch(`${API_BASE}/api/settings`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ key, value }),
      });
    } catch (e) {
      console.error(`Failed to update setting ${key}:`, e);
    }
  };

  const savePrompt = async (id: string, value: string): Promise<boolean> => {
    try {
      if (isTauriEnv()) {
        const { invoke } = await import('@tauri-apps/api/core');
        const updatedList = await invoke<PromptItem[]>('save_prompt', { id, value });
        if (updatedList && Array.isArray(updatedList)) {
          setPrompts(updatedList);
          showToast('✅ プロンプト設定を保存しました', 'success');
          return true;
        }
      }
      const res = await fetch(`${API_BASE}/api/prompts`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ id, value }),
      });
      const data = await res.json();
      if (data.success && data.prompts) {
        setPrompts(data.prompts);
        return true;
      }
      return false;
    } catch (e) {
      console.error(`Failed to save prompt ${id}:`, e);
      return false;
    }
  };

  const resetPrompt = async (id: string): Promise<boolean> => {
    try {
      if (isTauriEnv()) {
        const { invoke } = await import('@tauri-apps/api/core');
        const updatedList = await invoke<PromptItem[]>('reset_prompt', { id });
        if (updatedList && Array.isArray(updatedList)) {
          setPrompts(updatedList);
          showToast('🔄 プロンプトを初期デフォルトに戻しました', 'info');
          return true;
        }
      }
      const res = await fetch(`${API_BASE}/api/prompts/reset`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ id }),
      });
      const data = await res.json();
      if (data.success && data.prompts) {
        setPrompts(data.prompts);
        return true;
      }
      return false;
    } catch (e) {
      console.error(`Failed to reset prompt ${id}:`, e);
      return false;
    }
  };

  const clearLogs = () => {
    setLogs([]);
    if (isTauriEnv()) {
      import('@tauri-apps/api/core').then(({ invoke }) => {
        invoke('clear_app_logs').catch(() => {});
      }).catch(() => {});
    }
  };

  return {
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
    fetchWindows,
    fetchPreview,
    fetchSettings,
    fetchPrompts,
    savePrompt,
    resetPrompt,
    clearLogs,
    toast,
    showToast,
  };
}
