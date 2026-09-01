import React from 'react';
import { Cpu } from 'lucide-react';
import { ResourceInfo } from '../../types';

interface ResourceCardProps {
  vram: ResourceInfo;
  ram: ResourceInfo;
}

export const ResourceCard: React.FC<ResourceCardProps> = ({ vram, ram }) => {
  const formatMB = (mb: number) => {
    if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
    return `${Math.round(mb)} MB`;
  };

  return (
    <div className="linear-card p-3.5 flex flex-col gap-3">
      <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wider text-[#8a8f98]">
        <Cpu className="w-3.5 h-3.5 text-[#6366f1]" />
        <span>Resource Monitor</span>
      </div>

      {/* GPU VRAM */}
      <div className="flex flex-col gap-1.5">
        <div className="flex justify-between text-[11px] font-mono">
          <span className="text-[#8a8f98]">GPU VRAM</span>
          <span className="text-[#d0d6e0]">
            {formatMB(vram.used)} / {formatMB(vram.total)} ({Math.round(vram.percent)}%)
          </span>
        </div>
        <div className="w-full bg-[#161718] h-1.5 rounded-full overflow-hidden border border-[#23252a]">
          <div
            className={`h-full transition-all duration-300 ${
              vram.percent > 90 ? 'bg-[#eb5757]' : vram.percent > 70 ? 'bg-[#e4f222]' : 'bg-[#6366f1]'
            }`}
            style={{ width: `${Math.min(100, Math.max(0, vram.percent))}%` }}
          />
        </div>
      </div>

      {/* System RAM */}
      <div className="flex flex-col gap-1.5">
        <div className="flex justify-between text-[11px] font-mono">
          <span className="text-[#8a8f98]">System RAM</span>
          <span className="text-[#d0d6e0]">
            {formatMB(ram.used)} / {formatMB(ram.total)} ({Math.round(ram.percent)}%)
          </span>
        </div>
        <div className="w-full bg-[#161718] h-1.5 rounded-full overflow-hidden border border-[#23252a]">
          <div
            className={`h-full transition-all duration-300 ${
              ram.percent > 90 ? 'bg-[#eb5757]' : ram.percent > 70 ? 'bg-[#e4f222]' : 'bg-[#02b8cc]'
            }`}
            style={{ width: `${Math.min(100, Math.max(0, ram.percent))}%` }}
          />
        </div>
      </div>
    </div>
  );
};
