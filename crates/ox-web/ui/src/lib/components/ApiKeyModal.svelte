<script lang="ts">
  let {
    onsubmit,
    oncancel,
  }: {
    onsubmit?: (key: string) => void;
    oncancel?: () => void;
  } = $props();

  let inputValue = $state("");
  let inputEl: HTMLInputElement | undefined = $state();

  function handleSubmit() {
    const key = inputValue.trim();
    if (key) onsubmit?.(key);
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") handleSubmit();
    if (e.key === "Escape") oncancel?.();
  }

  $effect(() => {
    // Focus input after mount
    inputEl?.focus();
  });
</script>

<div class="api-key-overlay">
  <div class="api-key-dialog">
    <h3>Anthropic API Key</h3>
    <p>
      Enter your Anthropic API key to connect. Your key is stored in
      sessionStorage and never sent to any server other than api.anthropic.com.
    </p>
    <input
      type="password"
      class="api-key-input"
      placeholder="sk-ant-..."
      autocomplete="off"
      spellcheck="false"
      bind:value={inputValue}
      bind:this={inputEl}
      onkeydown={handleKeydown}
    />
    <div class="api-key-actions">
      <button class="save-btn api-key-submit" onclick={handleSubmit}>
        Connect
      </button>
      <button class="cancel-btn api-key-cancel" onclick={() => oncancel?.()}>
        Cancel
      </button>
    </div>
  </div>
</div>
