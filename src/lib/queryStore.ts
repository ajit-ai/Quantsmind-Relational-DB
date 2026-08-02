export type SavedQuery = {
  id: string;
  title: string;
  sql: string;
  createdAt: number;
  updatedAt: number;
};

const STORAGE_KEY = 'quantsmind-saved-queries';

function loadAll(): SavedQuery[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) return JSON.parse(raw) as SavedQuery[];
  } catch {
    /* ignore */
  }
  return [];
}

function saveAll(queries: SavedQuery[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(queries));
  } catch {
    /* ignore */
  }
}

function genId(): string {
  return `q_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
}

export const queryStore = {
  list(): SavedQuery[] {
    return loadAll().sort((a, b) => b.updatedAt - a.updatedAt);
  },

  save(title: string, sql: string, id?: string): SavedQuery {
    const all = loadAll();
    if (id) {
      const existing = all.find((q) => q.id === id);
      if (existing) {
        existing.title = title;
        existing.sql = sql;
        existing.updatedAt = Date.now();
        saveAll(all);
        return existing;
      }
    }
    const query: SavedQuery = {
      id: genId(),
      title,
      sql,
      createdAt: Date.now(),
      updatedAt: Date.now(),
    };
    all.push(query);
    saveAll(all);
    return query;
  },

  remove(id: string): void {
    const all = loadAll().filter((q) => q.id !== id);
    saveAll(all);
  },

  get(id: string): SavedQuery | null {
    return loadAll().find((q) => q.id === id) ?? null;
  },
};
