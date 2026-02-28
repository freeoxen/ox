import { describe, expect, test } from "bun:test";
import { makeEmpty, makeSection, makeKV } from "./dom-helpers";

describe("makeEmpty", () => {
  test("creates div with empty class and text", () => {
    const el = makeEmpty("nothing here");
    expect(el.tagName).toBe("DIV");
    expect(el.className).toBe("empty");
    expect(el.textContent).toBe("nothing here");
  });
});

describe("makeSection", () => {
  test("creates details with summary and body", () => {
    const el = makeSection("test section", () => {
      const div = document.createElement("div");
      div.textContent = "body";
      return div;
    });
    expect(el.tagName).toBe("DETAILS");
    expect(el.open).toBe(false);
    const summary = el.querySelector("summary");
    expect(summary?.textContent).toBe("test section");
    expect(el.children.length).toBe(2); // summary + body
  });

  test("starts open when requested", () => {
    const el = makeSection("open", () => document.createElement("div"), true);
    expect(el.open).toBe(true);
  });

  test("starts closed by default", () => {
    const el = makeSection("closed", () => document.createElement("div"));
    expect(el.open).toBe(false);
  });
});

describe("makeKV", () => {
  test("creates kv row with string styling", () => {
    const el = makeKV("key", "value", "str");
    expect(el.className).toBe("kv");
    const k = el.querySelector(".k");
    const v = el.querySelector(".v");
    expect(k?.textContent).toBe("key");
    expect(v?.textContent).toBe("value");
    expect(v?.classList.contains("str-val")).toBe(true);
  });

  test("creates kv row with number styling", () => {
    const el = makeKV("count", 42, "num");
    const v = el.querySelector(".v");
    expect(v?.textContent).toBe("42");
    expect(v?.classList.contains("num-val")).toBe(true);
  });

  test("creates kv row with no type styling", () => {
    const el = makeKV("x", "y", "");
    const v = el.querySelector(".v");
    expect(v?.classList.contains("str-val")).toBe(false);
    expect(v?.classList.contains("num-val")).toBe(false);
  });

  test('renders null value as "null"', () => {
    const el = makeKV("key", null, "str");
    const v = el.querySelector(".v");
    expect(v?.textContent).toBe("null");
  });

  test('renders undefined value as "null"', () => {
    const el = makeKV("key", undefined, "");
    const v = el.querySelector(".v");
    expect(v?.textContent).toBe("null");
  });
});
