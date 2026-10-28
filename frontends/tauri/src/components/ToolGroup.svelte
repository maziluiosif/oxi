<script lang="ts">
	import {slide} from 'svelte/transition';
	import {familyOf} from '../lib/tools';
	import type {ToolBlock} from '../lib/types';
	import Icon from './Icon.svelte';
	import ToolCard from './ToolCard.svelte';

	let {blocks, root}: {blocks: ToolBlock[]; root: string} = $props();
	let open = $state(false);

	// "Ran 6 commands, read 2 files, searched 3 times"
	const summary = $derived.by(() => {
		const counts = new Map<string, number>();
		for (const block of blocks) {
			const family = familyOf(block);
			counts.set(family, (counts.get(family) ?? 0) + 1);
		}
		const n = (k: string) => counts.get(k) ?? 0;
		const parts: string[] = [];
		const plural = (count: number, one: string, many: string) => `${count} ${count === 1 ? one : many}`;
		if (n('bash')) parts.push(`ran ${plural(n('bash'), 'command', 'commands')}`);
		if (n('read')) parts.push(`read ${plural(n('read'), 'file', 'files')}`);
		if (n('edit') + n('write')) parts.push(`edited ${plural(n('edit') + n('write'), 'file', 'files')}`);
		if (n('search')) parts.push(`searched ${plural(n('search'), 'time', 'times')}`);
		if (n('list')) parts.push(`listed ${plural(n('list'), 'folder', 'folders')}`);
		if (n('fetch') + n('websearch')) parts.push(`browsed ${plural(n('fetch') + n('websearch'), 'page', 'pages')}`);
		const rest = blocks.length - [...counts.entries()].filter(([k]) => ['bash', 'read', 'edit', 'write', 'search', 'list', 'fetch', 'websearch'].includes(k)).reduce((a, [, v]) => a + v, 0);
		if (rest) parts.push(`${plural(rest, 'other call', 'other calls')}`);
		const text = parts.join(', ');
		return text.charAt(0).toUpperCase() + text.slice(1);
	});
	const failed = $derived(blocks.filter(b => b.status === 'failed').length);
</script>

<div class="group" class:open>
	<button class="head" onclick={() => (open = !open)}>
		<span class="icons">
			<Icon name="layers" size={14} />
		</span>
		<span class="text">{summary}</span>
		{#if failed}<span class="failed">{failed} failed</span>{/if}
		<span class="grow"></span>
		<span class="chev" class:up={open}><Icon name="chevronDown" size={14} /></span>
	</button>
	{#if open}
		<div class="list" transition:slide={{duration: 160}}>
			{#each blocks as block, i (block.id ?? i)}
				<ToolCard {block} {root} />
			{/each}
		</div>
	{/if}
</div>

<style>
	.group {
		margin: 6px 0;
		border-radius: var(--radius);
		border: 1px solid var(--border);
		background: var(--card);
	}
	.group.open {
		border-color: var(--border-strong);
	}
	.head {
		display: flex;
		align-items: center;
		gap: 8px;
		width: 100%;
		min-height: 36px;
		padding: 0 10px 0 12px;
		border-radius: var(--radius);
		font-size: var(--fs-sm);
		color: var(--text-2);
		text-align: left;
	}
	.head:hover {
		background: var(--bg-hover);
	}
	.icons {
		display: flex;
		color: var(--text-3);
	}
	.text {
		font-weight: 500;
		color: var(--text);
	}
	.failed {
		font-size: var(--fs-xs);
		color: var(--red);
	}
	.grow {
		flex: 1;
	}
	.chev {
		display: flex;
		color: var(--text-3);
		transition: transform 0.18s var(--ease);
	}
	.chev.up {
		transform: rotate(180deg);
	}
	.list {
		padding: 2px 8px 8px;
		border-top: 1px solid var(--border);
	}
</style>
