// Run with `bun test --retry 1 --timeout 1`. bun:test's own timeout gives up on both attempts of the first test
// while their bodies are still waiting, and it does not tell node:test. The second test has no timeout.
const { test } = require("node:test");

const log = (...args) => console.log("OBS", ...args);
const realSetTimeout = globalThis.setTimeout;
const target = { method: () => "real" };
const goOn = Promise.withResolvers();
const went = Promise.withResolvers();
let attempts = 0;
let runs = 0;

test("abandoned twice", async t => {
  const n = ++attempts;
  if (n === 1) {
    t.mock.method(target, "method", () => "mocked");
    t.mock.timers.enable({ apis: ["setTimeout"] });
    t.after(() => log("t.after of the first attempt"));
  } else {
    log("second attempt:", `method=${target.method()}`, `setTimeout is real=${globalThis.setTimeout === realSetTimeout}`);
  }
  await goOn.promise;
  if (n === 2) went.resolve();
  throw new Error(`attempt ${n} fails long after bun:test gave up on it`);
});

// Both attempts fail while this test is running. Those failures belong to a test that bun:test gave up on,
// so bun:test must not charge them to this test and run it again.
test("runs once", { timeout: Infinity }, async () => {
  runs++;
  goOn.resolve();
  await went.promise;
  log("runs once:", "attempts of the other test=" + attempts, "runs of this test=" + runs);
});
