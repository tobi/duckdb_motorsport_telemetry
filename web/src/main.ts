import * as duckdb from '@duckdb/duckdb-wasm';
import duckdbMvp from '@duckdb/duckdb-wasm/dist/duckdb-mvp.wasm?url';
import duckdbEh from '@duckdb/duckdb-wasm/dist/duckdb-eh.wasm?url';
import mvpWorker from '@duckdb/duckdb-wasm/dist/duckdb-browser-mvp.worker.js?url';
import ehWorker from '@duckdb/duckdb-wasm/dist/duckdb-browser-eh.worker.js?url';
import './style.css';

type Row = Record<string, unknown>;
type QueryResult = { columns: string[]; rows: Row[]; elapsed: number; total: number };
type Lap = { number: number; label: string; startNs: number; endNs: number; durationNs: number; complete: boolean };
type ExampleRun = { name: string; eyebrow: string; detail: string; size: string; license: string; url: string; source: string; sha256: string };
type RoleKey = 'speed' | 'throttle' | 'brake' | 'gLat' | 'gLong' | 'distance';
type SignalRoles = Record<RoleKey, string>;

const BASE = '/duckdb_motorsport_telemetry/';
const EXT_REPO = `${location.origin}${BASE.replace(/\/$/, '')}`;
const bundles: duckdb.DuckDBBundles = {
  mvp: { mainModule: duckdbMvp, mainWorker: mvpWorker },
  eh: { mainModule: duckdbEh, mainWorker: ehWorker },
};
const EXAMPLES: ExampleRun[] = [
  {
    name: 'Lamborghini GT3 · Barcelona', eyebrow: 'IRACING / MOTEC LD / GPS', detail: '330 channels · 3 lap IDs · 107 seconds', size: '7.5 MB', license: 'Apache-2.0',
    url: 'https://raw.githubusercontent.com/JBonifay/motec-file-parser/34edb90bfc0374f500817cdb7151a99f3e9a98b5/sample.ld',
    source: 'https://github.com/JBonifay/motec-file-parser/blob/34edb90bfc0374f500817cdb7151a99f3e9a98b5/sample.ld',
    sha256: 'f43a70743eeadf9c20ff16169942d654c920d226d5584af1b2b33c8d7e60a291',
  },
];

let db: duckdb.AsyncDuckDB;
let conn: duckdb.AsyncDuckDBConnection;
let activeFile = '';
let activeDisplayName = '';
let metadata: Row[] = [];
let plotRows: Row[] = [];
let plotChannels: string[] = [];
let laps: Lap[] = [];
let activeLap: Lap | null = null;
let inspectedChannel = '';
let mapRows: Row[] = [];
let mapChannels: [string, string] | null = null;
let signalRoles: SignalRoles = { speed: '', throttle: '', brake: '', gLat: '', gLong: '', distance: '' };
let compareLap: Lap | null = null;
let compareRows: Row[] = [];
let scrubTimeNs: number | null = null;
let mapScreenPoints: { x: number; y: number; time: number }[] = [];
let vboLapCrossings: number[] = [];

const app = document.querySelector<HTMLDivElement>('#app')!;
app.innerHTML = `
  <header class="topbar">
    <a class="brand" href="${BASE}"><span class="mark">T<span>L</span></span><span>TELEMETRY <b>LAB</b></span></a>
    <div class="runtime"><i id="statusDot"></i><span id="runtimeText">Starting DuckDB-Wasm</span></div>
    <a class="github" href="https://github.com/tobi/duckdb_motorsport_telemetry" target="_blank" rel="noreferrer">GITHUB / SOURCE ↗</a>
  </header>
  <main>
    <section class="install-banner" aria-label="DuckDB installation">
      <div class="install-intro"><span>INSTALL / DUCKDB 1.4.3</span><strong>Use the extension in your own SQL</strong><small>Start DuckDB with <code>duckdb -unsigned</code>, then run:</small></div>
      <code class="install-command">INSTALL httpfs; LOAD httpfs;<br>INSTALL motorsport_telemetry FROM 'https://pages.tobi.lutke.com/duckdb_motorsport_telemetry';<br>LOAD motorsport_telemetry;</code>
      <button id="copyInstall" type="button">COPY SQL</button>
    </section>
    <section class="hero" id="dropZone">
      <div class="hero-copy">
        <div class="eyebrow">LOCAL-FIRST · ZERO UPLOAD</div>
        <h1>Drop telemetry.<br><em>Interrogate the lap.</em></h1>
        <p>Cosworth PDS, MoTeC LD, and VBOX VBO—parsed by Rust and queried by DuckDB, entirely inside your browser.</p>
        <div class="formats"><span>.PDS</span><span>.LD</span><span>.VBO</span></div>
      </div>
      <div class="drop-stack">
        <label class="drop-card" for="fileInput">
          <input id="fileInput" type="file" accept=".pds,.ld,.vbo" />
          <div class="drop-rings"><div class="upload-icon">↥</div></div>
          <strong>DROP A RUN HERE</strong>
          <small>or click to choose a file</small>
          <div class="privacy">◉ Your data never leaves this machine</div>
        </label>
        <div class="demo-launch" aria-label="Public example telemetry">
          <div class="demo-heading"><span>AUTO-LOADING REAL DEMO</span><small>Lamborghini GT3 · replace it with your file anytime</small></div>
          ${EXAMPLES.map((example, index) => `<div class="demo-run"><button data-example="${index}"><span>LOAD LAMBORGHINI GT3 · BARCELONA + GPS</span><b>RUN DEMO · ${example.size} →</b></button><a href="${example.source}" target="_blank" rel="noreferrer" title="${example.name} · ${example.license}">SOURCE · ${example.license}</a></div>`).join('')}
        </div>
      </div>
    </section>

    <section class="workspace hidden" id="workspace">
      <div class="run-strip">
        <div><span class="label">ACTIVE RUN</span><strong id="fileName">—</strong></div>
        <div><span class="label">FORMAT</span><strong id="fileFormat">—</strong></div>
        <div><span class="label">FILE SIZE</span><strong id="fileSize">—</strong></div>
        <button id="replaceFile">REPLACE FILE</button>
      </div>

      <div class="section-head"><div><span>01</span><h2>Run intelligence</h2></div><p>Automatic native-rate inspection</p></div>
      <div class="metric-grid" id="metrics"></div>
      <div class="performance-grid" id="performanceMetrics"></div>
      <div class="analysis-grid">
        <article class="panel trace-panel">
          <div class="panel-title"><div><span class="pulse"></span> QUICK TRACE <b id="traceLapLabel"></b></div><div id="traceLegend" class="legend"></div></div>
          <div class="lap-rail" id="lapRail"></div>
          <div class="comparison-bar"><span>DISTANCE OVERLAY</span><label>COMPARE TO <select id="compareLap"></select></label><small id="comparisonStatus">SELECTED LAP SOLID · REFERENCE DASHED</small></div>
          <div class="trace-stage"><canvas id="trace" height="260"></canvas><div class="scrubber hidden" id="scrubber"><i></i><div id="scrubValues"></div></div></div>
          <div class="axis-label">SCRUB FOR INTERPOLATED VALUES · LAP TIME →</div>
        </article>
        <article class="panel findings-panel">
          <div class="panel-title">SIGNAL FINDINGS <span>EXACT SAMPLES</span></div>
          <div id="findings"></div>
        </article>
      </div>
      <article class="panel track-map-panel hidden" id="trackMapPanel">
        <div class="panel-title"><div><span class="pulse"></span> GPS TRACK MAP <b id="mapLapLabel"></b></div><div id="mapCoords">SOURCE COORDINATES</div></div>
        <div class="map-stage"><canvas id="trackMap" height="430"></canvas><div class="map-tooltip hidden" id="mapTooltip"></div></div>
        <div class="axis-label">HOVER TO SCRUB TRACE · SELECTED LAP HIGHLIGHTED</div>
      </article>

      <div class="section-head"><div><span>02</span><h2>Channel map</h2></div><p>Source-exact names, units, and clocks</p></div>
      <div class="panel channel-panel">
        <div class="role-editor"><div><span>SIGNAL ROLES</span><small>Override automatic channel detection · saved for this channel layout</small></div><div class="role-selects" id="roleSelects"></div><button id="resetRoles">RESET AUTO</button></div>
        <div class="channel-tools"><input id="channelSearch" placeholder="Filter channels…" /><small>CLICK A CHANNEL TO INSPECT THE SELECTED LAP</small><span id="channelCount"></span></div>
        <div class="table-wrap"><table><thead><tr><th>CHANNEL</th><th>UNIT</th><th>TYPE</th><th>NATIVE RATE</th><th>SAMPLES</th><th>DURATION</th></tr></thead><tbody id="channelRows"></tbody></table></div>
      </div>

      <div class="section-head"><div><span>03</span><h2>SQL workbench</h2></div><p>The full DuckDB surface, in-browser</p></div>
      <div class="sql-grid">
        <article class="panel editor-panel">
          <div class="editor-top"><span>QUERY.SQL</span><div class="query-presets"><button data-query="metadata">CHANNELS</button><button data-query="stats">STATS</button><button data-query="samples">SAMPLES</button></div></div>
          <div class="editor-body"><div class="line-numbers" id="lineNumbers">1</div><textarea id="sqlEditor" spellcheck="false"></textarea></div>
          <div class="editor-footer"><span>⌘ / CTRL + ENTER TO RUN</span><button id="runQuery">RUN QUERY <b>▶</b></button></div>
        </article>
        <article class="panel result-panel">
          <div class="panel-title">RESULT <span id="queryTiming">READY</span></div>
          <div class="result-wrap" id="queryResult"><div class="empty-result">Run a query to inspect this recording.</div></div>
        </article>
      </div>
      <div class="recipe-shelf"><div class="recipe-heading"><span>QUERY RECIPES</span><small>LOAD INTO THE EDITOR</small></div><div class="recipe-grid" id="recipeGrid"></div></div>
    </section>
  </main>
  <aside class="channel-inspector hidden" id="channelInspector">
    <div class="inspector-head"><div><span>CHANNEL INSPECTOR · <b id="inspectorLap"></b></span><strong id="inspectorName">—</strong></div><button id="closeInspector" aria-label="Close channel inspector">×</button></div>
    <div class="inspector-summary" id="inspectorSummary"></div>
    <div class="inspector-shape"><div><span>VALUE SHAPE</span><canvas id="channelShape" height="120"></canvas></div><div><span>SAMPLE VALUES</span><div class="sample-list" id="sampleList"></div></div></div>
  </aside>
  <div class="inspector-scrim hidden" id="inspectorScrim"></div>
  <footer><a href="https://github.com/tobi/duckdb_motorsport_telemetry" target="_blank" rel="noreferrer">GITHUB / TOBI / DUCKDB MOTORSPORT TELEMETRY ↗</a><span>DUCKDB-WASM × RUST · NO SERVER · NO TRACKING · NO UPLOAD</span></footer>
  <div class="toast hidden" id="toast"></div>
`;

const $ = <T extends HTMLElement>(selector: string) => document.querySelector<T>(selector)!;
const statusDot = $('#statusDot');
const runtimeText = $('#runtimeText');
const editor = $<HTMLTextAreaElement>('#sqlEditor');
const input = $<HTMLInputElement>('#fileInput');

function sqlLiteral(value: string): string { return `'${value.replaceAll("'", "''")}'`; }
function quoteIdent(value: string): string { return `"${value.replaceAll('"', '""')}"`; }
function formatNumber(value: unknown, digits = 2): string {
  const n = Number(value);
  if (!Number.isFinite(n)) return '—';
  return new Intl.NumberFormat('en-US', { maximumFractionDigits: digits }).format(n);
}
function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB']; let value = bytes / 1024; let unit = units[0];
  for (let i = 1; i < units.length && value >= 1024; i++) { value /= 1024; unit = units[i]; }
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${unit}`;
}
function formatDuration(ns: unknown): string {
  const seconds = Number(ns) / 1e9;
  if (!Number.isFinite(seconds)) return '—';
  const mins = Math.floor(seconds / 60); const rest = seconds - mins * 60;
  return mins ? `${mins}:${rest.toFixed(1).padStart(4, '0')}` : `${rest.toFixed(2)} s`;
}
function arrowRows(table: Awaited<ReturnType<duckdb.AsyncDuckDBConnection['query']>>): Row[] {
  return table.toArray().map((row) => {
    const result: Row = {};
    for (const field of table.schema.fields) result[field.name] = row[field.name];
    return result;
  });
}
function toast(message: string, error = false) {
  const el = $('#toast'); el.textContent = message; el.classList.toggle('error', error); el.classList.remove('hidden');
  window.setTimeout(() => el.classList.add('hidden'), 4200);
}
function setRuntime(text: string, state: 'loading' | 'ready' | 'error' = 'loading') {
  runtimeText.textContent = text; statusDot.className = state;
}

async function init() {
  try {
    const bundle = await duckdb.selectBundle(bundles);
    if (!bundle.mainWorker) throw new Error('No compatible DuckDB-Wasm worker');
    const worker = new Worker(bundle.mainWorker);
    db = new duckdb.AsyncDuckDB(new duckdb.ConsoleLogger(duckdb.LogLevel.WARNING), worker);
    await db.instantiate(bundle.mainModule, bundle.pthreadWorker);
    await db.open({ allowUnsignedExtensions: true, query: { castBigIntToDouble: true } });
    conn = await db.connect();
    setRuntime('Loading telemetry extension', 'loading');
    await conn.query(`INSTALL motorsport_telemetry FROM ${sqlLiteral(EXT_REPO)}`);
    await conn.query('LOAD motorsport_telemetry');
    const version = arrowRows(await conn.query('SELECT version() AS version'))[0]?.version;
    setRuntime(`DuckDB ${version} · Extension ready`, 'ready');
    if (!new URLSearchParams(location.search).has('no-demo')) {
      const demoButton = document.querySelector<HTMLButtonElement>('[data-example="0"]');
      if (demoButton) await loadExample(0, demoButton);
    }
  } catch (error) {
    console.error(error); setRuntime('Runtime failed to start', 'error');
    toast(error instanceof Error ? error.message : String(error), true);
  }
}

function normalizedName(value: unknown) { return String(value).toLowerCase().replace(/[^a-z0-9]/g, ''); }

function detectSignalRoles(rows: Row[]): SignalRoles {
  const sampled = rows.filter((row) => Number(row.sample_count) > 0);
  const pick = (exact: string[], fallback: RegExp, exclude?: RegExp) => exact.map((wanted) => sampled.find((row) => normalizedName(row.name) === wanted)).find(Boolean)
    || sampled.find((row) => fallback.test(String(row.name)) && !exclude?.test(String(row.name)));
  const name = (row: Row | undefined) => String(row?.name || '');
  return {
    speed: name(pick(['groundspeed', 'speedref', 'corrspeed', 'vehiclespeed', 'gpsspeed', 'speed', 'velocitykmh'], /speed|velocity/i, /engine|wheel|target|limit/i)),
    throttle: name(pick(['throttlepos', 'driverthrottlepos', 'throttlepedal', 'throttle'], /throttle/i)),
    brake: name(pick(['brakepedalpos', 'driverbrakepressure', 'brakepressure', 'brake'], /brake|p_f_brake/i)),
    gLat: name(pick(['gforcelat', 'lateralacceleration', 'glat'], /accel.*lat|lat.*accel|glat|g force lat/i)),
    gLong: name(pick(['gforcelong', 'longitudinalacceleration', 'glong'], /accel.*long|long.*accel|glong|g force long/i)),
    distance: name(pick(['lapdistancecorrected', 'lapdistance', 'lapdist', 'lapdistpct', 'distance'], /lap.*dist|distance/i)),
  };
}

function roleStorageKey() {
  const signature = metadata.map((row) => normalizedName(row.name)).sort().join('|');
  let hash = 5381;
  for (const char of signature) hash = ((hash << 5) + hash) ^ char.charCodeAt(0);
  return `telemetry-roles:${String(metadata[0]?.format || 'unknown')}:${hash >>> 0}`;
}

function initializeSignalRoles() {
  signalRoles = detectSignalRoles(metadata);
  try {
    const saved = JSON.parse(localStorage.getItem(roleStorageKey()) || '{}') as Partial<SignalRoles>;
    const available = new Set(metadata.map((row) => String(row.name)));
    for (const key of Object.keys(signalRoles) as RoleKey[]) if (saved[key] && available.has(saved[key]!)) signalRoles[key] = saved[key]!;
  } catch { /* Ignore malformed local preferences. */ }
  renderRoleEditor();
}

function candidateChannels(_rows: Row[]): string[] {
  return [signalRoles.speed, signalRoles.throttle, signalRoles.brake, signalRoles.gLat, signalRoles.gLong]
    .filter((name, index, names) => Boolean(name) && names.indexOf(name) === index).slice(0, 3);
}

function renderRoleEditor() {
  const labels: Record<RoleKey, string> = { speed: 'SPEED', throttle: 'THROTTLE', brake: 'BRAKE', gLat: 'LAT G', gLong: 'LONG G', distance: 'LAP DISTANCE' };
  const channels = metadata.filter((row) => Number(row.sample_count) > 0).map((row) => String(row.name));
  $('#roleSelects').innerHTML = (Object.keys(labels) as RoleKey[]).map((key) => `<label><span>${labels[key]}</span><select data-role="${key}"><option value="">NOT SET</option>${channels.map((name) => `<option value="${escapeHtml(name)}" ${signalRoles[key] === name ? 'selected' : ''}>${escapeHtml(name)}</option>`).join('')}</select></label>`).join('');
  document.querySelectorAll<HTMLSelectElement>('[data-role]').forEach((select) => select.onchange = async () => {
    signalRoles[select.dataset.role as RoleKey] = select.value;
    localStorage.setItem(roleStorageKey(), JSON.stringify(signalRoles));
    await refreshSignalAnalysis();
  });
}

async function refreshSignalAnalysis() {
  await renderInsights();
  setPresets(true);
  renderRoleEditor();
}

async function sha256(bytes: ArrayBuffer) {
  const digest = await crypto.subtle.digest('SHA-256', bytes);
  return [...new Uint8Array(digest)].map((value) => value.toString(16).padStart(2, '0')).join('');
}

async function fetchExampleBytes(example: ExampleRun, progress: HTMLElement | null): Promise<ArrayBuffer> {
  const cache = 'caches' in window ? await caches.open('telemetry-demo-v1') : null;
  const cached = await cache?.match(example.url);
  if (cached) {
    const bytes = await cached.arrayBuffer();
    if (await sha256(bytes) === example.sha256) {
      if (progress) progress.textContent = `CACHED · ${example.size}`;
      return bytes;
    }
    await cache?.delete(example.url);
  }
  const response = await fetch(example.url);
  if (!response.ok) throw new Error(`Example download failed: HTTP ${response.status}`);
  const total = Number(response.headers.get('content-length')) || 0;
  const reader = response.body?.getReader();
  const chunks: Uint8Array[] = []; let received = 0;
  if (reader) {
    for (;;) {
      const { done, value } = await reader.read(); if (done) break;
      chunks.push(value); received += value.length;
      const percent = total ? ` · ${Math.round(received / total * 100)}%` : '';
      if (progress) progress.textContent = `FETCHING ${formatBytes(received)}${percent}`;
      setRuntime(`Downloading ${example.name}${percent}`, 'loading');
    }
  }
  const bytes = reader ? new Uint8Array(received) : new Uint8Array(await response.arrayBuffer());
  let offset = 0; for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
  const buffer = bytes.buffer;
  if (await sha256(buffer) !== example.sha256) throw new Error('Demo checksum did not match its pinned source');
  await cache?.put(example.url, new Response(buffer.slice(0), { headers: { 'content-type': 'application/octet-stream' } }));
  return buffer;
}

async function loadExample(index: number, button: HTMLButtonElement) {
  const example = EXAMPLES[index];
  if (!example || !conn) return;
  button.disabled = true;
  const original = button.querySelector('b')?.textContent || '';
  const progress = button.querySelector<HTMLElement>('b');
  if (progress) progress.textContent = `FETCHING ${example.size}…`;
  setRuntime(`Downloading ${example.name}`, 'loading');
  try {
    const bytes = await fetchExampleBytes(example, progress);
    await loadFile(new File([bytes], `${example.name.replaceAll(/[^a-z0-9]+/gi, '_')}.ld`));
  } catch (error) {
    setRuntime('Could not load public example', 'error');
    toast(error instanceof Error ? error.message : String(error), true);
  } finally {
    button.disabled = false;
    if (progress) progress.textContent = original;
  }
}

function gpsChannelPair(): [string, string] | null {
  const sampled = metadata.filter((row) => Number(row.sample_count) > 0);
  const normalized = (value: unknown) => String(value).toLowerCase().replace(/[^a-z0-9]/g, '');
  const find = (names: string[]) => sampled.find((row) => names.includes(normalized(row.name)));
  const latitude = find(['lat', 'latitude', 'gpslat', 'gpslatitude']);
  const longitude = find(['lon', 'long', 'longitude', 'gpslon', 'gpslongitude']);
  return latitude && longitude ? [String(latitude.name), String(longitude.name)] : null;
}

async function loadTrackMap() {
  mapChannels = gpsChannelPair();
  mapRows = [];
  if (mapChannels) {
    mapRows = arrowRows(await conn.query(`SELECT * FROM read_telemetry(${sqlLiteral(activeFile)}, rate := 20, channels := ${sqlLiteral(mapChannels.join(','))})`));
    mapRows = mapRows.filter((row) => {
      const lat = Number(row[mapChannels![0]]); const lon = Number(row[mapChannels![1]]);
      return Number.isFinite(lat) && Number.isFinite(lon) && Math.abs(lat) <= 90 && Math.abs(lon) <= 180 && (lat !== 0 || lon !== 0);
    });
  }
  $('#trackMapPanel').classList.toggle('hidden', mapRows.length < 2);
  drawTrackMap();
}

function drawTrackMap() {
  if (!mapChannels || mapRows.length < 2 || !activeLap) return;
  const canvas = $<HTMLCanvasElement>('#trackMap'); const rect = canvas.getBoundingClientRect(); const dpr = devicePixelRatio || 1; const height = rect.height;
  canvas.width = Math.max(1, rect.width * dpr); canvas.height = Math.max(1, height * dpr);
  const ctx = canvas.getContext('2d')!; ctx.scale(dpr, dpr); ctx.clearRect(0, 0, rect.width, height);
  const latitudes = mapRows.map((row) => Number(row[mapChannels![0]]));
  const longitudes = mapRows.map((row) => Number(row[mapChannels![1]]));
  const meanLat = latitudes.reduce((sum, value) => sum + value, 0) / latitudes.length;
  const cosLat = Math.cos(meanLat * Math.PI / 180);
  const points = mapRows.map((row, index) => ({ x: longitudes[index] * cosLat, y: latitudes[index], time: Number(row.time_ns) }));
  const xs = points.map((point) => point.x); const ys = points.map((point) => point.y);
  const minX = Math.min(...xs); const maxX = Math.max(...xs); const minY = Math.min(...ys); const maxY = Math.max(...ys);
  const pad = 35; const scale = Math.min((rect.width - pad * 2) / (maxX - minX || 1), (height - pad * 2) / (maxY - minY || 1));
  const project = (point: { x: number; y: number }) => ({ x: (rect.width - (maxX - minX) * scale) / 2 + (point.x - minX) * scale, y: (height + (maxY - minY) * scale) / 2 - (point.y - minY) * scale });
  mapScreenPoints = points.map((point) => ({ ...project(point), time: point.time }));
  const stroke = (subset: typeof points, color: string, width: number) => {
    if (subset.length < 2) return; ctx.beginPath();
    subset.forEach((point, index) => { const p = project(point); if (index) ctx.lineTo(p.x, p.y); else ctx.moveTo(p.x, p.y); });
    ctx.strokeStyle = color; ctx.lineWidth = width; ctx.lineJoin = 'round'; ctx.lineCap = 'round'; ctx.stroke();
  };
  stroke(points, '#303734', 2);
  const selected = points.filter((point) => point.time >= activeLap!.startNs && point.time < activeLap!.endNs);
  stroke(selected, '#d8ff32', 3);
  if (selected.length) {
    const start = project(selected[0]); ctx.fillStyle = '#ff5c35'; ctx.beginPath(); ctx.arc(start.x, start.y, 5, 0, Math.PI * 2); ctx.fill();
  }
  if (scrubTimeNs !== null && mapScreenPoints.length) {
    const marker = mapScreenPoints.reduce((best, point) => Math.abs(point.time - scrubTimeNs!) < Math.abs(best.time - scrubTimeNs!) ? point : best);
    ctx.fillStyle = '#49b9ff'; ctx.strokeStyle = '#e9edeb'; ctx.lineWidth = 2; ctx.beginPath(); ctx.arc(marker.x, marker.y, 6, 0, Math.PI * 2); ctx.fill(); ctx.stroke();
  }
  $('#mapLapLabel').textContent = activeLap.label;
  $('#mapCoords').textContent = `${meanLat.toFixed(4)}°, ${(longitudes.reduce((sum, value) => sum + value, 0) / longitudes.length).toFixed(4)}°`;
}

function scrubMap(clientX: number, clientY: number) {
  if (!activeLap || !mapScreenPoints.length || !plotRows.length) return;
  const rect = $<HTMLCanvasElement>('#trackMap').getBoundingClientRect(); const x = clientX - rect.left; const y = clientY - rect.top;
  const inLap = mapScreenPoints.filter((point) => point.time >= activeLap!.startNs && point.time < activeLap!.endNs);
  if (!inLap.length) return;
  const nearest = inLap.reduce((best, point) => Math.hypot(point.x - x, point.y - y) < Math.hypot(best.x - x, best.y - y) ? point : best);
  let plotIndex = 0; let timeError = Infinity;
  plotRows.forEach((row, index) => { const error = Math.abs(Number(row.time_ns) - nearest.time); if (error < timeError) { plotIndex = index; timeError = error; } });
  const progress = rowProgress(plotRows, plotIndex); showScrubAtProgress(progress);
  const row = plotRows[plotIndex]; const elapsed = (Number(row.time_ns) - activeLap.startNs) / 1e9;
  const speed = signalRoles.speed ? speedToKmh(Number(row[signalRoles.speed]), channelUnit(signalRoles.speed)) : NaN;
  const tooltip = $('#mapTooltip'); tooltip.style.left = `${nearest.x}px`; tooltip.style.top = `${nearest.y}px`;
  tooltip.innerHTML = `<strong>${activeLap.label} · +${elapsed.toFixed(3)} s</strong>${Number.isFinite(speed) ? `<span>SPEED <b>${formatNumber(speed, 1)} km/h</b></span>` : ''}${signalRoles.throttle ? `<span>THROTTLE <b>${formatNumber(row[signalRoles.throttle], 1)}</b></span>` : ''}${signalRoles.brake ? `<span>BRAKE <b>${formatNumber(row[signalRoles.brake], 1)}</b></span>` : ''}`;
  tooltip.classList.remove('hidden');
}

function detectVboLapCrossings(bytes: Uint8Array): number[] {
  const text = new TextDecoder().decode(bytes); const lines = text.split(/\r?\n/); let section = '';
  let columns: string[] = []; const data: number[][] = []; let gate: number[] | null = null;
  for (const raw of lines) {
    const line = raw.trim(); if (!line) continue;
    const match = line.match(/^\[([^\]]+)\]$/); if (match) { section = match[1].toLowerCase(); continue; }
    if (section === 'column names' && !columns.length) columns = line.split(/\s+/).map((value) => value.toLowerCase());
    else if (section === 'laptiming' && /^start\s/i.test(line)) {
      const values = line.match(/[+-]?\d+(?:\.\d+)?/g)?.map(Number) || []; if (values.length >= 4) gate = values.slice(0, 4);
    } else if (section === 'data') data.push(line.split(/\s+/).map(Number));
  }
  const lat = columns.indexOf('latitude'); const lon = columns.indexOf('longitude'); const time = columns.indexOf('time');
  if (!gate || lat < 0 || lon < 0 || time < 0 || data.length < 2) return [];
  const intersects = (a: number[], b: number[], c: number[], d: number[]) => {
    const cross = (p: number[], q: number[], r: number[]) => (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0]);
    return cross(a, b, c) * cross(a, b, d) <= 0 && cross(c, d, a) * cross(c, d, b) <= 0;
  };
  const seconds = (value: number) => { const hours = Math.floor(value / 10000); const minutes = Math.floor(value / 100) % 100; return hours * 3600 + minutes * 60 + value % 100; };
  const origin = seconds(data[0][time]); const crossings: number[] = [];
  for (let index = 1; index < data.length; index++) {
    if (intersects([data[index - 1][lon], data[index - 1][lat]], [data[index][lon], data[index][lat]], [gate[0], gate[1]], [gate[2], gate[3]])) {
      let elapsed = seconds(data[index][time]) - origin; if (elapsed < 0) elapsed += 86400;
      const ns = Math.round(elapsed * 1e9); if (!crossings.length || ns - crossings.at(-1)! > 10_000_000_000) crossings.push(ns);
    }
  }
  return crossings;
}

async function loadFile(file: File) {
  if (!conn) return toast('DuckDB is still starting', true);
  const extension = file.name.split('.').pop()?.toLowerCase();
  if (!extension || !['pds', 'ld', 'vbo'].includes(extension)) return toast('Choose a .pds, .ld, or .vbo telemetry file', true);
  setRuntime(`Reading ${file.name}`, 'loading');
  try {
    if (activeFile) await db.dropFile(activeFile).catch(() => undefined);
    activeFile = `active_run.${extension}`;
    activeDisplayName = file.name;
    const fileBytes = new Uint8Array(await file.arrayBuffer());
    vboLapCrossings = extension === 'vbo' ? detectVboLapCrossings(fileBytes) : [];
    await db.registerFileBuffer(activeFile, fileBytes);
    metadata = arrowRows(await conn.query(`SELECT * FROM telemetry_metadata(${sqlLiteral(activeFile)}) ORDER BY name`));
    if (!metadata.length) throw new Error('The parser found no telemetry channels');
    $('#workspace').classList.remove('hidden');
    $('#fileName').textContent = file.name;
    $('#fileFormat').textContent = String(metadata[0].format).toUpperCase();
    $('#fileSize').textContent = formatBytes(file.size);
    initializeSignalRoles();
    renderChannels(metadata);
    await renderInsights();
    await loadTrackMap();
    setPresets();
    setRuntime(`${file.name} · Ready`, 'ready');
    $('#workspace').scrollIntoView({ behavior: 'smooth', block: 'start' });
  } catch (error) {
    console.error(error); setRuntime('Could not parse telemetry', 'error');
    toast(error instanceof Error ? error.message : String(error), true);
  }
}

function channelUnit(name: string) { return String(metadata.find((row) => String(row.name) === name)?.unit || ''); }
function speedToKmh(value: number, unit: string) {
  const normalized = unit.toLowerCase().replaceAll(' ', '');
  if (normalized === 'm/s' || normalized === 'mps') return value * 3.6;
  if (normalized === 'mph') return value * 1.609344;
  return value;
}
function accelerationToG(value: number, unit: string) {
  const normalized = unit.toLowerCase().replaceAll(' ', '');
  return normalized.includes('m/s') || normalized.includes('m.s') ? value / 9.80665 : value;
}

async function renderPerformanceMetrics(completeLaps: Lap[]) {
  const best = [...completeLaps].sort((a, b) => a.durationNs - b.durationNs)[0];
  let topSpeed: number | null = null; let peakDirectional: number | null = null; let peakCombined: number | null = null;
  if (signalRoles.speed) {
    const row = arrowRows(await conn.query(`SELECT max(value) AS value FROM telemetry_samples(${sqlLiteral(activeFile)}, channel := ${sqlLiteral(signalRoles.speed)})`))[0];
    topSpeed = speedToKmh(Number(row?.value), channelUnit(signalRoles.speed));
  }
  const directional = await Promise.all([signalRoles.gLat, signalRoles.gLong].filter(Boolean).map(async (channel) => {
    const row = arrowRows(await conn.query(`SELECT max(abs(value)) AS value FROM telemetry_samples(${sqlLiteral(activeFile)}, channel := ${sqlLiteral(channel)})`))[0];
    return accelerationToG(Number(row?.value), channelUnit(channel));
  }));
  if (directional.length) peakDirectional = Math.max(...directional.filter(Number.isFinite));
  if (signalRoles.gLat && signalRoles.gLong) {
    const latFactor = accelerationToG(1, channelUnit(signalRoles.gLat));
    const longFactor = accelerationToG(1, channelUnit(signalRoles.gLong));
    const row = arrowRows(await conn.query(`SELECT max(sqrt(pow(${quoteIdent(signalRoles.gLat)} * ${latFactor}, 2) + pow(${quoteIdent(signalRoles.gLong)} * ${longFactor}, 2))) AS value FROM read_telemetry(${sqlLiteral(activeFile)}, rate := 100, channels := ${sqlLiteral(`${signalRoles.gLat},${signalRoles.gLong}`)})`))[0];
    peakCombined = Number(row?.value);
  }
  const cards = [
    ['TOP SPEED', topSpeed === null || !Number.isFinite(topSpeed) ? '—' : `${formatNumber(topSpeed, 1)} km/h`, signalRoles.speed || 'speed role not set'],
    ['HIGHEST DIRECTIONAL G', peakDirectional === null || !Number.isFinite(peakDirectional) ? '—' : `${formatNumber(peakDirectional, 3)} g`, [signalRoles.gLat, signalRoles.gLong].filter(Boolean).join(' · ') || 'G roles not set'],
    ['PEAK COMBINED G', peakCombined === null || !Number.isFinite(peakCombined) ? '—' : `${formatNumber(peakCombined, 3)} g`, signalRoles.gLat && signalRoles.gLong ? '100 Hz synchronized resultant' : 'both G roles required'],
    ['BEST COMPLETE LAP', best ? formatDuration(best.durationNs) : '—', best?.label || 'no complete lap detected'],
  ];
  $('#performanceMetrics').innerHTML = cards.map(([label, value, note]) => `<article><small>${label}</small><strong>${value}</strong><p>${escapeHtml(note)}</p></article>`).join('');
}

async function renderInsights() {
  const sampled = metadata.filter((row) => Number(row.sample_count) > 0);
  const duration = Math.max(...sampled.map((row) => Number(row.duration_ns) || 0));
  const samples = sampled.reduce((sum, row) => sum + Number(row.sample_count || 0), 0);
  const maxRate = Math.max(...sampled.map((row) => Number(row.frequency_hz) || 0));
  const metrics = [
    ['RECORDED CHANNELS', sampled.length, `${metadata.length - sampled.length} definitions without samples`],
    ['SESSION DURATION', formatDuration(duration), `${(duration / 1e9).toFixed(1)} seconds`],
    ['NATIVE SAMPLES', formatNumber(samples, 0), 'exact, before interpolation'],
    ['FASTEST CLOCK', `${formatNumber(maxRate, 1)} Hz`, `${new Set(sampled.map((r) => Number(r.frequency_hz))).size} distinct rates`],
  ];
  $('#metrics').innerHTML = metrics.map(([label, value, note], i) => `<article class="metric"><span>0${i + 1}</span><small>${label}</small><strong>${value}</strong><p>${note}</p></article>`).join('');

  plotChannels = candidateChannels(metadata).slice(0, 3);
  const interesting = candidateChannels(metadata);
  let stats: Row[] = [];
  if (interesting.length) {
    stats = arrowRows(await conn.query(`
      SELECT channel, any_value(unit) AS unit, count(*) AS samples,
             min(value) AS minimum, avg(value) AS mean, max(value) AS maximum
      FROM telemetry_samples(${sqlLiteral(activeFile)}, channel := ${sqlLiteral(interesting.join(','))})
      GROUP BY channel ORDER BY channel`));
  }
  $('#findings').innerHTML = stats.length ? stats.map((row) => `
    <div class="finding"><div><strong>${row.channel}</strong><small>${row.samples} native samples · ${row.unit || 'unitless'}</small></div>
    <div class="range"><span>${formatNumber(row.minimum)}</span><b>${formatNumber(row.maximum)}</b></div></div>`).join('') : '<div class="no-signals">No conventional speed, pedal, acceleration, or gear channels were identified. Use SQL to inspect the exact channel names.</div>';

  laps = await detectLaps(duration);
  const complete = laps.filter((lap) => lap.complete && lap.durationNs > 5_000_000_000);
  activeLap = complete.sort((a, b) => a.durationNs - b.durationNs)[0] || laps[0];
  compareLap = laps.find((lap) => lap !== activeLap && lap.complete) || null;
  await renderPerformanceMetrics(complete);
  renderLapRail();
  renderCompareSelector();
  await loadLapTrace();
}

function lapCounterChannel(): string | null {
  const sampled = metadata.filter((row) => Number(row.sample_count) > 0);
  const normalized = (name: unknown) => String(name).toLowerCase().replace(/[^a-z0-9]/g, '');
  const priorities = ['lapnumber', 'lapnum', 'lapcount', 'lapcounter', 'currentlap', 'lap'];
  for (const wanted of priorities) {
    const match = sampled.find((row) => normalized(row.name) === wanted);
    if (match) return String(match.name);
  }
  return null;
}

async function detectLaps(sessionDurationNs: number): Promise<Lap[]> {
  const channel = lapCounterChannel();
  if (channel) {
    const starts = arrowRows(await conn.query(`
      SELECT CAST(round(value) AS BIGINT) AS lap_number, min(time_ns) AS start_ns
      FROM telemetry_samples(${sqlLiteral(activeFile)}, channel := ${sqlLiteral(channel)})
      WHERE isfinite(value) AND value >= 0 AND value < 100000
      GROUP BY 1 ORDER BY start_ns`));
    if (starts.length > 1) {
      return starts.map((row, index) => {
        const startNs = Number(row.start_ns);
        const endNs = index + 1 < starts.length ? Number(starts[index + 1].start_ns) : sessionDurationNs;
        const number = Number(row.lap_number);
        return { number, label: number > 0 ? `LAP ${number}` : `SEG ${index + 1}`, startNs, endNs, durationNs: Math.max(0, endNs - startNs), complete: index > 0 && index < starts.length - 1 };
      }).filter((lap) => lap.durationNs > 0);
    }
  }
  if (vboLapCrossings.length) {
    const starts = [0, ...vboLapCrossings];
    return starts.map((startNs, index) => {
      const endNs = starts[index + 1] ?? sessionDurationNs;
      return { number: index + 1, label: `LAP ${index + 1}`, startNs, endNs, durationNs: endNs - startNs, complete: index > 0 && index < starts.length - 1 };
    }).filter((lap) => lap.durationNs > 0);
  }
  const sampled = metadata.filter((row) => Number(row.sample_count) > 0);
  const exact = (names: string[]) => names.map((wanted) => sampled.find((row) => normalizedName(row.name) === wanted)).find(Boolean);
  const timer = exact(['lapcurrentlaptime', 'laptime', 'laptimerunning']);
  const distance = exact(['lapdistancecorrected', 'lapdistance', 'lapdist', 'lapdistpct']);
  const resetChannel = timer || distance;
  if (resetChannel) {
    const threshold = timer ? 5 : 20;
    const resets = arrowRows(await conn.query(`
      WITH ordered AS (
        SELECT time_ns, value, lag(value) OVER (ORDER BY time_ns) AS previous
        FROM telemetry_samples(${sqlLiteral(activeFile)}, channel := ${sqlLiteral(String(resetChannel.name))})
      )
      SELECT time_ns FROM ordered
      WHERE previous IS NOT NULL AND previous - value > ${threshold}
      ORDER BY time_ns`)).map((row) => Number(row.time_ns)).filter((time, index, times) => !index || time - times[index - 1] > 5_000_000_000);
    if (resets.length) {
      const starts = [0, ...resets];
      return starts.map((startNs, index) => {
        const endNs = starts[index + 1] ?? sessionDurationNs;
        return { number: index + 1, label: `LAP ${index + 1}`, startNs, endNs, durationNs: endNs - startNs, complete: index > 0 && index < starts.length - 1 };
      }).filter((lap) => lap.durationNs > 0);
    }
  }
  return [{ number: 1, label: 'SESSION', startNs: 0, endNs: sessionDurationNs, durationNs: sessionDurationNs, complete: true }];
}

function renderLapRail() {
  if (!activeLap) return;
  const complete = laps.filter((lap) => lap.complete && lap.durationNs > 5_000_000_000);
  const best = complete.sort((a, b) => a.durationNs - b.durationNs)[0];
  $('#lapRail').innerHTML = laps.map((lap, index) => `<button data-lap="${index}" class="${lap === activeLap ? 'active' : ''} ${!lap.complete ? 'partial' : ''}"><span>${lap.label}${lap === best ? '<b>BEST</b>' : ''}</span><strong>${formatDuration(lap.durationNs)}</strong></button>`).join('');
  document.querySelectorAll<HTMLButtonElement>('[data-lap]').forEach((button) => button.onclick = () => selectLap(laps[Number(button.dataset.lap)]));
  $('#traceLapLabel').textContent = `${activeLap.label} · ${formatDuration(activeLap.durationNs)}`;
}

function renderCompareSelector() {
  const select = $<HTMLSelectElement>('#compareLap');
  select.innerHTML = `<option value="">NO REFERENCE</option>${laps.map((lap, index) => `<option value="${index}" ${lap === compareLap ? 'selected' : ''} ${lap === activeLap ? 'disabled' : ''}>${lap.label} · ${formatDuration(lap.durationNs)}${lap.complete ? '' : ' · PARTIAL'}</option>`).join('')}`;
  $('#comparisonStatus').textContent = signalRoles.distance ? `${signalRoles.distance} · SOLID VS DASHED` : 'NORMALIZED LAP PROGRESS · SOLID VS DASHED';
}

async function selectLap(lap: Lap) {
  const previous = activeLap;
  activeLap = lap;
  if (compareLap === activeLap) compareLap = previous && previous !== activeLap ? previous : laps.find((item) => item !== activeLap) || null;
  renderLapRail();
  renderCompareSelector();
  await loadLapTrace();
  setPresets(true);
  drawTrackMap();
  if (inspectedChannel) await inspectChannel(inspectedChannel);
}

function rowProgress(rows: Row[], index: number) {
  if (!rows.length) return 0;
  if (signalRoles.distance) {
    const values = rows.map((row) => Number(row[signalRoles.distance])).filter(Number.isFinite);
    const value = Number(rows[index]?.[signalRoles.distance]);
    if (values.length > 1 && Number.isFinite(value)) {
      const min = Math.min(...values); const max = Math.max(...values);
      if (max > min) return Math.max(0, Math.min(1, (value - min) / (max - min)));
    }
  }
  return index / Math.max(rows.length - 1, 1);
}

function rowAtProgress(rows: Row[], progress: number) {
  if (!rows.length) return undefined;
  let best = rows[0]; let error = Infinity;
  rows.forEach((row, index) => { const difference = Math.abs(rowProgress(rows, index) - progress); if (difference < error) { best = row; error = difference; } });
  return best;
}

async function loadLapTrace() {
  plotRows = []; compareRows = []; scrubTimeNs = null;
  $('#scrubber').classList.add('hidden');
  const channels = [...new Set([...plotChannels, signalRoles.distance].filter(Boolean))];
  if (activeLap && channels.length && activeLap.durationNs > 0) {
    const rate = Math.min(100, Math.max(10, Math.ceil(5_000 / Math.max(activeLap.durationNs / 1e9, 1))));
    plotRows = arrowRows(await conn.query(`SELECT * FROM read_telemetry(${sqlLiteral(activeFile)}, rate := ${rate}, channels := ${sqlLiteral(channels.join(','))}, start_ns := ${Math.round(activeLap.startNs)}, end_ns := ${Math.round(activeLap.endNs)})`));
    if (compareLap) compareRows = arrowRows(await conn.query(`SELECT * FROM read_telemetry(${sqlLiteral(activeFile)}, rate := ${rate}, channels := ${sqlLiteral(channels.join(','))}, start_ns := ${Math.round(compareLap.startNs)}, end_ns := ${Math.round(compareLap.endNs)})`));
  }
  drawTrace();
}

function drawTrace() {
  const canvas = $<HTMLCanvasElement>('#trace');
  const dpr = devicePixelRatio || 1; const rect = canvas.getBoundingClientRect();
  canvas.width = Math.max(1, rect.width * dpr); canvas.height = 260 * dpr;
  const ctx = canvas.getContext('2d')!; ctx.scale(dpr, dpr);
  const width = rect.width; const height = 260; ctx.clearRect(0, 0, width, height);
  ctx.strokeStyle = '#262b2d'; ctx.lineWidth = 1;
  for (let i = 0; i <= 5; i++) { const y = 16 + i * 42; ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(width, y); ctx.stroke(); }
  const colors = ['#d8ff32', '#ff5c35', '#49b9ff'];
  $('#traceLegend').innerHTML = plotChannels.map((name, i) => `<span><i style="background:${colors[i]}"></i>${name}</span>`).join('');
  plotChannels.forEach((channel, index) => {
    const values = [...plotRows, ...compareRows].map((row) => Number(row[channel])).filter(Number.isFinite);
    if (values.length < 2) return;
    const min = Math.min(...values); const max = Math.max(...values); const span = max - min || 1; const band = height / plotChannels.length;
    const stroke = (rows: Row[], dashed: boolean) => {
      if (rows.length < 2) return; ctx.beginPath(); ctx.setLineDash(dashed ? [5, 5] : []); ctx.globalAlpha = dashed ? .58 : 1;
      rows.forEach((row, i) => {
        const value = Number(row[channel]); if (!Number.isFinite(value)) return;
        const x = rowProgress(rows, i) * width; const y = index * band + 10 + (1 - (value - min) / span) * (band - 22);
        if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
      });
      ctx.strokeStyle = colors[index]; ctx.lineWidth = dashed ? 1 : index === 0 ? 2 : 1.4; ctx.stroke();
    };
    stroke(compareRows, true); stroke(plotRows, false); ctx.setLineDash([]); ctx.globalAlpha = 1;
  });
}

function showScrubAtProgress(ratio: number, reveal = true) {
  if (!plotRows.length || !activeLap) return;
  const row = rowAtProgress(plotRows, ratio); if (!row) return;
  const compare = rowAtProgress(compareRows, ratio);
  const elapsed = (Number(row.time_ns) - activeLap.startNs) / 1e9;
  const compareElapsed = compare && compareLap ? (Number(compare.time_ns) - compareLap.startNs) / 1e9 : null;
  const delta = compareElapsed === null ? '' : `<span class="delta">DELTA TO ${escapeHtml(compareLap!.label)} <b>${elapsed - compareElapsed >= 0 ? '+' : ''}${(elapsed - compareElapsed).toFixed(3)} s</b></span>`;
  const scrubber = $('#scrubber'); scrubber.style.left = `${ratio * 100}%`;
  $('#scrubValues').innerHTML = `<strong>+${elapsed.toFixed(3)} s · ${(ratio * 100).toFixed(1)}%</strong>${plotChannels.map((channel, index) => `<span><i style="background:${['#d8ff32', '#ff5c35', '#49b9ff'][index]}"></i>${escapeHtml(channel)} <b>${formatNumber(row[channel], 3)}</b></span>`).join('')}${delta}`;
  scrubber.classList.toggle('hidden', !reveal); scrubTimeNs = Number(row.time_ns); drawTrackMap();
}

function scrubTrace(clientX: number) {
  const rect = $<HTMLCanvasElement>('#trace').getBoundingClientRect();
  showScrubAtProgress(Math.max(0, Math.min(1, (clientX - rect.left) / rect.width)));
}

async function inspectChannel(name: string) {
  if (!activeLap) return;
  inspectedChannel = name;
  const meta = metadata.find((row) => String(row.name) === name);
  if (!meta || Number(meta.sample_count) === 0) return;
  $('#channelInspector').classList.remove('hidden');
  $('#inspectorScrim').classList.remove('hidden');
  $('#inspectorName').textContent = name;
  $('#inspectorLap').textContent = activeLap.label;
  $('#inspectorSummary').innerHTML = '<div class="inspector-loading">QUERYING EXACT SAMPLES…</div>';
  const bounds = `start_ns := ${Math.round(activeLap.startNs)}, end_ns := ${Math.round(activeLap.endNs)}`;
  try {
    const stats = arrowRows(await conn.query(`
      SELECT count(*) AS samples, min(value) AS minimum, avg(value) AS mean,
             max(value) AS maximum, approx_count_distinct(value) AS distinct_values
      FROM telemetry_samples(${sqlLiteral(activeFile)}, channel := ${sqlLiteral(name)}, ${bounds})`))[0];
    const snippets = arrowRows(await conn.query(`
      WITH source AS (
        SELECT time_ns, value, row_number() OVER (ORDER BY time_ns) AS rn,
               count(*) OVER () AS total
        FROM telemetry_samples(${sqlLiteral(activeFile)}, channel := ${sqlLiteral(name)}, ${bounds})
      )
      SELECT (time_ns - ${Math.round(activeLap.startNs)}) / 1e9 AS lap_seconds, value
      FROM source
      WHERE (rn - 1) % greatest(1, CAST(ceil(total / 80.0) AS BIGINT)) = 0
      ORDER BY time_ns LIMIT 80`));
    const discreteName = /(gear|lap|switch|flag|state|status|event|alarm|beacon)/i.test(name);
    const semantics = discreteName || !String(meta.data_type).startsWith('float') ? 'STEP / DISCRETE' : 'LINEAR / CONTINUOUS';
    $('#inspectorSummary').innerHTML = [
      ['STORAGE', String(meta.data_type), `${formatNumber(meta.frequency_hz)} Hz native`],
      ['SEMANTICS', semantics, 'wide-reader interpolation'],
      ['LAP RANGE', `${formatNumber(stats.minimum, 3)} → ${formatNumber(stats.maximum, 3)}`, String(meta.unit || 'unitless')],
      ['LAP SAMPLES', formatNumber(stats.samples, 0), `≈ ${formatNumber(stats.distinct_values, 0)} distinct`],
    ].map(([label, value, note]) => `<div><small>${label}</small><strong>${escapeHtml(value)}</strong><span>${escapeHtml(note)}</span></div>`).join('');
    $('#sampleList').innerHTML = snippets.slice(0, 24).map((row) => `<div><span>+${Number(row.lap_seconds).toFixed(3)} s</span><b>${formatNumber(row.value, 5)}</b></div>`).join('') || '<div class="no-samples">No samples in this lap window.</div>';
    drawChannelShape(snippets);
  } catch (error) {
    $('#inspectorSummary').innerHTML = `<div class="inspector-loading error">${escapeHtml(error instanceof Error ? error.message : String(error))}</div>`;
  }
}

function drawChannelShape(rows: Row[]) {
  const canvas = $<HTMLCanvasElement>('#channelShape'); const rect = canvas.getBoundingClientRect(); const dpr = devicePixelRatio || 1;
  canvas.width = Math.max(1, rect.width * dpr); canvas.height = 120 * dpr;
  const ctx = canvas.getContext('2d')!; ctx.scale(dpr, dpr); ctx.clearRect(0, 0, rect.width, 120);
  const values = rows.map((row) => Number(row.value)).filter(Number.isFinite); if (values.length < 2) return;
  const min = Math.min(...values); const max = Math.max(...values); const span = max - min || 1;
  ctx.strokeStyle = '#252b29'; for (let i = 1; i < 4; i++) { ctx.beginPath(); ctx.moveTo(0, i * 30); ctx.lineTo(rect.width, i * 30); ctx.stroke(); }
  ctx.strokeStyle = '#d8ff32'; ctx.lineWidth = 1.5; ctx.beginPath();
  values.forEach((value, index) => { const x = index / (values.length - 1) * rect.width; const y = 8 + (1 - (value - min) / span) * 104; if (index) ctx.lineTo(x, y); else ctx.moveTo(x, y); }); ctx.stroke();
}

function closeInspector() {
  inspectedChannel = '';
  $('#channelInspector').classList.add('hidden');
  $('#inspectorScrim').classList.add('hidden');
}

function renderChannels(rows: Row[]) {
  $('#channelCount').textContent = `${rows.filter((r) => Number(r.sample_count) > 0).length} RECORDED / ${rows.length} DEFINED`;
  $('#channelRows').innerHTML = rows.map((row) => `<tr data-channel="${escapeHtml(String(row.name))}" class="${Number(row.sample_count) ? 'inspectable' : 'empty'}"><td><b>${escapeHtml(String(row.name))}</b></td><td>${escapeHtml(String(row.unit || '—'))}</td><td>${row.data_type}</td><td>${formatNumber(row.frequency_hz, 2)} Hz</td><td>${formatNumber(row.sample_count, 0)}</td><td>${formatDuration(row.duration_ns)}</td></tr>`).join('');
}
function escapeHtml(value: string) { const div = document.createElement('div'); div.textContent = value; return div.innerHTML; }

function setPresets(preserveEditor = false) {
  const candidates = candidateChannels(metadata);
  const first = candidates[0] || String(metadata.find((r) => Number(r.sample_count))?.name || '');
  const lap = activeLap || { startNs: 0, endNs: 10_000_000_000, label: 'SESSION' };
  const start = Math.round(lap.startNs); const end = Math.round(lap.endNs);
  const sampled = metadata.filter((row) => Number(row.sample_count) > 0);
  const find = (pattern: RegExp) => String(sampled.find((row) => pattern.test(String(row.name)))?.name || '');
  const speed = signalRoles.speed || first;
  const throttle = signalRoles.throttle;
  const brake = signalRoles.brake;
  const gear = find(/(^|[^a-z])gear([^a-z]|$)/i);
  const gLat = signalRoles.gLat;
  const gLong = signalRoles.gLong;
  const gChannels = [...new Set([gLat, gLong].filter(Boolean))];
  const speedFactor = speedToKmh(1, channelUnit(speed));
  const gFactors = Object.fromEntries(gChannels.map((channel) => [channel, accelerationToG(1, channelUnit(channel))]));
  const gCase = gChannels.map((channel) => `WHEN ${sqlLiteral(channel)} THEN ${gFactors[channel]}`).join(' ');
  const presets: Record<string, string> = {
    metadata: `SELECT name, unit, frequency_hz, sample_count\nFROM telemetry_metadata(${sqlLiteral(activeFile)})\nWHERE sample_count > 0\nORDER BY name;`,
    stats: `SELECT channel, any_value(unit) AS unit,\n       count(*) AS samples, min(value), avg(value), max(value)\nFROM telemetry_samples(${sqlLiteral(activeFile)},\n     channel := ${sqlLiteral(candidates.join(','))},\n     start_ns := ${start}, end_ns := ${end})\nGROUP BY channel\nORDER BY channel;`,
    samples: `SELECT (time_ns - ${start}) / 1e9 AS lap_seconds, value\nFROM telemetry_samples(${sqlLiteral(activeFile)},\n     channel := ${sqlLiteral(first)},\n     start_ns := ${start}, end_ns := ${end})\nORDER BY time_ns\nLIMIT 500;`,
  };
  const recipes = [
    ['Top speed in session', `${speed} · normalized to km/h`, `SELECT max(value) * ${speedFactor} AS top_speed_kmh,\n       arg_max(time_ns, value) / 1e9 AS session_seconds\nFROM telemetry_samples(${sqlLiteral(activeFile)}, channel := ${sqlLiteral(speed)});`],
    ['Highest G in session', gChannels.length ? `${gChannels.join(' + ')} · normalized to g` : 'No G channel detected', gChannels.length ? `WITH normalized AS (\n  SELECT channel, time_ns, value * CASE channel ${gCase} END AS value_g\n  FROM telemetry_samples(${sqlLiteral(activeFile)}, channel := ${sqlLiteral(gChannels.join(','))})\n)\nSELECT arg_max(channel, abs(value_g)) AS channel,\n       arg_max(value_g, abs(value_g)) AS signed_g,\n       max(abs(value_g)) AS peak_absolute_g,\n       arg_max(time_ns, abs(value_g)) / 1e9 AS session_seconds\nFROM normalized;` : `SELECT 'No lateral or longitudinal G channel was detected' AS message;`],
    ['Peak combined G', gLat && gLong ? `${gLat} + ${gLong} · normalized to g` : 'Requires lateral and longitudinal G', gLat && gLong ? `WITH g AS (\n  SELECT time_ns, sqrt(pow(${quoteIdent(gLat)} * ${gFactors[gLat]}, 2) + pow(${quoteIdent(gLong)} * ${gFactors[gLong]}, 2)) AS combined_g\n  FROM read_telemetry(${sqlLiteral(activeFile)}, rate := 100,\n       channels := ${sqlLiteral(`${gLat},${gLong}`)})\n)\nSELECT max(combined_g) AS peak_combined_g,\n       arg_max(time_ns, combined_g) / 1e9 AS session_seconds\nFROM g;` : `SELECT 'Both lateral and longitudinal G channels are required' AS message;`],
    ['Native rate audit', 'Compare logger clocks and volume', `SELECT frequency_hz, count(*) AS channels, sum(sample_count) AS samples\nFROM telemetry_metadata(${sqlLiteral(activeFile)})\nWHERE sample_count > 0\nGROUP BY frequency_hz ORDER BY frequency_hz DESC;`],
    ['Current lap · wide', '100 Hz interpolated channel set', `SELECT (time_ns - ${start}) / 1e9 AS lap_seconds, * EXCLUDE (time_ns)\nFROM read_telemetry(${sqlLiteral(activeFile)}, rate := 100,\n     channels := ${sqlLiteral(candidates.join(','))},\n     start_ns := ${start}, end_ns := ${end})\nLIMIT 1000;`],
    ['Percentile envelope', `Distribution of ${speed}`, `SELECT quantile_cont(value, [0, .01, .1, .5, .9, .99, 1]) AS percentiles\nFROM telemetry_samples(${sqlLiteral(activeFile)}, channel := ${sqlLiteral(speed)});`],
    ['Channel coverage', 'Definitions with no physical samples', `SELECT name, unit, frequency_hz\nFROM telemetry_metadata(${sqlLiteral(activeFile)})\nWHERE sample_count = 0 ORDER BY name;`],
    ['Gear usage', gear ? `Time distribution for ${gear}` : 'Adapt after selecting a gear channel', gear ? `SELECT ${quoteIdent(gear)} AS gear, count(*) / 100.0 AS seconds\nFROM read_telemetry(${sqlLiteral(activeFile)}, rate := 100, channels := ${sqlLiteral(gear)},\n     start_ns := ${start}, end_ns := ${end})\nGROUP BY 1 ORDER BY 1;` : presets.samples],
    ['Pedal overlap', brake && throttle ? `${brake} × ${throttle}` : 'Adapt after selecting pedal channels', brake && throttle ? `SELECT count(*) / 100.0 AS overlap_seconds\nFROM read_telemetry(${sqlLiteral(activeFile)}, rate := 100,\n     channels := ${sqlLiteral(`${brake},${throttle}`)}, start_ns := ${start}, end_ns := ${end})\nWHERE ${quoteIdent(brake)} > 0 AND ${quoteIdent(throttle)} > 0;` : presets.stats],
    ['Discrete transitions', `Changes in ${gear || first}`, `WITH s AS (\n  SELECT time_ns, value, lag(value) OVER (ORDER BY time_ns) AS previous\n  FROM telemetry_samples(${sqlLiteral(activeFile)}, channel := ${sqlLiteral(gear || first)},\n       start_ns := ${start}, end_ns := ${end})\n) SELECT (time_ns - ${start}) / 1e9 AS lap_seconds, previous, value\nFROM s WHERE value IS DISTINCT FROM previous ORDER BY time_ns;`],
  ];
  if (!preserveEditor) editor.value = presets.metadata;
  updateLines();
  document.querySelectorAll<HTMLButtonElement>('[data-query]').forEach((button) => button.onclick = () => { editor.value = presets[button.dataset.query!]; updateLines(); });
  $('#recipeGrid').innerHTML = recipes.map(([title, description], index) => `<button data-recipe="${index}"><span>0${index + 1}</span><strong>${escapeHtml(title)}</strong><small>${escapeHtml(description)}</small><b>LOAD ↗</b></button>`).join('');
  document.querySelectorAll<HTMLButtonElement>('[data-recipe]').forEach((button) => button.onclick = () => { editor.value = recipes[Number(button.dataset.recipe)][2]; updateLines(); editor.scrollIntoView({ behavior: 'smooth', block: 'center' }); });
}

async function runQuery() {
  const sql = editor.value.trim(); if (!sql) return;
  $('#runQuery').setAttribute('disabled', 'true'); $('#queryTiming').textContent = 'RUNNING…';
  try {
    const started = performance.now(); const table = await conn.query(sql); const elapsed = performance.now() - started;
    const all = arrowRows(table); const result: QueryResult = { columns: table.schema.fields.map((f) => f.name), rows: all.slice(0, 500), elapsed, total: all.length };
    renderResult(result);
  } catch (error) {
    $('#queryTiming').textContent = 'ERROR';
    $('#queryResult').innerHTML = `<div class="query-error"><b>QUERY ERROR</b><pre>${escapeHtml(error instanceof Error ? error.message : String(error))}</pre></div>`;
  } finally { $('#runQuery').removeAttribute('disabled'); }
}
function renderResult(result: QueryResult) {
  $('#queryTiming').textContent = `${result.elapsed.toFixed(1)} MS · ${result.total} ROWS`;
  if (!result.columns.length) { $('#queryResult').innerHTML = '<div class="empty-result">Statement completed.</div>'; return; }
  $('#queryResult').innerHTML = `<table><thead><tr>${result.columns.map((c) => `<th>${escapeHtml(c)}</th>`).join('')}</tr></thead><tbody>${result.rows.map((row) => `<tr>${result.columns.map((c) => `<td>${escapeHtml(displayCell(row[c]))}</td>`).join('')}</tr>`).join('')}</tbody></table>${result.total > 500 ? '<div class="truncated">Showing first 500 rows</div>' : ''}`;
}
function displayCell(value: unknown): string {
  if (value === null || value === undefined) return 'NULL';
  if (typeof value === 'object') { try { return JSON.stringify(value, (_, v) => typeof v === 'bigint' ? v.toString() : v); } catch { return String(value); } }
  return String(value);
}
function updateLines() { $('#lineNumbers').textContent = Array.from({ length: editor.value.split('\n').length }, (_, i) => i + 1).join('\n'); }

document.querySelectorAll<HTMLButtonElement>('[data-example]').forEach((button) => {
  button.addEventListener('click', () => loadExample(Number(button.dataset.example), button));
});
$('#copyInstall').addEventListener('click', async () => {
  const sql = `INSTALL httpfs;\nLOAD httpfs;\nINSTALL motorsport_telemetry FROM 'https://pages.tobi.lutke.com/duckdb_motorsport_telemetry';\nLOAD motorsport_telemetry;`;
  await navigator.clipboard.writeText(sql);
  toast('Installation SQL copied');
});
input.addEventListener('change', () => input.files?.[0] && loadFile(input.files[0]));
$('#replaceFile').addEventListener('click', () => input.click());
$('#runQuery').addEventListener('click', runQuery);
editor.addEventListener('input', updateLines);
editor.addEventListener('keydown', (event) => { if ((event.metaKey || event.ctrlKey) && event.key === 'Enter') { event.preventDefault(); runQuery(); } });
$('#channelSearch').addEventListener('input', (event) => { const term = (event.target as HTMLInputElement).value.toLowerCase(); renderChannels(metadata.filter((row) => String(row.name).toLowerCase().includes(term) || String(row.unit).toLowerCase().includes(term))); });
$('#channelRows').addEventListener('click', (event) => {
  const row = (event.target as HTMLElement).closest<HTMLTableRowElement>('tr.inspectable');
  if (row?.dataset.channel) inspectChannel(row.dataset.channel);
});
$('#closeInspector').addEventListener('click', closeInspector);
$('#inspectorScrim').addEventListener('click', closeInspector);
$('#resetRoles').addEventListener('click', async () => {
  localStorage.removeItem(roleStorageKey()); signalRoles = detectSignalRoles(metadata); renderRoleEditor(); await refreshSignalAnalysis();
});
$('#compareLap').addEventListener('change', async (event) => {
  const value = (event.target as HTMLSelectElement).value; compareLap = value === '' ? null : laps[Number(value)]; await loadLapTrace(); renderCompareSelector();
});
$('#trace').addEventListener('pointermove', (event) => scrubTrace(event.clientX));
$('#trace').addEventListener('pointerdown', (event) => scrubTrace(event.clientX));
$('#trace').addEventListener('pointerleave', () => $('#scrubber').classList.add('hidden'));
$('#trackMap').addEventListener('pointermove', (event) => scrubMap(event.clientX, event.clientY));
$('#trackMap').addEventListener('pointerleave', () => $('#mapTooltip').classList.add('hidden'));
const drop = $('#dropZone');
for (const type of ['dragenter', 'dragover']) drop.addEventListener(type, (event) => { event.preventDefault(); drop.classList.add('dragging'); });
for (const type of ['dragleave', 'drop']) drop.addEventListener(type, (event) => { event.preventDefault(); drop.classList.remove('dragging'); });
drop.addEventListener('drop', (event) => { const file = (event as DragEvent).dataTransfer?.files[0]; if (file) loadFile(file); });
window.addEventListener('resize', () => { drawTrace(); drawTrackMap(); });

init();
