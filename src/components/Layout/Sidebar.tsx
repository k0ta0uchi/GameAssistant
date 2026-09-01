import React from 'react';
import { Settings, Database } from 'lucide-react';
import { ActionButtons } from '../Controls/ActionButtons';
import { AudioCard } from '../Controls/AudioCard';
import { TargetWindowCard } from '../Controls/TargetWindowCard';
import { ResourceCard } from '../Controls/ResourceCard';
import { ResourceInfo } from '../../types';

interface SidebarProps {
  sessionRunning: boolean;
  onStartSession: () => void;
  onStopSession: () => void;
  onRestartWhisper: () => void;

  inputDevices: string[];
  selectedDevice: string;
  onDeviceChange: (dev: string) => void;
  levelMeter: number;
  enableDiscordCapture: boolean;
  onToggleDiscordCapture: (enabled: boolean) => void;
  discordDevices: string[];
  selectedDiscordDevice: string;
  onDiscordDeviceChange: (dev: string) => void;

  windows: string[];
  selectedWindow: string;
  onWindowChange: (win: string) => void;
  onRefreshWindows: () => void;
  previewImage: string | null;
  onFetchPreview: () => void;

  vram: ResourceInfo;
  ram: ResourceInfo;

  onOpenSettings: () => void;
  onOpenMemory: () => void;
  isConnected: boolean;
}

export const Sidebar: React.FC<SidebarProps> = ({
  sessionRunning,
  onStartSession,
  onStopSession,
  onRestartWhisper,
  inputDevices,
  selectedDevice,
  onDeviceChange,
  levelMeter,
  enableDiscordCapture,
  onToggleDiscordCapture,
  discordDevices,
  selectedDiscordDevice,
  onDiscordDeviceChange,
  windows,
  selectedWindow,
  onWindowChange,
  onRefreshWindows,
  previewImage,
  onFetchPreview,
  vram,
  ram,
  onOpenSettings,
  onOpenMemory,
  isConnected,
}) => {
  return (
    <aside className="w-80 h-screen bg-[#08090a] border-r border-[#23252a] flex flex-col justify-between p-3.5 gap-3 overflow-y-auto shrink-0 select-none">
      <div className="flex flex-col gap-3">
        {/* アプリタイトル & 接続インジケーター */}
        <div className="flex items-center justify-between pb-2 border-b border-[#23252a]">
          <div className="flex items-center gap-2">
            <div className="w-2.5 h-2.5 rounded-full bg-[#e4f222] shadow-[0_0_8px_#e4f222]" />
            <h1 className="text-sm font-semibold tracking-wide text-white">GameAssistant</h1>
          </div>
          <div className="flex items-center gap-1.5 text-[11px] font-mono">
            <span
              className={`w-1.5 h-1.5 rounded-full ${
                isConnected ? 'bg-[#27a644]' : 'bg-[#eb5757]'
              }`}
            />
            <span className="text-[#8a8f98]">{isConnected ? 'ONLINE' : 'OFFLINE'}</span>
          </div>
        </div>

        {/* セッション操作ボタン */}
        <ActionButtons
          sessionRunning={sessionRunning}
          onStart={onStartSession}
          onStop={onStopSession}
          onRestartWhisper={onRestartWhisper}
        />

        {/* オーディオカード */}
        <AudioCard
          inputDevices={inputDevices}
          selectedDevice={selectedDevice}
          onDeviceChange={onDeviceChange}
          levelMeter={levelMeter}
          enableDiscordCapture={enableDiscordCapture}
          onToggleDiscordCapture={onToggleDiscordCapture}
          discordDevices={discordDevices}
          selectedDiscordDevice={selectedDiscordDevice}
          onDiscordDeviceChange={onDiscordDeviceChange}
        />

        {/* ターゲットウィンドウカード */}
        <TargetWindowCard
          windows={windows}
          selectedWindow={selectedWindow}
          onWindowChange={onWindowChange}
          onRefresh={onRefreshWindows}
          previewImage={previewImage}
          onFetchPreview={onFetchPreview}
        />

        {/* リソースカード */}
        <ResourceCard vram={vram} ram={ram} />
      </div>

      {/* フッターナビゲーション (Settings / Memory) */}
      <div className="pt-2 border-t border-[#23252a] grid grid-cols-2 gap-2">
        <button
          onClick={onOpenSettings}
          className="flex items-center justify-center gap-1.5 py-2 px-3 linear-btn-ghost text-xs font-medium"
        >
          <Settings className="w-3.5 h-3.5" />
          <span>Settings</span>
        </button>
        <button
          onClick={onOpenMemory}
          className="flex items-center justify-center gap-1.5 py-2 px-3 linear-btn-ghost text-xs font-medium"
        >
          <Database className="w-3.5 h-3.5" />
          <span>Memory</span>
        </button>
      </div>
    </aside>
  );
};
