export class OxAgent {
  free(): void;
  debug_context(): string;
  on_event(callback: Function): void;
  prompt(input: string): Promise<string>;
  register_tool(
    name: string,
    description: string,
    parameters_schema_json: string,
    callback: Function,
  ): void;
  list_models(): string;
  refresh_models(): Promise<string>;
  set_model(model_id: string): void;
  set_system_prompt(new_prompt: string): void;
  unregister_tool(name: string): void;
  set_api_key(provider: string, key: string): void;
  remove_api_key(provider: string): void;
  has_api_key(provider: string): boolean;
  set_provider(provider: string): void;
  get_provider(): string;
}
export function create_agent(system_prompt: string, api_key: string): OxAgent;
export default function init(module_or_path?: string): Promise<unknown>;
