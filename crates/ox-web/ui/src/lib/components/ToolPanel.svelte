<script lang="ts">
  import type { OxAgent } from "$lib/wasm";
  import type { ToolDef } from "$lib/types";
  import {
    ToolStore,
    BUILTIN_TOOLS,
    FACTORY_PROFILE,
    activeJsTools,
    applyProfile,
  } from "$lib/stores/tools";
  import { compileTool } from "$lib/tool-compiler";

  let {
    agent,
  }: {
    agent: OxAgent;
  } = $props();

  // Reactive state — bumped to trigger re-render after mutations
  let version = $state(0);
  function refresh() {
    version += 1;
  }

  // Derived from ToolStore (re-evaluated when version changes)
  let activeProfileName = $derived.by(() => {
    void version;
    return ToolStore.getActiveProfile();
  });
  let isFactory = $derived(ToolStore.isFactory(activeProfileName));
  let lib = $derived.by(() => {
    void version;
    return ToolStore.loadLibrary();
  });
  let profileTools = $derived.by(() => {
    void version;
    return ToolStore.getProfileTools(activeProfileName);
  });
  let toolNames = $derived(
    isFactory ? BUILTIN_TOOLS.map((t) => t.name) : Object.keys(lib).sort(),
  );

  // New profile input
  let showNewProfile = $state(false);
  let newProfileName = $state("");

  // Add tool form
  let addFormOpen = $state(false);
  let addName = $state("");
  let addDesc = $state("");
  let addParams = $state("");
  let addBody = $state("");
  let addError = $state("");

  // Edit tool form
  let editingTool = $state<string | null>(null);
  let editDesc = $state("");
  let editParams = $state("");
  let editBody = $state("");
  let editError = $state("");

  // Profile step (when saving to factory)
  let showProfileStep = $state(false);
  let profileStepName = $state("");
  let pendingDef: ToolDef | null = null;

  function handleProfileChange(e: Event) {
    const select = e.target as HTMLSelectElement;
    applyProfile(agent, select.value);
    refresh();
  }

  function handleNewProfile() {
    showNewProfile = true;
    newProfileName = "";
  }

  function confirmNewProfile() {
    const name = newProfileName.trim();
    if (!name || name === FACTORY_PROFILE) return;
    ToolStore.createProfile(name);
    applyProfile(agent, name);
    showNewProfile = false;
    refresh();
  }

  function cancelNewProfile() {
    showNewProfile = false;
    refresh();
  }

  function handleDeleteProfile() {
    if (isFactory) return;
    ToolStore.deleteProfile(activeProfileName);
    const fallback = ToolStore.profileNames()[0];
    applyProfile(agent, fallback);
    refresh();
  }

  function handleResetProfile() {
    for (const name of activeJsTools) {
      agent.unregister_tool(name);
    }
    activeJsTools.clear();
    ToolStore.setProfileTools(activeProfileName, []);
    refresh();
  }

  function handleCheckbox(name: string, checked: boolean) {
    if (checked) {
      const def = lib[name];
      try {
        const { schemaJson, callback } = compileTool(def);
        agent.register_tool(name, def.description, schemaJson, callback);
        activeJsTools.add(name);
        const updated = ToolStore.getProfileTools(activeProfileName);
        if (!updated.includes(name)) updated.push(name);
        ToolStore.setProfileTools(activeProfileName, updated);
      } catch (_) {
        // revert
      }
    } else {
      agent.unregister_tool(name);
      activeJsTools.delete(name);
      const updated = ToolStore.getProfileTools(activeProfileName).filter(
        (n) => n !== name,
      );
      ToolStore.setProfileTools(activeProfileName, updated);
    }
    refresh();
  }

  function handleCopy(name: string) {
    const def = lib[name];
    let copyName = def.name + "_copy";
    while (lib[copyName]) copyName += "_copy";
    addName = copyName;
    addDesc = def.description;
    addParams = def.params;
    addBody = def.body;
    addFormOpen = true;
    addError = "";
  }

  function handleEdit(name: string) {
    if (editingTool === name) {
      editingTool = null;
      return;
    }
    const def = lib[name];
    editingTool = name;
    editDesc = def.description;
    editParams = def.params;
    editBody = def.body;
    editError = "";
  }

  function saveEdit() {
    if (!editingTool) return;
    editError = "";
    const updated: ToolDef = {
      name: editingTool,
      description: editDesc.trim(),
      params: editParams.trim(),
      body: editBody.trim(),
    };
    if (!updated.description || !updated.params || !updated.body) {
      editError = "All fields are required.";
      return;
    }
    try {
      compileTool(updated);
    } catch (e: unknown) {
      editError = "Invalid: " + (e instanceof Error ? e.message : String(e));
      return;
    }
    ToolStore.saveTool(updated);
    if (activeJsTools.has(editingTool)) {
      const { schemaJson, callback } = compileTool(updated);
      agent.unregister_tool(editingTool);
      agent.register_tool(
        editingTool,
        updated.description,
        schemaJson,
        callback,
      );
    }
    editingTool = null;
    refresh();
  }

  function handleDelete(name: string) {
    if (activeJsTools.has(name)) {
      agent.unregister_tool(name);
      activeJsTools.delete(name);
    }
    ToolStore.deleteTool(name);
    refresh();
  }

  function validateAddForm(): ToolDef | null {
    addError = "";
    const name = addName.trim();
    const description = addDesc.trim();
    const params = addParams.trim();
    const body = addBody.trim();
    if (!name || !description || !params || !body) {
      addError = "All fields are required.";
      return null;
    }
    const def: ToolDef = { name, description, params, body };
    try {
      compileTool(def);
    } catch (e: unknown) {
      addError = "Invalid: " + (e instanceof Error ? e.message : String(e));
      return null;
    }
    return def;
  }

  function saveToolToProfile(def: ToolDef, targetProfile: string) {
    ToolStore.saveTool(def);
    try {
      const { schemaJson, callback } = compileTool(def);
      agent.register_tool(def.name, def.description, schemaJson, callback);
      activeJsTools.add(def.name);
      const updated = ToolStore.getProfileTools(targetProfile);
      if (!updated.includes(def.name)) updated.push(def.name);
      ToolStore.setProfileTools(targetProfile, updated);
    } catch (e: unknown) {
      addError =
        "Saved to library but registration failed: " +
        (e instanceof Error ? e.message : String(e));
      refresh();
      return;
    }
    addName = "";
    addDesc = "";
    addParams = "";
    addBody = "";
    addFormOpen = false;
    showProfileStep = false;
    refresh();
  }

  function handleSave() {
    const def = validateAddForm();
    if (!def) return;
    if (ToolStore.isFactory(ToolStore.getActiveProfile())) {
      pendingDef = def;
      showProfileStep = true;
      profileStepName = "";
    } else {
      saveToolToProfile(def, ToolStore.getActiveProfile());
    }
  }

  function handleProfileStepCreate() {
    const name = profileStepName.trim();
    if (!name || name === FACTORY_PROFILE || !pendingDef) return;
    ToolStore.createProfile(name);
    applyProfile(agent, name);
    saveToolToProfile(pendingDef, name);
    pendingDef = null;
  }

  function cancelProfileStep() {
    showProfileStep = false;
    pendingDef = null;
  }
</script>

<!-- Profile row -->
{#if showNewProfile}
  <div class="profile-row">
    <input
      type="text"
      class="profile-name-input"
      placeholder="profile name"
      bind:value={newProfileName}
      onkeydown={(e) => {
        if (e.key === "Enter") confirmNewProfile();
        if (e.key === "Escape") cancelNewProfile();
      }}
    />
    <button
      class="save-btn"
      style="font-size:11px;padding:2px 8px;font-weight:normal"
      onclick={confirmNewProfile}>ok</button
    >
    <button
      class="cancel-btn"
      style="font-size:11px;padding:2px 8px;font-weight:normal"
      onclick={cancelNewProfile}>cancel</button
    >
  </div>
{:else}
  <div class="profile-row">
    <select onchange={handleProfileChange}>
      {#each ToolStore.profileNames() as pname (pname)}
        <option value={pname} selected={pname === activeProfileName}
          >{pname}</option
        >
      {/each}
    </select>
    <button class="edit-btn" onclick={handleNewProfile}>new</button>
    <button class="edit-btn" disabled={isFactory} onclick={handleDeleteProfile}
      >del</button
    >
    {#if !isFactory}
      <button class="edit-btn" onclick={handleResetProfile}>reset</button>
    {/if}
  </div>
{/if}

<hr class="tool-panel-divider" />

<!-- Tool library list -->
{#if toolNames.length > 0}
  <div class="tool-library-list">
    {#each toolNames as name (name)}
      {@const isBuiltin = ToolStore.isBuiltin(name)}
      <div>
        <div class="tool-library-row">
          <input
            type="checkbox"
            checked={profileTools.includes(name)}
            disabled={isFactory}
            onchange={(e) =>
              handleCheckbox(name, (e.target as HTMLInputElement).checked)}
          />
          <span class="tool-lib-name">{name}</span>
          {#if !isFactory}
            <button class="tool-lib-del" onclick={() => handleCopy(name)}
              >copy</button
            >
            {#if !isBuiltin}
              <button class="tool-lib-del" onclick={() => handleEdit(name)}
                >edit</button
              >
              <button class="tool-lib-del" onclick={() => handleDelete(name)}
                >del</button
              >
            {/if}
          {/if}
        </div>

        <!-- Inline edit form -->
        {#if editingTool === name}
          <div
            class="tool-edit-form"
            style="padding-left:22px;padding-top:4px;padding-bottom:4px"
          >
            <label class="tool-form-field">
              <span>description</span>
              <input bind:value={editDesc} />
            </label>
            <label class="tool-form-field">
              <span>parameters</span>
              <input bind:value={editParams} />
            </label>
            <label class="tool-form-field">
              <span>body</span>
              <textarea rows="3" bind:value={editBody}></textarea>
            </label>
            <div class="edit-actions">
              <button class="save-btn" onclick={saveEdit}>save</button>
              <button class="cancel-btn" onclick={() => (editingTool = null)}
                >cancel</button
              >
            </div>
            {#if editError}
              <div class="tool-form-error">{editError}</div>
            {/if}
          </div>
        {/if}
      </div>
    {/each}
  </div>
{:else}
  <div class="tool-panel-empty">no tools in library</div>
{/if}

<hr class="tool-panel-divider" />

<!-- Add new tool -->
<details bind:open={addFormOpen}>
  <summary style="cursor:pointer">add new tool...</summary>
  <div style="padding-top:8px">
    <label class="tool-form-field">
      <span>name</span>
      <input placeholder="my_tool" bind:value={addName} />
    </label>
    <label class="tool-form-field">
      <span>description</span>
      <input placeholder="What this tool does" bind:value={addDesc} />
    </label>
    <label class="tool-form-field">
      <span>parameters (TypeScript style)</span>
      <input
        placeholder="(text: string, count?: number)"
        bind:value={addParams}
      />
    </label>
    <label class="tool-form-field">
      <span>function body (params available by name; return a string)</span>
      <textarea
        rows="4"
        placeholder="return String(n + 2);"
        bind:value={addBody}
      ></textarea>
    </label>

    {#if showProfileStep}
      <label class="tool-form-field">
        <span>new profile name</span>
        <input
          type="text"
          placeholder="my profile"
          bind:value={profileStepName}
          onkeydown={(e) => {
            if (e.key === "Enter") handleProfileStepCreate();
            if (e.key === "Escape") cancelProfileStep();
          }}
        />
      </label>
      <div class="tool-form-actions">
        <button class="save-btn" onclick={handleProfileStepCreate}
          >create & save</button
        >
        <button class="cancel-btn" onclick={cancelProfileStep}>cancel</button>
      </div>
    {:else}
      <div class="tool-form-actions">
        <button class="save-btn" onclick={handleSave}>save</button>
      </div>
    {/if}

    {#if addError}
      <div class="tool-form-error">{addError}</div>
    {/if}
  </div>
</details>
