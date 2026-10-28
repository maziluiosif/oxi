<script lang="ts" module>
	export type BlockAction = 'stage' | 'revert' | 'unstage';
	/** One run of changed lines: where it starts on each side and what it holds. */
	export type ChangeBlock = {oldStart: number; newStart: number; removed: string[]; added: string[]};
</script>

<script lang="ts">
	import {highlight} from '../lib/markdown';
	import Icon from './Icon.svelte';

	let {
		diff,
		offset = 0,
		lang = '',
		maxLines = 400,
		actions = [],
		onaction,
	}: {
		diff: string;
		offset?: number;
		lang?: string;
		maxLines?: number;
		actions?: BlockAction[];
		onaction?: (block: ChangeBlock, action: BlockAction) => void;
	} = $props();

	type Line = {type: 'add' | 'remove' | 'context'; text: string; old?: number; new?: number; block?: number};
	type Row = Line | {type: 'gap'} | {type: 'fold'; id: number; count: number};

	const CONTEXT = 3;

	const parsed = $derived.by(() => {
		const hunks: Line[][] = [];
		const blocks: ChangeBlock[] = [];
		let current: Line[] | null = null;
		let oldLine = 0;
		let newLine = 0;
		let open: ChangeBlock | null = null;
		const rows = diff.split('\n');
		if (rows[rows.length - 1] === '') rows.pop();
		for (const raw of rows) {
			const hunk = raw.match(/^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/);
			if (hunk) {
				current = [];
				hunks.push(current);
				oldLine = Number(hunk[1]) + offset;
				newLine = Number(hunk[2]) + offset;
				// `-0,0`: the file was empty; the first line is line 1.
				if (oldLine === 0) oldLine = 1;
				if (newLine === 0) newLine = 1;
				open = null;
				continue;
			}
			if (!current || raw.startsWith('\\')) continue;
			const kind = raw.startsWith('+') ? 'add' : raw.startsWith('-') ? 'remove' : raw.startsWith(' ') || raw === '' ? 'context' : null;
			if (!kind) continue;
			if (kind === 'context') {
				open = null;
				current.push({type: 'context', text: raw.slice(1), old: oldLine++, new: newLine++});
				continue;
			}
			if (!open) {
				open = {oldStart: oldLine, newStart: newLine, removed: [], added: []};
				blocks.push(open);
			}
			if (kind === 'add') {
				open.added.push(raw.slice(1));
				current.push({type: 'add', text: raw.slice(1), new: newLine++, block: blocks.length - 1});
			} else {
				open.removed.push(raw.slice(1));
				current.push({type: 'remove', text: raw.slice(1), old: oldLine++, block: blocks.length - 1});
			}
		}
		return {hunks, blocks};
	});

	let unfolded = $state<Record<number, boolean>>({});
	let showAll = $state(false);

	// Long unchanged stretches fold to a row, keeping a few lines around each change.
	const rows = $derived.by(() => {
		const out: Row[] = [];
		let foldId = 0;
		parsed.hunks.forEach((lines, h) => {
			if (h > 0) out.push({type: 'gap'});
			let i = 0;
			while (i < lines.length) {
				if (lines[i].type !== 'context') {
					out.push(lines[i++]);
					continue;
				}
				let j = i;
				while (j < lines.length && lines[j].type === 'context') j++;
				const run = lines.slice(i, j);
				const keepBefore = i === 0 ? 0 : CONTEXT;
				const keepAfter = j === lines.length ? 0 : CONTEXT;
				const id = foldId++;
				if (run.length > keepBefore + keepAfter + 2 && !unfolded[id]) {
					out.push(...run.slice(0, keepBefore));
					out.push({type: 'fold', id, count: run.length - keepBefore - keepAfter});
					out.push(...run.slice(run.length - keepAfter));
				} else out.push(...run);
				i = j;
			}
		});
		return out;
	});
	const visible = $derived(showAll ? rows : rows.slice(0, maxLines));

	const escape = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
	const html = (line: Line) => (lang ? highlight(line.text, lang) : escape(line.text));

	const ACTION_LABEL: Record<BlockAction, string> = {stage: 'Stage', revert: 'Revert', unstage: 'Unstage'};
	const ACTION_ICON = {stage: 'plus', revert: 'undo', unstage: 'minus'} as const;
	// The first row of each block carries its buttons.
	const firstOfBlock = (row: Row, index: number) =>
		'block' in row && row.block !== undefined && !(index > 0 && 'block' in visible[index - 1] && (visible[index - 1] as Line).block === row.block);
</script>

<div class="diff selectable">
	<table>
		<tbody>
			{#each visible as row, index}
				{#if row.type === 'gap'}
					<tr class="gap"><td colspan="4"><span>⋯</span></td></tr>
				{:else if row.type === 'fold'}
					<tr class="fold">
						<td colspan="4">
							<button onclick={() => (unfolded[row.id] = true)}>
								<Icon name="chevronDown" size={12} /> {row.count} unchanged {row.count === 1 ? 'line' : 'lines'}
							</button>
						</td>
					</tr>
				{:else}
					<tr class={row.type}>
						<td class="num">{row.type === 'add' ? '' : row.old}</td>
						<td class="num">{row.type === 'remove' ? '' : row.new}</td>
						<td class="sign">{row.type === 'add' ? '+' : row.type === 'remove' ? '−' : ''}</td>
						<td class="code">
							{@html html(row) || ' '}
							{#if actions.length && onaction && firstOfBlock(row, index)}
								<span class="block-actions">
									{#each actions as action}
										<button
											class:danger={action === 'revert'}
											onclick={() => onaction(parsed.blocks[(row as Line).block!], action)}
										>
											<Icon name={ACTION_ICON[action]} size={11} />{ACTION_LABEL[action]}
										</button>
									{/each}
								</span>
							{/if}
						</td>
					</tr>
				{/if}
			{/each}
		</tbody>
	</table>
	{#if rows.length > visible.length}
		<button class="more" onclick={() => (showAll = true)}>Show {rows.length - visible.length} more lines</button>
	{/if}
</div>

<style>
	.diff {
		font-family: var(--font-mono);
		font-size: var(--fs-mono);
		line-height: 1.55;
		overflow-x: auto;
		background: var(--code-bg);
	}
	table {
		border-collapse: collapse;
		min-width: 100%;
	}
	td {
		padding: 0;
		white-space: pre;
		vertical-align: top;
	}
	.num {
		width: 1%;
		min-width: 38px;
		padding: 0 6px 0 10px;
		text-align: right;
		color: var(--text-4);
		user-select: none;
		font-variant-numeric: tabular-nums;
	}
	.sign {
		width: 22px;
		padding: 0 6px 0 4px;
		text-align: center;
		color: var(--text-4);
		user-select: none;
	}
	.code {
		padding-right: 16px;
		position: relative;
	}
	tr.add {
		background: var(--diff-add);
	}
	tr.add .sign,
	tr.add .num {
		color: var(--green);
	}
	tr.remove {
		background: var(--diff-remove);
	}
	tr.remove .sign,
	tr.remove .num {
		color: var(--red);
	}
	tr.gap td {
		text-align: center;
		color: var(--text-4);
		background: var(--bg-hover);
		font-size: var(--fs-xs);
		line-height: 1.6;
	}
	tr.fold td {
		background: var(--bg-hover);
	}
	tr.fold button {
		display: flex;
		align-items: center;
		gap: 6px;
		width: 100%;
		padding: 2px 12px;
		font-family: var(--font-ui);
		font-size: var(--fs-xs);
		color: var(--text-3);
	}
	tr.fold button:hover {
		color: var(--accent);
	}
	.block-actions {
		position: absolute;
		right: 8px;
		top: 0;
		display: flex;
		gap: 4px;
		opacity: 0;
		transition: opacity 0.12s;
		font-family: var(--font-ui);
	}
	tr:hover .block-actions {
		opacity: 1;
	}
	.block-actions button {
		display: flex;
		align-items: center;
		gap: 4px;
		height: 20px;
		padding: 0 7px;
		border-radius: 5px;
		font-size: var(--fs-xs);
		background: var(--bg-raised);
		border: 1px solid var(--border-strong);
		color: var(--text-2);
		box-shadow: var(--shadow-sm);
	}
	.block-actions button:hover {
		color: var(--text);
		border-color: var(--accent);
	}
	.block-actions button.danger:hover {
		color: var(--red);
		border-color: var(--red);
	}
	.more {
		display: block;
		width: 100%;
		padding: 6px;
		font-size: var(--fs-xs);
		color: var(--accent);
		border-top: 1px solid var(--border);
		font-family: var(--font-ui);
	}
	.more:hover {
		background: var(--bg-hover);
	}
</style>
