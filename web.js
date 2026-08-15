import init, { App } from './pkg/arkade_duel.js?v=2.0.0';

const $ = (id) => document.getElementById(id);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const DIR = { w: 0, d: 1, s: 2, a: 3 };
const SERVER = 'https://mutinynet.arkade.sh';

let app = null;
let snapshot = null;
let inputQueue = [];
let pendingSweep = null;
let driverGeneration = 0;
let currentServer = '';

function text(id, value) {
  $(id).textContent = String(value ?? '');
}

function show(id) {
  $(id).classList.remove('hidden');
}

function hide(id) {
  $(id).classList.add('hidden');
}

function walletStorageKey(server) {
  return `arkade-maze:wallet:v2:${server}`;
}

function pendingStorageKey(server) {
  return `arkade-maze:pending:v2:${server}`;
}

function withTimeout(promise, ms, label) {
  return Promise.race([
    promise,
    new Promise((_, reject) => setTimeout(() => reject(new Error(`${label} timed out after ${Math.round(ms / 1000)}s`)), ms)),
  ]);
}

function readStoredKey(server) {
  return localStorage.getItem(walletStorageKey(server));
}

function persistKey(server, secret) {
  const key = walletStorageKey(server);
  localStorage.setItem(key, secret);
  if (localStorage.getItem(key) !== secret) throw new Error('Wallet key could not be verified in browser storage');
}

function persistPending(server, raw) {
  const key = pendingStorageKey(server);
  if (!raw) {
    localStorage.removeItem(key);
    if (localStorage.getItem(key) !== null) throw new Error('Finalized transaction journal could not be removed from browser storage');
    return;
  }
  localStorage.setItem(key, raw);
  if (localStorage.getItem(key) !== raw) throw new Error('Pending transaction journal could not be verified in browser storage');
}

function persistAppPending(instance) {
  persistPending(currentServer, instance.exportPending());
}

async function copy(value, button) {
  try {
    await navigator.clipboard.writeText(value);
    const before = button.textContent;
    button.textContent = 'Copied';
    setTimeout(() => { button.textContent = before; }, 900);
  } catch (error) {
    showError(`Clipboard failed: ${error}`);
  }
}

function showError(message) {
  text('boot-error', message);
  show('boot-error');
}

async function connectApp() {
  const generation = ++driverGeneration;
  app = null;
  snapshot = null;
  hide('app');
  hide('boot-error');
  text('phase', 'CONNECTING');
  text('boot-stage', 'reading /v1/info');

  const server = SERVER;
  currentServer = server;

  const storedKey = readStoredKey(server);
  const storedPending = localStorage.getItem(pendingStorageKey(server));
  const instance = await withTimeout(
    App.init(server, storedKey || undefined, storedPending || undefined),
    25_000,
    'Arkade connection',
  );
  const actualKey = instance.exportKey();
  if (storedKey && actualKey !== storedKey) throw new Error('Loaded wallet key does not match browser storage');
  if (!storedKey) persistKey(server, actualKey);

  if (generation !== driverGeneration) return;
  app = instance;
  persistAppPending(instance);
  text('wallet-address', instance.address());
  text('game-address', instance.gameAddress());
  text('recovery-key', instance.exportRecovery());
  snapshot = instance.snapshot();
  applySnapshot(snapshot);
  text('boot-stage', storedKey ? 'restored wallet; starting sync' : 'persisted wallet; starting sync');
  show('app');
  bindWalletAddressActions();
  driver(instance, generation);
}

function bindWalletAddressActions() {
  $('copy-wallet').onclick = () => copy(app.address(), $('copy-wallet'));
  $('copy-recovery').onclick = () => copy(app.exportRecovery(), $('copy-recovery'));
}

function parseImportedSecret(raw) {
  const value = raw.trim();
  if (!value) throw new Error('Enter a secret key or recovery JSON');
  if (/^[0-9a-fA-F]{64}$/.test(value)) return { secretKey: value.toLowerCase() };
  const recovery = JSON.parse(value);
  if (!/^[0-9a-fA-F]{64}$/.test(recovery.secretKey || '')) throw new Error('Recovery JSON has no valid secretKey');
  return { ...recovery, secretKey: recovery.secretKey.toLowerCase() };
}

function bindStaticActions() {
  $('connect').addEventListener('click', () => connectApp().catch((error) => {
    text('phase', 'CONNECT FAILED');
    text('boot-stage', 'not connected');
    showError(String(error));
  }));

  $('import-wallet').addEventListener('click', () => {
    try {
      const recovery = parseImportedSecret($('import-key').value);
      if (recovery.server && recovery.server.replace(/\/+$/, '') !== SERVER) {
        throw new Error('Recovery bundle is not for Mutinynet');
      }
      persistKey(SERVER, recovery.secretKey);
      persistPending(SERVER, recovery.pending ? JSON.stringify(recovery.pending) : '');
      location.reload();
    } catch (error) {
      showError(`Import failed: ${error}`);
    }
  });

  $('forget-wallet').addEventListener('click', () => {
    const controlled = snapshot?.knownBalance || snapshot?.pending || snapshot?.sending;
    const warning = controlled
      ? 'This wallet still has known funds or a pending transaction. Forgetting it can lose access. Continue?'
      : 'Forget this server wallet and create a new one?';
    if (!confirm(warning)) return;
    localStorage.removeItem(walletStorageKey(currentServer));
    localStorage.removeItem(pendingStorageKey(currentServer));
    location.reload();
  });

  $('sweep-all').addEventListener('click', () => {
    const destination = $('sweep-address').value.trim();
    if (!destination) return showError('Enter a sweep destination');
    if (!snapshot || snapshot.pending || snapshot.sending || pendingSweep) return showError('Wallet is busy');
    if (!confirm(`Sweep all safely spendable sats and assets to ${destination}? Game assets will stop being playable here.`)) return;
    pendingSweep = destination;
    text('wallet-action', 'sweep queued');
    $('sweep-all').disabled = true;
  });

  addEventListener('keydown', (event) => {
    if (event.repeat || event.ctrlKey || event.metaKey || event.altKey) return;
    const direction = DIR[event.key.toLowerCase()];
    if (direction === undefined) return;
    event.preventDefault();
    queueMove(direction);
  });
  for (const button of document.querySelectorAll('[data-dir]')) {
    button.addEventListener('click', () => queueMove(Number(button.dataset.dir)));
  }
  addEventListener('beforeunload', (event) => {
    if (snapshot?.pending || snapshot?.sending) {
      event.preventDefault();
      event.returnValue = '';
    }
  });
}

function queueMove(direction) {
  const totalQueued = (snapshot?.queued ?? 0) + inputQueue.length;
  if (!snapshot || snapshot.phase !== 'playing' || snapshot.pending || totalQueued >= 16) return;
  if ((snapshot.moveBalances?.[direction] ?? 0) <= 0) return;
  inputQueue.push(direction);
  updateQueueCount();
}

function updateQueueCount() {
  text('queue-count', (snapshot?.queued ?? 0) + inputQueue.length);
}

async function driver(instance, generation) {
  let firstSync = true;
  while (app === instance && generation === driverGeneration) {
    const directions = inputQueue;
    const sweep = pendingSweep;
    inputQueue = [];
    pendingSweep = null;
    updateQueueCount();
    try {
      if (firstSync) text('boot-stage', 'syncing wallet and game');
      const nextSnapshot = await withTimeout(
        instance.step(new Uint8Array(directions), sweep || undefined),
        70_000,
        'State sync',
      );
      try {
        persistAppPending(instance);
      } catch (error) {
        text('boot-stage', 'wallet persistence failed; transaction halted');
        showError(`Transaction journal failed: ${String(error)}`);
        return;
      }
      snapshot = nextSnapshot;
      applySnapshot(snapshot);
      hide('boot-error');
      text('boot-stage', 'connected');
      firstSync = false;
    } catch (error) {
      console.error('game step failed', error);
      showError(`State sync failed: ${String(error)}`);
    }
    await sleep(1000);
  }
}

function age(ms) {
  if (!ms) return 'never';
  return `${Math.max(0, Math.round((Date.now() - ms) / 1000))}s ago`;
}

function applySnapshot(state) {
  if (app) text('recovery-key', app.exportRecovery());
  text('version', `v${state.version}`);
  text('phase', state.phase.toUpperCase());
  text('network', state.network);
  text('operator', `${state.server} | ${state.operatorVersion} | ${state.signer}`);
  text('wallet-address', state.address);
  text('game-address', state.gameAddress);
  text('wallet-action', state.walletAction);
  text('balance', `${state.balance.toLocaleString()} sats`);
  text('known-balance', `${state.knownBalance.toLocaleString()} sats`);
  text('pending-tx', state.pendingTxid || 'none');
  text('player-id', state.playerId || 'not registered');
  text('player-count', state.players.length);
  text('event-count', state.events);
  text('sync', `${age(state.lastSyncMs)} / ${state.events} moves`);
  text('wallet-sync', age(state.walletSyncMs));
  text('last-error', state.lastError || 'none');
  text('log', (state.log || []).join('\n'));

  const balances = state.moveBalances || [0, 0, 0, 0];
  text('asset-w', balances[0]);
  text('asset-d', balances[1]);
  text('asset-s', balances[2]);
  text('asset-a', balances[3]);
  updateQueueCount();

  if (state.phase === 'fund-wallet') {
    const missing = Math.max(0, state.requiredFunding - state.balance);
    text('funding-note', state.fundingReady
      ? 'Funding inputs are ready. Registration will run automatically.'
      : `Fund this address. Minimum ${state.requiredFunding} sats; currently missing at least ${missing} sats.`);
  } else if (state.phase === 'issuing') {
    text('funding-note', 'Registration submitted or waiting for its indexed asset carrier.');
  } else if (state.phase === 'syncing') {
    text('funding-note', 'Wallet assets found. Full player history is being verified.');
  } else {
    text('funding-note', 'Wallet is ready. Each move burns one asset and recycles the carrier sats.');
  }

  if (state.network === 'mutinynet') {
    text('faucet-note', `Use "Send to Arkade" at https://faucet.mutinynet.com for ${state.address}; do not use its on-chain address field.`);
    show('faucet-note');
  } else if (state.network === 'regtest') {
    text('faucet-note', `Local funding: ./scripts/regtest.sh fund ${state.address} 1000`);
    show('faucet-note');
  } else {
    hide('faucet-note');
  }

  const controlsEnabled = state.phase === 'playing' && !state.sending && !state.pending;
  for (const button of document.querySelectorAll('[data-dir]')) {
    const direction = Number(button.dataset.dir);
    button.disabled = !controlsEnabled || balances[direction] <= 0;
  }
  $('sweep-all').disabled = state.pending || state.sending || state.balance <= 0;
  text('board-status', state.pending ? `pending ${state.pendingTxid || ''}` : state.phase);
  renderVtxos(state.walletVtxos || []);
}

function renderVtxos(vtxos) {
  const body = $('vtxos');
  body.replaceChildren();
  if (!vtxos.length) {
    const row = body.insertRow();
    const cell = row.insertCell();
    cell.colSpan = 6;
    cell.textContent = 'none';
    return;
  }
  for (const vtxo of vtxos) {
    const row = body.insertRow();
    const assetText = vtxo.assets.map((asset) => `${asset.assetId}:${asset.amount}`).join('\n') || '-';
    const values = [
      vtxo.outpoint,
      vtxo.amount.toLocaleString(),
      vtxo.status,
      vtxo.expiresAt ? new Date(vtxo.expiresAt * 1000).toLocaleString() : '-',
      assetText,
      vtxo.spentBy || '-',
    ];
    for (const value of values) row.insertCell().textContent = value;
  }
}

function playerColor(id) {
  const hue = parseInt(id.slice(0, 6), 16) % 360;
  return `hsl(${hue} 70% 60%)`;
}

function render() {
  const canvas = $('canvas');
  const rect = canvas.getBoundingClientRect();
  const ratio = Math.min(devicePixelRatio || 1, 2);
  const width = Math.max(1, Math.round(rect.width * ratio));
  const height = Math.max(1, Math.round(rect.height * ratio));
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }
  const context = canvas.getContext('2d');
  context.fillStyle = '#0b0b0b';
  context.fillRect(0, 0, width, height);
  if (snapshot?.mazeWidth) drawGame(context, width, height, snapshot);
  requestAnimationFrame(render);
}

function drawGame(context, width, height, state) {
  const cell = Math.min(width / state.mazeWidth, height / state.mazeHeight);
  const ox = (width - cell * state.mazeWidth) / 2;
  const oy = (height - cell * state.mazeHeight) / 2;
  context.fillStyle = '#222';
  for (const [x, y] of state.walls) context.fillRect(ox + x * cell, oy + y * cell, cell, cell);
  context.fillStyle = '#275';
  context.fillRect(ox + state.start[0] * cell, oy + state.start[1] * cell, cell, cell);
  context.fillStyle = '#733';
  context.fillRect(ox + state.goal[0] * cell, oy + state.goal[1] * cell, cell, cell);
  for (const player of state.players) {
    context.beginPath();
    context.arc(ox + (player.x + 0.5) * cell, oy + (player.y + 0.5) * cell, Math.max(3, cell * 0.25), 0, Math.PI * 2);
    context.fillStyle = playerColor(player.id);
    context.fill();
    if (player.isMe) {
      context.strokeStyle = '#fff';
      context.lineWidth = Math.max(1, cell * 0.08);
      context.stroke();
    }
  }
}

async function boot() {
  bindStaticActions();
  requestAnimationFrame(render);
  text('boot-stage', 'loading WASM');
  await withTimeout(init(), 20_000, 'WASM initialization');
  await connectApp();
}

boot().catch((error) => {
  text('phase', 'BOOT FAILED');
  text('boot-stage', 'not connected');
  showError(`Could not start Arkade Maze: ${String(error)}`);
});
