<script lang="ts">
	import {app, baseName, folderLabel} from '../lib/state.svelte';
	import {rpc, errorText} from '../lib/rpc';
	import Icon from './Icon.svelte';

	type Hit = {start: number; end: number; line: number; hlStart: number; hlEnd: number; preview: string};
	type FileHits = {display: string; hits: Hit[]};

	const root = $derived(app.activeRoot);
	let query = $state('');
	let include = $state('');
	let regex = $state(false);
	let caseSensitive = $state(false);
	let wholeWord = $state(false);
	let results = $state<FileHits[]>([]);
	let truncated = $state(false);
	let searching = $state(false);
	let error = $state('');
	let folded = $state<Record<string, boolean>>({});
	let input = $state<HTMLInputElement>();
	let seq = 0;

	$effect(() => {
		input?.focus();
	});

	let timer: ReturnType<typeof setTimeout>;
	$effect(() => {
		// Search as you type, a moment after the last key.
		const params = {query, include, regex, caseSensitive, wholeWord, root};
		clearTimeout(timer);
		timer = setTimeout(() => search(params), 280);
	});

	async function search(p: {query: string; include: string; regex: boolean; caseSensitive: boolean; wholeWord: boolean; root: string}) {
		const id = ++seq;
		error = '';
		if (!p.query.trim() || !p.root) {
			results = [];
			searching = false;
			return;
		}
		searching = true;
		try {
			const result = await rpc.request('fs.search', {
				root: p.root,
				query: p.query,
				regex: p.regex,
				case_sensitive: p.caseSensitive,
				whole_word: p.wholeWord,
				include: p.include,
			});
			if (id !== seq) return;
			results = (result.files ?? []).map((f: any) => ({
				display: f.display,
				hits: f.hits.map((h: any[]) => ({start: h[0], end: h[1], line: h[2], hlStart: h[3], hlEnd: h[4], preview: h[5]})),
			}));
			truncated = !!result.truncated;
		} catch (e) {
			if (id === seq) error = errorText(e);
		} finally {
			if (id === seq) searching = false;
		}
	}

	const total = $derived(results.reduce((n, f) => n + f.hits.length, 0));

	function open(file: FileHits, hit: Hit) {
		app.openFile(root, `${root}/${file.display}`, hit.line);
	}

	// The preview's highlight offsets are byte offsets into a UTF-8 line.
	function parts(hit: Hit): [string, string, string] {
		const bytes = new TextEncoder().encode(hit.preview);
		const dec = new TextDecoder();
		return [dec.decode(bytes.slice(0, hit.hlStart)), dec.decode(bytes.slice(hit.hlStart, hit.hlEnd)), dec.decode(bytes.slice(hit.hlEnd))];
	}
</script>

<div class="search">
	<div class="box">
		<div class="field-row">
			<Icon name="search" size={14} />
			<input bind:this={input} bind:value={query} placeholder="Search in {root ? folderLabel(root) : 'files'}" spellcheck="false" />
			<button class="opt" class:on={caseSensitive} onclick={() => (caseSensitive = !caseSensitive)} title="Match case">Aa</button>
			<button class="opt" class:on={wholeWord} onclick={() => (wholeWord = !wholeWord)} title="Whole word"><u>ab</u></button>
			<button class="opt" class:on={regex} onclick={() => (regex = !regex)} title="Regular expression">.*</button>
		</div>
		<input class="include field" bind:value={include} placeholder="Files to include (e.g. src/**/*.rs)" spellcheck="false" />
	</div>
	<div class="summary">
		{#if searching}<span class="spinner"></span> Searching…
		{:else if error}<span class="err">{error}</span>
		{:else if query.trim()}{total} {total === 1 ? 'result' : 'results'} in {results.length} {results.length === 1 ? 'file' : 'files'}{truncated ? ' (more not shown)' : ''}
		{/if}
	</div>
	<div class="results">
		{#each results as file (file.display)}
			<button class="file" onclick={() => (folded[file.display] = !folded[file.display])} title={file.display}>
				<span class="chev" class:folded={folded[file.display]}><Icon name="chevronDown" size={12} /></span>
				<span class="fname">{baseName(file.display)}</span>
				<span class="fdir">{file.display.split('/').slice(0, -1).join('/')}</span>
				<span class="badge">{file.hits.length}</span>
			</button>
			{#if !folded[file.display]}
				{#each file.hits as hit}
					{@const [a, b, c] = parts(hit)}
					<button class="hit" onclick={() => open(file, hit)}>
						<span class="ln">{hit.line}</span>
						<span class="pv">{a.trimStart()}<mark>{b}</mark>{c}</span>
					</button>
				{/each}
			{/if}
		{/each}
	</div>
</div>

<style>
	.search {
		flex: 1;
		display: flex;
		flex-direction: column;
		min-height: 0;
	}
	.box {
		padding: 0 10px 6px;
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.field-row {
		display: flex;
		align-items: center;
		gap: 6px;
		height: 32px;
		padding: 0 4px 0 9px;
		border-radius: 7px;
		border: 1px solid var(--border-strong);
		background: var(--input);
		color: var(--text-3);
	}
	.field-row:focus-within {
		border-color: var(--accent);
		box-shadow: 0 0 0 3px var(--accent-soft);
	}
	.field-row input {
		flex: 1;
		min-width: 0;
		border: none;
		outline: none;
		background: none;
		font-size: var(--fs-sm);
		color: var(--text);
	}
	.opt {
		width: 24px;
		height: 22px;
		border-radius: 5px;
		font-size: 11px;
		font-family: var(--font-mono);
		color: var(--text-3);
	}
	.opt:hover {
		background: var(--bg-hover);
	}
	.opt.on {
		background: var(--accent-soft);
		color: var(--accent);
	}
	.include {
		height: 28px;
		font-size: var(--fs-xs);
	}
	.summary {
		display: flex;
		align-items: center;
		gap: 7px;
		padding: 2px 14px 6px;
		font-size: var(--fs-xs);
		color: var(--text-3);
		min-height: 22px;
	}
	.err {
		color: var(--red);
	}
	.results {
		flex: 1;
		overflow-y: auto;
		padding: 0 6px 12px;
	}
	.file {
		display: flex;
		align-items: center;
		gap: 6px;
		width: 100%;
		height: 26px;
		padding: 0 6px;
		border-radius: 6px;
		font-size: var(--fs-sm);
		text-align: left;
	}
	.file:hover {
		background: var(--bg-hover);
	}
	.chev {
		display: flex;
		color: var(--text-4);
		transition: transform 0.15s;
	}
	.chev.folded {
		transform: rotate(-90deg);
	}
	.fname {
		flex: none;
		font-weight: 550;
	}
	.fdir {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--fs-xs);
		color: var(--text-4);
	}
	.hit {
		display: flex;
		align-items: baseline;
		gap: 8px;
		width: 100%;
		padding: 3px 8px 3px 26px;
		border-radius: 6px;
		text-align: left;
		font-size: var(--fs-xs);
		color: var(--text-2);
	}
	.hit:hover {
		background: var(--bg-hover);
		color: var(--text);
	}
	.ln {
		flex: none;
		min-width: 24px;
		color: var(--text-4);
		font-variant-numeric: tabular-nums;
		text-align: right;
	}
	.pv {
		font-family: var(--font-mono);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	mark {
		background: var(--yellow-soft);
		color: var(--text);
		border-radius: 3px;
		outline: 1px solid color-mix(in srgb, var(--yellow) 40%, transparent);
	}
</style>
