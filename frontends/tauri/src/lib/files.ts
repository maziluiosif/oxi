// File access through the daemon (`fs.*`), shared by the explorer, editor, palette and mentions.

import {rpc} from './rpc';

const lists = new Map<string, {at: number; files: Promise<string[]>}>();

/** Relative paths of the workspace's non-ignored files, cached for a little while. */
export function fileList(root: string, fresh = false): Promise<string[]> {
	const hit = lists.get(root);
	if (hit && !fresh && Date.now() - hit.at < 30_000) return hit.files;
	// Nested projects' dependency folders aren't covered by the root .gitignore.
	const files = rpc
		.request<string[]>('fs.walk', {root})
		.then(list => list.filter(f => !/(^|\/)(node_modules|target|dist|build|\.venv|__pycache__)\//.test(f)))
		.catch(() => [] as string[]);
	lists.set(root, {at: Date.now(), files});
	return files;
}

export function invalidateFiles(root: string) {
	lists.delete(root);
}

export type DirEntry = {name: string; dir: boolean};

export async function listDir(path: string, hidden = true): Promise<DirEntry[]> {
	const result = await rpc.request('fs.list', {path, files: true, hidden});
	return result.entries;
}

function decode(data: string): Uint8Array {
	const bin = atob(data);
	const bytes = new Uint8Array(bin.length);
	for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
	return bytes;
}

function encode(bytes: Uint8Array): string {
	let bin = '';
	const chunk = 0x8000;
	for (let i = 0; i < bytes.length; i += chunk) bin += String.fromCharCode(...bytes.subarray(i, i + chunk));
	return btoa(bin);
}

export type FileRead = {bytes: Uint8Array; base64: string; modified: number | null; size: number};

export async function readFile(path: string, max = 30_000_000): Promise<FileRead> {
	const result = await rpc.request('fs.read', {path, max});
	return {bytes: decode(result.data), base64: result.data, modified: result.meta?.modified ?? null, size: result.meta?.len ?? 0};
}

export function isBinary(bytes: Uint8Array): boolean {
	const n = Math.min(bytes.length, 8000);
	for (let i = 0; i < n; i++) if (bytes[i] === 0) return true;
	return false;
}

export async function readText(path: string): Promise<{text: string; modified: number | null}> {
	const file = await readFile(path, 20_000_000);
	return {text: new TextDecoder().decode(file.bytes), modified: file.modified};
}

export async function writeText(path: string, text: string): Promise<number | null> {
	const meta = await rpc.request('fs.write', {path, data: encode(new TextEncoder().encode(text))});
	return meta?.modified ?? null;
}

export async function stat(path: string): Promise<{exists: boolean; dir?: boolean; modified?: number; len?: number}> {
	return rpc.request('fs.stat', {path});
}

export function fsOp(op: string, path: string, to?: string) {
	return rpc.request('fs.op', {op, path, to});
}

const IMAGE = /\.(png|jpe?g|gif|webp|bmp|ico|svg)$/i;
export const isImagePath = (path: string) => IMAGE.test(path);

export function mimeOf(path: string): string {
	const ext = path.split('.').pop()?.toLowerCase() ?? '';
	return (
		{png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', webp: 'image/webp', bmp: 'image/bmp', ico: 'image/x-icon', svg: 'image/svg+xml'}[ext] ??
		'application/octet-stream'
	);
}
