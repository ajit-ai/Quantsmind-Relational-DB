import { useState } from 'react';
import { engine } from '@/lib/engine';
import {
  ShieldCheck, Play, RotateCcw, CheckCircle2, XCircle, Loader2,
  Atom, Lock, Repeat, Shield,
} from 'lucide-react';

type LogEntry = { type: 'info' | 'success' | 'error'; message: string };

export function AcidDemo() {
  const [log, setLog] = useState<LogEntry[]>([]);
  const [busy, setBusy] = useState(false);

  const addLog = (type: LogEntry['type'], message: string) => {
    setLog((prev) => [...prev, { type, message }]);
  };

  const clearLog = () => setLog([]);

  const runAtomicity = async () => {
    setBusy(true);
    clearLog();
    try {
      await engine.exec(`DROP TABLE IF EXISTS accounts;`);
      await engine.exec(`
        CREATE TABLE accounts (
          id SERIAL PRIMARY KEY,
          holder TEXT NOT NULL,
          balance NUMERIC(10,2) NOT NULL DEFAULT 0 CHECK (balance >= 0)
        );
      `);
      await engine.exec(`INSERT INTO accounts (holder, balance) VALUES ('Alice', 100.00), ('Bob', 50.00);`);
      addLog('info', 'Created accounts: Alice $100, Bob $50');

      await engine.beginTransaction();
      addLog('info', 'BEGIN transaction');

      try {
        await engine.exec(`UPDATE accounts SET balance = balance - 30 WHERE holder = 'Alice';`);
        addLog('info', 'Debited Alice $30');
        await engine.exec(`UPDATE accounts SET balance = balance + 30 WHERE holder = 'Bob';`);
        addLog('info', 'Credited Bob $30');
        await engine.exec(`UPDATE accounts SET balance = -999 WHERE holder = 'Alice';`);
        addLog('success', 'Transfer committed.');
        await engine.commitTransaction();
      } catch (e) {
        addLog('error', `Constraint violated: ${e instanceof Error ? e.message : String(e)}`);
        await engine.rollbackTransaction();
        addLog('info', 'ROLLBACK — all changes undone (atomicity)');
      }

      const result = await engine.query(`SELECT holder, balance FROM accounts ORDER BY holder;`);
      const balances = result.rows.map((r) => `${r.holder}: $${r.balance}`).join(', ');
      addLog('success', `After: ${balances}`);
      addLog('info', 'Balances are unchanged — the transfer was all-or-nothing.');
    } catch (e) {
      addLog('error', e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const runConsistency = async () => {
    setBusy(true);
    clearLog();
    try {
      await engine.exec(`DROP TABLE IF EXISTS products_demo;`);
      await engine.exec(`
        CREATE TABLE products_demo (
          id SERIAL PRIMARY KEY,
          name TEXT NOT NULL,
          price NUMERIC(10,2) NOT NULL CHECK (price > 0)
        );
      `);
      addLog('info', 'Created products_demo with CHECK (price > 0)');

      try {
        await engine.query(`INSERT INTO products_demo (name, price) VALUES ('Valid', 19.99);`);
        addLog('success', 'Inserted valid product (price 19.99)');
      } catch (e) {
        addLog('error', e instanceof Error ? e.message : String(e));
      }

      try {
        await engine.query(`INSERT INTO products_demo (name, price) VALUES ('Invalid', -5.00);`);
        addLog('success', 'Inserted invalid product');
      } catch (e) {
        addLog('error', `Rejected: ${e instanceof Error ? e.message : String(e)}`);
        addLog('info', 'CHECK constraint kept the database consistent.');
      }
    } catch (e) {
      addLog('error', e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const runIsolation = async () => {
    setBusy(true);
    clearLog();
    try {
      await engine.exec(`DROP TABLE IF EXISTS isolation_test;`);
      await engine.exec(`
        CREATE TABLE isolation_test (
          id SERIAL PRIMARY KEY,
          value INTEGER NOT NULL
        );
      `);
      await engine.exec(`INSERT INTO isolation_test (value) VALUES (1);`);
      addLog('info', 'Created isolation_test with value = 1');

      await engine.beginTransaction();
      addLog('info', 'BEGIN transaction A');

      await engine.exec(`UPDATE isolation_test SET value = 999 WHERE id = 1;`);
      addLog('info', 'Transaction A set value = 999 (not committed)');

      const beforeCommit = await engine.query(`SELECT value FROM isolation_test WHERE id = 1;`);
      addLog('info', `Within transaction A, reads: ${beforeCommit.rows[0]?.value}`);

      await engine.commitTransaction();
      addLog('success', 'COMMITTED — changes now visible to all');
      addLog('info', 'Each transaction sees a consistent snapshot until commit.');
    } catch (e) {
      addLog('error', e instanceof Error ? e.message : String(e));
      try {
        await engine.rollbackTransaction();
      } catch {
        /* ignore */
      }
    } finally {
      setBusy(false);
    }
  };

  const runDurability = async () => {
    setBusy(true);
    clearLog();
    try {
      await engine.exec(`DROP TABLE IF EXISTS durability_test;`);
      await engine.exec(`
        CREATE TABLE durability_test (
          id SERIAL PRIMARY KEY,
          note TEXT NOT NULL,
          saved_at TIMESTAMPTZ DEFAULT now()
        );
      `);
      await engine.query(`INSERT INTO durability_test (note) VALUES ('This data is durable');`);
      addLog('info', 'Inserted a row into durability_test');

      const r = await engine.query(`SELECT count(*)::int AS c FROM durability_test;`);
      addLog('success', `Row count: ${r.rows[0]?.c}`);

      addLog('info', 'Data is stored in IndexedDB — it survives page reloads and browser restarts.');
      addLog('info', 'Reload the page and run: SELECT * FROM durability_test;');
    } catch (e) {
      addLog('error', e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const demos = [
    { icon: Atom, title: 'Atomicity', desc: 'All-or-nothing transfers with rollback', run: runAtomicity },
    { icon: Shield, title: 'Consistency', desc: 'CHECK constraints reject invalid data', run: runConsistency },
    { icon: Lock, title: 'Isolation', desc: 'Transactions see a consistent snapshot', run: runIsolation },
    { icon: Repeat, title: 'Durability', desc: 'Data survives reloads via IndexedDB', run: runDurability },
  ];

  return (
    <div className="flex h-full flex-col bg-surface">
      <div className="border-b border-base bg-muted px-4 py-3">
        <div className="flex items-center gap-2">
          <ShieldCheck className="h-4 w-4 text-accent" />
          <span className="text-sm font-semibold text-primary">ACID Properties</span>
        </div>
        <p className="mt-1 text-xs text-muted">
          Live demos of the four guarantees of a relational database.
        </p>
      </div>

      <div className="grid grid-cols-2 gap-3 p-4 lg:grid-cols-4">
        {demos.map((d) => (
          <button
            key={d.title}
            onClick={d.run}
            disabled={busy}
            className="group flex flex-col items-start gap-1.5 rounded-lg border border-base bg-elevated p-3 text-left transition-all hover:shadow-md disabled:opacity-50"
          >
            <div className="flex items-center gap-2">
              <d.icon className="h-4 w-4 text-accent" />
              <span className="text-sm font-semibold text-primary">{d.title}</span>
            </div>
            <span className="text-xs text-secondary">{d.desc}</span>
            <span className="mt-1 flex items-center gap-1 text-xs text-accent">
              {busy ? (
                <Loader2 className="h-3 w-3 animate-spin" />
              ) : (
                <Play className="h-3 w-3" />
              )}
              Run demo
            </span>
          </button>
        ))}
      </div>

      <div className="flex-1 overflow-y-auto px-4 pb-4">
        <div className="mb-2 flex items-center justify-between">
          <span className="text-xs font-semibold uppercase tracking-wide text-muted">
            Output
          </span>
          {log.length > 0 && (
            <button
              onClick={clearLog}
              className="flex items-center gap-1 text-xs text-muted hover:text-secondary"
            >
              <RotateCcw className="h-3 w-3" /> Clear
            </button>
          )}
        </div>
        {log.length === 0 ? (
          <div className="py-8 text-center text-xs text-muted">
            Run a demo to see the output.
          </div>
        ) : (
          <ul className="space-y-1 font-mono text-xs">
            {log.map((entry, i) => (
              <li key={i} className="flex items-start gap-2">
                {entry.type === 'success' ? (
                  <CheckCircle2 className="mt-0.5 h-3.5 w-3.5 shrink-0 text-success" />
                ) : entry.type === 'error' ? (
                  <XCircle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-error" />
                ) : (
                  <span className="mt-0.5 h-3.5 w-3.5 shrink-0 text-center text-faint">›</span>
                )}
                <span
                  className={
                    entry.type === 'error'
                      ? 'text-error'
                      : entry.type === 'success'
                        ? 'text-success'
                        : 'text-secondary'
                  }
                >
                  {entry.message}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
