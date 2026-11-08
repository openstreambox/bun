// docs/compiler/compatibility.mdx, "Loader hooks and the module cache": a compiled program asks Module._resolveFilename and Module.prototype.require
// only for a path that it computes, and the build points at the store that replaces one with TS100080. The store is found through the module object:
// behind a variable, a choice of objects, a parameter of the function that is handed the object, Object.assign() and Reflect.set().
//
// A line of the program that ends with `// stores <hook>` is such a store. The build has to point at each of them, and at nothing else.
import { expect } from "bun:test";
import { bunEnv, bunExe, tempDir } from "harness";
import { join } from "node:path";
import { compileCommand, withThreads } from "./harness/flags";
import { run } from "./harness/run";
import { test } from "./supported";

const files: Record<string, string> = {
  "main.cjs": `const calls = [];
const wrap = original =>
  function (request, ...rest) {
    calls.push(String(request));
    return original.call(this, request, ...rest);
  };
const Module = require("node:module");

// Written out.
Module._resolveFilename = wrap(Module._resolveFilename); // stores Module._resolveFilename
require("node:module").prototype.require = wrap(Module.prototype.require); // stores Module.prototype.require

// Through a parameter.
function install(Mod) {
  Mod._resolveFilename = wrap(Mod._resolveFilename); // stores Module._resolveFilename
}
install(Module);

// Through a parameter of a method.
const helper = {
  install(Mod) {
    Mod.prototype.require = wrap(Mod.prototype.require); // stores Module.prototype.require
  },
};
helper.install(require("node:module"));

// Through a parameter of a function in another file, which hands the prototype on.
require("./install.cjs")(Module, wrap);

// By a call that copies or sets.
Object.assign(Module, { _resolveFilename: wrap(Module._resolveFilename) }); // stores Module._resolveFilename
Reflect.set(Module.prototype, "require", wrap(Module.prototype.require)); // stores Module.prototype.require

// Through a choice of two objects, as the package module-alias writes it.
const Chosen = module.constructor.length > 1 ? module.constructor : Module;
Chosen._resolveFilename = wrap(Chosen._resolveFilename); // stores Module._resolveFilename

// Through a variable that is assigned later.
let assignedLater;
assignedLater = Module;
assignedLater._resolveFilename = wrap(assignedLater._resolveFilename); // stores Module._resolveFilename

// Inside a pattern.
[
  Module._resolveFilename, // stores Module._resolveFilename
] = [wrap(Module._resolveFilename)];

// No store of a hook: an object of the program, a function that nothing calls, a callback that nothing calls.
const own = { name: "own" };
own._resolveFilename = wrap;
own.require = wrap;
function never(Mod) {
  Mod._resolveFilename = wrap(Mod._resolveFilename);
}
function makeCallback(Mod) {
  return () => {
    Mod.prototype.require = wrap(Mod.prototype.require);
  };
}
makeCallback(Module);

calls.length = 0;
const a = require("./lib/a.cjs");
console.log(a.mark, own.name, typeof never, calls.length);
`,
  "install.cjs": `module.exports = function install(Mod, wrap) {
  Mod._resolveFilename = wrap(Mod._resolveFilename); // stores Module._resolveFilename
  patch(Mod.prototype, wrap);
};
function patch(prototype, wrap) {
  prototype.require = wrap(prototype.require); // stores Module.prototype.require
}
`,
  "lib/a.cjs": `exports.mark = "a";\n`,
};

const stores = Object.entries(files).flatMap(([name, text]) =>
  text.split("\n").flatMap((line, index) => {
    const hook = /\/\/ stores (\S+)$/.exec(line)?.[1];
    return hook ? [`${name}:${index + 1}:${line.search(/\S/) + 1} ${hook}`] : [];
  }),
);

test("TS100080 stands at each store that replaces a loader hook", async () => {
  using dir = tempDir("aot-replaced-loader-hook", files);
  const cwd = String(dir);
  const executable = join(cwd, "main");
  const env = { ...bunEnv, NO_COLOR: "1" };

  // Under plain Bun each store is real: every hook that was stored is asked about the one path.
  const plain = await run([bunExe(), "main.cjs"], cwd, env);
  expect({ stdout: plain.stdout, exitCode: plain.exitCode }).toEqual({
    stdout: `a own function ${stores.length}\n`,
    exitCode: 0,
  });

  // The message is the front end's: one thread of the engine is enough.
  const command = [bunExe(), ...compileCommand, withThreads(1), "./main.cjs", "--outfile", executable];
  const built = await run(command, cwd, env);
  const said = built.stdout + built.stderr;
  const message = /TS100080: A compiled program calls (\S+) only [^\n]*\n\s+at \S*?([^\s\\/]+:\d+:\d+)/g;
  expect({
    pointedAt: Array.from(said.matchAll(message), ([, hook, place]) => `${place} ${hook}`).sort(),
    exitCode: built.exitCode,
  }).toEqual({ pointedAt: stores.toSorted(), exitCode: 0 });

  // The path is written out, so the executable asks none of the hooks.
  const compiled = await run([executable], cwd, env);
  expect({ stdout: compiled.stdout, exitCode: compiled.exitCode }).toEqual({
    stdout: "a own function 0\n",
    exitCode: 0,
  });
});
