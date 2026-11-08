// @ts-nocheck
import { describe, expect, test } from "vitest";

// Vitest's module object reads an export that is not set yet as undefined. A module namespace object throws for it.
const namespace = await import("./unset-export.mjs").catch(error => error.namespace);
const names = await import("./unset-names.mjs").catch(error => error.namespace);

describe("a module namespace with an export that is not set yet", () => {
  test("in a snapshot file", () => {
    expect(namespace).toMatchSnapshot();
    expect({ namespace }).toMatchSnapshot();
    expect([namespace, new Map([["key", namespace]])]).toMatchSnapshot();
  });
  test("in an inline snapshot", () => {
    expect({ namespace }).toMatchInlineSnapshot(`
      {
        "namespace": {
          "hoisted": [Function],
          "later": undefined,
        },
      }
    `);
  });
  test("with property matchers", () => {
    expect({ namespace, id: 1 }).toMatchSnapshot({ id: expect.any(Number) });
    expect(namespace).toMatchSnapshot({ hoisted: expect.any(Function) });
  });
  test("thrown", () => {
    expect(() => {
      throw { namespace };
    }).toThrowErrorMatchingSnapshot();
  });
  test("under the names that the printer reads", () => {
    expect(names).toMatchSnapshot();
  });
  // Before #44833, Bun printed such a namespace in a title for %s and for a key. It threw for %o and for the key of the export.
  test.each([[namespace]])("in a title for a string: %s", value => {
    expect(value.hoisted).toMatchSnapshot();
  });
  test.each([{ namespace }])("in a title for a key: $namespace", ({ namespace: value }) => {
    expect(value.hoisted).toMatchSnapshot();
  });
  test.each([[namespace]])("in a title for an object: %o", value => {
    expect(value.hoisted).toMatchSnapshot();
  });
  test.each([{ namespace }])("its export in a title: $namespace.later", ({ namespace: value }) => {
    expect(value.hoisted).toMatchSnapshot();
  });
});
