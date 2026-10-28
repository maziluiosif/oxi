// Every action with a name: the command palette lists them, keyboard shortcuts run them.

import {app} from './state.svelte';
import type {IconName} from './icons';

export type Command = {
	id: string;
	title: string;
	category?: string;
	icon?: IconName;
	/** `mod` = ⌘ on macOS, Ctrl elsewhere. */
	keys?: string;
	run: () => void;
	enabled?: () => boolean;
};

const isMac = navigator.platform.toLowerCase().includes('mac');

/** Hooks components register (focus the composer, open a folder, …). */
export const hooks: {
	focusComposer?: () => void;
	openFolder?: () => void;
	saveFile?: () => void;
	findInChat?: () => void;
} = {};

function toggle(key: 'sidebarOpen' | 'gitOpen' | 'terminalOpen') {
	app[key] = !app[key];
	app.persistLayout();
}

function showSidebar(view: 'chats' | 'files' | 'search') {
	if (app.sidebarOpen && app.sidebarView === view) {
		app.sidebarOpen = false;
	} else {
		app.sidebarOpen = true;
		app.sidebarView = view;
	}
	app.persistLayout();
}

export const commands: Command[] = [
	{id: 'chat.new', title: 'New Chat', category: 'Chat', icon: 'plus', keys: 'mod+n', run: () => app.newChat()},
	{id: 'folder.open', title: 'Open Folder…', category: 'Workspace', icon: 'folderPlus', keys: 'mod+o', run: () => hooks.openFolder?.()},
	{id: 'palette.files', title: 'Go to File…', category: 'Go', icon: 'file', keys: 'mod+p', run: () => (app.palette = 'files')},
	{id: 'palette.commands', title: 'Show All Commands', category: 'Go', icon: 'command', keys: 'mod+shift+p', run: () => (app.palette = 'commands')},
	{id: 'palette.commands2', title: 'Show All Commands', keys: 'mod+k', run: () => (app.palette = 'commands')},
	{id: 'palette.chats', title: 'Switch Chat…', category: 'Go', icon: 'chats', keys: 'mod+shift+o', run: () => (app.palette = 'chats')},
	{id: 'settings.open', title: 'Settings', category: 'App', icon: 'settings', keys: 'mod+,', run: () => (app.settingsOpen = true)},
	{id: 'view.sidebar', title: 'Toggle Sidebar', category: 'View', icon: 'sidebar', keys: 'mod+b', run: () => toggle('sidebarOpen')},
	{id: 'view.chats', title: 'Show Chats', category: 'View', icon: 'chats', keys: 'mod+shift+c', run: () => showSidebar('chats')},
	{id: 'view.files', title: 'Show Files', category: 'View', icon: 'folder', keys: 'mod+shift+e', run: () => showSidebar('files')},
	{id: 'view.search', title: 'Search in Files', category: 'View', icon: 'search', keys: 'mod+shift+f', run: () => showSidebar('search')},
	{id: 'view.git', title: 'Toggle Source Control', category: 'View', icon: 'git', keys: 'mod+shift+g', run: () => toggle('gitOpen')},
	{id: 'view.terminal', title: 'Toggle Terminal', category: 'View', icon: 'terminal', keys: 'mod+j', run: () => toggle('terminalOpen')},
	{id: 'view.terminal2', title: 'Toggle Terminal', keys: 'ctrl+`', run: () => toggle('terminalOpen')},
	{id: 'view.focus', title: 'Toggle Distraction-Free Mode', category: 'View', icon: 'focus', keys: 'mod+shift+d', run: () => (app.focusMode = !app.focusMode)},
	{
		id: 'view.zoomIn',
		title: 'Bigger Text',
		category: 'View',
		keys: 'mod+=',
		run: () => {
			app.fontSize = Math.min(20, app.fontSize + 1);
			app.persistLayout();
		},
	},
	{
		id: 'view.zoomOut',
		title: 'Smaller Text',
		category: 'View',
		keys: 'mod+-',
		run: () => {
			app.fontSize = Math.max(11, app.fontSize - 1);
			app.persistLayout();
		},
	},
	{
		id: 'view.zoomReset',
		title: 'Reset Text Size',
		category: 'View',
		keys: 'mod+0',
		run: () => {
			app.fontSize = 14;
			app.persistLayout();
		},
	},
	{id: 'tab.close', title: 'Close Tab', category: 'Tabs', icon: 'x', keys: 'mod+w', run: () => app.activeTab && app.closeTab(app.activeTab)},
	{id: 'tab.next', title: 'Next Tab', category: 'Tabs', keys: 'ctrl+tab', run: () => app.cycleTab(1)},
	{id: 'tab.prev', title: 'Previous Tab', category: 'Tabs', keys: 'ctrl+shift+tab', run: () => app.cycleTab(-1)},
	{id: 'tab.next2', title: 'Next Tab', keys: 'mod+shift+]', run: () => app.cycleTab(1)},
	{id: 'tab.prev2', title: 'Previous Tab', keys: 'mod+shift+[', run: () => app.cycleTab(-1)},
	{id: 'chat.focus', title: 'Focus Message Box', category: 'Chat', icon: 'message', keys: 'mod+l', run: () => hooks.focusComposer?.()},
	{
		id: 'chat.stop',
		title: 'Stop Response',
		category: 'Chat',
		icon: 'stop',
		keys: 'mod+.',
		run: () => {
			const chat = app.activeChat;
			if (chat?.running) app.cancel(chat);
		},
	},
	{
		id: 'chat.plan',
		title: 'Toggle Plan Mode',
		category: 'Chat',
		icon: 'map',
		keys: 'shift+tab',
		run: () => {
			const chat = app.activeChat;
			if (chat) chat.plan = !chat.plan;
		},
	},
	{
		id: 'chat.evidence',
		title: 'Toggle Evidence Mode',
		category: 'Chat',
		icon: 'shield',
		run: () => {
			const chat = app.activeChat;
			if (chat) chat.evidence = !chat.evidence;
		},
	},
	{id: 'chat.find', title: 'Find in Chat', category: 'Chat', icon: 'search', keys: 'mod+f', run: () => hooks.findInChat?.(), enabled: () => !!app.activeChat},
	{id: 'file.save', title: 'Save File', category: 'File', icon: 'save', keys: 'mod+s', run: () => hooks.saveFile?.(), enabled: () => app.tab?.kind === 'file'},
	{
		id: 'theme.cycle',
		title: 'Cycle Theme (System, Dark, Midnight, Light)',
		category: 'View',
		icon: 'palette',
		run: () => {
			const order = ['system', 'dark', 'midnight', 'light'] as const;
			app.theme = order[(order.indexOf(app.theme) + 1) % order.length];
			app.persistLayout();
		},
	},
	{id: 'oxi.reconnect', title: 'Reconnect to oxi', category: 'App', icon: 'refresh', run: () => import('./rpc').then(({rpc}) => rpc.restart())},
];

for (let i = 1; i <= 9; i++) {
	commands.push({
		id: `tab.${i}`,
		title: `Go to Tab ${i}`,
		keys: `mod+${i}`,
		run: () => {
			const tab = i === 9 ? app.tabs[app.tabs.length - 1] : app.tabs[i - 1];
			if (tab) app.activate(tab.key);
		},
	});
}

/** `mod+shift+p` → `⇧⌘P` on macOS, `Ctrl+Shift+P` elsewhere. */
export function formatKeys(keys: string): string {
	const parts = keys.split('+');
	if (isMac) {
		const map: Record<string, string> = {mod: '⌘', shift: '⇧', alt: '⌥', ctrl: '⌃', tab: '⇥', enter: '↩'};
		const order = ['ctrl', 'alt', 'shift', 'mod'];
		const mods = parts.slice(0, -1).sort((a, b) => order.indexOf(a) - order.indexOf(b));
		const key = parts[parts.length - 1];
		return mods.map(m => map[m] ?? m).join('') + (map[key] ?? key.toUpperCase());
	}
	return parts.map(p => (p === 'mod' ? 'Ctrl' : p.charAt(0).toUpperCase() + p.slice(1))).join('+');
}

function matches(event: KeyboardEvent, keys: string): boolean {
	const parts = keys.split('+');
	const key = parts[parts.length - 1];
	const want = new Set(parts.slice(0, -1));
	const mod = isMac ? event.metaKey : event.ctrlKey;
	if (want.has('mod') !== mod) return false;
	if (want.has('shift') !== event.shiftKey) return false;
	if (want.has('alt') !== event.altKey) return false;
	const ctrl = want.has('ctrl') || (!isMac && want.has('mod'));
	if (ctrl !== event.ctrlKey) return false;
	if (isMac && !want.has('mod') && event.metaKey) return false;
	const pressed = event.key.length === 1 ? event.key.toLowerCase() : event.key.toLowerCase();
	if (key === 'tab') return pressed === 'tab';
	// Shift changes the character ("}" for "]"); compare by physical key for those.
	if (key === ']' || key === '[') return event.code === (key === ']' ? 'BracketRight' : 'BracketLeft');
	if (key === '=' && (pressed === '+' || pressed === '=')) return true;
	if (key === '`') return event.code === 'Backquote';
	if (/^[0-9]$/.test(key)) return event.code === `Digit${key}`;
	return pressed === key;
}

/** Runs the command a key combination is bound to; false when none is. */
export function runShortcut(event: KeyboardEvent): boolean {
	const target = event.target as HTMLElement | null;
	const inEditor = !!target?.closest('.monaco-editor');
	const inTerminal = !!target?.closest('.xterm');
	for (const command of commands) {
		if (!command.keys || !matches(event, command.keys)) continue;
		// The editor and terminal keep the keys they use themselves.
		if (inEditor && ['chat.find', 'chat.plan', 'palette.commands2'].includes(command.id)) continue;
		if (inTerminal && ['chat.plan', 'chat.find', 'view.zoomIn', 'view.zoomOut'].includes(command.id)) continue;
		if (command.id === 'chat.plan' && !target?.closest('.composer')) continue;
		if (command.enabled && !command.enabled()) continue;
		command.run();
		return true;
	}
	return false;
}
