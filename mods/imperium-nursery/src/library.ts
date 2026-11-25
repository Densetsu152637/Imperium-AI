// The installed pets as the nursery uses them: each package's catalog (which animations it
// draws) and a cache of strip images, fetched only for animations actually shown.

import type { ModContext, PetPackage } from "@imperium/mod-sdk";

import { parsePetJson, standardCatalog, withDrawn } from "./pet.ts";
import type { Catalog } from "./pet.ts";

export type LoadedPet = {
  id: string;
  name: string;
  cell: { width: number; height: number };
  catalog: Catalog;
};

/** A package as the nursery draws it: its pet.json catalog, `drawn` from the package's list. */
export function loadPet(pkg: PetPackage): LoadedPet {
  const parsed = parsePetJson(pkg.petJson, pkg.id);
  const base = parsed.ok ? parsed.pet : null;
  return {
    id: pkg.id,
    name: pkg.displayName || pkg.id,
    cell: base?.cell ?? { width: pkg.cellWidth, height: pkg.cellHeight },
    catalog: withDrawn(base?.catalog ?? standardCatalog(), new Set(pkg.drawn)),
  };
}

export class PetLibrary {
  pets: LoadedPet[] = [];
  // A strip's data URL, or null once the host refused it (so it is not asked for again).
  private strips = new Map<string, string | null>();
  private pending = new Set<string>();

  private api: ModContext["pets"];
  private onStrip: () => void;

  constructor(api: ModContext["pets"], onStrip: () => void = () => {}) {
    this.api = api;
    this.onStrip = onStrip;
  }

  async load(): Promise<void> {
    const packages = await this.api.list();
    this.pets = packages.map(loadPet);
  }

  get(id: string | null): LoadedPet | undefined {
    return id === null ? undefined : this.pets.find((p) => p.id === id);
  }

  ids(): string[] {
    return this.pets.map((p) => p.id);
  }

  /** The strip's URL if it is here; otherwise starts fetching it and returns undefined. */
  strip(petId: string, animation: string): string | undefined {
    const key = `${petId}/${animation}`;
    const hit = this.strips.get(key);
    if (hit !== undefined) return hit ?? undefined;
    if (!this.pending.has(key)) {
      this.pending.add(key);
      this.api.strip(petId, animation).then(
        (url) => this.settle(key, url),
        () => this.settle(key, null),
      );
    }
    return undefined;
  }

  /** Whether the host refused this strip. */
  failed(petId: string, animation: string): boolean {
    return this.strips.get(`${petId}/${animation}`) === null;
  }

  private settle(key: string, url: string | null): void {
    this.pending.delete(key);
    this.strips.set(key, url);
    this.onStrip();
  }
}
