<script lang="ts">
	import {fade, fly} from 'svelte/transition';
	import {open as openDialog} from '@tauri-apps/plugin-dialog';
	import {invoke} from '@tauri-apps/api/core';
	import {app} from '../lib/state.svelte';
	import {rpc, errorText} from '../lib/rpc';
	import type {IconName} from '../lib/icons';
	import Icon from './Icon.svelte';
	import Toggle from './Toggle.svelte';

	const TABS: {id: string; label: string; icon: IconName}[] = [
		{id: 'general', label: 'General', icon: 'settings'},
		{id: 'appearance', label: 'Appearance', icon: 'palette'},
		{id: 'providers', label: 'Providers', icon: 'cpu'},
		{id: 'agent', label: 'Agent & Tools', icon: 'wrench'},
		{id: 'prompts', label: 'Prompts', icon: 'message'},
		{id: 'git', label: 'Git & GitHub', icon: 'git'},
		{id: 'connection', label: 'Connection', icon: 'server'},
	];

	const s = $derived(app.settings?.settings ?? null);
	const providers: any[] = $derived(app.settings?.providers ?? []);
	const tools: string[] = $derived(app.settings?.tools ?? []);
	let selectedProvider = $state('');
	let search = $state('');
	let saving = $state(false);

	$effect(() => {
		if (app.settingsOpen && app.connection === 'connected') app.loadSettings();
	});
	$effect(() => {
		if (!selectedProvider && providers.length) selectedProvider = providers.find(p => p.configured)?.id ?? providers[0].id;
	});

	async function patch(value: Record<string, unknown>) {
		saving = true;
		try {
			await app.patchSettings(value);
		} catch (e) {
			app.toast(errorText(e), 'error');
		}
		saving = false;
	}

	const patchProvider = (id: string, value: Record<string, unknown>) => patch({providers: {[id]: value}});

	async function setKey(provider: string, key: string) {
		try {
			const result = await rpc.request('settings.set_secret', {provider, key});
			app.applySettings(result);
			app.toast(key ? 'Key saved to the keychain' : 'Key removed', 'success');
		} catch (e) {
			app.toast(errorText(e), 'error');
		}
	}

	async function setGithubToken(token: string) {
		try {
			const result = await rpc.request('settings.set_secret', {github_token: token});
			app.applySettings(result);
			app.toast(token ? 'Token saved to the keychain' : 'Token removed', 'success');
		} catch (e) {
			app.toast(errorText(e), 'error');
		}
	}

	function setTool(index: number, on: boolean) {
		const list = [...(s?.tools_enabled ?? [])];
		list[index] = on;
		patch({tools_enabled: list});
	}

	let allowInput = $state('');
	function addAllow() {
		const prefix = allowInput.trim();
		if (!prefix) return;
		patch({bash_allowlist: [...(s?.bash_allowlist ?? []).filter((p: string) => p !== prefix), prefix]});
		allowInput = '';
	}

	// ---------- connection ----------
	let binary = $state(localStorage.getItem('oxi.binary') ?? '');
	let detected = $state('');
	let daemon = $state<any>(null);
	$effect(() => {
		if (app.settingsOpen && app.settingsTab === 'connection') {
			invoke<string | null>('oxi_binary').then(b => (detected = b ?? ''));
			rpc.request('daemon.hello', {}).then(h => (daemon = h)).catch(() => (daemon = null));
		}
	});
	async function pickBinary() {
		const path = await openDialog({multiple: false, title: 'Pick the oxi executable'});
		if (typeof path === 'string') binary = path;
	}
	function applyBinary() {
		if (binary.trim()) localStorage.setItem('oxi.binary', binary.trim());
		else localStorage.removeItem('oxi.binary');
		rpc.restart(binary.trim() || undefined);
	}

	function openSettingsFile() {
		const path = app.settings?.path;
		if (!path) return;
		app.settingsOpen = false;
		app.openFile(app.activeRoot || path.split('/').slice(0, -1).join('/'), path);
	}

	const matches = (text: string) => !search.trim() || text.toLowerCase().includes(search.trim().toLowerCase());
	const provider = $derived(providers.find(p => p.id === selectedProvider));
	const providerConfig = $derived(s?.providers?.[selectedProvider] ?? {});

	function onKey(event: KeyboardEvent) {
		if (app.settingsOpen && event.key === 'Escape') {
			event.stopPropagation();
			app.settingsOpen = false;
		}
	}
</script>

<svelte:window onkeydown={onKey} />

{#if app.settingsOpen}
	<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
	<div class="scrim" transition:fade={{duration: 120}} onclick={() => (app.settingsOpen = false)}>
		<div class="settings" onclick={e => e.stopPropagation()} transition:fly={{y: 10, duration: 180}}>
			<nav>
				<div class="search">
					<Icon name="search" size={13} />
					<input placeholder="Search settings" bind:value={search} />
				</div>
				{#each TABS as tab}
					<button class:on={app.settingsTab === tab.id} onclick={() => (app.settingsTab = tab.id)}>
						<Icon name={tab.icon} size={15} />
						{tab.label}
					</button>
				{/each}
				<span class="grow"></span>
				<button class="raw" onclick={openSettingsFile} title={app.settings?.path}>
					<Icon name="fileText" size={14} /> Edit settings.json
				</button>
			</nav>
			<section>
				<header>
					<h2>{TABS.find(t => t.id === app.settingsTab)?.label}</h2>
					{#if saving}<span class="saving"><span class="spinner"></span> Saving</span>{/if}
					<button class="icon-btn" onclick={() => (app.settingsOpen = false)} title="Close"><Icon name="x" /></button>
				</header>
				<div class="body">
					{#if !s && app.settingsTab !== 'appearance' && app.settingsTab !== 'connection'}
						<div class="empty">
							{app.connection === 'connected' ? 'This oxi build has no settings API; update oxi to edit settings here.' : 'Connect to oxi to edit its settings.'}
						</div>
					{:else if app.settingsTab === 'general'}
						{@render toggle('Notify when a response is ready', 'Desktop notification when a turn ends or needs approval while the window is in the background', s.notify_in_background, v => patch({notify_in_background: v}))}
						{@render toggle('Keep the Mac awake while working', 'Holds off idle sleep while an agent runs a turn', s.keep_awake_while_working, v => patch({keep_awake_while_working: v}))}
						{@render toggle('Name chats automatically', 'Ask the helper model for a short title after the first reply', s.auto_title_chats, v => patch({auto_title_chats: v}))}
						{@render toggle('Install updates automatically', 'oxi downloads new releases in the background', s.auto_update, v => patch({auto_update: v}))}
						{@render toggle('Activity log', 'Record every tool call to the activity log', s.activity_log_enabled, v => patch({activity_log_enabled: v}))}
						{#if matches('idle agent minutes acp stop')}
							<div class="row">
								<div class="text"><b>Stop idle agents after</b><span>Minutes an ACP agent may sit idle before its process stops; 0 never stops it</span></div>
								<input class="field num" type="number" min="0" value={s.acp_idle_minutes} onchange={e => patch({acp_idle_minutes: Number(e.currentTarget.value)})} />
							</div>
						{/if}
						{#if matches('max live agents')}
							<div class="row">
								<div class="text"><b>Agents kept running</b><span>Most ACP agent processes at once; 0 is no limit</span></div>
								<input class="field num" type="number" min="0" value={s.acp_max_live_agents} onchange={e => patch({acp_max_live_agents: Number(e.currentTarget.value)})} />
							</div>
						{/if}
					{:else if app.settingsTab === 'appearance'}
						<div class="row">
							<div class="text"><b>Theme</b><span>System follows macOS light/dark</span></div>
							<div class="seg">
								{#each [['system', 'System'], ['dark', 'Dark'], ['midnight', 'Midnight'], ['light', 'Light']] as [id, label]}
									<button class:on={app.theme === id} onclick={() => ((app.theme = id as any), app.persistLayout())}>{label}</button>
								{/each}
							</div>
						</div>
						<div class="row">
							<div class="text"><b>Text size</b><span>{app.fontSize}px · ⌘= and ⌘- change it anywhere</span></div>
							<input type="range" min="11" max="20" step="1" bind:value={app.fontSize} onchange={() => app.persistLayout()} />
						</div>
						<div class="row">
							<div class="text"><b>Tool calls</b><span>How much of each tool call the transcript shows</span></div>
							<div class="seg">
								{#each [['minimal', 'Minimal'], ['balanced', 'Balanced'], ['expanded', 'Expanded']] as [id, label]}
									<button class:on={app.toolDetail === id} onclick={() => ((app.toolDetail = id as any), app.persistLayout())}>{label}</button>
								{/each}
							</div>
						</div>
					{:else if app.settingsTab === 'providers'}
						<div class="providers">
							<div class="plist">
								{#each providers.filter(p => matches(p.label)) as p}
									<button class:on={p.id === selectedProvider} onclick={() => (selectedProvider = p.id)}>
										<span class="led" class:ok={p.configured}></span>
										<span>{p.label}</span>
									</button>
								{/each}
							</div>
							{#if provider}
								<div class="pdetail">
									<h3>{provider.label}</h3>
									<p class="hint">{provider.configured ? 'Ready to use.' : 'Not set up yet.'} {provider.acp ? 'Runs as an agent subprocess (ACP); it uses its own login unless you give it a key.' : ''}</p>
									{#if provider.id !== 'router' && !['ollama', 'lmstudio', 'llamacpp', 'localhf', 'remotehf'].includes(provider.id)}
										<label>API key</label>
										{#key provider.id}
											<form class="inline" onsubmit={e => {
												e.preventDefault();
												const input = e.currentTarget.elements.namedItem('key') as HTMLInputElement;
												setKey(provider.id, input.value.trim());
												input.value = '';
											}}>
												<input class="field mono" name="key" type="password" placeholder={provider.has_key ? '•••••••• saved in the keychain' : 'Paste a key'} autocomplete="off" />
												<button class="btn primary" type="submit">Save</button>
												{#if provider.has_key}<button class="btn danger" type="button" onclick={() => setKey(provider.id, '')}>Remove</button>{/if}
											</form>
										{/key}
									{/if}
									<label>Model</label>
									<input class="field mono" value={providerConfig.model_id ?? ''} placeholder={provider.default_model} onchange={e => patchProvider(provider.id, {model_id: e.currentTarget.value.trim()})} />
									{#if !provider.acp && provider.id !== 'router'}
										<label>Base URL</label>
										<input class="field mono" value={providerConfig.base_url ?? ''} placeholder={provider.default_base_url} onchange={e => patchProvider(provider.id, {base_url: e.currentTarget.value.trim()})} />
										<label>Context window (tokens)</label>
										<input class="field mono" type="number" value={providerConfig.context_window ?? ''} placeholder="auto" onchange={e => patchProvider(provider.id, {context_window: e.currentTarget.value ? Number(e.currentTarget.value) : null})} />
									{/if}
									{#if provider.acp}
										<label>Command</label>
										<input class="field mono" value={providerConfig.acp_command ?? ''} placeholder={provider.default_acp_command} onchange={e => patchProvider(provider.id, {acp_command: e.currentTarget.value.trim()})} />
									{/if}
									<label>Reasoning effort</label>
									<select class="field" value={providerConfig.effort ?? ''} onchange={e => patchProvider(provider.id, {effort: e.currentTarget.value})}>
										{#each ['', 'low', 'medium', 'high', 'xhigh', 'max'] as v}<option value={v}>{v || 'Auto'}</option>{/each}
									</select>
									<div class="row compact">
										<div class="text"><b>Default provider</b><span>New chats start here</span></div>
										<Toggle checked={s.active_provider === provider.id} onchange={v => v && patch({active_provider: provider.id})} />
									</div>
								</div>
							{/if}
						</div>
					{:else if app.settingsTab === 'agent'}
						{@render toggle('Ask before editing files', 'Approve each write, edit, delete or move of the built-in agent', s.require_write_edit_approval, v => patch({require_write_edit_approval: v}))}
						{@render toggle('Ask before running commands', 'Approve each bash command of the built-in agent', s.require_bash_approval, v => patch({require_bash_approval: v}))}
						{@render toggle('Read AGENTS.md', 'Add the project’s AGENTS.md to the system prompt', s.include_agents_md, v => patch({include_agents_md: v}))}
						{@render toggle('Read .oxi/rules', 'Add .oxi/rules and .cursor/rules to the system prompt', s.include_oxi_rules, v => patch({include_oxi_rules: v}))}
						{#if matches('allowed commands allowlist bash')}
							<div class="block">
								<b>Commands that run without asking</b>
								<div class="chips">
									{#each s.bash_allowlist ?? [] as prefix}
										<span class="chip"><code>{prefix}</code><button onclick={() => patch({bash_allowlist: s.bash_allowlist.filter((p: string) => p !== prefix)})}><Icon name="x" size={11} /></button></span>
									{/each}
								</div>
								<form class="inline" onsubmit={e => (e.preventDefault(), addAllow())}>
									<input class="field mono" bind:value={allowInput} placeholder="cargo test" />
									<button class="btn" type="submit">Add</button>
								</form>
							</div>
						{/if}
						{#if matches('tool rounds timeout')}
							<div class="row">
								<div class="text"><b>Max tool rounds per turn</b><span>0 is unlimited</span></div>
								<input class="field num" type="number" min="0" value={s.max_tool_rounds} onchange={e => patch({max_tool_rounds: Number(e.currentTarget.value)})} />
							</div>
							<div class="row">
								<div class="text"><b>Command timeout</b><span>Seconds a bash call may run</span></div>
								<input class="field num" type="number" min="1" value={s.bash_timeout_cap_secs} onchange={e => patch({bash_timeout_cap_secs: Number(e.currentTarget.value)})} />
							</div>
						{/if}
						{#if matches('web search searxng duckduckgo bing')}
							<div class="row">
								<div class="text"><b>Web search</b><span>Backend of the web_search tool</span></div>
								<select class="field sel" value={s.web_search_backend} onchange={e => patch({web_search_backend: e.currentTarget.value})}>
									<option value="duckduckgo">DuckDuckGo</option>
									<option value="bing">Bing</option>
									<option value="searxng">SearXNG</option>
								</select>
							</div>
							{#if s.web_search_backend === 'searxng'}
								<input class="field mono" value={s.searxng_url} placeholder="https://searx.example.com" onchange={e => patch({searxng_url: e.currentTarget.value.trim()})} />
							{/if}
						{/if}
						{#if matches('tools enabled') || tools.some(t => matches(t))}
							<div class="block">
								<b>Built-in tools</b>
								<div class="tools">
									{#each tools as tool, i}
										{#if matches(tool) || matches('tools enabled')}
											<label class="tool"><Toggle checked={s.tools_enabled?.[i] ?? true} onchange={v => setTool(i, v)} /> <code>{tool}</code></label>
										{/if}
									{/each}
								</div>
							</div>
						{/if}
						{#if (s.mcp_servers ?? []).length && matches('mcp servers')}
							<div class="block">
								<b>MCP servers</b>
								{#each s.mcp_servers as server, i}
									<div class="row compact">
										<div class="text"><b>{server.name}</b><span>{server.url || [server.command, ...(server.args ?? [])].join(' ')}</span></div>
										<Toggle
											checked={server.enabled}
											onchange={v => {
												const list = structuredClone(s.mcp_servers);
												list[i].enabled = v;
												patch({mcp_servers: list});
											}}
										/>
									</div>
								{/each}
							</div>
						{/if}
					{:else if app.settingsTab === 'prompts'}
						<label>System prompt</label>
						<textarea class="field mono tall" value={s.system_prompt} onchange={e => patch({system_prompt: e.currentTarget.value})}></textarea>
						<label>Commit message prompt</label>
						<textarea class="field mono" rows="6" value={s.commit_msg_system_prompt} onchange={e => patch({commit_msg_system_prompt: e.currentTarget.value})}></textarea>
						<label>Commit message model</label>
						<div class="inline">
							<select class="field" value={s.commit_msg_provider ?? ''} onchange={e => patch({commit_msg_provider: e.currentTarget.value || null})}>
								<option value="">Same as the active provider</option>
								{#each providers.filter(p => p.configured) as p}<option value={p.id}>{p.label}</option>{/each}
							</select>
							<input class="field mono" value={s.commit_msg_model_id} placeholder="model id (optional)" onchange={e => patch({commit_msg_model_id: e.currentTarget.value.trim()})} />
						</div>
					{:else if app.settingsTab === 'git'}
						<label>Author name</label>
						<input class="field" value={s.git_author_name} placeholder="from git config" onchange={e => patch({git_author_name: e.currentTarget.value})} />
						<label>Author email</label>
						<input class="field" value={s.git_author_email} placeholder="from git config" onchange={e => patch({git_author_email: e.currentTarget.value})} />
						<label>GitHub username</label>
						<input class="field" value={s.github_username} onchange={e => patch({github_username: e.currentTarget.value.trim()})} />
						<label>GitHub token</label>
						<form class="inline" onsubmit={e => {
							e.preventDefault();
							const input = e.currentTarget.elements.namedItem('token') as HTMLInputElement;
							setGithubToken(input.value.trim());
							input.value = '';
						}}>
							<input class="field mono" name="token" type="password" placeholder={app.settings?.has_github_token ? '•••••••• saved in the keychain' : 'ghp_…'} autocomplete="off" />
							<button class="btn primary" type="submit">Save</button>
							{#if app.settings?.has_github_token}<button class="btn danger" type="button" onclick={() => setGithubToken('')}>Remove</button>{/if}
						</form>
						{@render toggle('Use the git command for fetch, pull and push', 'Uses your credential helper and SSH keys instead of the token', s.git_use_system_cli, v => patch({git_use_system_cli: v}))}
					{:else if app.settingsTab === 'connection'}
						<p class="hint">This app talks to the oxi daemon through <code>oxi headless</code>, so chats are shared live with the oxi app, the editor plugins and the phone.</p>
						<div class="row">
							<div class="text"><b>Status</b><span class="selectable">{app.connection === 'connected' ? `Connected to oxi ${app.version}` : app.connectionDetail || app.connection}</span></div>
							<button class="btn" onclick={() => rpc.restart()}><Icon name="refresh" size={13} /> Reconnect</button>
						</div>
						{#if daemon}
							<div class="row">
								<div class="text"><b>Daemon</b><span>pid {daemon.pid} · {daemon.clients} {daemon.clients === 1 ? 'client' : 'clients'} · {daemon.chats_running} running</span></div>
							</div>
						{/if}
						<label>oxi executable</label>
						<div class="inline">
							<input class="field mono" bind:value={binary} placeholder={detected || 'oxi'} />
							<button class="btn" onclick={pickBinary}>Choose…</button>
							<button class="btn primary" onclick={applyBinary}>Use</button>
						</div>
						<p class="hint">Empty: <code>$OXI_PATH</code>, then <code>~/.local/bin/oxi</code>, the app bundle and PATH. Found: <code class="selectable">{rpc.binary || detected || 'none'}</code></p>
					{/if}
				</div>
			</section>
		</div>
	</div>
{/if}

{#snippet toggle(title: string, detail: string, value: boolean, onchange: (v: boolean) => void)}
	{#if matches(title + ' ' + detail)}
		<div class="row">
			<div class="text"><b>{title}</b><span>{detail}</span></div>
			<Toggle checked={!!value} {onchange} />
		</div>
	{/if}
{/snippet}

<style>
	.scrim {
		position: fixed;
		inset: 0;
		z-index: 850;
		background: var(--scrim);
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.settings {
		width: 900px;
		height: 640px;
		max-width: calc(100vw - 48px);
		max-height: calc(100vh - 48px);
		display: flex;
		border-radius: 16px;
		background: var(--bg-raised);
		border: 1px solid var(--border-strong);
		box-shadow: var(--shadow);
		overflow: hidden;
	}
	nav {
		width: 210px;
		flex: none;
		display: flex;
		flex-direction: column;
		gap: 2px;
		padding: 14px 10px;
		background: var(--sidebar-solid);
		border-right: 1px solid var(--border);
	}
	.search {
		display: flex;
		align-items: center;
		gap: 7px;
		height: 30px;
		padding: 0 9px;
		margin-bottom: 10px;
		border-radius: 7px;
		background: var(--bg-hover);
		color: var(--text-3);
	}
	.search input {
		flex: 1;
		min-width: 0;
		border: none;
		outline: none;
		background: none;
		font-size: var(--fs-sm);
		color: var(--text);
	}
	nav button {
		display: flex;
		align-items: center;
		gap: 9px;
		height: 32px;
		padding: 0 10px;
		border-radius: 7px;
		font-size: var(--fs-sm);
		color: var(--text-2);
		text-align: left;
	}
	nav button:hover {
		background: var(--bg-hover);
		color: var(--text);
	}
	nav button.on {
		background: var(--accent-soft);
		color: var(--accent);
		font-weight: 550;
	}
	.grow {
		flex: 1;
	}
	.raw {
		font-size: var(--fs-xs) !important;
		color: var(--text-3) !important;
	}
	section {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	header {
		display: flex;
		align-items: center;
		gap: 10px;
		height: 56px;
		padding: 0 14px 0 28px;
		border-bottom: 1px solid var(--border);
	}
	h2 {
		flex: 1;
		margin: 0;
		font-size: calc(var(--fs) + 3px);
		font-weight: 650;
		letter-spacing: -0.01em;
	}
	.saving {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: var(--fs-xs);
		color: var(--text-3);
	}
	.body {
		flex: 1;
		overflow-y: auto;
		padding: 18px 28px 32px;
		display: flex;
		flex-direction: column;
	}
	.row {
		display: flex;
		align-items: center;
		gap: 20px;
		padding: 12px 0;
		border-bottom: 1px solid var(--border);
	}
	.row.compact {
		border-bottom: none;
		padding: 8px 0;
	}
	.text {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
	}
	.text b {
		font-weight: 550;
		font-size: var(--fs-sm);
	}
	.text span {
		font-size: var(--fs-xs);
		color: var(--text-3);
	}
	label {
		display: block;
		margin: 14px 0 6px;
		font-size: var(--fs-xs);
		font-weight: 600;
		color: var(--text-2);
	}
	.field.num {
		width: 90px;
	}
	.field.sel {
		width: 160px;
	}
	.tall {
		min-height: 300px;
		font-size: var(--fs-xs) !important;
	}
	.inline {
		display: flex;
		gap: 6px;
	}
	.hint {
		font-size: var(--fs-xs);
		color: var(--text-3);
		margin: 6px 0;
	}
	.hint code,
	.block code {
		font-size: 0.95em;
		padding: 1px 5px;
		border-radius: 4px;
		background: var(--bg-active);
	}
	.seg {
		display: flex;
		padding: 2px;
		border-radius: 8px;
		background: var(--bg-hover);
		gap: 2px;
	}
	.seg button {
		height: 26px;
		padding: 0 12px;
		border-radius: 6px;
		font-size: var(--fs-xs);
		color: var(--text-2);
	}
	.seg button.on {
		background: var(--bg-raised);
		color: var(--text);
		box-shadow: var(--shadow-sm);
	}
	input[type='range'] {
		width: 180px;
		accent-color: var(--accent);
	}
	.providers {
		display: flex;
		gap: 20px;
		min-height: 100%;
	}
	.plist {
		width: 200px;
		flex: none;
		display: flex;
		flex-direction: column;
		gap: 1px;
	}
	.plist button {
		display: flex;
		align-items: center;
		gap: 8px;
		height: 30px;
		padding: 0 10px;
		border-radius: 7px;
		font-size: var(--fs-sm);
		color: var(--text-2);
		text-align: left;
	}
	.plist button:hover {
		background: var(--bg-hover);
	}
	.plist button.on {
		background: var(--bg-active);
		color: var(--text);
	}
	.led {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--text-4);
		flex: none;
	}
	.led.ok {
		background: var(--green);
	}
	.pdetail {
		flex: 1;
		min-width: 0;
	}
	.pdetail h3 {
		margin: 4px 0 4px;
		font-size: calc(var(--fs) + 1px);
	}
	.block {
		padding: 14px 0;
		border-bottom: 1px solid var(--border);
		display: flex;
		flex-direction: column;
		gap: 10px;
	}
	.block > b {
		font-size: var(--fs-sm);
		font-weight: 550;
	}
	.chips {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
	}
	.chip {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		padding: 2px 4px 2px 2px;
		border-radius: 6px;
		background: var(--bg-hover);
	}
	.chip button {
		display: flex;
		color: var(--text-3);
		padding: 2px;
		border-radius: 4px;
	}
	.chip button:hover {
		color: var(--red);
	}
	.tools {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
		gap: 8px 16px;
	}
	.tool {
		display: flex;
		align-items: center;
		gap: 8px;
		margin: 0;
		font-weight: 400;
	}
</style>
