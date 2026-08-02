import { useEffect, useState } from 'react';
import { engine, type TableSchema, type ColumnInfo } from '@/lib/engine';
import { X, Plus, Trash2, KeyRound, Save, Loader2, AlertTriangle } from 'lucide-react';

type DraftColumn = {
  name: string;
  dataType: string;
  isNullable: boolean;
  isPrimaryKey: boolean;
  defaultValue: string;
  _original?: ColumnInfo;
  _status: 'new' | 'modified' | 'unchanged' | 'deleted';
};

const TYPES = [
  'integer',
  'serial',
  'bigint',
  'text',
  'varchar(255)',
  'boolean',
  'numeric(10,2)',
  'date',
  'timestamp',
  'timestamptz',
  'uuid',
  'json',
  'jsonb',
];

type Props = {
  open: boolean;
  tableName: string | null;
  schemaName: string;
  onClose: () => void;
  onSaved: () => void;
};

function qualify(schema: string, table: string): string {
  return `"${schema}"."${table}"`;
}

export function TableDesigner({ open, tableName, schemaName, onClose, onSaved }: Props) {
  const [name, setName] = useState('');
  const [columns, setColumns] = useState<DraftColumn[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [info, setInfo] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setError(null);
    setInfo(null);
    if (tableName) {
      setLoading(true);
      engine
        .getTableSchema(tableName, schemaName)
        .then((schema: TableSchema) => {
          setName(schema.table);
          setColumns(
            schema.columns.map((c: ColumnInfo) => ({
              name: c.name,
              dataType: c.dataType,
              isNullable: c.isNullable,
              isPrimaryKey: c.isPrimaryKey,
              defaultValue: c.defaultValue ?? '',
              _original: c,
              _status: 'unchanged',
            })),
          );
        })
        .catch((e) => setError(e instanceof Error ? e.message : String(e)))
        .finally(() => setLoading(false));
    } else {
      setName('');
      setColumns([
        { name: 'id', dataType: 'serial', isNullable: false, isPrimaryKey: true, defaultValue: '', _status: 'new' },
        { name: 'name', dataType: 'text', isNullable: false, isPrimaryKey: false, defaultValue: '', _status: 'new' },
      ]);
    }
  }, [open, tableName, schemaName]);

  if (!open) return null;

  const updateCol = (i: number, patch: Partial<DraftColumn>) => {
    setColumns((prev) =>
      prev.map((c, idx) => {
        if (idx !== i) return c;
        const updated = { ...c, ...patch };
        if (c._status === 'unchanged' && c._original) {
          updated._status = 'modified';
        }
        return updated;
      }),
    );
  };

  const addColumn = () => {
    setColumns((prev) => [
      ...prev,
      { name: '', dataType: 'text', isNullable: true, isPrimaryKey: false, defaultValue: '', _status: 'new' },
    ]);
  };

  const removeColumn = (i: number) => {
    setColumns((prev) => {
      const col = prev[i];
      if (col._status === 'new') {
        return prev.filter((_, idx) => idx !== i);
      }
      return prev.map((c, idx) => (idx === i ? { ...c, _status: 'deleted' } : c));
    });
  };

  const buildCreateSql = (): string => {
    const cols = columns
      .filter((c) => c._status !== 'deleted')
      .map((c) => {
        let line = `"${c.name}" ${c.dataType}`;
        if (!c.isNullable) line += ' NOT NULL';
        if (c.defaultValue) line += ` DEFAULT ${c.defaultValue}`;
        return line;
      })
      .join(',\n  ');
    const pkCols = columns.filter((c) => c.isPrimaryKey && c._status !== 'deleted').map((c) => `"${c.name}"`);
    const pk = pkCols.length ? `,\n  PRIMARY KEY (${pkCols.join(', ')})` : '';
    return `CREATE TABLE ${qualify(schemaName, name)} (\n  ${cols}${pk}\n);`;
  };

  const buildAlterStatements = (): string[] => {
    const stmts: string[] = [];
    const qTable = qualify(schemaName, tableName!);

    for (const col of columns) {
      if (col._status === 'new') {
        let line = `ALTER TABLE ${qTable} ADD COLUMN "${col.name}" ${col.dataType}`;
        if (!col.isNullable) line += ' NOT NULL';
        if (col.defaultValue) line += ` DEFAULT ${col.defaultValue}`;
        stmts.push(line + ';');
      } else if (col._status === 'deleted' && col._original) {
        stmts.push(`ALTER TABLE ${qTable} DROP COLUMN IF EXISTS "${col._original.name}";`);
      } else if (col._status === 'modified' && col._original) {
        const orig = col._original;
        if (orig.name !== col.name) {
          stmts.push(
            `ALTER TABLE ${qTable} RENAME COLUMN "${orig.name}" TO "${col.name}";`,
          );
        }
        if (orig.dataType !== col.dataType) {
          stmts.push(
            `ALTER TABLE ${qTable} ALTER COLUMN "${col.name}" TYPE ${col.dataType} USING "${col.name}"::${col.dataType};`,
          );
        }
        if (orig.isNullable !== col.isNullable) {
          if (col.isNullable) {
            stmts.push(`ALTER TABLE ${qTable} ALTER COLUMN "${col.name}" DROP NOT NULL;`);
          } else {
            stmts.push(`ALTER TABLE ${qTable} ALTER COLUMN "${col.name}" SET NOT NULL;`);
          }
        }
        if ((orig.defaultValue ?? '') !== col.defaultValue) {
          if (col.defaultValue) {
            stmts.push(
              `ALTER TABLE ${qTable} ALTER COLUMN "${col.name}" SET DEFAULT ${col.defaultValue};`,
            );
          } else {
            stmts.push(
              `ALTER TABLE ${qTable} ALTER COLUMN "${col.name}" DROP DEFAULT;`,
            );
          }
        }
      }
    }

    return stmts;
  };

  const save = async () => {
    if (!name.trim()) {
      setError('Table name is required.');
      return;
    }
    const activeCols = columns.filter((c) => c._status !== 'deleted');
    if (activeCols.length === 0) {
      setError('At least one column is required.');
      return;
    }
    for (const c of activeCols) {
      if (!c.name.trim()) {
        setError('All columns must have a name.');
        return;
      }
    }
    setLoading(true);
    setError(null);
    setInfo(null);
    try {
      if (!tableName) {
        await engine.exec(buildCreateSql());
        setInfo(`Table "${name}" created.`);
      } else {
        const stmts = buildAlterStatements();
        if (stmts.length === 0) {
          setInfo('No changes to apply.');
        } else {
          for (const stmt of stmts) {
            await engine.exec(stmt);
          }
          setInfo(`Table "${tableName}" updated (${stmts.length} change${stmts.length > 1 ? 's' : ''}).`);
        }
      }
      onSaved();
      setTimeout(() => onClose(), 800);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30 backdrop-blur-sm">
      <div className="flex max-h-[85vh] w-full max-w-3xl flex-col rounded-xl border border-base bg-surface shadow-2xl">
        <div className="flex items-center justify-between border-b border-base px-5 py-3">
          <h2 className="text-sm font-semibold text-primary">
            {tableName ? `Design: ${schemaName}.${tableName}` : `New Table in ${schemaName}`}
          </h2>
          <button onClick={onClose} className="text-muted hover:text-secondary">
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto px-5 py-4">
          <label className="mb-1 block text-xs font-medium text-muted">Table name</label>
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            disabled={!!tableName}
            className="mb-4 w-full rounded-md border border-base bg-elevated px-3 py-2 text-sm text-primary outline-none focus:border-accent disabled:bg-muted disabled:opacity-60"
            placeholder="e.g. employees"
          />

          {loading ? (
            <div className="flex items-center gap-2 py-8 text-sm text-muted">
              <Loader2 className="h-4 w-4 animate-spin" /> Loading schema…
            </div>
          ) : (
            <div>
              <div className="mb-2 flex items-center justify-between">
                <span className="text-xs font-medium text-muted">Columns</span>
                <button
                  onClick={addColumn}
                  className="flex items-center gap-1 rounded px-2 py-1 text-xs text-accent hover:bg-accent-light"
                >
                  <Plus className="h-3.5 w-3.5" /> Add column
                </button>
              </div>

              <div className="space-y-1.5">
                {columns.map((col, i) => {
                  const isDeleted = col._status === 'deleted';
                  const isNew = col._status === 'new';
                  return (
                    <div
                      key={i}
                      className={`flex items-center gap-2 rounded-md border px-2 py-1.5 ${
                        isDeleted
                          ? 'border-error bg-error-light opacity-50'
                          : isNew
                            ? 'border-success bg-success-light/50'
                            : 'border-base bg-muted/50'
                      }`}
                    >
                      <button
                        onClick={() => updateCol(i, { isPrimaryKey: !col.isPrimaryKey })}
                        disabled={isDeleted}
                        title="Primary key"
                        className={`rounded p-1 ${
                          col.isPrimaryKey
                            ? 'text-warning'
                            : 'text-faint hover:text-muted'
                        }`}
                      >
                        <KeyRound className="h-3.5 w-3.5" />
                      </button>
                      <input
                        value={col.name}
                        onChange={(e) => updateCol(i, { name: e.target.value })}
                        disabled={isDeleted}
                        placeholder="column name"
                        className="w-32 rounded border border-transparent bg-transparent px-1.5 py-1 text-sm text-primary outline-none hover:border-base focus:border-accent"
                      />
                      <select
                        value={col.dataType}
                        onChange={(e) => updateCol(i, { dataType: e.target.value })}
                        disabled={isDeleted}
                        className="rounded border border-base bg-elevated px-1.5 py-1 text-xs text-secondary outline-none focus:border-accent"
                      >
                        {TYPES.map((t) => (
                          <option key={t} value={t}>
                            {t}
                          </option>
                        ))}
                      </select>
                      <input
                        value={col.defaultValue}
                        onChange={(e) => updateCol(i, { defaultValue: e.target.value })}
                        disabled={isDeleted}
                        placeholder="default"
                        className="w-28 rounded border border-transparent bg-transparent px-1.5 py-1 text-xs text-secondary outline-none hover:border-base focus:border-accent"
                      />
                      <label className="flex items-center gap-1 text-xs text-secondary">
                        <input
                          type="checkbox"
                          checked={!col.isNullable}
                          onChange={(e) => updateCol(i, { isNullable: !e.target.checked })}
                          disabled={isDeleted}
                          className="accent-accent"
                        />
                        NOT NULL
                      </label>
                      <button
                        onClick={() => removeColumn(i)}
                        className="ml-auto rounded p-1 text-muted hover:text-error"
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </button>
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          {error && (
            <div className="mt-4 flex items-start gap-2 rounded-md border border-error bg-error-light px-3 py-2 text-xs text-error">
              <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
              <span className="break-all">{error}</span>
            </div>
          )}
          {info && (
            <div className="mt-4 rounded-md border border-success bg-success-light px-3 py-2 text-xs text-success">
              {info}
            </div>
          )}
        </div>

        <div className="flex items-center justify-end gap-2 border-t border-base px-5 py-3">
          <button
            onClick={onClose}
            className="rounded-md px-3 py-1.5 text-xs text-secondary hover:bg-hover"
          >
            Cancel
          </button>
          <button
            onClick={save}
            disabled={loading}
            className="flex items-center gap-1.5 rounded-md bg-accent px-4 py-1.5 text-xs font-semibold text-accent-contrast hover:bg-accent-hover disabled:opacity-40"
          >
            {loading ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
            {tableName ? 'Update' : 'Create'}
          </button>
        </div>
      </div>
    </div>
  );
}
