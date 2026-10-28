// Stand-in for the Tauri runtime when the UI runs in a plain browser (`npm run dev`): IPC goes
// to the dev server's `oxi headless` relay (see dev/bridge-plugin.ts); window calls are no-ops.

import {mockIPC, mockWindows} from '@tauri-apps/api/mocks';
import {emit} from '@tauri-apps/api/event';

export function installDevShim() {
	// Screenshots drive the state from outside.
	import('./state.svelte').then(({app}) => ((window as any).__oxiApp = app));
	let events: EventSource | null = null;
	mockWindows('main');
	mockIPC(
		async (cmd, args: any) => {
			switch (cmd) {
				case 'oxi_start': {
					const res = await fetch('/__oxi/start');
					if (!res.ok) throw new Error(await res.text());
					if (!events) {
						events = new EventSource('/__oxi/events');
						events.onmessage = e => emit('oxi://line', e.data);
						events.addEventListener('exit', () => emit('oxi://exit', 0));
					}
					return (await res.json()).binary;
				}
				case 'oxi_send': {
					const res = await fetch('/__oxi/send', {method: 'POST', body: args.line});
					if (!res.ok) throw new Error(await res.text());
					return null;
				}
				case 'oxi_binary':
					return null;
				case 'home_dir':
					return null;
				case 'pty_spawn':
					throw new Error('The terminal needs the desktop app.');
				case 'plugin:notification|is_permission_granted':
					return false;
				default:
					return null;
			}
		},
		{shouldMockEvents: true},
	);
}
