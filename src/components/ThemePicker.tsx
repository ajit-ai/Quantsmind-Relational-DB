import { useState, useRef, useEffect } from 'react';
import { useTheme, type ThemeSkin } from '@/lib/theme';
import { Palette, Sun, Moon, Check } from 'lucide-react';

const SKINS: { id: ThemeSkin; label: string; color: string }[] = [
  { id: 'blue', label: 'Blue', color: '#2563eb' },
  { id: 'emerald', label: 'Emerald', color: '#059669' },
  { id: 'rose', label: 'Rose', color: '#e11d48' },
  { id: 'amber', label: 'Amber', color: '#d97706' },
  { id: 'violet', label: 'Violet', color: '#7c3aed' },
  { id: 'cyan', label: 'Cyan', color: '#0891b2' },
  { id: 'slate', label: 'Slate', color: '#475569' },
];

export function ThemePicker() {
  const { mode, skin, setMode, setSkin } = useTheme();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener('mousedown', onClick);
    return () => document.removeEventListener('mousedown', onClick);
  }, []);

  return (
    <div ref={ref} className="relative">
      <button
        onClick={() => setOpen((v) => !v)}
        className="flex items-center gap-1.5 rounded-md border border-base bg-surface px-2.5 py-1 text-xs text-secondary hover:bg-hover"
        title="Theme settings"
      >
        <Palette className="h-3.5 w-3.5 text-accent" />
        <span className="hidden sm:inline">Theme</span>
      </button>

      {open && (
        <div className="absolute right-0 z-50 mt-2 w-56 rounded-lg border border-base bg-surface shadow-xl">
          <div className="border-b border-base p-3">
            <div className="mb-2 text-xs font-semibold uppercase tracking-wide text-muted">
              Mode
            </div>
            <div className="flex gap-2">
              <button
                onClick={() => setMode('light')}
                className={`flex flex-1 items-center justify-center gap-1.5 rounded-md border px-3 py-1.5 text-xs font-medium transition-colors ${
                  mode === 'light'
                    ? 'border-accent bg-accent-light text-accent'
                    : 'border-base text-secondary hover:bg-hover'
                }`}
              >
                <Sun className="h-3.5 w-3.5" /> Light
              </button>
              <button
                onClick={() => setMode('dark')}
                className={`flex flex-1 items-center justify-center gap-1.5 rounded-md border px-3 py-1.5 text-xs font-medium transition-colors ${
                  mode === 'dark'
                    ? 'border-accent bg-accent-light text-accent'
                    : 'border-base text-secondary hover:bg-hover'
                }`}
              >
                <Moon className="h-3.5 w-3.5" /> Dark
              </button>
            </div>
          </div>

          <div className="p-3">
            <div className="mb-2 text-xs font-semibold uppercase tracking-wide text-muted">
              Accent Color
            </div>
            <div className="grid grid-cols-4 gap-2">
              {SKINS.map((s) => (
                <button
                  key={s.id}
                  onClick={() => setSkin(s.id)}
                  className={`group flex flex-col items-center gap-1 rounded-md border p-2 transition-colors ${
                    skin === s.id
                      ? 'border-accent bg-accent-light'
                      : 'border-base hover:bg-hover'
                  }`}
                >
                  <span
                    className="flex h-6 w-6 items-center justify-center rounded-full"
                    style={{ backgroundColor: s.color }}
                  >
                    {skin === s.id && <Check className="h-3.5 w-3.5 text-white" />}
                  </span>
                  <span className="text-[10px] text-secondary">{s.label}</span>
                </button>
              ))}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
