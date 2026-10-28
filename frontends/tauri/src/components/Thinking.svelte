<script lang="ts">
	import {slide} from 'svelte/transition';
	import Icon from './Icon.svelte';
	import Markdown from './Markdown.svelte';

	let {text, live}: {text: string; live: boolean} = $props();
	let open = $state(false);
	const words = $derived(text.trim().split(/\s+/).length);
	// While it streams, the last lines show under the header.
	const tail = $derived(live ? text.trim().split('\n').slice(-3).join('\n') : '');
</script>

<div class="thinking">
	<button class="head" onclick={() => (open = !open)}>
		<Icon name="brain" size={14} />
		<span class:shimmer={live}>{live ? 'Thinking' : 'Thought'}</span>
		{#if !live}<span class="meta">{words} words</span>{/if}
		<span class="chev" class:up={open}><Icon name="chevronDown" size={13} /></span>
	</button>
	{#if open}
		<div class="body" transition:slide={{duration: 160}}>
			<Markdown {text} streaming={live} />
		</div>
	{:else if live && tail}
		<div class="tail">{tail}</div>
	{/if}
</div>

<style>
	.thinking {
		margin: 4px 0 8px;
	}
	.head {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		height: 26px;
		padding: 0 8px 0 4px;
		margin-left: -4px;
		border-radius: 6px;
		color: var(--text-3);
		font-size: var(--fs-sm);
	}
	.head:hover {
		background: var(--bg-hover);
		color: var(--text-2);
	}
	.meta {
		font-size: var(--fs-xs);
		color: var(--text-4);
	}
	.chev {
		display: flex;
		transition: transform 0.18s var(--ease);
	}
	.chev.up {
		transform: rotate(180deg);
	}
	.body,
	.tail {
		margin: 4px 0 0 6px;
		padding: 2px 0 2px 14px;
		border-left: 2px solid var(--border-strong);
		color: var(--text-3);
		font-size: var(--fs-sm);
	}
	.tail {
		white-space: pre-wrap;
		max-height: 4.8em;
		overflow: hidden;
		mask-image: linear-gradient(to bottom, transparent, black 40%);
		-webkit-mask-image: linear-gradient(to bottom, transparent, black 40%);
	}
</style>
