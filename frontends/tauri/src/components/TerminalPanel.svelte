<script lang="ts" module>
	import type {Terminal} from '@xterm/xterm';
	import type {FitAddon} from '@xterm/addon-fit';

	type Session = {id: number; title: string; cwd: string; term: Terminal; fit: FitAddon; el: HTMLDivElement; exited: boolean; unlisten: (() => void)[]};
	// Shells outlive the panel being hidden: they live here, not in the component.
	const sessions: Session[] = [];
</script>

<script lang="ts">
	import {onMount, tick} from 'svelte';
	import {invoke} from '@tauri-apps/api/core';
	import {listen} from '@tauri-apps/api/event';
	import '@xterm/xterm/css/xterm.css';
	import {app, baseName} from '../lib/state.svelte';
	import {formatKeys} from '../lib/commands';
	import Icon from './Icon.svelte';

	let {dark}: {dark: boolean} = $props();

	let host = $state<HTMLDivElement>();
	let list = $state<{id: number; title: string; exited: boolean}[]>([]);
	let current = $state<number | null>(null);

	const theme = (dark: boolean) =>
		dark
			? {
					background: '#1d1f24',
					foreground: '#e4e6eb',
					cursor: '#79a8ff',
					selectionBackground: '#79a8ff44',
					black: '#2a2d34', red: '#ff7a7a', green: '#74d39a', yellow: '#e8c372', blue: '#79a8ff', magenta: '#c49cff', cyan: '#6fd0d8', white: '#d5d8de',
					brightBlack: '#5c6370', brightRed: '#ff9b9b', brightGreen: '#93e3b2', brightYellow: '#f2d491', brightBlue: '#9cbfff', brightMagenta: '#d6b8ff', brightCyan: '#94e0e6', brightWhite: '#ffffff',
				}
			: {
					background: '#fafbfc',
					foreground: '#1d2129',
					cursor: '#2f6fed',
					selectionBackground: '#2f6fed33',
					black: '#1d2129', red: '#d64545', green: '#1f9d55', yellow: '#b7791f', blue: '#2f6fed', magenta: '#7c4dff', cyan: '#0f7c8a', white: '#868d9b',
					brightBlack: '#545b69', brightRed: '#e05d5d', brightGreen: '#27b765', brightYellow: '#c98a2a', brightBlue: '#4a84f0', brightMagenta: '#9166ff', brightCyan: '#1593a3', brightWhite: '#b0b6c1',
				};

	function sync() {
		list = sessions.map(s => ({id: s.id, title: s.title, exited: s.exited}));
	}

	async function create() {
		const [{Terminal}, {FitAddon}, {WebLinksAddon}] = await Promise.all([
			import('@xterm/xterm'),
			import('@xterm/addon-fit'),
			import('@xterm/addon-web-links'),
		]);
		const el = document.createElement('div');
		el.className = 'term-host';
		host!.appendChild(el);
		const term = new Terminal({
			fontFamily: "'SF Mono', 'JetBrains Mono', Menlo, monospace",
			fontSize: Math.round(app.fontSize * 0.9),
			lineHeight: 1.2,
			cursorBlink: true,
			cursorStyle: 'bar',
			allowProposedApi: true,
			scrollback: 10000,
			macOptionIsMeta: true,
			theme: theme(dark),
		});
		const fit = new FitAddon();
		term.loadAddon(fit);
		term.loadAddon(
			new WebLinksAddon(async (_e, uri) => {
				const {openUrl} = await import('@tauri-apps/plugin-opener');
				openUrl(uri);
			}),
		);
		term.open(el);
		fit.fit();
		const cwd = app.activeRoot;
		let id: number;
		try {
			id = await invoke<number>('pty_spawn', {cwd: cwd || null, cols: term.cols, rows: term.rows});
		} catch (e) {
			term.write(`\x1b[31m${e}\x1b[0m\r\n`);
			return;
		}
		const session: Session = {id, title: cwd ? baseName(cwd) : 'shell', cwd, term, fit, el, exited: false, unlisten: []};
		session.unlisten.push(
			await listen<string>(`pty://${id}`, event => {
				const bin = atob(event.payload);
				const bytes = new Uint8Array(bin.length);
				for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
				term.write(bytes);
			}),
			await listen(`pty-exit://${id}`, () => {
				session.exited = true;
				term.write('\r\n\x1b[2m[process exited]\x1b[0m\r\n');
				sync();
			}),
		);
		term.onData(data => invoke('pty_write', {id, data}).catch(() => {}));
		term.onResize(({cols, rows}) => invoke('pty_resize', {id, cols, rows}).catch(() => {}));
		term.onTitleChange(title => {
			session.title = title.replace(/^.*[\\/]/, '') || session.title;
			sync();
		});
		// App shortcuts pass through the terminal.
		term.attachCustomKeyEventHandler(event => {
			const mod = event.metaKey || (event.ctrlKey && !navigator.platform.includes('Mac'));
			if (mod && ['j', 'b', 'p', 'k', 'n', 'w', ',', 't'].includes(event.key.toLowerCase())) return false;
			if (event.metaKey && event.key === 'c' && term.hasSelection()) {
				navigator.clipboard.writeText(term.getSelection());
				return false;
			}
			return true;
		});
		sessions.push(session);
		show(id);
	}

	function show(id: number) {
		current = id;
		for (const s of sessions) s.el.style.display = s.id === id ? 'block' : 'none';
		sync();
		tick().then(() => {
			const s = sessions.find(x => x.id === id);
			if (!s) return;
			s.fit.fit();
			s.term.focus();
		});
	}

	function close(id: number) {
		const index = sessions.findIndex(s => s.id === id);
		if (index < 0) return;
		const [s] = sessions.splice(index, 1);
		invoke('pty_kill', {id}).catch(() => {});
		s.unlisten.forEach(u => u());
		s.term.dispose();
		s.el.remove();
		sync();
		if (current === id) {
			const next = sessions[Math.min(index, sessions.length - 1)];
			if (next) show(next.id);
			else {
				current = null;
				app.terminalOpen = false;
				app.persistLayout();
			}
		}
	}

	onMount(() => {
		// Re-attach the shells kept from the last time the panel was open.
		for (const s of sessions) host!.appendChild(s.el);
		if (sessions.length) show(sessions[sessions.length - 1].id);
		else create();
		const observer = new ResizeObserver(() => {
			const s = sessions.find(x => x.id === current);
			try {
				s?.fit.fit();
			} catch {}
		});
		observer.observe(host!);
		return () => observer.disconnect();
	});

	$effect(() => {
		const t = theme(dark);
		for (const s of sessions) s.term.options.theme = t;
	});
	$effect(() => {
		const size = Math.round(app.fontSize * 0.9);
		for (const s of sessions) {
			s.term.options.fontSize = size;
			s.fit.fit();
		}
	});
</script>

<div class="panel">
	<div class="bar">
		<div class="tabs">
			{#each list as s (s.id)}
				<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
				<div class="tab" class:active={s.id === current} class:exited={s.exited} onclick={() => show(s.id)}>
					<Icon name="terminal" size={12} />
					<span>{s.title}</span>
					<button class="x" onclick={e => (e.stopPropagation(), close(s.id))} title="Kill terminal"><Icon name="x" size={11} /></button>
				</div>
			{/each}
		</div>
		<button class="icon-btn sm" title="New terminal" onclick={create}><Icon name="plus" size={14} /></button>
		<button class="icon-btn sm" title="Hide ({formatKeys('mod+j')})" onclick={() => ((app.terminalOpen = false), app.persistLayout())}>
			<Icon name="chevronDown" size={14} />
		</button>
	</div>
	<div class="host" bind:this={host}></div>
</div>

<style>
	.panel {
		height: 100%;
		display: flex;
		flex-direction: column;
	}
	.bar {
		height: 34px;
		flex: none;
		display: flex;
		align-items: center;
		gap: 2px;
		padding: 0 8px;
	}
	.tabs {
		flex: 1;
		display: flex;
		gap: 2px;
		overflow-x: auto;
		min-width: 0;
	}
	.tab {
		display: flex;
		align-items: center;
		gap: 6px;
		height: 24px;
		padding: 0 4px 0 9px;
		border-radius: 6px;
		font-size: var(--fs-xs);
		color: var(--text-3);
		white-space: nowrap;
	}
	.tab:hover {
		background: var(--bg-hover);
	}
	.tab.active {
		background: var(--bg-active);
		color: var(--text);
	}
	.tab.exited span {
		text-decoration: line-through;
	}
	.x {
		display: flex;
		padding: 2px;
		border-radius: 4px;
		opacity: 0;
	}
	.tab:hover .x {
		opacity: 1;
	}
	.x:hover {
		background: var(--bg-active);
	}
	.host {
		flex: 1;
		min-height: 0;
		padding: 2px 4px 4px 12px;
		position: relative;
	}
	.host :global(.term-host) {
		height: 100%;
	}
	.host :global(.xterm-viewport) {
		background: transparent !important;
	}
</style>
