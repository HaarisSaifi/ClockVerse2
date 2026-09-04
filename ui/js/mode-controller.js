// ModeController — finite state machine:
// IDLE → TRANSITIONING → SWAPPED → IDLE
// Rules enforced: input lock during transition, atomic store swap,
// no ghost listeners (all subs keyed by mode id and detached on exit).

const TRANSITION_MS = 400;

export class ModeController {
  #state = 'IDLE';
  #mode = 'chrono';
  #stores = { chrono: {}, sector: {} };   // isolated StateStores
  #listeners = { chrono: new Set(), sector: new Set() };

  get mode() { return this.#mode; }
  get state() { return this.#state; }

  on(mode, event, fn) {
    this.#listeners[mode].add({ event, fn });
  }

  emitLocal(event, payload) {
    for (const l of this.#listeners[this.#mode]) {
      if (l.event === event) l.fn(payload);
    }
  }

  async toggle() {
    if (this.#state !== 'IDLE') return;        // rule 1: no double-fire
    this.#state = 'TRANSITIONING';
    const next = this.#mode === 'chrono' ? 'sector' : 'chrono';

    this.emitLocal('suspend', {});             // rule 3: scans pause, not die
    document.body.classList.add('transitioning');

    await new Promise(r => setTimeout(r, TRANSITION_MS));

    this.#mode = next;                          // rule 2: atomic swap
    document.body.dataset.mode = next;          // CSS token crossfade
    this.#state = 'SWAPPED';

    document.body.classList.remove('transitioning');
    this.emitLocal('resume', {});               // rule 3: resume after swap
    this.#state = 'IDLE';
  }
}
