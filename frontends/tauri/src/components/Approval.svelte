<script lang="ts">
	import {fly} from 'svelte/transition';
	import {app, type Chat} from '../lib/state.svelte';
	import Icon from './Icon.svelte';

	let {chat}: {chat: Chat} = $props();
	const approval = $derived(chat.approval!);
	const isBash = $derived(approval.tool === 'bash');

	function onKey(event: KeyboardEvent) {
		if (!chat.approval || app.activeChat !== chat) return;
		const target = event.target as HTMLElement;
		const typing = target.closest('textarea, input') && (target as HTMLTextAreaElement).value?.trim();
		if (typing) return;
		if (event.key === 'Enter' && (event.metaKey || event.ctrlKey || !target.closest('textarea, input'))) {
			event.preventDefault();
			app.respond(chat, 'approve');
		} else if (event.key === 'Escape') {
			event.preventDefault();
			app.respond(chat, 'deny');
		}
	}
</script>

<svelte:window onkeydown={onKey} />

<div class="approval" transition:fly={{y: 10, duration: 180}}>
	<div class="head">
		<span class="icon"><Icon name="shield" size={15} /></span>
		<span class="title">Allow <b>{approval.tool}</b>?</span>
	</div>
	<pre class="summary selectable">{approval.summary}</pre>
	<div class="actions">
		<button class="btn primary" onclick={() => app.respond(chat, 'approve')}>
			Allow <kbd>↩</kbd>
		</button>
		<button class="btn" onclick={() => app.respond(chat, 'approve_rest')} title="Allow every remaining call in this turn">
			Allow for this turn
		</button>
		{#if isBash && approval.allow_prefix}
			<button class="btn" onclick={() => app.respond(chat, 'allow_prefix', approval.allow_prefix!)} title="Commands starting with this run without asking">
				Always allow <code>{approval.allow_prefix}</code>
			</button>
		{:else if !isBash}
			<button class="btn" onclick={() => app.respond(chat, 'allow_tool')} title="This tool runs without asking in this folder">
				Always allow here
			</button>
		{/if}
		<span class="grow"></span>
		<button class="btn ghost danger" onclick={() => app.respond(chat, 'deny')}>Deny <kbd>esc</kbd></button>
	</div>
</div>

<style>
	.approval {
		margin-bottom: 10px;
		padding: 12px 14px;
		border-radius: var(--radius-lg);
		border: 1px solid color-mix(in srgb, var(--yellow) 45%, var(--border));
		background: color-mix(in srgb, var(--yellow) 6%, var(--bg-raised));
		box-shadow: var(--shadow-sm);
	}
	.head {
		display: flex;
		align-items: center;
		gap: 8px;
		font-size: var(--fs-sm);
	}
	.icon {
		display: flex;
		color: var(--yellow);
	}
	.title b {
		font-family: var(--font-mono);
		font-weight: 600;
	}
	.summary {
		margin: 8px 0 10px;
		padding: 8px 10px;
		border-radius: 7px;
		background: var(--code-bg);
		border: 1px solid var(--border);
		font-size: var(--fs-mono);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		max-height: 200px;
		overflow-y: auto;
	}
	.actions {
		display: flex;
		align-items: center;
		gap: 6px;
		flex-wrap: wrap;
	}
	.grow {
		flex: 1;
	}
	.actions kbd {
		background: rgba(0, 0, 0, 0.15);
		border-color: transparent;
		color: inherit;
		opacity: 0.8;
	}
	.actions code {
		font-size: var(--fs-xs);
		padding: 1px 5px;
		border-radius: 4px;
		background: var(--bg-active);
	}
</style>
