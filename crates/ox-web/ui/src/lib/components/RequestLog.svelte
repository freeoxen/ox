<script lang="ts">
  import { requestLog } from "$lib/stores/request-log";

  function formatTime(ts: Date): string {
    return [
      String(ts.getHours()).padStart(2, "0"),
      String(ts.getMinutes()).padStart(2, "0"),
      String(ts.getSeconds()).padStart(2, "0"),
    ].join(":");
  }

  function parseEntry(data: string): {
    parsed: { messages?: unknown[]; tools?: unknown[] } | null;
    msgCount: number | string;
    toolCount: number;
  } {
    let parsed: { messages?: unknown[]; tools?: unknown[] } | null;
    try {
      parsed = JSON.parse(data);
    } catch (_) {
      parsed = null;
    }
    const msgCount = parsed?.messages ? parsed.messages.length : "?";
    const toolCount = parsed?.tools ? parsed.tools.length : 0;
    return { parsed, msgCount, toolCount };
  }
</script>

{#if $requestLog.length === 0}
  <div class="empty">no requests yet</div>
{:else}
  {#each $requestLog as entry, i (i)}
    {@const info = parseEntry(entry.data)}
    <details>
      <summary>
        #{i}
        {formatTime(entry.timestamp)} &mdash; {info.msgCount} messages, {info.toolCount}
        tool{info.toolCount !== 1 ? "s" : ""}
      </summary>
      <pre class="request-json">{info.parsed
          ? JSON.stringify(info.parsed, null, 2)
          : entry.data}</pre>
    </details>
  {/each}
{/if}
