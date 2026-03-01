export interface AgentEvent {
  type: string;
  data: string;
}

export interface ToolDef {
  name: string;
  description: string;
  params: string;
  body: string;
}

export interface CompiledTool {
  schemaJson: string;
  callback: (inputJson: string) => string;
}

export interface DebugContext {
  system: string | null;
  model: { id?: string; max_tokens?: number };
  tools: Array<{
    name: string;
    description: string;
    input_schema: unknown;
  }>;
  history: {
    count?: number;
    messages?: DebugMessage[];
  };
}

export interface DebugMessage {
  role: string;
  content: string | DebugContentBlock[];
}

export interface DebugContentBlock {
  type: string;
  text?: string;
  name?: string;
  input?: unknown;
  tool_use_id?: string;
  content?: string;
}

export interface RequestLogEntry {
  timestamp: Date;
  data: string;
}
