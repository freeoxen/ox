<script lang="ts">
  import { status } from "$lib/stores/status";
</script>

<div id="status-bar">
  {#if $status.label}
    <span class="status-dot"></span>
    <span class="status-label">{$status.label}</span>
    {#if $status.detail}
      <span class="status-detail">{$status.detail}</span>
    {/if}
  {/if}
</div>
