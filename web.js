import init, { App, keyMask } from './pkg/arkade_duel.js?v=0.2.6';

const $ = (id) => document.getElementById(id);
const params = new URLSearchParams(location.search);
const serverUrl = params.get('server') || undefined;

// The share link carries the host address in the fragment: /#tark1...
const fragmentAddr = location.hash.startsWith('#') ? location.hash.slice(1) : '';

// ---------------------------------------------------------------------------
// JS owns the event loop. The WASM App may only be touched via `step`, and
// each step must be awaited before the next: overlapping calls crash.
// Inputs and commands are plain JS variables handed over as arguments.
// ---------------------------------------------------------------------------

const keysDown = { w: false, a: false, s: false, d: false };
let fireCount = 0;
let command = '';        // '', 'host', 'join'
let commandArg = '';
let app = null;
let lastSnapshot = null;

const $id = (id) => document.getElementById(id);
function show(id) { $id(id).classList.remove('hidden'); }
function hide(id) { $id(id).classList.add('hidden'); }
function setText(id, text) { $id(id).textContent = text; }

async function boot() {
  await init();
  app = await App.init(serverUrl);
  applyNetwork(app.network());

  $('net-btn').onclick = () => {
    // Keep the fragment: on a same-network invite link this preserves the
    // lobby; mismatched links get caught by the ark1/tark1 guard anyway.
    location.href = location.pathname + (onMainnet ? '?server=https://mutinynet.arkade.sh' : '') + location.hash;
  };

  setText('address', app.address());
  setText('recovery-key', app.exportKey());
  $('copy-addr').onclick = () => navigator.clipboard.writeText(app.address());

  if (fragmentAddr.startsWith('tark1') || fragmentAddr.startsWith('ark1')) {
    show('join-row');
    setText('join-addr', fragmentAddr);
  }

  $('host-btn').onclick = () => {
    command = 'host';
    // Carry the network in the link so the joiner lands in the right place.
    const q = onMainnet ? '' : `?server=${serverUrl}`;
    const link = `${location.origin}${location.pathname}${q}#${app.address()}`;
    setText('host-link', link);
    show('host-link-row');
    $('copy-link').onclick = () => navigator.clipboard.writeText(link);
  };

  $('join-btn').onclick = () => {
    command = 'join';
    commandArg = fragmentAddr;
    $('join-btn').disabled = true;
    $('join-btn').textContent = 'SENDING START…';
    addLogLine('building and sending START tx…');
  };

  $('reset-btn').onclick = () => {
    command = 'reset';
    setTimeout(() => location.reload(), 2500);
  };

  bindKeys();
  requestAnimationFrame(render);
  driver(); // serialized poll loop (never overlaps itself)
}

async function driver() {
  // eslint-disable-next-line no-constant-condition
  while (true) {
    const mask = keyMask(keysDown.w, keysDown.a, keysDown.s, keysDown.d);
    const fires = fireCount;
    fireCount = 0;
    const cmd = command;
    const arg = commandArg;
    command = '';
    commandArg = '';
    try {
      lastSnapshot = await app.step(cmd, arg, mask, fires);
      applySnapshot(lastSnapshot);
    } catch (e) {
      console.warn('step failed', e);
      addLogLine(`step error: ${e}`);
      if ($('join-btn').disabled && command === '') {
        // a failed join leaves the command consumed; let the user retry
        $('join-btn').disabled = false;
        $('join-btn').textContent = 'ACCEPT & SEND START TX';
      }
    }
    await sleep(1400);
  }
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function addLogLine(text) {
  const ul = $('log');
  const li = document.createElement('li');
  li.textContent = text;
  ul.prepend(li);
}

function applySnapshot(s) {
  if (!s) return;
  setText('phase', (s.phase ?? 'unknown') + (s.sending ? ' (sending…)' : ''));
  setText('version', s.version || 'unknown');
  applyNetwork(s.network);
  setText('match-id', s.matchId || '—');
  setText('balance', s.balance === undefined ? '…' : `${s.balance} sats`);
  const ago = s.lastSyncMs ? Math.max(0, Math.round((Date.now() - s.lastSyncMs) / 1000)) : null;
  setText('sync', ago === null ? 'never' : `${ago}s ago · ${s.events ?? 0} chain events`);

  const ul = $('log');
  ul.innerHTML = '';
  for (const line of (s.log || []).slice(-12).reverse()) {
    const li = document.createElement('li');
    li.textContent = line;
    ul.appendChild(li);
  }

  if (s.phase === 'playing' || s.phase === 'done' || s.phase === 'arming') {
    show('arena');
  }
  if (s.phase === 'done') {
    const mine = s.winner === 0; // side 0 == us
    setText('result', `${mine ? 'YOU WIN' : 'YOU LOSE'}${s.verified ? ' — verified on-chain' : ''}`);
    $('result').className = mine ? 'win' : 'lose';
    show('result');
  } else {
    hide('result');
  }
}

let onMainnet = true;

function applyNetwork(network) {
  const mainnet = network === 'mainnet';
  onMainnet = mainnet;
  const badge = $('net-badge');
  badge.textContent = mainnet ? 'MAINNET' : 'SIGNET (mutinynet)';
  badge.className = mainnet ? 'badge mainnet' : 'badge signet';
  $('net-btn').textContent = mainnet ? 'switch to signet' : 'switch to mainnet';
  // A join link is only valid on the network it was created on: the address
  // HRP (ark1 vs tark1) encodes it, and keys/addresses differ per network.
  if (fragmentAddr) {
    const linkMainnet = fragmentAddr.startsWith('ark1');
    if (linkMainnet !== mainnet) {
      setText('net-warn', `this invite link is for ${linkMainnet ? 'mainnet' : 'signet'} — switch networks or ask for a fresh link`);
      show('net-warn');
      $('join-btn').disabled = true;
    }
  }
}

function bindKeys() {
  addEventListener('keydown', (e) => {
    const k = e.key.toLowerCase();
    if (k in keysDown) { keysDown[k] = true; e.preventDefault(); }
    if (k === ' ') { fireCount += 1; e.preventDefault(); }
  });
  addEventListener('keyup', (e) => {
    const k = e.key.toLowerCase();
    if (k in keysDown) { keysDown[k] = false; e.preventDefault(); }
  });
}

function render() {
  const canvas = $('canvas');
  const ctx = canvas.getContext('2d');
  ctx.fillStyle = '#0b0d10';
  ctx.fillRect(0, 0, canvas.width, canvas.height);

  if (lastSnapshot && lastSnapshot.players) {
    const [me, opp] = lastSnapshot.players;
    drawStick(ctx, opp[0], opp[1], '#e25563');   // opponent
    drawStick(ctx, me[0], me[1], '#6fd3ff');     // us
    ctx.fillStyle = '#ffd166';
    for (const [x, y] of lastSnapshot.bullets || []) {
      ctx.fillRect(x - 2, y - 2, 4, 4);
    }
    if (lastSnapshot.ammo) {
      ctx.fillStyle = '#8b949e';
      ctx.font = '12px monospace';
      ctx.fillText(`ammo ${lastSnapshot.ammo[0]} vs ${lastSnapshot.ammo[1]}`, 10, 16);
    }
  }
  requestAnimationFrame(render);
}

function drawStick(ctx, x, y, color) {
  ctx.strokeStyle = color;
  ctx.lineWidth = 2;
  ctx.beginPath();
  ctx.arc(x, y - 12, 5, 0, Math.PI * 2);           // head
  ctx.moveTo(x, y - 7); ctx.lineTo(x, y + 8);      // body
  ctx.moveTo(x, y + 8); ctx.lineTo(x - 6, y + 20); // legs
  ctx.moveTo(x, y + 8); ctx.lineTo(x + 6, y + 20);
  ctx.moveTo(x, y - 2); ctx.lineTo(x - 7, y + 4);  // arms
  ctx.moveTo(x, y - 2); ctx.lineTo(x + 7, y + 4);
  ctx.stroke();
}

boot().catch((e) => {
  document.body.innerHTML = `<pre class="warn">boot failed: ${e}</pre>`;
});
