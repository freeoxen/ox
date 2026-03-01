<script lang="ts">
  let {
    disabled = false,
    onsend,
  }: {
    disabled?: boolean;
    onsend?: (text: string) => void;
  } = $props();

  let value = $state("");

  function handleSend() {
    const text = value.trim();
    if (!text) return;
    value = "";
    onsend?.(text);
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") handleSend();
  }
</script>

<div id="input-row">
  <input
    id="input"
    type="text"
    placeholder="Type a message..."
    bind:value
    {disabled}
    onkeydown={handleKeydown}
  />
  <button id="send" {disabled} onclick={handleSend}>Send</button>
</div>
