<script lang="ts">
	let {checked, onchange, disabled = false}: {checked: boolean; onchange: (value: boolean) => void; disabled?: boolean} = $props();
</script>

<button class="toggle" class:on={checked} {disabled} role="switch" aria-checked={checked} onclick={() => onchange(!checked)}>
	<span class="knob"></span>
</button>

<style>
	.toggle {
		flex: none;
		width: 34px;
		height: 20px;
		border-radius: 10px;
		background: var(--bg-active);
		border: 1px solid var(--border-strong);
		position: relative;
		transition: background 0.18s, border-color 0.18s;
	}
	.toggle.on {
		background: var(--accent);
		border-color: transparent;
	}
	.knob {
		position: absolute;
		top: 2px;
		left: 2px;
		width: 14px;
		height: 14px;
		border-radius: 50%;
		background: white;
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.3);
		transition: transform 0.18s var(--ease);
	}
	.on .knob {
		transform: translateX(14px);
	}
	.toggle:disabled {
		opacity: 0.4;
	}
</style>
