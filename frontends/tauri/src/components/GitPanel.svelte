<script lang="ts">
	import {onMount} from 'svelte';
	import {slide} from 'svelte/transition';
	import {app, baseName, folderLabel} from '../lib/state.svelte';
	import {rpc, errorText} from '../lib/rpc';
	import {overlay} from '../lib/overlay.svelte';
	import {formatKeys} from '../lib/commands';
	import type {GitEntry, GitState} from '../lib/types';
	import Icon from './Icon.svelte';

	const root = $derived(app.activeRoot);
	let git = $state<GitState | null>(null);
	let busy = $state<string | null>(null);
	let message = $state('');
	let generating = $state(false);
	let sections = $state({staged: true, changes: true, history: false});
	let branchBtn = $state<HTMLButtonElement>();

	async function run(op: string, params: Record<string, unknown> = {}, label?: string) {
		if (!root) return null;
		const forRoot = root;
		if (label) busy = label;
		try {
			const state = await rpc.request<GitState>('git', {root: forRoot, op, ...params});
			if (forRoot === root) {
				git = state;
				if (state.error && label) app.toast(state.error, 'error');
			}
			return state;
		} catch (e) {
			if (label) app.toast(errorText(e), 'error');
			return null;
		} finally {
			if (label) busy = null;
		}
	}

	const refresh = () => run('refresh');

	$effect(() => {
		root;
		git = null;
		refresh();
	});

	onMount(() => {
		const timer = setInterval(() => document.visibilityState === 'visible' && app.windowFocused && refresh(), 4000);
		const onFocus = () => refresh();
		window.addEventListener('focus', onFocus);
		const off = rpc.on('turn', params => !params.running && refresh());
		return () => {
			clearInterval(timer);
			window.removeEventListener('focus', onFocus);
			off();
		};
	});

	async function showDiff(entry: GitEntry, staged: boolean) {
		const state = await run('diff', {path: entry.path, staged});
		if (state?.diff) app.openDiff(root, `${baseName(entry.path)} ${staged ? '(staged)' : '(working tree)'}`, state.diff.text, entry.path, staged);
	}

	async function showCommit(hash: string, summary: string) {
		const state = await run('show_commit', {hash});
		if (state?.diff) app.openDiff(root, `${hash.slice(0, 7)} ${summary}`, state.diff.text);
	}

	async function discard(paths: string[]) {
		const what = paths.length === 1 ? baseName(paths[0]) : `${paths.length} files`;
		if (await overlay.confirm(`Discard changes to ${what}?`, 'This cannot be undone.', 'Discard', true)) {
			run('discard', {paths}, 'Discarding');
		}
	}

	async function commit() {
		if (!git || !message.trim()) return;
		if (!git.staged.length) {
			if (!git.unstaged.length) return;
			await run('stage', {paths: git.unstaged.map(e => e.path)});
		}
		const state = await run('commit', {message: message.trim()}, 'Committing');
		if (state && !state.error) {
			message = '';
			app.toast('Committed', 'success');
		}
	}

	async function generate() {
		if (!root) return;
		generating = true;
		try {
			const result = await rpc.request('git.commit_message', {root});
			message = result.message ?? '';
		} catch (e) {
			app.toast(errorText(e), 'error');
		}
		generating = false;
	}

	function branchMenu() {
		if (!git || !branchBtn) return;
		overlay.menuAt(branchBtn, [
			{header: 'Switch branch'},
			...git.branches.slice(0, 30).map(branch => ({
				label: branch,
				checked: branch === git!.branch,
				action: () => branch !== git!.branch && run('checkout', {branch}, `Switching to ${branch}`),
			})),
			{separator: true},
			{
				label: 'New Branch…',
				icon: 'plus',
				action: async () => {
					const branch = await overlay.ask({title: 'New branch', placeholder: 'feature/name', confirm: 'Create'});
					if (branch?.trim()) run('new_branch', {branch: branch.trim()}, 'Creating branch');
				},
			},
		]);
	}

	function entryMenu(event: MouseEvent, entry: GitEntry, staged: boolean) {
		overlay.openMenu(event, [
			{label: 'Open Changes', icon: 'git', action: () => showDiff(entry, staged)},
			{label: 'Open File', icon: 'file', action: () => app.openFile(root, `${root}/${entry.path}`)},
			{separator: true},
			staged
				? {label: 'Unstage', icon: 'minus', action: () => run('unstage', {paths: [entry.path]})}
				: {label: 'Stage', icon: 'plus', action: () => run('stage', {paths: [entry.path]})},
			...(!staged ? [{label: 'Discard Changes', icon: 'undo' as const, danger: true, action: () => discard([entry.path])}] : []),
			{separator: true},
			{label: 'Copy Path', icon: 'copy', action: () => navigator.clipboard.writeText(entry.path)},
		]);
	}

	const statusClass = (s: string) => {
		const c = s.trim().charAt(0).toUpperCase();
		return c === 'U' ? 'deleted' : c === 'A' || c === '?' ? 'added' : c === 'D' ? 'deleted' : c === 'R' ? 'renamed' : 'modified';
	};
	const statusLetter = (s: string) => {
		const c = s.trim().charAt(0).toUpperCase();
		return c === '?' ? 'U' : c || 'M';
	};
	const dirOf = (path: string) => path.split('/').slice(0, -1).join('/');
	const changeCount = $derived((git?.staged.length ?? 0) + (git?.unstaged.length ?? 0));
</script>

<div class="git">
	<div class="head">
		<span class="title">Source Control</span>
		{#if root}<span class="ws" title={root}>{folderLabel(root)}</span>{/if}
		<span class="grow"></span>
		{#if busy}<span class="spinner"></span>{/if}
		<button class="icon-btn sm" title="Refresh" onclick={() => run('refresh', {}, '')}><Icon name="refresh" size={14} /></button>
		<button class="icon-btn sm" title="Close ({formatKeys('mod+shift+g')})" onclick={() => ((app.gitOpen = false), app.persistLayout())}>
			<Icon name="x" size={14} />
		</button>
	</div>

	{#if !root}
		<div class="empty">Open a folder to see its changes.</div>
	{:else if !git}
		<div class="empty"><span class="spinner"></span></div>
	{:else if !git.repo}
		<div class="empty">
			<p>This folder isn't a git repository.</p>
		</div>
	{:else}
		<div class="branch-row">
			<button class="branch" bind:this={branchBtn} onclick={branchMenu} title="Switch branch">
				<Icon name="branch" size={14} />
				<span>{git.branch || 'detached'}</span>
				<Icon name="chevronDown" size={12} />
			</button>
			<span class="grow"></span>
			<button class="sync" title="Fetch" onclick={() => run('fetch', {}, 'Fetching')}><Icon name="refresh" size={13} /></button>
			<button class="sync" title="Pull" onclick={() => run('pull', {}, 'Pulling')}>
				<Icon name="arrowDown" size={13} />{#if git.behind}<span>{git.behind}</span>{/if}
			</button>
			<button class="sync" title="Push" onclick={() => run('push', {}, 'Pushing')}>
				<Icon name="arrowUp" size={13} />{#if git.ahead}<span>{git.ahead}</span>{/if}
			</button>
		</div>

		<div class="commit">
			<div class="msg-wrap">
				<textarea
					class="field"
					rows="3"
					placeholder={git.staged.length ? 'Commit message' : changeCount ? 'Message (commits all changes)' : 'Nothing to commit'}
					bind:value={message}
					onkeydown={e => {
						if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
							e.preventDefault();
							commit();
						}
					}}
				></textarea>
				<button class="gen" title="Write the message with AI" onclick={generate} disabled={generating || !changeCount}>
					{#if generating}<span class="spinner"></span>{:else}<Icon name="sparkles" size={14} />{/if}
				</button>
			</div>
			<button class="btn primary full" disabled={!message.trim() || !changeCount || !!busy} onclick={commit}>
				<Icon name="check" size={14} />
				{git.staged.length ? `Commit ${git.staged.length} staged` : 'Commit all'}
			</button>
		</div>

		<div class="lists">
			{#if git.staged.length}
				<div class="section">
					<button class="sec-head" onclick={() => (sections.staged = !sections.staged)}>
						<span class="chev" class:folded={!sections.staged}><Icon name="chevronDown" size={12} /></span>
						Staged <span class="badge">{git.staged.length}</span>
						<span class="grow"></span>
						<span
							class="sec-act"
							role="button"
							tabindex="-1"
							title="Unstage all"
							onclick={e => (e.stopPropagation(), run('unstage', {paths: git!.staged.map(s => s.path)}))}
							onkeydown={() => {}}><Icon name="minus" size={13} /></span
						>
					</button>
					{#if sections.staged}
						<div transition:slide={{duration: 140}}>
							{#each git.staged as entry (entry.path)}
								{@render row(entry, true)}
							{/each}
						</div>
					{/if}
				</div>
			{/if}
			<div class="section">
				<button class="sec-head" onclick={() => (sections.changes = !sections.changes)}>
					<span class="chev" class:folded={!sections.changes}><Icon name="chevronDown" size={12} /></span>
					Changes <span class="badge">{git.unstaged.length}</span>
					<span class="grow"></span>
					{#if git.unstaged.length}
						<span
							class="sec-act"
							role="button"
							tabindex="-1"
							title="Discard all"
							onclick={e => (e.stopPropagation(), discard(git!.unstaged.map(s => s.path)))}
							onkeydown={() => {}}><Icon name="undo" size={13} /></span
						>
						<span
							class="sec-act"
							role="button"
							tabindex="-1"
							title="Stage all"
							onclick={e => (e.stopPropagation(), run('stage', {paths: git!.unstaged.map(s => s.path)}))}
							onkeydown={() => {}}><Icon name="plus" size={13} /></span
						>
					{/if}
				</button>
				{#if sections.changes}
					<div transition:slide={{duration: 140}}>
						{#each git.unstaged as entry (entry.path)}
							{@render row(entry, false)}
						{:else}
							<div class="clean"><Icon name="check" size={14} /> Working tree clean</div>
						{/each}
					</div>
				{/if}
			</div>
			<div class="section">
				<button class="sec-head" onclick={() => (sections.history = !sections.history)}>
					<span class="chev" class:folded={!sections.history}><Icon name="chevronDown" size={12} /></span>
					History
				</button>
				{#if sections.history}
					<div transition:slide={{duration: 140}}>
						{#each git.log as c (c.hash)}
							<button class="commit-row" onclick={() => showCommit(c.hash, c.message)} title="{c.hash}\n{c.author} · {c.date}\n\n{c.message}">
								<span class="dot"></span>
								<span class="cmsg">{c.message.split('\n')[0]}</span>
								<span class="cmeta">{c.author.split(' ')[0]} · {c.date}</span>
							</button>
						{/each}
					</div>
				{/if}
			</div>
		</div>
		{#if git.error}
			<div class="error selectable">{git.error}</div>
		{/if}
	{/if}
</div>

{#snippet row(entry: GitEntry, staged: boolean)}
	<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
	<div class="entry" onclick={() => showDiff(entry, staged)} oncontextmenu={e => entryMenu(e, entry, staged)} title={entry.path}>
		<span class="fname {statusClass(entry.status)}">{baseName(entry.path)}</span>
		<span class="fdir">{dirOf(entry.path)}</span>
		<span class="acts">
			<button class="icon-btn sm" title="Open file" onclick={e => (e.stopPropagation(), app.openFile(root, `${root}/${entry.path}`))}>
				<Icon name="file" size={13} />
			</button>
			{#if staged}
				<button class="icon-btn sm" title="Unstage" onclick={e => (e.stopPropagation(), run('unstage', {paths: [entry.path]}))}>
					<Icon name="minus" size={13} />
				</button>
			{:else}
				<button class="icon-btn sm" title="Discard" onclick={e => (e.stopPropagation(), discard([entry.path]))}>
					<Icon name="undo" size={13} />
				</button>
				<button class="icon-btn sm" title="Stage" onclick={e => (e.stopPropagation(), run('stage', {paths: [entry.path]}))}>
					<Icon name="plus" size={13} />
				</button>
			{/if}
		</span>
		<span class="status {statusClass(entry.status)}">{statusLetter(entry.status)}</span>
	</div>
{/snippet}

<style>
	.git {
		flex: 1;
		display: flex;
		flex-direction: column;
		min-height: 0;
	}
	.head {
		height: var(--titlebar);
		flex: none;
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 0 8px 0 14px;
		border-bottom: 1px solid var(--border);
	}
	.title {
		font-weight: 600;
		font-size: var(--fs-sm);
	}
	.ws {
		font-size: var(--fs-xs);
		color: var(--text-3);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.grow {
		flex: 1;
	}
	.branch-row {
		display: flex;
		align-items: center;
		gap: 2px;
		padding: 10px 10px 4px;
	}
	.branch {
		display: flex;
		align-items: center;
		gap: 6px;
		height: 28px;
		padding: 0 8px;
		border-radius: 7px;
		font-size: var(--fs-sm);
		font-weight: 550;
		min-width: 0;
	}
	.branch span {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.branch:hover {
		background: var(--bg-hover);
	}
	.sync {
		display: flex;
		align-items: center;
		gap: 3px;
		height: 26px;
		min-width: 26px;
		justify-content: center;
		padding: 0 6px;
		border-radius: 6px;
		color: var(--text-3);
		font-size: var(--fs-xs);
	}
	.sync span {
		color: var(--accent);
		font-weight: 600;
	}
	.sync:hover {
		background: var(--bg-hover);
		color: var(--text);
	}
	.commit {
		padding: 6px 12px 10px;
		display: flex;
		flex-direction: column;
		gap: 8px;
		border-bottom: 1px solid var(--border);
	}
	.msg-wrap {
		position: relative;
	}
	.msg-wrap textarea {
		resize: none;
		padding-right: 34px;
		min-height: 64px;
	}
	.gen {
		position: absolute;
		top: 6px;
		right: 6px;
		width: 24px;
		height: 24px;
		display: flex;
		align-items: center;
		justify-content: center;
		border-radius: 6px;
		color: var(--purple);
	}
	.gen:hover:not(:disabled) {
		background: var(--bg-hover);
	}
	.gen:disabled {
		opacity: 0.4;
	}
	.full {
		width: 100%;
	}
	.lists {
		flex: 1;
		overflow-y: auto;
		padding: 4px 4px 16px;
	}
	.sec-head {
		display: flex;
		align-items: center;
		gap: 6px;
		width: 100%;
		height: 28px;
		padding: 0 6px;
		border-radius: 6px;
		font-size: var(--fs-xs);
		font-weight: 650;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--text-3);
	}
	.sec-head:hover {
		background: var(--bg-hover);
	}
	.sec-act {
		display: flex;
		padding: 3px;
		border-radius: 5px;
		opacity: 0;
	}
	.sec-head:hover .sec-act {
		opacity: 1;
	}
	.sec-act:hover {
		background: var(--bg-active);
		color: var(--text);
	}
	.chev {
		display: flex;
		transition: transform 0.15s var(--ease);
	}
	.chev.folded {
		transform: rotate(-90deg);
	}
	.entry {
		display: flex;
		align-items: center;
		gap: 6px;
		height: 26px;
		padding: 0 8px 0 24px;
		border-radius: 6px;
		font-size: var(--fs-sm);
	}
	.entry:hover {
		background: var(--bg-hover);
	}
	.fname {
		flex: none;
		max-width: 60%;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.fname.deleted {
		text-decoration: line-through;
		color: var(--text-3);
	}
	.fdir {
		flex: 1;
		min-width: 0;
		font-size: var(--fs-xs);
		color: var(--text-4);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		direction: rtl;
		text-align: left;
	}
	.acts {
		display: none;
	}
	.entry:hover .acts {
		display: flex;
	}
	.status {
		width: 14px;
		text-align: center;
		font-size: var(--fs-xs);
		font-weight: 700;
		font-family: var(--font-mono);
	}
	.status.added {
		color: var(--green);
	}
	.status.modified {
		color: var(--yellow);
	}
	.status.deleted {
		color: var(--red);
	}
	.status.renamed {
		color: var(--accent);
	}
	.clean {
		display: flex;
		align-items: center;
		gap: 7px;
		padding: 6px 24px;
		font-size: var(--fs-sm);
		color: var(--text-3);
	}
	.commit-row {
		display: flex;
		align-items: center;
		gap: 8px;
		width: 100%;
		height: 28px;
		padding: 0 8px 0 18px;
		border-radius: 6px;
		text-align: left;
		font-size: var(--fs-sm);
	}
	.commit-row:hover {
		background: var(--bg-hover);
	}
	.dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		border: 1.5px solid var(--accent);
		flex: none;
	}
	.cmsg {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.cmeta {
		font-size: var(--fs-xs);
		color: var(--text-4);
		white-space: nowrap;
	}
	.error {
		margin: 8px;
		padding: 8px 10px;
		border-radius: 7px;
		background: var(--red-soft);
		color: var(--red);
		font-size: var(--fs-xs);
		white-space: pre-wrap;
	}
	.empty p {
		margin: 0;
	}
</style>
