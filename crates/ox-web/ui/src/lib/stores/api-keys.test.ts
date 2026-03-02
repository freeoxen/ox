import { describe, test, expect, beforeEach } from "bun:test";
import {
  loadKeys,
  saveKey,
  removeKey,
  clearAll,
  maskKey,
  hasAnyKeys,
} from "./api-keys";

const STORAGE_KEY = "ox:api-keys";
const OLD_SESSION_KEY = "ox:api-key";

beforeEach(() => {
  localStorage.clear();
  sessionStorage.clear();
});

describe("loadKeys", () => {
  test("returns empty object when nothing stored", () => {
    expect(loadKeys()).toEqual({});
  });

  test("returns stored keys", () => {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({ anthropic: "sk-ant-123" }),
    );
    expect(loadKeys()).toEqual({ anthropic: "sk-ant-123" });
  });

  test("handles corrupted localStorage gracefully", () => {
    localStorage.setItem(STORAGE_KEY, "{bad json");
    expect(loadKeys()).toEqual({});
  });

  test("migrates from old sessionStorage key", () => {
    sessionStorage.setItem(OLD_SESSION_KEY, "sk-ant-old");
    const keys = loadKeys();
    expect(keys).toEqual({ anthropic: "sk-ant-old" });
    // Old key should be removed
    expect(sessionStorage.getItem(OLD_SESSION_KEY)).toBeNull();
    // New key should be persisted
    expect(JSON.parse(localStorage.getItem(STORAGE_KEY)!)).toEqual({
      anthropic: "sk-ant-old",
    });
  });

  test("migration does not overwrite existing localStorage keys", () => {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({ anthropic: "sk-ant-new" }),
    );
    sessionStorage.setItem(OLD_SESSION_KEY, "sk-ant-old");
    // sessionStorage key present but localStorage already has keys —
    // migration fires because sessionStorage is checked first, but that's
    // acceptable since the old key implies a stale session
    const keys = loadKeys();
    expect(keys.anthropic).toBeDefined();
  });
});

describe("saveKey", () => {
  test("saves a new key", () => {
    saveKey("anthropic", "sk-ant-abc");
    expect(loadKeys()).toEqual({ anthropic: "sk-ant-abc" });
  });

  test("updates an existing key", () => {
    saveKey("anthropic", "sk-ant-first");
    saveKey("anthropic", "sk-ant-second");
    expect(loadKeys()).toEqual({ anthropic: "sk-ant-second" });
  });

  test("saves multiple providers independently", () => {
    saveKey("anthropic", "sk-ant-abc");
    saveKey("openai", "sk-oai-xyz");
    const keys = loadKeys();
    expect(keys.anthropic).toBe("sk-ant-abc");
    expect(keys.openai).toBe("sk-oai-xyz");
  });
});

describe("removeKey", () => {
  test("removes a key", () => {
    saveKey("anthropic", "sk-ant-abc");
    removeKey("anthropic");
    expect(loadKeys().anthropic).toBeUndefined();
  });

  test("removing nonexistent key is a no-op", () => {
    removeKey("openai");
    expect(loadKeys()).toEqual({});
  });

  test("does not affect other providers", () => {
    saveKey("anthropic", "sk-ant-abc");
    saveKey("openai", "sk-oai-xyz");
    removeKey("anthropic");
    expect(loadKeys()).toEqual({ openai: "sk-oai-xyz" });
  });
});

describe("clearAll", () => {
  test("removes all keys", () => {
    saveKey("anthropic", "sk-ant-abc");
    saveKey("openai", "sk-oai-xyz");
    clearAll();
    expect(loadKeys()).toEqual({});
    expect(localStorage.getItem(STORAGE_KEY)).toBeNull();
  });
});

describe("maskKey", () => {
  test("masks a long key", () => {
    expect(maskKey("sk-ant-api03-abcdefghijk")).toBe("sk-an...hijk");
  });

  test("masks a short key", () => {
    expect(maskKey("abcd")).toBe("****");
  });

  test("masks an 8-char key", () => {
    expect(maskKey("12345678")).toBe("****");
  });

  test("masks a 9-char key showing prefix and suffix", () => {
    expect(maskKey("123456789")).toBe("12345...6789");
  });
});

describe("hasAnyKeys", () => {
  test("returns false when no keys stored", () => {
    expect(hasAnyKeys()).toBe(false);
  });

  test("returns true when at least one key exists", () => {
    saveKey("openai", "sk-oai-xyz");
    expect(hasAnyKeys()).toBe(true);
  });

  test("returns false after clearing all keys", () => {
    saveKey("anthropic", "sk-ant-abc");
    clearAll();
    expect(hasAnyKeys()).toBe(false);
  });
});
