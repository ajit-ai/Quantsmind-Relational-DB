import { useEffect, useRef, useState } from 'react';
import { engine, type QueryResult } from '@/lib/engine';
import { queryStore } from '@/lib/queryStore';
import { formatSql } from '@/lib/formatter';
import {
  Play, Loader2, Trash2, CheckCircle2, XCircle, AlertTriangle,
  Code2, Save, FolderOpen, Download, Bookmark,
  Search, X, ChevronDown, ChevronUp, Replace, CheckSquare, Square,
} from 'lucide-react';

type HistoryItem = {
  sql: string;
  success: boolean;
  durationMs: number;
  rowCount: number;
};

type Props = {
  initialSql?: string;
  onResult: (result: QueryResult, sql: string) => void;
  onSchemaChanged: () => void;
  onOpenSaved: () => void;
  autoCommit: boolean;
  onToggleAutoCommit: () => void;
  onCommit: () => Promise<void>;
  onRollback: () => Promise<void>;
  inTransaction: boolean;
  externalAction?: { type: string; nonce: number } | null;
};

const TEMPLATES: { label: string; sql: string }[] = [
  { label: 'SELECT all', sql: 'SELECT * FROM "public"."table_name" LIMIT 100;' },
  { label: 'INSERT row', sql: 'INSERT INTO "public"."table_name" (col1, col2)\nVALUES (\'value1\', \'value2\');' },
  { label: 'UPDATE rows', sql: 'UPDATE "public"."table_name"\nSET col1 = \'new_value\'\nWHERE id = 1;' },
  { label: 'DELETE rows', sql: 'DELETE FROM "public"."table_name"\nWHERE id = 1;' },
  { label: 'CREATE TABLE', sql: 'CREATE TABLE "public"."new_table" (\n  id SERIAL PRIMARY KEY,\n  name TEXT NOT NULL,\n  created_at TIMESTAMPTZ DEFAULT now()\n);' },
  { label: 'ALTER TABLE', sql: 'ALTER TABLE "public"."table_name"\n  ADD COLUMN new_col TEXT;' },
  { label: 'CREATE INDEX', sql: 'CREATE INDEX idx_name\n  ON "public"."table_name" (column_name);' },
  { label: 'CREATE SCHEMA', sql: 'CREATE SCHEMA "my_schema";' },
  { label: 'JOIN', sql: 'SELECT a.*, b.*\nFROM "public"."table_a" a\nJOIN "public"."table_b" b ON a.id = b.a_id\nLIMIT 50;' },
  { label: 'GROUP BY', sql: 'SELECT column_name, count(*) AS cnt\nFROM "public"."table_name"\nGROUP BY column_name\nORDER BY cnt DESC;' },
  { label: 'DROP TABLE', sql: 'DROP TABLE IF EXISTS "public"."table_name" CASCADE;' },
  { label: 'TRUNCATE', sql: 'TRUNCATE TABLE "public"."table_name" RESTART IDENTITY CASCADE;' },
];

export function SqlEditor({
  initialSql,
  onResult,
  onSchemaChanged,
  onOpenSaved,
  autoCommit,
  onToggleAutoCommit,
  onCommit,
  onRollback,
  inTransaction,
  externalAction,
}: Props) {
  const [sql, setSql] = useState(initialSql ?? '');
  const [running, setRunning] = useState(false);
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [showTemplates, setShowTemplates] = useState(false);
  const [showSaveDialog, setShowSaveDialog] = useState(false);
  const [saveTitle, setSaveTitle] = useState('');
  const [savedQueryId, setSavedQueryId] = useState<string | undefined>(undefined);
  const [showFind, setShowFind] = useState(false);
  const [showReplace, setShowReplace] = useState(false);
  const [findText, setFindText] = useState('');
  const [replaceText, setReplaceText] = useState('');
  const [findMatchCase, setFindMatchCase] = useState(false);
  const [findCount, setFindCount] = useState(0);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const templateRef = useRef<HTMLDivElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const findInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (initialSql !== undefined) {
      setSql(initialSql);
      textareaRef.current?.focus();
    }
  }, [initialSql]);

  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      if (templateRef.current && !templateRef.current.contains(e.target as Node)) {
        setShowTemplates(false);
      }
    };
    document.addEventListener('mousedown', onClick);
    return () => document.removeEventListener('mousedown', onClick);
  }, []);

  // Handle external actions from MenuBar
  useEffect(() => {
    if (!externalAction) return;
    const { type, nonce } = externalAction;
    if (nonce === 0) return;

    switch (type) {
      case 'find':
        setShowFind(true);
        setShowReplace(false);
        setTimeout(() => findInputRef.current?.focus(), 50);
        break;
      case 'replace':
        setShowFind(true);
        setShowReplace(true);
        setTimeout(() => findInputRef.current?.focus(), 50);
        break;
      case 'format-sql':
        if (sql.trim()) {
          try {
            setSql(formatSql(sql));
            setSuccess('SQL formatted.');
            setTimeout(() => setSuccess(null), 1500);
          } catch {
            setError('Could not format SQL.');
          }
        }
        break;
      case 'save-query':
        if (savedQueryId) {
          doSaveQuery();
        } else {
          setSaveTitle('');
          setShowSaveDialog(true);
        }
        break;
      case 'save-as':
        setSavedQueryId(undefined);
        setSaveTitle('');
        setShowSaveDialog(true);
        break;
      case 'open-file':
        fileInputRef.current?.click();
        break;
      case 'export-sql':
        exportSql();
        break;
      case 'select-all':
        textareaRef.current?.select();
        break;
      case 'delete':
        setSql('');
        break;
      case 'new-query':
        setSql('');
        setSavedQueryId(undefined);
        setError(null);
        setSuccess(null);
        textareaRef.current?.focus();
        break;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [externalAction]);

  const isDdl = (s: string) =>
    /\b(create|alter|drop|truncate|rename|add|column|index|table|schema)\b/i.test(s);

  const runAll = async () => {
    const trimmed = sql.trim();
    if (!trimmed) return;
    await runStatements(trimmed);
  };

  const runSelection = async () => {
    const el = textareaRef.current;
    if (!el) return;
    const selected = sql.slice(el.selectionStart, el.selectionEnd).trim();
    if (!selected) {
      setError('No text selected. Select SQL to run, or use Run All.');
      setTimeout(() => setError(null), 2000);
      return;
    }
    await runStatements(selected);
  };

  const runStatement = async () => {
    const el = textareaRef.current;
    if (!el) return;
    const cursor = el.selectionStart;
    const statements = splitStatements(sql);
    let pos = 0;
    for (const stmt of statements) {
      const start = sql.indexOf(stmt.trim(), pos);
      const end = start + stmt.length;
      if (cursor >= start && cursor <= end) {
        await runStatements(stmt.trim());
        return;
      }
      pos = end;
    }
    if (statements.length > 0) {
      await runStatements(statements[statements.length - 1].trim());
    }
  };

  const runStatements = async (sqlText: string) => {
    setRunning(true);
    setError(null);
    setSuccess(null);
    try {
      const statements = splitStatements(sqlText);
      let lastResult: QueryResult | null = null;
      let changedSchema = false;

      for (const stmt of statements) {
        const s = stmt.trim();
        if (!s) continue;
        if (isDdl(s)) changedSchema = true;
        const result = await engine.query(s);
        lastResult = result;
      }

      if (lastResult) {
        onResult(lastResult, sqlText);
      }
      setHistory((prev) =>
        [
          {
            sql: sqlText,
            success: true,
            durationMs: lastResult?.durationMs ?? 0,
            rowCount: lastResult?.rowCount ?? 0,
          },
          ...prev,
        ].slice(0, 30),
      );
      const verb = lastResult?.command ?? 'OK';
      setSuccess(
        `${verb} — ${lastResult?.rowCount ?? 0} row(s) in ${lastResult?.durationMs ?? 0} ms`,
      );
      if (changedSchema) onSchemaChanged();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      setHistory((prev) =>
        [{ sql: sqlText, success: false, durationMs: 0, rowCount: 0 }, ...prev].slice(0, 30),
      );
    } finally {
      setRunning(false);
    }
  };

  const doSaveQuery = () => {
    const title = saveTitle.trim() || 'Untitled Query';
    const saved = queryStore.save(title, sql, savedQueryId);
    setSavedQueryId(saved.id);
    setShowSaveDialog(false);
    setSuccess(`Query saved as "${title}"`);
    setTimeout(() => setSuccess(null), 2000);
  };

  const exportSql = () => {
    const blob = new Blob([sql], { type: 'text/sql;charset=utf-8;' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `query_${Date.now()}.sql`;
    a.click();
    URL.revokeObjectURL(url);
  };

  const importSql = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      setSql(String(reader.result ?? ''));
      setSavedQueryId(undefined);
    };
    reader.readAsText(file);
    e.target.value = '';
  };

  const doFind = () => {
    if (!findText) {
      setFindCount(0);
      return;
    }
    const flags = findMatchCase ? 'g' : 'gi';
    const escaped = findText.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    const regex = new RegExp(escaped, flags);
    const matches = sql.match(regex);
    setFindCount(matches?.length ?? 0);
  };

  useEffect(() => {
    doFind();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [findText, findMatchCase, sql]);

  const findNext = () => {
    const el = textareaRef.current;
    if (!el || !findText) return;
    const flags = findMatchCase ? 'g' : 'gi';
    const escaped = findText.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    const regex = new RegExp(escaped, flags);
    const fromCursor = el.selectionEnd;
    const text = sql.slice(fromCursor);
    const match = regex.exec(text);
    if (match) {
      const start = fromCursor + match.index;
      const end = start + match[0].length;
      el.focus();
      el.setSelectionRange(start, end);
    } else {
      // Wrap around
      const matchFromStart = regex.exec(sql);
      if (matchFromStart) {
        el.focus();
        el.setSelectionRange(matchFromStart.index, matchFromStart.index + matchFromStart[0].length);
      }
    }
  };

  const replaceNext = () => {
    const el = textareaRef.current;
    if (!el || !findText) return;
    const selected = sql.slice(el.selectionStart, el.selectionEnd);
    const matches = findMatchCase
      ? selected === findText
      : selected.toLowerCase() === findText.toLowerCase();
    if (matches) {
      const next = sql.slice(0, el.selectionStart) + replaceText + sql.slice(el.selectionEnd);
      setSql(next);
      setTimeout(() => {
        el.setSelectionRange(el.selectionStart, el.selectionStart + replaceText.length);
      }, 0);
    }
    findNext();
  };

  const replaceAll = () => {
    if (!findText) return;
    const flags = findMatchCase ? 'g' : 'gi';
    const escaped = findText.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    const regex = new RegExp(escaped, flags);
    const count = (sql.match(regex) ?? []).length;
    setSql(sql.replace(regex, replaceText));
    setSuccess(`${count} replacement(s) made.`);
    setTimeout(() => setSuccess(null), 2000);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
      e.preventDefault();
      if (e.shiftKey) {
        runSelection();
      } else {
        runAll();
      }
    }
    if (e.altKey && e.key === 'Enter') {
      e.preventDefault();
      runStatement();
    }
    if ((e.metaKey || e.ctrlKey) && e.key === 's') {
      e.preventDefault();
      if (savedQueryId) {
        doSaveQuery();
      } else {
        setSaveTitle('');
        setShowSaveDialog(true);
      }
    }
    if ((e.metaKey || e.ctrlKey) && e.key === 'o') {
      e.preventDefault();
      fileInputRef.current?.click();
    }
    if ((e.metaKey || e.ctrlKey) && e.key === 'f') {
      e.preventDefault();
      setShowFind(true);
      setShowReplace(false);
      setTimeout(() => findInputRef.current?.focus(), 50);
    }
    if ((e.metaKey || e.ctrlKey) && e.key === 'h') {
      e.preventDefault();
      setShowFind(true);
      setShowReplace(true);
      setTimeout(() => findInputRef.current?.focus(), 50);
    }
    if (e.key === 'Escape' && showFind) {
      setShowFind(false);
      setShowReplace(false);
    }
    if (e.key === 'Tab') {
      e.preventDefault();
      const el = e.currentTarget;
      const start = el.selectionStart;
      const end = el.selectionEnd;
      const next = sql.slice(0, start) + '  ' + sql.slice(end);
      setSql(next);
      requestAnimationFrame(() => {
        el.selectionStart = el.selectionEnd = start + 2;
      });
    }
  };

  return (
    <div className="flex h-full flex-col bg-surface">
      <input
        ref={fileInputRef}
        type="file"
        accept=".sql,.txt"
        onChange={importSql}
        className="hidden"
      />

      <div className="flex items-center justify-between border-b border-base px-3 py-1.5">
        <span className="text-xs font-semibold uppercase tracking-wide text-muted">
          SQL Editor
        </span>
        <div className="flex items-center gap-1">
          {/* Auto-commit toggle */}
          <button
            onClick={onToggleAutoCommit}
            className={`flex items-center gap-1 rounded px-2 py-1 text-xs ${
              autoCommit ? 'text-success' : 'text-warning'
            } hover:bg-hover`}
            title={autoCommit ? 'Auto-commit ON' : 'Auto-commit OFF (manual transaction)'}
          >
            {autoCommit ? <CheckSquare className="h-3.5 w-3.5" /> : <Square className="h-3.5 w-3.5" />}
            <span className="hidden sm:inline">Auto</span>
          </button>

          {/* Commit / Rollback (only in manual mode) */}
          {!autoCommit && (
            <>
              <button
                onClick={onCommit}
                disabled={!inTransaction}
                className="flex items-center gap-1 rounded px-2 py-1 text-xs text-success hover:bg-success-light disabled:opacity-30"
                title="Commit transaction"
              >
                <CheckSquare className="h-3.5 w-3.5" />
              </button>
              <button
                onClick={onRollback}
                disabled={!inTransaction}
                className="flex items-center gap-1 rounded px-2 py-1 text-xs text-error hover:bg-error-light disabled:opacity-30"
                title="Rollback transaction"
              >
                <XCircle className="h-3.5 w-3.5" />
              </button>
            </>
          )}

          {/* Templates */}
          <div ref={templateRef} className="relative">
            <button
              onClick={() => setShowTemplates((v) => !v)}
              className="flex items-center gap-1 rounded px-2 py-1 text-xs text-secondary hover:bg-hover"
            >
              <Code2 className="h-3.5 w-3.5" /> Templates
            </button>
            {showTemplates && (
              <div className="absolute right-0 z-50 mt-1 w-56 rounded-md border border-base bg-surface shadow-xl">
                {TEMPLATES.map((t) => (
                  <button
                    key={t.label}
                    onClick={() => {
                      setSql(t.sql);
                      setShowTemplates(false);
                      setSavedQueryId(undefined);
                      textareaRef.current?.focus();
                    }}
                    className="block w-full px-3 py-1.5 text-left text-xs text-secondary hover:bg-hover hover:text-primary"
                  >
                    {t.label}
                  </button>
                ))}
              </div>
            )}
          </div>

          {/* Saved queries */}
          <button
            onClick={onOpenSaved}
            className="flex items-center gap-1 rounded px-2 py-1 text-xs text-secondary hover:bg-hover"
            title="Saved queries"
          >
            <Bookmark className="h-3.5 w-3.5" />
          </button>

          {/* Find */}
          <button
            onClick={() => {
              setShowFind((v) => !v);
              setShowReplace(false);
              setTimeout(() => findInputRef.current?.focus(), 50);
            }}
            className="rounded px-2 py-1 text-xs text-secondary hover:bg-hover"
            title="Find (Ctrl+F)"
          >
            <Search className="h-3.5 w-3.5" />
          </button>

          {/* Import file */}
          <button
            onClick={() => fileInputRef.current?.click()}
            className="rounded px-2 py-1 text-xs text-secondary hover:bg-hover"
            title="Open .sql file (Ctrl+O)"
          >
            <FolderOpen className="h-3.5 w-3.5" />
          </button>

          {/* Export file */}
          <button
            onClick={exportSql}
            disabled={!sql.trim()}
            className="rounded px-2 py-1 text-xs text-secondary hover:bg-hover disabled:opacity-30"
            title="Export as .sql file"
          >
            <Download className="h-3.5 w-3.5" />
          </button>

          {/* Save query */}
          <button
            onClick={() => {
              if (savedQueryId) {
                doSaveQuery();
              } else {
                setSaveTitle('');
                setShowSaveDialog(true);
              }
            }}
            disabled={!sql.trim()}
            className="rounded px-2 py-1 text-xs text-secondary hover:bg-hover disabled:opacity-30"
            title="Save query (Ctrl+S)"
          >
            <Save className="h-3.5 w-3.5" />
          </button>

          {/* Clear */}
          <button
            onClick={() => {
              setSql('');
              setError(null);
              setSuccess(null);
              setSavedQueryId(undefined);
            }}
            className="rounded px-2 py-1 text-xs text-secondary hover:bg-hover"
          >
            <Trash2 className="h-3.5 w-3.5" />
          </button>

          {/* Run dropdown */}
          <div className="relative">
            <button
              onClick={runAll}
              disabled={running || !sql.trim()}
              className="ml-1 flex items-center gap-1.5 rounded-md bg-accent px-3 py-1.5 text-xs font-semibold text-accent-contrast hover:bg-accent-hover disabled:opacity-40"
            >
              {running ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Play className="h-3.5 w-3.5" />
              )}
              Run
              <kbd className="ml-1 rounded bg-black/20 px-1 text-[10px]">⌘↵</kbd>
            </button>
          </div>
        </div>
      </div>

      {/* Find/Replace bar */}
      {showFind && (
        <div className="flex items-center gap-2 border-b border-base bg-muted px-3 py-1.5">
          <div className="flex items-center gap-1">
            <input
              ref={findInputRef}
              value={findText}
              onChange={(e) => setFindText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  if (e.shiftKey && showReplace) replaceNext();
                  else findNext();
                }
              }}
              placeholder="Find…"
              className="w-40 rounded border border-base bg-elevated px-2 py-1 text-xs text-primary outline-none focus:border-accent"
            />
            <span className="text-[10px] text-muted">{findCount} match(es)</span>
            <button
              onClick={() => setFindMatchCase((v) => !v)}
              className={`rounded px-1.5 py-0.5 text-[10px] ${
                findMatchCase ? 'bg-accent text-accent-contrast' : 'text-muted hover:bg-hover'
              }`}
              title="Match case"
            >
              Aa
            </button>
            <button
              onClick={findNext}
              className="rounded p-0.5 text-muted hover:bg-hover hover:text-primary"
              title="Find next"
            >
              <ChevronDown className="h-3.5 w-3.5" />
            </button>
            <button
              onClick={() => {
                const el = textareaRef.current;
                if (!el || !findText) return;
                const flags = findMatchCase ? 'g' : 'gi';
                const escaped = findText.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
                const regex = new RegExp(escaped, flags);
                const beforeCursor = sql.slice(0, el.selectionStart);
                const match = regex.exec(beforeCursor);
                if (match) {
                  el.focus();
                  el.setSelectionRange(match.index, match.index + match[0].length);
                }
              }}
              className="rounded p-0.5 text-muted hover:bg-hover hover:text-primary"
              title="Find previous"
            >
              <ChevronUp className="h-3.5 w-3.5" />
            </button>
          </div>

          {showReplace && (
            <div className="flex items-center gap-1">
              <input
                value={replaceText}
                onChange={(e) => setReplaceText(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    replaceNext();
                  }
                }}
                placeholder="Replace with…"
                className="w-40 rounded border border-base bg-elevated px-2 py-1 text-xs text-primary outline-none focus:border-accent"
              />
              <button
                onClick={replaceNext}
                className="rounded px-2 py-0.5 text-xs text-secondary hover:bg-hover"
                title="Replace next"
              >
                Replace
              </button>
              <button
                onClick={replaceAll}
                className="rounded px-2 py-0.5 text-xs text-secondary hover:bg-hover"
                title="Replace all"
              >
                All
              </button>
            </div>
          )}

          {!showReplace && (
            <button
              onClick={() => setShowReplace(true)}
              className="rounded p-0.5 text-muted hover:bg-hover hover:text-primary"
              title="Toggle replace"
            >
              <Replace className="h-3.5 w-3.5" />
            </button>
          )}

          <button
            onClick={() => {
              setShowFind(false);
              setShowReplace(false);
            }}
            className="ml-auto rounded p-0.5 text-muted hover:bg-hover hover:text-secondary"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}

      {/* Save dialog */}
      {showSaveDialog && (
        <div className="flex items-center gap-2 border-b border-accent bg-accent-light/50 px-3 py-2">
          <Save className="h-3.5 w-3.5 text-accent" />
          <input
            value={saveTitle}
            onChange={(e) => setSaveTitle(e.target.value)}
            placeholder="Query name…"
            autoFocus
            onKeyDown={(e) => {
              if (e.key === 'Enter') doSaveQuery();
              if (e.key === 'Escape') setShowSaveDialog(false);
            }}
            className="flex-1 rounded border border-base bg-elevated px-2 py-1 text-xs text-primary outline-none focus:border-accent"
          />
          <button
            onClick={doSaveQuery}
            className="rounded bg-accent px-3 py-1 text-xs font-semibold text-accent-contrast hover:bg-accent-hover"
          >
            Save
          </button>
          <button
            onClick={() => setShowSaveDialog(false)}
            className="rounded px-2 py-1 text-xs text-muted hover:bg-hover"
          >
            Cancel
          </button>
        </div>
      )}

      <textarea
        ref={textareaRef}
        value={sql}
        onChange={(e) => setSql(e.target.value)}
        onKeyDown={onKeyDown}
        spellCheck={false}
        placeholder="SELECT * FROM customers;"
        className="flex-1 resize-none bg-surface p-4 font-mono text-sm leading-relaxed text-primary outline-none placeholder:text-faint"
      />

      {(error || success) && (
        <div
          className={`flex items-start gap-2 border-t px-3 py-2 text-xs ${
            error
              ? 'border-error bg-error-light text-error'
              : 'border-success bg-success-light text-success'
          }`}
        >
          {error ? (
            <XCircle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          ) : (
            <CheckCircle2 className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          )}
          <span className="break-all">{error ?? success}</span>
        </div>
      )}

      {history.length > 0 && (
        <div className="max-h-28 overflow-y-auto border-t border-base bg-muted px-3 py-2">
          <div className="mb-1 text-[10px] font-semibold uppercase tracking-wide text-muted">
            History
          </div>
          <ul className="space-y-0.5">
            {history.map((h, i) => (
              <li key={i} className="flex items-center gap-2 text-xs">
                {h.success ? (
                  <CheckCircle2 className="h-3 w-3 text-success" />
                ) : (
                  <AlertTriangle className="h-3 w-3 text-error" />
                )}
                <button
                  onClick={() => setSql(h.sql)}
                  className="flex-1 truncate font-mono text-left text-secondary hover:text-primary"
                  title={h.sql}
                >
                  {h.sql}
                </button>
                <span className="shrink-0 text-muted">
                  {h.success ? `${h.rowCount}r · ${h.durationMs}ms` : 'error'}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

function splitStatements(sql: string): string[] {
  const statements: string[] = [];
  let current = '';
  let inSingle = false;
  let inDouble = false;
  let inDollar = false;
  let dollarTag = '';

  for (let i = 0; i < sql.length; i++) {
    const ch = sql[i];
    const next = sql[i + 1];

    if (!inSingle && !inDouble && !inDollar && ch === '$' && /[a-zA-Z_]/.test(next ?? '')) {
      let tag = '$';
      let j = i + 1;
      while (j < sql.length && /[a-zA-Z0-9_]/.test(sql[j])) {
        tag += sql[j];
        j++;
      }
      if (sql[j] === '$') {
        tag += '$';
        inDollar = true;
        dollarTag = tag;
        current += sql.slice(i, j + 1);
        i = j;
        continue;
      }
    }

    if (inDollar) {
      current += ch;
      if (ch === '$' && sql.slice(i).startsWith(dollarTag)) {
        current += sql.slice(i + 1, i + dollarTag.length);
        i += dollarTag.length - 1;
        inDollar = false;
        dollarTag = '';
      }
      continue;
    }

    if (ch === "'" && !inDouble) {
      inSingle = !inSingle;
    } else if (ch === '"' && !inSingle) {
      inDouble = !inDouble;
    }

    if (ch === ';' && !inSingle && !inDouble && !inDollar) {
      current += ch;
      statements.push(current);
      current = '';
      continue;
    }

    current += ch;
  }

  if (current.trim()) statements.push(current);
  return statements;
}
