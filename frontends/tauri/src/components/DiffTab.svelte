<script lang="ts">
	import {app, type Tab} from '../lib/state.svelte';
	import {rpc, errorText} from '../lib/rpc';
	import {langOf, parseDiff} from '../lib/tools';
	import DiffBlock from './DiffBlock.svelte';
	import Icon from './Icon.svelte';

	let {tab}: {tab: Extract<Tab, {kind: 'diff'}>} = $props();

	// One section per file (`diff --git a/x b/x`); a single-file diff may come without that header.
	const files = $derived.by(() => {
		const text = tab.text ?? '';
		const parts = text.split(/^(?=diff --git )/m).filter(p => p.trim());
		return parts.map(part => {
			const header = part.match(/^diff --git a\/(.+?) b\/(.+)$/m);
			const plus = part.match(/^\+\+\+ (?:b\/)?(.+)$/m);
			const path = header?.[2] ?? (plus && plus[1] !== '/dev/null' ? plus[1] : (tab.path ?? ''));
			const binary = /^Binary files/m.test(part);
			const stats = parseDiff(part);
			return {path, text: part, binary, ...stats};
		});
	});
	// A working-tree or staged diff of one file: its hunks can be staged, reverted or unstaged.
	const hunkActions = $derived(!!tab.path && tab.staged !== undefined);
	let busy = $state(false);

	type Hunk = {text: string; oldStart: number; newStart: number; oldLines: string[]; newLines: string[]};

	function hunksOf(text: string): Hunk[] {
		const out: Hunk[] = [];
		let current: Hunk | null = null;
		const rows = text.split('\n');
		if (rows[rows.length - 1] === '') rows.pop();
		for (const line of rows) {
			const header = line.match(/^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/);
			if (header) {
				current = {text: line, oldStart: Number(header[1]), newStart: Number(header[2]), oldLines: [], newLines: []};
				out.push(current);
				continue;
			}
			if (!current || line.startsWith('\\')) continue;
			current.text += '\n' + line;
			if (line.startsWith('+')) current.newLines.push(line.slice(1));
			else if (line.startsWith('-')) current.oldLines.push(line.slice(1));
			else if (line.startsWith(' ') || line === '') {
				current.oldLines.push(line.slice(1));
				current.newLines.push(line.slice(1));
			}
		}
		return out;
	}

	async function apply(hunk: Hunk, action: 'stage' | 'revert' | 'unstage') {
		if (!tab.path) return;
		// Empty-side hunks start one line later than git numbers them (`-0,0` means "before line 1").
		const at = (start: number, lines: string[]) => (lines.length ? start : start + 1);
		const edit =
			action === 'stage'
				? {path: tab.path, target: 'Index', start: at(hunk.oldStart, hunk.oldLines), expected: hunk.oldLines, replacement: hunk.newLines}
				: {
						path: tab.path,
						target: action === 'revert' ? 'WorkTree' : 'Index',
						start: at(hunk.newStart, hunk.newLines),
						expected: hunk.newLines,
						replacement: hunk.oldLines,
					};
		busy = true;
		try {
			const result = await rpc.request('fs.git', {cwd: tab.root, kind: 'apply_block', edit});
			if (result && typeof result === 'object' && 'Err' in result) throw new Error(result.Err);
			const state = await rpc.request('git', {root: tab.root, op: 'diff', path: tab.path, staged: !!tab.staged});
			const text = state?.diff?.text ?? '';
			const index = app.tabs.findIndex(t => t.key === tab.key);
			if (index >= 0) app.tabs[index] = {...tab, text};
			app.toast(action === 'stage' ? 'Staged' : action === 'unstage' ? 'Unstaged' : 'Reverted', 'success');
		} catch (e) {
			app.toast(errorText(e), 'error');
		}
		busy = false;
	}

	const message = $derived.by(() => {
		// `show_commit` puts the commit header before the first file.
		const first = (tab.text ?? '').split(/^diff --git /m)[0];
		return files.length && first.trim() && !first.startsWith('@@') && !first.startsWith('---') ? first.trim() : '';
	});
</script>

<div class="diff-tab">
	<div class="head">
		<Icon name="git" size={15} />
		<span class="title">{tab.title}</span>
		<span class="grow"></span>
		{#if tab.path}
			<button class="btn sm" onclick={() => app.openFile(tab.root, `${tab.root}/${tab.path}`)}>
				<Icon name="file" size={13} /> Open File
			</button>
		{/if}
	</div>
	<div class="scroll">
		{#if message}
			<pre class="message selectable">{message}</pre>
		{/if}
		{#each files as file}
			<div class="file">
				<div class="file-head">
					<button class="name" onclick={() => app.openFile(tab.root, `${tab.root}/${file.path}`)}>{file.path}</button>
					<span class="stats"><span class="add">+{file.added}</span> <span class="rem">−{file.removed}</span></span>
				</div>
				{#if file.binary}
					<div class="note">Binary file</div>
				{:else if file.lines.length && hunkActions}
					{#each hunksOf(file.text) as hunk}
						<div class="hunk-head">
							<span class="range">{hunk.text.split('\n')[0]}</span>
							<span class="grow"></span>
							{#if tab.staged}
								<button class="btn sm" disabled={busy} onclick={() => apply(hunk, 'unstage')}><Icon name="minus" size={12} /> Unstage</button>
							{:else}
								<button class="btn sm ghost danger" disabled={busy} onclick={() => apply(hunk, 'revert')}><Icon name="undo" size={12} /> Revert</button>
								<button class="btn sm" disabled={busy} onclick={() => apply(hunk, 'stage')}><Icon name="plus" size={12} /> Stage</button>
							{/if}
						</div>
						<DiffBlock diff={hunk.text} lang={langOf(file.path)} maxLines={2000} />
					{/each}
				{:else if file.lines.length}
					<DiffBlock diff={file.text} lang={langOf(file.path)} maxLines={2000} />
				{:else}
					<div class="note">No changes to show</div>
				{/if}
			</div>
		{:else}
			<div class="empty">No changes</div>
		{/each}
	</div>
</div>

<style>
	.diff-tab {
		flex: 1;
		display: flex;
		flex-direction: column;
		min-height: 0;
	}
	.head {
		height: 40px;
		flex: none;
		display: flex;
		align-items: center;
		gap: 9px;
		padding: 0 12px 0 18px;
		border-bottom: 1px solid var(--border);
		color: var(--text-2);
		font-size: var(--fs-sm);
	}
	.title {
		font-weight: 600;
		color: var(--text);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.grow {
		flex: 1;
	}
	.scroll {
		flex: 1;
		overflow: auto;
		padding: 16px 20px 40px;
	}
	.message {
		margin: 0 0 16px;
		padding: 12px 14px;
		border-radius: var(--radius);
		background: var(--card);
		border: 1px solid var(--border);
		font-size: var(--fs-mono);
		white-space: pre-wrap;
	}
	.file {
		border: 1px solid var(--border);
		border-radius: var(--radius);
		overflow: hidden;
		margin-bottom: 14px;
	}
	.file-head {
		display: flex;
		align-items: center;
		gap: 10px;
		height: 34px;
		padding: 0 12px;
		background: var(--card-head);
		border-bottom: 1px solid var(--border);
	}
	.name {
		flex: 1;
		text-align: left;
		font-family: var(--font-mono);
		font-size: var(--fs-mono);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.name:hover {
		color: var(--accent);
	}
	.stats {
		font-family: var(--font-mono);
		font-size: var(--fs-xs);
	}
	.add {
		color: var(--green);
	}
	.rem {
		color: var(--red);
	}
	.hunk-head {
		display: flex;
		align-items: center;
		gap: 6px;
		height: 32px;
		padding: 0 8px 0 12px;
		background: var(--bg-hover);
		border-top: 1px solid var(--border);
		border-bottom: 1px solid var(--border);
	}
	.hunk-head:first-of-type {
		border-top: none;
	}
	.range {
		font-family: var(--font-mono);
		font-size: var(--fs-xs);
		color: var(--text-3);
	}
	.note {
		padding: 12px;
		color: var(--text-3);
		font-size: var(--fs-sm);
	}
</style>
