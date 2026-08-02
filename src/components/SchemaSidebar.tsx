import { useEffect, useState, useRef } from 'react';
import { engine, type SchemaTable, type TableSchema, type SchemaInfo } from '@/lib/engine';
import {
  Database,
  Table,
  KeyRound,
  Link2,
  ChevronRight,
  ChevronDown,
  RefreshCw,
  Plus,
  ListTree,
  Trash2,
  Eraser,
  FolderPlus,
  Folder,
  FolderOpen,
  X,
  AlertTriangle,
  Loader2,
  Download,
  Copy,
  Pencil,
} from 'lucide-react';

type Props = {
  tables: SchemaTable[];
  schemas: SchemaInfo[];
  activeSchema: string;
  loading: boolean;
  selectedTable: string | null;
  onSchemaChange: (name: string) => void;
  onSchemaCreated: (name: string) => void;
  onSelectTable: (name: string) => void;
  onRefresh: () => void;
  onNewTable: () => void;
  onOpenDesigner: (name: string) => void;
  onOpenData: (name: string) => void;
  onDropTable: (name: string) => void;
  onTruncateTable: (name: string) => void;
  onRenameTable: (oldName: string, newName: string) => void;
  onDuplicateTable: (sourceName: string, targetName: string, includeData: boolean) => void;
  onExportData: (name: string) => void;
};

export function SchemaSidebar({
  tables,
  schemas,
  activeSchema,
  loading,
  selectedTable,
  onSchemaChange,
  onSchemaCreated,
  onSelectTable,
  onRefresh,
  onNewTable,
  onOpenDesigner,
  onOpenData,
  onDropTable,
  onTruncateTable,
  onRenameTable,
  onDuplicateTable,
  onExportData,
}: Props) {
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [schemasCache, setSchemasCache] = useState<Record<string, TableSchema>>({});
  const [menuTable, setMenuTable] = useState<string | null>(null);
  const [showSchemaMenu, setShowSchemaMenu] = useState(false);
  const [newSchemaName, setNewSchemaName] = useState('');
  const [creatingSchema, setCreatingSchema] = useState(false);
  const [schemaError, setSchemaError] = useState<string | null>(null);
  const [confirmDropSchema, setConfirmDropSchema] = useState<string | null>(null);
  const [renameTable, setRenameTable] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');
  const [dupTable, setDupTable] = useState<string | null>(null);
  const [dupTarget, setDupTarget] = useState('');
  const [dupIncludeData, setDupIncludeData] = useState(true);
  const schemaMenuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      if (schemaMenuRef.current && !schemaMenuRef.current.contains(e.target as Node)) {
        setShowSchemaMenu(false);
      }
    };
    document.addEventListener('mousedown', onClick);
    return () => document.removeEventListener('mousedown', onClick);
  }, []);

  useEffect(() => {
    const load = async () => {
      const names = [...expanded];
      const next: Record<string, TableSchema> = {};
      for (const name of names) {
        try {
          next[name] = await engine.getTableSchema(name, activeSchema);
        } catch {
          /* table may be mid-DDL */
        }
      }
      setSchemasCache((prev) => ({ ...prev, ...next }));
    };
    load();
  }, [expanded, tables, activeSchema]);

  const toggle = (name: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  };

  const createSchema = async () => {
    const name = newSchemaName.trim();
    if (!name) return;
    setCreatingSchema(true);
    setSchemaError(null);
    try {
      await engine.createSchema(name);
      setNewSchemaName('');
      setShowSchemaMenu(false);
      onSchemaCreated(name);
    } catch (e) {
      setSchemaError(e instanceof Error ? e.message : String(e));
    } finally {
      setCreatingSchema(false);
    }
  };

  const dropSchema = async (name: string) => {
    try {
      await engine.dropSchema(name, true);
      setConfirmDropSchema(null);
      onSchemaChange('public');
      onRefresh();
    } catch (e) {
      setSchemaError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <aside className="flex h-full flex-col border-r border-base bg-surface">
      {/* Schema selector */}
      <div className="border-b border-base px-3 py-2.5" ref={schemaMenuRef}>
        <div className="mb-1 text-[10px] font-semibold uppercase tracking-wide text-muted">
          Schema
        </div>
        <div className="relative">
          <button
            onClick={() => setShowSchemaMenu((v) => !v)}
            className="flex w-full items-center justify-between rounded-md border border-base bg-elevated px-2.5 py-1.5 text-sm text-primary hover:bg-hover"
          >
            <span className="flex items-center gap-2">
              <Folder className="h-3.5 w-3.5 text-accent" />
              <span className="font-medium">{activeSchema}</span>
              <span className="text-xs text-muted">
                ({tables.length})
              </span>
            </span>
            <ChevronDown className="h-3.5 w-3.5 text-muted" />
          </button>

          {showSchemaMenu && (
            <div className="absolute left-0 right-0 z-50 mt-1 max-h-72 overflow-y-auto rounded-md border border-base bg-surface shadow-xl">
              {schemas.map((s) => (
                <button
                  key={s.name}
                  onClick={() => {
                    onSchemaChange(s.name);
                    setShowSchemaMenu(false);
                  }}
                  className={`flex w-full items-center justify-between px-3 py-2 text-sm hover:bg-hover ${
                    s.name === activeSchema ? 'bg-accent-light text-accent' : 'text-secondary'
                  }`}
                >
                  <span className="flex items-center gap-2">
                    {s.name === activeSchema ? (
                      <FolderOpen className="h-3.5 w-3.5" />
                    ) : (
                      <Folder className="h-3.5 w-3.5" />
                    )}
                    {s.name}
                  </span>
                  <span className="text-xs text-muted">{s.tableCount} tables</span>
                </button>
              ))}

              <div className="border-t border-base p-2">
                <div className="flex items-center gap-1.5">
                  <input
                    value={newSchemaName}
                    onChange={(e) => setNewSchemaName(e.target.value)}
                    placeholder="New schema name…"
                    className="flex-1 rounded border border-base bg-elevated px-2 py-1 text-xs text-primary outline-none focus:border-accent"
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') createSchema();
                    }}
                  />
                  <button
                    onClick={createSchema}
                    disabled={creatingSchema || !newSchemaName.trim()}
                    className="flex items-center rounded bg-accent px-2 py-1 text-xs text-accent-contrast hover:bg-accent-hover disabled:opacity-40"
                  >
                    {creatingSchema ? (
                      <Loader2 className="h-3.5 w-3.5 animate-spin" />
                    ) : (
                      <FolderPlus className="h-3.5 w-3.5" />
                    )}
                  </button>
                </div>
                {schemaError && (
                  <p className="mt-1 text-[10px] text-error">{schemaError}</p>
                )}
              </div>

              {activeSchema !== 'public' && (
                <div className="border-t border-base p-2">
                  <button
                    onClick={() => {
                      setConfirmDropSchema(activeSchema);
                      setShowSchemaMenu(false);
                    }}
                    className="flex w-full items-center gap-1.5 rounded px-2 py-1 text-xs text-error hover:bg-error-light"
                  >
                    <Trash2 className="h-3.5 w-3.5" /> Drop schema "{activeSchema}"
                  </button>
                </div>
              )}
            </div>
          )}
        </div>

        {confirmDropSchema && (
          <div className="mt-2 flex items-start gap-2 rounded-md border border-error bg-error-light px-2.5 py-2 text-xs text-error">
            <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
            <span>
              Drop schema "{confirmDropSchema}" and all its tables?
              <div className="mt-1.5 flex gap-2">
                <button
                  onClick={() => dropSchema(confirmDropSchema)}
                  className="rounded bg-error px-2 py-0.5 text-accent-contrast hover:opacity-80"
                >
                  Drop
                </button>
                <button
                  onClick={() => setConfirmDropSchema(null)}
                  className="rounded px-2 py-0.5 hover:bg-hover"
                >
                  Cancel
                </button>
              </div>
            </span>
          </div>
        )}
      </div>

      {/* Tables header */}
      <div className="flex items-center justify-between border-b border-base px-4 py-2.5">
        <div className="flex items-center gap-2">
          <Database className="h-4 w-4 text-accent" />
          <span className="text-sm font-semibold text-primary">Tables</span>
          <span className="rounded-full bg-muted px-1.5 py-0.5 text-[10px] text-muted">
            {tables.length}
          </span>
        </div>
        <div className="flex items-center gap-1">
          <button
            onClick={onRefresh}
            title="Refresh"
            className="rounded p-1 text-muted hover:bg-hover hover:text-secondary"
          >
            <RefreshCw className={`h-3.5 w-3.5 ${loading ? 'animate-spin' : ''}`} />
          </button>
          <button
            onClick={onNewTable}
            title="New table"
            className="rounded p-1 text-muted hover:bg-hover hover:text-accent"
          >
            <Plus className="h-4 w-4" />
          </button>
        </div>
      </div>

      {/* Tables list */}
      <div className="flex-1 overflow-y-auto px-2 py-2">
        {loading && tables.length === 0 ? (
          <div className="px-2 py-4 text-xs text-muted">Loading tables…</div>
        ) : tables.length === 0 ? (
          <div className="px-2 py-4 text-xs text-muted">
            No tables in "{activeSchema}". Click + to create one.
          </div>
        ) : (
          <ul className="space-y-0.5">
            {tables.map((t) => {
              const isOpen = expanded.has(t.name);
              const isSelected = selectedTable === t.name;
              const schema = schemasCache[t.name];
              return (
                <li key={`${t.schema}.${t.name}`}>
                  <div
                    className={`group relative flex items-center gap-1 rounded-md px-1 py-1 ${
                      isSelected ? 'bg-accent-light' : 'hover:bg-hover'
                    }`}
                  >
                    <button
                      onClick={() => toggle(t.name)}
                      className="rounded p-0.5 text-muted hover:text-secondary"
                    >
                      {isOpen ? (
                        <ChevronDown className="h-3.5 w-3.5" />
                      ) : (
                        <ChevronRight className="h-3.5 w-3.5" />
                      )}
                    </button>
                    <button
                      onClick={() => onSelectTable(t.name)}
                      className="flex flex-1 items-center gap-2 text-left"
                    >
                      <Table className={`h-3.5 w-3.5 ${isSelected ? 'text-accent' : 'text-muted'}`} />
                      <span className={`text-sm ${isSelected ? 'font-medium text-accent' : 'text-secondary'}`}>
                        {t.name}
                      </span>
                    </button>
                    <div className="hidden items-center gap-0.5 group-hover:flex">
                      <button
                        title="Browse data"
                        onClick={() => onOpenData(t.name)}
                        className="rounded p-1 text-muted hover:bg-muted hover:text-accent"
                      >
                        <ListTree className="h-3.5 w-3.5" />
                      </button>
                      <button
                        title="Design"
                        onClick={() => onOpenDesigner(t.name)}
                        className="rounded p-1 text-muted hover:bg-muted hover:text-accent"
                      >
                        <KeyRound className="h-3.5 w-3.5" />
                      </button>
                      <button
                        title="More"
                        onClick={() => setMenuTable(menuTable === t.name ? null : t.name)}
                        className="rounded p-1 text-muted hover:bg-muted hover:text-secondary"
                      >
                        <ChevronRight className="h-3.5 w-3.5 rotate-90" />
                      </button>
                    </div>
                  </div>

                  {menuTable === t.name && (
                    <div className="fixed right-4 top-16 z-50 rounded-md border border-base bg-surface py-1 shadow-lg">
                      <button
                        onClick={() => {
                          onOpenData(t.name);
                          setMenuTable(null);
                        }}
                        className="flex w-full items-center gap-2 px-3 py-1.5 text-xs text-secondary hover:bg-hover"
                      >
                        <ListTree className="h-3.5 w-3.5 text-accent" /> Browse Data
                      </button>
                      <button
                        onClick={() => {
                          onOpenDesigner(t.name);
                          setMenuTable(null);
                        }}
                        className="flex w-full items-center gap-2 px-3 py-1.5 text-xs text-secondary hover:bg-hover"
                      >
                        <KeyRound className="h-3.5 w-3.5 text-accent" /> Design Table
                      </button>
                      <div className="my-1 border-t border-base" />
                      <button
                        onClick={() => {
                          setRenameTable(t.name);
                          setRenameValue(t.name);
                          setMenuTable(null);
                        }}
                        className="flex w-full items-center gap-2 px-3 py-1.5 text-xs text-secondary hover:bg-hover"
                      >
                        <Pencil className="h-3.5 w-3.5 text-muted" /> Rename Table…
                      </button>
                      <button
                        onClick={() => {
                          setDupTable(t.name);
                          setDupTarget(`${t.name}_copy`);
                          setMenuTable(null);
                        }}
                        className="flex w-full items-center gap-2 px-3 py-1.5 text-xs text-secondary hover:bg-hover"
                      >
                        <Copy className="h-3.5 w-3.5 text-muted" /> Duplicate Table…
                      </button>
                      <button
                        onClick={() => {
                          onExportData(t.name);
                          setMenuTable(null);
                        }}
                        className="flex w-full items-center gap-2 px-3 py-1.5 text-xs text-secondary hover:bg-hover"
                      >
                        <Download className="h-3.5 w-3.5 text-muted" /> Export Data (CSV)
                      </button>
                      <div className="my-1 border-t border-base" />
                      <button
                        onClick={() => {
                          onTruncateTable(t.name);
                          setMenuTable(null);
                        }}
                        className="flex w-full items-center gap-2 px-3 py-1.5 text-xs text-warning hover:bg-warning-light"
                      >
                        <Eraser className="h-3.5 w-3.5" /> Truncate
                      </button>
                      <button
                        onClick={() => {
                          if (confirm(`Drop table "${t.name}"? This cannot be undone.`)) {
                            onDropTable(t.name);
                          }
                          setMenuTable(null);
                        }}
                        className="flex w-full items-center gap-2 px-3 py-1.5 text-xs text-error hover:bg-error-light"
                      >
                        <Trash2 className="h-3.5 w-3.5" /> Drop Table
                      </button>
                    </div>
                  )}

                  {isOpen && schema && (
                    <ul className="ml-6 mt-0.5 space-y-0.5 border-l border-base pl-2">
                      {schema.columns.map((col) => (
                        <li
                          key={col.name}
                          className="flex items-center gap-1.5 py-0.5 text-xs"
                        >
                          {col.isPrimaryKey ? (
                            <KeyRound className="h-3 w-3 text-warning" />
                          ) : (
                            <span className="h-3 w-3" />
                          )}
                          <span className="text-secondary">{col.name}</span>
                          <span className="text-muted">{col.dataType}</span>
                          {!col.isNullable && (
                            <span className="text-muted">· NN</span>
                          )}
                        </li>
                      ))}
                      {schema.foreignKeys.map((fk) => (
                        <li
                          key={fk.name}
                          className="flex items-center gap-1.5 py-0.5 text-xs text-muted"
                        >
                          <Link2 className="h-3 w-3 text-success" />
                          <span>
                            {fk.columnName} → {fk.referencesTable}.{fk.referencesColumn}
                          </span>
                        </li>
                      ))}
                      <li className="py-0.5 text-[10px] text-muted">
                        {schema.rowCount.toLocaleString()} rows
                      </li>
                    </ul>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>

      {/* Rename dialog */}
      {renameTable && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30 backdrop-blur-sm">
          <div className="w-80 rounded-lg border border-base bg-surface p-4 shadow-xl">
            <h3 className="mb-3 text-sm font-semibold text-primary">Rename Table</h3>
            <input
              value={renameValue}
              onChange={(e) => setRenameValue(e.target.value)}
              autoFocus
              onKeyDown={(e) => {
                if (e.key === 'Enter' && renameValue.trim()) {
                  onRenameTable(renameTable, renameValue.trim());
                  setRenameTable(null);
                }
                if (e.key === 'Escape') setRenameTable(null);
              }}
              className="mb-3 w-full rounded-md border border-base bg-elevated px-3 py-2 text-sm text-primary outline-none focus:border-accent"
            />
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setRenameTable(null)}
                className="rounded px-3 py-1.5 text-xs text-secondary hover:bg-hover"
              >
                Cancel
              </button>
              <button
                onClick={() => {
                  if (renameValue.trim()) {
                    onRenameTable(renameTable, renameValue.trim());
                    setRenameTable(null);
                  }
                }}
                className="rounded bg-accent px-3 py-1.5 text-xs font-semibold text-accent-contrast hover:bg-accent-hover"
              >
                Rename
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Duplicate dialog */}
      {dupTable && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30 backdrop-blur-sm">
          <div className="w-80 rounded-lg border border-base bg-surface p-4 shadow-xl">
            <h3 className="mb-3 text-sm font-semibold text-primary">Duplicate Table</h3>
            <p className="mb-2 text-xs text-muted">Source: {dupTable}</p>
            <input
              value={dupTarget}
              onChange={(e) => setDupTarget(e.target.value)}
              placeholder="New table name…"
              autoFocus
              onKeyDown={(e) => {
                if (e.key === 'Enter' && dupTarget.trim()) {
                  onDuplicateTable(dupTable, dupTarget.trim(), dupIncludeData);
                  setDupTable(null);
                }
                if (e.key === 'Escape') setDupTable(null);
              }}
              className="mb-3 w-full rounded-md border border-base bg-elevated px-3 py-2 text-sm text-primary outline-none focus:border-accent"
            />
            <label className="mb-3 flex items-center gap-2 text-xs text-secondary">
              <input
                type="checkbox"
                checked={dupIncludeData}
                onChange={(e) => setDupIncludeData(e.target.checked)}
                className="accent-accent"
              />
              Include data
            </label>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setDupTable(null)}
                className="rounded px-3 py-1.5 text-xs text-secondary hover:bg-hover"
              >
                Cancel
              </button>
              <button
                onClick={() => {
                  if (dupTarget.trim()) {
                    onDuplicateTable(dupTable, dupTarget.trim(), dupIncludeData);
                    setDupTable(null);
                  }
                }}
                className="rounded bg-accent px-3 py-1.5 text-xs font-semibold text-accent-contrast hover:bg-accent-hover"
              >
                Duplicate
              </button>
            </div>
          </div>
        </div>
      )}
    </aside>
  );
}
