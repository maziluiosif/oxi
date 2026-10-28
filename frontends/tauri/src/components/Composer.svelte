<script lang="ts">
	import {tick} from 'svelte';
	import {fly} from 'svelte/transition';
	import {open as openDialog} from '@tauri-apps/plugin-dialog';
	import {app, baseName, type Chat} from '../lib/state.svelte';
	import {rpc, errorText} from '../lib/rpc';
	import {overlay} from '../lib/overlay.svelte';
	import {fileList, readFile, isImagePath, mimeOf, isBinary} from '../lib/files';
	import {formatKeys} from '../lib/commands';
	import {fuzzy, formatDuration} from '../lib/tools';
	import type {Attachment} from '../lib/types';
	import Icon from './Icon.svelte';
	import Picker, {type PickerItem} from './Picker.svelte';

	let {chat}: {chat: Chat} = $props();

	let textarea = $state<HTMLTextAreaElement>();
	let text = $state(localStorage.getItem(`oxi.draft:${chat.key}`) ?? '');
	let attachments = $state<Attachment[]>([]);
	let dragging = $state(false);
	let sending = $state(false);

	export function focus() {
		textarea?.focus();
	}

	export function setText(value: string) {
		text = value;
		tick().then(() => {
			grow();
			textarea?.focus();
			textarea?.setSelectionRange(text.length, text.length);
		});
	}

	// Drafts survive switching chats and restarts; a draft chat's key changes when it's sent.
	let draftKey = chat.key;
	$effect(() => {
		const key = chat.key;
		if (key !== draftKey) {
			localStorage.removeItem(`oxi.draft:${draftKey}`);
			draftKey = key;
		}
		if (text.trim()) localStorage.setItem(`oxi.draft:${key}`, text);
		else localStorage.removeItem(`oxi.draft:${key}`);
	});

	// "Ask oxi about the selection" in the editor.
	$effect(() => {
		const onInsert = (event: Event) => {
			const detail = (event as CustomEvent).detail;
			if (detail?.chat !== chat.key) return;
			setText((text.trim() ? text.trimEnd() + '\n\n' : '') + detail.text);
		};
		window.addEventListener('oxi:insert', onInsert);
		return () => window.removeEventListener('oxi:insert', onInsert);
	});

	function grow() {
		if (!textarea) return;
		textarea.style.height = 'auto';
		textarea.style.height = Math.min(textarea.scrollHeight, window.innerHeight * 0.4) + 'px';
	}
	$effect(() => {
		text;
		tick().then(grow);
	});

	const canSend = $derived((text.trim().length > 0 || attachments.length > 0) && app.connection === 'connected');

	async function send() {
		if (!canSend || sending) return;
		const slash = text.trim().match(/^\/(\w+)\s*(.*)$/s);
		if (slash && runSlash(slash[1], slash[2])) return;
		sending = true;
		const body = text.trim();
		const files = attachments;
		text = '';
		attachments = [];
		history = null;
		const ok = await app.send(chat, body, files);
		if (!ok) {
			text = body;
			attachments = files;
		}
		sending = false;
	}

	// ---------- slash commands ----------
	const SLASH: {name: string; detail: string; run: (arg: string) => void}[] = [
		{name: 'new', detail: 'Start a new chat', run: () => app.newChat(chat.root)},
		{name: 'plan', detail: 'Toggle plan mode', run: () => (chat.plan = !chat.plan)},
		{name: 'evidence', detail: 'Toggle evidence mode', run: () => (chat.evidence = !chat.evidence)},
		{name: 'model', detail: 'Pick the model', run: () => openModels()},
		{name: 'provider', detail: 'Pick the provider', run: () => openProviders()},
		{
			name: 'rename',
			detail: 'Rename this chat',
			run: arg => {
				if (chat.id && arg.trim()) app.renameChat(chat.root, chat.id, arg.trim());
			},
		},
		{name: 'settings', detail: 'Open settings', run: () => (app.settingsOpen = true)},
		{name: 'terminal', detail: 'Toggle the terminal', run: () => (app.terminalOpen = !app.terminalOpen)},
		{name: 'git', detail: 'Toggle source control', run: () => (app.gitOpen = !app.gitOpen)},
	];

	function runSlash(name: string, arg: string): boolean {
		const command = SLASH.find(c => c.name === name);
		if (!command) return false;
		text = '';
		command.run(arg);
		return true;
	}

	// ---------- @ mentions and / menu ----------
	type Suggest = {kind: 'file' | 'slash'; start: number; query: string; items: {value: string; label: string; detail?: string}[]};
	let suggest = $state<Suggest | null>(null);
	let suggestIndex = $state(0);

	async function updateSuggest() {
		if (!textarea) return;
		const caret = textarea.selectionStart;
		const before = text.slice(0, caret);
		const slash = before.match(/^\/(\w*)$/);
		if (slash) {
			const q = slash[1];
			const items = SLASH.filter(c => c.name.startsWith(q)).map(c => ({value: c.name, label: '/' + c.name, detail: c.detail}));
			suggest = items.length ? {kind: 'slash', start: 0, query: q, items} : null;
			suggestIndex = 0;
			return;
		}
		const at = before.match(/(^|\s)@([^\s@]*)$/);
		if (!at) {
			suggest = null;
			return;
		}
		const query = at[2];
		const start = caret - query.length - 1;
		const files = await fileList(chat.root);
		const ranked = (query ? files.map(f => ({f, m: fuzzy(query, f)})).filter(x => x.m) : files.map(f => ({f, m: {score: 0}})))
			.sort((a, b) => b.m!.score - a.m!.score)
			.slice(0, 40)
			.map(x => ({value: x.f, label: baseName(x.f), detail: x.f}));
		suggest = ranked.length ? {kind: 'file', start, query, items: ranked} : null;
		suggestIndex = 0;
	}

	function acceptSuggest(index = suggestIndex) {
		if (!suggest || !textarea) return;
		const item = suggest.items[index];
		if (!item) return;
		if (suggest.kind === 'slash') {
			text = '';
			suggest = null;
			runSlash(item.value, '');
			return;
		}
		const caret = textarea.selectionStart;
		const insert = `@${item.value} `;
		text = text.slice(0, suggest.start) + insert + text.slice(caret);
		const pos = suggest.start + insert.length;
		suggest = null;
		tick().then(() => textarea?.setSelectionRange(pos, pos));
	}

	// ---------- history (↑ on an empty box) ----------
	let history = $state<number | null>(null);
	const sent = $derived(chat.messages.filter(m => m.role === 'user' && !m.summary).map(m => (m as any).text as string));

	function onKeydown(event: KeyboardEvent) {
		if (suggest) {
			if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
				const n = suggest.items.length;
				suggestIndex = (suggestIndex + (event.key === 'ArrowDown' ? 1 : -1) + n) % n;
				event.preventDefault();
				return;
			}
			if (event.key === 'Enter' || event.key === 'Tab') {
				event.preventDefault();
				acceptSuggest();
				return;
			}
			if (event.key === 'Escape') {
				suggest = null;
				event.preventDefault();
				event.stopPropagation();
				return;
			}
		}
		if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
			event.preventDefault();
			send();
			return;
		}
		if (event.key === 'Escape' && chat.running && !text) {
			event.preventDefault();
			app.cancel(chat);
			return;
		}
		if (event.key === 'ArrowUp' && (history !== null || !text) && textarea?.selectionStart === 0 && sent.length) {
			event.preventDefault();
			history = history === null ? sent.length - 1 : Math.max(0, history - 1);
			text = sent[history];
			return;
		}
		if (event.key === 'ArrowDown' && history !== null && textarea?.selectionStart === text.length) {
			event.preventDefault();
			history = history + 1 < sent.length ? history + 1 : null;
			text = history === null ? '' : sent[history];
		}
	}

	// ---------- attachments ----------
	async function addFile(file: File) {
		if (file.type.startsWith('image/')) {
			const data = await blobBase64(file);
			attachments = [...attachments, {kind: 'image', mime: file.type, data, name: file.name || 'image.png'}];
		} else {
			const bytes = new Uint8Array(await file.arrayBuffer());
			if (isBinary(bytes)) {
				app.toast(`${file.name} isn't text or an image`, 'error');
				return;
			}
			attachments = [...attachments, {kind: 'text', name: file.name, text: new TextDecoder().decode(bytes)}];
		}
	}

	function blobBase64(blob: Blob): Promise<string> {
		return new Promise(resolve => {
			const reader = new FileReader();
			reader.onload = () => resolve(String(reader.result).split(',')[1] ?? '');
			reader.readAsDataURL(blob);
		});
	}

	async function onPaste(event: ClipboardEvent) {
		const files = [...(event.clipboardData?.files ?? [])];
		if (files.length) {
			event.preventDefault();
			for (const file of files) await addFile(file);
			return;
		}
		// Long pastes become an attachment instead of filling the box.
		const pasted = event.clipboardData?.getData('text/plain') ?? '';
		if (pasted.length > 6000 || pasted.split('\n').length > 120) {
			event.preventDefault();
			attachments = [...attachments, {kind: 'text', name: `Pasted text (${pasted.split('\n').length} lines)`, text: pasted}];
		}
	}

	async function onDrop(event: DragEvent) {
		event.preventDefault();
		dragging = false;
		for (const file of event.dataTransfer?.files ?? []) await addFile(file);
	}

	async function pickFiles() {
		const picked = await openDialog({multiple: true, defaultPath: chat.root, title: 'Attach files'});
		const paths = Array.isArray(picked) ? picked : picked ? [picked] : [];
		for (const path of paths) {
			try {
				const file = await readFile(path, 20_000_000);
				if (isImagePath(path)) {
					attachments = [...attachments, {kind: 'image', mime: mimeOf(path), data: file.base64, name: baseName(path)}];
				} else if (isBinary(file.bytes)) {
					app.toast(`${baseName(path)} isn't text or an image`, 'error');
				} else {
					attachments = [...attachments, {kind: 'text', name: baseName(path), text: new TextDecoder().decode(file.bytes)}];
				}
			} catch (e) {
				app.toast(errorText(e), 'error');
			}
		}
	}

	// ---------- provider / model / effort ----------
	type Pick = {anchor: DOMRect; kind: 'provider' | 'model'; items: PickerItem[]; loading: boolean};
	let pick = $state<Pick | null>(null);
	let providerBtn = $state<HTMLButtonElement>();
	let modelBtn = $state<HTMLButtonElement>();
	let effortBtn = $state<HTMLButtonElement>();

	const provider = $derived(app.providers.find(p => p.id === chat.config?.provider));
	const isRouter = $derived(chat.config?.provider === 'router');
	const EFFORT_FULL = ['', 'low', 'medium', 'high', 'xhigh', 'max'];
	const EFFORT_SHORT = ['', 'low', 'medium', 'high'];
	const effortValues = $derived.by(() => {
		const p = chat.config?.provider ?? '';
		if (['customanthropic', 'claudecodeacp'].includes(p)) return EFFORT_FULL;
		if (['openai', 'gptcodex', 'opencodego', 'azureopenai', 'codexacp', 'cursoracp', 'musecodeacp'].includes(p)) return EFFORT_SHORT;
		return null;
	});
	const ROUTER: PickerItem[] = [
		{value: 'balanced', label: 'Balanced', detail: 'Subscription quota first, then the cheapest model that fits'},
		{value: 'subscriptions-first', label: 'Subscriptions first', detail: 'Spend subscription windows before pay-per-use'},
		{value: 'cheapest', label: 'Cheapest', detail: 'Lowest cost that still clears the quality bar'},
		{value: 'best-quality', label: 'Best quality', detail: 'Strongest model available'},
		{value: 'local-first', label: 'Local first', detail: 'Stay on local models unless the task needs more'},
	];

	function openProviders() {
		if (!providerBtn) return;
		pick = {
			anchor: providerBtn.getBoundingClientRect(),
			kind: 'provider',
			loading: false,
			items: app.providers.map(p => ({
				value: p.id,
				label: p.label,
				detail: p.description.replace(/\s*\(.*\)$/, '') === p.label ? undefined : p.description,
				group: p.acp ? 'Agents' : 'Models',
			})),
		};
	}

	const modelCache = new Map<string, string[]>();

	async function openModels() {
		if (!modelBtn || !chat.config) return;
		const config = chat.config;
		const anchor = modelBtn.getBoundingClientRect();
		if (isRouter) {
			pick = {anchor, kind: 'model', loading: false, items: ROUTER};
			return;
		}
		const cached = modelCache.get(config.provider);
		const toItems = (models: string[]) => models.map(m => ({value: m, label: m}));
		pick = {anchor, kind: 'model', loading: !cached, items: toItems(cached ?? [config.model].filter(Boolean))};
		try {
			const result = await rpc.request('models.list', {root: chat.root, provider: config.provider, chat: chat.id ?? undefined});
			const models: string[] = (result.models ?? []).map((m: any) => (typeof m === 'string' ? m : (m.id ?? m.name)));
			const list = models.length ? models : [config.model].filter(Boolean);
			modelCache.set(config.provider, list);
			if (pick?.kind === 'model') pick = {...pick, loading: false, items: toItems(list)};
		} catch (e) {
			if (pick?.kind === 'model') pick = {...pick, loading: false};
		}
	}

	function onPick(value: string) {
		const kind = pick?.kind;
		pick = null;
		if (kind === 'provider') app.setConfig(chat, {provider: value});
		else if (kind === 'model') app.setConfig(chat, {model: value});
		focus();
	}

	function openEffort() {
		if (!effortBtn || !effortValues) return;
		overlay.menuAt(
			effortBtn,
			effortValues.map(value => ({
				label: value ? value.charAt(0).toUpperCase() + value.slice(1) : 'Auto',
				checked: (chat.config?.effort ?? '') === value,
				action: () => app.setConfig(chat, {effort: value}),
			})),
		);
	}

	const modelLabel = $derived.by(() => {
		const model = chat.config?.model ?? '';
		if (isRouter) return ROUTER.find(r => r.value === model)?.label ?? model;
		return model.length > 28 ? model.slice(0, 27) + '…' : model || 'default';
	});

	// ---------- context & quota ----------
	const contextPct = $derived(chat.context && chat.context.size ? Math.min(1, chat.context.used / chat.context.size) : 0);
	const quota = $derived(chat.config ? app.quota[chat.config.provider] : undefined);
	const quotaWindow = $derived(quota?.windows?.length ? quota.windows.reduce((a, b) => (b.used_pct > a.used_pct ? b : a)) : null);
	const fmtTokens = (n: number) => (n >= 1000 ? `${Math.round(n / 1000)}k` : String(n));
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
	class="composer"
	class:dragging
	class:plan={chat.plan}
	ondragover={e => {
		e.preventDefault();
		dragging = true;
	}}
	ondragleave={() => (dragging = false)}
	ondrop={onDrop}
>
	{#if suggest}
		<div class="suggest" transition:fly={{y: 6, duration: 120}}>
			{#each suggest.items as item, i (item.value)}
				<button class="sitem" class:active={i === suggestIndex} onmouseenter={() => (suggestIndex = i)} onmousedown={e => (e.preventDefault(), acceptSuggest(i))}>
					<Icon name={suggest.kind === 'file' ? 'file' : 'command'} size={13} />
					<span class="slabel">{item.label}</span>
					{#if item.detail}<span class="sdetail">{item.detail}</span>{/if}
				</button>
			{/each}
		</div>
	{/if}

	{#if attachments.length}
		<div class="attachments">
			{#each attachments as attachment, i}
				<div class="att" transition:fly={{y: 4, duration: 120}}>
					{#if attachment.kind === 'image'}
						<img src="data:{attachment.mime};base64,{attachment.data}" alt={attachment.name} />
					{:else}
						<Icon name="fileText" size={14} />
					{/if}
					<span class="att-name">{attachment.name}</span>
					<button class="icon-btn sm" onclick={() => (attachments = attachments.filter((_, j) => j !== i))} title="Remove">
						<Icon name="x" size={11} />
					</button>
				</div>
			{/each}
		</div>
	{/if}

	<textarea
		bind:this={textarea}
		bind:value={text}
		rows="1"
		placeholder={chat.running ? 'Queue a follow-up…' : chat.plan ? 'Describe what to plan…' : 'Message oxi…  @ to mention files, / for commands'}
		onkeydown={onKeydown}
		oninput={updateSuggest}
		onclick={updateSuggest}
		onpaste={onPaste}
		onblur={() => setTimeout(() => (suggest = null), 120)}
		spellcheck="true"
	></textarea>

	<div class="bar">
		<button class="icon-btn" title="Attach files" onclick={pickFiles}><Icon name="paperclip" size={16} /></button>
		<button class="sel" bind:this={providerBtn} onclick={openProviders} title="Provider">
			<span>{provider?.label ?? chat.config?.label ?? 'Provider'}</span>
			<Icon name="chevronDown" size={12} />
		</button>
		<button class="sel" bind:this={modelBtn} onclick={openModels} title={chat.config?.model}>
			<span class="model">{modelLabel}</span>
			<Icon name="chevronDown" size={12} />
		</button>
		{#if effortValues}
			<button class="sel" bind:this={effortBtn} onclick={openEffort} title="Reasoning effort">
				<Icon name="brain" size={13} />
				<span>{chat.config?.effort ? chat.config.effort.charAt(0).toUpperCase() + chat.config.effort.slice(1) : 'Auto'}</span>
			</button>
		{/if}
		<button class="chip" class:on={chat.plan} onclick={() => (chat.plan = !chat.plan)} title="Plan mode: the agent plans before it edits ({formatKeys('shift+tab')})">
			<Icon name="map" size={13} /> Plan
		</button>
		<button class="chip ev" class:on={chat.evidence} onclick={() => (chat.evidence = !chat.evidence)} title="Evidence mode: prove changes with checks and captures">
			<Icon name="shield" size={13} /> Evidence
		</button>

		<span class="grow"></span>

		{#if chat.queued}
			<span class="queued" title="Prompts waiting for the current turn">{chat.queued} queued</span>
		{/if}
		{#if quotaWindow}
			<span
				class="quota"
				class:warn={quotaWindow.used_pct >= 70}
				class:bad={quotaWindow.used_pct >= 90}
				title={quota!.windows.map(w => `${w.label}: ${Math.round(w.used_pct)}%${w.resets_in ? ` · resets in ${formatDuration(w.resets_in)}` : ''}`).join('\n')}
			>
				{quotaWindow.label} {Math.round(quotaWindow.used_pct)}%
			</span>
		{/if}
		{#if chat.context && chat.context.size}
			<span class="ctx" title="Context: {fmtTokens(chat.context.used)} of {fmtTokens(chat.context.size)} tokens">
				<svg width="18" height="18" viewBox="0 0 20 20">
					<circle cx="10" cy="10" r="7.5" class="ring-bg" />
					<circle
						cx="10"
						cy="10"
						r="7.5"
						class="ring"
						class:warn={contextPct > 0.8}
						stroke-dasharray="{contextPct * 47.1} 47.1"
						transform="rotate(-90 10 10)"
					/>
				</svg>
			</span>
		{/if}
		{#if chat.running && !text.trim() && !attachments.length}
			<button class="send stop" onclick={() => app.cancel(chat)} title="Stop ({formatKeys('mod+.')})">
				<Icon name="stop" size={14} stroke={2.5} />
			</button>
		{:else}
			<button class="send" disabled={!canSend} onclick={send} title={chat.running ? 'Queue' : 'Send (↩)'}>
				<Icon name="arrowUp" size={16} stroke={2.4} />
			</button>
		{/if}
	</div>
</div>

{#if pick}
	<Picker
		anchor={pick.anchor}
		items={pick.items}
		loading={pick.loading}
		selected={pick.kind === 'provider' ? chat.config?.provider : chat.config?.model}
		placeholder={pick.kind === 'provider' ? 'Search providers' : 'Search or type a model id'}
		custom={pick.kind === 'model' && !isRouter}
		onpick={onPick}
		onclose={() => ((pick = null), focus())}
	/>
{/if}

<style>
	.composer {
		position: relative;
		border-radius: 16px;
		border: 1px solid var(--border-strong);
		background: var(--input);
		box-shadow: var(--shadow-sm);
		transition: border-color 0.15s, box-shadow 0.15s;
	}
	.composer:focus-within {
		border-color: color-mix(in srgb, var(--accent) 55%, var(--border-strong));
		box-shadow: 0 0 0 3px var(--accent-soft), var(--shadow-sm);
	}
	.composer.plan {
		border-color: color-mix(in srgb, var(--purple) 50%, var(--border-strong));
	}
	.composer.dragging {
		border-color: var(--accent);
		border-style: dashed;
	}
	textarea {
		display: block;
		width: 100%;
		min-height: 52px;
		max-height: 40vh;
		padding: 14px 16px 4px;
		border: none;
		outline: none;
		resize: none;
		background: none;
		font-size: var(--fs);
		line-height: 1.5;
		color: var(--text);
		user-select: text;
		-webkit-user-select: text;
	}
	textarea::placeholder {
		color: var(--text-4);
	}
	.bar {
		display: flex;
		align-items: center;
		gap: 4px;
		padding: 6px 8px 8px;
		min-width: 0;
	}
	.sel {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		height: 28px;
		padding: 0 8px;
		border-radius: 7px;
		font-size: var(--fs-xs);
		color: var(--text-2);
		white-space: nowrap;
		min-width: 0;
		transition: background 0.12s, color 0.12s;
	}
	.sel span {
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.sel:hover {
		background: var(--bg-hover);
		color: var(--text);
	}
	.model {
		font-family: var(--font-mono);
		font-size: calc(var(--fs-xs) - 0.5px);
	}
	.chip {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		height: 26px;
		padding: 0 9px;
		border-radius: 13px;
		font-size: var(--fs-xs);
		color: var(--text-3);
		border: 1px solid transparent;
		transition: all 0.12s;
		white-space: nowrap;
	}
	.chip:hover {
		background: var(--bg-hover);
		color: var(--text-2);
	}
	.chip.on {
		color: var(--purple);
		background: color-mix(in srgb, var(--purple) 12%, transparent);
		border-color: color-mix(in srgb, var(--purple) 30%, transparent);
	}
	.chip.ev.on {
		color: var(--green);
		background: var(--green-soft);
		border-color: color-mix(in srgb, var(--green) 30%, transparent);
	}
	.grow {
		flex: 1;
	}
	.queued,
	.quota {
		font-size: var(--fs-xs);
		color: var(--text-3);
		padding: 0 6px;
		white-space: nowrap;
	}
	.quota.warn {
		color: var(--yellow);
	}
	.quota.bad {
		color: var(--red);
	}
	.ctx {
		display: flex;
		padding: 0 4px;
	}
	.ring-bg {
		fill: none;
		stroke: var(--bg-active);
		stroke-width: 2.4;
	}
	.ring {
		fill: none;
		stroke: var(--text-3);
		stroke-width: 2.4;
		stroke-linecap: round;
		transition: stroke-dasharray 0.4s var(--ease);
	}
	.ring.warn {
		stroke: var(--yellow);
	}
	.send {
		width: 32px;
		height: 32px;
		border-radius: 50%;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--accent);
		color: var(--accent-text);
		flex: none;
		transition: transform 0.1s, background 0.15s, opacity 0.15s;
	}
	.send:hover:not(:disabled) {
		background: var(--accent-strong);
	}
	.send:active:not(:disabled) {
		transform: scale(0.94);
	}
	.send:disabled {
		opacity: 0.3;
	}
	.send.stop {
		background: var(--text);
		color: var(--bg);
	}
	.attachments {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
		padding: 10px 12px 0;
	}
	.att {
		display: flex;
		align-items: center;
		gap: 6px;
		height: 32px;
		padding: 0 2px 0 6px;
		border-radius: 8px;
		background: var(--bg-hover);
		border: 1px solid var(--border);
		font-size: var(--fs-xs);
		max-width: 240px;
	}
	.att img {
		width: 24px;
		height: 24px;
		object-fit: cover;
		border-radius: 4px;
	}
	.att-name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.suggest {
		position: absolute;
		left: 8px;
		right: 8px;
		bottom: calc(100% + 6px);
		max-height: 280px;
		overflow-y: auto;
		padding: 5px;
		border-radius: 12px;
		background: var(--bg-raised);
		border: 1px solid var(--border-strong);
		box-shadow: var(--shadow);
		z-index: 30;
	}
	.sitem {
		display: flex;
		align-items: center;
		gap: 8px;
		width: 100%;
		height: 30px;
		padding: 0 8px;
		border-radius: 7px;
		font-size: var(--fs-sm);
		color: var(--text-2);
		text-align: left;
	}
	.sitem.active {
		background: var(--bg-active);
		color: var(--text);
	}
	.slabel {
		flex: none;
	}
	.sdetail {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--text-3);
		font-size: var(--fs-xs);
		text-align: right;
		direction: rtl;
	}
</style>
