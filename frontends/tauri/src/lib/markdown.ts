// Markdown for answers and thinking: CommonMark + GFM tables, highlighted fences, no raw HTML
// (model output must never become markup that runs in the app).

import MarkdownIt from 'markdown-it';
import hljs from 'highlight.js/lib/common';

const escape = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

const ALIASES: Record<string, string> = {
	sh: 'bash',
	zsh: 'bash',
	shell: 'bash',
	console: 'bash',
	ts: 'typescript',
	tsx: 'typescript',
	js: 'javascript',
	jsx: 'javascript',
	py: 'python',
	rs: 'rust',
	yml: 'yaml',
	md: 'markdown',
	kt: 'kotlin',
	'c++': 'cpp',
	h: 'c',
	toml: 'ini',
};

export function highlight(code: string, lang: string): string {
	const name = ALIASES[lang.toLowerCase()] ?? lang.toLowerCase();
	if (name && hljs.getLanguage(name)) {
		try {
			return hljs.highlight(code, {language: name, ignoreIllegals: true}).value;
		} catch {}
	}
	return escape(code);
}

const md = new MarkdownIt({html: false, linkify: true, breaks: false, typographer: false});

md.renderer.rules.fence = (tokens, index) => {
	const token = tokens[index];
	const lang = token.info.trim().split(/\s+/)[0] ?? '';
	const code = token.content.replace(/\n$/, '');
	const label = lang ? `<span class="code-lang">${escape(lang)}</span>` : '<span class="code-lang"></span>';
	return (
		`<div class="code-block" data-lang="${escape(lang)}">` +
		`<div class="code-head">${label}<button class="code-copy" data-copy title="Copy">Copy</button></div>` +
		`<pre><code class="hljs">${highlight(code, lang)}</code></pre></div>`
	);
};

// Agents show screenshots as `![](file:///…)`; the webview can't load file URLs, so those
// images carry their path and `Markdown.svelte` loads them through the daemon.
const defaultValidate = md.validateLink;
md.validateLink = url => /^file:/i.test(url.trim()) || defaultValidate(url);
md.renderer.rules.image = (tokens, i) => {
	const token = tokens[i];
	const src = String(token.attrGet('src') ?? '');
	const alt = escape(token.content);
	if (/^file:/i.test(src)) {
		const path = decodeURIComponent(src.replace(/^file:\/\//i, ''));
		return `<img class="local-img" data-local="${escape(path)}" alt="${alt}" />`;
	}
	if (/^https?:|^data:image\//i.test(src)) return `<img src="${escape(src)}" alt="${alt}" />`;
	return alt;
};

// Links open in the browser (handled by a click listener), never in the app's webview.
const defaultLinkOpen = md.renderer.rules.link_open ?? ((tokens, i, options, _env, self) => self.renderToken(tokens, i, options));
md.renderer.rules.link_open = (tokens, i, options, env, self) => {
	tokens[i].attrSet('data-external', '1');
	return defaultLinkOpen(tokens, i, options, env, self);
};

// Tables scroll on their own instead of widening the transcript.
md.renderer.rules.table_open = () => '<div class="table-wrap"><table>';
md.renderer.rules.table_close = () => '</table></div>';

// Inline code that names a file becomes a link into the editor.
const defaultCode = md.renderer.rules.code_inline!;
md.renderer.rules.code_inline = (tokens, i, options, env, self) => {
	const content = tokens[i].content;
	if (/^[\w./-]+\.[a-z0-9]{1,6}(:\d+)?$/i.test(content) && content.includes('.')) {
		return `<code class="file-ref" data-file="${escape(content)}">${escape(content)}</code>`;
	}
	return defaultCode(tokens, i, options, env, self);
};

const cache = new Map<string, string>();

export function renderMarkdown(text: string): string {
	const hit = cache.get(text);
	if (hit !== undefined) return hit;
	const html = md.render(text);
	if (cache.size > 600) cache.clear();
	cache.set(text, html);
	return html;
}

/** Streaming text often ends inside a fence; close it so the rest doesn't render as prose. */
export function renderStreaming(text: string): string {
	const fences = (text.match(/^```/gm) ?? []).length;
	return md.render(fences % 2 === 1 ? text + '\n```' : text);
}
