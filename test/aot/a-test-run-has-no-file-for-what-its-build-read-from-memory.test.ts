import { expect } from "bun:test";
import { bunEnv, bunExe, tempDir } from "harness";
import { run } from "./harness/run";
import { test } from "./supported";

// The build of a test run reads entry points of its own from memory, under names in node_modules/.cache: one stands in for what the runner does for `--globals=vitest`,
// one for the window of a test file that asks for a DOM. No disk ever held them, so a test finds no file and no folder of theirs, as under plain `bun test`.
const looks = (runner: string) => `
import { test } from "${runner}";
import { existsSync, mkdirSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
test("the run put nothing into the project", () => {
  const packages = join(import.meta.dir, "node_modules");
  const cache = join(packages, ".cache");
  const kind = path => (statSync(path, { throwIfNoEntry: false })?.isDirectory() ? "a folder" : "nothing");
  console.log("| finds", existsSync(packages), existsSync(cache), kind(cache), readdirSync(import.meta.dir).sort().join());
  // As a tool does that keeps its files there.
  if (!existsSync(cache)) mkdirSync(cache, { recursive: true });
  writeFileSync(join(cache, "written.txt"), "text");
  console.log("| wrote", readdirSync(cache).join());
});
`;

const projects: [title: string, flags: string[], files: Record<string, string>, finds: string][] = [
  [
    "the globals of vitest",
    ["--globals=vitest"],
    { "a.test.js": looks("vitest") },
    "false false nothing a.test.js,package.json",
  ],
  [
    "the window of a DOM",
    [],
    {
      "a.test.js": `/** @vitest-environment happy-dom */` + looks("bun:test"),
      "node_modules/happy-dom/package.json": `{ "name": "happy-dom", "version": "1.0.0", "main": "index.cjs" }`,
      "node_modules/happy-dom/index.cjs": `
        exports.Window = class Window {
          constructor() {
            this.document = {};
            this.window = this;
            this.happyDOM = { abort() {}, cancelAsync() {} };
          }
          close() {}
        };`,
    },
    "true false nothing a.test.js,node_modules,package.json",
  ],
];

for (const [title, flags, files, finds] of projects) {
  test.concurrent(`the entry point that stands in for ${title} is no file`, async () => {
    // A test writes into its project, so each run has a project of its own.
    const printed = async (...mode: string[]) => {
      using directory = tempDir("aot-no-file-for-a-stand-in", {
        "package.json": `{ "name": "p", "type": "module" }`,
        ...files,
      });
      const { stdout, stderr, exitCode } = await run([bunExe(), "test", ...flags, ...mode], String(directory), bunEnv);
      const lines = (stdout + "\n" + stderr).split("\n").map(line => line.trim());
      return { lines: lines.filter(line => /^\| |error: |^\d+ (pass|fail)$/.test(line)), exitCode };
    };
    const plain = await printed();
    expect(plain).toEqual({ lines: [`| finds ${finds}`, "| wrote written.txt", "1 pass", "0 fail"], exitCode: 0 });
    expect(await printed("--aot")).toEqual(plain);
  });
}
