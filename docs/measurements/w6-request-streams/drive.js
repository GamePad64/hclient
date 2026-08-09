// Minimal WebDriver client: start a driver, open a session that accepts the
// self-signed cert, navigate, wait for the page to report.
const { spawn } = require('node:child_process');
const fs = require('node:fs');

const which = process.argv[2]; // chrome | firefox
const url = process.argv[3];
const label = process.argv[4];

const CHROMEDRIVER =
  process.env.HOME + '/.cache/.wasm-pack/chromedriver-d65213741bcf1a26/chromedriver';
const GECKODRIVER =
  process.env.HOME + '/.cache/.wasm-pack/geckodriver-df9057ce9c9cc43e/geckodriver';

const port = which === 'chrome' ? 9515 : 9516;
const bin = which === 'chrome' ? CHROMEDRIVER : GECKODRIVER;
const args = which === 'chrome' ? ['--port=' + port] : ['--port', String(port)];

const caps =
  which === 'chrome'
    ? {
        capabilities: {
          alwaysMatch: {
            acceptInsecureCerts: true,
            'goog:chromeOptions': {
              args: ['--headless=new', '--no-sandbox', '--disable-dev-shm-usage', '--disable-gpu'],
            },
          },
        },
      }
    : {
        capabilities: {
          alwaysMatch: {
            acceptInsecureCerts: true,
            'moz:firefoxOptions': { args: ['-headless'] },
          },
        },
      };

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

(async () => {
  const drv = spawn(bin, args, { stdio: ['ignore', 'pipe', 'pipe'] });
  drv.stdout.on('data', () => {});
  drv.stderr.on('data', () => {});
  let up = false;
  for (let i = 0; i < 60; i++) {
    try {
      const r = await fetch(`http://127.0.0.1:${port}/status`);
      if (r.ok) {
        up = true;
        break;
      }
    } catch {}
    await sleep(250);
  }
  if (!up) {
    console.error('driver did not come up');
    drv.kill();
    process.exit(1);
  }
  const mk = await fetch(`http://127.0.0.1:${port}/session`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(caps),
  });
  const mkj = await mk.json();
  const sid = mkj.value && mkj.value.sessionId;
  if (!sid) {
    console.error('no session: ' + JSON.stringify(mkj).slice(0, 800));
    drv.kill();
    process.exit(1);
  }
  try {
    await fetch(`http://127.0.0.1:${port}/session/${sid}/url`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ url }),
    });
    const target = __dirname + '/report-' + label + '.json';
    for (let i = 0; i < 120; i++) {
      if (fs.existsSync(target)) break;
      await sleep(500);
    }
    if (!fs.existsSync(target)) {
      const src = await fetch(`http://127.0.0.1:${port}/session/${sid}/source`);
      console.error('no report; page source: ' + JSON.stringify(await src.json()).slice(0, 2000));
      process.exitCode = 1;
    } else {
      console.log('report written: ' + target);
    }
  } finally {
    await fetch(`http://127.0.0.1:${port}/session/${sid}`, { method: 'DELETE' }).catch(() => {});
    drv.kill();
  }
})();
