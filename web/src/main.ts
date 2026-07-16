import * as duckdb from '@duckdb/duckdb-wasm';
import duckdbMvp from '@duckdb/duckdb-wasm/dist/duckdb-mvp.wasm?url';
import duckdbEh from '@duckdb/duckdb-wasm/dist/duckdb-eh.wasm?url';
import mvpWorker from '@duckdb/duckdb-wasm/dist/duckdb-browser-mvp.worker.js?url';
import ehWorker from '@duckdb/duckdb-wasm/dist/duckdb-browser-eh.worker.js?url';
import './style.css';

type Row = Record<string, unknown>;
type QueryResult = { columns: string[]; rows: Row[]; elapsed: number; total: number };

const BASE = '/duckdb_motorsport_telemetry/';
const EXT_REPO = `${location.origin}${BASE.replace(/\/$/, '')}`;
const bundles: duckdb.DuckDBBundles = {
  mvp: { mainModule: duckdbMvp, mainWorker: mvpWorker },
  eh: { mainModule: duckdbEh, mainWorker: ehWorker },
};

let db: duckdb.AsyncDuckDB;
let conn: duckdb.AsyncDuckDBConnection;
let activeFile = '';
let activeDisplayName = '';
let metadata: Row[] = [];
let plotRows: Row[] = [];
let plotChannels: string[] = [];

const app = document.querySelector<HTMLDivElement>('#app')!;
app.innerHTML = `
  <header class="topbar">
    <a class="brand" href="${BASE}"><span class="mark">T<span>L</span></span><span>TELEMETRY <b>LAB</b></span></a>
    <div class="runtime"><i id="statusDot"></i><span id="runtimeText">Starting DuckDB-Wasm</span></div>
    <a class="github" href="https://github.com/tobi/duckdb_motorsport_telemetry">SOURCE ↗</a>
  </header>
  <main>
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
          <div class="panel-title"><div><span class="pulse"></span> QUICK TRACE</div><div id="traceLegend" class="legend"></div></div>
          <canvas id="trace" height="260"></canvas>
          <div class="axis-label">SESSION TIME →</div>
        </article>
        <article class="panel findings-panel">
          <div class="panel-title">SIGNAL FINDINGS <span>EXACT SAMPLES</span></div>
          <div id="findings"></div>
        </article>
      </div>

      <div class="section-head"><div><span>02</span><h2>Channel map</h2></div><p>Source-exact names, units, and clocks</p></div>
      <div class="panel channel-panel">
        <div class="channel-tools"><input id="channelSearch" placeholder="Filter channels…" /><span id="channelCount"></span></div>
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
    </section>
  </main>
  <footer><span>DUCKDB-WASM × RUST</span><span>NO SERVER · NO TRACKING · NO UPLOAD</span></footer>
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

  plotRows = [];
  if (plotChannels.length && duration > 0) {
    const rate = Math.min(20, Math.max(1, Math.ceil(10_000 / Math.max(duration / 1e9, 1))));
    plotRows = arrowRows(await conn.query(`SELECT * FROM read_telemetry(${sqlLiteral(activeFile)}, rate := ${rate}, channels := ${sqlLiteral(plotChannels.join(','))})`));
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

function renderChannels(rows: Row[]) {
  $('#channelCount').textContent = `${rows.filter((r) => Number(r.sample_count) > 0).length} RECORDED / ${rows.length} DEFINED`;
  $('#channelRows').innerHTML = rows.map((row) => `<tr class="${Number(row.sample_count) ? '' : 'empty'}"><td><b>${escapeHtml(String(row.name))}</b></td><td>${escapeHtml(String(row.unit || '—'))}</td><td>${row.data_type}</td><td>${formatNumber(row.frequency_hz, 2)} Hz</td><td>${formatNumber(row.sample_count, 0)}</td><td>${formatDuration(row.duration_ns)}</td></tr>`).join('');
}
function escapeHtml(value: string) { const div = document.createElement('div'); div.textContent = value; return div.innerHTML; }

function setPresets() {
  const candidates = candidateChannels(metadata);
  const first = candidates[0] || String(metadata.find((r) => Number(r.sample_count))?.name || '');
  const presets: Record<string, string> = {
    metadata: `SELECT name, unit, frequency_hz, sample_count\nFROM telemetry_metadata(${sqlLiteral(activeFile)})\nWHERE sample_count > 0\nORDER BY name;`,
    stats: `SELECT channel, any_value(unit) AS unit,\n       count(*) AS samples, min(value), avg(value), max(value)\nFROM telemetry_samples(${sqlLiteral(activeFile)},\n     channel := ${sqlLiteral(candidates.join(','))})\nGROUP BY channel\nORDER BY channel;`,
    samples: `SELECT time_ns / 1e9 AS seconds, value\nFROM telemetry_samples(${sqlLiteral(activeFile)},\n     channel := ${sqlLiteral(first)},\n     start_ns := 0, end_ns := 10000000000)\nORDER BY time_ns\nLIMIT 500;`,
  };
  editor.value = presets.metadata; updateLines();
  document.querySelectorAll<HTMLButtonElement>('[data-query]').forEach((button) => button.onclick = () => { editor.value = presets[button.dataset.query!]; updateLines(); });
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

input.addEventListener('change', () => input.files?.[0] && loadFile(input.files[0]));
$('#replaceFile').addEventListener('click', () => input.click());
$('#runQuery').addEventListener('click', runQuery);
editor.addEventListener('input', updateLines);
editor.addEventListener('keydown', (event) => { if ((event.metaKey || event.ctrlKey) && event.key === 'Enter') { event.preventDefault(); runQuery(); } });
$('#channelSearch').addEventListener('input', (event) => { const term = (event.target as HTMLInputElement).value.toLowerCase(); renderChannels(metadata.filter((row) => String(row.name).toLowerCase().includes(term) || String(row.unit).toLowerCase().includes(term))); });
const drop = $('#dropZone');
for (const type of ['dragenter', 'dragover']) drop.addEventListener(type, (event) => { event.preventDefault(); drop.classList.add('dragging'); });
for (const type of ['dragleave', 'drop']) drop.addEventListener(type, (event) => { event.preventDefault(); drop.classList.remove('dragging'); });
drop.addEventListener('drop', (event) => { const file = (event as DragEvent).dataTransfer?.files[0]; if (file) loadFile(file); });
window.addEventListener('resize', drawTrace);

init();
