<script lang="ts">
	// A drag handle between two panes. `invert`: the pane sized is after the handle (right/bottom).
	let {
		direction,
		size = $bindable(),
		min,
		max,
		invert = false,
		onend,
	}: {direction: 'x' | 'y'; size: number; min: number; max: number; invert?: boolean; onend?: () => void} = $props();

	let dragging = $state(false);

	function down(event: PointerEvent) {
		event.preventDefault();
		const start = direction === 'x' ? event.clientX : event.clientY;
		const startSize = size;
		dragging = true;
		const target = event.currentTarget as HTMLElement;
		target.setPointerCapture(event.pointerId);
		const move = (e: PointerEvent) => {
			const delta = (direction === 'x' ? e.clientX : e.clientY) - start;
			size = Math.round(Math.max(min, Math.min(max, startSize + (invert ? -delta : delta))));
		};
		const up = () => {
			dragging = false;
			target.removeEventListener('pointermove', move);
			target.removeEventListener('pointerup', up);
			window.dispatchEvent(new Event('resize'));
			onend?.();
		};
		target.addEventListener('pointermove', move);
		target.addEventListener('pointerup', up);
	}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="splitter {direction}" class:dragging onpointerdown={down}></div>

<style>
	.splitter {
		flex: none;
		position: relative;
		z-index: 20;
	}
	.splitter.x {
		width: 0;
		cursor: col-resize;
	}
	.splitter.y {
		height: 0;
		cursor: row-resize;
	}
	.splitter::after {
		content: '';
		position: absolute;
		transition: background 0.15s;
	}
	.splitter.x::after {
		top: 0;
		bottom: 0;
		left: -3px;
		width: 6px;
	}
	.splitter.y::after {
		left: 0;
		right: 0;
		top: -3px;
		height: 6px;
	}
	.splitter:hover::after,
	.splitter.dragging::after {
		background: color-mix(in srgb, var(--accent) 45%, transparent);
		transition-delay: 0.15s;
	}
	.splitter.dragging::after {
		transition-delay: 0s;
	}
</style>
