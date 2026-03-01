import { describe, expect, test, beforeEach } from "bun:test";
import { ToolStore, BUILTIN_TOOLS, FACTORY_PROFILE } from "./tools";
import type { ToolDef } from "../types";

// localStorage is available in bun's test environment

beforeEach(() => {
  localStorage.clear();
});

describe("ToolStore.isFactory", () => {
  test("returns true for factory profile", () => {
    expect(ToolStore.isFactory(FACTORY_PROFILE)).toBe(true);
  });

  test("returns false for other names", () => {
    expect(ToolStore.isFactory("custom")).toBe(false);
  });
});

describe("ToolStore.isBuiltin", () => {
  test("returns true for builtin tool names", () => {
    expect(ToolStore.isBuiltin("reverse_text")).toBe(true);
  });

  test("returns false for non-builtin names", () => {
    expect(ToolStore.isBuiltin("my_tool")).toBe(false);
  });
});

describe("ToolStore library", () => {
  test("loadLibrary returns builtins by default", () => {
    const lib = ToolStore.loadLibrary();
    expect(lib.reverse_text).toBeDefined();
    expect(lib.reverse_text.name).toBe("reverse_text");
  });

  test("saveTool persists and loadLibrary returns it", () => {
    const def: ToolDef = {
      name: "my_tool",
      description: "test",
      params: "(x: string)",
      body: "return x;",
    };
    ToolStore.saveTool(def);
    const lib = ToolStore.loadLibrary();
    expect(lib.my_tool).toEqual(def);
    // Builtins still present
    expect(lib.reverse_text).toBeDefined();
  });

  test("deleteTool removes user tool", () => {
    const def: ToolDef = {
      name: "temp",
      description: "test",
      params: "()",
      body: 'return "ok";',
    };
    ToolStore.saveTool(def);
    expect(ToolStore.loadLibrary().temp).toBeDefined();
    ToolStore.deleteTool("temp");
    expect(ToolStore.loadLibrary().temp).toBeUndefined();
  });

  test("deleteTool refuses to delete builtins", () => {
    ToolStore.deleteTool("reverse_text");
    expect(ToolStore.loadLibrary().reverse_text).toBeDefined();
  });

  test("deleteTool removes tool from profiles", () => {
    const def: ToolDef = {
      name: "temp",
      description: "test",
      params: "()",
      body: 'return "ok";',
    };
    ToolStore.saveTool(def);
    ToolStore.createProfile("p1");
    ToolStore.setProfileTools("p1", ["temp", "other"]);
    ToolStore.deleteTool("temp");
    expect(ToolStore.getProfileTools("p1")).toEqual(["other"]);
  });

  test("loadLibrary handles corrupted localStorage gracefully", () => {
    localStorage.setItem("ox:tool-library", "{bad json");
    const lib = ToolStore.loadLibrary();
    // Should still have builtins
    expect(lib.reverse_text).toBeDefined();
  });
});

describe("ToolStore profiles", () => {
  test("profileNames includes factory and default", () => {
    const names = ToolStore.profileNames();
    expect(names[0]).toBe(FACTORY_PROFILE);
    expect(names).toContain("default");
  });

  test("getActiveProfile returns factory default initially", () => {
    expect(ToolStore.getActiveProfile()).toBe(FACTORY_PROFILE);
  });

  test("setActiveProfile and getActiveProfile round-trip", () => {
    ToolStore.createProfile("test");
    ToolStore.setActiveProfile("test");
    expect(ToolStore.getActiveProfile()).toBe("test");
  });

  test("getProfileTools returns builtin names for factory", () => {
    const tools = ToolStore.getProfileTools(FACTORY_PROFILE);
    expect(tools).toEqual(BUILTIN_TOOLS.map((t) => t.name));
  });

  test("getProfileTools returns empty for new profile", () => {
    ToolStore.createProfile("empty");
    expect(ToolStore.getProfileTools("empty")).toEqual([]);
  });

  test("setProfileTools persists", () => {
    ToolStore.createProfile("p1");
    ToolStore.setProfileTools("p1", ["a", "b"]);
    expect(ToolStore.getProfileTools("p1")).toEqual(["a", "b"]);
  });

  test("setProfileTools is no-op for factory", () => {
    ToolStore.setProfileTools(FACTORY_PROFILE, ["x"]);
    // factory tools are always the builtins
    expect(ToolStore.getProfileTools(FACTORY_PROFILE)).toEqual(
      BUILTIN_TOOLS.map((t) => t.name),
    );
  });

  test("createProfile is no-op for factory name", () => {
    const before = ToolStore.profileNames().length;
    ToolStore.createProfile(FACTORY_PROFILE);
    expect(ToolStore.profileNames().length).toBe(before);
  });

  test("createProfile does not clobber existing profile", () => {
    ToolStore.createProfile("p1");
    ToolStore.setProfileTools("p1", ["a"]);
    ToolStore.createProfile("p1");
    expect(ToolStore.getProfileTools("p1")).toEqual(["a"]);
  });

  test("deleteProfile removes profile and falls back", () => {
    ToolStore.createProfile("temp");
    ToolStore.setActiveProfile("temp");
    ToolStore.deleteProfile("temp");
    expect(ToolStore.profileNames()).not.toContain("temp");
    // Active should have fallen back
    expect(ToolStore.getActiveProfile()).not.toBe("temp");
  });

  test("deleteProfile is no-op for factory", () => {
    ToolStore.deleteProfile(FACTORY_PROFILE);
    expect(ToolStore.profileNames()).toContain(FACTORY_PROFILE);
  });

  test("handles corrupted profiles localStorage gracefully", () => {
    localStorage.setItem("ox:profiles", "not json");
    // Should return defaults without throwing
    expect(ToolStore.getActiveProfile()).toBe(FACTORY_PROFILE);
    expect(ToolStore.profileNames()).toContain(FACTORY_PROFILE);
  });
});
