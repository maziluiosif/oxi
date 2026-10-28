<script lang="ts">
	import {app, folderLabel} from '../lib/state.svelte';
	import {rpc} from '../lib/rpc';
	import {overlay} from '../lib/overlay.svelte';
	import {formatKeys} from '../lib/commands';
	import {formatDuration} from '../lib/tools';
	import Icon from './Icon.svelte';

	const chat = $derived(app.activeChat);
	const running = $derived(Object.values(app.chatLists).flat().filter(c => c.running).length);
	const quota = $derived(chat?.config ? app.quota[chat.config.provider] : undefined);

	function connectionMenu(event: MouseEvent) {
		overlay.openMenu(event, [
			{header: app.connection === 'connected' ? `Connected · oxi ${app.version}` : app.connection === 'connecting' ? 'Connecting…' : 'Not connected'},
			...(rpc.binary ? [{header: rpc.binary}] : []),
			{separator: true},
			{label: 'Reconnect', icon: 'refresh', action: () => rpc.restart()},
			{label: 'Connection Settings…', icon: 'settings', action: () => ((app.settingsTab = 'connection'), (app.settingsOpen = true))},
		]);
	}

	const THEMES = [
		{id: 'system', label: 'System'},
		{id: 'dark', label: 'Dark'},
		{id: 'midnight', label: 'Midnight'},
		{id: 'light', label: 'Light'},
	] as const;

	function themeMenu(event: MouseEvent) {
		overlay.openMenu(
			event,
			THEMES.map(t => ({
				label: t.label,
				checked: app.theme === t.id,
				action: () => {
					app.theme = t.id;
					app.persistLayout();
				},
			})),
		);
	}
</script>

<footer class="status">
	<button class="item conn {app.connection}" onclick={connectionMenu} title={app.connection === 'down' ? app.connectionDetail : `oxi ${app.version}`}>
		<span class="led"></span>
		{#if app.connection === 'connected'}oxi {app.version}{:else if app.connection === 'connecting'}Connecting…{:else}Disconnected{/if}
	</button>
	{#if app.activeRoot}
		<button class="item" onclick={() => ((app.sidebarOpen = true), (app.sidebarView = 'files'))} title={app.activeRoot}>
			<Icon name="folder" size={12} />
			{folderLabel(app.activeRoot)}
		</button>
	{/if}
	{#if running}
		<span class="item"><span class="spinner"></span> {running} working</span>
	{/if}
	<span class="grow"></span>
	{#if quota?.windows?.length}
		<span class="item" title={quota.plan ?? ''}>
			{#each quota.windows as w, i}
				{#if i}<span class="sep">·</span>{/if}
				<span class:warn={w.used_pct >= 70} class:bad={w.used_pct >= 90}>
					{w.label} {Math.round(w.used_pct)}%{#if w.resets_in}<span class="dim"> ↻{formatDuration(w.resets_in)}</span>{/if}
				</span>
			{/each}
		</span>
	{/if}
	{#if chat?.context?.size}
		<span class="item" title="Context window used">
			{Math.round((chat.context.used / chat.context.size) * 100)}% context
		</span>
	{/if}
	<button class="item icon" class:on={app.sidebarOpen} onclick={() => ((app.sidebarOpen = !app.sidebarOpen), app.persistLayout())} title="Sidebar ({formatKeys('mod+b')})">
		<Icon name="sidebar" size={13} />
	</button>
	<button class="item icon" class:on={app.terminalOpen} onclick={() => ((app.terminalOpen = !app.terminalOpen), app.persistLayout())} title="Terminal ({formatKeys('mod+j')})">
		<Icon name="terminal" size={13} />
	</button>
	<button class="item icon" class:on={app.gitOpen} onclick={() => ((app.gitOpen = !app.gitOpen), app.persistLayout())} title="Source control ({formatKeys('mod+shift+g')})">
		<Icon name="git" size={13} />
	</button>
	<button class="item icon" onclick={themeMenu} title="Theme">
		<Icon name={app.theme === 'light' ? 'sun' : 'moon'} size={13} />
	</button>
	<button class="item icon" onclick={() => (app.settingsOpen = true)} title="Settings ({formatKeys('mod+,')})">
		<Icon name="settings" size={13} />
	</button>
</footer>

<style>
	.status {
		height: var(--statusbar);
		flex: none;
		display: flex;
		align-items: center;
		gap: 2px;
		padding: 0 8px;
		font-size: var(--fs-xs);
		color: var(--text-3);
		background: var(--sidebar-solid);
		border-top: 1px solid var(--border);
	}
	.item {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		height: 20px;
		padding: 0 7px;
		border-radius: 5px;
		white-space: nowrap;
		font-size: var(--fs-xs);
	}
	button.item:hover {
		background: var(--bg-hover);
		color: var(--text);
	}
	.item.icon {
		padding: 0 5px;
	}
	.item.icon.on {
		color: var(--accent);
	}
	.item .spinner {
		width: 9px;
		height: 9px;
	}
	.led {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--text-4);
	}
	.connected .led {
		background: var(--green);
		box-shadow: 0 0 6px color-mix(in srgb, var(--green) 60%, transparent);
	}
	.connecting .led {
		background: var(--yellow);
		animation: blink 1s infinite;
	}
	.down {
		color: var(--red);
	}
	.down .led {
		background: var(--red);
	}
	@keyframes blink {
		50% {
			opacity: 0.3;
		}
	}
	.grow {
		flex: 1;
	}
	.sep {
		color: var(--text-4);
	}
	.dim {
		color: var(--text-4);
	}
	.warn {
		color: var(--yellow);
	}
	.bad {
		color: var(--red);
	}
</style>
