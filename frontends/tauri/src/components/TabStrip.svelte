<script lang="ts">
	import {app, baseName, type Tab} from '../lib/state.svelte';
	import {overlay} from '../lib/overlay.svelte';
	import {formatKeys} from '../lib/commands';
	import Icon from './Icon.svelte';

	let {mac, sidebarOpen}: {mac: boolean; sidebarOpen: boolean} = $props();

	function label(tab: Tab): string {
		if (tab.kind === 'chat') return app.chats[tab.chat]?.title || 'New chat';
		if (tab.kind === 'file') return baseName(tab.path);
		return tab.title;
	}

	function menu(event: MouseEvent, tab: Tab) {
		overlay.openMenu(event, [
			{label: 'Close', shortcut: formatKeys('mod+w'), action: () => app.closeTab(tab.key)},
			{label: 'Close Others', action: () => app.tabs.filter(t => t.key !== tab.key).forEach(t => app.closeTab(t.key))},
			{label: 'Close Tabs to the Right', action: () => app.tabs.slice(app.tabs.indexOf(tab) + 1).forEach(t => app.closeTab(t.key))},
			...(tab.kind === 'file'
				? [
						{separator: true} as const,
						{label: 'Copy Path', icon: 'copy' as const, action: () => navigator.clipboard.writeText(tab.path)},
						{
							label: 'Reveal in Finder',
							icon: 'folderOpen' as const,
							action: async () => (await import('@tauri-apps/plugin-opener')).revealItemInDir(tab.path),
						},
					]
				: []),
		]);
	}

	let dragKey = $state<string | null>(null);

	function drop(target: Tab) {
		if (!dragKey || dragKey === target.key) return;
		const from = app.tabs.findIndex(t => t.key === dragKey);
		const to = app.tabs.findIndex(t => t.key === target.key);
		const [moved] = app.tabs.splice(from, 1);
		app.tabs.splice(to, 0, moved);
		dragKey = null;
		app.saveTabs();
	}

	let strip = $state<HTMLDivElement>();
	$effect(() => {
		const key = app.activeTab;
		const el = strip?.querySelector(`[data-key="${CSS.escape(key)}"]`);
		el?.scrollIntoView({block: 'nearest', inline: 'nearest', behavior: 'smooth'});
	});
</script>

<header class="bar" class:pad={mac && !sidebarOpen} data-tauri-drag-region>
	{#if !sidebarOpen && !app.focusMode}
		<button class="icon-btn" title="Show sidebar ({formatKeys('mod+b')})" onclick={() => ((app.sidebarOpen = true), app.persistLayout())}>
			<Icon name="sidebar" />
		</button>
		<button class="icon-btn" title="New chat ({formatKeys('mod+n')})" onclick={() => app.newChat()}>
			<Icon name="edit" />
		</button>
	{/if}
	<div class="tabs" bind:this={strip} data-tauri-drag-region>
		{#each app.tabs as tab (tab.key)}
			{@const chat = tab.kind === 'chat' ? app.chats[tab.chat] : null}
			<!-- svelte-ignore a11y_no_static_element_interactions -->
			<div
				class="tab"
				class:active={tab.key === app.activeTab}
				data-key={tab.key}
				draggable="true"
				ondragstart={() => (dragKey = tab.key)}
				ondragover={e => e.preventDefault()}
				ondrop={() => drop(tab)}
				onmousedown={e => {
					if (e.button === 1) {
						e.preventDefault();
						app.closeTab(tab.key);
					} else if (e.button === 0) app.activate(tab.key);
				}}
				oncontextmenu={e => menu(e, tab)}
				title={tab.kind === 'file' ? tab.path : label(tab)}
			>
				<span class="kind">
					{#if chat?.approval}
						<span class="dot warn"></span>
					{:else if chat?.running}
						<span class="spinner"></span>
					{:else}
						<Icon name={tab.kind === 'chat' ? 'chat' : tab.kind === 'diff' ? 'git' : 'file'} size={13} />
					{/if}
				</span>
				<span class="name">{label(tab)}</span>
				<button
					class="close"
					class:dirty={app.dirty[tab.key]}
					onmousedown={e => e.stopPropagation()}
					onclick={() => app.closeTab(tab.key)}
					title="Close"
				>
					<span class="dirty-dot"></span>
					<Icon name="x" size={12} stroke={2} />
				</button>
			</div>
		{/each}
		<div class="fill" data-tauri-drag-region></div>
	</div>
	{#if !app.focusMode}
		<div class="tools">
			<button
				class="icon-btn"
				class:active={app.terminalOpen}
				title="Terminal ({formatKeys('mod+j')})"
				onclick={() => ((app.terminalOpen = !app.terminalOpen), app.persistLayout())}
			>
				<Icon name="panelBottom" />
			</button>
			<button
				class="icon-btn"
				class:active={app.gitOpen}
				title="Source control ({formatKeys('mod+shift+g')})"
				onclick={() => ((app.gitOpen = !app.gitOpen), app.persistLayout())}
			>
				<Icon name="panelRight" />
			</button>
		</div>
	{/if}
</header>

<style>
	.bar {
		height: var(--titlebar);
		flex: none;
		display: flex;
		align-items: center;
		gap: 2px;
		padding: 0 8px;
		border-bottom: 1px solid var(--border);
		background: var(--bg);
	}
	.bar.pad {
		padding-left: 82px;
	}
	.tabs {
		flex: 1;
		min-width: 0;
		display: flex;
		align-items: center;
		gap: 3px;
		overflow-x: auto;
		height: 100%;
		padding: 0 4px;
		scrollbar-width: none;
	}
	.tabs::-webkit-scrollbar {
		display: none;
	}
	.fill {
		flex: 1;
		align-self: stretch;
		min-width: 20px;
	}
	.tab {
		flex: none;
		display: flex;
		align-items: center;
		gap: 7px;
		height: 30px;
		max-width: 220px;
		padding: 0 4px 0 10px;
		border-radius: 8px;
		font-size: var(--fs-sm);
		color: var(--text-3);
		transition: background 0.12s, color 0.12s;
		position: relative;
	}
	.tab:hover {
		background: var(--bg-hover);
		color: var(--text-2);
	}
	.tab.active {
		background: var(--bg-active);
		color: var(--text);
	}
	.kind {
		display: flex;
		width: 13px;
		justify-content: center;
		opacity: 0.85;
	}
	.dot.warn {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--yellow);
	}
	.name {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.close {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 20px;
		height: 20px;
		border-radius: 5px;
		color: var(--text-3);
		opacity: 0;
		position: relative;
	}
	.tab:hover .close,
	.tab.active .close {
		opacity: 1;
	}
	.close:hover {
		background: var(--bg-active);
		color: var(--text);
	}
	.dirty-dot {
		display: none;
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--text-2);
	}
	.close.dirty {
		opacity: 1;
	}
	.close.dirty .dirty-dot {
		display: block;
	}
	.close.dirty :global(svg) {
		display: none;
	}
	.close.dirty:hover .dirty-dot {
		display: none;
	}
	.close.dirty:hover :global(svg) {
		display: block;
	}
	.tools {
		display: flex;
		gap: 2px;
		padding-left: 4px;
	}
</style>
