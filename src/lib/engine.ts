import { PGlite } from '@electric-sql/pglite';

export type QueryResult = {
  columns: string[];
  rows: Record<string, unknown>[];
  rowCount: number;
  command?: string;
  durationMs: number;
};

export type SchemaInfo = {
  name: string;
  tableCount: number;
};

export type SchemaTable = {
  name: string;
  schema: string;
  type: string;
};

export type ColumnInfo = {
  name: string;
  dataType: string;
  isNullable: boolean;
  isPrimaryKey: boolean;
  defaultValue: string | null;
  position: number;
};

export type ForeignKeyInfo = {
  name: string;
  columnName: string;
  referencesTable: string;
  referencesColumn: string;
  onDelete: string;
  onUpdate: string;
};

export type TableSchema = {
  table: string;
  schema: string;
  columns: ColumnInfo[];
  foreignKeys: ForeignKeyInfo[];
  rowCount: number;
};

function inferCommand(sql: string): string {
  const trimmed = sql.trim().toUpperCase();
  if (trimmed.startsWith('SELECT')) return 'SELECT';
  if (trimmed.startsWith('INSERT')) return 'INSERT';
  if (trimmed.startsWith('UPDATE')) return 'UPDATE';
  if (trimmed.startsWith('DELETE')) return 'DELETE';
  if (trimmed.startsWith('CREATE')) return 'CREATE';
  if (trimmed.startsWith('ALTER')) return 'ALTER';
  if (trimmed.startsWith('DROP')) return 'DROP';
  if (trimmed.startsWith('TRUNCATE')) return 'TRUNCATE';
  if (trimmed.startsWith('BEGIN')) return 'BEGIN';
  if (trimmed.startsWith('COMMIT')) return 'COMMIT';
  if (trimmed.startsWith('ROLLBACK')) return 'ROLLBACK';
  return 'OK';
}

function qualify(schema: string, table: string): string {
  return `"${schema}"."${table}"`;
}

class DatabaseEngine {
  private db: PGlite | null = null;
  private initPromise: Promise<void> | null = null;
  private listeners: Set<() => void> = new Set();
  private inTransaction = false;

  async init(): Promise<void> {
    if (this.db) return;
    if (this.initPromise) return this.initPromise;

    this.initPromise = (async () => {
      this.db = new PGlite('idb://quantsmind');
      await this.db.waitReady;
      await this.seedIfEmpty();
    })();

    return this.initPromise;
  }

  private async seedIfEmpty(): Promise<void> {
    const result = await this.rawQuery(
      `SELECT count(*)::int AS cnt FROM information_schema.tables WHERE table_schema = 'public'`,
    );
    const count = (result.rows[0]?.cnt as number) ?? 0;
    if (count > 0) return;

    await this.exec(`
      CREATE TABLE customers (
        id SERIAL PRIMARY KEY,
        name TEXT NOT NULL,
        email TEXT UNIQUE,
        city TEXT,
        created_at TIMESTAMPTZ DEFAULT now()
      );

      CREATE TABLE products (
        id SERIAL PRIMARY KEY,
        name TEXT NOT NULL,
        price NUMERIC(10,2) NOT NULL CHECK (price > 0),
        stock INTEGER NOT NULL DEFAULT 0 CHECK (stock >= 0)
      );

      CREATE TABLE orders (
        id SERIAL PRIMARY KEY,
        customer_id INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
        placed_at TIMESTAMPTZ DEFAULT now(),
        status TEXT NOT NULL DEFAULT 'pending'
      );

      CREATE TABLE order_items (
        id SERIAL PRIMARY KEY,
        order_id INTEGER NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
        product_id INTEGER NOT NULL REFERENCES products(id) ON DELETE RESTRICT,
        quantity INTEGER NOT NULL CHECK (quantity > 0),
        unit_price NUMERIC(10,2) NOT NULL
      );

      INSERT INTO customers (name, email, city) VALUES
        ('Acme Corp', 'billing@acme.example', 'New York'),
        ('Globex Inc', 'ap@globex.example', 'London'),
        ('Initech', 'contact@initech.example', 'Austin');

      INSERT INTO products (name, price, stock) VALUES
        ('Widget Pro', 29.99, 120),
        ('Gadget Lite', 14.50, 300),
        ('Super Gizmo', 99.00, 45),
        ('Doohickey', 7.25, 0);

      INSERT INTO orders (customer_id, status) VALUES
        (1, 'fulfilled'), (2, 'pending'), (1, 'pending');

      INSERT INTO order_items (order_id, product_id, quantity, unit_price) VALUES
        (1, 1, 5, 29.99),
        (1, 3, 2, 99.00),
        (2, 2, 10, 14.50),
        (3, 4, 50, 7.25);
    `);
  }

  async exec(sql: string): Promise<void> {
    await this.init();
    const upper = sql.trim().toUpperCase();
    if (upper === 'BEGIN') this.inTransaction = true;
    if (upper === 'COMMIT' || upper === 'ROLLBACK') this.inTransaction = false;
    await this.db!.exec(sql);
    this.notify();
  }

  async query(sql: string, params?: unknown[]): Promise<QueryResult> {
    await this.init();
    const start = performance.now();
    const result = await this.db!.query(sql, params);
    const durationMs = Math.round(performance.now() - start);

    const rows = (result.rows ?? []) as Record<string, unknown>[];
    const columns = (result.fields ?? []).map((f) => f.name);

    return {
      columns,
      rows,
      rowCount: result.affectedRows ?? rows.length,
      command: inferCommand(sql),
      durationMs,
    };
  }

  private async rawQuery(sql: string): Promise<QueryResult> {
    const result = await this.db!.query(sql);
    return {
      columns: (result.fields ?? []).map((f) => f.name),
      rows: (result.rows ?? []) as Record<string, unknown>[],
      rowCount: (result.rows ?? []).length,
      durationMs: 0,
    };
  }

  // ── Schema management ──

  async listSchemas(): Promise<SchemaInfo[]> {
    await this.init();
    const result = await this.query(
      `SELECT n.nspname AS name,
              count(t.table_name)::int AS table_count
       FROM pg_namespace n
       LEFT JOIN information_schema.tables t
         ON t.table_schema = n.nspname
         AND t.table_type = 'BASE TABLE'
       WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
         AND n.nspname NOT LIKE 'pg_temp_%'
       GROUP BY n.nspname
       ORDER BY n.nspname`,
    );
    return result.rows as unknown as SchemaInfo[];
  }

  async createSchema(name: string): Promise<void> {
    await this.exec(`CREATE SCHEMA IF NOT EXISTS "${name}";`);
  }

  async dropSchema(name: string, cascade = false): Promise<void> {
    const clause = cascade ? 'CASCADE' : 'RESTRICT';
    await this.exec(`DROP SCHEMA IF EXISTS "${name}" ${clause};`);
  }

  // ── Table listing ──

  async listTables(schema = 'public'): Promise<SchemaTable[]> {
    await this.init();
    const result = await this.query(
      `SELECT table_name AS name, table_schema AS schema, table_type AS type
       FROM information_schema.tables
       WHERE table_schema = $1
       ORDER BY table_name`,
      [schema],
    );
    return result.rows as unknown as SchemaTable[];
  }

  async listAllTables(): Promise<SchemaTable[]> {
    await this.init();
    const result = await this.query(
      `SELECT table_name AS name, table_schema AS schema, table_type AS type
       FROM information_schema.tables
       WHERE table_schema NOT IN ('pg_catalog', 'information_schema')
       ORDER BY table_schema, table_name`,
    );
    return result.rows as unknown as SchemaTable[];
  }

  // ── Table schema ──

  async getTableSchema(tableName: string, schema = 'public'): Promise<TableSchema> {
    await this.init();

    const columnsResult = await this.query(
      `SELECT
         c.column_name AS name,
         c.data_type AS dataType,
         c.is_nullable = 'YES' AS isNullable,
         COALESCE(pk.is_pk, false) AS isPrimaryKey,
         c.column_default AS defaultValue,
         c.ordinal_position AS position
       FROM information_schema.columns c
       LEFT JOIN (
         SELECT kcu.column_name, kcu.table_name, kcu.table_schema
         FROM information_schema.key_column_usage kcu
         JOIN information_schema.table_constraints tc
           ON tc.constraint_name = kcu.constraint_name
           AND tc.table_schema = kcu.table_schema
         WHERE tc.constraint_type = 'PRIMARY KEY'
       ) pk ON pk.column_name = c.column_name
         AND pk.table_name = c.table_name
         AND pk.table_schema = c.table_schema
       WHERE c.table_name = $1 AND c.table_schema = $2
       ORDER BY c.ordinal_position`,
      [tableName, schema],
    );

    const fkResult = await this.query(
      `SELECT
         con.conname AS name,
         a.attname AS columnName,
         ref.relname AS referencesTable,
         af.attname AS referencesColumn,
         conf.deltype AS onDelete,
         conf.updtype AS onUpdate
       FROM pg_constraint con
       JOIN pg_class rel ON rel.oid = con.conrelid
       JOIN pg_namespace ns ON ns.oid = rel.relnamespace
       JOIN pg_class ref ON ref.oid = con.confrelid
       JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = con.conkey[1]
       JOIN pg_attribute af ON af.attrelid = con.confrelid AND af.attnum = con.confkey[1]
       WHERE con.contype = 'f' AND rel.relname = $1 AND ns.nspname = $2`,
      [tableName, schema],
    );

    const countResult = await this.query(
      `SELECT count(*)::int AS cnt FROM ${qualify(schema, tableName)}`,
    );

    return {
      table: tableName,
      schema,
      columns: columnsResult.rows as unknown as ColumnInfo[],
      foreignKeys: fkResult.rows as unknown as ForeignKeyInfo[],
      rowCount: (countResult.rows[0]?.cnt as number) ?? 0,
    };
  }

  async getTableData(tableName: string, schema = 'public', limit = 100, offset = 0): Promise<QueryResult> {
    const pkResult = await this.query(
      `SELECT a.attname AS col
       FROM pg_index i
       JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey)
       JOIN pg_class c ON c.oid = i.indrelid
       JOIN pg_namespace n ON n.oid = c.relnamespace
       WHERE i.indisprimary AND c.relname = $1 AND n.nspname = $2
       ORDER BY array_position(i.indkey, a.attnum)
       LIMIT 1`,
      [tableName, schema],
    );
    const pkCol = (pkResult.rows[0]?.col as string) ?? null;
    const orderClause = pkCol ? `ORDER BY "${pkCol}"` : '';
    return this.query(
      `SELECT * FROM ${qualify(schema, tableName)} ${orderClause} LIMIT $1 OFFSET $2`,
      [limit, offset],
    );
  }

  async countRows(tableName: string, schema = 'public'): Promise<number> {
    const r = await this.query(`SELECT count(*)::int AS cnt FROM ${qualify(schema, tableName)}`);
    return (r.rows[0]?.cnt as number) ?? 0;
  }

  async dropTable(tableName: string, schema = 'public'): Promise<void> {
    await this.exec(`DROP TABLE IF EXISTS ${qualify(schema, tableName)} CASCADE;`);
  }

  async renameTable(oldName: string, newName: string, schema = 'public'): Promise<void> {
    await this.exec(`ALTER TABLE ${qualify(schema, oldName)} RENAME TO "${newName}";`);
  }

  async duplicateTable(
    sourceName: string,
    targetName: string,
    schema = 'public',
    includeData = true,
  ): Promise<void> {
    const q = qualify(schema, sourceName);
    const target = qualify(schema, targetName);
    await this.exec(`CREATE TABLE ${target} (LIKE ${q} INCLUDING ALL);`);
    if (includeData) {
      await this.exec(`INSERT INTO ${target} SELECT * FROM ${q};`);
    }
  }

  async truncateTable(tableName: string, schema = 'public'): Promise<void> {
    await this.exec(`TRUNCATE TABLE ${qualify(schema, tableName)} RESTART IDENTITY CASCADE;`);
  }

  async exportTableData(tableName: string, schema = 'public'): Promise<string> {
    const result = await this.query(`SELECT * FROM ${qualify(schema, tableName)}`);
    const escape = (s: string) => {
      if (/[",\n]/.test(s)) return `"${s.replace(/"/g, '""')}"`;
      return s;
    };
    const formatVal = (val: unknown): string => {
      if (val === null) return '';
      if (val === undefined) return '';
      if (val instanceof Date) return val.toISOString();
      if (typeof val === 'object') return JSON.stringify(val);
      return String(val);
    };
    const header = result.columns.map(escape).join(',');
    const body = result.rows
      .map((row) => result.columns.map((col) => escape(formatVal(row[col]))).join(','))
      .join('\n');
    return `${header}\n${body}`;
  }

  async getRowCount(tableName: string, schema = 'public'): Promise<number> {
    return this.countRows(tableName, schema);
  }

  // ── DML helpers ──

  async insertRow(
    tableName: string,
    schema: string,
    data: Record<string, unknown>,
  ): Promise<QueryResult> {
    const cols = Object.keys(data).filter((k) => data[k] !== undefined && data[k] !== '');
    if (cols.length === 0) throw new Error('No columns provided for insert');
    const placeholders = cols.map((_, i) => `$${i + 1}`).join(', ');
    const colNames = cols.map((c) => `"${c}"`).join(', ');
    const values = cols.map((c) => data[c]);
    return this.query(
      `INSERT INTO ${qualify(schema, tableName)} (${colNames}) VALUES (${placeholders})`,
      values,
    );
  }

  async updateRow(
    tableName: string,
    schema: string,
    pkColumns: string[],
    pkValues: unknown[],
    updates: Record<string, unknown>,
  ): Promise<QueryResult> {
    const setCols = Object.keys(updates);
    if (setCols.length === 0) throw new Error('No columns to update');
    const setClauses = setCols.map((c, i) => `"${c}" = $${i + 1}`).join(', ');
    const setValues = setCols.map((c) => updates[c]);
    const whereClauses = pkColumns.map((c, i) => `"${c}" = $${setCols.length + i + 1}`).join(' AND ');
    return this.query(
      `UPDATE ${qualify(schema, tableName)} SET ${setClauses} WHERE ${whereClauses}`,
      [...setValues, ...pkValues],
    );
  }

  async deleteRow(
    tableName: string,
    schema: string,
    pkColumns: string[],
    pkValues: unknown[],
  ): Promise<QueryResult> {
    const whereClauses = pkColumns.map((c, i) => `"${c}" = $${i + 1}`).join(' AND ');
    return this.query(
      `DELETE FROM ${qualify(schema, tableName)} WHERE ${whereClauses}`,
      pkValues,
    );
  }

  // ── Transactions ──

  async beginTransaction(): Promise<void> {
    await this.exec('BEGIN');
  }

  async commitTransaction(): Promise<void> {
    await this.exec('COMMIT');
  }

  async rollbackTransaction(): Promise<void> {
    await this.exec('ROLLBACK');
  }

  isInTransaction(): boolean {
    return this.inTransaction;
  }

  // ── Change notifications ──

  onChange(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private notify(): void {
    this.listeners.forEach((fn) => fn());
  }
}

export const engine = new DatabaseEngine();
