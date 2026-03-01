<script lang="ts">
  import { messages, streamingText } from "$lib/stores/chat";
  import { tick } from "svelte";

  let outputEl: HTMLDivElement | undefined = $state();

  async function scrollToBottom() {
    await tick();
    if (outputEl) outputEl.scrollTop = outputEl.scrollHeight;
  }

  $effect(() => {
    // Re-run on any message or stream change
    void $messages;
    void $streamingText;
    scrollToBottom();
  });
</script>

<div id="output" bind:this={outputEl}>
  {#each $messages as msg, i (i)}
    <span class={msg.cls}>{msg.text}</span>
  {/each}
  {#if $streamingText}
    <span class="assistant-msg">{$streamingText}</span>
  {/if}
</div>
