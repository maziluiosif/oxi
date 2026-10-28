<script lang="ts">
	import {tick} from 'svelte';
	import {fade, fly, scale} from 'svelte/transition';
	import {overlay} from '../lib/overlay.svelte';
	import {app} from '../lib/state.svelte';
	import Icon from './Icon.svelte';

	let menuEl = $state<HTMLDivElement>();
	let menuPos = $state({x: 0, y: 0});
	let active = $state(-1);
	let input = $state<HTMLInputElement>();
	let value = $state('');

	// Keep the menu on screen.
	$effect(() => {
		const menu = overlay.menu;
		if (!menu) return;
		active = -1;
		menuPos = {x: menu.x, y: menu.y};
		tick().then(() => {
			if (!menuEl) return;
			const rect = menuEl.getBoundingClientRect();
			menuPos = {
				x: Math.max(6, Math.min(menu.x, window.innerWidth - rect.width - 6)),
				y: menu.y + rect.height > window.innerHeight - 6 ? Math.max(6, menu.y - rect.height) : menu.y,
			};
		});
	});

	$effect(() => {
		const prompt = overlay.prompt;
		if (!prompt) return;
		value = prompt.value ?? '';
		tick().then(() => {
			input?.focus();
			input?.select();
		});
	});

	const actionable = () =>
		(overlay.menu?.items ?? []).map((item, i) => ('action' in item && !item.disabled ? i : -1)).filter(i => i >= 0);

	function onKey(event: KeyboardEvent) {
		if (overlay.menu) {
			const indexes = actionable();
			if (event.key === 'Escape') overlay.closeMenu();
			else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
				const pos = indexes.indexOf(active);
				const step = event.key === 'ArrowDown' ? 1 : -1;
				active = indexes[(pos + step + indexes.length) % indexes.length] ?? -1;
			} else if (event.key === 'Enter' && active >= 0) {
				const item = overlay.menu.items[active];
				overlay.closeMenu();
				if ('action' in item) item.action();
			} else return;
			event.preventDefault();
			event.stopPropagation();
		}
	}

	function finish(result: string | null) {
		const prompt = overlay.prompt;
		overlay.prompt = null;
		prompt?.resolve(result);
	}

	function dismiss(id: number) {
		app.toasts = app.toasts.filter(t => t.id !== id);
	}
</script>

<svelte:window onkeydown={onKey} onblur={() => overlay.closeMenu()} />

{#if overlay.menu}
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div class="menu-scrim" onmousedown={() => overlay.closeMenu()} oncontextmenu={e => { e.preventDefault(); overlay.closeMenu(); }}></div>
	<div
		class="menu"
		role="menu"
		tabindex="-1"
		bind:this={menuEl}
		style="left:{menuPos.x}px; top:{menuPos.y}px"
		transition:scale={{start: 0.97, duration: 110}}
	>
		{#each overlay.menu.items as item, i}
			{#if 'separator' in item}
				<div class="sep"></div>
			{:else if 'header' in item}
				<div class="header">{item.header}</div>
			{:else}
				<button
					class="item"
					class:danger={item.danger}
					class:active={active === i}
					disabled={item.disabled}
					role="menuitem"
					onmouseenter={() => (active = i)}
					onclick={() => {
						overlay.closeMenu();
						item.action();
					}}
				>
					<span class="check">
						{#if item.checked}<Icon name="check" size={13} stroke={2.2} />{:else if item.icon}<Icon name={item.icon} size={14} />{/if}
					</span>
					<span class="label">{item.label}</span>
					{#if item.shortcut}<span class="shortcut">{item.shortcut}</span>{/if}
				</button>
			{/if}
		{/each}
	</div>
{/if}

{#if overlay.prompt}
	{@const prompt = overlay.prompt}
	<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
	<div class="scrim" transition:fade={{duration: 120}} onclick={() => finish(null)}>
		<div class="dialog" onclick={e => e.stopPropagation()} transition:fly={{y: 8, duration: 160}}>
			<h3>{prompt.title}</h3>
			{#if prompt.message}<p>{prompt.message}</p>{/if}
			<form
				onsubmit={e => {
					e.preventDefault();
					finish(prompt.confirmOnly ? '' : value);
				}}
			>
				{#if !prompt.confirmOnly}
					<input
						class="field"
						bind:this={input}
						bind:value
						placeholder={prompt.placeholder}
						onkeydown={e => e.key === 'Escape' && finish(null)}
					/>
				{/if}
				<div class="actions">
					<button type="button" class="btn" onclick={() => finish(null)}>Cancel</button>
					<!-- svelte-ignore a11y_autofocus -->
					<button type="submit" class="btn {prompt.danger ? 'danger solid' : 'primary'}" autofocus={prompt.confirmOnly}>
						{prompt.confirm ?? 'OK'}
					</button>
				</div>
			</form>
		</div>
	</div>
{/if}

<div class="toasts">
	{#each app.toasts as toast (toast.id)}
		<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
		<div class="toast {toast.kind}" in:fly={{y: 12, duration: 180}} out:fade={{duration: 150}} onclick={() => dismiss(toast.id)}>
			<Icon name={toast.kind === 'error' ? 'alert' : toast.kind === 'success' ? 'check' : 'info'} size={15} />
			<span>{toast.text}</span>
		</div>
	{/each}
</div>

<style>
	.menu-scrim {
		position: fixed;
		inset: 0;
		z-index: 900;
	}
	.menu {
		position: fixed;
		z-index: 901;
		min-width: 200px;
		max-width: 340px;
		padding: 5px;
		border-radius: 10px;
		background: color-mix(in srgb, var(--bg-raised) 94%, transparent);
		backdrop-filter: blur(20px) saturate(1.6);
		-webkit-backdrop-filter: blur(20px) saturate(1.6);
		border: 1px solid var(--border-strong);
		box-shadow: var(--shadow);
		transform-origin: top left;
		outline: none;
	}
	.item {
		display: flex;
		align-items: center;
		gap: 8px;
		width: 100%;
		height: 28px;
		padding: 0 10px 0 6px;
		border-radius: 6px;
		font-size: var(--fs-sm);
		text-align: left;
	}
	.item.active:not(:disabled) {
		background: var(--accent);
		color: var(--accent-text);
	}
	.item.active:not(:disabled) .shortcut,
	.item.active:not(:disabled) .check {
		color: inherit;
	}
	.item:disabled {
		opacity: 0.4;
	}
	.item.danger:not(.active) {
		color: var(--red);
	}
	.check {
		width: 16px;
		display: flex;
		justify-content: center;
		color: var(--text-3);
	}
	.label {
		flex: 1;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.shortcut {
		color: var(--text-3);
		font-size: var(--fs-xs);
	}
	.sep {
		height: 1px;
		margin: 4px 6px;
		background: var(--border);
	}
	.header {
		padding: 6px 10px 3px;
		font-size: var(--fs-xs);
		font-weight: 600;
		color: var(--text-3);
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.scrim {
		position: fixed;
		inset: 0;
		z-index: 950;
		background: var(--scrim);
		display: flex;
		align-items: flex-start;
		justify-content: center;
		padding-top: 18vh;
	}
	.dialog {
		width: 420px;
		max-width: calc(100vw - 40px);
		padding: 20px;
		border-radius: var(--radius-lg);
		background: var(--bg-raised);
		border: 1px solid var(--border-strong);
		box-shadow: var(--shadow);
	}
	.dialog h3 {
		margin: 0 0 6px;
		font-size: calc(var(--fs) + 1px);
		font-weight: 650;
	}
	.dialog p {
		margin: 0 0 14px;
		color: var(--text-2);
		font-size: var(--fs-sm);
		overflow-wrap: anywhere;
	}
	.dialog form {
		display: flex;
		flex-direction: column;
		gap: 16px;
	}
	.actions {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
	}

	.toasts {
		position: fixed;
		bottom: calc(var(--statusbar) + 14px);
		left: 50%;
		transform: translateX(-50%);
		z-index: 980;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 8px;
		pointer-events: none;
	}
	.toast {
		pointer-events: auto;
		display: flex;
		align-items: center;
		gap: 9px;
		max-width: 560px;
		padding: 9px 14px;
		border-radius: 10px;
		background: var(--bg-raised);
		border: 1px solid var(--border-strong);
		box-shadow: var(--shadow);
		font-size: var(--fs-sm);
	}
	.toast.error {
		color: var(--red);
	}
	.toast.success {
		color: var(--green);
	}
	.toast span {
		color: var(--text);
	}
</style>
