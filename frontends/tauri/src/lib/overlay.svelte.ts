// Context menus and small dialogs (prompt, confirm), drawn once by `Overlays.svelte`.

import type {IconName} from './icons';

export type MenuItem =
	| {label: string; icon?: IconName; shortcut?: string; danger?: boolean; disabled?: boolean; checked?: boolean; action: () => void}
	| {separator: true}
	| {header: string};

export type PromptRequest = {
	title: string;
	message?: string;
	value?: string;
	placeholder?: string;
	confirm?: string;
	danger?: boolean;
	/** No text field: a yes/no question. */
	confirmOnly?: boolean;
	resolve: (value: string | null) => void;
};

class Overlay {
	menu = $state<{x: number; y: number; items: MenuItem[]} | null>(null);
	prompt = $state<PromptRequest | null>(null);

	openMenu(event: MouseEvent | {x: number; y: number}, items: MenuItem[]) {
		if ('preventDefault' in event) {
			event.preventDefault();
			event.stopPropagation();
		}
		const x = 'clientX' in event ? event.clientX : event.x;
		const y = 'clientY' in event ? event.clientY : event.y;
		this.menu = {x, y, items};
	}

	/** A menu under a button. */
	menuAt(element: Element, items: MenuItem[], align: 'left' | 'right' = 'left') {
		const rect = element.getBoundingClientRect();
		this.menu = {x: align === 'left' ? rect.left : rect.right - 220, y: rect.bottom + 4, items};
	}

	closeMenu() {
		this.menu = null;
	}

	ask(request: Omit<PromptRequest, 'resolve'>): Promise<string | null> {
		return new Promise(resolve => {
			this.prompt = {...request, resolve};
		});
	}

	async confirm(title: string, message: string, confirm = 'OK', danger = false): Promise<boolean> {
		const result = await this.ask({title, message, confirm, danger, confirmOnly: true});
		return result !== null;
	}
}

export const overlay = new Overlay();
