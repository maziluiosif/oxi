<script lang="ts">
	import {rpc} from '../lib/rpc';
	import type {Chat} from '../lib/state.svelte';
	import type {UserMessage} from '../lib/types';
	import Icon from './Icon.svelte';

	let {message, index, chat, onreuse}: {message: UserMessage; index: number; chat: Chat; onreuse: (text: string) => void} = $props();

	let images = $state<string[]>([]);
	let zoom = $state<string | null>(null);
	let expanded = $state(false);
	const long = $derived(message.text.length > 1200 || message.text.split('\n').length > 18);

	$effect(() => {
		if (!message.images?.length || !chat.id) return;
		const id = chat.id;
		Promise.all(
			message.images.map(img =>
				rpc
					.request('chat.image', {root: chat.root, chat: id, message: index, index: img.index})
					.then(r => `data:${r.mime};base64,${r.data}`)
					.catch(() => ''),
			),
		).then(urls => (images = urls.filter(Boolean)));
	});

	const textAttachments = $derived(message.attachments.filter(a => !a.startsWith('image (')));
</script>

{#if message.summary}
	<div class="summary">
		<Icon name="layers" size={13} /> Earlier messages were summarized to save context
	</div>
{:else}
	<div class="user">
		<div class="bubble selectable" class:clamped={long && !expanded}>
			{#if images.length}
				<div class="images">
					{#each images as src}
						<button class="thumb" onclick={() => (zoom = src)}><img {src} alt="attachment" /></button>
					{/each}
				</div>
			{/if}
			{#if textAttachments.length}
				<div class="chips">
					{#each textAttachments as name}
						<span class="chip"><Icon name="paperclip" size={12} /> {name}</span>
					{/each}
				</div>
			{/if}
			{#if message.text}<div class="text">{message.text}</div>{/if}
		</div>
		<div class="actions">
			{#if long}
				<button class="icon-btn sm" title={expanded ? 'Collapse' : 'Expand'} onclick={() => (expanded = !expanded)}>
					<Icon name={expanded ? 'chevronUp' : 'chevronDown'} size={13} />
				</button>
			{/if}
			<button class="icon-btn sm" title="Edit as new message" onclick={() => onreuse(message.text)}><Icon name="edit" size={13} /></button>
			<button class="icon-btn sm" title="Copy" onclick={() => navigator.clipboard.writeText(message.text)}><Icon name="copy" size={13} /></button>
		</div>
	</div>
{/if}

{#if zoom}
	<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
	<div class="zoom" onclick={() => (zoom = null)}><img src={zoom} alt="attachment" /></div>
{/if}

<style>
	.user {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		margin: 22px 0 14px;
	}
	.bubble {
		max-width: min(85%, 640px);
		padding: 10px 15px;
		border-radius: 16px 16px 4px 16px;
		background: var(--user-bubble);
		border: 1px solid var(--border);
		line-height: 1.55;
		position: relative;
	}
	.bubble.clamped {
		max-height: 300px;
		overflow: hidden;
		mask-image: linear-gradient(to bottom, black 70%, transparent);
		-webkit-mask-image: linear-gradient(to bottom, black 70%, transparent);
	}
	.text {
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.images {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
		margin-bottom: 8px;
	}
	.thumb img {
		display: block;
		max-height: 140px;
		max-width: 240px;
		border-radius: 8px;
		border: 1px solid var(--border);
	}
	.chips {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
		margin-bottom: 6px;
	}
	.chip {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		padding: 2px 8px;
		border-radius: 6px;
		font-size: var(--fs-xs);
		background: var(--bg-active);
		color: var(--text-2);
	}
	.actions {
		display: flex;
		gap: 2px;
		margin-top: 4px;
		opacity: 0;
		transition: opacity 0.15s;
	}
	.user:hover .actions {
		opacity: 1;
	}
	.summary {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 7px;
		margin: 18px 0;
		font-size: var(--fs-xs);
		color: var(--text-3);
	}
	.zoom {
		position: fixed;
		inset: 0;
		z-index: 960;
		background: var(--scrim);
		backdrop-filter: blur(6px);
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 40px;
	}
	.zoom img {
		max-width: 100%;
		max-height: 100%;
		border-radius: 10px;
		box-shadow: var(--shadow);
	}
</style>
