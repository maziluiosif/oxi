// `npm run dev` in a plain browser: this Vite middleware runs `oxi headless` and relays it over
// HTTP (POST to send a line, server-sent events for what oxi prints), standing in for the Rust
// bridge. Only the dev server has it; the Tauri build never loads it.

import {spawn, type ChildProcessWithoutNullStreams} from 'node:child_process';
import {existsSync} from 'node:fs';
import {homedir} from 'node:os';
import path from 'node:path';
import type {Plugin} from 'vite';
import type {ServerResponse} from 'node:http';

function findOxi(): string | undefined {
	const candidates = [process.env.OXI_PATH, path.join(homedir(), '.local/bin/oxi'), '/Applications/oxi.app/Contents/MacOS/oxi'];
	for (const dir of (process.env.PATH ?? '').split(path.delimiter)) if (dir) candidates.push(path.join(dir, 'oxi'));
	return candidates.find(c => c && existsSync(c));
}

export function oxiBridge(): Plugin {
	let proc: ChildProcessWithoutNullStreams | null = null;
	const clients = new Set<ServerResponse>();
	let buffer = '';

	const start = () => {
		if (proc && proc.exitCode === null) return findOxi();
		const binary = findOxi();
		if (!binary) throw new Error('oxi not found (set OXI_PATH)');
		proc = spawn(binary, ['headless'], {cwd: homedir(), stdio: ['pipe', 'pipe', 'pipe'], detached: true});
		proc.stdout.setEncoding('utf8');
		proc.stdout.on('data', (chunk: string) => {
			buffer += chunk;
			let at: number;
			while ((at = buffer.indexOf('\n')) >= 0) {
				const line = buffer.slice(0, at);
				buffer = buffer.slice(at + 1);
				if (line.trim()) for (const c of clients) c.write(`data: ${line}\n\n`);
			}
		});
		proc.stderr.on('data', () => {});
		proc.on('exit', () => {
			proc = null;
			for (const c of clients) c.write(`event: exit\ndata: 1\n\n`);
		});
		return binary;
	};

	return {
		name: 'oxi-dev-bridge',
		apply: 'serve',
		configureServer(server) {
			server.middlewares.use('/__oxi', (req, res) => {
				if (req.url === '/events') {
					res.writeHead(200, {'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache', Connection: 'keep-alive'});
					res.write(': hi\n\n');
					clients.add(res);
					req.on('close', () => clients.delete(res));
					return;
				}
				if (req.url === '/start') {
					try {
						res.end(JSON.stringify({binary: start()}));
					} catch (e) {
						res.statusCode = 500;
						res.end(String(e));
					}
					return;
				}
				if (req.url === '/send') {
					let body = '';
					req.on('data', chunk => (body += chunk));
					req.on('end', () => {
						if (!proc) {
							res.statusCode = 503;
							res.end('oxi is not running');
							return;
						}
						proc.stdin.write(body + '\n');
						res.end('ok');
					});
					return;
				}
				res.statusCode = 404;
				res.end();
			});
			server.httpServer?.on('close', () => proc?.kill());
		},
	};
}
