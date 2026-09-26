import React from 'react';
import { Mic, Headphones, Volume2 } from 'lucide-react';

interface AudioCardProps {
  inputDevices: string[];
  selectedDevice: string;
  onDeviceChange: (device: string) => void;
  levelMeter: number; // 0 - 100
  enableDiscordCapture: boolean;
  onToggleDiscordCapture: (enabled: boolean) => void;
  discordDevices?: string[];
  selectedDiscordDevice?: string;
  onDiscordDeviceChange?: (device: string) => void;
  discordLevelMeter?: number; // 0 - 100
}

export const AudioCard: React.FC<AudioCardProps> = ({
  inputDevices,
  selectedDevice,
  onDeviceChange,
  levelMeter,
  enableDiscordCapture,
  onToggleDiscordCapture,
  discordLevelMeter = 0,
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
          <span>{levelMeter.toFixed(1)}%</span>
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
        <div className="flex items-center justify-between">
          <label className="flex items-center gap-2 cursor-pointer group">
            <Headphones className="w-3.5 h-3.5 text-[#02b8cc]" />
            <span className="text-xs text-[#d0d6e0] font-medium group-hover:text-white transition-colors">
              Capture Discord Audio
            </span>
          </label>
          <div className="flex items-center gap-2">
            {enableDiscordCapture && (
              <div className="flex items-center gap-1 text-[11px] font-mono text-[#02b8cc]">
                <Volume2 className="w-3 h-3" />
                <span>{discordLevelMeter.toFixed(1)}%</span>
              </div>
            )}
            <input
              type="checkbox"
              checked={enableDiscordCapture}
              onChange={(e) => onToggleDiscordCapture(e.target.checked)}
              className="w-4 h-4 rounded accent-[#e4f222] bg-[#08090a] border-[#383b3f] cursor-pointer"
            />
          </div>
        </div>

        {enableDiscordCapture && (
          <div className="flex flex-col gap-1.5">
            <div className="flex items-center justify-between text-[11px] bg-[#0d0e0f] px-2.5 py-1.5 rounded border border-[#23252a]">
              <span className="text-[#8a8f98] font-medium">Target Process</span>
              <div className="flex items-center gap-1.5 font-mono text-[#02b8cc]">
                <span className="w-1.5 h-1.5 rounded-full bg-[#02b8cc] animate-pulse" />
                <span>Discord.exe (Auto)</span>
              </div>
            </div>

            {/* Discord 音声レベルメーターバー */}
            <div className="w-full bg-[#161718] h-1.5 rounded-full overflow-hidden border border-[#23252a]">
              <div
                className={`h-full transition-all duration-75 ${
                  discordLevelMeter > 70
                    ? 'bg-[#eb5757]'
                    : discordLevelMeter > 30
                    ? 'bg-[#02b8cc]'
                    : 'bg-[#27a644]'
                }`}
                style={{ width: `${Math.min(100, Math.max(0, discordLevelMeter))}%` }}
              />
            </div>
          </div>
        )}
      </div>
    </div>
  );
};
