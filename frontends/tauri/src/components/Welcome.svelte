<script lang="ts">
	import {fade} from 'svelte/transition';
	import {app, folderLabel} from '../lib/state.svelte';
	import {rpc} from '../lib/rpc';
	import {formatKeys} from '../lib/commands';
	import type {Workspace} from '../lib/types';
	import Icon from './Icon.svelte';

	let {openFolder}: {openFolder: () => void} = $props();
	let recent = $state<Workspace[]>([]);

	$effect(() => {
		if (app.connection === 'connected') {
			rpc.request<Workspace[]>('workspaces.list').then(list => (recent = list.slice(0, 8))).catch(() => {});
		}
	});

	async function openRecent(path: string) {
		await app.addWorkspace(path);
		app.newChat(path);
	}
</script>

<div class="welcome" in:fade={{duration: 200}}>
	<div class="inner">
		<div class="logo">oxi</div>
		<p class="tag">A coding agent that works in your folders.</p>
		{#if app.connection === 'down'}
			<div class="down">
				<Icon name="alert" size={16} />
				<div>
					<b>Can't reach oxi.</b>
					<p class="selectable">{app.connectionDetail}</p>
					<button class="btn sm" onclick={() => rpc.restart()}><Icon name="refresh" size={13} /> Try again</button>
					<button class="btn sm ghost" onclick={() => ((app.settingsTab = 'connection'), (app.settingsOpen = true))}>Connection settings</button>
				</div>
			</div>
		{/if}
		<div class="actions">
			<button class="action" onclick={() => app.newChat()} disabled={!app.activeRoot}>
				<Icon name="edit" size={18} />
				<span>New chat</span>
				<kbd>{formatKeys('mod+n')}</kbd>
			</button>
			<button class="action" onclick={openFolder}>
				<Icon name="folderPlus" size={18} />
				<span>Open folder</span>
				<kbd>{formatKeys('mod+o')}</kbd>
			</button>
			<button class="action" onclick={() => (app.palette = 'commands')}>
				<Icon name="command" size={18} />
				<span>Commands</span>
				<kbd>{formatKeys('mod+k')}</kbd>
			</button>
		</div>
		{#if recent.length}
			<h4>Recent folders</h4>
			<div class="recent">
				{#each recent as ws}
					<button class="ws" onclick={() => openRecent(ws.path)} title={ws.path}>
						<Icon name="folder" size={15} />
						<span class="name">{folderLabel(ws.path)}</span>
						{#if ws.chats}<span class="n">{ws.chats} chats</span>{/if}
					</button>
				{/each}
			</div>
		{/if}
	</div>
</div>

<style>
	.welcome {
		position: absolute;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		overflow-y: auto;
	}
	.inner {
		width: 440px;
		max-width: calc(100% - 40px);
		padding: 40px 0;
	}
	.logo {
		font-size: 56px;
		font-weight: 750;
		letter-spacing: -0.05em;
		background: linear-gradient(135deg, var(--accent), var(--purple));
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
		line-height: 1;
	}
	.tag {
		color: var(--text-3);
		margin: 10px 0 28px;
	}
	.actions {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.action {
		display: flex;
		align-items: center;
		gap: 12px;
		height: 44px;
		padding: 0 14px;
		border-radius: 10px;
		border: 1px solid var(--border);
		background: var(--card);
		font-size: var(--fs);
		transition: background 0.12s, border-color 0.12s, transform 0.12s;
	}
	.action:hover:not(:disabled) {
		border-color: var(--border-strong);
		background: var(--bg-hover);
	}
	.action:disabled {
		opacity: 0.45;
	}
	.action span {
		flex: 1;
		text-align: left;
	}
	.action :global(svg) {
		color: var(--accent);
	}
	h4 {
		margin: 28px 0 8px;
		font-size: var(--fs-xs);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-3);
		font-weight: 650;
	}
	.ws {
		display: flex;
		align-items: center;
		gap: 10px;
		width: 100%;
		height: 32px;
		padding: 0 10px;
		border-radius: 7px;
		color: var(--text-2);
		font-size: var(--fs-sm);
	}
	.ws:hover {
		background: var(--bg-hover);
		color: var(--text);
	}
	.name {
		flex: 1;
		text-align: left;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.n {
		font-size: var(--fs-xs);
		color: var(--text-4);
	}
	.down {
		display: flex;
		gap: 10px;
		padding: 12px 14px;
		margin-bottom: 18px;
		border-radius: 10px;
		background: var(--red-soft);
		color: var(--red);
		font-size: var(--fs-sm);
	}
	.down b {
		color: var(--text);
	}
	.down p {
		margin: 4px 0 10px;
		color: var(--text-2);
		font-size: var(--fs-xs);
		overflow-wrap: anywhere;
	}
</style>
