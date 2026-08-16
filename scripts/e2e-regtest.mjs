#!/usr/bin/env node
import { execFileSync, spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import net from 'node:net';
import path from 'node:path';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const WEB_PORT = Number(process.env.ARKADE_E2E_WEB_PORT || 18775);
const DRIVER_PORT = Number(process.env.ARKADE_E2E_DRIVER_PORT || 14454);
const WEB_URL = `http://127.0.0.1:${WEB_PORT}`;
const DRIVER_URL = `http://127.0.0.1:${DRIVER_PORT}`;
const SERVER = 'http://127.0.0.1:7070';
const PENDING_KEY = `arkade-arena:pending:v3:${SERVER}`;
const PREPARED_MARKER = 'arkade-city:e2e-prepared';

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function startProcess(command, args) {
  const child = spawn(command, args, { cwd: ROOT, stdio: ['ignore', 'pipe', 'pipe'] });
  let output = '';
  let spawnError = null;
  const collect = (chunk) => {
    output += chunk.toString();
    if (output.length > 20_000) output = output.slice(-20_000);
  };
  child.stdout.on('data', collect);
  child.stderr.on('data', collect);
  child.on('error', (error) => {
    spawnError = error;
    collect(`${command} failed to start: ${error.message}\n`);
  });
  return { child, output: () => output, spawnError: () => spawnError };
}

function assertPortAvailable(port, label) {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.once('error', (error) => reject(new Error(`${label} port ${port} is unavailable: ${error.message}`)));
    server.listen({ host: '127.0.0.1', port, exclusive: true }, () => server.close(resolve));
  });
}

async function waitForHttp(url, timeoutMs = 20_000, process = null) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (process?.spawnError()) {
      throw new Error(`process failed while waiting for ${url}:\n${process.output()}`);
    }
    if (process && (process.child.exitCode !== null || process.child.signalCode !== null)) {
      throw new Error(`process exited while waiting for ${url}:\n${process.output()}`);
    }
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
    try {
      last = await inspect();
    } catch (error) {
      last = { inspectionError: String(error) };
      await sleep(250);
      continue;
    }
    if (accept(last)) return last;
    await sleep(250);
  }
  throw new Error(`${label} timed out: ${JSON.stringify(last)}`);
}

async function main() {
  await waitForHttp(`${SERVER}/v1/info`, 5_000);
  await Promise.all([
    assertPortAvailable(WEB_PORT, 'web server'),
    assertPortAvailable(DRIVER_PORT, 'WebDriver'),
  ]);
  const web = startProcess('python3', ['-m', 'http.server', String(WEB_PORT), '--bind', '127.0.0.1']);
  const driver = startProcess('geckodriver', ['--port', String(DRIVER_PORT)]);
  let sessionId;

  try {
    await Promise.all([
      waitForHttp(`${WEB_URL}/`, 20_000, web),
      waitForHttp(`${DRIVER_URL}/status`, 20_000, driver),
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
        title: document.title,
        phase: document.getElementById('entry-panel').classList.contains('hidden')
          ? 'playing'
          : document.getElementById('enter-game').textContent.startsWith('Entering') ? 'issuing' : 'fund-wallet',
        network: document.getElementById('network').textContent,
        networkLabel: document.getElementById('network-label').textContent,
        error: document.getElementById('boot-error').classList.contains('hidden') ? '' : document.getElementById('boot-error').textContent,
        lastError: document.getElementById('last-error').textContent,
        address: document.getElementById('wallet-address').textContent,
        playerId: document.getElementById('player-id').textContent,
        players: number('player-count'),
        events: number('event-count'),
        position: (() => {
          const me = globalThis.__ARKADE_E2E_SNAPSHOT?.players?.find((player) => player.isMe);
          return me ? [me.x, me.y] : null;
        })(),
        balance: number('balance'),
        knownBalance: number('known-balance'),
        assets: [number('asset-w'), number('asset-d'), number('asset-s'), number('asset-a'), number('asset-bullet'), number('asset-life')],
        canMintPack: Boolean(globalThis.__ARKADE_E2E_SNAPSHOT?.canMintPack),
        enterDisabled: document.getElementById('enter-game').disabled,
        pending: localStorage.getItem(${JSON.stringify(PENDING_KEY)}),
        log: document.getElementById('log').textContent,
        vtxos: [...document.querySelectorAll('#vtxos tr')].flatMap((row) => row.cells.length === 6 ? [{
          outpoint: row.cells[0].textContent,
          status: row.cells[2].textContent,
          spentBy: row.cells[5].textContent,
        }] : []),
      };
    `);
    const preparedMarker = () => execute(`
      const raw = sessionStorage.getItem(${JSON.stringify(PREPARED_MARKER)});
      if (!raw) return null;
      const value = JSON.parse(raw);
      return { ...value, reloaded: value.pageOrigin !== performance.timeOrigin };
    `);
    const armPreparedReload = (kind) => execute(`
      const expectedKind = arguments[0];
      const pendingKey = ${JSON.stringify(PENDING_KEY)};
      const markerKey = ${JSON.stringify(PREPARED_MARKER)};
      sessionStorage.removeItem(markerKey);
      globalThis.__ARKADE_E2E_PAUSE_AFTER_PREPARE = expectedKind;
      const timer = setInterval(() => {
        const raw = localStorage.getItem(pendingKey);
        if (!raw) return;
        const value = JSON.parse(raw);
        if (value.action.kind !== expectedKind || value.action.transaction.stage !== 'prepared') return;
        clearInterval(timer);
        sessionStorage.setItem(markerKey, JSON.stringify({
          kind: value.action.kind,
          stage: value.action.transaction.stage,
          txid: value.action.transaction.txid,
          pageOrigin: performance.timeOrigin,
        }));
        location.reload();
      }, 10);
    `, [kind]);
    const reload = () => wd('POST', '/refresh', {});

    await wd('POST', '/url', { url: `${WEB_URL}/?network=regtest&e2e=1` });
    const initial = await waitFor('initial regtest wallet', inspect, (state) => (
      state.title === 'Arkade City'
        && state.network === 'regtest'
        && state.networkLabel === 'Local regtest'
        && state.address.startsWith('tark1')
        && state.phase === 'fund-wallet'
        && !state.error
    ));
    console.log(`wallet ready: ${initial.address}`);

    execFileSync(path.join(ROOT, 'scripts/regtest.sh'), ['fund', initial.address, '1000'], {
      cwd: ROOT,
      stdio: 'pipe',
      encoding: 'utf8',
    });
    await waitFor('funded wallet', inspect, (state) => (
      state.address === initial.address && state.balance === 1000 && !state.enterDisabled
    ));
    await armPreparedReload('issuance');
    await execute(`document.getElementById('enter-game').click();`);
    const preparedIssuance = await waitFor(
      'prepared issuance reload',
      preparedMarker,
      (journal) => journal?.kind === 'issuance' && journal.stage === 'prepared' && journal.reloaded,
    );
    const registered = await waitFor('registration recovery', inspect, (state) => (
      state.address === initial.address
        && state.phase === 'playing'
        && state.playerId !== 'not registered'
        && state.balance === 670
        && JSON.stringify(state.assets) === JSON.stringify([50, 50, 50, 50, 50, 5])
        && !state.pending
        && !state.error
        && state.lastError === 'none'
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
        && state.lastError === 'none'
    ));

    execFileSync(path.join(ROOT, 'scripts/regtest.sh'), ['fund', initial.address, '660'], {
      cwd: ROOT,
      stdio: 'pipe',
      encoding: 'utf8',
    });
    await waitFor('funded action-pack mint', inspect, (state) => (
      state.address === initial.address && state.balance === 1330 && state.canMintPack
    ));
    await execute(`
      window.confirm = () => true;
      document.getElementById('mint-pack').click();
    `);
    const mintedPack = await waitFor('new action pack mint', inspect, (state) => (
      state.address === initial.address
        && state.phase === 'playing'
        && state.playerId !== replayedRegistration.playerId
        && state.players >= replayedRegistration.players + 1
        && state.balance === 1000
        && JSON.stringify(state.assets) === JSON.stringify([50, 50, 50, 50, 50, 5])
        && !state.pending
        && !state.error
        && state.lastError === 'none'
    ));
    console.log(`new action pack minted: ${mintedPack.playerId}`);

    await reload();
    const replayedMint = await waitFor('new action pack replay', inspect, (state) => (
      state.address === initial.address
        && state.phase === 'playing'
        && state.playerId === mintedPack.playerId
        && JSON.stringify(state.assets) === JSON.stringify([50, 50, 50, 50, 50, 5])
        && !state.pending
        && state.lastError === 'none'
    ));

    const moveDirection = await execute(`
      const state = globalThis.__ARKADE_E2E_SNAPSHOT;
      const me = state.players.find((player) => player.isMe);
      const walls = new Set(state.walls.map(([x, y]) => x + ',' + y));
      return [[0, -1], [1, 0], [0, 1], [-1, 0]]
        .findIndex(([dx, dy]) => !walls.has((me.x + dx) + ',' + (me.y + dy)));
    `);
    if (moveDirection < 0) throw new Error('player spawned without an open neighboring cell');
    await waitFor(
      'lock-in action window',
      () => execute(`return !document.querySelector('[data-dir="' + arguments[0] + '"]').disabled;`, [moveDirection]),
      Boolean,
    );
    await armPreparedReload('move');
    await execute(`document.querySelector('[data-dir="' + arguments[0] + '"]').click();`, [moveDirection]);
    const preparedMove = await waitFor(
      'prepared move reload',
      preparedMarker,
      (journal) => journal?.kind === 'move' && journal.stage === 'prepared' && journal.reloaded,
    );
    await waitFor('move recovery and authoritative indexing', inspect, (state) => (
      state.address === initial.address
        && state.phase === 'playing'
        && state.events >= replayedMint.events + 1
        && state.assets[moveDirection] === 49
        && JSON.stringify(state.position) !== JSON.stringify(replayedMint.position)
        && state.vtxos.some((vtxo) => vtxo.outpoint.startsWith(`${preparedMove.txid}:`))
        && !state.pending
        && !state.error
        && state.lastError === 'none'
    ), 120_000);
    console.log(`move recovered: ${preparedMove.txid}`);

    const recipient = await executeAsync(`
      const server = arguments[0];
      const done = arguments[arguments.length - 1];
      (async () => {
        const module = await import('./pkg-regtest/arkade_city.js');
        await module.default();
        const wallet = await module.App.init(server, undefined, undefined);
        done({ address: wallet.address(), key: wallet.exportKey() });
      })().catch((error) => done({ error: String(error) }));
    `, [SERVER]);
    if (recipient.error || !recipient.address?.startsWith('tark1')) throw new Error(`recipient creation failed: ${JSON.stringify(recipient)}`);
    await armPreparedReload('sweep');
    await execute(`
      window.confirm = () => true;
      document.getElementById('sweep-address').value = arguments[0];
      document.getElementById('sweep-all').click();
    `, [recipient.address]);
    const preparedSweep = await waitFor(
      'prepared sweep reload',
      preparedMarker,
      (journal) => journal?.kind === 'sweep' && journal.stage === 'prepared' && journal.reloaded,
    );
    const swept = await waitFor('sweep recovery', inspect, (state) => (
      state.address === initial.address
        && state.balance === 0
        && state.knownBalance === 0
        && !state.pending
        && !state.error
        && state.lastError === 'none'
    ));

    const recipientState = await executeAsync(`
      const [server, key] = arguments;
      const done = arguments[arguments.length - 1];
      (async () => {
        const module = await import('./pkg-regtest/arkade_city.js');
        await module.default();
        const wallet = await module.App.init(server, key, undefined);
        const state = await wallet.step(new Uint8Array(), false, false, undefined);
        done({
          address: state.address,
          balance: state.balance,
          knownBalance: state.knownBalance,
          assets: state.walletVtxos.flatMap((vtxo) => vtxo.assets.map((asset) => asset.amount)).sort((a, b) => a - b),
          outpoints: state.walletVtxos.map((vtxo) => vtxo.outpoint),
          lastError: state.lastError,
        });
      })().catch((error) => done({ error: String(error) }));
    `, [SERVER, recipient.key]);
    if (recipientState.error
        || recipientState.address !== recipient.address
        || recipientState.balance !== 1000
        || recipientState.knownBalance !== 1000
        || JSON.stringify(recipientState.assets) !== JSON.stringify([5, 5, 49, 50, 50, 50, 50, 50, 50, 50, 50, 50])
        || !recipientState.outpoints.some((outpoint) => outpoint.startsWith(`${preparedSweep.txid}:`))
        || recipientState.lastError) {
      throw new Error(`recipient verification failed: ${JSON.stringify(recipientState)}`);
    }
    console.log(`sweep recovered: ${preparedSweep.txid}`);
    console.log(JSON.stringify({
      registration: registered.playerId,
      mintedPlayer: mintedPack.playerId,
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
