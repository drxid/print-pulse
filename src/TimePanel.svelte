<script lang="ts">
  // Compact inline time picker: hours and minutes as small editable fields
  // with ‹ › steppers on either side. Type digits directly, or use the arrows
  // (they repeat while held), the ↑/↓ keys or the mouse wheel; minutes step by
  // 5. Enter confirms, Esc restores the original time. The parent animates the
  // container it sits in (expanding in place, since the window is sized to its
  // content and a popover would be clipped).
  let {
    value,
    hoursLabel,
    minutesLabel,
    doneLabel,
    onpick,
  }: {
    /** Minutes after midnight. */
    value: number;
    hoursLabel: string;
    minutesLabel: string;
    doneLabel: string;
    /** `done` is true when the user confirms — the panel can close then. */
    onpick: (value: number, done: boolean) => void;
  } = $props();

  type Part = "h" | "m";
  const pad = (n: number) => String(n).padStart(2, "0");

  // The panel owns the value while it is open, so held arrows and fast wheel
  // spins never wait for (or get rewound by) the backend round-trip.
  const initial = () => value % (24 * 60);
  const original = initial();
  let current = $state(original);
  const hours = $derived(Math.floor(current / 60));
  const minutes = $derived(current % 60);

  // What is typed into each field; `null` means "show the current value".
  let draft = $state<Record<Part, string | null>>({ h: null, m: null });
  let minuteInput = $state<HTMLInputElement>();

  function set(h: number, m: number) {
    current = h * 60 + m;
    onpick(current, false);
  }

  function step(part: Part, dir: 1 | -1) {
    commit(part);
    if (part === "h") {
      set((hours + dir + 24) % 24, minutes);
    } else {
      // Snap to the 5-minute grid first, then move along it.
      const snapped = dir > 0 ? Math.floor(minutes / 5) * 5 + 5 : Math.ceil(minutes / 5) * 5 - 5;
      set(hours, (snapped + 60) % 60);
    }
  }

  /** Apply whatever was typed into a field (clamped), then show it formatted. */
  function commit(part: Part) {
    const text = draft[part];
    draft[part] = null;
    if (text === null || text === "") return;
    const n = Number(text);
    if (part === "h") set(Math.min(n, 23), minutes);
    else set(hours, Math.min(n, 59));
  }

  function onInput(part: Part, e: Event) {
    const el = e.currentTarget as HTMLInputElement;
    const digits = el.value.replace(/\D/g, "").slice(0, 2);
    el.value = digits;
    draft[part] = digits;
    // A full hour (or one that can't take a second digit) jumps to minutes.
    if (part === "h" && (digits.length === 2 || Number(digits) > 2)) {
      commit("h");
      minuteInput?.focus();
    }
  }

  function onKey(part: Part, e: KeyboardEvent) {
    if (e.key === "ArrowUp" || e.key === "ArrowDown") {
      e.preventDefault();
      step(part, e.key === "ArrowUp" ? 1 : -1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      confirm();
    } else if (e.key === "Escape") {
      e.preventDefault();
      draft = { h: null, m: null };
      current = original;
      onpick(original, true);
    }
  }

  function confirm() {
    commit("h");
    commit("m");
    onpick(current, true);
  }

  let timer: ReturnType<typeof setTimeout> | null = null;
  function hold(part: Part, dir: 1 | -1) {
    step(part, dir);
    const repeat = (delay: number) => {
      timer = setTimeout(() => {
        step(part, dir);
        repeat(80);
      }, delay);
    };
    repeat(400);
  }
  function release() {
    if (timer) clearTimeout(timer);
    timer = null;
  }

  function wheel(part: Part) {
    return (e: WheelEvent) => {
      e.preventDefault();
      step(part, e.deltaY < 0 ? 1 : -1);
    };
  }
</script>

<svelte:window onpointerup={release} onblur={release} />

{#snippet stepper(part: Part, label: string, shown: number)}
  <div class="stepper" title={label} onwheel={wheel(part)}>
    <button class="arrow" tabindex="-1" aria-label="−" onpointerdown={() => hold(part, -1)} onpointerleave={release}>
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
        <path d="M15 18l-6-6 6-6" />
      </svg>
    </button>
    {#if part === "m"}
      <input
        class="num"
        bind:this={minuteInput}
        aria-label={label}
        inputmode="numeric"
        maxlength="2"
        value={draft.m ?? pad(shown)}
        onfocus={(e) => e.currentTarget.select()}
        oninput={(e) => onInput("m", e)}
        onkeydown={(e) => onKey("m", e)}
        onblur={() => commit("m")}
      />
    {:else}
      <input
        class="num"
        aria-label={label}
        inputmode="numeric"
        maxlength="2"
        value={draft.h ?? pad(shown)}
        onfocus={(e) => e.currentTarget.select()}
        oninput={(e) => onInput("h", e)}
        onkeydown={(e) => onKey("h", e)}
        onblur={() => commit("h")}
      />
    {/if}
    <button class="arrow" tabindex="-1" aria-label="+" onpointerdown={() => hold(part, 1)} onpointerleave={release}>
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
        <path d="M9 18l6-6-6-6" />
      </svg>
    </button>
  </div>
{/snippet}

<div class="tp">
  {@render stepper("h", hoursLabel, hours)}
  <span class="colon">:</span>
  {@render stepper("m", minutesLabel, minutes)}
  <button class="done" onclick={confirm}>{doneLabel}</button>
</div>

<style>
  .tp {
    /* Times read left to right in every language, so keep ‹ − · + › in place. */
    direction: ltr;
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
  }
  .stepper {
    display: flex;
    align-items: center;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 9px;
    padding: 2px;
    transition: border-color 0.15s ease;
  }
  .stepper:focus-within {
    border-color: var(--accent);
  }
  .arrow {
    width: 26px;
    height: 26px;
    display: flex;
    align-items: center;
    justify-content: center;
    border: none;
    border-radius: 7px;
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
    touch-action: none;
    transition: background 0.12s ease, color 0.12s ease;
  }
  .arrow:hover {
    background: var(--card-hover);
    color: var(--text);
  }
  .arrow:active {
    color: var(--accent);
  }
  .num {
    width: 30px;
    padding: 2px 0;
    border: none;
    border-radius: 5px;
    background: transparent;
    text-align: center;
    font: inherit;
    font-size: 15px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--text);
    cursor: text;
    user-select: text;
    -webkit-user-select: text;
  }
  .num:focus-visible {
    outline: none;
  }
  .num::selection {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .colon {
    font-size: 15px;
    font-weight: 600;
    color: var(--text-dim);
  }
  .done {
    margin-inline-start: auto;
    background: var(--accent-soft);
    border: 1px solid var(--accent);
    color: var(--accent);
    border-radius: 8px;
    padding: 6px 12px;
    font: inherit;
    font-size: 12.5px;
    font-weight: 600;
    cursor: pointer;
  }
  .done:hover {
    filter: brightness(1.1);
  }
</style>
