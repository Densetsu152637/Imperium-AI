// Steers free pets with a WebAssembly behaviour, one running instance per pet. Pure: the host's
// `behaviours.load` is injected, and a behaviour that is refused or stops (trap, too slow, invalid
// answer) just stops steering that pet, which goes back to the built-in roaming; the nursery never
// depends on it. The host reports the fault to the person; the director keeps a short note too.

import type { Behaviour, BehaviourState, Behaviours } from "@imperium/mod-sdk"

import { hash } from "../pick.ts"

/** How often a pet's behaviour runs, in ms; between ticks the last velocity holds. */
export const TICK_MS = 250

/** After the host says it is full, how long before the director asks again (a freed slot asks sooner). */
export const RETRY_MS = 5000

export type Guide = { vx: number; vy: number; shot: string | null }

/** What the room tells the director about a free pet. */
export type PetState = Omit<BehaviourState, "dt" | "free" | "busy">

type Slot = { behaviour: Behaviour; lastAt: number; vx: number; vy: number }

export class Director {
  private readonly host: Pick<Behaviours, "load">
  private readonly bytes: Uint8Array
  private readonly name: string
  private readonly slots = new Map<string, Slot>()
  private readonly failed = new Set<string>()
  /** Do not ask the host to load before this time (it was full), or 0. */
  private retryAt = 0
  /** Why the director cannot run at all (the module was refused), or null. */
  refused: string | null = null
  /** One line per behaviour that stopped, newest last. */
  readonly faults: string[] = []

  constructor(host: Pick<Behaviours, "load">, name: string, bytes: Uint8Array) {
    this.host = host
    this.name = name
    this.bytes = bytes
  }

  /** Whether any pet can still be steered. */
  get active(): boolean {
    return this.refused === null
  }

  /**
   * How pet `key` should move now, or null for the built-in roaming (not steered, or its
   * behaviour is stopped). A behaviour that is busy-pet-aware is told so but the room only asks
   * for free pets.
   */
  guide(key: string, state: PetState, now: number): Guide | null {
    if (this.refused !== null || this.failed.has(key)) return null
    let slot = this.slots.get(key)
    if (!slot) {
      if (now < this.retryAt) return null
      const loaded = this.host.load(`${this.name}:${key}`, this.bytes, { seed: hash(key) })
      if (!loaded.ok) {
        // Too many behaviours running: this pet roams the built-in way for now; the module is fine.
        if (loaded.busy) {
          this.retryAt = now + RETRY_MS
          return null
        }
        // A module that was refused is refused for every pet; stop trying.
        this.refused = loaded.error
        this.faults.push(`${this.name}: ${loaded.error}`)
        return null
      }
      slot = { behaviour: loaded.behaviour, lastAt: now - TICK_MS, vx: 0, vy: 0 }
      this.slots.set(key, slot)
    }
    if (now - slot.lastAt < TICK_MS) return { vx: slot.vx, vy: slot.vy, shot: null }
    const dt = Math.min(1000, Math.max(0, now - slot.lastAt))
    slot.lastAt = now
    const result = slot.behaviour.tick({ ...state, dt, free: true, busy: false })
    if (!result.ok) {
      this.stop(key, result.error)
      return null
    }
    slot.vx = result.vx
    slot.vy = result.vy
    return { vx: result.vx, vy: result.vy, shot: result.shot }
  }

  /** The pet left the nursery: free its instance. */
  forget(key: string): void {
    this.slots.get(key)?.behaviour.close()
    this.retryAt = 0
    this.slots.delete(key)
    this.failed.delete(key)
  }

  /** Stops every instance (the behaviour was switched off). */
  clear(): void {
    for (const key of [...this.slots.keys()]) this.forget(key)
    this.failed.clear()
  }

  private stop(key: string, reason: string): void {
    this.faults.push(`${this.name}:${key}: ${reason}`)
    if (this.faults.length > 20) this.faults.shift()
    this.forget(key)
    this.failed.add(key)
  }
}
