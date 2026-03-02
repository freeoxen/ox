import { describe, test, expect, beforeEach } from "bun:test";
import { get } from "svelte/store";
import { usage, addUsage, resetUsage } from "./usage";

beforeEach(() => {
  resetUsage();
});

describe("usage store", () => {
  test("starts at zero", () => {
    const s = get(usage);
    expect(s.requests).toBe(0);
    expect(s.inputTokens).toBe(0);
    expect(s.outputTokens).toBe(0);
  });

  test("addUsage increments counters", () => {
    addUsage(100, 50);
    const s = get(usage);
    expect(s.requests).toBe(1);
    expect(s.inputTokens).toBe(100);
    expect(s.outputTokens).toBe(50);
  });

  test("addUsage accumulates across calls", () => {
    addUsage(100, 50);
    addUsage(200, 75);
    addUsage(300, 25);
    const s = get(usage);
    expect(s.requests).toBe(3);
    expect(s.inputTokens).toBe(600);
    expect(s.outputTokens).toBe(150);
  });

  test("resetUsage zeros everything", () => {
    addUsage(500, 200);
    resetUsage();
    const s = get(usage);
    expect(s.requests).toBe(0);
    expect(s.inputTokens).toBe(0);
    expect(s.outputTokens).toBe(0);
  });

  test("addUsage with zero tokens still increments request count", () => {
    addUsage(0, 0);
    const s = get(usage);
    expect(s.requests).toBe(1);
    expect(s.inputTokens).toBe(0);
    expect(s.outputTokens).toBe(0);
  });
});
