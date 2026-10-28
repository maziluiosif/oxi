import {mount} from 'svelte';
import './styles/app.css';
import App from './App.svelte';

// In a plain browser (the dev server), stand in for the Tauri runtime.
if (import.meta.env.DEV && !('__TAURI_INTERNALS__' in window)) {
	const {installDevShim} = await import('./lib/dev-shim');
	installDevShim();
}

export default mount(App, {target: document.getElementById('app')!});
