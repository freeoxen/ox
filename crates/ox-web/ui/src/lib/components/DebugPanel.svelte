<script lang="ts">
  import type { OxAgent } from "$lib/wasm";
  import type {
    DebugContext,
    DebugMessage,
    DebugContentBlock,
  } from "$lib/types";

  let {
    agent,
    debugContext,
  }: {
    agent: OxAgent;
    debugContext: DebugContext | null;
  } = $props();

  let editingSystem = $state(false);
  let systemDraft = $state("");

  function startEditSystem() {
    systemDraft = debugContext?.system ?? "";
    editingSystem = true;
  }

  function saveSystem() {
    try {
      agent.set_system_prompt(systemDraft);
    } catch (err) {
      alert("Failed to save: " + err);
    }
    editingSystem = false;
  }

  function cancelEditSystem() {
    editingSystem = false;
  }

  function isToolResult(msg: DebugMessage): boolean {
    return (
      msg.role === "user" &&
      Array.isArray(msg.content) &&
      msg.content.length > 0 &&
      (msg.content[0] as DebugContentBlock).type === "tool_result"
    );
  }

  function truncate(text: string, max: number): string {
    return text.length > max ? text.slice(0, max) + "..." : text;
  }
</script>

{#if debugContext}
  {@const ctx = debugContext}
  {@const tools = ctx.tools || []}
  {@const hist = ctx.history || {}}
  {@const histCount = hist.count || 0}
  {@const histMessages = hist.messages || []}
  {@const systemVal = ctx.system}

  <!-- /system (editable) -->
  <details open>
    <summary>
      <span class="section-header">
        /system
        {#if !editingSystem}
          <button
            class="edit-btn"
            onclick={(e) => {
              e.preventDefault();
              e.stopPropagation();
              startEditSystem();
            }}>edit</button
          >
        {/if}
      </span>
    </summary>
    <div class="section-body">
      {#if editingSystem}
        <textarea class="system-textarea" bind:value={systemDraft}></textarea>
        <div class="edit-actions">
          <button class="save-btn" onclick={saveSystem}>save</button>
          <button class="cancel-btn" onclick={cancelEditSystem}>cancel</button>
        </div>
      {:else if systemVal != null}
        <div class="str-val">{truncate(String(systemVal), 200)}</div>
      {:else}
        <div class="empty">null</div>
      {/if}
    </div>
  </details>

  <!-- /model -->
  <details open>
    <summary>/model</summary>
    <div class="section-body">
      <div class="kv">
        <span class="k">id</span>
        <span class="v str-val">{ctx.model?.id ?? "null"}</span>
      </div>
      <div class="kv">
        <span class="k">max_tokens</span>
        <span class="v num-val">{ctx.model?.max_tokens ?? "null"}</span>
      </div>
    </div>
  </details>

  <!-- /tools -->
  <details>
    <summary>/tools ({tools.length})</summary>
    <div class="section-body">
      {#if tools.length === 0}
        <div class="empty">none</div>
      {:else}
        {#each tools as tool (tool.name)}
          <details>
            <summary>
              <span class="tool-name">{tool.name}</span>
            </summary>
            <div class="section-body">
              <div class="kv">
                <span class="k">description</span>
                <span class="v str-val">{tool.description}</span>
              </div>
              <div class="kv">
                <span class="k">input_schema</span>
                <pre
                  class="v"
                  style="margin:0;white-space:pre-wrap">{JSON.stringify(
                    tool.input_schema,
                    null,
                    2,
                  )}</pre>
              </div>
            </div>
          </details>
        {/each}
      {/if}
    </div>
  </details>

  <!-- /history -->
  <details open>
    <summary>/history ({histCount})</summary>
    <div class="section-body">
      {#if histMessages.length === 0}
        <div class="empty">empty</div>
      {:else}
        {#each histMessages as msg, i (i)}
          {@const toolResult = isToolResult(msg)}
          <div class="msg-entry">
            <div>
              <span class="text-muted">#{i} </span>
              {#if toolResult}
                <span class="role-badge role-tool-result">tool_result</span>
              {:else if msg.role === "assistant"}
                <span class="role-badge role-assistant">assistant</span>
              {:else}
                <span class="role-badge role-user">user</span>
              {/if}
            </div>
            <div class="msg-content">
              {#if toolResult}
                {#each msg.content as r}
                  {@const block = r as DebugContentBlock}
                  <div>
                    <span class="text-muted"
                      >{block.tool_use_id
                        ? block.tool_use_id.slice(0, 12) + "..."
                        : "?"}</span
                    >
                    <span class="text-muted"> &rarr; </span>
                    <span class="str-val">{block.content || ""}</span>
                  </div>
                {/each}
              {:else if msg.role === "assistant" && Array.isArray(msg.content)}
                {#each msg.content as block}
                  {@const b = block as DebugContentBlock}
                  <div>
                    {#if b.type === "text"}
                      <span class="block-tag">[text] </span>
                      <span>{truncate(b.text || "", 120)}</span>
                    {:else if b.type === "tool_use"}
                      <span class="block-tag">[tool_use] </span>
                      <span class="tool-name">{b.name} </span>
                      <span class="text-muted">{JSON.stringify(b.input)}</span>
                    {/if}
                  </div>
                {/each}
              {:else}
                {truncate(
                  typeof msg.content === "string"
                    ? msg.content
                    : JSON.stringify(msg.content),
                  200,
                )}
              {/if}
            </div>
          </div>
        {/each}
      {/if}
    </div>
  </details>
{:else}
  <div class="panel-loading"><div class="spinner"></div></div>
{/if}
