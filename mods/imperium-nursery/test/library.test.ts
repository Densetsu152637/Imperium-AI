// Pet packages as the nursery loads them, and the strip cache with a fake host.

import { test } from "node:test";

import type { PetPackage } from "@imperium/mod-sdk";

import { PetLibrary, loadPet } from "../src/library.ts";
import { expect } from "./expect.ts";

const pkg: PetPackage = {
  id: "tiny",
  displayName: "Tiny",
  description: "",
  author: "",
  license: "MIT",
  cellWidth: 16,
  cellHeight: 16,
  petJson: JSON.stringify({ id: "tiny", spriteVersionNumber: 3, cell: { width: 16, height: 16 } }),
  drawn: ["idle", "working"],
};

test("a package loads with its catalog and drawn animations; a broken pet.json still loads", () => {
  const pet = loadPet(pkg);
  expect(pet.catalog.working?.drawn).toBe(true);
  expect(pet.catalog.jumping?.drawn).toBe(false);
  const broken = loadPet({ ...pkg, petJson: "{", cellWidth: 32, cellHeight: 24 });
  expect(broken.cell).toEqual({ width: 32, height: 24 });
  expect(broken.catalog.idle?.drawn).toBe(true);
});

test("strips are fetched once per pet and animation, and a refusal is remembered", async () => {
  const asked: string[] = [];
  let settled = 0;
  const lib = new PetLibrary(
    {
      list: async () => [pkg],
      strip: async (id, animation) => {
        asked.push(`${id}/${animation}`);
        if (animation === "working") throw new Error("not drawn");
        return `data:image/png;base64,${animation}`;
      },
    },
    () => settled++,
  );
  await lib.load();
  expect(lib.ids()).toEqual(["tiny"]);
  expect(lib.strip("tiny", "idle")).toBeUndefined();
  expect(lib.strip("tiny", "idle")).toBeUndefined();
  expect(lib.strip("tiny", "working")).toBeUndefined();
  await new Promise((resolve) => setTimeout(resolve, 5));
  expect(lib.strip("tiny", "idle")).toBe("data:image/png;base64,idle");
  expect(lib.failed("tiny", "working")).toBe(true);
  expect(lib.failed("tiny", "idle")).toBe(false);
  expect(asked).toEqual(["tiny/idle", "tiny/working"]);
  expect(settled).toBe(2);
});
