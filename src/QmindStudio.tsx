import { useState } from 'react';
import { runSql } from './lib/desktop';

interface Row { [k: string]: unknown }

export default function QmindStudio() {
  const [sql, setSql] = useState(
    "CREATE TABLE demo (id INTEGER NOT NULL, name TEXT);\nINSERT INTO demo VALUES (1,'Ada'),(2,'Grace');\nSELECT * FROM demo;"
  );
  const [out, setOut] = useState<string>('Engine ready — press Run.');
  const [grid, setGrid] = useState<{ cols: string[]; rows: Row[] } | null>(null);
  const [busy, setBusy] = useState(false);

  async function onRun() {
    setBusy(true);
    setGrid(null);
    let last = '';
    for (const stmt of sql.split(';').map((s) => s.trim()).filter(Boolean)) {
      const res = await runSql(stmt + ';');
      if (!res.ok) {
        last = 'ERROR: ' + res.error;
        setGrid(null);
        break;
      }
      if (res.columns.length > 0) {
        setGrid({ cols: res.columns, rows: res.rows as unknown as Row[] });
        last = `${res.rows.length} row(s)`;
      } else {
        last = `OK — ${res.rowsAffected} row(s) affected`;
      }
    }
    setOut(last);
    setBusy(false);
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100vh', background: '#0b1020', color: '#e6e9f2', fontFamily: 'Segoe UI, sans-serif' }}>
      <header style={{ padding: '10px 16px', borderBottom: '1px solid #1d2742', display: 'flex', gap: 12, alignItems: 'center' }}>
        <div style={{ width: 28, height: 28, borderRadius: 8, background: '#1e6feb', display: 'flex', alignItems: 'center', justifyContent: 'center', fontWeight: 700 }}>Q</div>
        <strong>QuantsMind Studio</strong>
        <span style={{ color: '#8b93a7', fontSize: 12 }}>Rust engine · MVCC · WAL-persistent</span>
        <button
          onClick={onRun}
          disabled={busy}
          style={{ marginLeft: 'auto', background: '#1e6feb', border: 0, borderRadius: 8, padding: '8px 22px', color: 'white', cursor: 'pointer', fontWeight: 600 }}
        >
          {busy ? 'Running…' : 'Run (F5)'}
        </button>
      </header>

      <textarea
        value={sql}
        onChange={(e) => setSql(e.target.value)}
        onKeyDown={(e) => { if (e.key === 'F5') { e.preventDefault(); void onRun(); } }}
        spellCheck={false}
        style={{ height: 180, resize: 'vertical', background: '#0f152b', color: '#d7dcEA', border: '1px solid #1d2742', margin: 12, borderRadius: 10, padding: 12, fontFamily: 'Consolas, monospace', fontSize: 13 }}
      />

      <div style={{ color: '#8b93a7', padding: '0 16px 4px', fontSize: 12 }}>{out}</div>

      <div style={{ flex: 1, overflow: 'auto', margin: 12, border: '1px solid #1d2742', borderRadius: 10 }}>
        {grid ? (
          <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
            <thead>
              <tr>
                {grid.cols.map((c) => (
                  <th key={c} style={{ textAlign: 'left', position: 'sticky', top: 0, background: '#141c36', padding: '8px 12px', borderBottom: '1px solid #243055' }}>{c}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {grid.rows.map((r, i) => (
                <tr key={i}>
                  {grid.cols.map((c) => (
                    <td key={c} style={{ padding: '6px 12px', borderTop: '1px solid #182038' }}>
                      {r[c] === null || r[c] === undefined ? <em style={{ color: '#58607a' }}>NULL</em> : String(r[c])}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        ) : (
          <div style={{ padding: 16, color: '#58607a' }}>Results appear here.</div>
        )}
      </div>

      <footer style={{ padding: '6px 16px', borderTop: '1px solid #1d2742', color: '#58607a', fontSize: 11 }}>
        Data persists in qmind-data/desktop-wal.log — restart-safe (MVCC committed txns only)
      </footer>
    </div>
  );
}