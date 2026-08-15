#!/usr/bin/env node
import { execFileSync, spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const WEB_PORT = Number(process.env.ARKADE_E2E_WEB_PORT || 18775);
const DRIVER_PORT = Number(process.env.ARKADE_E2E_DRIVER_PORT || 14454);
const WEB_URL = `http://127.0.0.1:${WEB_PORT}`;
const DRIVER_URL = `http://127.0.0.1:${DRIVER_PORT}`;
const SERVER = 'http://127.0.0.1:7070';
const PENDING_KEY = `arkade-maze:pending:v2:${SERVER}`;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function startProcess(command, args) {
  const child = spawn(command, args, { cwd: ROOT, stdio: ['ignore', 'pipe', 'pipe'] });
  let output = '';
  const collect = (chunk) => {
    output += chunk.toString();
    if (output.length > 20_000) output = output.slice(-20_000);
  };
  child.stdout.on('data', collect);
  child.stderr.on('data', collect);
  return { child, output: () => output };
}

async function waitForHttp(url, timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {}
    await sleep(200);
  }
  throw new Error(`timed out waiting for ${url}`);
}

async function request(method, pathName, body) {
  const response = await fetch(`${DRIVER_URL}${pathName}`, {
    method,
    headers: { 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const payload = await response.json();
  if (!response.ok || payload.value?.error) throw new Error(JSON.stringify(payload));
  return payload.value;
}

async function waitFor(label, inspect, accept, timeoutMs = 90_000) {
  const deadline = Date.now() + timeoutMs;
  let last;
  while (Date.now() < deadline) {
    last = await inspect();
    if (accept(last)) return last;
    await sleep(250);
  }
  throw new Error(`${label} timed out: ${JSON.stringify(last)}`);
}

async function main() {
  await waitForHttp(`${SERVER}/v1/info`, 5_000);
  const web = startProcess('python3', ['-m', 'http.server', String(WEB_PORT), '--bind', '127.0.0.1']);
  const driver = startProcess('geckodriver', ['--port', String(DRIVER_PORT)]);
  let sessionId;

  try {
    await Promise.all([
      waitForHttp(`${WEB_URL}/`),
      waitForHttp(`${DRIVER_URL}/status`),
    ]);
    const session = await request('POST', '/session', {
      capabilities: {
        alwaysMatch: {
          browserName: 'firefox',
          unhandledPromptBehavior: 'accept',
          'moz:firefoxOptions': { args: ['-headless'] },
        },
      },
    });
    sessionId = session.sessionId;
    const wd = (method, suffix, body) => request(method, `/session/${sessionId}${suffix}`, body);
    const execute = (script, args = []) => wd('POST', '/execute/sync', { script, args });
    const executeAsync = (script, args = []) => wd('POST', '/execute/async', { script, args });
    await wd('POST', '/timeouts', { implicit: 0, pageLoad: 30_000, script: 120_000 });

    const inspect = () => execute(`
      const number = (id) => Number(document.getElementById(id).textContent.replace(/[^0-9]/g, '') || 0);
      return {
        phase: document.getElementById('phase').textContent.toLowerCase(),
        stage: document.getElementById('boot-stage').textContent,
        error: document.getElementById('boot-error').classList.contains('hidden') ? '' : document.getElementById('boot-error').textContent,
        lastError: document.getElementById('last-error').textContent,
        address: document.getElementById('wallet-address').textContent,
        playerId: document.getElementById('player-id').textContent,
        players: number('player-count'),
        events: number('event-count'),
        balance: number('balance'),
        knownBalance: number('known-balance'),
        assets: [number('asset-w'), number('asset-d'), number('asset-s'), number('asset-a')],
        pending: localStorage.getItem(${JSON.stringify(PENDING_KEY)}),
        log: document.getElementById('log').textContent,
      };
    `);
    const pending = () => execute(`
      const raw = localStorage.getItem(${JSON.stringify(PENDING_KEY)});
      if (!raw) return null;
      const value = JSON.parse(raw);
      return { kind: value.action.kind, stage: value.action.transaction.stage, txid: value.action.transaction.txid };
    `);
    const reload = () => wd('POST', '/refresh', {});

    await wd('POST', '/url', { url: `${WEB_URL}/` });
    await execute(`
      document.getElementById('mode').value = 'regtest';
      document.getElementById('server-url').value = ${JSON.stringify(SERVER)};
      document.getElementById('connect').click();
    `);
    const initial = await waitFor('initial regtest wallet', inspect, (state) => (
      state.stage === 'connected' && state.address.startsWith('tark1') && state.phase === 'fund-wallet' && !state.error
    ));
    console.log(`wallet ready: ${initial.address}`);

    execFileSync(path.join(ROOT, 'scripts/regtest.sh'), ['fund', initial.address, '1000'], {
      cwd: ROOT,
      stdio: 'pipe',
      encoding: 'utf8',
    });
    const preparedIssuance = await waitFor(
      'prepared issuance journal',
      pending,
      (journal) => journal?.kind === 'issuance' && journal.stage === 'prepared',
    );
    await reload();
    const registered = await waitFor('registration recovery', inspect, (state) => (
      state.address === initial.address
        && state.phase === 'playing'
        && state.playerId !== 'not registered'
        && state.balance === 670
        && state.assets.every((amount) => amount === 50)
        && !state.pending
        && !state.error
    ));
    if (registered.playerId !== preparedIssuance.txid) throw new Error('registered player ID differs from prepared issuance txid');
    console.log(`registration recovered: ${registered.playerId}`);

    await reload();
    const replayedRegistration = await waitFor('registration replay', inspect, (state) => (
      state.address === initial.address
        && state.phase === 'playing'
        && state.playerId === registered.playerId
        && state.players >= initial.players + 1
        && !state.pending
    ));

    await execute(`document.querySelector('[data-dir="0"]').click();`);
    const preparedMove = await waitFor(
      'prepared move journal',
      pending,
      (journal) => journal?.kind === 'move' && journal.stage === 'prepared',
    );
    await reload();
    const moved = await waitFor('move recovery', inspect, (state) => (
      state.address === initial.address
        && state.phase === 'playing'
        && state.events >= replayedRegistration.events + 1
        && state.assets[0] === 49
        && !state.pending
        && !state.error
    ));
    console.log(`move recovered: ${preparedMove.txid}`);

    const recipient = await executeAsync(`
      const server = arguments[0];
      const done = arguments[arguments.length - 1];
      (async () => {
        const { default: init, App } = await import('./pkg/arkade_duel.js?v=2.0.0');
        await init();
        const wallet = await App.init(server, undefined, undefined);
        done({ address: wallet.address(), key: wallet.exportKey() });
      })().catch((error) => done({ error: String(error) }));
    `, [SERVER]);
    if (recipient.error || !recipient.address?.startsWith('tark1')) throw new Error(`recipient creation failed: ${JSON.stringify(recipient)}`);
    await execute(`
      window.confirm = () => true;
      document.getElementById('sweep-address').value = arguments[0];
      document.getElementById('sweep-all').click();
    `, [recipient.address]);
    const preparedSweep = await waitFor(
      'prepared sweep journal',
      pending,
      (journal) => journal?.kind === 'sweep' && journal.stage === 'prepared',
    );
    await reload();
    const swept = await waitFor('sweep recovery', inspect, (state) => (
      state.address === initial.address
        && state.balance === 0
        && state.knownBalance === 0
        && !state.pending
        && !state.error
    ));

    const recipientState = await executeAsync(`
      const [server, key] = arguments;
      const done = arguments[arguments.length - 1];
      (async () => {
        const { default: init, App } = await import('./pkg/arkade_duel.js?v=2.0.0');
        await init();
        const wallet = await App.init(server, key, undefined);
        const state = await wallet.step(new Uint8Array(), undefined);
        done({
          address: state.address,
          balance: state.balance,
          knownBalance: state.knownBalance,
          assets: state.walletVtxos.flatMap((vtxo) => vtxo.assets.map((asset) => asset.amount)).sort((a, b) => a - b),
          lastError: state.lastError,
        });
      })().catch((error) => done({ error: String(error) }));
    `, [SERVER, recipient.key]);
    if (recipientState.error
        || recipientState.address !== recipient.address
        || recipientState.balance !== 670
        || recipientState.knownBalance !== 670
        || JSON.stringify(recipientState.assets) !== JSON.stringify([49, 50, 50, 50])) {
      throw new Error(`recipient verification failed: ${JSON.stringify(recipientState)}`);
    }
    console.log(`sweep recovered: ${preparedSweep.txid}`);
    console.log(JSON.stringify({
      registration: registered.playerId,
      move: preparedMove.txid,
      sweep: preparedSweep.txid,
      finalSourceBalance: swept.balance,
      recipientBalance: recipientState.balance,
      recipientAssets: recipientState.assets,
    }));
  } catch (error) {
    console.error(error);
    console.error(`web process output:\n${web.output()}`);
    console.error(`geckodriver output:\n${driver.output()}`);
    process.exitCode = 1;
  } finally {
    if (sessionId) {
      try { await request('DELETE', `/session/${sessionId}`); } catch {}
    }
    web.child.kill('SIGTERM');
    driver.child.kill('SIGTERM');
  }
}

await main();
