// App state: workspaces, chat lists, the chats loaded here, tabs and panels. The daemon owns
// the chats; this mirrors what it reports and sends what the user does.

import {rpc, errorText, type ConnectionState} from './rpc';
import type {
	Approval,
	Attachment,
	ChatConfig,
	ChatSummary,
	Message,
	Provider,
	Quota,
} from './types';

export type Tab =
	| {key: string; kind: 'chat'; chat: string}
	| {key: string; kind: 'file'; path: string; root: string; line?: number}
	| {key: string; kind: 'diff'; title: string; text: string; root: string; path?: string; staged?: boolean};

export type SidebarView = 'chats' | 'files' | 'search';
export type ThemeId = 'dark' | 'light' | 'midnight' | 'system';

export class Chat {
	/** The session file path; a draft has none until its first prompt is sent. */
	id = $state<string | null>(null);
	key: string;
	root: string;
	title = $state('New chat');
	messages = $state.raw<Message[]>([]);
	config = $state<ChatConfig | null>(null);
	plan = $state(false);
	evidence = $state(false);
	running = $state(false);
	queued = $state(0);
	approval = $state<Approval | null>(null);
	status = $state<string | null>(null);
	context = $state<{used: number; size: number} | null>(null);
	loaded = $state(false);
	error = $state<string | null>(null);
	/** When the running turn started here, for the elapsed counter. */
	startedAt = $state<number | null>(null);

	constructor(key: string, root: string, id: string | null) {
		this.key = key;
		this.root = root;
		this.id = id;
	}
}

export type Toast = {id: number; text: string; kind: 'info' | 'error' | 'success'};

const stored = <T>(key: string, fallback: T): T => {
	try {
		const value = localStorage.getItem(key);
		return value == null ? fallback : (JSON.parse(value) as T);
	} catch {
		return fallback;
	}
};
const store = (key: string, value: unknown) => localStorage.setItem(key, JSON.stringify(value));

let draftCounter = 0;

class AppState {
	connection = $state<ConnectionState>('connecting');
	connectionDetail = $state('');
	version = $state('');
	providers = $state.raw<Provider[]>([]);
	settings = $state.raw<any>(null);

	workspaces = $state<string[]>([]);
	folded = $state<Record<string, boolean>>(stored('oxi.folded', {}));
	activeRoot = $state<string>(stored('oxi.activeRoot', ''));
	chatLists = $state<Record<string, ChatSummary[]>>({});
	chats = $state<Record<string, Chat>>({});

	tabs = $state<Tab[]>([]);
	activeTab = $state<string>('');
	dirty = $state<Record<string, boolean>>({});

	sidebarView = $state<SidebarView>('chats');
	sidebarOpen = $state(stored('oxi.sidebarOpen', true));
	sidebarWidth = $state(stored('oxi.sidebarWidth', 268));
	gitOpen = $state(stored('oxi.gitOpen', false));
	gitWidth = $state(stored('oxi.gitWidth', 340));
	terminalOpen = $state(stored('oxi.terminalOpen', false));
	terminalHeight = $state(stored('oxi.terminalHeight', 260));
	theme = $state<ThemeId>(stored('oxi.theme', 'system'));
	fontSize = $state<number>(stored('oxi.fontSize', 14));
	toolDetail = $state<'minimal' | 'balanced' | 'expanded'>(stored('oxi.toolDetail', 'balanced'));
	focusMode = $state(false);

	settingsOpen = $state(false);
	settingsTab = $state('general');
	palette = $state<null | 'commands' | 'files' | 'chats'>(null);
	toasts = $state<Toast[]>([]);
	quota = $state<Record<string, Quota>>({});
	windowFocused = $state(true);

	get activeChat(): Chat | null {
		const tab = this.tabs.find(t => t.key === this.activeTab);
		return tab?.kind === 'chat' ? (this.chats[tab.chat] ?? null) : null;
	}

	get tab(): Tab | undefined {
		return this.tabs.find(t => t.key === this.activeTab);
	}

	// ---------- lifecycle ----------

	async boot() {
		rpc.onState((state, detail) => {
			this.connection = state;
			this.connectionDetail = detail;
			if (state === 'connected') this.onConnected();
		});
		this.listen();
		await rpc.start();
	}

	private async onConnected() {
		try {
			const init = await rpc.request('initialize');
			this.version = init.version;
			this.providers = init.providers;
		} catch (e) {
			this.toast(errorText(e), 'error');
		}
		await this.loadSettings();
		if (!this.activeRoot || !this.workspaces.includes(this.activeRoot)) {
			this.activeRoot = this.workspaces[0] ?? '';
		}
		await Promise.all(this.workspaces.map(root => this.refreshChats(root)));
		// Chats open before a reconnect: read them again, a turn may have ended meanwhile.
		for (const chat of Object.values(this.chats)) {
			if (chat.id) this.loadChat(chat);
		}
		if (!this.tabs.length) this.restoreTabs();
	}

	async loadSettings() {
		try {
			const result = await rpc.request('settings.get');
			this.applySettings(result);
		} catch (e) {
			// An older oxi without `settings.*`: fall back to the folders with chats.
			const list = await rpc.request<any[]>('workspaces.list').catch(() => []);
			this.workspaces = list.slice(0, 8).map(w => w.path);
		}
	}

	applySettings(result: any) {
		this.settings = result;
		const entries: any[] = result?.settings?.workspaces ?? [];
		this.workspaces = entries.map(e => e.root_path).filter((p: string) => !p.includes('://'));
	}

	async patchSettings(patch: Record<string, unknown>) {
		const result = await rpc.request('settings.set', {patch});
		this.applySettings(result);
		return result;
	}

	private restoreTabs() {
		const saved = stored<{tabs: any[]; active: string}>('oxi.tabs', {tabs: [], active: ''});
		for (const tab of saved.tabs) {
			if (tab.kind === 'chat' && tab.id) this.openChat(tab.root, tab.id, false);
			if (tab.kind === 'file') this.openFile(tab.root, tab.path, undefined, false);
		}
		const active = this.tabs.find(t => t.key === saved.active);
		if (active) this.activeTab = active.key;
		if (!this.tabs.some(t => t.kind === 'chat')) {
			const root = this.activeRoot;
			const first = root && this.chatLists[root]?.[0];
			if (first) this.openChat(root, first.id);
			else if (root) this.newChat(root);
		}
		if (!this.activeTab && this.tabs.length) this.activeTab = this.tabs[0].key;
	}

	saveTabs() {
		const tabs = this.tabs
			.map(tab => {
				if (tab.kind === 'chat') {
					const chat = this.chats[tab.chat];
					return chat?.id ? {kind: 'chat', id: chat.id, root: chat.root} : null;
				}
				if (tab.kind === 'file') return {kind: 'file', path: tab.path, root: tab.root};
				return null;
			})
			.filter(Boolean);
		store('oxi.tabs', {tabs, active: this.activeTab});
		store('oxi.activeRoot', this.activeRoot);
	}

	persistLayout() {
		store('oxi.sidebarOpen', this.sidebarOpen);
		store('oxi.sidebarWidth', this.sidebarWidth);
		store('oxi.gitOpen', this.gitOpen);
		store('oxi.gitWidth', this.gitWidth);
		store('oxi.terminalOpen', this.terminalOpen);
		store('oxi.terminalHeight', this.terminalHeight);
		store('oxi.folded', this.folded);
		store('oxi.theme', this.theme);
		store('oxi.fontSize', this.fontSize);
		store('oxi.toolDetail', this.toolDetail);
	}

	// ---------- notifications ----------

	private chatById(id: string): Chat | undefined {
		return this.chats[id];
	}

	private listen() {
		rpc.on('message', ({chat, index, message}) => {
			const c = this.chatById(chat);
			if (!c) return;
			const messages = c.messages.slice();
			messages[index] = message;
			c.messages = messages;
		});
		rpc.on('turn', params => {
			const c = this.chatById(params.chat);
			if (c) {
				const wasRunning = c.running;
				c.running = params.running;
				c.queued = params.queued ?? 0;
				if (params.plan != null) c.plan = params.plan;
				if (params.evidence != null) c.evidence = params.evidence;
				if (params.running && !wasRunning) c.startedAt = Date.now();
				if (!params.running) c.startedAt = null;
				if (params.outcome) this.onTurnFinished(c, params.outcome, params.error);
			}
			this.markRunning(params.chat, params.running);
		});
		rpc.on('approval', ({chat, approval}) => {
			const c = this.chatById(chat);
			if (c) c.approval = approval;
			for (const list of Object.values(this.chatLists)) {
				const item = list.find(s => s.id === chat);
				if (item) item.approval = !!approval;
			}
			if (approval && c) this.notify('Approval needed', `${c.title}: ${approval.summary}`);
		});
		rpc.on('status', ({chat, text}) => {
			const c = this.chatById(chat);
			if (c) c.status = text;
		});
		rpc.on('context', ({chat, used, size}) => {
			const c = this.chatById(chat);
			if (c) c.context = {used, size};
		});
		rpc.on('config', ({chat, config}) => {
			const c = this.chatById(chat);
			if (c) c.config = config;
		});
		rpc.on('title', ({chat, title}) => {
			const c = this.chatById(chat);
			if (c) c.title = title;
		});
		rpc.on('chats_changed', ({root}) => {
			if (this.workspaces.includes(root) || this.chatLists[root]) this.refreshChats(root);
		});
		rpc.on('chat_reloaded', ({chat}) => {
			const c = this.chatById(chat);
			if (c) this.loadChat(c);
		});
		rpc.on('chat_removed', ({chat}) => {
			const c = this.chatById(chat);
			if (c) this.closeTab(`chat:${c.key}`);
		});
		rpc.on('settings_changed', () => this.loadSettings());
	}

	private markRunning(id: string, running: boolean) {
		for (const list of Object.values(this.chatLists)) {
			const item = list.find(s => s.id === id);
			if (item) item.running = running;
		}
	}

	private onTurnFinished(chat: Chat, outcome: string, error: string | null) {
		if (outcome === 'failed' && error) chat.error = error;
		if (outcome === 'success') this.notify('Response ready', chat.title);
		if (outcome === 'failed') this.notify('Turn failed', error ?? chat.title);
		this.refreshQuota(chat.config?.provider);
	}

	private async notify(title: string, body: string) {
		if (this.windowFocused) return;
		if (this.settings?.settings?.notify_in_background === false) return;
		try {
			const n = await import('@tauri-apps/plugin-notification');
			let granted = await n.isPermissionGranted();
			if (!granted) granted = (await n.requestPermission()) === 'granted';
			if (granted) n.sendNotification({title, body});
			const {getCurrentWindow} = await import('@tauri-apps/api/window');
			getCurrentWindow().requestUserAttention(2).catch(() => {});
		} catch {}
	}

	// ---------- workspaces & chat lists ----------

	async refreshChats(root: string) {
		try {
			const list = await rpc.request<ChatSummary[]>('chats.list', {root});
			list.sort((a, b) => b.modified - a.modified);
			this.chatLists[root] = list;
		} catch (e) {
			this.chatLists[root] = [];
		}
	}

	async addWorkspace(path: string) {
		if (!this.workspaces.includes(path)) {
			const entries = [...(this.settings?.settings?.workspaces ?? []), {root_path: path, folded: false}];
			try {
				await this.patchSettings({workspaces: entries});
			} catch (e) {
				this.workspaces = [...this.workspaces, path];
			}
		}
		this.activeRoot = path;
		this.folded[path] = false;
		await this.refreshChats(path);
		this.saveTabs();
	}

	async removeWorkspace(path: string) {
		const entries = (this.settings?.settings?.workspaces ?? []).filter((e: any) => e.root_path !== path);
		try {
			await this.patchSettings({workspaces: entries});
		} catch {
			this.workspaces = this.workspaces.filter(w => w !== path);
		}
		if (this.activeRoot === path) this.activeRoot = this.workspaces[0] ?? '';
	}

	setActiveRoot(root: string) {
		if (this.activeRoot === root) return;
		this.activeRoot = root;
		this.saveTabs();
	}

	// ---------- chats ----------

	openChat(root: string, id: string, activate = true) {
		let chat = this.chats[id];
		if (!chat) {
			chat = new Chat(id, root, id);
			const summary = this.chatLists[root]?.find(s => s.id === id);
			if (summary) chat.title = summary.title;
			this.chats[id] = chat;
			this.loadChat(chat);
		}
		const key = `chat:${chat.key}`;
		if (!this.tabs.some(t => t.key === key)) {
			// Replace the active chat tab rather than piling up tabs, like the app's sidebar.
			const current = this.tab;
			const replace = activate && current?.kind === 'chat' && !this.pinnedTabs.has(current.key);
			const tab: Tab = {key, kind: 'chat', chat: chat.key};
			if (replace) {
				const index = this.tabs.findIndex(t => t.key === current!.key);
				this.tabs[index] = tab;
			} else {
				this.tabs.push(tab);
			}
		}
		if (activate) {
			this.activeTab = key;
			this.activeRoot = root;
		}
		this.saveTabs();
		return chat;
	}

	/** Chat tabs opened with "Open in new tab": a click in the sidebar doesn't replace them. */
	pinnedTabs = new Set<string>();

	openChatInNewTab(root: string, id: string) {
		const key = `chat:${id}`;
		if (!this.tabs.some(t => t.key === key)) {
			this.openChat(root, id, false);
			this.pinnedTabs.add(key);
		}
		this.activeTab = key;
		this.saveTabs();
	}

	async loadChat(chat: Chat) {
		if (!chat.id) return;
		try {
			const result = await rpc.request('chat.open', {root: chat.root, chat: chat.id});
			chat.title = result.title;
			chat.config = result.config;
			chat.plan = result.plan;
			chat.evidence = result.evidence;
			chat.running = result.running;
			chat.queued = result.queued;
			chat.approval = result.approval;
			chat.status = result.status;
			chat.context = result.context;
			chat.messages = result.messages;
			chat.loaded = true;
			if (chat.running && !chat.startedAt) chat.startedAt = Date.now();
			this.refreshQuota(chat.config?.provider);
		} catch (e) {
			chat.error = errorText(e);
			chat.loaded = true;
		}
	}

	newChat(root = this.activeRoot) {
		if (!root) {
			this.toast('Open a folder first', 'info');
			return null;
		}
		// Reuse an empty draft for this folder.
		const existing = Object.values(this.chats).find(c => !c.id && c.root === root);
		const chat = existing ?? new Chat(`draft-${++draftCounter}`, root, null);
		if (!existing) {
			chat.loaded = true;
			this.chats[chat.key] = chat;
			this.draftConfig(chat);
		}
		const key = `chat:${chat.key}`;
		if (!this.tabs.some(t => t.key === key)) {
			const current = this.tab;
			if (current?.kind === 'chat' && !this.pinnedTabs.has(current.key)) {
				const index = this.tabs.findIndex(t => t.key === current.key);
				this.tabs[index] = {key, kind: 'chat', chat: chat.key};
			} else {
				this.tabs.push({key, kind: 'chat', chat: chat.key});
			}
		}
		this.activeTab = key;
		this.activeRoot = root;
		return chat;
	}

	private async draftConfig(chat: Chat) {
		try {
			chat.config = await rpc.request('chat.set_config', {root: chat.root});
		} catch {}
	}

	async setConfig(chat: Chat, change: {provider?: string; model?: string; effort?: string}) {
		try {
			const config = await rpc.request('chat.set_config', {root: chat.root, chat: chat.id ?? undefined, ...change});
			chat.config = config;
			this.refreshQuota(config.provider);
		} catch (e) {
			this.toast(errorText(e), 'error');
		}
	}

	async send(chat: Chat, text: string, attachments: Attachment[]) {
		chat.error = null;
		const params: Record<string, unknown> = {
			root: chat.root,
			text,
			plan: chat.plan,
			evidence: chat.evidence,
			attachments: attachments.map(a =>
				a.kind === 'image' ? {mime: a.mime, data: a.data} : {name: a.name, text: a.text},
			),
		};
		if (chat.id) params.chat = chat.id;
		else if (chat.config) Object.assign(params, {provider: chat.config.provider, model: chat.config.model, effort: chat.config.effort});
		try {
			const result = await rpc.request('prompt.send', params);
			if (!chat.id) {
				// The draft became a chat: key it by its session file from now on.
				const oldKey = chat.key;
				chat.id = result.chat;
				chat.title = result.title || chat.title;
				chat.key = result.chat;
				delete this.chats[oldKey];
				this.chats[result.chat] = chat;
				const tab = this.tabs.find(t => t.key === `chat:${oldKey}`);
				if (tab && tab.kind === 'chat') {
					tab.key = `chat:${chat.key}`;
					tab.chat = chat.key;
					if (this.activeTab === `chat:${oldKey}`) this.activeTab = tab.key;
				}
				this.saveTabs();
				// Messages streamed before the id was known were dropped; read the chat.
				this.loadChat(chat);
			}
			return true;
		} catch (e) {
			chat.error = errorText(e);
			return false;
		}
	}

	async cancel(chat: Chat) {
		if (chat.id) await rpc.request('cancel', {chat: chat.id}).catch(() => {});
	}

	async respond(chat: Chat, decision: string, prefix?: string) {
		if (!chat.id) return;
		try {
			await rpc.request('approval.respond', {chat: chat.id, decision, prefix});
			chat.approval = null;
		} catch (e) {
			this.toast(errorText(e), 'error');
		}
	}

	async renameChat(root: string, id: string, title: string) {
		try {
			await rpc.request('chat.rename', {root, chat: id, title});
		} catch (e) {
			this.toast(errorText(e), 'error');
		}
	}

	async deleteChat(root: string, id: string) {
		try {
			await rpc.request('chat.delete', {root, chat: id});
			const chat = this.chats[id];
			if (chat) this.closeTab(`chat:${chat.key}`);
			delete this.chats[id];
		} catch (e) {
			this.toast(errorText(e), 'error');
		}
	}

	async refreshQuota(provider?: string | null) {
		if (!provider) return;
		try {
			const quota = await rpc.request<Quota>('quota.get', {provider});
			this.quota[provider] = quota;
		} catch {}
	}

	// ---------- tabs ----------

	openFile(root: string, path: string, line?: number, activate = true) {
		const key = `file:${path}`;
		const existing = this.tabs.find(t => t.key === key);
		if (existing && existing.kind === 'file') {
			if (line) existing.line = line;
		} else {
			this.tabs.push({key, kind: 'file', path, root, line});
		}
		if (activate) this.activeTab = key;
		this.saveTabs();
	}

	openDiff(root: string, title: string, text: string, path?: string, staged?: boolean) {
		const key = `diff:${path ?? title}:${staged ? 's' : 'u'}`;
		const existing = this.tabs.findIndex(t => t.key === key);
		const tab: Tab = {key, kind: 'diff', title, text, root, path, staged};
		if (existing >= 0) this.tabs[existing] = tab;
		else this.tabs.push(tab);
		this.activeTab = key;
	}

	closeTab(key: string) {
		const index = this.tabs.findIndex(t => t.key === key);
		if (index < 0) return;
		const tab = this.tabs[index];
		this.tabs.splice(index, 1);
		this.pinnedTabs.delete(key);
		delete this.dirty[key];
		if (tab.kind === 'chat') {
			const chat = this.chats[tab.chat];
			if (chat && !chat.id) delete this.chats[tab.chat];
		}
		if (this.activeTab === key) {
			const next = this.tabs[Math.min(index, this.tabs.length - 1)];
			this.activeTab = next?.key ?? '';
		}
		this.saveTabs();
	}

	activate(key: string) {
		this.activeTab = key;
		const tab = this.tab;
		if (tab?.kind === 'chat') {
			const chat = this.chats[tab.chat];
			if (chat) this.activeRoot = chat.root;
		} else if (tab && 'root' in tab) {
			this.activeRoot = tab.root;
		}
		this.saveTabs();
	}

	cycleTab(step: number) {
		if (!this.tabs.length) return;
		const index = this.tabs.findIndex(t => t.key === this.activeTab);
		const next = this.tabs[(index + step + this.tabs.length) % this.tabs.length];
		this.activate(next.key);
	}

	// ---------- toasts ----------

	private toastId = 0;
	toast(text: string, kind: Toast['kind'] = 'info') {
		const id = ++this.toastId;
		this.toasts.push({id, text, kind});
		setTimeout(() => {
			this.toasts = this.toasts.filter(t => t.id !== id);
		}, kind === 'error' ? 6000 : 3200);
	}
}

export const app = new AppState();

/** `/Users/me/Projects/oxi` → `Projects/oxi`, the way the app's sidebar names folders. */
export function folderLabel(path: string): string {
	const parts = path.split(/[\\/]/).filter(Boolean);
	return parts.slice(-2).join('/') || path;
}

export function baseName(path: string): string {
	return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

export function relativeTo(root: string, path: string): string {
	if (path.startsWith(root + '/')) return path.slice(root.length + 1);
	return path;
}
