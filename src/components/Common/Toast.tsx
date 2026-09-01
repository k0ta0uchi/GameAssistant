import React from 'react';
import { Sparkles, CheckCircle2, AlertTriangle, X } from 'lucide-react';

interface ToastProps {
  toast: {
    id: string;
    message: string;
    type: 'success' | 'info' | 'warning';
  } | null;
  onClose?: () => void;
}

export const Toast: React.FC<ToastProps> = ({ toast, onClose }) => {
  if (!toast) return null;

  const isSuccess = toast.type === 'success';
  const isWarning = toast.type === 'warning';

  return (
    <div className="fixed top-5 right-5 z-50 flex items-center gap-3 px-4 py-3 rounded-lg border bg-[#101214]/95 backdrop-blur-md shadow-2xl transition-all duration-300 animate-in fade-in slide-in-from-top-4 border-[#27a644]/30 text-[#f3f4f6]">
      <div className="shrink-0">
        {isSuccess ? (
          <div className="p-1 rounded-full bg-[#162d1e] text-[#4ade80] border border-[#22c55e]/20">
            <Sparkles className="w-4 h-4" />
          </div>
        ) : isWarning ? (
          <div className="p-1 rounded-full bg-[#2e260e] text-[#e4f222] border border-[#e4f222]/30">
            <AlertTriangle className="w-4 h-4" />
          </div>
        ) : (
          <div className="p-1 rounded-full bg-[#0d233a] text-[#38bdf8] border border-[#38bdf8]/30">
            <CheckCircle2 className="w-4 h-4" />
          </div>
        )}
      </div>

      <div className="text-xs font-medium tracking-wide leading-relaxed pr-2">
        {toast.message}
      </div>

      {onClose && (
        <button
          onClick={onClose}
          className="shrink-0 text-[#8a8f98] hover:text-[#f3f4f6] transition-colors p-0.5 rounded hover:bg-[#202228]"
        >
          <X className="w-3.5 h-3.5" />
        </button>
      )}
    </div>
  );
};
