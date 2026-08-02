import { useState, useCallback } from 'react';
import { type QueryResult } from '@/lib/engine';
import { Table2, Inbox, Clock, Rows3, CheckCircle2, Download, Copy, Clipboard } from 'lucide-react';

type Props = {
  result: QueryResult | null;
  sql: string | null;
};

export function ResultsPanel({ result, sql }: Props) {
  const [page, setPage] = useState(0);
  const [copiedCell, setCopiedCell] = useState<string | null>(null);
  const pageSize = 50;

  const copyToClipboard = useCallback((text: string, label?: string) => {
    navigator.clipboard.writeText(text).then(() => {
      setCopiedCell(label ?? 'copied');
      setTimeout(() => setCopiedCell(null), 1500);
    });
  }, []);

  if (!result) {
    return (
      <div className="flex h-full flex-col items-center justify-center bg-base text-muted">
        <Inbox className="mb-3 h-10 w-10 text-faint" />
        <span className="text-sm font-medium text-secondary">No results yet</span>
        <span className="mt-1 text-xs text-muted">
          Write a query and press Run (or ⌘↵)
        </span>
      </div>
    );
  }

  const totalPages = Math.max(1, Math.ceil(result.rows.length / pageSize));
  const currentPage = Math.min(page, totalPages - 1);
  const start = currentPage * pageSize;
  const pageRows = result.rows.slice(start, start + pageSize);

  const formatCell = (val: unknown): string => {
    if (val === null) return 'NULL';
    if (val === undefined) return '';
    if (val instanceof Date) return val.toISOString();
    if (typeof val === 'object') return JSON.stringify(val);
    return String(val);
  };

  const exportCsv = () => {
    const escape = (s: string) => {
      if (/[",\n]/.test(s)) return `"${s.replace(/"/g, '""')}"`;
      return s;
    };
    const header = result.columns.map(escape).join(',');
    const body = result.rows
      .map((row) => result.columns.map((col) => escape(formatCell(row[col]))).join(','))
      .join('\n');
    const csv = `${header}\n${body}`;
    const blob = new Blob([csv], { type: 'text/csv;charset=utf-8;' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `query_result_${Date.now()}.csv`;
    a.click();
    URL.revokeObjectURL(url);
  };

  const copyRow = (row: Record<string, unknown>) => {
    const line = result.columns.map((col) => formatCell(row[col])).join('\t');
    copyToClipboard(line, 'row');
  };

  const copyColumn = (col: string) => {
    const values = result.rows.map((row) => formatCell(row[col])).join('\n');
    copyToClipboard(values, col);
  };

  return (
    <div className="flex h-full flex-col bg-surface">
      <div className="flex items-center justify-between border-b border-base bg-muted px-3 py-2">
        <div className="flex items-center gap-3">
          <div className="flex items-center gap-1.5">
            <Table2 className="h-3.5 w-3.5 text-accent" />
            <span className="text-xs font-semibold uppercase tracking-wide text-secondary">
              {result.command ?? 'Result'}
            </span>
          </div>
          <div className="flex items-center gap-3 text-xs text-muted">
            <span className="flex items-center gap-1">
              <Rows3 className="h-3 w-3" />
              {result.rowCount.toLocaleString()} row(s)
            </span>
            <span className="flex items-center gap-1">
              <Clock className="h-3 w-3" />
              {result.durationMs} ms
            </span>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {result.rows.length > 0 && result.columns.length > 0 && (
            <>
              <button
                onClick={() => {
                  const tsv = [
                    result.columns.join('\t'),
                    ...result.rows.map((row) => result.columns.map((col) => formatCell(row[col])).join('\t')),
                  ].join('\n');
                  copyToClipboard(tsv, 'all');
                }}
                className="flex items-center gap-1 rounded-md px-2 py-1 text-xs text-secondary hover:bg-hover"
                title="Copy all as TSV"
              >
                <Clipboard className="h-3.5 w-3.5" /> Copy All
              </button>
              <button
                onClick={exportCsv}
                className="flex items-center gap-1 rounded-md px-2 py-1 text-xs text-secondary hover:bg-hover"
                title="Export as CSV"
              >
                <Download className="h-3.5 w-3.5" /> CSV
              </button>
            </>
          )}
          {result.rows.length > pageSize && (
            <div className="flex items-center gap-2 text-xs text-secondary">
              <button
                onClick={() => setPage((p) => Math.max(0, p - 1))}
                disabled={currentPage === 0}
                className="rounded px-2 py-0.5 hover:bg-hover disabled:opacity-30"
              >
                Prev
              </button>
              <span className="text-muted">
                {currentPage + 1} / {totalPages}
              </span>
              <button
                onClick={() => setPage((p) => Math.min(totalPages - 1, p + 1))}
                disabled={currentPage >= totalPages - 1}
                className="rounded px-2 py-0.5 hover:bg-hover disabled:opacity-30"
              >
                Next
              </button>
            </div>
          )}
        </div>
      </div>

      {copiedCell && (
        <div className="flex items-center gap-1.5 border-b border-success bg-success-light px-3 py-1 text-[10px] text-success">
          <CheckCircle2 className="h-3 w-3" />
          {copiedCell === 'all' ? 'All rows copied to clipboard' : copiedCell === 'row' ? 'Row copied' : `Copied "${copiedCell}"`}
        </div>
      )}

      {result.columns.length === 0 ? (
        <div className="flex flex-1 flex-col items-center justify-center bg-base text-muted">
          <CheckCircle2 className="h-10 w-10 text-success" />
          <span className="mt-2 text-sm text-secondary">Query executed successfully — no rows returned.</span>
          {sql && (
            <code className="mt-2 max-w-md truncate rounded bg-muted px-2 py-1 text-xs text-muted">
              {sql}
            </code>
          )}
        </div>
      ) : (
        <div className="flex-1 overflow-auto">
          <table className="w-full border-collapse text-sm">
            <thead className="sticky top-0 z-10">
              <tr className="bg-muted">
                <th className="w-12 border-b border-base px-2 py-2 text-right text-xs font-medium text-muted">
                  #
                </th>
                {result.columns.map((col) => (
                  <th
                    key={col}
                    className="group border-b border-l border-base px-3 py-2 text-left text-xs font-semibold text-secondary"
                  >
                    <div className="flex items-center justify-between">
                      <span>{col}</span>
                      <button
                        onClick={() => copyColumn(col)}
                        className="ml-1 rounded p-0.5 text-muted opacity-0 hover:bg-hover hover:text-accent group-hover:opacity-100"
                        title="Copy column"
                      >
                        <Copy className="h-3 w-3" />
                      </button>
                    </div>
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {pageRows.map((row, i) => (
                <tr
                  key={start + i}
                  className="group border-b border-base hover:bg-accent-light/40"
                >
                  <td className="px-2 py-1.5 text-right text-xs text-faint">
                    {start + i + 1}
                  </td>
                  {result.columns.map((col) => {
                    const val = row[col];
                    const isNull = val === null;
                    const cellText = formatCell(val);
                    return (
                      <td
                        key={col}
                        className={`group/cell relative border-l border-base px-3 py-1.5 font-mono text-xs ${
                          isNull ? 'italic text-faint' : 'text-primary'
                        }`}
                      >
                        <div className="flex items-center justify-between">
                          <span className="truncate">{cellText}</span>
                          <div className="flex shrink-0 items-center gap-0.5 opacity-0 group-hover/cell:opacity-100">
                            <button
                              onClick={() => copyToClipboard(cellText, col)}
                              className="rounded p-0.5 text-muted hover:bg-hover hover:text-accent"
                              title="Copy cell"
                            >
                              <Copy className="h-3 w-3" />
                            </button>
                          </div>
                        </div>
                      </td>
                    );
                  })}
                  <td className="border-l border-base px-1 py-1.5">
                    <button
                      onClick={() => copyRow(row)}
                      className="rounded p-0.5 text-muted opacity-0 hover:bg-hover hover:text-accent group-hover:opacity-100"
                      title="Copy row"
                    >
                      <Copy className="h-3 w-3" />
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
