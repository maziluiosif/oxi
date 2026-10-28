<script lang="ts">
	import {app, baseName, folderLabel} from '../lib/state.svelte';
	import {listDir, fsOp, invalidateFiles, type DirEntry} from '../lib/files';
	import {overlay} from '../lib/overlay.svelte';
	import {errorText} from '../lib/rpc';
	import Icon from './Icon.svelte';

	const root = $derived(app.activeRoot);
	let children = $state<Record<string, DirEntry[]>>({});
	let open = $state<Record<string, boolean>>({});
	let showHidden = $state(false);
	let selected = $state<string | null>(null);

	const HIDDEN = new Set(['.git', 'node_modules', 'target', '.DS_Store', 'dist', 'build', '.idea', '__pycache__']);

	async function load(dir: string) {
		try {
			const entries = await listDir(dir, true);
			children[dir] = entries;
		} catch (e) {
			children[dir] = [];
		}
	}

	$effect(() => {
		if (!root) return;
		children = {};
		open = {[root]: true};
		load(root);
	});

	async function toggle(path: string) {
		open[path] = !open[path];
		if (open[path] && !children[path]) await load(path);
	}

	async function refresh() {
		const dirs = Object.keys(open).filter(d => open[d]);
		await Promise.all(dirs.map(load));
		invalidateFiles(root);
	}

	// Pick up what the agent created or deleted.
	$effect(() => {
		if (!root) return;
		const timer = setInterval(() => app.windowFocused && refresh(), 5000);
		return () => clearInterval(timer);
	});

	function visible(entries: DirEntry[]) {
		return entries.filter(e => showHidden || !(e.name.startsWith('.') || HIDDEN.has(e.name)));
	}

	function parentOf(path: string) {
		return path.split('/').slice(0, -1).join('/');
	}

	async function create(dir: string, kind: 'file' | 'folder') {
		const name = await overlay.ask({title: kind === 'file' ? 'New file' : 'New folder', placeholder: kind === 'file' ? 'name.ext or path/to/file' : 'folder name', confirm: 'Create'});
		if (!name?.trim()) return;
		const path = `${dir}/${name.trim()}`;
		try {
			if (kind === 'folder') await fsOp('create_dir_all', path);
			else {
				const parent = parentOf(path);
				if (parent !== dir) await fsOp('create_dir_all', parent);
				await fsOp('create_file', path);
			}
			open[dir] = true;
			await refresh();
			if (kind === 'file') app.openFile(root, path);
		} catch (e) {
			app.toast(errorText(e), 'error');
		}
	}

	async function rename(path: string) {
		const name = await overlay.ask({title: 'Rename', value: baseName(path), confirm: 'Rename'});
		if (!name?.trim() || name === baseName(path)) return;
		try {
			await fsOp('rename', path, `${parentOf(path)}/${name.trim()}`);
			await refresh();
		} catch (e) {
			app.toast(errorText(e), 'error');
		}
	}

	async function remove(path: string, dir: boolean) {
		if (!(await overlay.confirm(`Delete ${baseName(path)}?`, dir ? 'The folder must be empty.' : 'The file will be deleted from disk.', 'Delete', true))) return;
		try {
			await fsOp(dir ? 'remove_dir' : 'remove_file', path);
			const tab = app.tabs.find(t => t.kind === 'file' && t.path === path);
			if (tab) app.closeTab(tab.key);
			await refresh();
		} catch (e) {
			app.toast(errorText(e), 'error');
		}
	}

	function menu(event: MouseEvent, path: string, dir: boolean) {
		selected = path;
		const folder = dir ? path : parentOf(path);
		overlay.openMenu(event, [
			{label: 'New File…', icon: 'filePlus', action: () => create(folder, 'file')},
			{label: 'New Folder…', icon: 'folderPlus', action: () => create(folder, 'folder')},
			{separator: true},
			...(!dir ? [{label: 'Mention in Chat', icon: 'at' as const, action: () => mention(path)}] : []),
			{label: 'Copy Path', icon: 'copy', action: () => navigator.clipboard.writeText(path)},
			{label: 'Copy Relative Path', action: () => navigator.clipboard.writeText(path.slice(root.length + 1))},
			{
				label: 'Reveal in Finder',
				icon: 'folderOpen',
				action: async () => (await import('@tauri-apps/plugin-opener')).revealItemInDir(path),
			},
			{separator: true},
			{label: 'Rename…', icon: 'edit', action: () => rename(path)},
			{label: 'Delete…', icon: 'trash', danger: true, action: () => remove(path, dir)},
		]);
	}

	function mention(path: string) {
		const chat = app.activeChat ?? Object.values(app.chats).find(c => c.root === root) ?? app.newChat(root);
		if (!chat) return;
		app.activate(`chat:${chat.key}`);
		setTimeout(() => window.dispatchEvent(new CustomEvent('oxi:insert', {detail: {chat: chat.key, text: `@${path.slice(root.length + 1)} `}})), 50);
	}

	const ICON_COLORS: Record<string, string> = {
		rs: '#dea584', ts: '#4f9cf0', tsx: '#4f9cf0', js: '#e8c372', jsx: '#e8c372', py: '#5a9fd4', go: '#6fd0d8', md: '#a9aeb9',
		json: '#e8c372', toml: '#c49cff', yaml: '#ff7a7a', yml: '#ff7a7a', svelte: '#ff7a5a', html: '#f28b5b', css: '#79a8ff',
		kt: '#a77bff', java: '#f2a970', swift: '#ff8a65', sh: '#74d39a', lock: '#737985',
	};
	const colorOf = (name: string) => ICON_COLORS[name.split('.').pop()?.toLowerCase() ?? ''] ?? 'var(--text-3)';
</script>

<div class="tree-head">
	<span class="root" title={root}>{root ? folderLabel(root) : 'No folder'}</span>
	<span class="grow"></span>
	{#if root}
		<button class="icon-btn sm" title="New file" onclick={() => create(root, 'file')}><Icon name="filePlus" size={14} /></button>
		<button class="icon-btn sm" title="New folder" onclick={() => create(root, 'folder')}><Icon name="folderPlus" size={14} /></button>
		<button class="icon-btn sm" class:active={showHidden} title="Show hidden and ignored files" onclick={() => (showHidden = !showHidden)}>
			<Icon name="eye" size={14} />
		</button>
		<button class="icon-btn sm" title="Refresh" onclick={refresh}><Icon name="refresh" size={14} /></button>
	{/if}
</div>

<div class="tree">
	{#if root}
		{@render dir(root, 0)}
	{:else}
		<div class="empty">Open a folder to browse its files.</div>
	{/if}
</div>

{#snippet dir(path: string, depth: number)}
	{#if children[path]}
		{#each visible(children[path]) as entry (entry.name)}
			{@const full = `${path}/${entry.name}`}
			{@const activeFile = app.tab?.kind === 'file' && app.tab.path === full}
			<button
				class="node"
				class:active={activeFile}
				class:selected={selected === full}
				style="padding-left:{10 + depth * 14}px"
				onclick={() => {
					selected = full;
					if (entry.dir) toggle(full);
					else app.openFile(root, full);
				}}
				oncontextmenu={e => menu(e, full, entry.dir)}
				title={full}
				draggable={!entry.dir}
				ondragstart={e => e.dataTransfer?.setData('text/plain', `@${full.slice(root.length + 1)} `)}
			>
				{#if entry.dir}
					<span class="chev" class:open={open[full]}><Icon name="chevronRight" size={12} stroke={2} /></span>
					<Icon name={open[full] ? 'folderOpen' : 'folder'} size={14} class="folder-ico" />
				{:else}
					<span class="chev"></span>
					<span class="file-ico" style="color:{colorOf(entry.name)}"><Icon name="file" size={14} /></span>
				{/if}
				<span class="name">{entry.name}</span>
			</button>
			{#if entry.dir && open[full]}
				{@render dir(full, depth + 1)}
			{/if}
		{/each}
	{:else}
		<div class="loading" style="padding-left:{24 + depth * 14}px"><span class="spinner"></span></div>
	{/if}
{/snippet}

<style>
	.tree-head {
		display: flex;
		align-items: center;
		gap: 2px;
		padding: 0 8px 4px 14px;
		height: 30px;
	}
	.root {
		font-size: var(--fs-xs);
		font-weight: 650;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--text-3);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.grow {
		flex: 1;
	}
	.tree {
		flex: 1;
		overflow: auto;
		padding: 0 6px 12px;
	}
	.node {
		display: flex;
		align-items: center;
		gap: 5px;
		width: 100%;
		height: 26px;
		padding-right: 8px;
		border-radius: 6px;
		font-size: var(--fs-sm);
		color: var(--text-2);
		text-align: left;
	}
	.node:hover {
		background: var(--bg-hover);
		color: var(--text);
	}
	.node.selected {
		background: var(--bg-active);
	}
	.node.active {
		background: var(--accent-soft);
		color: var(--text);
	}
	.chev {
		width: 12px;
		display: flex;
		color: var(--text-4);
		transition: transform 0.15s var(--ease);
		flex: none;
	}
	.chev.open {
		transform: rotate(90deg);
	}
	.node :global(.folder-ico) {
		color: var(--accent);
		opacity: 0.85;
	}
	.file-ico {
		display: flex;
		opacity: 0.9;
	}
	.name {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.loading {
		height: 24px;
		display: flex;
		align-items: center;
	}
</style>
