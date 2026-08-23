// Thin typed bridge to the Rust engine (Tauri command `run_sql`).
// Falls back to an error payload when running outside the desktop shell
// (e.g. plain `npm run dev` in a browser).

export interface ExecOk {
  ok: true;
  columns: string[];
  rows: unknown[][];
  rowsAffected: number;
}
export interface ExecErr {
  ok: false;
  error: string;
}
export type ExecResult = ExecOk | ExecErr;

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

async function invokeRunSql(sql: string): Promise<string> {
  const w = window as never as {
    __TAURI_INTERNALS__?: { invoke: (cmd: string, args: object) => Promise<string> };
  };
  if (!w.__TAURI_INTERNALS__) {
    return JSON.stringify({ ok: false, error: "desktop shell required" });
  }
  return w.__TAURI_INTERNALS__.invoke("run_sql", { sql });
}

export async function runSql(sql: string): Promise<ExecResult> {
  try {
    const raw = await invokeRunSql(sql);
    return JSON.parse(raw) as ExecResult;
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}