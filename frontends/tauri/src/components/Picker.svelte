<script lang="ts" module>
	export type PickerItem = {value: string; label: string; detail?: string; group?: string};
</script>

<script lang="ts">
	import {tick} from 'svelte';
	import {scale} from 'svelte/transition';
	import {fuzzy} from '../lib/tools';
	import Icon from './Icon.svelte';

	let {
		anchor,
		items,
		selected,
		loading = false,
		placeholder = 'Search',
		custom = false,
		onpick,
		onclose,
	}: {
		anchor: DOMRect;
		items: PickerItem[];
		selected?: string;
		loading?: boolean;
		placeholder?: string;
		/** Offer the typed text as a value of its own (a model id the list doesn't have). */
		custom?: boolean;
		onpick: (value: string) => void;
		onclose: () => void;
	} = $props();

	let query = $state('');
	let active = $state(0);
	let input = $state<HTMLInputElement>();
	let list = $state<HTMLDivElement>();

	const filtered = $derived.by(() => {
		const q = query.trim();
		if (!q) return items;
		return items
			.map(item => ({item, m: fuzzy(q, item.label + ' ' + (item.detail ?? ''))}))
			.filter(x => x.m)
			.sort((a, b) => b.m!.score - a.m!.score)
			.map(x => x.item);
	});
	const showCustom = $derived(custom && query.trim() && !items.some(i => i.value === query.trim()));
	const total = $derived(filtered.length + (showCustom ? 1 : 0));

	$effect(() => {
		tick().then(() => input?.focus());
	});
	$effect(() => {
		query;
		active = 0;
	});
	$effect(() => {
		const el = list?.querySelector(`[data-i="${active}"]`);
		el?.scrollIntoView({block: 'nearest'});
	});

	// Opens above the composer when there's no room below.
	const below = $derived(window.innerHeight - anchor.bottom > 340);
	const style = $derived(
		(below ? `top:${anchor.bottom + 6}px;` : `bottom:${window.innerHeight - anchor.top + 6}px;`) +
			`left:${Math.min(anchor.left, window.innerWidth - 340)}px`,
	);

	function key(event: KeyboardEvent) {
		if (event.key === 'ArrowDown') active = (active + 1) % Math.max(total, 1);
		else if (event.key === 'ArrowUp') active = (active - 1 + total) % Math.max(total, 1);
		else if (event.key === 'Enter') {
			if (active < filtered.length) onpick(filtered[active].value);
			else if (showCustom) onpick(query.trim());
		} else if (event.key === 'Escape') onclose();
		else return;
		event.preventDefault();
		event.stopPropagation();
	}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="scrim" onmousedown={onclose}></div>
<div class="picker" {style} transition:scale={{start: 0.97, duration: 110}}>
	{#if items.length > 6 || custom}
		<div class="search">
			<Icon name="search" size={13} />
			<input bind:this={input} bind:value={query} {placeholder} onkeydown={key} spellcheck="false" />
			{#if loading}<span class="spinner"></span>{/if}
		</div>
	{:else}
		<input class="hidden-input" bind:this={input} onkeydown={key} />
	{/if}
	<div class="list" bind:this={list}>
		{#each filtered as item, i (item.value)}
			{#if item.group && (i === 0 || filtered[i - 1].group !== item.group)}
				<div class="group">{item.group}</div>
			{/if}
			<button class="item" class:active={i === active} data-i={i} onmouseenter={() => (active = i)} onclick={() => onpick(item.value)}>
				<span class="check">{#if item.value === selected}<Icon name="check" size={13} stroke={2.2} />{/if}</span>
				<span class="text">
					<span class="label">{item.label}</span>
					{#if item.detail}<span class="detail">{item.detail}</span>{/if}
				</span>
			</button>
		{/each}
		{#if showCustom}
			<button
				class="item"
				class:active={active === filtered.length}
				data-i={filtered.length}
				onmouseenter={() => (active = filtered.length)}
				onclick={() => onpick(query.trim())}
			>
				<span class="check"><Icon name="plus" size={13} /></span>
				<span class="text"><span class="label">Use “{query.trim()}”</span></span>
			</button>
		{/if}
		{#if !filtered.length && !showCustom}
			<div class="none">{loading ? 'Loading…' : 'Nothing found'}</div>
		{/if}
	</div>
</div>

<style>
	.scrim {
		position: fixed;
		inset: 0;
		z-index: 900;
	}
	.picker {
		position: fixed;
		z-index: 901;
		width: 320px;
		max-height: 360px;
		display: flex;
		flex-direction: column;
		border-radius: 12px;
		background: color-mix(in srgb, var(--bg-raised) 95%, transparent);
		backdrop-filter: blur(20px) saturate(1.6);
		-webkit-backdrop-filter: blur(20px) saturate(1.6);
		border: 1px solid var(--border-strong);
		box-shadow: var(--shadow);
		overflow: hidden;
	}
	.search {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 0 12px;
		height: 38px;
		border-bottom: 1px solid var(--border);
		color: var(--text-3);
	}
	.search input {
		flex: 1;
		border: none;
		outline: none;
		background: none;
		font-size: var(--fs-sm);
		color: var(--text);
	}
	.hidden-input {
		position: absolute;
		opacity: 0;
		pointer-events: none;
		height: 0;
	}
	.list {
		overflow-y: auto;
		padding: 5px;
	}
	.group {
		padding: 6px 8px 3px;
		font-size: var(--fs-xs);
		color: var(--text-3);
		font-weight: 600;
	}
	.item {
		display: flex;
		align-items: center;
		gap: 8px;
		width: 100%;
		padding: 5px 8px 5px 4px;
		border-radius: 7px;
		text-align: left;
		font-size: var(--fs-sm);
	}
	.item.active {
		background: var(--bg-active);
	}
	.check {
		width: 16px;
		display: flex;
		justify-content: center;
		color: var(--accent);
		flex: none;
	}
	.text {
		display: flex;
		flex-direction: column;
		min-width: 0;
	}
	.label {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.detail {
		font-size: var(--fs-xs);
		color: var(--text-3);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.none {
		padding: 14px;
		text-align: center;
		color: var(--text-3);
		font-size: var(--fs-sm);
	}
</style>
