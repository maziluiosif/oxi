<script lang="ts">
	import {app, folderLabel, type SidebarView} from '../lib/state.svelte';
	import {overlay} from '../lib/overlay.svelte';
	import {formatKeys} from '../lib/commands';
	import {dateGroup, relativeTime} from '../lib/tools';
	import type {ChatSummary} from '../lib/types';
	import Icon from './Icon.svelte';
	import FileTree from './FileTree.svelte';
	import SearchPanel from './SearchPanel.svelte';

	let {openFolder}: {openFolder: () => void} = $props();

	const isMac = navigator.platform.toLowerCase().includes('mac');
	let filter = $state('');

	const activeChatId = $derived(app.activeChat?.id ?? null);

	function groups(list: ChatSummary[], root: string) {
		const q = filter.trim().toLowerCase();
		const items = q ? list.filter(c => c.title.toLowerCase().includes(q)) : list;
		const pinned = pinnedOf(root);
		const out: {label: string; chats: ChatSummary[]}[] = [];
		for (const chat of items) {
			const label = pinned.includes(chat.id) ? 'Pinned' : dateGroup(chat.modified);
			let group = out.find(g => g.label === label);
			if (!group) out.push((group = {label, chats: []}));
			group.chats.push(chat);
		}
		out.sort((a, b) => (a.label === 'Pinned' ? -1 : b.label === 'Pinned' ? 1 : 0));
		return out;
	}

	function pinnedOf(root: string): string[] {
		const entry = (app.settings?.settings?.workspaces ?? []).find((w: any) => w.root_path === root);
		return entry?.pinned ?? [];
	}

	async function togglePin(root: string, id: string) {
		const entries = structuredClone(app.settings?.settings?.workspaces ?? []);
		const entry = entries.find((w: any) => w.root_path === root);
		if (!entry) return;
		const pinned: string[] = entry.pinned ?? [];
		entry.pinned = pinned.includes(id) ? pinned.filter(p => p !== id) : [...pinned, id];
		await app.patchSettings({workspaces: entries}).catch(() => {});
	}

	function chatMenu(event: MouseEvent, root: string, chat: ChatSummary) {
		const pinned = pinnedOf(root).includes(chat.id);
		overlay.openMenu(event, [
			{label: 'Open in New Tab', icon: 'plus', action: () => app.openChatInNewTab(root, chat.id)},
			{label: pinned ? 'Unpin' : 'Pin to Top', icon: 'pin', action: () => togglePin(root, chat.id)},
			{label: 'Rename…', icon: 'edit', action: () => rename(root, chat)},
			{separator: true},
			{label: 'Copy Chat File Path', icon: 'copy', action: () => navigator.clipboard.writeText(chat.id)},
			{label: 'Reveal Chat File', icon: 'folderOpen', action: () => reveal(chat.id)},
			{separator: true},
			{label: 'Delete…', icon: 'trash', danger: true, action: () => remove(root, chat)},
		]);
	}

	async function reveal(path: string) {
		const {revealItemInDir} = await import('@tauri-apps/plugin-opener');
		revealItemInDir(path).catch(() => {});
	}

	async function rename(root: string, chat: ChatSummary) {
		const title = await overlay.ask({title: 'Rename chat', value: chat.title, confirm: 'Rename'});
		if (title?.trim()) app.renameChat(root, chat.id, title.trim());
	}

	async function remove(root: string, chat: ChatSummary) {
		if (await overlay.confirm('Delete this chat?', `“${chat.title}” will be deleted from disk.`, 'Delete', true)) {
			app.deleteChat(root, chat.id);
		}
	}

	function workspaceMenu(event: MouseEvent, root: string) {
		overlay.openMenu(event, [
			{label: 'New Chat Here', icon: 'plus', action: () => app.newChat(root)},
			{label: 'Show Files', icon: 'folder', action: () => ((app.activeRoot = root), (app.sidebarView = 'files'))},
			{label: 'Reveal in Finder', icon: 'folderOpen', action: () => reveal(root)},
			{label: 'Copy Path', icon: 'copy', action: () => navigator.clipboard.writeText(root)},
			{separator: true},
			{label: 'Remove from Sidebar', icon: 'x', danger: true, action: () => app.removeWorkspace(root)},
		]);
	}

	function toggleFold(root: string) {
		app.folded[root] = !app.folded[root];
		app.persistLayout();
	}

	const views: {id: SidebarView; icon: 'chats' | 'folder' | 'search'; label: string}[] = [
		{id: 'chats', icon: 'chats', label: 'Chats'},
		{id: 'files', icon: 'folder', label: 'Files'},
		{id: 'search', icon: 'search', label: 'Search'},
	];
</script>

<div class="top" data-tauri-drag-region class:mac={isMac}>
	<div class="spacer" data-tauri-drag-region></div>
	<button class="icon-btn" title="Hide sidebar ({formatKeys('mod+b')})" onclick={() => ((app.sidebarOpen = false), app.persistLayout())}>
		<Icon name="sidebar" />
	</button>
	<button class="icon-btn" title="New chat ({formatKeys('mod+n')})" onclick={() => app.newChat()}>
		<Icon name="edit" />
	</button>
</div>

<div class="switch">
	{#each views as view}
		<button class:on={app.sidebarView === view.id} onclick={() => (app.sidebarView = view.id)}>
			<Icon name={view.icon} size={14} />
			<span>{view.label}</span>
		</button>
	{/each}
</div>

{#if app.sidebarView === 'chats'}
	<div class="filter">
		<Icon name="search" size={14} />
		<input placeholder="Search chats" bind:value={filter} spellcheck="false" />
		{#if filter}
			<button class="icon-btn sm" onclick={() => (filter = '')}><Icon name="x" size={12} /></button>
		{/if}
	</div>
	<div class="list">
		{#each app.workspaces as root (root)}
			{@const list = app.chatLists[root] ?? []}
			{@const folded = app.folded[root] && !filter}
			<section class:active-root={root === app.activeRoot}>
				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div
					class="ws"
					role="button"
					tabindex="0"
					onclick={() => toggleFold(root)}
					onkeydown={e => e.key === 'Enter' && toggleFold(root)}
					oncontextmenu={e => workspaceMenu(e, root)}
					title={root}
				>
					<span class="chev" class:folded><Icon name="chevronDown" size={12} stroke={2.2} /></span>
					<Icon name={folded ? 'folder' : 'folderOpen'} size={15} />
					<span class="ws-name">{folderLabel(root)}</span>
					<span class="ws-actions">
						<button class="icon-btn sm" title="New chat in this folder" onclick={e => (e.stopPropagation(), app.newChat(root))}>
							<Icon name="plus" size={14} />
						</button>
						<button class="icon-btn sm" title="More" onclick={e => (e.stopPropagation(), workspaceMenu(e, root))}>
							<Icon name="more" size={14} />
						</button>
					</span>
				</div>
				{#if !folded}
					{#each groups(list, root) as group (group.label)}
						<div class="group">{group.label}</div>
						{#each group.chats as chat (chat.id)}
							<button
								class="chat"
								class:active={chat.id === activeChatId}
								onclick={e => (e.metaKey || e.ctrlKey ? app.openChatInNewTab(root, chat.id) : app.openChat(root, chat.id))}
								oncontextmenu={e => chatMenu(e, root, chat)}
								ondblclick={() => rename(root, chat)}
								title={chat.title}
							>
								<span class="chat-title">{chat.title || 'Untitled'}</span>
								{#if chat.approval}
									<span class="pill warn" title="Waiting for your approval">!</span>
								{:else if chat.running}
									<span class="spinner"></span>
								{:else}
									<span class="time">{relativeTime(chat.modified)}</span>
								{/if}
							</button>
						{/each}
					{:else}
						{#if !filter}
							<button class="chat empty-row" onclick={() => app.newChat(root)}>
								<span class="chat-title">No chats yet — start one</span>
							</button>
						{/if}
					{/each}
				{/if}
			</section>
		{:else}
			<div class="empty">
				<p>No folders yet.</p>
				<button class="btn" onclick={openFolder}><Icon name="folderPlus" size={14} /> Open Folder</button>
			</div>
		{/each}
	</div>
	<div class="bottom">
		<button class="row-btn" onclick={openFolder} title="Open folder ({formatKeys('mod+o')})">
			<Icon name="folderPlus" size={15} /> Open Folder
		</button>
	</div>
{:else if app.sidebarView === 'files'}
	<FileTree />
{:else}
	<SearchPanel />
{/if}

<style>
	.top {
		height: var(--titlebar);
		flex: none;
		display: flex;
		align-items: center;
		gap: 2px;
		padding: 0 10px;
	}
	.top.mac {
		padding-left: 84px;
	}
	.spacer {
		flex: 1;
		align-self: stretch;
	}
	.switch {
		display: flex;
		margin: 0 10px 8px;
		padding: 2px;
		border-radius: 8px;
		background: var(--bg-hover);
		gap: 2px;
	}
	.switch button {
		flex: 1;
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 5px;
		height: 26px;
		border-radius: 6px;
		font-size: var(--fs-xs);
		font-weight: 500;
		color: var(--text-3);
		transition: background 0.15s, color 0.15s, box-shadow 0.15s;
	}
	.switch button:hover {
		color: var(--text);
	}
	.switch button.on {
		background: var(--bg-raised);
		color: var(--text);
		box-shadow: var(--shadow-sm);
	}
	.filter {
		display: flex;
		align-items: center;
		gap: 7px;
		margin: 0 10px 6px;
		padding: 0 6px 0 9px;
		height: 30px;
		border-radius: 7px;
		background: var(--bg-hover);
		color: var(--text-3);
		border: 1px solid transparent;
		transition: border-color 0.15s, background 0.15s;
	}
	.filter:focus-within {
		border-color: var(--accent);
		background: var(--input);
	}
	.filter input {
		flex: 1;
		min-width: 0;
		border: none;
		outline: none;
		background: none;
		font-size: var(--fs-sm);
		color: var(--text);
	}
	.list {
		flex: 1;
		overflow-y: auto;
		padding: 2px 6px 10px;
	}
	section {
		margin-bottom: 4px;
	}
	.ws {
		display: flex;
		align-items: center;
		gap: 6px;
		height: 30px;
		padding: 0 4px 0 2px;
		border-radius: 7px;
		color: var(--text-2);
		font-weight: 550;
		font-size: var(--fs-sm);
		outline: none;
	}
	.ws:hover {
		background: var(--bg-hover);
	}
	.active-root .ws {
		color: var(--text);
	}
	.chev {
		display: flex;
		color: var(--text-4);
		transition: transform 0.15s var(--ease);
	}
	.chev.folded {
		transform: rotate(-90deg);
	}
	.ws-name {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.ws-actions {
		display: flex;
		opacity: 0;
		transition: opacity 0.12s;
	}
	.ws:hover .ws-actions {
		opacity: 1;
	}
	.group {
		padding: 8px 10px 3px 28px;
		font-size: var(--fs-xs);
		color: var(--text-4);
		font-weight: 550;
	}
	.chat {
		display: flex;
		align-items: center;
		gap: 8px;
		width: 100%;
		height: 30px;
		padding: 0 8px 0 28px;
		border-radius: 7px;
		text-align: left;
		color: var(--text-2);
		font-size: var(--fs-sm);
		transition: background 0.1s;
	}
	.chat:hover {
		background: var(--bg-hover);
		color: var(--text);
	}
	.chat.active {
		background: var(--accent-soft);
		color: var(--text);
	}
	.chat-title {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.empty-row .chat-title {
		color: var(--text-4);
		font-style: italic;
	}
	.time {
		font-size: var(--fs-xs);
		color: var(--text-4);
		font-variant-numeric: tabular-nums;
	}
	.pill.warn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 16px;
		height: 16px;
		border-radius: 50%;
		background: var(--yellow);
		color: #1a1300;
		font-size: 11px;
		font-weight: 800;
		animation: pulse 1.6s ease-in-out infinite;
	}
	@keyframes pulse {
		50% {
			opacity: 0.55;
		}
	}
	.empty p {
		margin: 0 0 12px;
	}
	.bottom {
		flex: none;
		padding: 6px 8px 8px;
		border-top: 1px solid var(--border);
	}
	.row-btn {
		display: flex;
		align-items: center;
		gap: 8px;
		width: 100%;
		height: 30px;
		padding: 0 10px;
		border-radius: 7px;
		font-size: var(--fs-sm);
		color: var(--text-2);
	}
	.row-btn:hover {
		background: var(--bg-hover);
		color: var(--text);
	}
</style>
