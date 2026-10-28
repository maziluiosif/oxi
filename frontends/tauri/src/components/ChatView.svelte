<script lang="ts">
	import {tick} from 'svelte';
	import {fade, fly} from 'svelte/transition';
	import {app, folderLabel, type Chat} from '../lib/state.svelte';
	import {hooks} from '../lib/commands';
	import {todosOf, type Todo} from '../lib/tools';
	import type {ToolBlock} from '../lib/types';
	import UserMessage from './UserMessage.svelte';
	import AssistantMessage from './AssistantMessage.svelte';
	import Composer from './Composer.svelte';
	import Approval from './Approval.svelte';
	import Icon from './Icon.svelte';

	let {chat, active}: {chat: Chat; active: boolean} = $props();

	let scroller = $state<HTMLDivElement>();
	let content = $state<HTMLDivElement>();
	let stuck = $state(true);
	let composer = $state<{focus: () => void; setText: (text: string) => void}>();

	// Follow the stream while the view is at the bottom; leave it alone once the user scrolls up.
	function onScroll() {
		if (!scroller) return;
		stuck = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 40;
	}

	function toBottom(smooth = false) {
		if (!scroller) return;
		scroller.scrollTo({top: scroller.scrollHeight, behavior: smooth ? 'smooth' : 'auto'});
	}

	$effect(() => {
		if (!content) return;
		const observer = new ResizeObserver(() => {
			if (stuck) toBottom();
		});
		observer.observe(content);
		return () => observer.disconnect();
	});

	// A chat opened (or reloaded) starts at its end.
	let lastLoaded = false;
	$effect(() => {
		if (chat.loaded && !lastLoaded) {
			lastLoaded = true;
			tick().then(() => toBottom());
		}
	});

	// Sending scrolls back down.
	let lastCount = 0;
	$effect(() => {
		const count = chat.messages.length;
		if (count > lastCount && chat.messages[count - 1]?.role === 'assistant' && chat.running) {
			stuck = true;
			tick().then(() => toBottom(true));
		}
		lastCount = count;
	});

	$effect(() => {
		if (active) {
			hooks.focusComposer = () => composer?.focus();
			hooks.findInChat = () => openFind();
			tick().then(() => composer?.focus());
		}
	});

	// The checklist of the turn in progress (or the last one), above the composer.
	const todos = $derived.by((): Todo[] | null => {
		for (let m = chat.messages.length - 1; m >= 0; m--) {
			const message = chat.messages[m];
			if (message.role !== 'assistant') continue;
			for (let b = message.blocks.length - 1; b >= 0; b--) {
				const block = message.blocks[b];
				if (block.type === 'tool') {
					const items = todosOf(block as ToolBlock);
					if (items) return items;
				}
			}
			return null;
		}
		return null;
	});
	const showTodos = $derived(!!todos?.length && (chat.running || todos.some(t => t.status !== 'completed')));
	let todosOpen = $state(true);

	// ---------- find in chat (CSS highlights, no DOM changes) ----------
	let findOpen = $state(false);
	let findQuery = $state('');
	let findInput = $state<HTMLInputElement>();
	let matches = $state<Range[]>([]);
	let current = $state(0);

	async function openFind() {
		findOpen = true;
		await tick();
		findInput?.focus();
		findInput?.select();
	}

	function closeFind() {
		findOpen = false;
		(CSS as any).highlights?.delete('find');
		(CSS as any).highlights?.delete('find-current');
		composer?.focus();
	}

	function runFind() {
		const highlights = (CSS as any).highlights;
		if (!content || !highlights) return;
		const q = findQuery.toLowerCase();
		const found: Range[] = [];
		if (q) {
			const walker = document.createTreeWalker(content, NodeFilter.SHOW_TEXT);
			let node: Node | null;
			while ((node = walker.nextNode())) {
				const text = node.textContent?.toLowerCase() ?? '';
				let at = text.indexOf(q);
				while (at >= 0) {
					const range = new Range();
					range.setStart(node, at);
					range.setEnd(node, at + q.length);
					found.push(range);
					at = text.indexOf(q, at + q.length);
				}
			}
		}
		matches = found;
		current = Math.max(0, found.length - 1);
		paint();
	}

	function paint() {
		const highlights = (CSS as any).highlights;
		const Highlight = (window as any).Highlight;
		if (!highlights || !Highlight) return;
		highlights.set('find', new Highlight(...matches));
		const range = matches[current];
		if (range) {
			highlights.set('find-current', new Highlight(range));
			const rect = range.getBoundingClientRect();
			const box = scroller!.getBoundingClientRect();
			if (rect.top < box.top + 60 || rect.bottom > box.bottom - 60) {
				scroller!.scrollBy({top: rect.top - box.top - box.height / 2, behavior: 'smooth'});
			}
			stuck = false;
		} else highlights.delete('find-current');
	}

	function step(dir: number) {
		if (!matches.length) return;
		current = (current + dir + matches.length) % matches.length;
		paint();
	}
</script>

<div class="chat">
	{#if findOpen}
		<div class="find" transition:fly={{y: -6, duration: 140}}>
			<Icon name="search" size={14} />
			<input
				bind:this={findInput}
				bind:value={findQuery}
				oninput={runFind}
				placeholder="Find in chat"
				onkeydown={e => {
					if (e.key === 'Escape') closeFind();
					else if (e.key === 'Enter') step(e.shiftKey ? 1 : -1);
				}}
			/>
			<span class="count">{matches.length ? `${current + 1}/${matches.length}` : findQuery ? '0' : ''}</span>
			<button class="icon-btn sm" onclick={() => step(-1)} title="Previous"><Icon name="chevronUp" size={14} /></button>
			<button class="icon-btn sm" onclick={() => step(1)} title="Next"><Icon name="chevronDown" size={14} /></button>
			<button class="icon-btn sm" onclick={closeFind} title="Close"><Icon name="x" size={13} /></button>
		</div>
	{/if}

	<div class="scroller" bind:this={scroller} onscroll={onScroll}>
		<div class="column" bind:this={content}>
			{#if !chat.loaded}
				<div class="loading"><span class="spinner"></span></div>
			{:else if !chat.messages.length}
				<div class="hello" in:fade={{duration: 200}}>
					<div class="logo">oxi</div>
					<p class="where"><Icon name="folder" size={14} /> {folderLabel(chat.root)}</p>
					<p class="hint">Ask about the code, or tell it what to change.</p>
				</div>
			{:else}
				{#each chat.messages as message, index (index)}
					<div class="row">
						{#if message.role === 'user'}
							<UserMessage {message} {index} {chat} onreuse={text => composer?.setText(text)} />
						{:else}
							<AssistantMessage
								{message}
								root={chat.root}
								startedAt={index === chat.messages.length - 1 ? chat.startedAt : null}
							/>
						{/if}
					</div>
				{/each}
			{/if}
			<div class="tail"></div>
		</div>
	</div>

	{#if !stuck}
		<button class="jump" onclick={() => ((stuck = true), toBottom(true))} transition:fade={{duration: 120}} title="Scroll to bottom">
			<Icon name="arrowDown" size={15} />
		</button>
	{/if}

	<div class="dock">
		<div class="column">
			{#if showTodos && todos}
				{@const done = todos.filter(t => t.status === 'completed').length}
				<div class="todo-dock" transition:fly={{y: 8, duration: 160}}>
					<button class="todo-head" onclick={() => (todosOpen = !todosOpen)}>
						<Icon name="checklist" size={14} />
						<span>Tasks</span>
						<span class="progress"><span style="width:{(done / todos.length) * 100}%"></span></span>
						<span class="dim">{done}/{todos.length}</span>
						<span class="chev" class:up={todosOpen}><Icon name="chevronDown" size={13} /></span>
					</button>
					{#if todosOpen}
						<div class="todo-list">
							{#each todos as todo}
								<div class="todo {todo.status}">
									<span class="mark">
										{#if todo.status === 'completed'}<Icon name="check" size={11} stroke={2.6} />{:else if todo.status === 'in_progress'}<span class="spinner"></span>{/if}
									</span>
									<span>{todo.status === 'in_progress' && todo.activeForm ? todo.activeForm : todo.content}</span>
								</div>
							{/each}
						</div>
					{/if}
				</div>
			{/if}
			{#if chat.approval}
				<Approval {chat} />
			{/if}
			{#if chat.status}
				<div class="status warn" transition:fly={{y: 6, duration: 140}}><span class="spinner"></span> {chat.status}</div>
			{/if}
			{#if chat.error}
				<div class="status error" transition:fly={{y: 6, duration: 140}}>
					<Icon name="alert" size={14} />
					<span class="selectable">{chat.error}</span>
					<button class="icon-btn sm" onclick={() => (chat.error = null)}><Icon name="x" size={12} /></button>
				</div>
			{/if}
			<Composer {chat} bind:this={composer} />
		</div>
	</div>
</div>

<style>
	.chat {
		flex: 1;
		display: flex;
		flex-direction: column;
		min-height: 0;
		position: relative;
	}
	.scroller {
		flex: 1;
		overflow-y: auto;
		overflow-x: hidden;
		scrollbar-gutter: stable;
	}
	.column {
		width: 100%;
		max-width: var(--chat-width);
		margin: 0 auto;
		padding: 0 28px;
	}
	.scroller .column {
		padding-top: 18px;
	}
	.row {
		content-visibility: auto;
		contain-intrinsic-size: auto 120px;
	}
	.tail {
		height: 24px;
	}
	.loading {
		display: flex;
		justify-content: center;
		padding: 80px 0;
	}
	.hello {
		text-align: center;
		padding: 18vh 0 40px;
		color: var(--text-3);
	}
	.logo {
		font-size: 44px;
		font-weight: 700;
		letter-spacing: -0.04em;
		background: linear-gradient(135deg, var(--accent), var(--purple));
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
		margin-bottom: 6px;
	}
	.where {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		margin: 0 0 4px;
		color: var(--text-2);
		font-weight: 500;
	}
	.hint {
		margin: 0;
		font-size: var(--fs-sm);
	}
	.jump {
		position: absolute;
		left: 50%;
		transform: translateX(-50%);
		bottom: 150px;
		width: 34px;
		height: 34px;
		border-radius: 50%;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--bg-raised);
		border: 1px solid var(--border-strong);
		box-shadow: var(--shadow-sm);
		color: var(--text-2);
		z-index: 5;
	}
	.jump:hover {
		color: var(--text);
	}
	.dock {
		flex: none;
		padding-bottom: 14px;
		position: relative;
	}
	.dock::before {
		content: '';
		position: absolute;
		left: 0;
		right: 0;
		top: -24px;
		height: 24px;
		background: linear-gradient(to bottom, transparent, var(--bg));
		pointer-events: none;
	}
	.status {
		display: flex;
		align-items: center;
		gap: 8px;
		margin-bottom: 8px;
		padding: 8px 12px;
		border-radius: var(--radius);
		font-size: var(--fs-sm);
	}
	.status.warn {
		background: var(--yellow-soft);
		color: var(--yellow);
	}
	.status.error {
		background: var(--red-soft);
		color: var(--red);
	}
	.status.error span {
		flex: 1;
		color: var(--text);
		overflow-wrap: anywhere;
	}
	.todo-dock {
		margin-bottom: 8px;
		border-radius: var(--radius);
		border: 1px solid var(--border);
		background: var(--card);
		overflow: hidden;
	}
	.todo-head {
		display: flex;
		align-items: center;
		gap: 8px;
		width: 100%;
		height: 32px;
		padding: 0 12px;
		font-size: var(--fs-sm);
		font-weight: 550;
		color: var(--text-2);
	}
	.todo-head:hover {
		background: var(--bg-hover);
	}
	.progress {
		flex: 1;
		height: 4px;
		border-radius: 2px;
		background: var(--bg-active);
		overflow: hidden;
		max-width: 160px;
		margin-left: 4px;
	}
	.progress span {
		display: block;
		height: 100%;
		background: var(--green);
		border-radius: 2px;
		transition: width 0.4s var(--ease);
	}
	.dim {
		color: var(--text-3);
		font-weight: 400;
		font-size: var(--fs-xs);
		margin-left: auto;
	}
	.chev {
		display: flex;
		color: var(--text-3);
		transition: transform 0.18s var(--ease);
	}
	.chev.up {
		transform: rotate(180deg);
	}
	.todo-list {
		padding: 2px 12px 10px;
		display: flex;
		flex-direction: column;
		gap: 3px;
		max-height: 180px;
		overflow-y: auto;
	}
	.todo {
		display: flex;
		align-items: center;
		gap: 9px;
		font-size: var(--fs-sm);
		color: var(--text-2);
	}
	.mark {
		flex: none;
		width: 14px;
		height: 14px;
		border-radius: 4px;
		border: 1.5px solid var(--text-4);
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.mark .spinner {
		width: 9px;
		height: 9px;
		border-width: 1.4px;
	}
	.todo.in_progress .mark {
		border: none;
	}
	.todo.completed .mark {
		background: var(--green);
		border-color: var(--green);
		color: var(--bg);
	}
	.todo.completed span:last-child {
		color: var(--text-3);
		text-decoration: line-through;
	}
	.todo.in_progress span:last-child {
		color: var(--text);
	}
	.find {
		position: absolute;
		top: 10px;
		right: 22px;
		z-index: 10;
		display: flex;
		align-items: center;
		gap: 4px;
		padding: 4px 4px 4px 10px;
		border-radius: 10px;
		background: var(--bg-raised);
		border: 1px solid var(--border-strong);
		box-shadow: var(--shadow);
		color: var(--text-3);
	}
	.find input {
		width: 200px;
		border: none;
		outline: none;
		background: none;
		font-size: var(--fs-sm);
		color: var(--text);
		margin-left: 4px;
	}
	.count {
		font-size: var(--fs-xs);
		min-width: 34px;
		text-align: right;
		font-variant-numeric: tabular-nums;
	}
	:global(::highlight(find)) {
		background-color: var(--yellow-soft);
		color: inherit;
	}
	:global(::highlight(find-current)) {
		background-color: var(--yellow);
		color: #1a1300;
	}
</style>
