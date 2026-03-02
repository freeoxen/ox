<script lang="ts">
  import { onMount } from "svelte";
  import type { OxAgent } from "$lib/wasm";
  import {
    loadKeys,
    saveKey,
    removeKey,
    clearAll,
    maskKey,
  } from "$lib/stores/api-keys";
  import { usage, resetUsage } from "$lib/stores/usage";
  import ThemePicker from "$lib/components/ThemePicker.svelte";

  let {
    onclose,
    agent,
    highlight,
  }: {
    onclose: () => void;
    agent: OxAgent;
    highlight?: string;
  } = $props();

  const PROVIDERS = [
    { id: "anthropic", label: "Anthropic", placeholder: "sk-ant-..." },
    { id: "openai", label: "OpenAI", placeholder: "sk-..." },
  ] as const;

  let keys = $state<Record<string, string>>({});
  let inputValues = $state<Record<string, string>>({});
  let usageSnapshot = $state({ requests: 0, inputTokens: 0, outputTokens: 0 });

  onMount(() => {
    keys = loadKeys() as Record<string, string>;
    const unsub = usage.subscribe((v) => {
      usageSnapshot = v;
    });
    return unsub;
  });

  function handleSave(providerId: string) {
    const value = inputValues[providerId]?.trim();
    if (!value) return;
    saveKey(providerId, value);
    agent.set_api_key(providerId, value);
    keys = loadKeys() as Record<string, string>;
    inputValues[providerId] = "";
  }

  function handleRemove(providerId: string) {
    removeKey(providerId);
    agent.remove_api_key(providerId);
    keys = loadKeys() as Record<string, string>;
  }

  function handleClearAll() {
    clearAll();
    for (const p of PROVIDERS) {
      agent.remove_api_key(p.id);
    }
    keys = {};
  }

  function handleKeydown(e: KeyboardEvent, providerId: string) {
    if (e.key === "Enter") handleSave(providerId);
    if (e.key === "Escape") onclose();
  }
</script>

<div class="api-key-overlay" role="presentation" onclick={onclose}>
  <div
    class="settings-dialog"
    role="dialog"
    aria-label="Settings"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => {
      if (e.key === "Escape") onclose();
    }}
  >
    <h3>Settings</h3>

    <div class="settings-section">
      <h4>API Keys</h4>
      <p class="settings-hint">
        {#if highlight}
          Enter your {PROVIDERS.find((p) => p.id === highlight)?.label ??
            highlight}
          API key to use this provider.
        {:else}
          Keys are stored in localStorage and sent only to their respective API
          endpoints.
        {/if}
      </p>
      {#each PROVIDERS as provider (provider.id)}
        <div class="settings-provider-row">
          <span class="settings-provider-label">{provider.label}</span>
          {#if keys[provider.id]}
            <span class="settings-key-masked"
              >{maskKey(keys[provider.id]!)}</span
            >
            <button
              class="edit-btn settings-remove-btn"
              onclick={() => handleRemove(provider.id)}
            >
              Remove
            </button>
          {:else}
            <input
              type="password"
              class="api-key-input settings-key-input"
              placeholder={provider.placeholder}
              autocomplete="off"
              spellcheck="false"
              bind:value={inputValues[provider.id]}
              onkeydown={(e) => handleKeydown(e, provider.id)}
            />
            <button
              class="save-btn settings-save-btn"
              onclick={() => handleSave(provider.id)}
            >
              Save
            </button>
          {/if}
        </div>
      {/each}
    </div>

    <div class="settings-section">
      <h4>Session Usage</h4>
      <div class="settings-usage-grid">
        <span class="settings-usage-label">Requests</span>
        <span class="settings-usage-value">{usageSnapshot.requests}</span>
        <span class="settings-usage-label">Input tokens</span>
        <span class="settings-usage-value"
          >{usageSnapshot.inputTokens.toLocaleString()}</span
        >
        <span class="settings-usage-label">Output tokens</span>
        <span class="settings-usage-value"
          >{usageSnapshot.outputTokens.toLocaleString()}</span
        >
      </div>
      <button class="edit-btn" onclick={resetUsage}>Reset</button>
    </div>

    <div class="settings-section settings-theme-section">
      <h4>Theme</h4>
      <div class="settings-theme-clock">
        <ThemePicker />
      </div>
    </div>

    <div class="settings-actions">
      <button class="cancel-btn" onclick={handleClearAll}>Clear all keys</button
      >
      <button class="save-btn" onclick={onclose}>Close</button>
    </div>
  </div>
</div>
