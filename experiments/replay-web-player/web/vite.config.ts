import react from '@vitejs/plugin-react';
import { spawn } from 'node:child_process';
import { readdir, stat } from 'node:fs/promises';
import { extname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { Plugin } from 'vite';
import { defineConfig } from 'vitest/config';

const webRoot = fileURLToPath(new URL('.', import.meta.url));
const experimentRoot = resolve(webRoot, '..');
const gameRoot = process.env.WOWS_GAME_DIR ?? 'C:\\Games\\World_of_Warships';
const replayRoot = join(gameRoot, 'replays');
const exporterManifest = join(experimentRoot, 'exporter', 'Cargo.toml');
const generatedOutput = join(webRoot, 'public', 'generated');

interface ReplaySummary {
  id: string;
  filename: string;
  shipName: string;
  shipClass: 'destroyer' | 'cruiser' | 'battleship' | 'carrier' | 'submarine';
  mapName: string;
  playedAt: string;
  modifiedAt: string;
  size: number;
}

function titleCase(value: string): string {
  return value
    .replace(/[-_]+/g, ' ')
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function replaySummary(path: string, size: number, modifiedAt: Date): ReplaySummary {
  const segments = path.split(/[\\/]/);
  const filename = segments[segments.length - 1] ?? path;
  const match = filename.match(/^(\d{8})_(\d{6})_([A-Z0-9]+)-(.+?)_(\d+)_([^.]*)\.wowsreplay$/i);
  const typeCode = match?.[3]?.slice(2, 4).toUpperCase();
  const shipClass = typeCode === 'SD' ? 'destroyer'
    : typeCode === 'SC' ? 'cruiser'
      : typeCode === 'SA' ? 'carrier'
        : typeCode === 'SS' ? 'submarine'
          : 'battleship';
  const date = match?.[1];
  const time = match?.[2];
  const playedAt = date && time
    ? new Date(
      Number(date.slice(0, 4)), Number(date.slice(4, 6)) - 1, Number(date.slice(6, 8)),
      Number(time.slice(0, 2)), Number(time.slice(2, 4)), Number(time.slice(4, 6)),
    ).toISOString()
    : modifiedAt.toISOString();

  return {
    id: relative(replayRoot, path).split(sep).join('/'),
    filename,
    shipName: titleCase(match?.[4] ?? filename.replace(/\.wowsreplay$/i, '')),
    shipClass,
    mapName: titleCase((match?.[6] ?? 'Unknown map').replace(/^(?:NE|OC)_/i, '')),
    playedAt,
    modifiedAt: modifiedAt.toISOString(),
    size,
  };
}

async function localReplays(): Promise<ReplaySummary[]> {
  const paths: string[] = [];
  const visit = async (directory: string, depth: number): Promise<void> => {
    if (depth > 2) return;
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) await visit(path, depth + 1);
      else if (entry.isFile() && extname(entry.name).toLowerCase() === '.wowsreplay') paths.push(path);
    }
  };
  await visit(replayRoot, 0);
  const summaries = await Promise.all(paths.map(async (path) => {
    const info = await stat(path);
    return replaySummary(path, info.size, info.mtime);
  }));
  return summaries.sort((left, right) => right.playedAt.localeCompare(left.playedAt)).slice(0, 100);
}

function json(response: import('node:http').ServerResponse, status: number, value: unknown): void {
  response.statusCode = status;
  response.setHeader('Content-Type', 'application/json; charset=utf-8');
  response.setHeader('Cache-Control', 'no-store');
  response.end(JSON.stringify(value));
}

async function body(request: import('node:http').IncomingMessage): Promise<unknown> {
  let value = '';
  for await (const chunk of request) {
    value += chunk.toString();
    if (value.length > 8_192) throw new Error('Request body is too large');
  }
  return JSON.parse(value || '{}');
}

function replayApi(): Plugin {
  let exportRunning = false;
  return {
    name: 'local-replay-experiment-api',
    configureServer(server) {
      server.middlewares.use(async (request, response, next) => {
        const url = new URL(request.url ?? '/', 'http://127.0.0.1');
        if (url.pathname === '/api/replays' && request.method === 'GET') {
          try {
            json(response, 200, { replayRoot, replays: await localReplays() });
          } catch (reason) {
            json(response, 500, { error: reason instanceof Error ? reason.message : String(reason) });
          }
          return;
        }
        if (url.pathname !== '/api/replays/load' || request.method !== 'POST') {
          next();
          return;
        }
        if (exportRunning) {
          json(response, 409, { error: 'Another replay is still being prepared.' });
          return;
        }

        try {
          const value = await body(request) as { id?: unknown };
          if (typeof value.id !== 'string' || !value.id.toLowerCase().endsWith('.wowsreplay')) {
            throw new Error('A valid replay id is required.');
          }
          const root = resolve(replayRoot);
          const replay = resolve(root, value.id.split('/').join(sep));
          if (!replay.toLowerCase().startsWith(`${root.toLowerCase()}${sep}`)) throw new Error('Replay path escapes the configured folder.');
          const info = await stat(replay);
          if (!info.isFile()) throw new Error('Replay does not exist.');

          exportRunning = true;
          await new Promise<void>((resolveExport, rejectExport) => {
            const child = spawn('cargo', [
              'run', '--release', '--quiet', '--manifest-path', exporterManifest, '--',
              '--game', gameRoot,
              '--replay', replay,
              '--output', generatedOutput,
            ], { cwd: experimentRoot, windowsHide: true });
            let diagnostics = '';
            child.stderr.setEncoding('utf8');
            child.stderr.on('data', (chunk: string) => { diagnostics = `${diagnostics}${chunk}`.slice(-12_000); });
            child.on('error', rejectExport);
            child.on('close', (code) => {
              if (code === 0) resolveExport();
              else rejectExport(new Error(diagnostics.trim() || `Exporter exited with code ${code}`));
            });
          });
          json(response, 200, { ok: true, id: value.id });
        } catch (reason) {
          json(response, 500, { error: reason instanceof Error ? reason.message : String(reason) });
        } finally {
          exportRunning = false;
        }
      });
    },
  };
}

export default defineConfig(({ mode }) => {
  // `npm run build:bridge` (`vite build --mode bridge`) produces the dist/
  // the Tauri bundler stages into src-tauri/player/, served by the bridge
  // under /player/*: assets must be prefixed accordingly, and the app must
  // read replays/scenes from the bridge's routes instead of the vite-dev
  // experiment's /api/* endpoints (see src/engine/bridgeApi.ts).
  const bridge = mode === 'bridge';
  return {
    base: bridge ? '/player/' : '/',
    plugins: [react(), replayApi()],
    define: {
      'import.meta.env.VITE_BRIDGE': JSON.stringify(bridge ? '1' : ''),
    },
    test: {
      environment: 'node',
    },
  };
});
