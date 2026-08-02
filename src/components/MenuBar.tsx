import { useState, useRef, useEffect } from 'react';
import {
  File, Edit, Play, Eye, HelpCircle, ChevronDown,
  FolderOpen, Save, Download, FilePlus, FileText, Printer,
  Undo2, Redo2, Scissors, Copy, ClipboardPaste, Search, Replace,
  Trash2, CheckSquare, Square,
  PlayCircle, PlaySquare, StopCircle, RefreshCw, Code2,
  PanelLeft, PanelRight, Maximize2, Columns,
  Keyboard, Info, Github, BookOpen, ShieldCheck,
} from 'lucide-react';

export type MenuAction =
  | 'new-query' | 'open-file' | 'save-query' | 'save-as' | 'export-sql' | 'export-csv' | 'print'
  | 'undo' | 'redo' | 'cut' | 'copy' | 'paste' | 'find' | 'replace' | 'delete' | 'select-all' | 'format-sql'
  | 'run-all' | 'run-selection' | 'run-statement' | 'commit' | 'rollback' | 'toggle-autocommit' | 'refresh'
  | 'toggle-sidebar' | 'toggle-saved' | 'toggle-theme' | 'zoom-in' | 'zoom-out' | 'reset-zoom'
  | 'shortcuts' | 'about' | 'docs' | 'acid-demo';

type MenuItem = {
  label?: string;
  action?: MenuAction;
  shortcut?: string;
  separator?: boolean;
  icon?: React.ComponentType<{ className?: string }>;
  disabled?: boolean;
};

type Menu = { label: string; icon: React.ComponentType<{ className?: string }>; items: MenuItem[] };

type Props = {
  onAction: (action: MenuAction) => void;
  autoCommit: boolean;
  inTransaction: boolean;
  hasSelection: boolean;
  hasSql: boolean;
};

export function MenuBar({ onAction, autoCommit, inTransaction, hasSelection, hasSql }: Props) {
  const [openMenu, setOpenMenu] = useState<string | null>(null);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpenMenu(null);
      }
    };
    document.addEventListener('mousedown', onClick);
    return () => document.removeEventListener('mousedown', onClick);
  }, []);

  const menus: Menu[] = [
    {
      label: 'File',
      icon: File,
      items: [
        { label: 'New Query', action: 'new-query', shortcut: 'Ctrl+N', icon: FilePlus },
        { label: 'Open File…', action: 'open-file', shortcut: 'Ctrl+O', icon: FolderOpen },
        { label: 'Save Query', action: 'save-query', shortcut: 'Ctrl+S', icon: Save },
        { label: 'Save As…', action: 'save-as', icon: FileText },
        { separator: true },
        { label: 'Export SQL…', action: 'export-sql', icon: Download },
        { label: 'Export Results as CSV', action: 'export-csv', icon: Download },
        { separator: true },
        { label: 'Print', action: 'print', shortcut: 'Ctrl+P', icon: Printer },
      ],
    },
    {
      label: 'Edit',
      icon: Edit,
      items: [
        { label: 'Undo', action: 'undo', shortcut: 'Ctrl+Z', icon: Undo2 },
        { label: 'Redo', action: 'redo', shortcut: 'Ctrl+Y', icon: Redo2 },
        { separator: true },
        { label: 'Cut', action: 'cut', shortcut: 'Ctrl+X', icon: Scissors },
        { label: 'Copy', action: 'copy', shortcut: 'Ctrl+C', icon: Copy },
        { label: 'Paste', action: 'paste', shortcut: 'Ctrl+V', icon: ClipboardPaste },
        { separator: true },
        { label: 'Find…', action: 'find', shortcut: 'Ctrl+F', icon: Search },
        { label: 'Replace…', action: 'replace', shortcut: 'Ctrl+H', icon: Replace },
        { separator: true },
        { label: 'Select All', action: 'select-all', shortcut: 'Ctrl+A', icon: CheckSquare },
        { label: 'Delete', action: 'delete', icon: Trash2 },
        { separator: true },
        { label: 'Format SQL', action: 'format-sql', shortcut: 'Ctrl+I', icon: Code2 },
      ],
    },
    {
      label: 'Run',
      icon: Play,
      items: [
        { label: 'Run All', action: 'run-all', shortcut: 'Ctrl+↵', icon: PlayCircle, disabled: !hasSql },
        { label: 'Run Selection', action: 'run-selection', shortcut: 'Ctrl+Shift+↵', icon: PlaySquare, disabled: !hasSelection },
        { label: 'Run Current Statement', action: 'run-statement', shortcut: 'Alt+↵', icon: Play, disabled: !hasSql },
        { separator: true },
        {
          label: autoCommit ? 'Auto-Commit: ON' : 'Auto-Commit: OFF',
          action: 'toggle-autocommit',
          icon: autoCommit ? CheckSquare : Square,
        },
        { label: 'Commit Transaction', action: 'commit', icon: CheckSquare, disabled: !inTransaction },
        { label: 'Rollback Transaction', action: 'rollback', icon: StopCircle, disabled: !inTransaction },
        { separator: true },
        { label: 'Refresh Schema', action: 'refresh', shortcut: 'F5', icon: RefreshCw },
      ],
    },
    {
      label: 'View',
      icon: Eye,
      items: [
        { label: 'Toggle Sidebar', action: 'toggle-sidebar', shortcut: 'Ctrl+B', icon: PanelLeft },
        { label: 'Toggle Saved Queries', action: 'toggle-saved', icon: FileText },
        { label: 'Toggle Theme', action: 'toggle-theme', icon: Columns },
        { separator: true },
        { label: 'Zoom In', action: 'zoom-in', shortcut: 'Ctrl++', icon: Maximize2 },
        { label: 'Zoom Out', action: 'zoom-out', shortcut: 'Ctrl+-', icon: Maximize2 },
        { label: 'Reset Zoom', action: 'reset-zoom', shortcut: 'Ctrl+0' },
      ],
    },
    {
      label: 'Help',
      icon: HelpCircle,
      items: [
        { label: 'Keyboard Shortcuts', action: 'shortcuts', shortcut: 'Ctrl+/', icon: Keyboard },
        { label: 'ACID Properties Demo', action: 'acid-demo', icon: ShieldCheck },
        { separator: true },
        { label: 'Documentation', action: 'docs', icon: BookOpen },
        { label: 'About Quantsmind', action: 'about', icon: Info },
      ],
    },
  ];

  return (
    <div ref={ref} className="flex items-center gap-0 border-b border-base bg-surface px-1 py-0.5">
      {menus.map((menu) => (
        <div key={menu.label} className="relative">
          <button
            onClick={() => setOpenMenu(openMenu === menu.label ? null : menu.label)}
            onMouseEnter={() => {
              if (openMenu) setOpenMenu(menu.label);
            }}
            className={`flex items-center gap-1 rounded px-2.5 py-1 text-xs font-medium transition-colors ${
              openMenu === menu.label
                ? 'bg-accent-light text-accent'
                : 'text-secondary hover:bg-hover hover:text-primary'
            }`}
          >
            <menu.icon className="h-3.5 w-3.5" />
            {menu.label}
            <ChevronDown className="h-3 w-3 opacity-50" />
          </button>

          {openMenu === menu.label && (
            <div className="absolute left-0 z-50 mt-0.5 w-64 rounded-md border border-base bg-surface py-1 shadow-xl">
              {menu.items.map((item, i) => {
                if (item.separator) {
                  return <div key={i} className="my-1 border-t border-base" />;
                }
                return (
                  <button
                    key={i}
                    onClick={() => {
                      if (item.action) {
                        onAction(item.action);
                        setOpenMenu(null);
                      }
                    }}
                    disabled={item.disabled}
                    className="flex w-full items-center gap-2.5 px-3 py-1.5 text-left text-xs text-secondary hover:bg-hover hover:text-primary disabled:opacity-30 disabled:hover:bg-transparent"
                  >
                    {item.icon && <item.icon className="h-3.5 w-3.5 shrink-0 text-muted" />}
                    <span className="flex-1">{item.label}</span>
                    {item.shortcut && (
                      <kbd className="rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted">
                        {item.shortcut}
                      </kbd>
                    )}
                  </button>
                );
              })}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
