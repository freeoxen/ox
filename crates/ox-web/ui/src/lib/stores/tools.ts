import type { ToolDef } from "$lib/types";
import type { OxAgent } from "$lib/wasm";
import { compileTool } from "$lib/tool-compiler";

export const BUILTIN_TOOLS: ToolDef[] = [
  {
    name: "reverse_text",
    description: "Reverse the characters in a string",
    params: "(text: string)",
    body: 'return text.split("").reverse().join("");',
  },
];

const BUILTIN_NAMES = new Set(BUILTIN_TOOLS.map((t) => t.name));

export const FACTORY_PROFILE = "factory default";

interface ProfileData {
  active: string;
  profiles: Record<string, string[]>;
}

export const ToolStore = {
  isFactory(name: string): boolean {
    return name === FACTORY_PROFILE;
  },

  isBuiltin(name: string): boolean {
    return BUILTIN_NAMES.has(name);
  },

  loadLibrary(): Record<string, ToolDef> {
    const lib: Record<string, ToolDef> = {};
    for (const t of BUILTIN_TOOLS) lib[t.name] = t;
    try {
      const user = JSON.parse(
        localStorage.getItem("ox:tool-library") as string,
      );
      if (user) Object.assign(lib, user);
    } catch (_) {
      /* empty */
    }
    return lib;
  },

  _loadUserLibrary(): Record<string, ToolDef> {
    try {
      return (
        JSON.parse(localStorage.getItem("ox:tool-library") as string) || {}
      );
    } catch (_) {
      return {};
    }
  },

  saveLibrary(lib: Record<string, ToolDef>): void {
    localStorage.setItem("ox:tool-library", JSON.stringify(lib));
  },

  saveTool(def: ToolDef): void {
    const lib = this._loadUserLibrary();
    lib[def.name] = def;
    this.saveLibrary(lib);
  },

  deleteTool(name: string): void {
    if (BUILTIN_NAMES.has(name)) return;
    const lib = this._loadUserLibrary();
    delete lib[name];
    this.saveLibrary(lib);
    const data = this._loadUserProfiles();
    for (const pname of Object.keys(data.profiles)) {
      data.profiles[pname] = data.profiles[pname].filter((n) => n !== name);
    }
    this._saveUserProfiles(data);
  },

  _loadUserProfiles(): ProfileData {
    try {
      const data = JSON.parse(
        localStorage.getItem("ox:profiles") as string,
      ) as ProfileData;
      if (data && data.profiles && Object.keys(data.profiles).length > 0) {
        return data;
      }
    } catch (_) {
      /* empty */
    }
    return { active: FACTORY_PROFILE, profiles: { default: [] } };
  },

  _saveUserProfiles(data: ProfileData): void {
    localStorage.setItem("ox:profiles", JSON.stringify(data));
  },

  getActiveProfile(): string {
    return this._loadUserProfiles().active;
  },

  setActiveProfile(name: string): void {
    const data = this._loadUserProfiles();
    data.active = name;
    this._saveUserProfiles(data);
  },

  getProfileTools(name: string): string[] {
    if (name === FACTORY_PROFILE) return BUILTIN_TOOLS.map((t) => t.name);
    const data = this._loadUserProfiles();
    return data.profiles[name] || [];
  },

  setProfileTools(name: string, toolNames: string[]): void {
    if (name === FACTORY_PROFILE) return;
    const data = this._loadUserProfiles();
    data.profiles[name] = toolNames;
    this._saveUserProfiles(data);
  },

  createProfile(name: string): void {
    if (name === FACTORY_PROFILE) return;
    const data = this._loadUserProfiles();
    if (!data.profiles[name]) data.profiles[name] = [];
    this._saveUserProfiles(data);
  },

  deleteProfile(name: string): void {
    if (name === FACTORY_PROFILE) return;
    const data = this._loadUserProfiles();
    delete data.profiles[name];
    const remaining = Object.keys(data.profiles);
    data.active = remaining.length > 0 ? remaining[0] : FACTORY_PROFILE;
    this._saveUserProfiles(data);
  },

  profileNames(): string[] {
    const userNames = Object.keys(this._loadUserProfiles().profiles);
    return [FACTORY_PROFILE, ...userNames];
  },
};

// --- Active JS tools set (shared across tool panel + agent) ---

export let activeJsTools = new Set<string>();

export function applyProfile(agent: OxAgent, profileName: string): void {
  for (const name of activeJsTools) {
    agent.unregister_tool(name);
  }
  activeJsTools.clear();
  const toolNames = ToolStore.getProfileTools(profileName);
  const lib = ToolStore.loadLibrary();
  for (const name of toolNames) {
    const def = lib[name];
    if (!def) continue;
    try {
      const { schemaJson, callback } = compileTool(def);
      agent.register_tool(name, def.description, schemaJson, callback);
      activeJsTools.add(name);
    } catch (_) {
      /* skip tools that fail to compile */
    }
  }
  ToolStore.setActiveProfile(profileName);
}
