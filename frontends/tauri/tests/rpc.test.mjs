import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

function harness() {
	const handlers = new Map();
	const timers = new Map();
	const calls = [];
	let timerId = 0;
	let rpc;
	const invoke = async (command, args) => {
		calls.push(command);
		if (command === 'oxi_start') return '/test/oxi';
		if (command === 'oxi_send') {
			const request = JSON.parse(args.line);
			if (request.method === 'initialize') {
				handlers.get('oxi://line')({payload: JSON.stringify({id: request.id, result: {}})});
			}
		}
	};
	const source = readFileSync(new URL('../src/lib/rpc.ts', import.meta.url), 'utf8');
	const {outputText} = ts.transpileModule(source, {
		compilerOptions: {module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022},
	});
	const exports = {};
	new Function('exports', 'require', 'localStorage', 'setTimeout', 'clearTimeout', outputText)(
		exports,
		name => name.endsWith('/core')
			? {invoke}
			: {listen: async (event, handler) => handlers.set(event, handler)},
		{getItem: () => null},
		callback => { timers.set(++timerId, callback); return timerId; },
		id => timers.delete(id),
	);
	rpc = exports.rpc;
	return {rpc, handlers, timers, calls};
}

test('manual restart rejects requests belonging to the stopped bridge', async () => {
	const {rpc} = harness();
	await rpc.start();
	const pending = assert.rejects(rpc.request('chat.open'), /oxi stopped/);
	await rpc.restart();
	await pending;
	assert.equal(rpc.state, 'connected');
});

test('manual restart cancels an already scheduled automatic reconnect', async () => {
	const {rpc, handlers, timers, calls} = harness();
	await rpc.start();
	handlers.get('oxi://exit')({});
	assert.equal(timers.size, 1);
	await rpc.restart();
	assert.equal(timers.size, 0);
	assert.equal(calls.filter(call => call === 'oxi_start').length, 2);
	assert.equal(rpc.state, 'connected');
});

test('unexpected exit rejects pending requests and schedules reconnect', async () => {
	const {rpc, handlers, timers} = harness();
	await rpc.start();
	const pending = assert.rejects(rpc.request('chat.open'), /oxi stopped/);
	handlers.get('oxi://exit')({});
	await pending;
	assert.equal(rpc.state, 'down');
	assert.equal(timers.size, 1);
	await [...timers.values()][0]();
	assert.equal(rpc.state, 'connected');
});
