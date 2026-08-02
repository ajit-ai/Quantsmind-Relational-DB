/**
 * Lightweight SQL formatter — keyword uppercasing, indentation, and newlines
 * for SELECT, INSERT, UPDATE, DELETE, CREATE TABLE, ALTER TABLE, etc.
 */

const KEYWORDS = [
  'SELECT', 'FROM', 'WHERE', 'INSERT', 'INTO', 'VALUES', 'UPDATE', 'SET',
  'DELETE', 'CREATE', 'TABLE', 'INDEX', 'SCHEMA', 'ALTER', 'ADD', 'COLUMN',
  'DROP', 'TRUNCATE', 'JOIN', 'INNER', 'LEFT', 'RIGHT', 'FULL', 'OUTER',
  'ON', 'GROUP', 'BY', 'ORDER', 'HAVING', 'LIMIT', 'OFFSET', 'AS',
  'AND', 'OR', 'NOT', 'NULL', 'IS', 'IN', 'EXISTS', 'BETWEEN', 'LIKE',
  'CASE', 'WHEN', 'THEN', 'ELSE', 'END', 'DISTINCT', 'ALL', 'UNION',
  'PRIMARY', 'KEY', 'FOREIGN', 'REFERENCES', 'CHECK', 'DEFAULT',
  'CONSTRAINT', 'UNIQUE', 'CASCADE', 'RESTRICT', 'RESTART', 'IDENTITY',
  'IF', 'EXISTS', 'RENAME', 'TO', 'TYPE', 'USING',
  'BEGIN', 'COMMIT', 'ROLLBACK', 'TRANSACTION', 'START',
  'WITH', 'RECURSIVE',
];

const TOP_CLAUSES = new Set([
  'SELECT', 'FROM', 'WHERE', 'INSERT', 'UPDATE', 'DELETE',
  'CREATE', 'ALTER', 'DROP', 'TRUNCATE', 'WITH', 'VALUES',
]);

const SUB_CLAUSES = new Set([
  'JOIN', 'INNER', 'LEFT', 'RIGHT', 'FULL', 'ON',
  'GROUP', 'ORDER', 'HAVING', 'LIMIT', 'OFFSET',
  'AND', 'OR', 'WHEN', 'THEN', 'ELSE', 'END',
  'VALUES', 'SET',
]);

function isKeyword(word: string): boolean {
  return KEYWORDS.includes(word.toUpperCase());
}

export function formatSql(input: string): string {
  const trimmed = input.trim();
  if (!trimmed) return '';

  // Tokenize: split into tokens but preserve strings and identifiers
  const tokens: string[] = [];
  let i = 0;
  while (i < trimmed.length) {
    const ch = trimmed[i];

    // Single-quoted string
    if (ch === "'") {
      let end = i + 1;
      while (end < trimmed.length && trimmed[end] !== "'") end++;
      tokens.push(trimmed.slice(i, Math.min(end + 1, trimmed.length)));
      i = end + 1;
      continue;
    }

    // Double-quoted identifier
    if (ch === '"') {
      let end = i + 1;
      while (end < trimmed.length && trimmed[end] !== '"') end++;
      tokens.push(trimmed.slice(i, Math.min(end + 1, trimmed.length)));
      i = end + 1;
      continue;
    }

    // Dollar-quoted
    if (ch === '$' && /[a-zA-Z_]/.test(trimmed[i + 1] ?? '')) {
      let tag = '$';
      let j = i + 1;
      while (j < trimmed.length && /[a-zA-Z0-9_]/.test(trimmed[j])) {
        tag += trimmed[j];
        j++;
      }
      if (trimmed[j] === '$') {
        tag += '$';
        const closeIdx = trimmed.indexOf(tag, j + 1);
        if (closeIdx >= 0) {
          tokens.push(trimmed.slice(i, closeIdx + tag.length));
          i = closeIdx + tag.length;
          continue;
        }
      }
    }

    // Word
    if (/[a-zA-Z_]/.test(ch)) {
      let end = i;
      while (end < trimmed.length && /[a-zA-Z0-9_]/.test(trimmed[end])) end++;
      tokens.push(trimmed.slice(i, end));
      i = end;
      continue;
    }

    // Number
    if (/[0-9]/.test(ch)) {
      let end = i;
      while (end < trimmed.length && /[0-9.]/.test(trimmed[end])) end++;
      tokens.push(trimmed.slice(i, end));
      i = end;
      continue;
    }

    // Whitespace
    if (/\s/.test(ch)) {
      let end = i;
      while (end < trimmed.length && /\s/.test(trimmed[end])) end++;
      i = end;
      continue;
    }

    // Punctuation
    tokens.push(ch);
    i++;
  }

  // Build formatted output
  let output = '';
  let indent = 0;
  let afterSelect = false;
  let afterValues = false;
  let inCreate = false;
  let inInsert = false;
  let parenDepth = 0;
  let prevToken = '';

  const newline = () => {
    output = output.trimEnd();
    output += '\n' + '  '.repeat(indent);
  };

  for (let t = 0; t < tokens.length; t++) {
    const raw = tokens[t];
    const upper = raw.toUpperCase();
    const isKw = isKeyword(raw);

    // Handle commas
    if (raw === ',') {
      output = output.trimEnd();
      output += ',';
      if (afterSelect || afterValues || inCreate) {
        newline();
      } else {
        output += ' ';
      }
      prevToken = raw;
      continue;
    }

    // Handle parentheses
    if (raw === '(') {
      parenDepth++;
      output += '(';
      if (inCreate) {
        indent++;
        newline();
      }
      prevToken = raw;
      continue;
    }
    if (raw === ')') {
      parenDepth = Math.max(0, parenDepth - 1);
      if (inCreate && parenDepth === 0) {
        indent = Math.max(0, indent - 1);
        newline();
      }
      output = output.trimEnd();
      output += ') ';
      prevToken = raw;
      continue;
    }

    // Top-level clauses get a newline
    if (isKw && TOP_CLAUSES.has(upper) && parenDepth === 0) {
      if (output.trim()) newline();
      indent = 0;
      afterSelect = upper === 'SELECT';
      afterValues = false;
      inCreate = upper === 'CREATE';
      inInsert = upper === 'INSERT';
      output += upper;
      prevToken = raw;
      continue;
    }

    // Sub-clauses get a newline at same indent
    if (isKw && SUB_CLAUSES.has(upper) && parenDepth === 0) {
      if (upper === 'AND' || upper === 'OR') {
        output = output.trimEnd();
        output += ' ' + upper + ' ';
      } else {
        newline();
        output += upper;
      }
      if (upper === 'VALUES') afterValues = true;
      if (upper === 'SET') afterValues = false;
      prevToken = raw;
      continue;
    }

    // Uppercase keywords, preserve everything else
    const displayToken = isKw ? upper : raw;

    // Add spacing
    if (output && !output.endsWith(' ') && !output.endsWith('\n') && !output.endsWith('(') && raw !== ')' && raw !== ';' && prevToken !== '(' && prevToken !== '.') {
      output += ' ';
    }
    if (prevToken === '.') {
      output = output.trimEnd();
    }

    output += displayToken;
    prevToken = raw;
  }

  return output.trimEnd() + ';';
}
