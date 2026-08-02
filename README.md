# Quantsmind — Relational Database Studio

A web-based relational database management studio with a real Postgres engine running entirely in the browser. Built with React, TypeScript, Vite, and PGlite (WASM Postgres). Works on Windows and Linux — no server install required.

## Features

- **SQL Editor** — Write and run SQL with multi-statement support, run history, and Ctrl/Cmd+Enter shortcut
- **Schema Browser** — Explore tables, columns, primary keys, foreign keys, and row counts
- **Visual Table Designer** — Create and inspect tables without writing SQL
- **Data Browser** — View, insert, edit, and delete rows in any table
- **ACID Demos** — Live demonstrations of Atomicity, Consistency, Isolation, and Durability
- **Persistent Storage** — Data survives page reloads via IndexedDB

## Prerequisites

- [Node.js](https://nodejs.org/) version 18 or higher

## How to Run

### 1. Install dependencies

```bash
npm install
```

### 2. Start the development server

```bash
npm run dev
```

Then open your browser to the URL shown in the terminal (typically `http://localhost:5173`).

### 3. Build for production

```bash
npm run build
```

This creates a `dist/` folder with the optimized app. Preview it with:

```bash
npm run preview
```

## Platform Notes

- **Windows**: Use Command Prompt, PowerShell, or Git Bash to run the commands above
- **Linux**: Use any terminal
- The app runs identically on both platforms since it is a standard web application

## Tech Stack

| Component | Technology |
|-----------|-----------|
| Frontend | React 18 + TypeScript |
| Build tool | Vite 5 |
| Styling | Tailwind CSS |
| Icons | Lucide React |
| Database engine | PGlite (WASM PostgreSQL) |
| Storage | IndexedDB (browser persistence) |

## First Load

The initial page load downloads the WASM Postgres engine (~10 MB). After the first load it is cached by the browser and subsequent loads are fast.

## Sample Data

The app comes pre-seeded with an e-commerce schema:

- `customers` — customer records
- `products` — product catalog with price and stock
- `orders` — orders placed by customers
- `order_items` — line items linking orders to products

All tables include foreign keys and constraints so you can explore relational features immediately.
