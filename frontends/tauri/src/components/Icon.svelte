<script lang="ts">
	import {ICONS, type IconName} from '../lib/icons';

	let {name, size = 16, stroke = 1.75, class: cls = ''}: {name: IconName; size?: number; stroke?: number; class?: string} = $props();
</script>

<svg
	class="icon {cls}"
	width={size}
	height={size}
	viewBox="0 0 24 24"
	fill="none"
	stroke="currentColor"
	stroke-width={stroke}
	stroke-linecap="round"
	stroke-linejoin="round"
	aria-hidden="true"><path d={ICONS[name]} /></svg
>

<style>
	.icon {
		flex: none;
		display: block;
	}
</style>
