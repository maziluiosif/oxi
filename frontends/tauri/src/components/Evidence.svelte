<script lang="ts">
	import {rpc} from '../lib/rpc';
	import Icon from './Icon.svelte';

	let {report}: {report: any} = $props();

	const items: any[] = $derived(report?.items ?? []);
	const checks = $derived(items.filter(i => i.kind === 'check'));
	const visuals = $derived(items.filter(i => i.kind === 'image' && i.file));
	const passed = (item: any) => item.exit_code === 0 && !item.timed_out;

	function claimStatus(claim: any): 'attested' | 'contradicted' | 'declared' {
		const cited = items.filter(i => claim.evidence?.includes(i.id));
		if (cited.some(i => i.kind === 'check' && !passed(i))) return 'contradicted';
		if (cited.length && cited.every(i => (i.kind === 'check' ? passed(i) : i.origin === 'oxi' || i.fresh))) return 'attested';
		return 'declared';
	}

	let images = $state<Record<string, string>>({});
	let zoom = $state<string | null>(null);
	let openCheck = $state<string | null>(null);

	$effect(() => {
		for (const item of visuals) {
			if (images[item.id]) continue;
			rpc
				.request('fs.read', {path: item.file, max: 20_000_000})
				.then(r => (images[item.id] = `data:image/png;base64,${r.data}`))
				.catch(() => {});
		}
	});
</script>

{#if report && (items.length || report.claims?.length || report.requested)}
	<div class="evidence" class:stale={report.stale}>
		<div class="head">
			<Icon name="shield" size={14} />
			<span>Evidence</span>
			{#if report.stale}<span class="tag warn">stale</span>{/if}
			{#if report.requested && !report.submitted}<span class="tag warn">not submitted</span>{/if}
		</div>
		{#if report.summary}<p class="summary selectable">{report.summary}</p>{/if}
		{#if report.claims?.length}
			<ul class="claims">
				{#each report.claims as claim}
					{@const status = claimStatus(claim)}
					<li class={status}>
						<Icon name={status === 'attested' ? 'check' : status === 'contradicted' ? 'x' : 'info'} size={13} stroke={2.2} />
						<span class="selectable">{claim.text}</span>
					</li>
				{/each}
			</ul>
		{/if}
		{#if visuals.length}
			<div class="visuals">
				{#each visuals as item}
					<figure>
						{#if images[item.id]}
							<button onclick={() => (zoom = images[item.id])}><img src={images[item.id]} alt={item.label} /></button>
						{:else}
							<div class="ph"></div>
						{/if}
						<figcaption>{item.phase} · {item.label}</figcaption>
					</figure>
				{/each}
			</div>
		{/if}
		{#each checks as check}
			<div class="check">
				<button class="check-head" onclick={() => (openCheck = openCheck === check.id ? null : check.id)}>
					<span class="state" class:ok={passed(check)}>
						<Icon name={passed(check) ? 'check' : 'x'} size={12} stroke={2.4} />
					</span>
					<span class="label">{check.label || check.command}</span>
					<span class="ms">{(check.duration_ms / 1000).toFixed(1)}s</span>
				</button>
				{#if openCheck === check.id}
					<pre class="selectable">$ {check.command}
{check.output}</pre>
				{/if}
			</div>
		{/each}
	</div>
{/if}

{#if zoom}
	<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
	<div class="zoom" onclick={() => (zoom = null)}><img src={zoom} alt="evidence" /></div>
{/if}

<style>
	.evidence {
		margin: 10px 0;
		padding: 12px 14px;
		border-radius: var(--radius);
		border: 1px solid color-mix(in srgb, var(--green) 30%, var(--border));
		background: color-mix(in srgb, var(--green) 4%, var(--card));
	}
	.evidence.stale {
		border-color: color-mix(in srgb, var(--yellow) 40%, var(--border));
	}
	.head {
		display: flex;
		align-items: center;
		gap: 7px;
		font-weight: 600;
		font-size: var(--fs-sm);
		color: var(--green);
	}
	.tag {
		font-size: var(--fs-xs);
		font-weight: 500;
		padding: 0 6px;
		border-radius: 5px;
	}
	.tag.warn {
		color: var(--yellow);
		background: var(--yellow-soft);
	}
	.summary {
		margin: 8px 0;
		font-size: var(--fs-sm);
		color: var(--text-2);
	}
	.claims {
		list-style: none;
		margin: 8px 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 4px;
		font-size: var(--fs-sm);
	}
	.claims li {
		display: flex;
		gap: 8px;
		align-items: flex-start;
	}
	.claims li :global(svg) {
		margin-top: 3px;
	}
	.claims .attested :global(svg) {
		color: var(--green);
	}
	.claims .contradicted :global(svg) {
		color: var(--red);
	}
	.claims .declared :global(svg) {
		color: var(--text-3);
	}
	.visuals {
		display: flex;
		gap: 10px;
		flex-wrap: wrap;
		margin: 10px 0;
	}
	figure {
		margin: 0;
	}
	figure img,
	.ph {
		display: block;
		width: 200px;
		height: 125px;
		object-fit: cover;
		border-radius: 8px;
		border: 1px solid var(--border);
		background: var(--bg-hover);
	}
	figcaption {
		font-size: var(--fs-xs);
		color: var(--text-3);
		margin-top: 4px;
	}
	.check {
		border-top: 1px solid var(--border);
	}
	.check-head {
		display: flex;
		align-items: center;
		gap: 8px;
		width: 100%;
		padding: 6px 0;
		font-size: var(--fs-sm);
		text-align: left;
	}
	.state {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 18px;
		height: 18px;
		border-radius: 50%;
		background: var(--red-soft);
		color: var(--red);
	}
	.state.ok {
		background: var(--green-soft);
		color: var(--green);
	}
	.label {
		flex: 1;
		font-family: var(--font-mono);
		font-size: var(--fs-mono);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.ms {
		font-size: var(--fs-xs);
		color: var(--text-3);
	}
	pre {
		margin: 0 0 8px;
		padding: 8px 10px;
		border-radius: 6px;
		background: var(--code-bg);
		font-size: var(--fs-mono);
		white-space: pre-wrap;
		max-height: 260px;
		overflow: auto;
	}
	.zoom {
		position: fixed;
		inset: 0;
		z-index: 960;
		background: var(--scrim);
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 40px;
	}
	.zoom img {
		max-width: 100%;
		max-height: 100%;
		border-radius: 10px;
	}
</style>
