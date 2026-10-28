<script lang="ts">
	import {slide} from 'svelte/transition';
	import {app} from '../lib/state.svelte';
	import {diffOffset, langOf, parseArgs, parseDiff, resultNote, todosOf, toolView} from '../lib/tools';
	import {highlight} from '../lib/markdown';
	import type {ToolBlock} from '../lib/types';
	import Icon from './Icon.svelte';
	import DiffBlock from './DiffBlock.svelte';
	import Markdown from './Markdown.svelte';

	let {block, root}: {block: ToolBlock; root: string} = $props();

	const view = $derived(toolView(block, root));
	const running = $derived(block.status === 'in_progress' || block.status === 'pending');
	const failed = $derived(block.status === 'failed');
	const stats = $derived(block.diff ? parseDiff(block.diff) : null);
	const note = $derived(resultNote(block, view));
	const todos = $derived(todosOf(block));
	const plan = $derived(view.family === 'plan' ? String(parseArgs(block).plan ?? '') : '');

	// Edits show their diff right away (unless the user chose minimal tool detail); the
	// rest open on a click, or on their own with "expanded".
	const defaultOpen = $derived(
		app.toolDetail === 'expanded' ||
			(app.toolDetail === 'balanced' && !!block.diff && (view.family === 'edit' || view.family === 'write')),
	);
	let toggled = $state<boolean | null>(null);
	const open = $derived(toggled ?? defaultOpen);
	const hasBody = $derived(!!(block.diff || block.output?.trim() || block.args));

	const outputLang = $derived(view.family === 'read' && view.file ? langOf(view.file) : '');
	const output = $derived(cleanOutput(block.output ?? ''));

	function cleanOutput(text: string): string {
		// Agents fence what they read; the fences aren't part of the file.
		const t = text.replace(/\s+$/, '');
		const fenced = t.match(/^```[^\n]*\n([\s\S]*?)\n?```$/);
		return fenced ? fenced[1] : t;
	}

	function openFile(event: MouseEvent) {
		event.stopPropagation();
		if (!view.file) return;
		const path = view.file.startsWith('/') ? view.file : `${root}/${view.file}`;
		const line = block.locations?.find(l => l.line)?.line ?? undefined;
		app.openFile(root, path, line);
	}

	function copy(event: MouseEvent, text: string) {
		event.stopPropagation();
		navigator.clipboard.writeText(text);
		app.toast('Copied', 'success');
	}

	const argsPretty = $derived.by(() => {
		if (!block.args) return '';
		try {
			return JSON.stringify(JSON.parse(block.args), null, 2);
		} catch {
			return block.args;
		}
	});
</script>

{#if todos}
	<div class="todos">
		{#each todos as todo}
			<div class="todo {todo.status}">
				<span class="box">
					{#if todo.status === 'completed'}<Icon name="check" size={11} stroke={2.6} />{:else if todo.status === 'in_progress'}<span class="cur"></span>{/if}
				</span>
				<span class="todo-text">{todo.status === 'in_progress' && todo.activeForm ? todo.activeForm : todo.content}</span>
			</div>
		{/each}
	</div>
{:else if plan}
	<div class="plan">
		<div class="plan-head"><Icon name="map" size={14} /> Plan</div>
		<Markdown text={plan} {root} />
	</div>
{:else}
	<div class="card" class:open class:failed>
		<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
		<div class="head" class:clickable={hasBody} onclick={() => hasBody && (toggled = !open)}>
			<span class="ico">
				{#if running}<span class="spinner"></span>{:else}<Icon name={view.icon} size={14} />{/if}
			</span>
			<span class="verb" class:shimmer={running}>{view.verb}</span>
			{#if view.subject}
				{#if view.file}
					<button class="subject file" onclick={openFile} title="Open {view.subject}">{view.subject}</button>
				{:else}
					<span class="subject">{view.subject}</span>
				{/if}
			{/if}
			{#if note}<span class="note" class:bad={failed}>· {note}</span>{/if}
			<span class="grow"></span>
			{#if stats && (stats.added || stats.removed)}
				<span class="stats"><span class="add">+{stats.added}</span> <span class="rem">−{stats.removed}</span></span>
			{/if}
			{#if failed}<span class="fail"><Icon name="alert" size={13} /></span>{/if}
			{#if hasBody}
				<span class="chev" class:up={open}><Icon name="chevronDown" size={14} /></span>
			{/if}
		</div>
		{#if open && hasBody}
			<div class="body" transition:slide={{duration: 160}}>
				{#if block.diff}
					<DiffBlock diff={block.diff} offset={diffOffset(block)} lang={view.file ? langOf(view.file) : ''} />
					{#if block.diff_truncated}<div class="cut">Diff cut short</div>{/if}
				{:else if view.family === 'bash'}
					<div class="term selectable">
						<div class="cmd"><span class="prompt">$</span> {parseArgs(block).command ?? view.subject}</div>
						{#if output}<pre>{output}</pre>{/if}
						<button class="copy" onclick={e => copy(e, output)} title="Copy output"><Icon name="copy" size={12} /></button>
					</div>
				{:else if output}
					<div class="out selectable">
						<pre>{#if outputLang}{@html highlight(output, outputLang)}{:else}{output}{/if}</pre>
						<button class="copy" onclick={e => copy(e, output)} title="Copy output"><Icon name="copy" size={12} /></button>
					</div>
				{:else if argsPretty}
					<div class="out selectable"><pre>{argsPretty}</pre></div>
				{/if}
				{#if block.output_truncated && block.full_output_path}
					<div class="cut">
						Showing the end of the output ·
						<button class="link" onclick={() => app.openFile(root, block.full_output_path!)}>open full output</button>
					</div>
				{/if}
			</div>
		{/if}
	</div>
{/if}

<style>
	.card {
		border: 1px solid var(--border);
		border-radius: var(--radius);
		background: var(--card);
		overflow: hidden;
		margin: 6px 0;
		transition: border-color 0.15s;
	}
	.card.open {
		border-color: var(--border-strong);
	}
	.card.failed {
		border-color: color-mix(in srgb, var(--red) 35%, var(--border));
	}
	.head {
		display: flex;
		align-items: center;
		gap: 8px;
		min-height: 36px;
		padding: 0 10px 0 12px;
		font-size: var(--fs-sm);
		color: var(--text-2);
		user-select: none;
	}
	.head.clickable:hover {
		background: var(--bg-hover);
	}
	.open .head {
		border-bottom: 1px solid var(--border);
		background: var(--card-head);
	}
	.ico {
		display: flex;
		width: 15px;
		justify-content: center;
		color: var(--text-3);
	}
	.verb {
		color: var(--text);
		font-weight: 550;
		flex: none;
	}
	.subject {
		font-family: var(--font-mono);
		font-size: var(--fs-mono);
		color: var(--text-2);
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.subject.file:hover {
		color: var(--accent);
		text-decoration: underline;
	}
	.note {
		color: var(--text-3);
		font-size: var(--fs-xs);
		white-space: nowrap;
		flex: none;
	}
	.note.bad {
		color: var(--red);
	}
	.grow {
		flex: 1;
	}
	.stats {
		font-family: var(--font-mono);
		font-size: var(--fs-xs);
		white-space: nowrap;
	}
	.add {
		color: var(--green);
	}
	.rem {
		color: var(--red);
	}
	.fail {
		color: var(--red);
		display: flex;
	}
	.chev {
		display: flex;
		color: var(--text-3);
		transition: transform 0.18s var(--ease);
	}
	.chev.up {
		transform: rotate(180deg);
	}
	.body {
		max-height: 460px;
		overflow: auto;
		position: relative;
	}
	pre {
		margin: 0;
		padding: 10px 14px;
		font-size: var(--fs-mono);
		line-height: 1.55;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.out,
	.term {
		position: relative;
		background: var(--code-bg);
	}
	.term .cmd {
		padding: 10px 14px 0;
		font-family: var(--font-mono);
		font-size: var(--fs-mono);
		color: var(--text);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.term pre {
		color: var(--text-2);
	}
	.prompt {
		color: var(--green);
		user-select: none;
	}
	.copy {
		position: absolute;
		top: 6px;
		right: 6px;
		display: flex;
		padding: 5px;
		border-radius: 5px;
		color: var(--text-3);
		background: var(--card);
		border: 1px solid var(--border);
		opacity: 0;
		transition: opacity 0.15s;
	}
	.body:hover .copy {
		opacity: 1;
	}
	.copy:hover {
		color: var(--text);
	}
	.cut {
		padding: 6px 14px;
		font-size: var(--fs-xs);
		color: var(--text-3);
		border-top: 1px solid var(--border);
		background: var(--card-head);
	}
	.link {
		color: var(--accent);
		font-size: inherit;
	}
	.link:hover {
		text-decoration: underline;
	}

	.todos {
		margin: 8px 0;
		padding: 10px 14px;
		border-radius: var(--radius);
		border: 1px solid var(--border);
		background: var(--card);
		display: flex;
		flex-direction: column;
		gap: 5px;
	}
	.todo {
		display: flex;
		align-items: flex-start;
		gap: 9px;
		font-size: var(--fs-sm);
		color: var(--text-2);
	}
	.box {
		flex: none;
		margin-top: 3px;
		width: 14px;
		height: 14px;
		border-radius: 4px;
		border: 1.5px solid var(--text-4);
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.todo.completed .box {
		background: var(--green);
		border-color: var(--green);
		color: var(--bg);
	}
	.todo.completed .todo-text {
		text-decoration: line-through;
		color: var(--text-3);
	}
	.todo.in_progress .box {
		border-color: var(--accent);
	}
	.todo.in_progress .todo-text {
		color: var(--text);
		font-weight: 550;
	}
	.cur {
		width: 6px;
		height: 6px;
		border-radius: 2px;
		background: var(--accent);
	}
	.plan {
		margin: 8px 0;
		padding: 12px 16px;
		border-radius: var(--radius);
		border: 1px solid color-mix(in srgb, var(--accent) 35%, var(--border));
		background: color-mix(in srgb, var(--accent) 5%, var(--card));
	}
	.plan-head {
		display: flex;
		align-items: center;
		gap: 7px;
		font-weight: 600;
		font-size: var(--fs-sm);
		color: var(--accent);
		margin-bottom: 8px;
	}
</style>
