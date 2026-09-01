import React from 'react';
import { Mic, Headphones, Volume2 } from 'lucide-react';

interface AudioCardProps {
  inputDevices: string[];
  selectedDevice: string;
  onDeviceChange: (device: string) => void;
  levelMeter: number; // 0 - 100
  enableDiscordCapture: boolean;
  onToggleDiscordCapture: (enabled: boolean) => void;
  discordDevices: string[];
  selectedDiscordDevice: string;
  onDiscordDeviceChange: (device: string) => void;
}

export const AudioCard: React.FC<AudioCardProps> = ({
  inputDevices,
  selectedDevice,
  onDeviceChange,
  levelMeter,
  enableDiscordCapture,
  onToggleDiscordCapture,
  discordDevices,
  selectedDiscordDevice,
  onDiscordDeviceChange,
}) => {
  return (
    <div className="linear-card p-3.5 flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wider text-[#8a8f98]">
          <Mic className="w-3.5 h-3.5 text-[#e4f222]" />
          <span>Audio Input</span>
        </div>
        <div className="flex items-center gap-1 text-[11px] font-mono text-[#8a8f98]">
          <Volume2 className="w-3 h-3" />
          <span>{levelMeter}%</span>
        </div>
      </div>

      {/* マイクデバイス選択 */}
      <div>
        <label className="block text-[11px] text-[#8a8f98] mb-1 font-medium">Microphone</label>
        <select
          value={selectedDevice}
          onChange={(e) => onDeviceChange(e.target.value)}
          className="w-full text-xs linear-input py-1.5 px-2 bg-[#0f1011] text-[#d0d6e0] cursor-pointer"
        >
          {inputDevices.length === 0 ? (
            <option value="" className="bg-[#161718] text-[#8a8f98]">(No devices found)</option>
          ) : (
            inputDevices.map((dev) => (
              <option key={dev} value={dev} className="bg-[#161718] text-[#d0d6e0]">
                {dev}
              </option>
            ))
          )}
        </select>
      </div>

      {/* 音声レベルメーターバー */}
      <div className="w-full bg-[#161718] h-1.5 rounded-full overflow-hidden border border-[#23252a]">
        <div
          className={`h-full transition-all duration-75 ${
            levelMeter > 70 ? 'bg-[#eb5757]' : levelMeter > 30 ? 'bg-[#e4f222]' : 'bg-[#27a644]'
          }`}
          style={{ width: `${Math.min(100, Math.max(0, levelMeter))}%` }}
        />
      </div>

      {/* Discord キャプチャ設定 */}
      <div className="pt-2 border-t border-[#23252a] flex flex-col gap-2">
        <label className="flex items-center justify-between cursor-pointer group">
          <div className="flex items-center gap-2">
            <Headphones className="w-3.5 h-3.5 text-[#02b8cc]" />
            <span className="text-xs text-[#d0d6e0] font-medium group-hover:text-white transition-colors">
              Capture Discord Audio
            </span>
          </div>
          <input
            type="checkbox"
            checked={enableDiscordCapture}
            onChange={(e) => onToggleDiscordCapture(e.target.checked)}
            className="w-4 h-4 rounded accent-[#e4f222] bg-[#08090a] border-[#383b3f] cursor-pointer"
          />
        </label>

        {enableDiscordCapture && (
          <div className="pl-5">
            <select
              value={selectedDiscordDevice}
              onChange={(e) => onDiscordDeviceChange(e.target.value)}
              className="w-full text-[11px] linear-input py-1 px-2 bg-[#0f1011] text-[#d0d6e0] cursor-pointer"
            >
              {discordDevices.map((dev) => (
                <option key={dev} value={dev} className="bg-[#161718] text-[#d0d6e0]">
                  {dev}
                </option>
              ))}
            </select>
          </div>
        )}
      </div>
    </div>
  );
};
