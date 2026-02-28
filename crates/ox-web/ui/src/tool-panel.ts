import { toolPanelEl } from "./dom";
import { ToolStore, BUILTIN_TOOLS, FACTORY_PROFILE } from "./tool-store";
import { compileTool } from "./tool-compiler";
import type { OxAgent } from "/pkg/ox_web.js";
import type { ToolDef } from "./types";

export let activeJsTools = new Set<string>();

export function applyProfile(agent: OxAgent, profileName: string): void {
  for (const name of activeJsTools) {
    agent.unregister_tool(name);
  }
  activeJsTools.clear();
  const toolNames = ToolStore.getProfileTools(profileName);
  const lib = ToolStore.loadLibrary();
  for (const name of toolNames) {
    const def = lib[name];
    if (!def) continue;
    try {
      const { schemaJson, callback } = compileTool(def);
      agent.register_tool(name, def.description, schemaJson, callback);
      activeJsTools.add(name);
    } catch (_) {
      /* skip tools that fail to compile */
    }
  }
  ToolStore.setActiveProfile(profileName);
}

export function refreshToolPanel(agent: OxAgent): void {
  toolPanelEl.textContent = "";
  const activeProfile = ToolStore.getActiveProfile();
  const isFactory = ToolStore.isFactory(activeProfile);
  const lib = ToolStore.loadLibrary();
  const profileTools = ToolStore.getProfileTools(activeProfile);

  // Profile row
  const profileRow = document.createElement("div");
  profileRow.className = "profile-row";
  const select = document.createElement("select");
  for (const pname of ToolStore.profileNames()) {
    const opt = document.createElement("option");
    opt.value = pname;
    opt.textContent = pname;
    if (pname === activeProfile) opt.selected = true;
    select.appendChild(opt);
  }
  select.addEventListener("change", () => {
    applyProfile(agent, select.value);
    refreshToolPanel(agent);
  });
  profileRow.appendChild(select);

  const newBtn = document.createElement("button");
  newBtn.className = "edit-btn";
  newBtn.textContent = "new";
  newBtn.addEventListener("click", () => {
    showNewProfileInput(agent, profileRow);
  });
  profileRow.appendChild(newBtn);

  const delBtn = document.createElement("button");
  delBtn.className = "edit-btn";
  delBtn.textContent = "del";
  delBtn.disabled = isFactory;
  delBtn.addEventListener("click", () => {
    if (isFactory) return;
    ToolStore.deleteProfile(activeProfile);
    const fallback = ToolStore.profileNames()[0];
    applyProfile(agent, fallback);
    refreshToolPanel(agent);
  });
  profileRow.appendChild(delBtn);

  if (!isFactory) {
    const resetBtn = document.createElement("button");
    resetBtn.className = "edit-btn";
    resetBtn.textContent = "reset";
    resetBtn.addEventListener("click", () => {
      for (const name of activeJsTools) {
        agent.unregister_tool(name);
      }
      activeJsTools.clear();
      ToolStore.setProfileTools(activeProfile, []);
      refreshToolPanel(agent);
    });
    profileRow.appendChild(resetBtn);
  }

  toolPanelEl.appendChild(profileRow);

  // Divider
  const hr1 = document.createElement("hr");
  hr1.className = "tool-panel-divider";
  toolPanelEl.appendChild(hr1);

  // Tool library list
  const toolNames = isFactory
    ? BUILTIN_TOOLS.map((t) => t.name)
    : Object.keys(lib).sort();

  // We need prefillAddForm to be available for copy buttons, so declare it early.
  // It will reference addDetails and inputs which are created later (only on non-factory).
  let prefillAddForm: ((srcDef: ToolDef) => void) | null = null;

  if (toolNames.length > 0) {
    const listDiv = document.createElement("div");
    listDiv.className = "tool-library-list";
    for (const name of toolNames) {
      const isBuiltin = ToolStore.isBuiltin(name);
      const wrapper = document.createElement("div");
      const row = document.createElement("div");
      row.className = "tool-library-row";
      const cb = document.createElement("input");
      cb.type = "checkbox";
      cb.checked = profileTools.includes(name);
      cb.disabled = isFactory;
      if (!isFactory) {
        cb.addEventListener("change", () => {
          if (cb.checked) {
            const def = lib[name];
            try {
              const { schemaJson, callback } = compileTool(def);
              agent.register_tool(name, def.description, schemaJson, callback);
              activeJsTools.add(name);
              const updated = ToolStore.getProfileTools(activeProfile);
              if (!updated.includes(name)) updated.push(name);
              ToolStore.setProfileTools(activeProfile, updated);
            } catch (_) {
              cb.checked = false;
            }
          } else {
            agent.unregister_tool(name);
            activeJsTools.delete(name);
            const updated = ToolStore.getProfileTools(activeProfile).filter(
              (n) => n !== name,
            );
            ToolStore.setProfileTools(activeProfile, updated);
          }
        });
      }
      row.appendChild(cb);
      const nameSpan = document.createElement("span");
      nameSpan.className = "tool-lib-name";
      nameSpan.textContent = name;
      row.appendChild(nameSpan);
      if (!isFactory) {
        const copyToolBtn = document.createElement("button");
        copyToolBtn.className = "tool-lib-del";
        copyToolBtn.textContent = "copy";
        copyToolBtn.addEventListener("click", () => {
          if (prefillAddForm) prefillAddForm(lib[name]);
        });
        row.appendChild(copyToolBtn);
        if (!isBuiltin) {
          const editToolBtn = document.createElement("button");
          editToolBtn.className = "tool-lib-del";
          editToolBtn.textContent = "edit";
          editToolBtn.addEventListener("click", () => {
            showToolEditForm(agent, wrapper, lib[name]);
          });
          row.appendChild(editToolBtn);
          const delToolBtn = document.createElement("button");
          delToolBtn.className = "tool-lib-del";
          delToolBtn.textContent = "del";
          delToolBtn.addEventListener("click", () => {
            if (activeJsTools.has(name)) {
              agent.unregister_tool(name);
              activeJsTools.delete(name);
            }
            ToolStore.deleteTool(name);
            refreshToolPanel(agent);
          });
          row.appendChild(delToolBtn);
        }
      }
      wrapper.appendChild(row);
      listDiv.appendChild(wrapper);
    }
    toolPanelEl.appendChild(listDiv);
  } else {
    const emptyDiv = document.createElement("div");
    emptyDiv.className = "tool-panel-empty";
    emptyDiv.textContent = "no tools in library";
    toolPanelEl.appendChild(emptyDiv);
  }

  // Add form — always visible
  const hr2 = document.createElement("hr");
  hr2.className = "tool-panel-divider";
  toolPanelEl.appendChild(hr2);

  const addDetails = document.createElement("details");
  const addSummary = document.createElement("summary");
  addSummary.textContent = "add new tool...";
  addSummary.style.cursor = "pointer";
  addDetails.appendChild(addSummary);

  const formDiv = document.createElement("div");
  formDiv.style.paddingTop = "8px";

  const fields = [
    {
      id: "add-tool-name",
      label: "name",
      placeholder: "my_tool",
      tag: "input",
    },
    {
      id: "add-tool-desc",
      label: "description",
      placeholder: "What this tool does",
      tag: "input",
    },
    {
      id: "add-tool-params",
      label: "parameters (TypeScript style)",
      placeholder: "(text: string, count?: number)",
      tag: "input",
    },
    {
      id: "add-tool-body",
      label: "function body (params available by name; return a string)",
      placeholder: "return String(n + 2);",
      tag: "textarea",
    },
  ];
  const inputs: Record<string, HTMLInputElement | HTMLTextAreaElement> = {};
  for (const f of fields) {
    const fieldDiv = document.createElement("div");
    fieldDiv.className = "tool-form-field";
    const lbl = document.createElement("label");
    lbl.textContent = f.label;
    fieldDiv.appendChild(lbl);
    const el = document.createElement(f.tag) as
      | HTMLInputElement
      | HTMLTextAreaElement;
    el.placeholder = f.placeholder;
    if (f.tag === "textarea") (el as HTMLTextAreaElement).rows = 4;
    fieldDiv.appendChild(el);
    formDiv.appendChild(fieldDiv);
    inputs[f.id] = el;
  }

  const errorEl = document.createElement("div");
  errorEl.className = "tool-form-error";

  const actionsDiv = document.createElement("div");
  actionsDiv.className = "tool-form-actions";

  function validateForm(): ToolDef | null {
    errorEl.textContent = "";
    const name = inputs["add-tool-name"].value.trim();
    const description = inputs["add-tool-desc"].value.trim();
    const params = inputs["add-tool-params"].value.trim();
    const body = inputs["add-tool-body"].value.trim();
    if (!name || !description || !params || !body) {
      errorEl.textContent = "All fields are required.";
      return null;
    }
    const def: ToolDef = { name, description, params, body };
    try {
      compileTool(def);
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : String(e);
      errorEl.textContent = "Invalid: " + msg;
      return null;
    }
    return def;
  }

  function saveToolToProfile(def: ToolDef, targetProfile: string): void {
    ToolStore.saveTool(def);
    try {
      const { schemaJson, callback } = compileTool(def);
      agent.register_tool(def.name, def.description, schemaJson, callback);
      activeJsTools.add(def.name);
      const updated = ToolStore.getProfileTools(targetProfile);
      if (!updated.includes(def.name)) updated.push(def.name);
      ToolStore.setProfileTools(targetProfile, updated);
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : String(e);
      errorEl.textContent = "Saved to library but registration failed: " + msg;
      refreshToolPanel(agent);
      return;
    }
    refreshToolPanel(agent);
  }

  function showSaveActions(): void {
    actionsDiv.textContent = "";
    const saveBtn = document.createElement("button");
    saveBtn.className = "save-btn";
    saveBtn.textContent = "save";
    saveBtn.addEventListener("click", () => {
      const def = validateForm();
      if (!def) return;
      if (ToolStore.isFactory(ToolStore.getActiveProfile())) {
        showProfileStep(def);
      } else {
        saveToolToProfile(def, ToolStore.getActiveProfile());
      }
    });
    actionsDiv.appendChild(saveBtn);
  }

  function showProfileStep(def: ToolDef): void {
    actionsDiv.textContent = "";
    errorEl.textContent = "";

    const hint = document.createElement("div");
    hint.className = "tool-form-field";
    const hintLabel = document.createElement("label");
    hintLabel.textContent = "new profile name";
    hint.appendChild(hintLabel);
    const nameInput = document.createElement("input");
    nameInput.type = "text";
    nameInput.placeholder = "my profile";
    hint.appendChild(nameInput);
    actionsDiv.appendChild(hint);

    const btnRow = document.createElement("div");
    btnRow.className = "tool-form-actions";
    const createBtn = document.createElement("button");
    createBtn.className = "save-btn";
    createBtn.textContent = "create & save";
    createBtn.addEventListener("click", () => {
      const profileName = nameInput.value.trim();
      if (!profileName || profileName === FACTORY_PROFILE) {
        nameInput.style.borderColor = "var(--vermillion)";
        return;
      }
      ToolStore.createProfile(profileName);
      applyProfile(agent, profileName);
      saveToolToProfile(def, profileName);
    });
    btnRow.appendChild(createBtn);

    const cancelBtn = document.createElement("button");
    cancelBtn.className = "cancel-btn";
    cancelBtn.textContent = "cancel";
    cancelBtn.addEventListener("click", () => showSaveActions());
    btnRow.appendChild(cancelBtn);
    actionsDiv.appendChild(btnRow);

    nameInput.focus();
    nameInput.addEventListener("keydown", (e: KeyboardEvent) => {
      if (e.key === "Enter") createBtn.click();
      if (e.key === "Escape") cancelBtn.click();
    });
  }

  showSaveActions();
  formDiv.appendChild(actionsDiv);
  formDiv.appendChild(errorEl);
  addDetails.appendChild(formDiv);
  toolPanelEl.appendChild(addDetails);

  // Wire up prefillAddForm now that addDetails and inputs exist
  prefillAddForm = (srcDef: ToolDef) => {
    let copyName = srcDef.name + "_copy";
    while (lib[copyName]) copyName += "_copy";
    inputs["add-tool-name"].value = copyName;
    inputs["add-tool-desc"].value = srcDef.description;
    inputs["add-tool-params"].value = srcDef.params;
    inputs["add-tool-body"].value = srcDef.body;
    addDetails.open = true;
    inputs["add-tool-name"].focus();
    (inputs["add-tool-name"] as HTMLInputElement).select();
  };
}

function showToolEditForm(
  agent: OxAgent,
  wrapper: HTMLElement,
  def: ToolDef,
): void {
  const existing = wrapper.querySelector(".tool-edit-form");
  if (existing) {
    existing.remove();
    return;
  }

  const form = document.createElement("div");
  form.className = "tool-edit-form";
  form.style.paddingLeft = "22px";
  form.style.paddingTop = "4px";
  form.style.paddingBottom = "4px";

  const editFields = [
    {
      key: "description",
      label: "description",
      val: def.description,
      tag: "input",
    },
    { key: "params", label: "parameters", val: def.params, tag: "input" },
    { key: "body", label: "body", val: def.body, tag: "textarea" },
  ];
  const editInputs: Record<string, HTMLInputElement | HTMLTextAreaElement> = {};
  for (const f of editFields) {
    const fieldDiv = document.createElement("div");
    fieldDiv.className = "tool-form-field";
    const lbl = document.createElement("label");
    lbl.textContent = f.label;
    fieldDiv.appendChild(lbl);
    const el = document.createElement(f.tag) as
      | HTMLInputElement
      | HTMLTextAreaElement;
    el.value = f.val;
    if (f.tag === "textarea") (el as HTMLTextAreaElement).rows = 3;
    fieldDiv.appendChild(el);
    form.appendChild(fieldDiv);
    editInputs[f.key] = el;
  }

  const errEl = document.createElement("div");
  errEl.className = "tool-form-error";

  const actions = document.createElement("div");
  actions.className = "edit-actions";
  const saveBtn = document.createElement("button");
  saveBtn.className = "save-btn";
  saveBtn.textContent = "save";
  saveBtn.addEventListener("click", () => {
    errEl.textContent = "";
    const updated: ToolDef = {
      name: def.name,
      description: editInputs.description.value.trim(),
      params: editInputs.params.value.trim(),
      body: editInputs.body.value.trim(),
    };
    if (!updated.description || !updated.params || !updated.body) {
      errEl.textContent = "All fields are required.";
      return;
    }
    try {
      compileTool(updated);
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : String(e);
      errEl.textContent = "Invalid: " + msg;
      return;
    }
    ToolStore.saveTool(updated);
    if (activeJsTools.has(def.name)) {
      const { schemaJson, callback } = compileTool(updated);
      agent.unregister_tool(def.name);
      agent.register_tool(def.name, updated.description, schemaJson, callback);
    }
    refreshToolPanel(agent);
  });
  const cancelBtn = document.createElement("button");
  cancelBtn.className = "cancel-btn";
  cancelBtn.textContent = "cancel";
  cancelBtn.addEventListener("click", () => form.remove());
  actions.appendChild(saveBtn);
  actions.appendChild(cancelBtn);
  form.appendChild(actions);
  form.appendChild(errEl);
  wrapper.appendChild(form);
}

function showNewProfileInput(agent: OxAgent, profileRow: HTMLElement): void {
  const container = profileRow.parentElement!;
  const inputRow = document.createElement("div");
  inputRow.className = "profile-row";
  const nameInput = document.createElement("input");
  nameInput.type = "text";
  nameInput.className = "profile-name-input";
  nameInput.placeholder = "profile name";
  inputRow.appendChild(nameInput);
  const okBtn = document.createElement("button");
  okBtn.className = "save-btn";
  okBtn.textContent = "ok";
  okBtn.style.fontSize = "11px";
  okBtn.style.padding = "2px 8px";
  okBtn.style.fontWeight = "normal";
  okBtn.addEventListener("click", () => {
    const name = nameInput.value.trim();
    if (!name || name === FACTORY_PROFILE) {
      nameInput.style.borderColor = "var(--vermillion)";
      return;
    }
    ToolStore.createProfile(name);
    applyProfile(agent, name);
    refreshToolPanel(agent);
  });
  inputRow.appendChild(okBtn);
  const cancelBtn = document.createElement("button");
  cancelBtn.className = "cancel-btn";
  cancelBtn.textContent = "cancel";
  cancelBtn.style.fontSize = "11px";
  cancelBtn.style.padding = "2px 8px";
  cancelBtn.style.fontWeight = "normal";
  cancelBtn.addEventListener("click", () => refreshToolPanel(agent));
  inputRow.appendChild(cancelBtn);
  container.replaceChild(inputRow, profileRow);
  nameInput.focus();
  nameInput.addEventListener("keydown", (e: KeyboardEvent) => {
    if (e.key === "Enter") okBtn.click();
    if (e.key === "Escape") cancelBtn.click();
  });
}
