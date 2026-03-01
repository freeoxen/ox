import type { ToolDef, CompiledTool } from "./types";

const TS_TYPE_MAP: Record<string, string> = {
  string: "string",
  number: "number",
  boolean: "boolean",
  integer: "integer",
};

interface ParamSchema {
  type: "object";
  properties: Record<string, { type: string }>;
  required: string[];
}

export function parseParamSignature(sig: string): ParamSchema {
  sig = sig.trim();
  if (sig.startsWith("(") && sig.endsWith(")")) {
    sig = sig.slice(1, -1);
  }
  const properties: Record<string, { type: string }> = {};
  const required: string[] = [];
  if (!sig) return { type: "object", properties, required };
  for (const param of sig.split(",")) {
    const trimmed = param.trim();
    if (!trimmed) continue;
    const m = trimmed.match(/^(\w+)(\?)?\s*:\s*(\w+)$/);
    if (!m) throw new Error('cannot parse "' + trimmed + '"');
    const [, name, optional, tsType] = m;
    const jsonType = TS_TYPE_MAP[tsType.toLowerCase()];
    if (!jsonType) throw new Error('unknown type "' + tsType + '"');
    properties[name] = { type: jsonType };
    if (!optional) required.push(name);
  }
  return { type: "object", properties, required };
}

export function compileTool(def: ToolDef): CompiledTool {
  const schemaObj = parseParamSignature(def.params);
  const paramNames = Object.keys(schemaObj.properties);
  const destructureArg =
    paramNames.length > 0 ? "{" + paramNames.join(", ") + "}" : "_";
  const userFn = new Function(destructureArg, def.body);
  const callback = (inputJson: string): string => {
    try {
      return String(userFn(JSON.parse(inputJson)));
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : String(e);
      return "error: " + msg;
    }
  };
  return { schemaJson: JSON.stringify(schemaObj), callback };
}
