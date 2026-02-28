const STORAGE_KEY = "ox:api-key";

export function getStoredApiKey(): string | null {
  return sessionStorage.getItem(STORAGE_KEY);
}

export function storeApiKey(key: string): void {
  sessionStorage.setItem(STORAGE_KEY, key);
}

export function clearApiKey(): void {
  sessionStorage.removeItem(STORAGE_KEY);
}

/** Show a modal overlay prompting the user for their Anthropic API key. */
export function showApiKeyPrompt(): Promise<string | null> {
  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.className = "api-key-overlay";

    const dialog = document.createElement("div");
    dialog.className = "api-key-dialog";

    dialog.innerHTML = `
      <h3>Anthropic API Key</h3>
      <p>Enter your Anthropic API key to connect. Your key is stored in sessionStorage and never sent to any server other than api.anthropic.com.</p>
      <input type="password" class="api-key-input" placeholder="sk-ant-..." autocomplete="off" spellcheck="false" />
      <div class="api-key-actions">
        <button class="save-btn api-key-submit">Connect</button>
        <button class="cancel-btn api-key-cancel">Cancel</button>
      </div>
    `;

    overlay.appendChild(dialog);
    document.body.appendChild(overlay);

    const input = dialog.querySelector<HTMLInputElement>(".api-key-input")!;
    const submitBtn =
      dialog.querySelector<HTMLButtonElement>(".api-key-submit")!;
    const cancelBtn =
      dialog.querySelector<HTMLButtonElement>(".api-key-cancel")!;

    function cleanup(result: string | null) {
      overlay.remove();
      resolve(result);
    }

    function submit() {
      const key = input.value.trim();
      if (key) {
        cleanup(key);
      }
    }

    submitBtn.addEventListener("click", submit);
    input.addEventListener("keydown", (e: KeyboardEvent) => {
      if (e.key === "Enter") submit();
    });
    cancelBtn.addEventListener("click", () => cleanup(null));

    // Focus the input after a frame so the overlay is rendered
    requestAnimationFrame(() => input.focus());
  });
}
