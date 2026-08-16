const query = new URLSearchParams(location.search);
const REGTEST_MODE = ['127.0.0.1', 'localhost'].includes(location.hostname)
  && query.get('network') === 'regtest';
const E2E_MODE = REGTEST_MODE && query.get('e2e') === '1';
const ASSET_REVISION = '3.0.0-fast2';
const packageUrl = REGTEST_MODE
  ? `./pkg-regtest/arkade_city.js?v=${ASSET_REVISION}`
  : `./pkg/arkade_city.js?v=${ASSET_REVISION}`;
const wasmUrl = REGTEST_MODE
  ? `./pkg-regtest/arkade_city_bg.wasm?v=${ASSET_REVISION}`
  : `./pkg/arkade_city_bg.wasm?v=${ASSET_REVISION}`;
const { default: init, App } = await import(packageUrl);

const $ = (id) => document.getElementById(id);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const DIR = { w: 0, d: 1, s: 2, a: 3 };
const SHOOT = 4;
const REVIVE = 5;
const POLL_MS = 100;
const TRACE_MS = 620;
const ACTION_LABELS = ['MOVE UP', 'MOVE RIGHT', 'MOVE DOWN', 'MOVE LEFT', 'SHOOT', 'REVIVE'];
const SERVER = REGTEST_MODE ? 'http://127.0.0.1:7070' : 'https://mutinynet.arkade.sh';

let app = null;
let snapshot = null;
let inputQueue = [];
let pendingEnterGame = false;
let pendingSweep = null;
let driverGeneration = 0;
let currentServer = '';
let driverWakePending = false;
let driverWakeResolve = null;
let inFlightDirections = [];
let selectedBrowserAction = null;
const seenShotTraces = new Set();
let activeShotTraces = [];
let canvasDirty = true;
let leaderboardKey = null;
let vtxosKey = null;

function text(id, value) {
  const element = $(id);
  const next = String(value ?? '');
  if (element.textContent !== next) element.textContent = next;
}

function show(id) {
  $(id).classList.remove('hidden');
}

function hide(id) {
  $(id).classList.add('hidden');
}

function walletStorageKey(server) {
  return `arkade-arena:wallet:v3:${server}`;
}

function pendingStorageKey(server) {
  return `arkade-arena:pending:v3:${server}`;
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
  const raw = instance.exportPending();
  persistPending(currentServer, raw);
  return raw;
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
  text('wallet-address-detail', instance.address());
  text('recovery-key', instance.exportRecovery());
  snapshot = instance.snapshot();
  applySnapshot(snapshot);
  show('app');
  bindWalletAddressActions();
  driver(instance, generation);
}

function bindWalletAddressActions() {
  $('copy-wallet').onclick = () => copy(app.address(), $('copy-wallet'));
  $('copy-wallet-detail').onclick = () => copy(app.address(), $('copy-wallet-detail'));
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
  addEventListener('resize', invalidateCanvas);
  $('connect').addEventListener('click', () => connectApp().catch((error) => {
    showError(String(error));
  }));

  $('enter-game').addEventListener('click', () => {
    if (!snapshot?.fundingReady || snapshot.pending || snapshot.sending) return;
    pendingEnterGame = true;
    $('enter-game').disabled = true;
    $('enter-game').textContent = 'Entering…';
    wakeDriver();
  });

  $('import-wallet').addEventListener('click', () => {
    try {
      const recovery = parseImportedSecret($('import-key').value);
      if (recovery.server && recovery.server.replace(/\/+$/, '') !== SERVER) {
        throw new Error('Recovery bundle is not for this Arkade server');
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
    wakeDriver();
  });

  addEventListener('keydown', (event) => {
    if (event.repeat || event.ctrlKey || event.metaKey || event.altKey) return;
    const key = event.key.toLowerCase();
    const direction = event.code === 'Space' ? SHOOT : key === 'r' ? REVIVE : DIR[key];
    if (direction === undefined) return;
    event.preventDefault();
    queueMove(direction);
  });
  for (const button of document.querySelectorAll('[data-dir]')) {
    button.addEventListener('click', () => queueMove(Number(button.dataset.dir)));
  }
  for (const button of document.querySelectorAll('[data-action]')) {
    button.addEventListener('click', () => queueMove(Number(button.dataset.action)));
  }
  addEventListener('beforeunload', (event) => {
    if (!E2E_MODE && (snapshot?.pending || snapshot?.sending || snapshot?.queued || inputQueue.length)) {
      event.preventDefault();
      event.returnValue = '';
    }
  });
}

function queueMove(direction) {
  const totalQueued = (snapshot?.queued ?? 0) + inputQueue.length;
  if (!snapshot || snapshot.phase !== 'playing' || !snapshot.canAct
      || selectedBrowserAction !== null || totalQueued >= 1) return;
  const me = snapshot.players.find((player) => player.isMe);
  if (!me || (direction === REVIVE ? me.hp > 0 : me.hp <= 0)) return;
  const reserved = [
    ...inFlightDirections,
    ...inputQueue,
  ].filter((action) => action === direction).length;
  if ((snapshot.moveBalances?.[direction] ?? 0) <= reserved) return;
  selectedBrowserAction = direction;
  inputQueue.push(direction);
  updateQueueCount();
  updateControls(snapshot);
  invalidateCanvas();
  if (!snapshot.pending) wakeDriver();
}

function updateQueueCount() {
  const queued = (snapshot?.queued ?? 0) + inputQueue.length;
  const action = selectedBrowserAction ?? snapshot?.projectedAction;
  const submitting = action !== null && action !== undefined
    && (snapshot?.pending || queued > 0 || inFlightDirections.length > 0);
  const waiting = action !== null && action !== undefined && !submitting;
  const status = submitting
    ? `Submitting ${ACTION_LABELS[action]}`
    : waiting
      ? `Waiting for index: ${ACTION_LABELS[action]}`
      : 'Ready';
  text('queue-status', status);
  $('queue-status').classList.toggle('active', submitting || waiting);
}

function wakeDriver() {
  driverWakePending = true;
  if (driverWakeResolve) {
    driverWakeResolve();
    driverWakeResolve = null;
  }
}

async function waitForDriver(delay) {
  if (driverWakePending) {
    driverWakePending = false;
    return;
  }
  await Promise.race([
    sleep(delay),
    new Promise((resolve) => { driverWakeResolve = resolve; }),
  ]);
  driverWakeResolve = null;
  driverWakePending = false;
}

async function driver(instance, generation) {
  while (app === instance && generation === driverGeneration) {
    const tickStarted = performance.now();
    const wasPending = snapshot?.pending ?? false;
    const canSendMoves = snapshot?.phase === 'playing'
      && !wasPending;
    const directions = canSendMoves ? inputQueue : [];
    const enterGame = pendingEnterGame;
    const sweep = pendingSweep;
    if (canSendMoves) inputQueue = [];
    inFlightDirections = directions;
    pendingEnterGame = false;
    pendingSweep = null;
    try {
      const nextSnapshot = await instance.step(
        new Uint8Array(directions),
        enterGame,
        sweep || undefined,
      );
      let rawPending;
      try {
        rawPending = persistAppPending(instance);
      } catch (error) {
        showError(`Transaction journal failed: ${String(error)}`);
        return;
      }
      snapshot = nextSnapshot;
      inFlightDirections = [];
      applySnapshot(snapshot);
      hide('boot-error');
      if (E2E_MODE && rawPending) {
        const pending = JSON.parse(rawPending);
        if (pending.action?.transaction?.stage === 'prepared'
            && globalThis.__ARKADE_E2E_PAUSE_AFTER_PREPARE === pending.action.kind) {
          while (app === instance
              && generation === driverGeneration
              && globalThis.__ARKADE_E2E_PAUSE_AFTER_PREPARE === pending.action.kind) {
            await sleep(25);
          }
        }
      }
    } catch (error) {
      console.error('game step failed', error);
      showError(`State sync failed: ${String(error)}`);
    }
    const queued = (snapshot?.queued ?? 0) + inputQueue.length;
    const continueImmediately = (!wasPending && snapshot?.pending)
      || (wasPending && !snapshot?.pending && queued > 0)
      || pendingEnterGame
      || pendingSweep;
    if (!continueImmediately) {
      await waitForDriver(Math.max(0, POLL_MS - (performance.now() - tickStarted)));
    }
  }
}

function age(ms) {
  if (!ms) return 'never';
  return `${Math.max(0, Math.round((Date.now() - ms) / 1000))}s ago`;
}

function applySnapshot(state) {
  if (selectedBrowserAction !== null
      && (state.projectedAction === null || state.projectedAction === undefined)
      && !state.pending
      && !state.queued
      && inputQueue.length === 0
      && inFlightDirections.length === 0) {
    selectedBrowserAction = null;
  }
  if (E2E_MODE) globalThis.__ARKADE_E2E_SNAPSHOT = state;
  invalidateCanvas();
  if (app) text('recovery-key', app.exportRecovery());
  text('version', `v${state.version}`);
  text('network', state.network);
  text('operator', `${state.server} | ${state.operatorVersion} | ${state.signer}`);
  text('wallet-address', state.address);
  text('wallet-address-detail', state.address);
  text('wallet-action', state.walletAction);
  text('balance', `${state.balance.toLocaleString()} sats`);
  text('known-balance', `${state.knownBalance.toLocaleString()} sats`);
  text('pending-tx', state.pendingTxid || 'none');
  text('player-id', state.playerId || 'not registered');
  text('player-count', state.players.length);
  text('event-count', state.events);
  observeShotTraces(state.shotTraces || []);
  text('sync', `${age(state.lastSyncMs)} / ${state.events} actions`);
  text('wallet-sync', age(state.walletSyncMs));
  text('last-error', state.lastError || 'none');
  text('log', (state.log || []).join('\n'));
  text('required-funding', state.requiredFunding.toLocaleString());
  text('wallet-summary', `${state.balance.toLocaleString()} sats`);
  text('network-label', REGTEST_MODE ? 'Local regtest' : 'Mutinynet');
  if (REGTEST_MODE) {
    text('funding-help', `Local funding: ./scripts/regtest.sh fund ${state.address} 1000`);
  }

  const balances = state.moveBalances || [0, 0, 0, 0, 0, 0];
  text('asset-w', balances[0]);
  text('asset-d', balances[1]);
  text('asset-s', balances[2]);
  text('asset-a', balances[3]);
  text('asset-bullet', balances[4]);
  text('asset-life', balances[5]);
  const movesRemaining = balances.slice(0, 4).reduce((total, amount) => total + amount, 0);
  text('move-total', movesRemaining);
  $('move-stock').classList.toggle('depleted', Boolean(state.playerId) && movesRemaining === 0);
  $('ammo-stock').classList.toggle('depleted', Boolean(state.playerId) && balances[4] === 0);
  text('shoot-balance', balances[4]);
  text('revive-balance', balances[5]);
  const me = state.players.find((player) => player.isMe);
  text('local-hp', me?.hp ?? '-');
  text('max-hp', state.maxHp);
  for (const [index, pip] of [...document.querySelectorAll('.hp-pips i')].entries()) {
    pip.classList.toggle('empty', !me || index >= me.hp);
  }
  text('local-kills', me?.kills ?? 0);
  renderLeaderboard(state.players);
  updateQueueCount();

  if (state.phase === 'fund-wallet') {
    const missing = Math.max(0, state.requiredFunding - state.balance);
    text('funding-note', state.fundingReady
      ? `Funded with ${state.balance.toLocaleString()} sats. Click Enter game when you are ready.`
      : `Balance: ${state.balance.toLocaleString()} sats. Send at least ${missing.toLocaleString()} more sats to the player address above.`);
  } else if (state.phase === 'issuing') {
    text('funding-note', 'Entering the game: creating and indexing your registration.');
  } else if (state.phase === 'syncing') {
    text('funding-note', 'Registration found. Verifying the shared game history.');
  } else {
    text('funding-note', 'You are in the game.');
  }

  const entering = state.phase === 'issuing' || state.phase === 'syncing';
  const showEntry = state.phase === 'fund-wallet' || entering;
  showEntry ? show('entry-panel') : hide('entry-panel');
  $('enter-game').disabled = state.phase !== 'fund-wallet' || !state.fundingReady || state.pending || state.sending || pendingEnterGame;
  $('enter-game').textContent = entering || pendingEnterGame ? 'Entering…' : 'Enter game';

  updateControls(state);
  $('sweep-all').disabled = state.pending || state.sending || state.balance <= 0;
  renderVtxos(state.walletVtxos || []);
}

function updateControls(state) {
  const me = state.players.find((player) => player.isMe);
  const balances = state.moveBalances || [0, 0, 0, 0, 0, 0];
  const controlsEnabled = state.phase === 'playing'
    && state.canAct
    && !state.sending
    && !state.pending
    && inputQueue.length === 0
    && inFlightDirections.length === 0
    && selectedBrowserAction === null;
  for (const button of document.querySelectorAll('[data-dir]')) {
    const direction = Number(button.dataset.dir);
    button.disabled = !controlsEnabled || !me || me.hp <= 0 || balances[direction] <= 0;
  }
  for (const button of document.querySelectorAll('[data-action]')) {
    const action = Number(button.dataset.action);
    const invalidState = action === REVIVE ? !me || me.hp > 0 : !me || me.hp <= 0;
    button.disabled = !controlsEnabled || invalidState || balances[action] <= 0;
  }
}

function observeShotTraces(traces) {
  const started = performance.now();
  for (const trace of traces) {
    const key = trace.id;
    if (seenShotTraces.has(key)) continue;
    seenShotTraces.add(key);
    activeShotTraces.push({ trace, started });
  }
  while (seenShotTraces.size > 256) seenShotTraces.delete(seenShotTraces.values().next().value);
}

function renderLeaderboard(players) {
  const nextKey = players
    .map((player) => `${player.id}:${player.kills}:${player.hp}:${player.isMe}`)
    .join('|');
  if (nextKey === leaderboardKey) return;
  leaderboardKey = nextKey;
  const board = $('leaderboard');
  board.replaceChildren();
  const sorted = [...players].sort((left, right) => right.kills - left.kills || left.id.localeCompare(right.id));
  for (const player of sorted) {
    const item = document.createElement('li');
    item.textContent = `${player.id.slice(0, 10)}${player.isMe ? ' (you)' : ''} · ${player.kills} kills · ${player.hp} HP`;
    board.append(item);
  }
  if (!sorted.length) {
    const item = document.createElement('li');
    item.textContent = 'No players yet';
    board.append(item);
  }
}

function renderVtxos(vtxos) {
  const nextKey = JSON.stringify(vtxos);
  if (nextKey === vtxosKey) return;
  vtxosKey = nextKey;
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

function invalidateCanvas() {
  canvasDirty = true;
}

function drawProjectedAction(context, state, cell, ox, oy, viewCells, cameraX, cameraY, me) {
  const action = selectedBrowserAction ?? state.projectedAction;
  if (!me || action === null || action === undefined) return;
  const directions = [[0, -1], [1, 0], [0, 1], [-1, 0]];
  const center = (x, y) => [
    ox + (x - cameraX + 0.5) * cell,
    oy + (y - cameraY + 0.5) * cell,
  ];
  const wallKeys = new Set(state.walls.map(([x, y]) => `${x},${y}`));

  context.save();
  context.beginPath();
  context.rect(ox, oy, viewCells * cell, viewCells * cell);
  context.clip();

  if (action <= 3) {
    const [dx, dy] = directions[action];
    const blocked = wallKeys.has(`${me.x + dx},${me.y + dy}`);
    const ghostX = blocked ? me.x : me.x + dx;
    const ghostY = blocked ? me.y : me.y + dy;
    const [px, py] = center(ghostX, ghostY);
    context.strokeStyle = blocked ? '#ff9f43' : '#74d8ff';
    context.fillStyle = blocked ? 'rgba(255, 159, 67, 0.10)' : 'rgba(116, 216, 255, 0.14)';
    context.lineWidth = Math.max(2, cell * 0.07);
    context.setLineDash([cell * 0.16, cell * 0.1]);
    context.fillRect(px - cell * 0.34, py - cell * 0.34, cell * 0.68, cell * 0.68);
    context.strokeRect(px - cell * 0.34, py - cell * 0.34, cell * 0.68, cell * 0.68);
    context.setLineDash([]);
    context.beginPath();
    context.moveTo(px - dx * cell * 0.08, py - dy * cell * 0.08);
    context.lineTo(px + dx * cell * 0.36, py + dy * cell * 0.36);
    context.stroke();
  } else if (action === SHOOT && me.hp > 0) {
    const [dx, dy] = directions[me.facing] || directions[1];
    const cells = [];
    let x = me.x + dx;
    let y = me.y + dy;
    for (let count = 0; count < state.arenaWidth + state.arenaHeight; count += 1) {
      cells.push([x, y]);
      const hitWall = wallKeys.has(`${x},${y}`);
      const hitPlayer = state.players.some((player) => player.id !== me.id
        && player.hp > 0 && player.x === x && player.y === y);
      if (hitWall || hitPlayer) break;
      x += dx;
      y += dy;
    }
    context.fillStyle = 'rgba(255, 209, 102, 0.16)';
    for (const [rayX, rayY] of cells) {
      context.fillRect(
        ox + (rayX - cameraX) * cell + 1,
        oy + (rayY - cameraY) * cell + 1,
        cell - 2,
        cell - 2,
      );
    }
    const [startX, startY] = center(me.x, me.y);
    const [endX, endY] = center(x, y);
    context.strokeStyle = '#ffd166';
    context.lineWidth = Math.max(2, cell * 0.065);
    context.setLineDash([cell * 0.2, cell * 0.1]);
    context.beginPath();
    context.moveTo(startX, startY);
    context.lineTo(endX, endY);
    context.stroke();
    context.setLineDash([]);
    context.strokeStyle = '#fff4b8';
    context.lineWidth = Math.max(2, cell * 0.09);
    context.strokeRect(endX - cell * 0.38, endY - cell * 0.38, cell * 0.76, cell * 0.76);
  } else if (action === REVIVE) {
    const [px, py] = center(me.x, me.y);
    context.strokeStyle = '#65d36e';
    context.lineWidth = Math.max(2, cell * 0.08);
    context.setLineDash([cell * 0.14, cell * 0.08]);
    context.beginPath();
    context.arc(px, py, cell * 0.43, 0, Math.PI * 2);
    context.stroke();
    context.setLineDash([]);
    context.fillStyle = '#b8ffbf';
    context.font = `800 ${Math.max(9, cell * 0.2)}px ui-sans-serif, system-ui, sans-serif`;
    context.textAlign = 'center';
    context.fillText('REVIVE', px, py - cell * 0.55);
  }
  context.restore();
}

function render() {
  if (!canvasDirty && activeShotTraces.length === 0) {
    requestAnimationFrame(render);
    return;
  }
  canvasDirty = false;
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
  if (snapshot?.arenaWidth) drawGame(context, width, height, snapshot);
  requestAnimationFrame(render);
}

function drawGame(context, width, height, state) {
  const viewCells = Math.min(15, state.arenaWidth, state.arenaHeight);
  const cell = Math.min(width, height) / viewCells;
  const me = state.players.find((player) => player.isMe);
  const centerX = me?.x ?? Math.floor(state.arenaWidth / 2);
  const centerY = me?.y ?? Math.floor(state.arenaHeight / 2);
  const maxX = state.arenaWidth - viewCells;
  const maxY = state.arenaHeight - viewCells;
  const cameraX = Math.max(0, Math.min(maxX, centerX - Math.floor(viewCells / 2)));
  const cameraY = Math.max(0, Math.min(maxY, centerY - Math.floor(viewCells / 2)));
  const ox = (width - cell * viewCells) / 2;
  const oy = (height - cell * viewCells) / 2;
  context.fillStyle = '#171a17';
  context.fillRect(ox, oy, cell * viewCells, cell * viewCells);
  context.strokeStyle = '#202820';
  context.lineWidth = 1;
  for (let i = 0; i <= viewCells; i += 1) {
    context.beginPath(); context.moveTo(ox + i * cell, oy); context.lineTo(ox + i * cell, oy + viewCells * cell); context.stroke();
    context.beginPath(); context.moveTo(ox, oy + i * cell); context.lineTo(ox + viewCells * cell, oy + i * cell); context.stroke();
  }
  const visible = ([x, y]) => x >= cameraX && y >= cameraY && x < cameraX + viewCells && y < cameraY + viewCells;
  context.fillStyle = '#454941';
  for (const [x, y] of state.walls.filter(visible)) context.fillRect(ox + (x - cameraX) * cell, oy + (y - cameraY) * cell, cell, cell);
  context.fillStyle = '#8c633f';
  for (const [x, y] of state.houses.filter(visible)) {
    context.fillRect(ox + (x - cameraX) * cell + 1, oy + (y - cameraY) * cell + 1, cell - 2, cell - 2);
  }
  for (const player of state.players) {
    const displayed = player;
    if (!visible([displayed.x, displayed.y])) continue;
    const px = ox + (displayed.x - cameraX + 0.5) * cell;
    const py = oy + (displayed.y - cameraY + 0.5) * cell;
    context.beginPath();
    context.arc(px, py, Math.max(4, cell * 0.25), 0, Math.PI * 2);
    context.fillStyle = playerColor(player.id);
    context.globalAlpha = player.hp > 0 ? 1 : 0.28;
    context.fill();
    context.globalAlpha = 1;
    if (player.isMe) {
      context.strokeStyle = '#fff';
      context.lineWidth = Math.max(1, cell * 0.08);
      context.stroke();
    }
    if (player.hp > 0) {
      const facing = displayed.facing;
      const [dx, dy] = [[0, -1], [1, 0], [0, 1], [-1, 0]][facing] || [1, 0];
      context.strokeStyle = '#fff';
      context.lineWidth = Math.max(2, cell * 0.07);
      context.beginPath(); context.moveTo(px, py); context.lineTo(px + dx * cell * 0.38, py + dy * cell * 0.38); context.stroke();
      const barWidth = cell * 0.62;
      const barGap = Math.max(1, cell * 0.035);
      const pipWidth = (barWidth - barGap * (state.maxHp - 1)) / state.maxHp;
      const barX = px - barWidth / 2;
      const barY = py - cell * 0.44;
      const barHeight = Math.max(3, cell * 0.08);
      for (let hp = 0; hp < state.maxHp; hp += 1) {
        context.fillStyle = hp < player.hp ? (player.hp > 1 ? '#65d36e' : '#e65d4f') : '#555';
        context.fillRect(barX + hp * (pipWidth + barGap), barY, pipWidth, barHeight);
      }
    }
  }
  drawProjectedAction(context, state, cell, ox, oy, viewCells, cameraX, cameraY, me);

  const balances = state.moveBalances || [0, 0, 0, 0, 0, 0];
  const warnings = [];
  if (state.playerId && balances.slice(0, 4).every((amount) => amount === 0)) warnings.push('OUT OF MOVES');
  if (state.playerId && balances[4] === 0) warnings.push('OUT OF BULLETS');
  context.save();
  context.font = `800 ${Math.max(10, cell * 0.24)}px ui-sans-serif, system-ui, sans-serif`;
  context.textAlign = 'right';
  context.textBaseline = 'top';
  warnings.forEach((warning, index) => {
    const width = context.measureText(warning).width + cell * 0.35;
    const x = ox + viewCells * cell - cell * 0.18;
    const y = oy + cell * (0.18 + index * 0.52);
    context.fillStyle = '#9f2929';
    context.fillRect(x - width, y, width, cell * 0.4);
    context.fillStyle = '#fff';
    context.fillText(warning, x - cell * 0.16, y + cell * 0.08);
  });
  context.restore();

  const now = performance.now();
  activeShotTraces = activeShotTraces.filter(({ started }) => now - started < TRACE_MS);
  for (const { trace, started } of activeShotTraces) {
    const progress = Math.min(1, (now - started) / TRACE_MS);
    const startX = ox + (trace.start[0] - cameraX + 0.5) * cell;
    const startY = oy + (trace.start[1] - cameraY + 0.5) * cell;
    const endX = ox + (trace.end[0] - cameraX + 0.5) * cell;
    const endY = oy + (trace.end[1] - cameraY + 0.5) * cell;
    const bulletX = startX + (endX - startX) * progress;
    const bulletY = startY + (endY - startY) * progress;
    const tailProgress = Math.max(0, progress - 0.16);
    context.strokeStyle = '#ffd166';
    context.lineWidth = Math.max(2, cell * 0.09);
    context.beginPath();
    context.moveTo(startX + (endX - startX) * tailProgress, startY + (endY - startY) * tailProgress);
    context.lineTo(bulletX, bulletY);
    context.stroke();
    context.fillStyle = '#fff4b8';
    context.beginPath(); context.arc(bulletX, bulletY, Math.max(2, cell * 0.08), 0, Math.PI * 2); context.fill();
  }
}

async function boot() {
  bindStaticActions();
  requestAnimationFrame(render);
  await withTimeout(init({ module_or_path: new URL(wasmUrl, import.meta.url) }), 20_000, 'WASM initialization');
  await connectApp();
}

boot().catch((error) => {
  showError(`Could not start Arkade City: ${String(error)}`);
});
