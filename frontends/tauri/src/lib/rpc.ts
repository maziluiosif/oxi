// The oxi daemon, through the `oxi headless` bridge the Rust side runs: requests carry an id
// and get one reply; notifications (streaming messages, turn state, …) come without one and
// may be about chats another client (the app, Sublime, the phone) is running.

import {invoke} from '@tauri-apps/api/core';
import {listen} from '@tauri-apps/api/event';

type Pending = {resolve: (value: any) => void; reject: (error: Error) => void; method: string};
type Handler = (params: any) => void;

export type ConnectionState = 'connecting' | 'connected' | 'down';

class Rpc {
	private nextId = 1;
	private pending = new Map<number, Pending>();
	private handlers = new Map<string, Set<Handler>>();
	private stateHandlers = new Set<(state: ConnectionState, detail: string) => void>();
	private restarts = 0;
	private started = false;
	state: ConnectionState = 'connecting';
	binary = '';
	readonly log: string[] = [];

	async start(binary?: string) {
		if (!this.started) {
			this.started = true;
			await listen<string>('oxi://line', event => this.onLine(event.payload));
			await listen('oxi://exit', () => this.onExit());
			await listen<string>('oxi://log', event => {
				this.log.push(event.payload);
				if (this.log.length > 400) this.log.splice(0, this.log.length - 400);
			});
		}
		this.setState('connecting', '');
		try {
			this.binary = await invoke<string>('oxi_start', {binary: binary ?? localStorage.getItem('oxi.binary') ?? null});
			await this.request('initialize');
			this.restarts = 0;
			this.setState('connected', this.binary);
		} catch (e) {
			this.setState('down', String(e));
		}
	}

	request<T = any>(method: string, params: Record<string, unknown> = {}): Promise<T> {
		const id = this.nextId++;
		return new Promise<T>((resolve, reject) => {
			this.pending.set(id, {resolve, reject, method});
			invoke('oxi_send', {line: JSON.stringify({id, method, params})}).catch(error => {
				this.pending.delete(id);
				reject(new Error(String(error)));
			});
		});
	}

	on(method: string, handler: Handler): () => void {
		let set = this.handlers.get(method);
		if (!set) this.handlers.set(method, (set = new Set()));
		set.add(handler);
		return () => set!.delete(handler);
	}

	onState(handler: (state: ConnectionState, detail: string) => void) {
		this.stateHandlers.add(handler);
		return () => this.stateHandlers.delete(handler);
	}

	async restart(binary?: string) {
		await invoke('oxi_stop').catch(() => {});
		await this.start(binary);
	}

	private setState(state: ConnectionState, detail: string) {
		this.state = state;
		for (const handler of this.stateHandlers) handler(state, detail);
	}

	private onLine(line: string) {
		let message: any;
		try {
			message = JSON.parse(line);
		} catch {
			this.log.push(`unreadable: ${line.slice(0, 200)}`);
			return;
		}
		if (message.method) {
			for (const handler of this.handlers.get(message.method) ?? []) {
				try {
					handler(message.params ?? {});
				} catch (e) {
					console.error(`oxi: ${message.method} handler`, e);
				}
			}
			return;
		}
		const pending = this.pending.get(message.id);
		if (!pending) return;
		this.pending.delete(message.id);
		if (message.error != null) pending.reject(new Error(String(message.error)));
		else pending.resolve(message.result);
	}

	private onExit() {
		for (const pending of this.pending.values()) pending.reject(new Error('oxi stopped'));
		this.pending.clear();
		this.setState('down', 'oxi stopped');
		// The bridge exits when a newer daemon replaces the old one; come back on our own.
		if (this.restarts < 5) {
			this.restarts++;
			setTimeout(() => this.start(), 400 * this.restarts);
		}
	}
}

export const rpc = new Rpc();

/** A user-facing error text: the daemon's messages are already sentences. */
export function errorText(error: unknown): string {
	const text = error instanceof Error ? error.message : String(error);
	return text.replace(/^(not_found|already_exists|permission_denied|invalid_data|other):/, '');
}
