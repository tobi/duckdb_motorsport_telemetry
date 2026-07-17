import * as duckdb from '@duckdb/duckdb-wasm';
import duckdbMvp from '@duckdb/duckdb-wasm/dist/duckdb-mvp.wasm?url';
import duckdbEh from '@duckdb/duckdb-wasm/dist/duckdb-eh.wasm?url';
import mvpWorker from '@duckdb/duckdb-wasm/dist/duckdb-browser-mvp.worker.js?url';
import ehWorker from '@duckdb/duckdb-wasm/dist/duckdb-browser-eh.worker.js?url';
import './style.css';

type Row = Record<string, unknown>;
type QueryResult = { columns: string[]; rows: Row[]; elapsed: number; total: number };
type Lap = { number: number; label: string; startNs: number; endNs: number; durationNs: number; complete: boolean };
type ExampleRun = { name: string; eyebrow: string; detail: string; size: string; license: string; url: string; source: string };

const BASE = '/duckdb_motorsport_telemetry/';
const EXT_REPO = `${location.origin}${BASE.replace(/\/$/, '')}`;
const bundles: duckdb.DuckDBBundles = {
  mvp: { mainModule: duckdbMvp, mainWorker: mvpWorker },
  eh: { mainModule: duckdbEh, mainWorker: ehWorker },
};
const EXAMPLES: ExampleRun[] = [
  {
    name: 'Lotus Evora · VIR Full', eyebrow: 'REAL CAR / MOTEC LD', detail: '199 channels · 3 lap IDs · 133 seconds', size: '4.8 MB', license: 'MIT',
    url: 'https://raw.githubusercontent.com/Friss/i3rs/faf1f520510eee23ec48462ea6cc89f29bd3b6ec/test_data/VIR_LAP.ld',
    source: 'https://github.com/Friss/i3rs/blob/faf1f520510eee23ec48462ea6cc89f29bd3b6ec/test_data/VIR_LAP.ld',
  },
  {
    name: 'Lamborghini GT3 · Barcelona', eyebrow: 'IRACING / MOTEC LD / GPS', detail: '330 channels · 3 lap IDs · 107 seconds', size: '7.5 MB', license: 'Apache-2.0',
    url: 'https://raw.githubusercontent.com/JBonifay/motec-file-parser/34edb90bfc0374f500817cdb7151a99f3e9a98b5/sample.ld',
    source: 'https://github.com/JBonifay/motec-file-parser/blob/34edb90bfc0374f500817cdb7151a99f3e9a98b5/sample.ld',
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
      <label class="drop-card" for="fileInput">
        <input id="fileInput" type="file" accept=".pds,.ld,.vbo" />
        <div class="drop-rings"><div class="upload-icon">↥</div></div>
        <strong>DROP A RUN HERE</strong>
        <small>or click to choose a file</small>
        <div class="privacy">◉ Your data never leaves this machine</div>
      </label>
    </section>
    <section class="example-runs" aria-label="Public example telemetry">
      <div class="example-label"><span>NO FILE HANDY?</span><strong>Run public telemetry</strong><small>Fetched directly from the attributed open-source repository.</small></div>
      ${EXAMPLES.map((example, index) => `<div class="example-run"><button data-example="${index}"><span>${example.eyebrow}</span><strong>${example.name}</strong><small>${example.detail}</small><b>LOAD ${example.size} →</b></button><a href="${example.source}" target="_blank" rel="noreferrer">SOURCE · ${example.license} ↗</a></div>`).join('')}
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
      <div class="analysis-grid">
        <article class="panel trace-panel">
          <div class="panel-title"><div><span class="pulse"></span> QUICK TRACE <b id="traceLapLabel"></b></div><div id="traceLegend" class="legend"></div></div>
          <div class="lap-rail" id="lapRail"></div>
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
        <canvas id="trackMap" height="430"></canvas>
        <div class="axis-label">FULL SESSION · SELECTED LAP HIGHLIGHTED</div>
      </article>

      <div class="section-head"><div><span>02</span><h2>Channel map</h2></div><p>Source-exact names, units, and clocks</p></div>
      <div class="panel channel-panel">
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
  } catch (error) {
    console.error(error); setRuntime('Runtime failed to start', 'error');
    toast(error instanceof Error ? error.message : String(error), true);
  }
}

function candidateChannels(rows: Row[]): string[] {
  const sampled = rows.filter((row) => Number(row.sample_count) > 0);
  const groups = [
    /(^|[^a-z])(speed|velocity|vcar)([^a-z]|$)/i,
    /(brake|p_f_brake)/i,
    /(throttle|pedal)/i,
    /(accel.*lat|lat.*accel|glat|g force lat)/i,
    /(accel.*long|long.*accel|glong|g force long)/i,
    /(^|[^a-z])gear([^a-z]|$)/i,
  ];
  const picked: string[] = [];
  for (const pattern of groups) {
    const match = sampled.find((row) => pattern.test(String(row.name)) && !picked.includes(String(row.name)));
    if (match) picked.push(String(match.name));
  }
  return picked.slice(0, 6);
}

async function loadExample(index: number, button: HTMLButtonElement) {
  const example = EXAMPLES[index];
  if (!example || !conn) return;
  button.disabled = true;
  const original = button.querySelector('b')?.textContent || '';
  const progress = button.querySelector('b');
  if (progress) progress.textContent = `FETCHING ${example.size}…`;
  setRuntime(`Downloading ${example.name}`, 'loading');
  try {
    const response = await fetch(example.url);
    if (!response.ok) throw new Error(`Example download failed: HTTP ${response.status}`);
    const bytes = await response.arrayBuffer();
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
  $('#mapLapLabel').textContent = activeLap.label;
  $('#mapCoords').textContent = `${meanLat.toFixed(4)}°, ${(longitudes.reduce((sum, value) => sum + value, 0) / longitudes.length).toFixed(4)}°`;
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
    await db.registerFileBuffer(activeFile, new Uint8Array(await file.arrayBuffer()));
    metadata = arrowRows(await conn.query(`SELECT * FROM telemetry_metadata(${sqlLiteral(activeFile)}) ORDER BY name`));
    if (!metadata.length) throw new Error('The parser found no telemetry channels');
    $('#workspace').classList.remove('hidden');
    $('#fileName').textContent = file.name;
    $('#fileFormat').textContent = String(metadata[0].format).toUpperCase();
    $('#fileSize').textContent = formatBytes(file.size);
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
  renderLapRail();
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

async function selectLap(lap: Lap) {
  activeLap = lap;
  renderLapRail();
  await loadLapTrace();
  setPresets(true);
  drawTrackMap();
  if (inspectedChannel) await inspectChannel(inspectedChannel);
}

async function loadLapTrace() {
  plotRows = [];
  $('#scrubber').classList.add('hidden');
  if (activeLap && plotChannels.length && activeLap.durationNs > 0) {
    const seconds = activeLap.durationNs / 1e9;
    const rate = Math.min(100, Math.max(10, Math.ceil(5_000 / Math.max(seconds, 1))));
    plotRows = arrowRows(await conn.query(`SELECT * FROM read_telemetry(${sqlLiteral(activeFile)}, rate := ${rate}, channels := ${sqlLiteral(plotChannels.join(','))}, start_ns := ${Math.round(activeLap.startNs)}, end_ns := ${Math.round(activeLap.endNs)})`));
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
    const values = plotRows.map((row) => Number(row[channel])).filter(Number.isFinite);
    if (values.length < 2) return;
    const min = Math.min(...values); const max = Math.max(...values); const span = max - min || 1;
    ctx.strokeStyle = colors[index]; ctx.lineWidth = index === 0 ? 2 : 1.4; ctx.beginPath();
    plotRows.forEach((row, i) => {
      const value = Number(row[channel]); if (!Number.isFinite(value)) return;
      const x = (i / Math.max(plotRows.length - 1, 1)) * width;
      const band = height / plotChannels.length; const y = index * band + 10 + (1 - (value - min) / span) * (band - 22);
      if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
    }); ctx.stroke();
  });
}

function scrubTrace(clientX: number) {
  if (!plotRows.length || !activeLap) return;
  const canvas = $<HTMLCanvasElement>('#trace');
  const rect = canvas.getBoundingClientRect();
  const ratio = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
  const row = plotRows[Math.round(ratio * (plotRows.length - 1))];
  const elapsed = (Number(row.time_ns) - activeLap.startNs) / 1e9;
  const scrubber = $('#scrubber');
  scrubber.style.left = `${ratio * 100}%`;
  $('#scrubValues').innerHTML = `<strong>+${elapsed.toFixed(3)} s</strong>${plotChannels.map((channel, index) => `<span><i style="background:${['#d8ff32', '#ff5c35', '#49b9ff'][index]}"></i>${escapeHtml(channel)} <b>${formatNumber(row[channel], 3)}</b></span>`).join('')}`;
  scrubber.classList.remove('hidden');
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
  const find = (pattern: RegExp) => String(metadata.find((row) => Number(row.sample_count) > 0 && pattern.test(String(row.name)))?.name || '');
  const speed = find(/speed|velocity/i) || first;
  const throttle = find(/throttle|pedal/i);
  const brake = find(/brake/i);
  const gear = find(/(^|[^a-z])gear([^a-z]|$)/i);
  const accel = find(/accel.*(long|lat)|g.?force|glong|glat/i) || first;
  const presets: Record<string, string> = {
    metadata: `SELECT name, unit, frequency_hz, sample_count\nFROM telemetry_metadata(${sqlLiteral(activeFile)})\nWHERE sample_count > 0\nORDER BY name;`,
    stats: `SELECT channel, any_value(unit) AS unit,\n       count(*) AS samples, min(value), avg(value), max(value)\nFROM telemetry_samples(${sqlLiteral(activeFile)},\n     channel := ${sqlLiteral(candidates.join(','))},\n     start_ns := ${start}, end_ns := ${end})\nGROUP BY channel\nORDER BY channel;`,
    samples: `SELECT (time_ns - ${start}) / 1e9 AS lap_seconds, value\nFROM telemetry_samples(${sqlLiteral(activeFile)},\n     channel := ${sqlLiteral(first)},\n     start_ns := ${start}, end_ns := ${end})\nORDER BY time_ns\nLIMIT 500;`,
  };
  const recipes = [
    ['Native rate audit', 'Compare logger clocks and volume', `SELECT frequency_hz, count(*) AS channels, sum(sample_count) AS samples\nFROM telemetry_metadata(${sqlLiteral(activeFile)})\nWHERE sample_count > 0\nGROUP BY frequency_hz ORDER BY frequency_hz DESC;`],
    ['Current lap · wide', '100 Hz interpolated channel set', `SELECT (time_ns - ${start}) / 1e9 AS lap_seconds, * EXCLUDE (time_ns)\nFROM read_telemetry(${sqlLiteral(activeFile)}, rate := 100,\n     channels := ${sqlLiteral(candidates.join(','))},\n     start_ns := ${start}, end_ns := ${end})\nLIMIT 1000;`],
    ['Percentile envelope', `Distribution of ${speed}`, `SELECT quantile_cont(value, [0, .01, .1, .5, .9, .99, 1]) AS percentiles\nFROM telemetry_samples(${sqlLiteral(activeFile)}, channel := ${sqlLiteral(speed)},\n     start_ns := ${start}, end_ns := ${end});`],
    ['Peak events', `Largest absolute ${accel} values`, `SELECT (time_ns - ${start}) / 1e9 AS lap_seconds, value\nFROM telemetry_samples(${sqlLiteral(activeFile)}, channel := ${sqlLiteral(accel)},\n     start_ns := ${start}, end_ns := ${end})\nORDER BY abs(value) DESC LIMIT 20;`],
    ['Fastest zones', `Top ${speed} samples`, `SELECT (time_ns - ${start}) / 1e9 AS lap_seconds, value\nFROM telemetry_samples(${sqlLiteral(activeFile)}, channel := ${sqlLiteral(speed)},\n     start_ns := ${start}, end_ns := ${end})\nORDER BY value DESC LIMIT 25;`],
    ['Channel coverage', 'Definitions with no physical samples', `SELECT name, unit, frequency_hz\nFROM telemetry_metadata(${sqlLiteral(activeFile)})\nWHERE sample_count = 0 ORDER BY name;`],
    ['Gear usage', gear ? `Time distribution for ${gear}` : 'Adapt after selecting a gear channel', gear ? `SELECT ${quoteIdent(gear)} AS gear, count(*) / 100.0 AS seconds\nFROM read_telemetry(${sqlLiteral(activeFile)}, rate := 100, channels := ${sqlLiteral(gear)},\n     start_ns := ${start}, end_ns := ${end})\nGROUP BY 1 ORDER BY 1;` : presets.samples],
    ['Pedal overlap', brake && throttle ? `${brake} × ${throttle}` : 'Adapt after selecting pedal channels', brake && throttle ? `SELECT count(*) / 100.0 AS overlap_seconds\nFROM read_telemetry(${sqlLiteral(activeFile)}, rate := 100,\n     channels := ${sqlLiteral(`${brake},${throttle}`)}, start_ns := ${start}, end_ns := ${end})\nWHERE ${quoteIdent(brake)} > 0 AND ${quoteIdent(throttle)} > 0;` : presets.stats],
    ['Discrete transitions', `Changes in ${gear || first}`, `WITH s AS (\n  SELECT time_ns, value, lag(value) OVER (ORDER BY time_ns) AS previous\n  FROM telemetry_samples(${sqlLiteral(activeFile)}, channel := ${sqlLiteral(gear || first)},\n       start_ns := ${start}, end_ns := ${end})\n) SELECT (time_ns - ${start}) / 1e9 AS lap_seconds, previous, value\nFROM s WHERE value IS DISTINCT FROM previous ORDER BY time_ns;`],
    ['Correlation', 'Relationship between selected signals', candidates.length > 1 ? `SELECT corr(${quoteIdent(candidates[0])}, ${quoteIdent(candidates[1])}) AS correlation\nFROM read_telemetry(${sqlLiteral(activeFile)}, rate := 100,\n     channels := ${sqlLiteral(candidates.slice(0, 2).join(','))}, start_ns := ${start}, end_ns := ${end});` : presets.stats],
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
$('#trace').addEventListener('pointermove', (event) => scrubTrace(event.clientX));
$('#trace').addEventListener('pointerdown', (event) => scrubTrace(event.clientX));
$('#trace').addEventListener('pointerleave', () => $('#scrubber').classList.add('hidden'));
const drop = $('#dropZone');
for (const type of ['dragenter', 'dragover']) drop.addEventListener(type, (event) => { event.preventDefault(); drop.classList.add('dragging'); });
for (const type of ['dragleave', 'drop']) drop.addEventListener(type, (event) => { event.preventDefault(); drop.classList.remove('dragging'); });
drop.addEventListener('drop', (event) => { const file = (event as DragEvent).dataTransfer?.files[0]; if (file) loadFile(file); });
window.addEventListener('resize', () => { drawTrace(); drawTrackMap(); });

init();
