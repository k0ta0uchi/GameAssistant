import type React from "react";
import { useState, useEffect, useLayoutEffect, useRef } from "react";

export interface ContextMenuAction {
  label: string;
  onSelect: () => void;
  disabled?: boolean;
  danger?: boolean;
}

export interface ContextMenuProps {
  x: number;
  y: number;
  label: string;
  actions: ContextMenuAction[];
  onClose: () => void;
}

/**
 * A small, keyboard-complete context menu shared by the raw and semantic
 * memory views. The native context menu is suppressed only for the memory
 * row itself; Escape, outside click, and the ContextMenu/Shift+F10 keys all
 * dismiss it again.
 */
export const MemoryContextMenu: React.FC<ContextMenuProps> = ({
  x,
  y,
  label,
  actions,
  onClose,
}) => {
  const menuRef = useRef<HTMLDivElement | null>(null);
  const actionRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const [position, setPosition] = useState({ x, y });

  useLayoutEffect(() => {
    const viewportWidth =
      typeof window === "undefined" ? 1024 : window.innerWidth;
    const viewportHeight =
      typeof window === "undefined" ? 768 : window.innerHeight;
    const menuWidth = 236;
    const menuHeight = Math.min(360, Math.max(64, actions.length * 38 + 20));
    setPosition({
      x: Math.max(8, Math.min(x, viewportWidth - menuWidth - 8)),
      y: Math.max(8, Math.min(y, viewportHeight - menuHeight - 8)),
    });
  }, [actions.length, x, y]);

  useEffect(() => {
    const firstEnabled = actions.findIndex((action) => !action.disabled);
    if (firstEnabled >= 0) actionRefs.current[firstEnabled]?.focus();

    const handlePointerDown = (event: PointerEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) onClose();
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
        return;
      }
      const enabledIndexes = actions
        .map((action, index) => (action.disabled ? -1 : index))
        .filter((index) => index >= 0);
      if (
        enabledIndexes.length === 0 ||
        !["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)
      )
        return;
      event.preventDefault();
      const activeIndex = enabledIndexes.indexOf(
        actionRefs.current.findIndex(
          (button) => button === document.activeElement,
        ),
      );
      const current = activeIndex < 0 ? 0 : activeIndex;
      const next =
        event.key === "Home"
          ? 0
          : event.key === "End"
            ? enabledIndexes.length - 1
            : (current +
                (event.key === "ArrowUp" ? -1 : 1) +
                enabledIndexes.length) %
              enabledIndexes.length;
      actionRefs.current[enabledIndexes[next]]?.focus();
    };
    document.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [actions, onClose]);

  return (
    <div
      ref={menuRef}
      role="menu"
      aria-label={label}
      className="fixed z-[80] min-w-[236px] max-w-[280px] overflow-y-auto rounded-[7px] border border-[#383b3f] bg-[#161718] p-1.5 shadow-2xl shadow-black/50"
      style={{
        left: position.x,
        top: position.y,
        maxHeight: "min(360px, calc(100vh - 16px))",
      }}
      onPointerDown={(event) => event.stopPropagation()}
    >
      {actions.map((action, index) => (
        <button
          key={action.label}
          ref={(button) => {
            actionRefs.current[index] = button;
          }}
          type="button"
          role="menuitem"
          disabled={action.disabled}
          onClick={() => {
            onClose();
            if (!action.disabled) action.onSelect();
          }}
          className={`flex w-full items-center rounded-[5px] px-2.5 py-2 text-left text-xs transition-colors disabled:cursor-not-allowed disabled:opacity-35 ${
            action.danger
              ? "text-[#f87171] hover:bg-[#eb5757]/10"
              : "text-[#d0d6e0] hover:bg-[#23252a] hover:text-white"
          }`}
        >
          {action.label}
        </button>
      ))}
    </div>
  );
};
