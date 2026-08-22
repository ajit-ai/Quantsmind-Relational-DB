import { useEffect, useState, useCallback, useRef } from 'react';
import { engine, type QueryResult, type SchemaTable, type SchemaInfo } from '@/lib/engine';
import { SchemaSidebar } from '@/components/SchemaSidebar';
import { SqlEditor } from '@/components/SqlEditor';
import { ResultsPanel } from '@/components/ResultsPanel';
import { TableDesigner } from '@/components/TableDesigner';
import { DataBrowser } from '@/components/DataBrowser';
import { AcidDemo } from '@/components/AcidDemo';
import { ThemePicker } from '@/components/ThemePicker';
import { SavedQueries } from '@/components/SavedQueries';
import { MenuBar, type MenuAction } from '@/components/MenuBar';
import { ShortcutsDialog, AboutDialog } from '@/components/Dialogs';
import { useTheme } from '@/lib/theme';
import {
  Database, Loader2, Terminal, Table2, ShieldCheck, X, PanelLeft,
} from 'lucide-react';

type Tab =
  | { kind: 'query'; sql?: string }
  | { kind: 'data'; table: string; schema: string }
  | { kind: 'acid' };

export default function App() {
  const { toggleMode } = useTheme();
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [tables, setTables] = useState<SchemaTable[]>([]);
  const [schemas, setSchemas] = useState<SchemaInfo[]>([]);
  const [activeSchema, setActiveSchema] = useState('public');
  const [loadingTables, setLoadingTables] = useState(false);
  const [selectedTable, setSelectedTable] = useState<string | null>(null);
  const [result, setResult] = useState<QueryResult | null>(null);
  const [lastSql, setLastSql] = useState<string | null>(null);
  const [designerOpen, setDesignerOpen] = useState(false);
  const [designerTable, setDesignerTable] = useState<string | null>(null);
  const [designerSchema, setDesignerSchema] = useState('public');
  const [tabs, setTabs] = useState<Tab[]>([{ kind: 'query' }]);
  const [activeTab, setActiveTab] = useState(0);
  const [sidebarWidth, setSidebarWidth] = useState(288);
  const [editorHeight, setEditorHeight] = useState(280);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [savedOpen, setSavedOpen] = useState(false);
  const [autoCommit, setAutoCommit] = useState(true);
  const [inTransaction, setInTransaction] = useState(false);
  const [zoom, setZoom] = useState(1);
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  const [aboutOpen, setAboutOpen] = useState(false);
  const [editorAction, setEditorAction] = useState<{ type: string; nonce: number } | null>(null);

  const refreshSchemas = useCallback(async () => {
    try {
      const s = await engine.listSchemas();
      setSchemas(s);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const refreshTables = useCallback(async (schema?: string) => {
    const s = schema ?? activeSchema;
    setLoadingTables(true);
    try {
      const t = await engine.listTables(s);
      setTables(t);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoadingTables(false);
    }
  }, [activeSchema]);

  useEffect(() => {
    (async () => {
      try {
        await engine.init();
        await refreshSchemas();
        await refreshTables();
        setReady(true);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    })();
  }, [refreshSchemas, refreshTables]);

  const handleSchemaChange = async (schema: string) => {
    setActiveSchema(schema);
    setSelectedTable(null);
    await refreshTables(schema);
  };

  const handleSchemaCreated = async (schema: string) => {
    await refreshSchemas();
    setActiveSchema(schema);
    await refreshTables(schema);
  };

  const handleResult = (r: QueryResult, sql: string) => {
    setResult(r);
    setLastSql(sql);
  };

  const openQueryTab = (sql?: string) => {
    const idx = tabs.findIndex((t) => t.kind === 'query');
    if (idx >= 0) {
      setActiveTab(idx);
      if (sql) {
        setTabs((prev) => prev.map((t, i) => (i === idx ? { kind: 'query', sql } : t)));
      }
    } else {
      setTabs((prev) => [...prev, { kind: 'query', sql }]);
      setActiveTab(tabs.length);
    }
  };

  const openDataTab = (table: string, schema: string) => {
    const existing = tabs.findIndex((t) => t.kind === 'data' && t.table === table && t.schema === schema);
    if (existing >= 0) {
      setActiveTab(existing);
    } else {
      setTabs((prev) => [...prev, { kind: 'data', table, schema }]);
      setActiveTab(tabs.length);
    }
  };

  const openAcidTab = () => {
    const idx = tabs.findIndex((t) => t.kind === 'acid');
    if (idx >= 0) {
      setActiveTab(idx);
    } else {
      setTabs((prev) => [...prev, { kind: 'acid' }]);
      setActiveTab(tabs.length);
    }
  };

  const closeTab = (index: number) => {
    setTabs((prev) => {
      const next = prev.filter((_, i) => i !== index);
      if (next.length === 0) return [{ kind: 'query' }];
      return next;
    });
    setActiveTab((prev) => Math.max(0, Math.min(prev, tabs.length - 2)));
  };

  const openDesigner = (table: string | null, schema: string) => {
    setDesignerTable(table);
    setDesignerSchema(schema);
    setDesignerOpen(true);
  };

  const currentTab = tabs[activeTab] ?? { kind: 'query' as const };

  // Sidebar resize
  const sidebarDragRef = useRef(false);
  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (sidebarDragRef.current) {
        setSidebarWidth(Math.max(200, Math.min(500, e.clientX)));
      }
    };
    const onUp = () => {
      sidebarDragRef.current = false;
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    return () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
  }, []);

  // Editor/results resize
  const editorDragRef = useRef(false);
  const containerRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (editorDragRef.current && containerRef.current) {
        const rect = containerRef.current.getBoundingClientRect();
        const offset = e.clientY - rect.top;
        setEditorHeight(Math.max(80, Math.min(rect.height - 80, offset)));
      }
    };
    const onUp = () => {
      editorDragRef.current = false;
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    return () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
  }, []);

  // Transaction management
  const handleToggleAutoCommit = async () => {
    if (autoCommit && inTransaction) {
      await engine.rollbackTransaction();
      setInTransaction(false);
    }
    setAutoCommit((v) => !v);
  };

  const handleCommit = async () => {
    await engine.commitTransaction();
    setInTransaction(false);
    await refreshSchemas();
    await refreshTables();
  };

  const handleRollback = async () => {
    await engine.rollbackTransaction();
    setInTransaction(false);
    await refreshSchemas();
    await refreshTables();
  };

  // Menu actions
  const handleMenuAction = (action: MenuAction) => {
    switch (action) {
      case 'new-query':
        openQueryTab('');
        setEditorAction({ type: 'new-query', nonce: Date.now() });
        break;
      case 'open-file':
        setEditorAction({ type: 'open-file', nonce: Date.now() });
        break;
      case 'save-query':
        setEditorAction({ type: 'save-query', nonce: Date.now() });
        break;
      case 'save-as':
        setEditorAction({ type: 'save-as', nonce: Date.now() });
        break;
      case 'export-sql':
        setEditorAction({ type: 'export-sql', nonce: Date.now() });
        break;
      case 'export-csv':
        // handled by results panel — trigger via keyboard
        break;
      case 'print':
        window.print();
        break;
      case 'undo':
        document.execCommand('undo');
        break;
      case 'redo':
        document.execCommand('redo');
        break;
      case 'cut':
        document.execCommand('cut');
        break;
      case 'copy':
        document.execCommand('copy');
        break;
      case 'paste':
        document.execCommand('paste');
        break;
      case 'find':
        setEditorAction({ type: 'find', nonce: Date.now() });
        break;
      case 'replace':
        setEditorAction({ type: 'replace', nonce: Date.now() });
        break;
      case 'select-all':
        setEditorAction({ type: 'select-all', nonce: Date.now() });
        break;
      case 'delete':
        setEditorAction({ type: 'delete', nonce: Date.now() });
        break;
      case 'format-sql':
        setEditorAction({ type: 'format-sql', nonce: Date.now() });
        break;
      case 'run-all':
        setEditorAction({ type: 'run-all', nonce: Date.now() });
        break;
      case 'run-selection':
        setEditorAction({ type: 'run-selection', nonce: Date.now() });
        break;
      case 'run-statement':
        setEditorAction({ type: 'run-statement', nonce: Date.now() });
        break;
      case 'commit':
        handleCommit();
        break;
      case 'rollback':
        handleRollback();
        break;
      case 'toggle-autocommit':
        handleToggleAutoCommit();
        break;
      case 'refresh':
        refreshSchemas();
        refreshTables();
        break;
      case 'toggle-sidebar':
        setSidebarOpen((v) => !v);
        break;
      case 'toggle-saved':
        setSavedOpen((v) => !v);
        break;
      case 'toggle-theme':
        toggleMode();
        break;
      case 'zoom-in':
        setZoom((z) => Math.min(2, z + 0.1));
        break;
      case 'zoom-out':
        setZoom((z) => Math.max(0.5, z - 0.1));
        break;
      case 'reset-zoom':
        setZoom(1);
        break;
      case 'shortcuts':
        setShortcutsOpen(true);
        break;
      case 'about':
        setAboutOpen(true);
        break;
      case 'acid-demo':
        openAcidTab();
        break;
      case 'docs':
        window.open('https://www.postgresql.org/docs/', '_blank');
        break;
    }
  };

  // Global keyboard shortcuts
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'b') {
        e.preventDefault();
        setSidebarOpen((v) => !v);
      }
      if ((e.metaKey || e.ctrlKey) && e.key === '/') {
        e.preventDefault();
        setShortcutsOpen(true);
      }
      if (e.key === 'F5') {
        e.preventDefault();
        refreshSchemas();
        refreshTables();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [refreshSchemas, refreshTables]);

  if (!ready) {
    return (
      <div className="flex h-screen flex-col items-center justify-center bg-base text-secondary">
        {error ? (
          <div className="max-w-md text-center">
            <p className="mb-2 text-lg font-semibold text-error">Failed to start database</p>
            <p className="text-sm text-muted">{error}</p>
          </div>
        ) : (
          <div className="flex flex-col items-center gap-3">
            <Loader2 className="h-8 w-8 animate-spin text-accent" />
            <p className="text-sm text-muted">Starting database engine…</p>
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="flex h-screen flex-col bg-base text-primary" style={{ fontSize: `${zoom}rem` }}>
      {/* Menu bar */}
      <MenuBar
        onAction={handleMenuAction}
        autoCommit={autoCommit}
        inTransaction={inTransaction}
        hasSelection={true}
        hasSql={!!(currentTab.kind === 'query' && (currentTab as { kind: 'query'; sql?: string }).sql)}
      />

      {/* Branding Header - Moved to top right corner */}
      <div className="flex items-center justify-end border-b border-base bg-surface px-4 py-2 shadow-sm">
        <div className="flex items-center gap-2.5">
          <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-accent">
            <Database className="h-4 w-4 text-accent-contrast" />
          </div>
          <div>
            <h1 className="text-sm font-bold tracking-tight text-primary">QuantsMind Relational Studio</h1>
          </div>
        </div>
      </div>

      {/* Top bar - removed branding */}
      <header className="flex items-center justify-between border-b border-base bg-surface px-4 py-2 shadow-sm">
        <div className="flex items-center gap-2.5">
          <button
            onClick={() => setSidebarOpen((v) => !v)}
            className="rounded-md p-1.5 text-muted hover:bg-hover hover:text-secondary"
            title="Toggle sidebar (Ctrl+B)"
          >
            <PanelLeft className="h-4 w-4" />
          </button>
        </div>
        <div className="flex items-center gap-2">
          <span className="hidden items-center gap-1.5 rounded-full border border-success bg-success-light px-2.5 py-1 text-xs text-success sm:flex">
            <span className="h-1.5 w-1.5 rounded-full bg-success" />
            Connected
          </span>
          {!autoCommit && (
            <span className={`flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs ${
              inTransaction
                ? 'border-warning bg-warning-light text-warning'
                : 'border-base text-muted'
            }`}>
              <span className={`h-1.5 w-1.5 rounded-full ${inTransaction ? 'bg-warning' : 'bg-muted'}`} />
              {inTransaction ? 'Transaction' : 'Manual'}
            </span>
          )}
          <button
            onClick={openAcidTab}
            className="flex items-center gap-1.5 rounded-md border border-base px-2.5 py-1 text-xs text-secondary hover:bg-hover"
          >
            <ShieldCheck className="h-3.5 w-3.5 text-accent" /> ACID
          </button>
          <ThemePicker />
        </div>
      </header>

      {/* Main layout */}
      <div className="flex flex-1 overflow-hidden">
        {sidebarOpen && (
          <>
            <div style={{ width: sidebarWidth }} className="shrink-0">
              <SchemaSidebar
                tables={tables}
                schemas={schemas}
                activeSchema={activeSchema}
                loading={loadingTables}
                selectedTable={selectedTable}
                onSchemaChange={handleSchemaChange}
                onSchemaCreated={handleSchemaCreated}
                onSelectTable={(name) => {
                  setSelectedTable(name);
                  openQueryTab(`SELECT * FROM "${activeSchema}"."${name}" LIMIT 100;`);
                }}
                onRefresh={() => {
                  refreshSchemas();
                  refreshTables();
                }}
                onNewTable={() => openDesigner(null, activeSchema)}
                onOpenDesigner={(name) => openDesigner(name, activeSchema)}
                onOpenData={(name) => openDataTab(name, activeSchema)}
                onDropTable={async (name) => {
                  await engine.dropTable(name, activeSchema);
                  await refreshTables();
                  await refreshSchemas();
                }}
                onTruncateTable={async (name) => {
                  await engine.truncateTable(name, activeSchema);
                  await refreshTables();
                }}
                onRenameTable={async (oldName, newName) => {
                  await engine.renameTable(oldName, newName, activeSchema);
                  await refreshTables();
                }}
                onDuplicateTable={async (sourceName, targetName, includeData) => {
                  await engine.duplicateTable(sourceName, targetName, activeSchema, includeData);
                  await refreshTables();
                  await refreshSchemas();
                }}
                onExportData={async (name) => {
                  const csv = await engine.exportTableData(name, activeSchema);
                  const blob = new Blob([csv], { type: 'text/csv;charset=utf-8;' });
                  const url = URL.createObjectURL(blob);
                  const a = document.createElement('a');
                  a.href = url;
                  a.download = `${name}_export_${Date.now()}.csv`;
                  a.click();
                  URL.revokeObjectURL(url);
                }}
              />
            </div>
            <div
              onMouseDown={() => {
                sidebarDragRef.current = true;
                document.body.style.cursor = 'col-resize';
                document.body.style.userSelect = 'none';
              }}
              className="w-1 cursor-col-resize bg-border-base hover:bg-accent"
            />
          </>
        )}

        {/* Center: tabs + content */}
        <div className="flex flex-1 flex-col overflow-hidden">
          {/* Tab bar */}
          <div className="flex items-center border-b border-base bg-surface">
            {tabs.map((tab, i) => (
              <button
                key={i}
                onClick={() => setActiveTab(i)}
                className={`group flex items-center gap-2 border-r border-base px-4 py-2.5 text-xs transition-colors ${
                  i === activeTab
                    ? 'bg-muted text-accent'
                    : 'text-secondary hover:bg-muted hover:text-primary'
                }`}
              >
                {tab.kind === 'query' && <Terminal className="h-3.5 w-3.5" />}
                {tab.kind === 'data' && <Table2 className="h-3.5 w-3.5" />}
                {tab.kind === 'acid' && <ShieldCheck className="h-3.5 w-3.5" />}
                <span>
                  {tab.kind === 'query'
                    ? 'Query'
                    : tab.kind === 'data'
                      ? `${tab.schema}.${tab.table}`
                      : 'ACID'}
                </span>
                {tabs.length > 1 && (
                  <span
                    onClick={(e) => {
                      e.stopPropagation();
                      closeTab(i);
                    }}
                    className="ml-1 rounded p-0.5 text-muted opacity-0 hover:bg-muted hover:text-primary group-hover:opacity-100"
                  >
                    <X className="h-3 w-3" />
                  </span>
                )}
              </button>
            ))}
          </div>

          {/* Tab content */}
          <div className="flex flex-1 overflow-hidden">
            {savedOpen && currentTab.kind === 'query' && (
              <SavedQueries
                onLoad={(sql) => {
                  openQueryTab(sql);
                  setSavedOpen(false);
                }}
              />
            )}
            {currentTab.kind === 'query' && (
              <div ref={containerRef} className="flex flex-1 flex-col overflow-hidden">
                <div style={{ height: editorHeight }} className="min-h-20 shrink-0 overflow-hidden border-b border-base">
                  <SqlEditor
                    initialSql={currentTab.sql}
                    onResult={handleResult}
                    onSchemaChanged={() => {
                      refreshSchemas();
                      refreshTables();
                    }}
                    onOpenSaved={() => setSavedOpen(true)}
                    autoCommit={autoCommit}
                    onToggleAutoCommit={handleToggleAutoCommit}
                    onCommit={handleCommit}
                    onRollback={handleRollback}
                    inTransaction={inTransaction}
                    externalAction={editorAction}
                  />
                </div>
                <div
                  onMouseDown={() => {
                    editorDragRef.current = true;
                    document.body.style.cursor = 'row-resize';
                    document.body.style.userSelect = 'none';
                  }}
                  className="h-1.5 cursor-row-resize bg-border-base hover:bg-accent"
                />
                <div className="flex-1 overflow-hidden">
                  <ResultsPanel result={result} sql={lastSql} />
                </div>
              </div>
            )}
            {currentTab.kind === 'data' && (
              <DataBrowser
                tableName={currentTab.table}
                schemaName={currentTab.schema}
                onClose={() => closeTab(activeTab)}
                onSchemaChanged={() => {
                  refreshSchemas();
                  refreshTables();
                }}
              />
            )}
            {currentTab.kind === 'acid' && <AcidDemo />}
          </div>
        </div>
      </div>

      <TableDesigner
        open={designerOpen}
        tableName={designerTable}
        schemaName={designerSchema}
        onClose={() => setDesignerOpen(false)}
        onSaved={() => {
          refreshSchemas();
          refreshTables();
        }}
      />

      <ShortcutsDialog open={shortcutsOpen} onClose={() => setShortcutsOpen(false)} />
      <AboutDialog open={aboutOpen} onClose={() => setAboutOpen(false)} />

      {/* Status bar */}
      <footer className="flex items-center justify-between border-t border-base bg-surface px-4 py-1.5 text-[11px] text-muted">
        <div className="flex items-center gap-4">
          <span className="flex items-center gap-1.5">
            <Database className="h-3 w-3 text-accent" />
            <span>IndexedDB (PGlite)</span>
          </span>
          <span>
            Schema: <span className="text-secondary">{activeSchema}</span>
          </span>
          <span>
            Tables: <span className="text-secondary">{tables.length}</span>
          </span>
          <span>
            Auto-Commit: <span className={autoCommit ? 'text-success' : 'text-warning'}>{autoCommit ? 'ON' : 'OFF'}</span>
          </span>
          {inTransaction && (
            <span className="flex items-center gap-1 text-warning">
              <span className="h-1.5 w-1.5 rounded-full bg-warning" />
              Transaction active
            </span>
          )}
        </div>
        <div className="flex items-center gap-3">
          {result && (
            <span>
              Last: <span className="text-secondary">{result.command}</span> · {result.rowCount} rows · {result.durationMs}ms
            </span>
          )}
          <span>Zoom: {Math.round(zoom * 100)}%</span>
          <span>PGlite · PostgreSQL WASM</span>
        </div>
      </footer>
    </div>
  );
}
