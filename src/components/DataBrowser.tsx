import { useEffect, useState, useCallback } from 'react';
import { engine, type QueryResult, type TableSchema } from '@/lib/engine';
import {
  X, Plus, Trash2, Save, Loader2, AlertTriangle, RefreshCw, ChevronLeft,
  Pencil, CheckCircle2, Copy, ClipboardPaste, Download,
} from 'lucide-react';

type Props = {
  tableName: string;
  schemaName: string;
  onClose: () => void;
  onSchemaChanged: () => void;
};

type RowState = Record<string, unknown>;

export function DataBrowser({ tableName, schemaName, onClose, onSchemaChanged }: Props) {
  const [schema, setSchema] = useState<TableSchema | null>(null);
  const [result, setResult] = useState<QueryResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [info, setInfo] = useState<string | null>(null);
  const [editing, setEditing] = useState<Record<number, RowState>>({});
  const [inserting, setInserting] = useState(false);
  const [insertRow, setInsertRow] = useState<RowState>({});
  const [page, setPage] = useState(0);
  const [total, setTotal] = useState(0);
  const [copiedMsg, setCopiedMsg] = useState<string | null>(null);
  const pageSize = 25;

  const copyToClipboard = (text: string, label?: string) => {
    navigator.clipboard.writeText(text).then(() => {
      setCopiedMsg(label ?? 'Copied');
      setTimeout(() => setCopiedMsg(null), 1500);
    });
  };

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const sc = await engine.getTableSchema(tableName, schemaName);
      setSchema(sc);
      const count = await engine.countRows(tableName, schemaName);
      setTotal(count);
      const data = await engine.getTableData(tableName, schemaName, pageSize, page * pageSize);
      setResult(data);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [tableName, schemaName, page]);

  useEffect(() => {
    load();
  }, [load]);

  const pkColumns = schema?.columns.filter((c) => c.isPrimaryKey).map((c) => c.name) ?? [];

  const startEdit = (index: number) => {
    setEditing((prev) => ({ ...prev, [index]: { ...result!.rows[index] } }));
  };

  const cancelEdit = (index: number) => {
    setEditing((prev) => {
      const next = { ...prev };
      delete next[index];
      return next;
    });
  };

  const updateField = (index: number, col: string, value: string) => {
    setEditing((prev) => ({
      ...prev,
      [index]: { ...prev[index], [col]: value === '' ? null : value },
    }));
  };

  const saveRow = async (index: number) => {
    if (!schema || !result) return;
    const edited = editing[index];
    if (!edited) return;
    const original = result.rows[index];
    const pkVals = pkColumns.map((c) => original[c]);
    if (pkVals.some((v) => v === undefined || v === null)) {
      setError('Cannot edit row without a primary key value.');
      return;
    }

    const updates: Record<string, unknown> = {};
    for (const col of schema.columns) {
      if (!col.isPrimaryKey && edited[col.name] !== original[col.name]) {
        updates[col.name] = edited[col.name];
      }
    }

    if (Object.keys(updates).length === 0) {
      cancelEdit(index);
      return;
    }

    try {
      await engine.updateRow(tableName, schemaName, pkColumns, pkVals, updates);
      setInfo('Row updated.');
      cancelEdit(index);
      onSchemaChanged();
      await load();
      setTimeout(() => setInfo(null), 2000);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const deleteRow = async (row: RowState) => {
    if (!schema) return;
    if (pkColumns.length === 0) {
      setError('Cannot delete without a primary key.');
      return;
    }
    const pkVals = pkColumns.map((c) => row[c]);
    try {
      await engine.deleteRow(tableName, schemaName, pkColumns, pkVals);
      setInfo('Row deleted.');
      onSchemaChanged();
      await load();
      setTimeout(() => setInfo(null), 2000);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const doInsert = async () => {
    if (!schema) return;
    const data: Record<string, unknown> = {};
    for (const col of schema.columns) {
      const val = insertRow[col.name];
      if (val !== undefined && val !== '') {
        data[col.name] = val;
      }
    }
    if (Object.keys(data).length === 0) {
      setError('Enter at least one value.');
      return;
    }
    try {
      await engine.insertRow(tableName, schemaName, data);
      setInfo('Row inserted.');
      setInserting(false);
      setInsertRow({});
      onSchemaChanged();
      await load();
      setTimeout(() => setInfo(null), 2000);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const formatCell = (val: unknown): string => {
    if (val === null) return 'NULL';
    if (val === undefined) return '';
    if (val instanceof Date) return val.toISOString();
    if (typeof val === 'object') return JSON.stringify(val);
    return String(val);
  };

  const totalPages = Math.max(1, Math.ceil(total / pageSize));
  const rows = result?.rows ?? [];

  return (
    <div className="flex h-full flex-col bg-surface">
      <div className="flex items-center justify-between border-b border-base bg-muted px-3 py-2">
        <div className="flex items-center gap-2">
          <button
            onClick={onClose}
            className="rounded p-1 text-muted hover:bg-hover hover:text-secondary"
          >
            <ChevronLeft className="h-4 w-4" />
          </button>
          <span className="text-xs font-semibold uppercase tracking-wide text-secondary">
            Browse: {schemaName}.{tableName}
          </span>
          <span className="rounded-full bg-muted px-2 py-0.5 text-xs text-muted">
            {total.toLocaleString()} rows
          </span>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={load}
            className="flex items-center gap-1 rounded-md px-2 py-1 text-xs text-secondary hover:bg-hover"
          >
            <RefreshCw className={`h-3.5 w-3.5 ${loading ? 'animate-spin' : ''}`} /> Refresh
          </button>
          <button
            onClick={() => {
              const tsv = rows.map((row) =>
                schema!.columns.map((col) => formatCell(row[col.name])).join('\t'),
              ).join('\n');
              copyToClipboard(tsv, 'All rows copied');
            }}
            disabled={!schema || rows.length === 0}
            className="flex items-center gap-1 rounded-md px-2 py-1 text-xs text-secondary hover:bg-hover disabled:opacity-30"
            title="Copy all rows as TSV"
          >
            <Copy className="h-3.5 w-3.5" /> Copy
          </button>
          <button
            onClick={async () => {
              try {
                const text = await navigator.clipboard.readText();
                if (!text.trim()) return;
                const lines = text.trim().split('\n');
                let inserted = 0;
                for (const line of lines) {
                  const values = line.split('\t');
                  const data: Record<string, unknown> = {};
                  schema!.columns.forEach((col, ci) => {
                    if (ci < values.length && values[ci] !== '' && values[ci] !== 'NULL') {
                      data[col.name] = values[ci];
                    }
                  });
                  if (Object.keys(data).length > 0) {
                    await engine.insertRow(tableName, schemaName, data);
                    inserted++;
                  }
                }
                setInfo(`${inserted} row(s) pasted and inserted.`);
                onSchemaChanged();
                await load();
                setTimeout(() => setInfo(null), 2000);
              } catch (e) {
                setError(e instanceof Error ? e.message : String(e));
              }
            }}
            disabled={!schema}
            className="flex items-center gap-1 rounded-md px-2 py-1 text-xs text-secondary hover:bg-hover disabled:opacity-30"
            title="Paste TSV data as new rows"
          >
            <ClipboardPaste className="h-3.5 w-3.5" /> Paste
          </button>
          <button
            onClick={() => {
              const escape = (s: string) => {
                if (/[",\n]/.test(s)) return `"${s.replace(/"/g, '""')}"`;
                return s;
              };
              const header = schema!.columns.map((c) => escape(c.name)).join(',');
              const body = rows.map((row) =>
                schema!.columns.map((col) => escape(formatCell(row[col.name]))).join(','),
              ).join('\n');
              const csv = `${header}\n${body}`;
              const blob = new Blob([csv], { type: 'text/csv;charset=utf-8;' });
              const url = URL.createObjectURL(blob);
              const a = document.createElement('a');
              a.href = url;
              a.download = `${tableName}_export_${Date.now()}.csv`;
              a.click();
              URL.revokeObjectURL(url);
            }}
            disabled={!schema || rows.length === 0}
            className="flex items-center gap-1 rounded-md px-2 py-1 text-xs text-secondary hover:bg-hover disabled:opacity-30"
            title="Export as CSV"
          >
            <Download className="h-3.5 w-3.5" /> CSV
          </button>
          <button
            onClick={() => {
              setInserting(true);
              setInsertRow({});
            }}
            className="flex items-center gap-1 rounded-md bg-accent px-2.5 py-1 text-xs font-semibold text-accent-contrast hover:bg-accent-hover"
          >
            <Plus className="h-3.5 w-3.5" /> Insert
          </button>
        </div>
      </div>

      {loading && !schema ? (
        <div className="flex flex-1 items-center justify-center text-sm text-muted">
          <Loader2 className="mr-2 h-4 w-4 animate-spin" /> Loading…
        </div>
      ) : (
        <div className="flex-1 overflow-auto">
          {inserting && schema && (
            <div className="border-b-2 border-accent bg-accent-light/50">
              <div className="flex items-center gap-1 px-2 py-1.5">
                {schema.columns.map((col) => (
                  <input
                    key={col.name}
                    placeholder={col.name}
                    value={(insertRow[col.name] as string) ?? ''}
                    onChange={(e) =>
                      setInsertRow((prev) => ({
                        ...prev,
                        [col.name]: e.target.value === '' ? undefined : e.target.value,
                      }))
                    }
                    className="min-w-0 flex-1 rounded border border-base bg-elevated px-2 py-1 text-xs text-primary outline-none focus:border-accent"
                  />
                ))}
                <button
                  onClick={doInsert}
                  className="flex items-center gap-1 rounded bg-accent px-2 py-1 text-xs font-semibold text-accent-contrast hover:bg-accent-hover"
                >
                  <Save className="h-3 w-3" /> Save
                </button>
                <button
                  onClick={() => setInserting(false)}
                  className="rounded px-2 py-1 text-xs text-muted hover:bg-hover"
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              </div>
            </div>
          )}
          <table className="w-full border-collapse text-sm">
            <thead className="sticky top-0 z-10">
              <tr className="bg-muted">
                <th className="w-20 border-b border-base px-2 py-2 text-center text-xs font-medium text-muted">
                  Actions
                </th>
                {schema?.columns.map((col) => (
                  <th
                    key={col.name}
                    className="border-b border-l border-base px-3 py-2 text-left text-xs font-semibold text-secondary"
                  >
                    {col.name}
                    <span className="ml-1 font-normal text-muted">{col.dataType}</span>
                    {col.isPrimaryKey && (
                      <span className="ml-1 rounded bg-warning-light px-1 text-[10px] text-warning">
                        PK
                      </span>
                    )}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rows.map((row, i) => {
                const isEditing = editing[i] !== undefined;
                const editedRow = editing[i] ?? row;
                return (
                  <tr key={i} className="group border-b border-base hover:bg-hover">
                    <td className="px-2 py-1.5 text-center">
                      {isEditing ? (
                        <div className="flex items-center justify-center gap-1">
                          <button
                            onClick={() => saveRow(i)}
                            className="rounded p-1 text-success hover:bg-success-light"
                            title="Save"
                          >
                            <CheckCircle2 className="h-3.5 w-3.5" />
                          </button>
                          <button
                            onClick={() => cancelEdit(i)}
                            className="rounded p-1 text-muted hover:bg-hover"
                            title="Cancel"
                          >
                            <X className="h-3.5 w-3.5" />
                          </button>
                        </div>
                      ) : (
                        <div className="flex items-center justify-center gap-1 opacity-0 group-hover:opacity-100">
                          <button
                            onClick={() => startEdit(i)}
                            className="rounded p-1 text-accent hover:bg-accent-light"
                            title="Edit"
                          >
                            <Pencil className="h-3.5 w-3.5" />
                          </button>
                          <button
                            onClick={() => deleteRow(row)}
                            className="rounded p-1 text-error hover:bg-error-light"
                            title="Delete"
                          >
                            <Trash2 className="h-3.5 w-3.5" />
                          </button>
                        </div>
                      )}
                    </td>
                    {schema?.columns.map((col) => {
                      const val = isEditing ? editedRow[col.name] : row[col.name];
                      if (isEditing && !col.isPrimaryKey) {
                        return (
                          <td
                            key={col.name}
                            className="border-l border-base px-1 py-1"
                          >
                            <input
                              value={val === null ? '' : String(val ?? '')}
                              onChange={(e) => updateField(i, col.name, e.target.value)}
                              className="w-full rounded border border-base bg-elevated px-2 py-1 font-mono text-xs text-primary outline-none focus:border-accent"
                            />
                          </td>
                        );
                      }
                      return (
                        <td
                          key={col.name}
                          className={`border-l border-base px-3 py-1.5 font-mono text-xs ${
                            val === null ? 'italic text-faint' : 'text-primary'
                          }`}
                        >
                          {formatCell(val)}
                        </td>
                      );
                    })}
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      {copiedMsg && (
        <div className="flex items-center gap-1.5 border-b border-success bg-success-light px-3 py-1 text-[10px] text-success">
          <CheckCircle2 className="h-3 w-3" />
          {copiedMsg}
        </div>
      )}

      {total > pageSize && (
        <div className="flex items-center justify-center gap-3 border-t border-base bg-muted px-3 py-2 text-xs text-secondary">
          <button
            onClick={() => setPage((p) => Math.max(0, p - 1))}
            disabled={page === 0}
            className="rounded px-2 py-0.5 hover:bg-hover disabled:opacity-30"
          >
            Prev
          </button>
          <span>
            Page {page + 1} of {totalPages}
          </span>
          <button
            onClick={() => setPage((p) => Math.min(totalPages - 1, p + 1))}
            disabled={page >= totalPages - 1}
            className="rounded px-2 py-0.5 hover:bg-hover disabled:opacity-30"
          >
            Next
          </button>
        </div>
      )}

      {error && (
        <div className="flex items-start gap-2 border-t border-error bg-error-light px-3 py-2 text-xs text-error">
          <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          <span className="break-all">{error}</span>
          <button onClick={() => setError(null)} className="ml-auto text-error">
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}
      {info && (
        <div className="flex items-center gap-2 border-t border-success bg-success-light px-3 py-2 text-xs text-success">
          <CheckCircle2 className="h-3.5 w-3.5" />
          {info}
        </div>
      )}
    </div>
  );
}
