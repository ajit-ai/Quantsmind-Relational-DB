import { X } from 'lucide-react';

type Props = {
  open: boolean;
  onClose: () => void;
};

const SHORTCUTS = [
  { category: 'Query', items: [
    { keys: 'Ctrl+Enter', action: 'Run all SQL' },
    { keys: 'Ctrl+Shift+Enter', action: 'Run selected SQL' },
    { keys: 'Alt+Enter', action: 'Run current statement' },
    { keys: 'Ctrl+S', action: 'Save query' },
    { keys: 'Ctrl+O', action: 'Open .sql file' },
    { keys: 'Ctrl+I', action: 'Format SQL' },
  ]},
  { category: 'Edit', items: [
    { keys: 'Ctrl+F', action: 'Find' },
    { keys: 'Ctrl+H', action: 'Find & Replace' },
    { keys: 'Ctrl+A', action: 'Select all' },
    { keys: 'Ctrl+Z', action: 'Undo' },
    { keys: 'Ctrl+Y', action: 'Redo' },
    { keys: 'Tab', action: 'Insert 2 spaces' },
  ]},
  { category: 'View', items: [
    { keys: 'Ctrl+B', action: 'Toggle sidebar' },
    { keys: 'Ctrl++', action: 'Zoom in' },
    { keys: 'Ctrl+-', action: 'Zoom out' },
    { keys: 'Ctrl+0', action: 'Reset zoom' },
    { keys: 'F5', action: 'Refresh schema' },
  ]},
];

export function ShortcutsDialog({ open, onClose }: Props) {
  if (!open) return null;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30 backdrop-blur-sm">
      <div className="w-full max-w-2xl rounded-xl border border-base bg-surface shadow-2xl">
        <div className="flex items-center justify-between border-b border-base px-5 py-3">
          <h2 className="text-sm font-semibold text-primary">Keyboard Shortcuts</h2>
          <button onClick={onClose} className="text-muted hover:text-secondary">
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="max-h-[70vh] overflow-y-auto p-5">
          <div className="grid gap-6 sm:grid-cols-2 lg:grid-cols-3">
            {SHORTCUTS.map((group) => (
              <div key={group.category}>
                <h3 className="mb-2 text-xs font-semibold uppercase tracking-wide text-accent">
                  {group.category}
                </h3>
                <ul className="space-y-1.5">
                  {group.items.map((item) => (
                    <li key={item.keys} className="flex items-center justify-between gap-2 text-xs">
                      <span className="text-secondary">{item.action}</span>
                      <kbd className="rounded border border-base bg-muted px-1.5 py-0.5 font-mono text-[10px] text-muted">
                        {item.keys}
                      </kbd>
                    </li>
                  ))}
                </ul>
              </div>
            ))}
          </div>
        </div>
        <div className="border-t border-base px-5 py-3 text-right">
          <button
            onClick={onClose}
            className="rounded-md bg-accent px-4 py-1.5 text-xs font-semibold text-accent-contrast hover:bg-accent-hover"
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}

export function AboutDialog({ open, onClose }: Props) {
  if (!open) return null;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30 backdrop-blur-sm">
      <div className="w-full max-w-md rounded-xl border border-base bg-surface shadow-2xl">
        <div className="flex items-center justify-between border-b border-base px-5 py-3">
          <h2 className="text-sm font-semibold text-primary">About Quantsmind</h2>
          <button onClick={onClose} className="text-muted hover:text-secondary">
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="p-6 text-center">
          <div className="mx-auto mb-3 flex h-14 w-14 items-center justify-center rounded-xl bg-accent">
            <Database className="h-7 w-7 text-accent-contrast" />
          </div>
          <h3 className="text-lg font-bold text-primary">Quantsmind</h3>
          <p className="text-xs text-muted">Relational Database Studio</p>
          <p className="mt-1 text-xs text-muted">Version 1.0.0</p>
          <div className="mt-4 space-y-1 text-left text-xs text-secondary">
            <p>A browser-based relational database studio with full SQL support, powered by PGlite (PostgreSQL WASM).</p>
            <p className="mt-2 text-muted">Features:</p>
            <ul className="ml-4 list-disc space-y-0.5 text-muted">
              <li>Full DDL & DML support (CREATE, ALTER, DROP, INSERT, UPDATE, DELETE)</li>
              <li>Schema management (create, drop, switch)</li>
              <li>Visual table designer</li>
              <li>Data browser with inline editing</li>
              <li>ACID property demonstrations</li>
              <li>Saved queries & file import/export</li>
              <li>Light/Dark themes with 7 accent colors</li>
            </ul>
          </div>
        </div>
        <div className="border-t border-base px-5 py-3 text-right">
          <button
            onClick={onClose}
            className="rounded-md bg-accent px-4 py-1.5 text-xs font-semibold text-accent-contrast hover:bg-accent-hover"
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}

import { Database } from 'lucide-react';
