import { useEffect, useState } from 'react';
import { queryStore, type SavedQuery } from '@/lib/queryStore';
import { Bookmark, Search, Trash2, FileText, Clock, Plus, X } from 'lucide-react';

type Props = {
  onLoad: (sql: string) => void;
};

export function SavedQueries({ onLoad }: Props) {
  const [queries, setQueries] = useState<SavedQuery[]>([]);
  const [search, setSearch] = useState('');
  const [open, setOpen] = useState(false);

  const refresh = () => setQueries(queryStore.list());

  useEffect(() => {
    refresh();
  }, []);

  const filtered = queries.filter(
    (q) =>
      q.title.toLowerCase().includes(search.toLowerCase()) ||
      q.sql.toLowerCase().includes(search.toLowerCase()),
  );

  if (!open) {
    return (
      <button
        onClick={() => {
          setOpen(true);
          refresh();
        }}
        className="flex items-center gap-1.5 rounded-md border border-base px-2.5 py-1 text-xs text-secondary hover:bg-hover"
        title="Saved queries"
      >
        <Bookmark className="h-3.5 w-3.5 text-accent" />
        <span className="hidden sm:inline">Saved</span>
      </button>
    );
  }

  return (
    <div className="flex w-72 flex-col border-r border-base bg-surface">
      <div className="flex items-center justify-between border-b border-base px-3 py-2.5">
        <div className="flex items-center gap-2">
          <Bookmark className="h-4 w-4 text-accent" />
          <span className="text-sm font-semibold text-primary">Saved Queries</span>
          <span className="rounded-full bg-muted px-1.5 py-0.5 text-[10px] text-muted">
            {queries.length}
          </span>
        </div>
        <button
          onClick={() => setOpen(false)}
          className="rounded p-1 text-muted hover:bg-hover hover:text-secondary"
        >
          <X className="h-4 w-4" />
        </button>
      </div>

      <div className="border-b border-base px-3 py-2">
        <div className="flex items-center gap-2 rounded-md border border-base bg-elevated px-2 py-1">
          <Search className="h-3.5 w-3.5 text-muted" />
          <input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search queries…"
            className="flex-1 bg-transparent text-xs text-primary outline-none placeholder:text-faint"
          />
        </div>
      </div>

      <div className="flex-1 overflow-y-auto px-2 py-2">
        {filtered.length === 0 ? (
          <div className="px-2 py-4 text-center text-xs text-muted">
            {queries.length === 0
              ? 'No saved queries yet. Use Save in the editor.'
              : 'No matches found.'}
          </div>
        ) : (
          <ul className="space-y-1">
            {filtered.map((q) => (
              <li key={q.id} className="group">
                <div className="flex items-center gap-2 rounded-md px-2 py-1.5 hover:bg-hover">
                  <FileText className="h-3.5 w-3.5 shrink-0 text-muted" />
                  <button
                    onClick={() => {
                      onLoad(q.sql);
                      setOpen(false);
                    }}
                    className="flex-1 text-left"
                  >
                    <div className="truncate text-xs font-medium text-primary">{q.title}</div>
                    <div className="flex items-center gap-1 text-[10px] text-muted">
                      <Clock className="h-2.5 w-2.5" />
                      {new Date(q.updatedAt).toLocaleDateString()}
                    </div>
                  </button>
                  <button
                    onClick={() => {
                      queryStore.remove(q.id);
                      refresh();
                    }}
                    className="rounded p-1 text-muted opacity-0 hover:bg-error-light hover:text-error group-hover:opacity-100"
                    title="Delete"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
