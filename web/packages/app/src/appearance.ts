type Appearance = "system" | "light" | "dark";

const STORAGE_KEY = "luvus.web.appearance";
const NEXT: Record<Appearance, Appearance> = { system: "light", light: "dark", dark: "system" };

/** Browser-local presentation only; never changes the shared terminal owner. */
export class BrowserAppearance extends EventTarget {
  readonly #system = matchMedia("(prefers-color-scheme: light)");
  #preference = this.#read();

  constructor() {
    super();
    this.#apply();
    this.#system.addEventListener("change", () => {
      if (this.#preference === "system") this.#apply();
    });
    window.addEventListener("storage", (event) => {
      if (event.key !== STORAGE_KEY && event.key !== null) return;
      // Ignore changes in sessionStorage. Access can throw in restricted browsers.
      try { if (event.storageArea !== localStorage) return; } catch { return; }
      this.#preference = this.#read();
      this.#apply();
    });
  }

  get preference(): Appearance { return this.#preference; }

  get theme(): "light" | "dark" {
    return this.#preference === "system" ? (this.#system.matches ? "light" : "dark") : this.#preference;
  }

  get label(): string {
    const title = (value: Appearance) => value[0]!.toUpperCase() + value.slice(1);
    const current = title(this.#preference) + (this.#preference === "system" ? ` (${this.theme})` : "");
    return `Appearance: ${current}. Switch to ${title(NEXT[this.#preference])}`;
  }

  cycle(): void {
    this.#preference = NEXT[this.#preference];
    try { localStorage.setItem(STORAGE_KEY, this.#preference); } catch { /* Still usable for this page. */ }
    this.#apply();
  }

  #read(): Appearance {
    try {
      const stored = localStorage.getItem(STORAGE_KEY);
      if (stored === "light" || stored === "dark") return stored;
    } catch { /* Follow the system if browser storage is unavailable. */ }
    return "system";
  }

  #apply(): void {
    document.documentElement.dataset.theme = this.theme;
    document.documentElement.dataset.appearance = this.#preference;
    document.querySelector('meta[name="theme-color"]')?.setAttribute("content", this.theme === "light" ? "#f5f6f8" : "#111219");
    this.dispatchEvent(new Event("change"));
  }
}
