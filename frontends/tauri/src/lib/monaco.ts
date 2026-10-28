// Monaco, loaded on first use (it's most of the bundle) with its web workers and oxi's themes.

import type * as Monaco from 'monaco-editor';

let loading: Promise<typeof Monaco> | null = null;

export function loadMonaco(): Promise<typeof Monaco> {
	if (loading) return loading;
	loading = (async () => {
		const [monaco, editorWorker, jsonWorker, cssWorker, htmlWorker, tsWorker] = await Promise.all([
			import('monaco-editor'),
			import('monaco-editor/editor/editor.worker?worker'),
			import('monaco-editor/language/json/json.worker?worker'),
			import('monaco-editor/language/css/css.worker?worker'),
			import('monaco-editor/language/html/html.worker?worker'),
			import('monaco-editor/language/typescript/ts.worker?worker'),
		]);
		(self as any).MonacoEnvironment = {
			getWorker(_: string, label: string) {
				if (label === 'json') return new jsonWorker.default();
				if (label === 'css' || label === 'scss' || label === 'less') return new cssWorker.default();
				if (label === 'html' || label === 'handlebars' || label === 'razor') return new htmlWorker.default();
				if (label === 'typescript' || label === 'javascript') return new tsWorker.default();
				return new editorWorker.default();
			},
		};
		defineThemes(monaco);
		// The workspace isn't a TS project Monaco knows; squiggles about missing imports are noise.
		const ts = (monaco as any).typescript ?? (monaco.languages as any).typescript;
		for (const defaults of [ts?.typescriptDefaults, ts?.javascriptDefaults]) {
			defaults?.setDiagnosticsOptions?.({noSemanticValidation: true, noSyntaxValidation: false});
		}
		return monaco;
	})();
	return loading;
}

function defineThemes(monaco: typeof Monaco) {
	monaco.editor.defineTheme('oxi-dark', {
		base: 'vs-dark',
		inherit: true,
		rules: [
			{token: 'comment', foreground: '6b7280', fontStyle: 'italic'},
			{token: 'keyword', foreground: 'c49cff'},
			{token: 'string', foreground: '9fd88a'},
			{token: 'number', foreground: 'f2a970'},
			{token: 'type', foreground: '6fd0d8'},
			{token: 'function', foreground: '79a8ff'},
		],
		colors: {
			'editor.background': '#1a1c21',
			'editor.foreground': '#e4e6eb',
			'editorLineNumber.foreground': '#4a505b',
			'editorLineNumber.activeForeground': '#a9aeb9',
			'editor.lineHighlightBackground': '#ffffff08',
			'editor.lineHighlightBorder': '#00000000',
			'editor.selectionBackground': '#79a8ff40',
			'editor.inactiveSelectionBackground': '#79a8ff22',
			'editorCursor.foreground': '#79a8ff',
			'editorIndentGuide.background1': '#ffffff10',
			'editorIndentGuide.activeBackground1': '#ffffff24',
			'editorGutter.background': '#1a1c21',
			'editorWidget.background': '#212329',
			'editorWidget.border': '#ffffff1a',
			'editorSuggestWidget.background': '#212329',
			'editorSuggestWidget.selectedBackground': '#79a8ff30',
			'scrollbarSlider.background': '#ffffff14',
			'scrollbarSlider.hoverBackground': '#ffffff24',
			'minimap.background': '#1a1c21',
			'diffEditor.insertedTextBackground': '#74d39a22',
			'diffEditor.removedTextBackground': '#ff7a7a22',
			'diffEditor.insertedLineBackground': '#74d39a14',
			'diffEditor.removedLineBackground': '#ff7a7a14',
		},
	});
	monaco.editor.defineTheme('oxi-light', {
		base: 'vs',
		inherit: true,
		rules: [
			{token: 'comment', foreground: '8a919c', fontStyle: 'italic'},
			{token: 'keyword', foreground: '8b3fd9'},
			{token: 'string', foreground: '2a8a3a'},
			{token: 'number', foreground: 'c0571b'},
		],
		colors: {
			'editor.background': '#ffffff',
			'editorLineNumber.foreground': '#b0b6c1',
			'editorLineNumber.activeForeground': '#545b69',
			'editor.lineHighlightBackground': '#0f172a07',
			'editor.lineHighlightBorder': '#00000000',
			'editorCursor.foreground': '#2f6fed',
			'editor.selectionBackground': '#2f6fed30',
		},
	});
}

const EXT: Record<string, string> = {
	rs: 'rust', ts: 'typescript', tsx: 'typescript', mts: 'typescript', js: 'javascript', jsx: 'javascript', mjs: 'javascript',
	cjs: 'javascript', py: 'python', go: 'go', java: 'java', kt: 'kotlin', kts: 'kotlin', swift: 'swift', c: 'c', h: 'cpp',
	cc: 'cpp', cpp: 'cpp', hpp: 'cpp', cs: 'csharp', rb: 'ruby', php: 'php', sh: 'shell', bash: 'shell', zsh: 'shell',
	json: 'json', jsonc: 'json', toml: 'ini', ini: 'ini', yaml: 'yaml', yml: 'yaml', md: 'markdown', markdown: 'markdown',
	html: 'html', htm: 'html', xml: 'xml', svg: 'xml', svelte: 'html', vue: 'html', css: 'css', scss: 'scss', less: 'less',
	sql: 'sql', lua: 'lua', dart: 'dart', r: 'r', ps1: 'powershell', bat: 'bat', gradle: 'kotlin', graphql: 'graphql',
	proto: 'protobuf', tf: 'hcl', dockerfile: 'dockerfile', txt: 'plaintext', lock: 'plaintext',
};

export function languageOf(path: string): string {
	const name = path.split(/[\\/]/).pop()?.toLowerCase() ?? '';
	if (name === 'dockerfile') return 'dockerfile';
	if (name === 'makefile') return 'plaintext';
	const ext = name.includes('.') ? name.split('.').pop()! : '';
	return EXT[ext] ?? 'plaintext';
}

export const editorOptions = (fontSize: number): Monaco.editor.IStandaloneEditorConstructionOptions => ({
	fontFamily: "'SF Mono', 'JetBrains Mono', Menlo, monospace",
	fontSize: Math.round(fontSize * 0.93),
	lineHeight: Math.round(fontSize * 1.55),
	automaticLayout: true,
	minimap: {enabled: true, renderCharacters: false, maxColumn: 90, scale: 1},
	scrollBeyondLastLine: false,
	smoothScrolling: true,
	cursorSmoothCaretAnimation: 'on',
	cursorBlinking: 'smooth',
	renderLineHighlight: 'all',
	padding: {top: 10, bottom: 10},
	bracketPairColorization: {enabled: true},
	guides: {bracketPairs: 'active', indentation: true},
	stickyScroll: {enabled: true},
	fontLigatures: false,
	multiCursorModifier: 'alt',
	scrollbar: {verticalScrollbarSize: 10, horizontalScrollbarSize: 10, useShadows: false},
	overviewRulerBorder: false,
	renderWhitespace: 'selection',
	folding: true,
	showFoldingControls: 'mouseover',
	tabSize: 4,
	detectIndentation: true,
});
