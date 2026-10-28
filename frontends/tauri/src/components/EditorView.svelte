<script lang="ts">
	import {onDestroy, onMount} from 'svelte';
	import type * as Monaco from 'monaco-editor';
	import {app, relativeTo, type Tab} from '../lib/state.svelte';
	import {editorOptions, languageOf, loadMonaco} from '../lib/monaco';
	import {readFile, writeText, stat, isImagePath, mimeOf, isBinary} from '../lib/files';
	import {errorText} from '../lib/rpc';
	import {hooks} from '../lib/commands';
	import {overlay} from '../lib/overlay.svelte';
	import Icon from './Icon.svelte';
	import Markdown from './Markdown.svelte';

	let {tab, active, dark}: {tab: Extract<Tab, {kind: 'file'}>; active: boolean; dark: boolean} = $props();

	let host = $state<HTMLDivElement>();
	let editor: Monaco.editor.IStandaloneCodeEditor | null = null;
	let model: Monaco.editor.ITextModel | null = null;
	let monacoRef: typeof Monaco | null = null;
	let view = $state<'loading' | 'text' | 'image' | 'binary' | 'error'>('loading');
	let error = $state('');
	let image = $state('');
	let saved = '';
	let diskModified: number | null = null;
	let cursor = $state({line: 1, col: 1});
	let preview = $state(false);
	let previewText = $state('');
	const isMarkdown = $derived(/\.(md|markdown)$/i.test(tab.path));
	const language = $derived(languageOf(tab.path));

	onMount(() => {
		load();
		const onFocus = () => checkDisk();
		window.addEventListener('focus', onFocus);
		return () => window.removeEventListener('focus', onFocus);
	});

	onDestroy(() => {
		editor?.dispose();
		model?.dispose();
	});

	async function load() {
		try {
			if (isImagePath(tab.path) && !tab.path.endsWith('.svg')) {
				const file = await readFile(tab.path);
				image = `data:${mimeOf(tab.path)};base64,${file.base64}`;
				view = 'image';
				return;
			}
			const file = await readFile(tab.path, 15_000_000);
			if (isBinary(file.bytes)) {
				view = 'binary';
				return;
			}
			const text = new TextDecoder().decode(file.bytes);
			diskModified = file.modified;
			saved = text;
			const monaco = await loadMonaco();
			monacoRef = monaco;
			view = 'text';
			await Promise.resolve();
			const uri = monaco.Uri.file(tab.path);
			model = monaco.editor.getModel(uri) ?? monaco.editor.createModel(text, language, uri);
			editor = monaco.editor.create(host!, {
				...editorOptions(app.fontSize),
				model,
				theme: dark ? 'oxi-dark' : 'oxi-light',
			});
			model.onDidChangeContent(() => {
				app.dirty[tab.key] = model!.getValue() !== saved;
				if (preview) previewText = model!.getValue();
			});
			editor.onDidChangeCursorPosition(e => (cursor = {line: e.position.lineNumber, col: e.position.column}));
			editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, () => save());
			// App shortcuts that Monaco would otherwise swallow.
			editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyP, () => (app.palette = 'files'));
			editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyMod.Shift | monaco.KeyCode.KeyP, () => (app.palette = 'commands'));
			editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyK, () => (app.palette = 'commands'));
			editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyL, () => sendSelection());
			editor.addAction({
				id: 'oxi.askAboutSelection',
				label: 'Ask oxi About Selection',
				contextMenuGroupId: '0_oxi',
				keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyL],
				run: () => sendSelection(),
			});
			// Panels opening beside the editor resize it without a window resize.
			const observer = new ResizeObserver(() => editor?.layout());
			observer.observe(host!);
			editor.onDidDispose(() => observer.disconnect());
			if (tab.line) reveal(tab.line);
			if (active) editor.focus();
		} catch (e) {
			error = errorText(e);
			view = 'error';
		}
	}

	function reveal(line: number) {
		if (!editor) return;
		editor.revealLineInCenter(line);
		editor.setPosition({lineNumber: line, column: 1});
		const decorations = editor.createDecorationsCollection([
			{range: new monacoRef!.Range(line, 1, line, 1), options: {isWholeLine: true, className: 'flash-line'}},
		]);
		setTimeout(() => decorations.clear(), 1200);
	}

	$effect(() => {
		const line = tab.line;
		if (line && editor) reveal(line);
	});

	$effect(() => {
		const theme = dark ? 'oxi-dark' : 'oxi-light';
		monacoRef?.editor.setTheme(theme);
	});

	$effect(() => {
		const size = app.fontSize;
		editor?.updateOptions(editorOptions(size));
	});

	$effect(() => {
		if (active) {
			hooks.saveFile = () => save();
			setTimeout(() => editor?.focus(), 0);
		}
	});

	async function save() {
		if (!model) return;
		const text = model.getValue();
		try {
			const current = await stat(tab.path);
			if (current?.exists && diskModified && current.modified && current.modified !== diskModified) {
				const overwrite = await overlay.confirm(
					'The file changed on disk',
					`${relativeTo(tab.root, tab.path)} was changed outside the editor since you opened it. Overwrite it with your version?`,
					'Overwrite',
					true,
				);
				if (!overwrite) return;
			}
			diskModified = await writeText(tab.path, text);
			saved = text;
			app.dirty[tab.key] = false;
		} catch (e) {
			app.toast(`Save failed: ${errorText(e)}`, 'error');
		}
	}

	// Pick up changes the agent (or anything else) wrote, unless there are unsaved edits.
	async function checkDisk() {
		if (!model || view !== 'text') return;
		const current = await stat(tab.path).catch(() => null);
		if (!current?.exists || !current.modified || current.modified === diskModified) return;
		if (app.dirty[tab.key]) return;
		const file = await readFile(tab.path).catch(() => null);
		if (!file) return;
		const text = new TextDecoder().decode(file.bytes);
		diskModified = file.modified;
		saved = text;
		if (text !== model.getValue()) {
			const view = editor?.saveViewState();
			model.pushEditOperations([], [{range: model.getFullModelRange(), text}], () => null);
			if (view) editor?.restoreViewState(view);
		}
		app.dirty[tab.key] = false;
	}

	$effect(() => {
		if (!active) return;
		checkDisk();
		const timer = setInterval(checkDisk, 2500);
		return () => clearInterval(timer);
	});

	function sendSelection() {
		if (!editor || !model) return;
		const selection = editor.getSelection();
		const text = selection && !selection.isEmpty() ? model.getValueInRange(selection) : '';
		const rel = relativeTo(tab.root, tab.path);
		const lines = selection && !selection.isEmpty() ? `:${selection.startLineNumber}-${selection.endLineNumber}` : '';
		const chat = Object.values(app.chats).find(c => c.root === tab.root && app.tabs.some(t => t.kind === 'chat' && t.chat === c.key)) ?? app.newChat(tab.root);
		if (!chat) return;
		const key = `chat:${chat.key}`;
		app.activate(key);
		const quote = text ? `@${rel}${lines}\n\`\`\`${language}\n${text}\n\`\`\`\n` : `@${rel} `;
		setTimeout(() => window.dispatchEvent(new CustomEvent('oxi:insert', {detail: {chat: chat.key, text: quote}})), 50);
	}

	function togglePreview() {
		preview = !preview;
		if (preview) previewText = model?.getValue() ?? '';
	}
</script>

<div class="editor-view">
	<div class="crumbs">
		<span class="path">{relativeTo(tab.root, tab.path)}</span>
		<span class="grow"></span>
		{#if view === 'text'}
			<span class="meta">Ln {cursor.line}, Col {cursor.col}</span>
			<span class="meta">{language}</span>
			{#if isMarkdown}
				<button class="icon-btn sm" class:active={preview} onclick={togglePreview} title="Preview"><Icon name="eye" size={14} /></button>
			{/if}
			<button class="icon-btn sm" onclick={sendSelection} title="Ask oxi about the selection (⌘L)"><Icon name="sparkles" size={14} /></button>
			<button class="icon-btn sm" onclick={save} disabled={!app.dirty[tab.key]} title="Save (⌘S)"><Icon name="save" size={14} /></button>
		{/if}
	</div>
	<div class="body">
		{#if view === 'loading'}
			<div class="center"><span class="spinner"></span></div>
		{:else if view === 'image'}
			<div class="center checker"><img src={image} alt={tab.path} /></div>
		{:else if view === 'binary'}
			<div class="center muted"><Icon name="file" size={28} /> This is a binary file.</div>
		{:else if view === 'error'}
			<div class="center muted"><Icon name="alert" size={24} /> {error}</div>
		{/if}
		<div class="split" class:hidden={view !== 'text'}>
			<div class="monaco" bind:this={host}></div>
			{#if preview}
				<div class="preview"><Markdown text={previewText} root={tab.root} /></div>
			{/if}
		</div>
	</div>
</div>

<style>
	.editor-view {
		flex: 1;
		display: flex;
		flex-direction: column;
		min-height: 0;
	}
	.crumbs {
		height: 30px;
		flex: none;
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 0 10px 0 16px;
		font-size: var(--fs-xs);
		color: var(--text-3);
		border-bottom: 1px solid var(--border);
	}
	.path {
		font-family: var(--font-mono);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.grow {
		flex: 1;
	}
	.meta {
		white-space: nowrap;
	}
	.body {
		flex: 1;
		min-height: 0;
		position: relative;
	}
	.split {
		position: absolute;
		inset: 0;
		display: flex;
	}
	.split.hidden {
		visibility: hidden;
	}
	.monaco {
		flex: 1;
		min-width: 0;
		overflow: hidden;
	}
	.preview {
		flex: 1;
		min-width: 0;
		overflow-y: auto;
		padding: 24px 32px;
		border-left: 1px solid var(--border);
	}
	.center {
		position: absolute;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 10px;
		z-index: 1;
	}
	.muted {
		color: var(--text-3);
	}
	.checker {
		background-color: var(--bg-sunken);
		background-image: linear-gradient(45deg, var(--bg-hover) 25%, transparent 25%), linear-gradient(-45deg, var(--bg-hover) 25%, transparent 25%),
			linear-gradient(45deg, transparent 75%, var(--bg-hover) 75%), linear-gradient(-45deg, transparent 75%, var(--bg-hover) 75%);
		background-size: 20px 20px;
		background-position: 0 0, 0 10px, 10px -10px, -10px 0;
	}
	.checker img {
		max-width: 90%;
		max-height: 90%;
		box-shadow: var(--shadow);
	}
	:global(.flash-line) {
		background: var(--accent-soft);
		transition: background 0.6s;
	}
</style>
