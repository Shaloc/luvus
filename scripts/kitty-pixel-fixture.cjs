// Optional real terminal-browser PixelEngine exercise, in a private test PTY.
// No Electron/browser daemon, user tabs, network, or production Luvus access.
const fs = require('node:fs');
const path = require('node:path');
const root = fs.realpathSync(process.env.LUVUS_SMOKE_ROOT);
const repo = path.dirname(__dirname);
if (path.dirname(root) !== path.join(repo, 'target') ||
    fs.readFileSync(path.join(root, '.isolated-smoke'), 'utf8') !== root) {
  throw new Error('not an isolated smoke home');
}
const binding = require(process.env.LUVUS_TEST_PIXEL_BINDING);
const engine = new binding.PixelEngine(undefined, undefined, {...process.env, TERM: 'xterm-kitty'});
const events = [];
engine.start((error, json) => {
  events.push(error ? String(error) : JSON.parse(json));
});
// Opt-in transport benchmark only. This drives the installed native engine,
// not Chromium, and must not be reported as browser/display FPS.
if (process.argv[2] === '--perf') {
  const fps = Number(process.argv[3]);
  const label = process.argv[4];
  if (![30, 60].includes(fps) || !/^(local|remote)-(30|60)$/.test(label)) {
    throw new Error('invalid isolated benchmark');
  }
  const submitted = [];
  const props = (id) => ({style: {
    width: '100%', height: '100%', background: [id, 64, 128, 255],
  }});
  // Allow terminal negotiation and initial layout to settle before measuring.
  setTimeout(() => {
    engine.applyOps(JSON.stringify({view: 0, seq: 1, ops: [
      {op: 'create', id: 1, props: props(0)},
      {op: 'insertBefore', parent: 0, child: 1, before: null},
    ]}));
    setTimeout(() => {
      let id = 0;
      const started = performance.now();
      const tick = () => {
        ++id;
        submitted.push({id, at: Number(process.hrtime.bigint()) / 1e9});
        engine.applyOps(JSON.stringify({view: 0, seq: id + 1,
          ops: [{op: 'update', id: 1, props: props(id)}]}));
        if (id < fps * 3) {
          setTimeout(tick, Math.max(0, started + id * 1000 / fps - performance.now()));
        } else {
          setTimeout(() => {
            engine.stop();
            fs.writeFileSync(path.join(root, `graphics-perf-${label}.json`),
              JSON.stringify({fps, submitted, events}));
            process.exit(0);
          }, 1000);
        }
      };
      tick();
    }, 500);
  }, 1500);
} else {
  engine.applyOps(JSON.stringify({view: 0, seq: 1, ops: [
    {op: 'create', id: 1, props: {style: {width: '100%', height: '100%', background: [255, 0, 0, 255]}}},
    {op: 'insertBefore', parent: 0, child: 1, before: null},
  ]}));
  // Exercise the same subsequent render flush used by the React frontend;
  // observing the initial empty engine canvas is not a red-image assertion.
  const flush = setInterval(() => {
    engine.applyOps(JSON.stringify({view: 0, ops: []}));
  }, 50);
  setTimeout(() => {
    clearInterval(flush);
    engine.stop();
    fs.writeFileSync(path.join(root, 'graphics-pixel.json'), JSON.stringify(events));
    process.exit(0);
  }, 4000);
}
