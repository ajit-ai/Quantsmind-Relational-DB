import { createContext, useContext, useEffect, useState, type ReactNode } from 'react';

export type ThemeMode = 'light' | 'dark';
export type ThemeSkin = 'blue' | 'emerald' | 'rose' | 'amber' | 'violet' | 'cyan' | 'slate';

type ThemeContextValue = {
  mode: ThemeMode;
  skin: ThemeSkin;
  setMode: (m: ThemeMode) => void;
  setSkin: (s: ThemeSkin) => void;
  toggleMode: () => void;
};

const ThemeContext = createContext<ThemeContextValue | null>(null);

const STORAGE_KEY = 'quantsmind-theme';

type StoredTheme = { mode: ThemeMode; skin: ThemeSkin };

function loadStored(): StoredTheme {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) return JSON.parse(raw) as StoredTheme;
  } catch {
    /* ignore */
  }
  return { mode: 'light', skin: 'blue' };
}

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [mode, setModeState] = useState<ThemeMode>('light');
  const [skin, setSkinState] = useState<ThemeSkin>('blue');

  useEffect(() => {
    const stored = loadStored();
    setModeState(stored.mode);
    setSkinState(stored.skin);
  }, []);

  useEffect(() => {
    document.documentElement.setAttribute('data-theme-mode', mode);
    document.documentElement.setAttribute('data-theme-skin', skin);
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify({ mode, skin }));
    } catch {
      /* ignore */
    }
  }, [mode, skin]);

  const setMode = (m: ThemeMode) => setModeState(m);
  const setSkin = (s: ThemeSkin) => setSkinState(s);
  const toggleMode = () => setModeState((m) => (m === 'light' ? 'dark' : 'light'));

  return (
    <ThemeContext.Provider value={{ mode, skin, setMode, setSkin, toggleMode }}>
      {children}
    </ThemeContext.Provider>
  );
}

export function useTheme(): ThemeContextValue {
  const ctx = useContext(ThemeContext);
  if (!ctx) throw new Error('useTheme must be used within ThemeProvider');
  return ctx;
}
