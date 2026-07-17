import { chromium } from 'playwright-core';
import { writeFileSync } from 'node:fs';

function pdsFixture(path) {
  const data = Buffer.alloc(0x700);
  const u32 = (at, value) => data.writeUInt32LE(value, at);
  const utf16 = (at, value) => { for (let i = 0; i < value.length; i++) data.writeUInt16LE(value.charCodeAt(i), at + i * 2); };
  const directory = (at, offset, count, classB, next) => { u32(at, offset); u32(at + 8, count); u32(at + 0x10, 8); u32(at + 0x14, classB); u32(at + 0x18, next); };
  directory(0x80, 0x200, 1, 1, 2); directory(0xa0, 0x300, 1, 3, 0); directory(0xc0, 0x380, 0, 1, 0);
  u32(0x200, 7); utf16(0x208, 'Speed'); utf16(0x250, 'm/s');
  u32(0x300, 0); u32(0x304, 7); u32(0x308, 7); u32(0x318, 10_000_000); u32(0x31c, 4); u32(0x338, 0x400);
  [10, 20, 30, 40].forEach((value, index) => data.writeDoubleLE(value, 0x400 + index * 8));
  writeFileSync(path, data);
}
function motecFixture(path) {
  const data = Buffer.alloc(0x400); const u16 = (at, value) => data.writeUInt16LE(value, at); const u32 = (at, value) => data.writeUInt32LE(value, at);
  u32(0, 0x40); u32(8, 0x200); u32(0x208, 0x300); u32(0x20c, 4); u16(0x212, 0x07); u16(0x214, 4); u16(0x216, 2);
  data.write('Speed', 0x220); data.write('m/s', 0x240); [1, 2, 3, 4].forEach((value, index) => data.writeFloatLE(value, 0x300 + index * 4));
  writeFileSync(path, data);
}

const executablePath = process.env.CHROME || (process.platform === 'linux' ? '/usr/bin/google-chrome' : undefined);
const browser = await chromium.launch({ executablePath, headless: true, args: ['--no-sandbox'] });
const page = await browser.newPage(); const errors = [];
page.on('pageerror', error => errors.push(error.message));
const testUrl = new URL(process.env.TEST_URL || 'http://127.0.0.1:4173/duckdb_motorsport_telemetry/');
testUrl.searchParams.set('no-demo', '1');
await page.goto(testUrl.href);
await page.waitForFunction(() => document.querySelector('#runtimeText')?.textContent?.includes('Extension ready'), null, { timeout: 60_000 });

const pds = '/tmp/telemetry-browser-smoke.pds'; pdsFixture(pds);
await page.locator('#fileInput').setInputFiles(pds);
await page.waitForFunction(() => document.querySelector('#fileFormat')?.textContent === 'PDS' && document.querySelectorAll('#channelRows tr').length === 1, null, { timeout: 30_000 });

const motec = '/tmp/telemetry-browser-smoke.ld'; motecFixture(motec);
await page.locator('#fileInput').setInputFiles(motec);
await page.waitForFunction(() => document.querySelector('#fileFormat')?.textContent === 'MOTEC' && document.querySelectorAll('#channelRows tr').length === 1, null, { timeout: 30_000 });

const vbo = '/tmp/telemetry-browser-smoke.vbo';
writeFileSync(vbo, `[header]\ntime\nvelocity kmh\nthrottle\nlap\nlatitude\nlongitude\n[column names]\ntime velocity throttle lap latitude longitude\n[data]\n120000.0 10 0 0 41.0000 2.0000\n120010.0 20 10 0 41.0010 2.0010\n120020.0 30 20 1 41.0020 2.0020\n120030.0 40 30 1 41.0030 2.0020\n120040.0 50 40 1 41.0040 2.0010\n120050.0 60 50 1 41.0040 2.0000\n120100.0 70 60 2 41.0030 1.9990\n120110.0 80 70 2 41.0020 1.9980\n120120.0 90 80 2 41.0010 1.9980\n120130.0 60 70 3 41.0000 1.9990\n120140.0 40 40 3 40.9995 2.0000\n120150.0 20 10 3 41.0000 2.0000\n`);
await page.locator('#fileInput').setInputFiles(vbo);
await page.waitForFunction(() => document.querySelector('#fileFormat')?.textContent === 'VBO' && document.querySelectorAll('#channelRows tr').length === 6, null, { timeout: 30_000 });
await page.waitForFunction(() => document.querySelectorAll('#lapRail button').length === 4 && document.querySelector('#lapRail button.active')?.textContent?.includes('BEST') && document.querySelectorAll('#recipeGrid button').length === 10 && document.querySelector('#recipeGrid button')?.textContent?.includes('Top speed in session') && !document.querySelector('#trackMapPanel')?.classList.contains('hidden') && document.querySelectorAll('[data-example]').length === 1 && document.querySelector('.demo-heading')?.textContent?.includes('AUTO-LOADING REAL DEMO'), null, { timeout: 30_000 });
await page.locator('#trace').hover({ position: { x: 300, y: 100 } });
await page.waitForFunction(() => !document.querySelector('#scrubber')?.classList.contains('hidden'));
await page.locator('#channelRows tr[data-channel="velocity kmh"]').click();
await page.waitForFunction(() => document.querySelectorAll('#sampleList > div').length > 0, null, { timeout: 30_000 });
await page.click('#closeInspector');
await page.click('#runQuery');
await page.waitForFunction(() => document.querySelector('#queryTiming')?.textContent?.includes('6 ROWS'), null, { timeout: 30_000 });
await page.locator('[data-recipe="0"]').click();
await page.click('#runQuery');
await page.waitForFunction(() => document.querySelector('#queryTiming')?.textContent?.includes('1 ROWS') && document.querySelector('#queryResult')?.textContent?.includes('top_speed'), null, { timeout: 30_000 });
if (errors.length) throw new Error(errors.join('\n'));
console.log('DuckDB-Wasm extension parsed synthetic PDS, MoTeC, and VBO files and ran browser SQL');
await browser.close();
