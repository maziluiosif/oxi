<script lang="ts">
	import {onMount} from 'svelte';
	import {getCurrentWindow} from '@tauri-apps/api/window';
	import {open as openDialog} from '@tauri-apps/plugin-dialog';
	import {app} from './lib/state.svelte';
	import {commands, runShortcut} from './lib/commands';
	import Sidebar from './components/Sidebar.svelte';
	import TabStrip from './components/TabStrip.svelte';
	import ChatView from './components/ChatView.svelte';
	import EditorView from './components/EditorView.svelte';
	import DiffTab from './components/DiffTab.svelte';
	import GitPanel from './components/GitPanel.svelte';
	import TerminalPanel from './components/TerminalPanel.svelte';
	import StatusBar from './components/StatusBar.svelte';
	import Palette from './components/Palette.svelte';
	import Settings from './components/Settings.svelte';
	import Overlays from './components/Overlays.svelte';
	import Welcome from './components/Welcome.svelte';
	import Splitter from './components/Splitter.svelte';
	import Icon from './components/Icon.svelte';

	const isMac = navigator.platform.toLowerCase().includes('mac');

	let zoom = $state<string | null>(null);
	$effect(() => {
		const onZoom = (e: Event) => (zoom = (e as CustomEvent).detail);
		window.addEventListener('oxi:zoom', onZoom);
		return () => window.removeEventListener('oxi:zoom', onZoom);
	});

	let systemDark = $state(window.matchMedia('(prefers-color-scheme: dark)').matches);
	const theme = $derived(app.theme === 'system' ? (systemDark ? 'dark' : 'light') : app.theme);

	$effect(() => {
		document.documentElement.dataset.theme = theme;
		document.documentElement.style.setProperty('--fs', `${app.fontSize}px`);
	});

	$effect(() => {
		const chat = app.activeChat;
		const title = chat ? chat.title : app.tab?.kind === 'file' ? app.tab.path.split('/').pop() : 'oxi';
		getCurrentWindow().setTitle(title ? `${title} — oxi` : 'oxi').catch(() => {});
	});

	// Dock badge: chats waiting for an approval.
	$effect(() => {
		const waiting = Object.values(app.chatLists)
			.flat()
			.filter(c => c.approval).length;
		getCurrentWindow()
			.setBadgeCount(waiting || undefined)
			.catch(() => {});
	});

	onMount(() => {
		const media = window.matchMedia('(prefers-color-scheme: dark)');
		const onScheme = () => (systemDark = media.matches);
		media.addEventListener('change', onScheme);
		const win = getCurrentWindow();
		let unlisten: (() => void) | undefined;
		win.onFocusChanged(({payload}) => (app.windowFocused = payload)).then(u => (unlisten = u));
		app.boot();
		return () => {
			media.removeEventListener('change', onScheme);
			unlisten?.();
		};
	});

	function onKeydown(event: KeyboardEvent) {
		if (runShortcut(event)) {
			event.preventDefault();
			event.stopPropagation();
		}
	}

	async function openFolder() {
		const folder = await openDialog({directory: true, multiple: false, title: 'Open a folder'});
		if (typeof folder === 'string') {
			await app.addWorkspace(folder);
			app.newChat(folder);
		}
	}

	// Links in answers open in the browser, never inside the app's webview.
	async function onClick(event: MouseEvent) {
		const target = event.target as HTMLElement;
		const link = target.closest('a[href]') as HTMLAnchorElement | null;
		if (link && !link.closest('.monaco-editor')) {
			event.preventDefault();
			const href = link.getAttribute('href') ?? '';
			if (/^https?:|^mailto:/.test(href)) {
				const {openUrl} = await import('@tauri-apps/plugin-opener');
				openUrl(href);
			}
		}
	}

	// The webview's own menu (Reload, Inspect) is no use here; text fields keep theirs.
	function onContextMenu(event: MouseEvent) {
		const target = event.target as HTMLElement;
		if (target.closest('input, textarea, .monaco-editor, .xterm, .selectable, .md')) return;
		event.preventDefault();
	}

	// Files dropped on the window open in the editor (the composer takes its own drops).
	function onDragOver(event: DragEvent) {
		event.preventDefault();
	}
</script>

<svelte:window onkeydown={onKeydown} onclick={onClick} oncontextmenu={onContextMenu} ondragover={onDragOver} ondrop={e => e.preventDefault()} />

<div class="app" class:mac={isMac} class:focus={app.focusMode}>
	<div class="body">
		{#if app.sidebarOpen && !app.focusMode}
			<aside class="sidebar" style="width:{app.sidebarWidth}px">
				<Sidebar {openFolder} />
			</aside>
			<Splitter
				direction="x"
				bind:size={app.sidebarWidth}
				min={210}
				max={460}
				onend={() => app.persistLayout()}
			/>
		{/if}
		<main class="main">
			<TabStrip mac={isMac} sidebarOpen={app.sidebarOpen && !app.focusMode} />
			<div class="work">
				<div class="center">
					<div class="content">
						{#each app.tabs as tab (tab.key)}
							<div class="pane" class:hidden={tab.key !== app.activeTab}>
								{#if tab.kind === 'chat'}
									{@const chat = app.chats[tab.chat]}
									{#if chat}<ChatView {chat} active={tab.key === app.activeTab} />{/if}
								{:else if tab.kind === 'file'}
									<EditorView {tab} active={tab.key === app.activeTab} dark={theme !== 'light'} />
								{:else if tab.kind === 'diff'}
									<DiffTab {tab} />
								{/if}
							</div>
						{/each}
						{#if !app.tabs.length}
							<Welcome {openFolder} />
						{/if}
					</div>
					{#if app.terminalOpen && !app.focusMode}
						<Splitter
							direction="y"
							bind:size={app.terminalHeight}
							min={120}
							max={900}
							invert
							onend={() => app.persistLayout()}
						/>
						<div class="terminal" style="height:{app.terminalHeight}px">
							<TerminalPanel dark={theme !== 'light'} />
						</div>
					{/if}
				</div>
				{#if app.gitOpen && !app.focusMode}
					<Splitter direction="x" bind:size={app.gitWidth} min={260} max={640} invert onend={() => app.persistLayout()} />
					<aside class="right" style="width:{app.gitWidth}px">
						<GitPanel />
					</aside>
				{/if}
			</div>
		</main>
	</div>
	{#if !app.focusMode}
		<StatusBar />
	{:else}
		<button class="leave-focus" onclick={() => (app.focusMode = false)} title="Leave distraction-free mode">
			<Icon name="focus" size={14} /> Exit focus
		</button>
	{/if}
</div>

{#if zoom}
	<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
	<div class="zoom" onclick={() => (zoom = null)}><img src={zoom} alt="" /></div>
{/if}

<Palette {openFolder} />
<Settings />
<Overlays />

<style>
	.app {
		display: flex;
		flex-direction: column;
		height: 100vh;
		background: var(--sidebar-solid);
	}
	.app.mac {
		background: transparent;
	}
	.body {
		flex: 1;
		display: flex;
		min-height: 0;
	}
	.sidebar {
		flex: none;
		display: flex;
		flex-direction: column;
		background: var(--sidebar);
		min-width: 0;
	}
	.main {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		background: var(--bg);
		border-left: 1px solid var(--border);
	}
	.focus .main {
		border-left: none;
	}
	.work {
		flex: 1;
		display: flex;
		min-height: 0;
	}
	.center {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.content {
		flex: 1;
		min-height: 0;
		position: relative;
	}
	.pane {
		position: absolute;
		inset: 0;
		display: flex;
		flex-direction: column;
	}
	.pane.hidden {
		visibility: hidden;
		pointer-events: none;
	}
	.terminal {
		flex: none;
		border-top: 1px solid var(--border);
		background: var(--panel);
		min-height: 0;
	}
	.right {
		flex: none;
		border-left: 1px solid var(--border);
		background: var(--panel);
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.zoom {
		position: fixed;
		inset: 0;
		z-index: 960;
		background: var(--scrim);
		backdrop-filter: blur(6px);
		-webkit-backdrop-filter: blur(6px);
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 40px;
		cursor: zoom-out;
	}
	.zoom img {
		max-width: 100%;
		max-height: 100%;
		border-radius: 10px;
		box-shadow: var(--shadow);
	}
	.leave-focus {
		position: fixed;
		right: 14px;
		bottom: 12px;
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 5px 10px;
		border-radius: 8px;
		font-size: var(--fs-xs);
		color: var(--text-3);
		background: var(--bg-raised);
		border: 1px solid var(--border);
		opacity: 0.6;
		transition: opacity 0.15s;
		z-index: 50;
	}
	.leave-focus:hover {
		opacity: 1;
	}
</style>
