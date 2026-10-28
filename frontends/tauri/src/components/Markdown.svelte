<script lang="ts">
	import {renderMarkdown, renderStreaming} from '../lib/markdown';
	import {app} from '../lib/state.svelte';
	import {readFile, mimeOf} from '../lib/files';

	const images = new Map<string, Promise<string>>();
	function loadImage(path: string): Promise<string> {
		if (!images.has(path)) {
			images.set(
				path,
				readFile(path, 25_000_000)
					.then(f => `data:${mimeOf(path)};base64,${f.base64}`)
					.catch(() => ''),
			);
		}
		return images.get(path)!;
	}

	let {text, streaming = false, root = '', class: cls = ''}: {text: string; streaming?: boolean; root?: string; class?: string} = $props();

	const html = $derived(streaming ? renderStreaming(text) : renderMarkdown(text));
	let el = $state<HTMLDivElement>();

	// Local images (`file://`) come through the daemon as data URLs.
	$effect(() => {
		html;
		for (const img of el?.querySelectorAll<HTMLImageElement>('img[data-local]:not([src])') ?? []) {
			const path = img.dataset.local!;
			loadImage(path).then(src => src && (img.src = src));
		}
	});

	function onClick(event: MouseEvent) {
		const target = event.target as HTMLElement;
		const copy = target.closest('[data-copy]');
		if (copy) {
			const code = copy.closest('.code-block')?.querySelector('code')?.textContent ?? '';
			navigator.clipboard.writeText(code);
			copy.textContent = 'Copied';
			setTimeout(() => (copy.textContent = 'Copy'), 1400);
			return;
		}
		const img = target.closest('img') as HTMLImageElement | null;
		if (img?.src) {
			window.dispatchEvent(new CustomEvent('oxi:zoom', {detail: img.src}));
			return;
		}
		const ref = target.closest('[data-file]') as HTMLElement | null;
		if (ref && root) {
			const [file, line] = (ref.dataset.file ?? '').split(':');
			const path = file.startsWith('/') ? file : `${root}/${file}`;
			app.openFile(root, path, line ? Number(line) : undefined);
		}
	}
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="md {cls}" bind:this={el} onclick={onClick}>{@html html}</div>
