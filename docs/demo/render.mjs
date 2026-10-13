#!/usr/bin/env node
// Render an o5s demo film (a deterministic HTML timeline) to an mp4.
//
//   node render.mjs                          # -> out/o5s-demo.mp4 (1080p, 24 fps)
//   node render.mjs --scale 2 --out-height 1080  # supersampled 1080p
//   node render.mjs --fps 12 --out-height 720   # faster draft
//   node render.mjs --stills 12,100,185,325 # PNG stills at given seconds
//
// Needs: ffmpeg on PATH, and playwright (any install — a local node_modules,
// a global one, or an ~/.npm/_npx cache) with its chromium headless shell.

import { createRequire } from 'node:module';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, readdirSync, renameSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);

function resolvePlaywright() {
  for (const name of ['playwright', 'playwright-core']) {
    try { return require(name); } catch {}
  }
  const npx = path.join(homedir(), '.npm', '_npx');
  if (existsSync(npx)) {
    const hits = readdirSync(npx)
      .map(d => path.join(npx, d, 'node_modules', 'playwright'))
      .filter(p => existsSync(p))
      .map(p => ({ p, v: require(path.join(p, 'package.json')).version }))
      .sort((a, b) => b.v.localeCompare(a.v, undefined, { numeric: true }));
    if (hits.length) return createRequire(path.join(hits[0].p, 'x.js'))('playwright');
  }
  throw new Error('playwright not found — run: npm i playwright-core');
}

function findHeadlessShell() {
  const root = path.join(homedir(), '.cache', 'ms-playwright');
  if (!existsSync(root)) return null;
  const dirs = readdirSync(root).filter(d => d.startsWith('chromium'))
    .sort((a, b) => (b.match(/\d+$/)?.[0] ?? 0) - (a.match(/\d+$/)?.[0] ?? 0));
  for (const d of dirs) {
    for (const bin of ['headless_shell', 'chrome']) {
      const p = path.join(root, d, 'chrome-linux', bin);
      if (existsSync(p)) return p;
    }
  }
  return null;
}

const args = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = args.indexOf('--' + name);
  return i >= 0 ? args[i + 1] : dflt;
};
const FPS = Number(opt('fps', 24));
const CRF = Number(opt('crf', 16));
// Supersampling: capture the fixed 1920x1080 stage at SCALEx device pixels
// (2 -> 3840x2160) for crisp text, then optionally downscale to OUT_HEIGHT.
const SCALE = Number(opt('scale', 1));
const WIDTH = Number(opt('width', 1920));
const HEIGHT = Math.round(WIDTH * 9 / 16);
const FAST = args.includes('--fast');
// Final video height. Unset keeps the native capture (SCALE=2 -> true 4K);
// pass e.g. --out-height 1080 for supersampled, very crisp 1080p.
const OUT_HEIGHT = Number(opt('out-height', 0)) || 0;
const stills = opt('stills', null);
const PAGE = opt('page', 'index.html');
const BASE = path.basename(PAGE, '.html');
const AUDIO = path.join(here, 'audio', 'narration.mp3');

const outDir = path.join(here, 'out');
mkdirSync(outDir, { recursive: true });

const { chromium } = resolvePlaywright();
const executablePath = findHeadlessShell();
const browser = await chromium.launch({
  executablePath: executablePath ?? undefined,
  // No `--force-device-scale-factor`: the context `deviceScaleFactor` below
  // drives supersampling, and forcing 1 here would override it back to 1x.
  args: ['--hide-scrollbars', '--font-render-hinting=none'],
});
const page = await browser.newPage({
  viewport: { width: WIDTH, height: HEIGHT },
  deviceScaleFactor: SCALE,
});
await page.goto('file://' + path.join(here, PAGE) + '?render=1');
await page.evaluate(() => document.fonts.ready);
const TOTAL = await page.evaluate(() => window.TOTAL);

async function frame(ms, type, quality) {
  await page.evaluate(t => window.SEEK(t), ms);
  return page.screenshot({ type, ...(quality ? { quality } : {}) });
}

if (stills) {
  for (const s of stills.split(',')) {
    const buf = await frame(Number(s) * 1000, 'png');
    const f = path.join(outDir, `still-${BASE}-${Number(s).toFixed(1)}s.png`);
    writeFileSync(f, buf);
    console.log('wrote', f);
  }
  await browser.close();
  process.exit(0);
}

const nFrames = Math.ceil((TOTAL / 1000) * FPS);
const outFile = path.join(outDir, BASE === 'index' ? 'o5s-demo.mp4' : `o5s-${BASE}.mp4`);
const renderingFile = outFile.replace(/\.mp4$/, '.rendering.mp4');
const capH = HEIGHT * SCALE;
const outH = OUT_HEIGHT || capH;
console.log(
  `rendering ${nFrames} frames @ ${FPS}fps (${(TOTAL / 1000).toFixed(0)}s), ` +
  `capture ${WIDTH * SCALE}x${capH} -> output ${Math.round((WIDTH * SCALE) * (outH / capH))}x${outH} -> ${outFile}`,
);

// Downscale with lanczos only when the output height is below the capture
// height (supersampling → crisp). No filter when shipping the native capture.
const vf = OUT_HEIGHT && OUT_HEIGHT < capH ? ['-vf', `scale=-2:${OUT_HEIGHT}:flags=lanczos`] : [];
const withAudio = existsSync(AUDIO) && !args.includes('--silent');
const ff = spawn('ffmpeg', [
  '-y', '-f', 'image2pipe', '-framerate', String(FPS), '-i', '-',
  ...(withAudio ? ['-i', AUDIO] : []),
  '-map', '0:v:0', ...(withAudio ? ['-map', '1:a:0'] : []),
  '-c:v', 'libx264', '-preset', FAST ? 'veryfast' : 'slow', '-crf', String(CRF),
  ...vf,
  ...(withAudio ? ['-c:a', 'aac', '-b:a', '160k'] : []),
  '-pix_fmt', 'yuv420p', '-movflags', '+faststart', renderingFile,
], { stdio: ['pipe', 'inherit', 'inherit'] });
const ffDone = new Promise((res, rej) =>
  ff.on('close', c => (c === 0 ? res() : rej(new Error('ffmpeg exit ' + c)))));

const t0 = Date.now();
for (let i = 0; i < nFrames; i++) {
  // Lossless PNG frames: JPEG (even q95) fuzzes text edges with block/ringing
  // artifacts that H.264 then bakes in. PNG lets the encoder be the only
  // lossy stage — a big sharpness win for this terminal/text-heavy film.
  const buf = await frame((i / FPS) * 1000, FAST ? 'jpeg' : 'png', FAST ? 90 : undefined);
  if (!ff.stdin.write(buf)) await new Promise(r => ff.stdin.once('drain', r));
  if (i % 150 === 0) {
    const rate = (i + 1) / ((Date.now() - t0) / 1000);
    process.stdout.write(`\r${i}/${nFrames} frames  (${rate.toFixed(1)} fps, eta ${Math.round((nFrames - i) / rate)}s)   `);
  }
}
ff.stdin.end();
await ffDone;
await browser.close();
renameSync(renderingFile, outFile);
console.log(`\ndone -> ${outFile}`);
