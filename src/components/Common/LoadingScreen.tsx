import React, { useEffect, useState } from 'react';
import './LoadingScreen.css';

interface LoadingScreenProps {
  isConnected: boolean;
  onReady?: () => void;
}

const INIT_MESSAGES = [
  '起動しています...',
  'モデルを準備中...',
  '接続待機中...',
];

export const LoadingScreen: React.FC<LoadingScreenProps> = ({ isConnected, onReady }) => {
  const [msgIndex, setMsgIndex] = useState(0);
  const [progress, setProgress] = useState(25);
  const [isDone, setIsDone] = useState(false);
  const [shouldRender, setShouldRender] = useState(true);

  // React マウント時に HTML インラインプレローダーを消去
  useEffect(() => {
    const startupLoader = document.getElementById('app-startup-loader');
    if (startupLoader) {
      startupLoader.classList.add('loaded');
      setTimeout(() => {
        startupLoader.remove();
      }, 300);
    }
  }, []);

  // 接続待機メッセージの更新
  useEffect(() => {
    if (!isConnected) {
      const timer = setInterval(() => {
        setMsgIndex((prev) => (prev < INIT_MESSAGES.length - 1 ? prev + 1 : prev));
        setProgress((prev) => Math.min(prev + 20, 75));
      }, 800);
      return () => clearInterval(timer);
    } else {
      // 接続完了時: 即座に100%にしてメインUIへ高速フェードアウト
      setProgress(100);
      const doneTimer = setTimeout(() => {
        setIsDone(true);
        if (onReady) onReady();
      }, 100);

      const removeTimer = setTimeout(() => {
        setShouldRender(false);
      }, 300);

      return () => {
        clearTimeout(doneTimer);
        clearTimeout(removeTimer);
      };
    }
  }, [isConnected, onReady]);

  if (!shouldRender) {
    return null;
  }

  return (
    <div className={`loading-screen-overlay ${isDone ? 'fade-out' : ''}`}>
      <div className="loading-content">
        <div className="loading-spinner" />
        <div className="loading-app-name">GameAssistant</div>
        <div className="loading-status-text">
          {INIT_MESSAGES[msgIndex]}
        </div>
        <div className="loading-line-container">
          <div className="loading-line-fill" style={{ width: `${progress}%` }} />
        </div>
      </div>
    </div>
  );
};
