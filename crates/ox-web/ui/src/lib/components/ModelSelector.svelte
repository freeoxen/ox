<script lang="ts">
  import { onMount } from "svelte";
  import type { OxAgent } from "$lib/wasm";
  import { loadKeys } from "$lib/stores/api-keys";

  let {
    agent,
    onrequestkey,
    activateProvider,
    onactivated,
  }: {
    agent: OxAgent;
    onrequestkey?: (provider: string) => void;
    /** Set this to a provider name to trigger a switch (e.g. after a key is added). */
    activateProvider?: string;
    /** Called after activateProvider is consumed, so parent can clear it. */
    onactivated?: () => void;
  } = $props();

  const MODEL_STORAGE_KEY = "ox:model";
  const PROVIDER_STORAGE_KEY = "ox:provider";

  const DEFAULT_MODELS: Record<string, string> = {
    anthropic: "claude-sonnet-4-20250514",
    openai: "gpt-4o",
  };

  const PROVIDERS = [
    { id: "anthropic", label: "Anthropic" },
    { id: "openai", label: "OpenAI" },
  ] as const;

  const initialProvider =
    localStorage.getItem(PROVIDER_STORAGE_KEY) || "anthropic";
  const initialDefaultModel =
    DEFAULT_MODELS[initialProvider] ?? DEFAULT_MODELS.anthropic;

  let selectedProvider = $state(initialProvider);
  let models = $state<{ id: string; display_name: string }[]>([
    { id: initialDefaultModel, display_name: initialDefaultModel },
  ]);
  let selectedModel = $state(
    localStorage.getItem(MODEL_STORAGE_KEY) || initialDefaultModel,
  );

  /** Apply provider switch: update namespace, reset model, refresh catalog. */
  function applyProvider(provider: string) {
    selectedProvider = provider;
    localStorage.setItem(PROVIDER_STORAGE_KEY, provider);
    agent.set_provider(provider);

    // Reset to default model for this provider
    const defaultModel = DEFAULT_MODELS[provider] ?? DEFAULT_MODELS.anthropic;
    selectedModel = defaultModel;
    localStorage.setItem(MODEL_STORAGE_KEY, selectedModel);
    agent.set_model(selectedModel);
    models = [{ id: defaultModel, display_name: defaultModel }];

    // Refresh catalog for new provider
    agent
      .refresh_models()
      .then((catalogJson: string) => {
        const catalog: { id: string; display_name: string }[] =
          JSON.parse(catalogJson);
        if (catalog.length === 0) return;
        models = catalog;
        if (!catalog.some((m) => m.id === selectedModel)) {
          selectedModel = defaultModel;
          agent.set_model(defaultModel);
        }
      })
      .catch(() => {
        // Catalog fetch failed — keep defaults
      });
  }

  function handleProviderChange(e: Event) {
    const select = e.target as HTMLSelectElement;
    const newProvider = select.value;

    // Check if the user has a key for this provider
    const keys = loadKeys();
    if (!keys[newProvider]) {
      // Revert the DOM select and open settings for key entry
      select.value = selectedProvider;
      onrequestkey?.(newProvider);
      return;
    }

    applyProvider(newProvider);
  }

  function handleModelChange(e: Event) {
    const select = e.target as HTMLSelectElement;
    selectedModel = select.value;
    localStorage.setItem(MODEL_STORAGE_KEY, selectedModel);
    agent.set_model(selectedModel);
  }

  /** Refresh model catalog for the current provider (non-blocking). */
  function refreshCatalog() {
    const defaultModel =
      DEFAULT_MODELS[selectedProvider] ?? DEFAULT_MODELS.anthropic;
    agent
      .refresh_models()
      .then((catalogJson: string) => {
        const catalog: { id: string; display_name: string }[] =
          JSON.parse(catalogJson);
        if (catalog.length === 0) return;
        models = catalog;
        if (!catalog.some((m) => m.id === selectedModel)) {
          selectedModel = defaultModel;
          agent.set_model(defaultModel);
        }
      })
      .catch(() => {
        // Catalog fetch failed — keep defaults
      });
  }

  // React to parent requesting a provider activation (e.g. after key added in settings)
  $effect(() => {
    if (!activateProvider) return;
    if (activateProvider !== selectedProvider) {
      applyProvider(activateProvider);
    } else {
      // Same provider, but key may have just been added — refresh catalog
      refreshCatalog();
    }
    onactivated?.();
  });

  onMount(() => {
    // Apply stored provider
    agent.set_provider(selectedProvider);

    // Apply stored model
    const defaultModel =
      DEFAULT_MODELS[selectedProvider] ?? DEFAULT_MODELS.anthropic;
    if (selectedModel !== defaultModel) {
      if (!models.some((m) => m.id === selectedModel)) {
        models = [
          ...models,
          { id: selectedModel, display_name: selectedModel },
        ];
      }
      agent.set_model(selectedModel);
    }

    // Fetch model catalog (non-blocking)
    refreshCatalog();
  });
</script>

<div class="model-selector-row">
  <select
    class="provider-select"
    value={selectedProvider}
    onchange={handleProviderChange}
  >
    {#each PROVIDERS as provider (provider.id)}
      <option value={provider.id}>{provider.label}</option>
    {/each}
  </select>
  <select
    class="model-select"
    value={selectedModel}
    onchange={handleModelChange}
  >
    {#each models as model (model.id)}
      <option value={model.id}>{model.display_name}</option>
    {/each}
  </select>
</div>
