// How a tool call reads in the transcript: a verb, its subject, a short result, an icon. The
// mapping follows `frontends/code/src/tools.ts` so every oxi frontend names calls alike.

import type {IconName} from './icons';
import type {ToolBlock} from './types';

export type Family = 'bash' | 'read' | 'write' | 'edit' | 'search' | 'list' | 'fetch' | 'websearch' | 'task' | 'todo' | 'plan' | 'other';

export type ToolView = {
	/** Past tense once done ("Ran"), present while running ("Running"). */
	verb: string;
	subject: string;
	family: Family;
	icon: IconName;
	/** The file the call is about, for "open in editor". */
	file: string | null;
};

const NAMES: Record<string, Family> = {
	bash: 'bash',
	terminal: 'bash',
	execute: 'bash',
	read: 'read',
	read_file: 'read',
	write: 'write',
	write_file: 'write',
	edit: 'edit',
	multiedit: 'edit',
	multi_edit: 'edit',
	str_replace: 'edit',
	notebookedit: 'edit',
	grep: 'search',
	find: 'search',
	glob: 'search',
	codebase_search: 'search',
	search: 'search',
	ls: 'list',
	web_search: 'websearch',
	websearch: 'websearch',
	web_fetch: 'fetch',
	webfetch: 'fetch',
	fetch: 'fetch',
	task: 'task',
	agent: 'task',
	think: 'task',
	todo_write: 'todo',
	todowrite: 'todo',
	exitplanmode: 'plan',
	exit_plan_mode: 'plan',
	switch_mode: 'plan',
};

const VERBS: Record<Family, [string, string]> = {
	bash: ['Running', 'Ran'],
	read: ['Reading', 'Read'],
	write: ['Writing', 'Wrote'],
	edit: ['Editing', 'Edited'],
	search: ['Searching', 'Searched'],
	list: ['Listing', 'Listed'],
	fetch: ['Fetching', 'Fetched'],
	websearch: ['Searching the web', 'Searched the web'],
	task: ['Working on', 'Task'],
	todo: ['Updating todos', 'Updated todos'],
	plan: ['Planning', 'Plan'],
	other: ['Calling', 'Called'],
};

const ICONS: Record<Family, IconName> = {
	bash: 'terminal',
	read: 'fileText',
	write: 'filePlus',
	edit: 'edit',
	search: 'search',
	list: 'list',
	fetch: 'globe',
	websearch: 'globe',
	task: 'layers',
	todo: 'checklist',
	plan: 'map',
	other: 'wrench',
};

export function parseArgs(block: ToolBlock): Record<string, any> {
	if (!block.args) return {};
	try {
		const value = JSON.parse(block.args);
		return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
	} catch {
		return {};
	}
}

export function relativePath(file: string, root: string): string {
	if (!file) return file;
	const r = root.replace(/[\\/]+$/, '');
	if (file.startsWith(r + '/') || file.startsWith(r + '\\')) return file.slice(r.length + 1);
	return file;
}

const firstLine = (text: string) => (text.split('\n')[0] ?? '').trim();

export function truncate(text: string, max: number): string {
	return text.length > max ? text.slice(0, max - 1) + '…' : text;
}

/** ACP titles repeat the tool ("Read src/x.rs", "`ls`"); keep what is about the call. */
function cleanTitle(title: string): string {
	let t = firstLine(title);
	if (t.startsWith('`') && t.endsWith('`') && t.length > 1) t = t.slice(1, -1);
	if (/^(read|edit|write) file$/i.test(t)) return '';
	const m = t.match(/^(Read|Edit|Write|Run|Terminal|Bash|Search|Grep|Find|List|Fetch)\s+(.*)$/i);
	if (m) t = m[2];
	return t.replace(/^`|`$/g, '');
}

export function familyOf(block: ToolBlock): Family {
	const lower = (block.name || '').toLowerCase();
	if (/todo/.test(lower)) return 'todo';
	if (NAMES[lower]) return NAMES[lower];
	const mcp = lower.match(/^mcp__.+?__(.+)$/);
	if (mcp && NAMES[mcp[1]]) return NAMES[mcp[1]];
	return NAMES[block.kind] ?? (block.kind === 'edit' ? 'edit' : 'other');
}

export function toolView(block: ToolBlock, root: string): ToolView {
	const family = familyOf(block);
	const args = parseArgs(block);
	const done = block.status !== 'in_progress' && block.status !== 'pending';
	const [running, past] = VERBS[family];
	let verb = done ? past : running;
	const fileArg = String(args.file_path ?? args.path ?? args.notebook_path ?? block.locations?.[0]?.path ?? '');
	const title = cleanTitle(block.title ?? '');
	let subject = '';
	let file: string | null = null;
	switch (family) {
		case 'bash':
			subject = firstLine(String(args.command ?? title));
			break;
		case 'read':
		case 'write':
		case 'edit':
			file = fileArg || null;
			subject = relativePath(fileArg || title, root);
			break;
		case 'search': {
			const pattern = args.pattern ?? args.query ?? args.glob;
			subject = pattern !== undefined ? String(pattern) : title;
			if (args.path) subject += `  in ${relativePath(String(args.path), root)}`;
			break;
		}
		case 'list':
			subject = relativePath(fileArg || title || '.', root);
			break;
		case 'fetch':
			subject = String(args.url ?? title);
			break;
		case 'websearch':
			subject = String(args.query ?? title);
			break;
		case 'task':
			subject = String(args.description ?? title);
			break;
		case 'todo':
		case 'plan':
			subject = '';
			break;
		default: {
			const mcp = (block.name || '').match(/^mcp__(.+?)__(.+)$/);
			if (mcp) {
				verb = done ? 'Called' : 'Calling';
				subject = `${mcp[2]} · ${mcp[1]}`;
			} else {
				verb = humanize(block.name || 'tool');
				subject = fileArg ? relativePath(fileArg, root) : title;
			}
		}
	}
	return {verb, subject: truncate(subject, 200), family, icon: ICONS[family], file};
}

function humanize(name: string): string {
	const text = name.replace(/[_-]+/g, ' ').trim();
	return text.charAt(0).toUpperCase() + text.slice(1);
}

export type DiffLine = {type: 'add' | 'remove' | 'context' | 'gap'; text: string; old?: number; line?: number};

/**
 * Lines of a unified diff with their line numbers. ACP agents send diffs of the edited
 * snippet, numbered from 1; `offset` moves them to where the snippet is in the file.
 */
export function parseDiff(diff: string, offset = 0): {lines: DiffLine[]; added: number; removed: number} {
	const lines: DiffLine[] = [];
	let added = 0;
	let removed = 0;
	let oldLine = 0;
	let newLine = 0;
	let hunks = 0;
	const rows = diff.split('\n');
	if (rows[rows.length - 1] === '') rows.pop();
	for (const raw of rows) {
		const hunk = raw.match(/^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/);
		if (!hunk && (hunks === 0 || raw.startsWith('\\'))) continue;
		if (hunk) {
			if (hunks++ > 0) lines.push({type: 'gap', text: ''});
			oldLine = Number(hunk[1]) + offset;
			newLine = Number(hunk[2]) + offset;
			continue;
		}
		if (raw.startsWith('+')) {
			lines.push({type: 'add', text: raw.slice(1), line: newLine++});
			added++;
		} else if (raw.startsWith('-')) {
			lines.push({type: 'remove', text: raw.slice(1), old: oldLine++});
			removed++;
		} else if (raw.startsWith(' ') || raw === '') {
			lines.push({type: 'context', text: raw.slice(1), old: oldLine++, line: newLine++});
		}
	}
	return {lines, added, removed};
}

/** Where an agent's snippet diff sits in the file: its first location is the edit's line. */
export function diffOffset(block: ToolBlock): number {
	const line = block.locations?.find(l => l.line)?.line;
	const first = block.diff?.match(/^@@ -1(?:,\d+)? \+1(?:,\d+)? @@/m);
	return line && line > 1 && first ? line - 1 : 0;
}

/** The short note after the subject: "13 lines", "+4 −1", "3 results". */
export function resultNote(block: ToolBlock, view: ToolView): string {
	if (block.status === 'failed') return 'failed';
	if (block.status === 'interrupted') return 'interrupted';
	if (block.status === 'in_progress' || block.status === 'pending') return '';
	const output = (block.output ?? '').replace(/\s+$/, '');
	const count = output ? output.split('\n').filter(line => !/^```/.test(line)).length : 0;
	switch (view.family) {
		case 'bash':
			return count ? `${count} ${count === 1 ? 'line' : 'lines'}` : '';
		case 'read': {
			const args = parseArgs(block);
			if (args.offset || args.limit) {
				const from = Number(args.offset ?? 1) || 1;
				return `lines ${from}–${from + Math.max(count - 1, 0)}`;
			}
			return count ? `${count} ${count === 1 ? 'line' : 'lines'}` : '';
		}
		case 'search':
			if (!output || /^no (matches|files|results)/i.test(output)) return 'no results';
			return `${count} ${count === 1 ? 'result' : 'results'}`;
		case 'list':
			return count ? `${count} entries` : '';
		default:
			return '';
	}
}

export type Todo = {content: string; status: 'pending' | 'in_progress' | 'completed'; activeForm?: string};

/** The latest checklist the agent wrote in this turn (`todo_write`, Claude Code's TodoWrite). */
export function todosOf(block: ToolBlock): Todo[] | null {
	if (familyOf(block) !== 'todo') return null;
	const args = parseArgs(block);
	const items = args.todos ?? args.items;
	if (!Array.isArray(items)) return null;
	return items
		.map((item: any) => ({
			content: String(item.content ?? item.title ?? item.text ?? ''),
			status: item.status === 'completed' || item.status === 'in_progress' ? item.status : 'pending',
			activeForm: item.activeForm,
		}))
		.filter((t: Todo) => t.content);
}

export function formatDuration(secs: number): string {
	if (secs < 60) return `${Math.max(1, Math.round(secs))}s`;
	const m = Math.floor(secs / 60);
	const s = Math.round(secs % 60);
	if (m < 60) return s ? `${m}m ${s}s` : `${m}m`;
	const h = Math.floor(m / 60);
	return `${h}h ${m % 60}m`;
}

export function relativeTime(unixSecs: number): string {
	const diff = Date.now() / 1000 - unixSecs;
	if (diff < 60) return 'now';
	if (diff < 3600) return `${Math.floor(diff / 60)}m`;
	if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
	if (diff < 86400 * 30) return `${Math.floor(diff / 86400)}d`;
	if (diff < 86400 * 365) return `${Math.floor(diff / 86400 / 30)}mo`;
	return `${Math.floor(diff / 86400 / 365)}y`;
}

export function dateGroup(unixSecs: number): string {
	const now = new Date();
	const startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() / 1000;
	if (unixSecs >= startOfToday) return 'Today';
	if (unixSecs >= startOfToday - 86400) return 'Yesterday';
	if (unixSecs >= startOfToday - 86400 * 7) return 'Previous 7 days';
	if (unixSecs >= startOfToday - 86400 * 30) return 'Previous 30 days';
	return 'Older';
}

/** Fuzzy match for the palettes: every query char in order; bonus for word starts and runs. */
export function fuzzy(query: string, text: string): {score: number; hits: number[]} | null {
	if (!query) return {score: 0, hits: []};
	const q = query.toLowerCase();
	const t = text.toLowerCase();
	const hits: number[] = [];
	let score = 0;
	let ti = 0;
	let prev = -2;
	for (const ch of q) {
		if (ch === ' ') continue;
		const found = t.indexOf(ch, ti);
		if (found < 0) return null;
		const boundary = found === 0 || /[\s/_.\-]/.test(t[found - 1]);
		score += boundary ? 8 : 1;
		if (found === prev + 1) score += 5;
		score -= Math.min(found - ti, 6) * 0.3;
		hits.push(found);
		prev = found;
		ti = found + 1;
	}
	// Prefer whole-word runs, and matches in the file name over the folders.
	const slash = text.lastIndexOf('/');
	const name = t.slice(slash + 1);
	const compact = q.replace(/\s+/g, '');
	if (name.startsWith(compact)) score += 60;
	else if (name.includes(compact)) score += 40;
	else if (t.includes(compact)) score += 25;
	if (hits.length && hits[0] > slash) score += 10;
	score -= text.length * 0.02;
	return {score, hits};
}

const EXT_LANG: Record<string, string> = {
	rs: 'rust', ts: 'typescript', tsx: 'typescript', js: 'javascript', jsx: 'javascript', mjs: 'javascript', cjs: 'javascript',
	py: 'python', go: 'go', java: 'java', kt: 'kotlin', kts: 'kotlin', swift: 'swift', c: 'c', h: 'c', cc: 'cpp', cpp: 'cpp',
	hpp: 'cpp', cs: 'csharp', rb: 'ruby', php: 'php', sh: 'bash', bash: 'bash', zsh: 'bash', json: 'json', toml: 'ini',
	yaml: 'yaml', yml: 'yaml', md: 'markdown', html: 'xml', xml: 'xml', svelte: 'xml', vue: 'xml', css: 'css', scss: 'scss',
	sql: 'sql', lua: 'lua', dart: 'dart', r: 'r', ini: 'ini', dockerfile: 'dockerfile', makefile: 'makefile', gradle: 'kotlin',
};

/** highlight.js language for a file path, or '' for plain text. */
export function langOf(path: string): string {
	const name = path.split(/[\\/]/).pop()?.toLowerCase() ?? '';
	if (name === 'dockerfile') return 'dockerfile';
	if (name === 'makefile') return 'makefile';
	const ext = name.includes('.') ? name.split('.').pop()! : '';
	return EXT_LANG[ext] ?? '';
}
