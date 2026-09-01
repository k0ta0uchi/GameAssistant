import { useState, useEffect, useRef, useCallback } from 'react';
import { WsMessage } from '../types';

const isTauriEnv = () => typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

export function useWebSocket(url: string = 'ws://127.0.0.1:18080/ws') {
  const [isConnected, setIsConnected] = useState<boolean>(() => isTauriEnv());
  const [lastMessage, setLastMessage] = useState<WsMessage | null>(null);
  const listenersRef = useRef<Set<(msg: WsMessage) => void>>(new Set());
  const wsRef = useRef<WebSocket | null>(null);
  const reconnectTimeoutRef = useRef<NodeJS.Timeout | null>(null);

  const addListener = useCallback((listener: (msg: WsMessage) => void) => {
    listenersRef.current.add(listener);
    return () => {
      listenersRef.current.delete(listener);
    };
  }, []);

  const connect = useCallback(() => {
    // Tauri 環境では Rust ネイティブ IPC を使用するため Python WebSocket への接続待機は不要
    if (isTauriEnv()) {
      setIsConnected(true);
      return;
    }

    try {
      const ws = new WebSocket(url);

      ws.onopen = () => {
        setIsConnected(true);
        console.log('[WebSocket] Connected to GameAssistant backend.');
      };

      ws.onmessage = (event) => {
        try {
          const data: WsMessage = JSON.parse(event.data);
          setLastMessage(data);
          listenersRef.current.forEach((listener) => listener(data));
        } catch (err) {
          console.error('[WebSocket] Failed to parse message:', err);
        }
      };

      ws.onclose = () => {
        setIsConnected(false);
        console.log('[WebSocket] Disconnected. Retrying in 2s...');
        reconnectTimeoutRef.current = setTimeout(connect, 2000);
      };

      ws.onerror = (err) => {
        console.error('[WebSocket] Error:', err);
        ws.close();
      };

      wsRef.current = ws;
    } catch (err) {
      console.error('[WebSocket] Connection error:', err);
      reconnectTimeoutRef.current = setTimeout(connect, 2000);
    }
  }, [url]);

  useEffect(() => {
    connect();
    return () => {
      if (reconnectTimeoutRef.current) clearTimeout(reconnectTimeoutRef.current);
      if (wsRef.current) wsRef.current.close();
    };
  }, [connect]);

  const sendMessage = useCallback((msg: any) => {
    if (wsRef.current && wsRef.current.readyState === WebSocket.OPEN) {
      wsRef.current.send(typeof msg === 'string' ? msg : JSON.stringify(msg));
    }
  }, []);

  return {
    isConnected,
    lastMessage,
    addListener,
    sendMessage,
  };
}
