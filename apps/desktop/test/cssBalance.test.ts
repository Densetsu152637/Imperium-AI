// Guards against unclosed or stray braces in the desktop stylesheets: one missing `}` silently
// drops every rule after it. Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const srcDir = fileURLToPath(new URL("../src", import.meta.url));

function cssFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) =>
    entry.isDirectory() ? cssFiles(join(dir, entry.name)) : entry.name.endsWith(".css") ? [join(dir, entry.name)] : [],
  );
}

/** Describes the first brace problem (a stray `}` or a block never closed), or null when balanced. */
function checkBalance(css: string): string | null {
  let depth = 0;
  let line = 1;
  const openedAt: number[] = [];
  for (let i = 0; i < css.length; i++) {
    const ch = css[i];
    if (ch === "\n") {
      line++;
    } else if (ch === "/" && css[i + 1] === "*") {
      const end = css.indexOf("*/", i + 2);
      const stop = end === -1 ? css.length : end + 2;
      line += css.slice(i, stop).split("\n").length - 1;
      i = stop - 1;
    } else if (ch === '"' || ch === "'") {
      i++;
      while (i < css.length && css[i] !== ch && css[i] !== "\n") {
        if (css[i] === "\\") i++;
        i++;
      }
    } else if (ch === "{") {
      depth++;
      openedAt.push(line);
    } else if (ch === "}") {
      depth--;
      openedAt.pop();
      if (depth < 0) return `stray "}" at line ${line}`;
    }
  }
  return depth === 0 ? null : `${depth} unclosed "{", the last opened at line ${openedAt[openedAt.length - 1]}`;
}

test("the checker spots stray and unclosed braces and ignores comments and strings", () => {
  assert.equal(checkBalance("a { b: c; }"), null);
  assert.equal(checkBalance('/* { */ a::after { content: "}{"; }'), null);
  assert.match(checkBalance("a { b: c;") ?? "", /unclosed/);
  assert.match(checkBalance("a { } }") ?? "", /stray/);
});

test("every stylesheet under src has balanced braces", () => {
  const files = cssFiles(srcDir);
  assert.ok(files.length > 0, "no .css files found");
  for (const file of files) {
    assert.equal(checkBalance(readFileSync(file, "utf8")), null, file);
  }
});
