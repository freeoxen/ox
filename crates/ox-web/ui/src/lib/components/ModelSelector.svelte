<script lang="ts">
  import { onMount } from "svelte";
  import type { OxAgent } from "$lib/wasm";

  let {
    agent,
  }: {
    agent: OxAgent;
  } = $props();

  const MODEL_STORAGE_KEY = "ox:model";
  const DEFAULT_MODEL = "claude-sonnet-4-20250514";

  let models = $state<{ id: string; display_name: string }[]>([
    { id: DEFAULT_MODEL, display_name: DEFAULT_MODEL },
  ]);
  let selectedModel = $state(
    sessionStorage.getItem(MODEL_STORAGE_KEY) || DEFAULT_MODEL,
  );

  function handleChange(e: Event) {
    const select = e.target as HTMLSelectElement;
    selectedModel = select.value;
    sessionStorage.setItem(MODEL_STORAGE_KEY, selectedModel);
    agent.set_model(selectedModel);
  }

  onMount(() => {
    // Apply stored model
    if (selectedModel !== DEFAULT_MODEL) {
      if (!models.some((m) => m.id === selectedModel)) {
        models = [
          ...models,
          { id: selectedModel, display_name: selectedModel },
        ];
      }
      agent.set_model(selectedModel);
    }

    // Fetch model catalog (non-blocking)
    agent
      .refresh_models()
      .then((catalogJson: string) => {
        const catalog: { id: string; display_name: string }[] =
          JSON.parse(catalogJson);
        if (catalog.length === 0) return;

        const current = selectedModel;
        models = catalog;

        if (!catalog.some((m) => m.id === current)) {
          selectedModel = DEFAULT_MODEL;
          agent.set_model(DEFAULT_MODEL);
        }
      })
      .catch(() => {
        // Catalog fetch failed -- keep defaults
      });
  });
</script>

<select class="model-select" value={selectedModel} onchange={handleChange}>
  {#each models as model (model.id)}
    <option value={model.id}>{model.display_name}</option>
  {/each}
</select>
