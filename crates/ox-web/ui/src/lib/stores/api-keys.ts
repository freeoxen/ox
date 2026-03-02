const STORAGE_KEY = "ox:api-keys";
const OLD_SESSION_KEY = "ox:api-key";

export interface ApiKeyStore {
  [provider: string]: string | undefined;
  anthropic?: string;
  openai?: string;
}

/** Load all API keys from localStorage, migrating from old sessionStorage if needed. */
export function loadKeys(): ApiKeyStore {
  // Migrate from old sessionStorage key
  const oldKey = sessionStorage.getItem(OLD_SESSION_KEY);
  if (oldKey) {
    const keys: ApiKeyStore = { anthropic: oldKey };
    localStorage.setItem(STORAGE_KEY, JSON.stringify(keys));
    sessionStorage.removeItem(OLD_SESSION_KEY);
    return keys;
  }

  const raw = localStorage.getItem(STORAGE_KEY);
  if (!raw) return {};
  try {
    return JSON.parse(raw) as ApiKeyStore;
  } catch {
    return {};
  }
}

/** Save or update a single provider key. */
export function saveKey(provider: string, key: string): void {
  const keys = loadKeys();
  keys[provider] = key;
  localStorage.setItem(STORAGE_KEY, JSON.stringify(keys));
}

/** Remove a single provider key. */
export function removeKey(provider: string): void {
  const keys = loadKeys();
  delete keys[provider];
  localStorage.setItem(STORAGE_KEY, JSON.stringify(keys));
}

/** Delete all stored keys. */
export function clearAll(): void {
  localStorage.removeItem(STORAGE_KEY);
}

/** Mask a key for display: "sk-...xxxx" (last 4 chars). */
export function maskKey(key: string): string {
  if (key.length <= 8) return "****";
  const prefix = key.slice(0, 5);
  const suffix = key.slice(-4);
  return `${prefix}...${suffix}`;
}

/** Check if any keys are configured. */
export function hasAnyKeys(): boolean {
  const keys = loadKeys();
  return Object.values(keys).some((v) => v && v.length > 0);
}
