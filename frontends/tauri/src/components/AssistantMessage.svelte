<script lang="ts">
	import type {AssistantMessage, Block} from '../lib/types';
	import {formatDuration} from '../lib/tools';
	import {app} from '../lib/state.svelte';
	import Markdown from './Markdown.svelte';
	import ToolCard from './ToolCard.svelte';
	import ToolGroup from './ToolGroup.svelte';
	import {familyOf} from '../lib/tools';
	import type {ToolBlock} from '../lib/types';
	import Thinking from './Thinking.svelte';
	import Evidence from './Evidence.svelte';
	import Icon from './Icon.svelte';

	let {message, root, startedAt = null}: {message: AssistantMessage; root: string; startedAt?: number | null} = $props();

	const blocks = $derived(message.blocks ?? []);
	// Steps: everything up to the last tool call or thinking; the answer after them stays in view
	// when a finished turn's steps are folded.
	const lastStep = $derived.by(() => {
		for (let i = blocks.length - 1; i >= 0; i--) if (blocks[i].type !== 'answer') return i;
		return -1;
	});
	// Runs of 3+ finished calls without a diff to show fold into one summary row.
	type Segment = {kind: 'block'; block: Block; index: number} | {kind: 'group'; blocks: ToolBlock[]; index: number; last: number};
	const groupable = (b: Block) =>
		b.type === 'tool' &&
		b.status !== 'in_progress' &&
		b.status !== 'pending' &&
		!b.diff &&
		!['todo', 'plan'].includes(familyOf(b));
	const segments = $derived.by((): Segment[] => {
		const out: Segment[] = [];
		let run: {blocks: ToolBlock[]; start: number} | null = null;
		const flush = () => {
			if (!run) return;
			if (run.blocks.length >= 3 && app.toolDetail !== 'expanded') {
				out.push({kind: 'group', blocks: run.blocks, index: run.start, last: run.start + run.blocks.length - 1});
			} else run.blocks.forEach((block, k) => out.push({kind: 'block', block, index: run!.start + k}));
			run = null;
		};
		blocks.forEach((block, index) => {
			if (groupable(block)) {
				if (!run) run = {blocks: [], start: index};
				run.blocks.push(block as ToolBlock);
			} else if (block.type === 'answer' && !block.text.trim()) {
				// Empty text between calls doesn't break a run.
			} else {
				flush();
				out.push({kind: 'block', block, index});
			}
		});
		flush();
		return out;
	});
	const toolCount = $derived(blocks.filter(b => b.type === 'tool').length);
	const stepsFoldable = $derived(!message.streaming && lastStep >= 0 && toolCount > 0);
	let folded = $state<boolean | null>(null);
	const isFolded = $derived(folded ?? (stepsFoldable && app.toolDetail === 'minimal'));

	let now = $state(Date.now());
	$effect(() => {
		if (!message.streaming) return;
		const timer = setInterval(() => (now = Date.now()), 1000);
		return () => clearInterval(timer);
	});
	const elapsed = $derived(
		message.streaming && startedAt ? (now - startedAt) / 1000 : (message.worked_secs ?? null),
	);

	const answerText = $derived(
		blocks
			.filter((b): b is Extract<Block, {type: 'answer'}> => b.type === 'answer')
			.map(b => b.text)
			.join('\n\n')
			.trim(),
	);
	const empty = $derived(!blocks.length || blocks.every(b => b.type === 'answer' && !b.text.trim()));
	const isError = (text: string) => /^\s*\[Error\]/.test(text);

	let copied = $state(false);
	function copyAnswer() {
		navigator.clipboard.writeText(answerText);
		copied = true;
		setTimeout(() => (copied = false), 1400);
	}
</script>

<div class="assistant">
	{#if (message.streaming || stepsFoldable) && (toolCount > 0 || message.streaming)}
		<button class="worked" onclick={() => (folded = !isFolded)} disabled={message.streaming}>
			{#if message.streaming}
				<span class="shimmer">Working{elapsed ? ` for ${formatDuration(elapsed)}` : '…'}</span>
			{:else}
				<span class="chev" class:folded={isFolded}><Icon name="chevronDown" size={13} /></span>
				<span>Worked{elapsed ? ` for ${formatDuration(elapsed)}` : ''}</span>
				<span class="dim">· {toolCount} {toolCount === 1 ? 'step' : 'steps'}</span>
			{/if}
		</button>
	{/if}

	{#each segments as segment (segment.index)}
		{#if !(isFolded && (segment.kind === 'group' ? segment.last : segment.index) <= lastStep)}
			{#if segment.kind === 'group'}
				<ToolGroup blocks={segment.blocks} {root} />
			{:else}
				{@const block = segment.block}
				{@const i = segment.index}
				<div class="block">
					{#if block.type === 'answer'}
						{#if block.text.trim()}
							{#if isError(block.text)}
								<div class="error selectable">
									<Icon name="alert" size={15} />
									<span>{block.text.replace(/^\s*\[Error\]\s*/, '').trim()}</span>
								</div>
							{:else}
								<Markdown text={block.text} streaming={message.streaming && i === blocks.length - 1} {root} />
							{/if}
						{/if}
					{:else if block.type === 'thinking'}
						{#if block.text.trim()}
							<Thinking text={block.text} live={message.streaming && i === blocks.length - 1} />
						{/if}
					{:else}
						<ToolCard {block} {root} />
					{/if}
				</div>
			{/if}
		{/if}
	{/each}

	{#if message.streaming && empty}
		<div class="pending"><span class="dot"></span><span class="dot"></span><span class="dot"></span></div>
	{/if}

	{#if message.evidence}
		<Evidence report={message.evidence} />
	{/if}

	{#if !message.streaming && !empty}
		<div class="foot">
			{#if message.route}
				<span class="route" title={message.route.reason || ''}>
					{message.route.label}{message.route.model ? ` · ${message.route.model}` : ''}
					{#if message.route.failover_from}<span class="failover">· failover from {message.route.failover_from}</span>{/if}
				</span>
			{/if}
			{#if answerText}
				<button class="icon-btn sm" title="Copy answer" onclick={copyAnswer}>
					<Icon name={copied ? 'check' : 'copy'} size={13} />
				</button>
			{/if}
		</div>
	{/if}
</div>

<style>
	.assistant {
		margin: 6px 0 10px;
	}
	.worked {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		height: 26px;
		padding: 0 8px 0 4px;
		margin: 0 0 4px -4px;
		border-radius: 6px;
		font-size: var(--fs-sm);
		color: var(--text-3);
	}
	.worked:not(:disabled):hover {
		background: var(--bg-hover);
		color: var(--text-2);
	}
	.chev {
		display: flex;
		transition: transform 0.18s var(--ease);
	}
	.chev.folded {
		transform: rotate(-90deg);
	}
	.dim {
		color: var(--text-4);
	}
	.block :global(.md) {
		margin: 6px 0 10px;
	}
	.error {
		display: flex;
		gap: 9px;
		align-items: flex-start;
		padding: 10px 14px;
		margin: 8px 0;
		border-radius: var(--radius);
		background: var(--red-soft);
		color: var(--red);
		font-size: var(--fs-sm);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.error :global(svg) {
		margin-top: 2px;
	}
	.error span {
		color: var(--text);
	}
	.pending {
		display: flex;
		gap: 5px;
		padding: 10px 0;
	}
	.pending .dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--text-3);
		animation: bounce 1.2s infinite ease-in-out;
	}
	.pending .dot:nth-child(2) {
		animation-delay: 0.15s;
	}
	.pending .dot:nth-child(3) {
		animation-delay: 0.3s;
	}
	@keyframes bounce {
		0%,
		80%,
		100% {
			opacity: 0.25;
			transform: translateY(0);
		}
		40% {
			opacity: 1;
			transform: translateY(-3px);
		}
	}
	.foot {
		display: flex;
		align-items: center;
		gap: 6px;
		margin-top: 2px;
		min-height: 22px;
		opacity: 0;
		transition: opacity 0.15s;
	}
	.assistant:hover .foot {
		opacity: 1;
	}
	.route {
		font-size: var(--fs-xs);
		color: var(--text-4);
	}
	.failover {
		color: var(--yellow);
	}
</style>
