<script lang="ts">
	import {tick} from 'svelte';
	import {fade, fly} from 'svelte/transition';
	import {app, baseName, folderLabel} from '../lib/state.svelte';
	import {commands, formatKeys, hooks} from '../lib/commands';
	import {fileList} from '../lib/files';
	import {fuzzy, relativeTime} from '../lib/tools';
	import type {IconName} from '../lib/icons';
	import Icon from './Icon.svelte';

	let {openFolder}: {openFolder: () => void} = $props();
	hooks.openFolder = () => openFolder();

	type Row = {key: string; label: string; detail?: string; icon: IconName; keys?: string; hits?: number[]; run: () => void};

	let query = $state('');
	let active = $state(0);
	let input = $state<HTMLInputElement>();
	let list = $state<HTMLDivElement>();
	let files = $state<string[]>([]);

	// `>` switches to commands, like VS Code; `#` to chats.
	const mode = $derived.by(() => {
		if (app.palette === 'files' && query.startsWith('>')) return 'commands';
		if (app.palette === 'files' && query.startsWith('#')) return 'chats';
		return app.palette;
	});
	const q = $derived(mode !== app.palette ? query.slice(1).trim() : query.trim());

	$effect(() => {
		if (!app.palette) return;
		query = '';
		active = 0;
		tick().then(() => input?.focus());
		if (app.activeRoot) fileList(app.activeRoot).then(f => (files = f));
	});

	const rows = $derived.by((): Row[] => {
		if (mode === 'commands') {
			const seen = new Set<string>();
			const all = commands.filter(c => c.category && !seen.has(c.title) && seen.add(c.title) && (!c.enabled || c.enabled()));
			return rank(
				all.map(c => ({key: c.id, label: c.title, detail: c.category, icon: c.icon ?? 'command', keys: c.keys, run: c.run})),
				r => `${r.detail}: ${r.label}`,
			);
		}
		if (mode === 'chats') {
			const chats = Object.entries(app.chatLists).flatMap(([root, list]) =>
				list.map(c => ({
					key: c.id,
					label: c.title,
					detail: `${folderLabel(root)} · ${relativeTime(c.modified)}`,
					icon: (c.running ? 'zap' : 'chat') as IconName,
					run: () => app.openChat(root, c.id),
				})),
			);
			return rank(chats, r => r.label, 200);
		}
		const root = app.activeRoot;
		const open = app.tabs.filter(t => t.kind === 'file').map(t => (t as any).path.slice(root.length + 1));
		const ordered = q ? files : [...open.filter(f => files.includes(f)), ...files.filter(f => !open.includes(f))];
		return rank(
			ordered.map(f => ({key: f, label: baseName(f), detail: f, icon: 'file' as IconName, run: () => app.openFile(root, `${root}/${f}`)})),
			r => r.detail!,
			80,
		);
	});

	function rank(items: Row[], text: (r: Row) => string, limit = 120): Row[] {
		if (!q) return items.slice(0, limit);
		return items
			.map(r => ({r, m: fuzzy(q, text(r))}))
			.filter(x => x.m)
			.sort((a, b) => b.m!.score - a.m!.score)
			.slice(0, limit)
			.map(x => x.r);
	}

	$effect(() => {
		q;
		mode;
		active = 0;
	});
	$effect(() => {
		list?.querySelector(`[data-i="${active}"]`)?.scrollIntoView({block: 'nearest'});
	});

	function choose(row: Row | undefined) {
		if (!row) return;
		app.palette = null;
		setTimeout(row.run, 0);
	}

	function key(event: KeyboardEvent) {
		if (event.key === 'ArrowDown') active = Math.min(rows.length - 1, active + 1);
		else if (event.key === 'ArrowUp') active = Math.max(0, active - 1);
		else if (event.key === 'Enter') choose(rows[active]);
		else if (event.key === 'Escape') app.palette = null;
		else return;
		event.preventDefault();
		event.stopPropagation();
	}

	const placeholder = $derived(
		mode === 'commands' ? 'Type a command' : mode === 'chats' ? 'Search chats in every folder' : 'Go to file  (> commands, # chats)',
	);
</script>

{#if app.palette}
	<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
	<div class="scrim" transition:fade={{duration: 100}} onclick={() => (app.palette = null)}>
		<div class="palette" onclick={e => e.stopPropagation()} transition:fly={{y: -8, duration: 140}}>
			<div class="input">
				<Icon name={mode === 'commands' ? 'command' : mode === 'chats' ? 'chats' : 'search'} size={16} />
				<input bind:this={input} bind:value={query} {placeholder} onkeydown={key} spellcheck="false" />
			</div>
			<div class="list" bind:this={list}>
				{#each rows as row, i (row.key)}
					<button class="row" class:active={i === active} data-i={i} onmousemove={() => (active = i)} onclick={() => choose(row)}>
						<Icon name={row.icon} size={15} />
						<span class="label">{row.label}</span>
						{#if row.detail && mode !== 'commands'}<span class="detail">{row.detail}</span>{/if}
						<span class="grow"></span>
						{#if row.detail && mode === 'commands'}<span class="cat">{row.detail}</span>{/if}
						{#if row.keys}<kbd>{formatKeys(row.keys)}</kbd>{/if}
					</button>
				{:else}
					<div class="none">{mode === 'files' && !app.activeRoot ? 'Open a folder first' : 'No matches'}</div>
				{/each}
			</div>
		</div>
	</div>
{/if}

<style>
	.scrim {
		position: fixed;
		inset: 0;
		z-index: 800;
		background: color-mix(in srgb, var(--scrim) 50%, transparent);
		display: flex;
		justify-content: center;
		align-items: flex-start;
		padding-top: 12vh;
	}
	.palette {
		width: 620px;
		max-width: calc(100vw - 40px);
		border-radius: 14px;
		background: color-mix(in srgb, var(--bg-raised) 92%, transparent);
		backdrop-filter: blur(30px) saturate(1.8);
		-webkit-backdrop-filter: blur(30px) saturate(1.8);
		border: 1px solid var(--border-strong);
		box-shadow: var(--shadow);
		overflow: hidden;
	}
	.input {
		display: flex;
		align-items: center;
		gap: 10px;
		height: 50px;
		padding: 0 16px;
		color: var(--text-3);
		border-bottom: 1px solid var(--border);
	}
	.input input {
		flex: 1;
		border: none;
		outline: none;
		background: none;
		font-size: calc(var(--fs) + 1px);
		color: var(--text);
	}
	.list {
		max-height: 420px;
		overflow-y: auto;
		padding: 6px;
	}
	.row {
		display: flex;
		align-items: center;
		gap: 10px;
		width: 100%;
		height: 36px;
		padding: 0 10px;
		border-radius: 8px;
		text-align: left;
		color: var(--text-2);
		font-size: var(--fs-sm);
	}
	.row.active {
		background: var(--accent);
		color: var(--accent-text);
	}
	.row.active .detail,
	.row.active .cat,
	.row.active kbd {
		color: inherit;
		opacity: 0.8;
	}
	.row.active kbd {
		background: rgba(0, 0, 0, 0.15);
		border-color: transparent;
	}
	.label {
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		flex: none;
		max-width: 60%;
	}
	.detail {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--text-3);
		font-size: var(--fs-xs);
	}
	.cat {
		color: var(--text-3);
		font-size: var(--fs-xs);
	}
	.grow {
		flex: 1;
	}
	.none {
		padding: 22px;
		text-align: center;
		color: var(--text-3);
		font-size: var(--fs-sm);
	}
</style>
