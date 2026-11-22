// Style guards (docs/plans/ui-polish.md, guard tests 2 to 4): every stylesheet declares how many
// literal colours, font sizes, font weights, radii, shadows, durations and z-indexes it still holds,
// and the count must match exactly, so each cleanup lowers the number in its own pull request and
// nothing new slips in; custom properties that are read are defined; and no new native <select>
// appears. Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const srcDir = fileURLToPath(new URL("../src", import.meta.url));

function files(dir: string, ext: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) =>
    entry.isDirectory() ? files(join(dir, entry.name), ext) : entry.name.endsWith(ext) ? [join(dir, entry.name)] : [],
  );
}
const rel = (file: string) => relative(srcDir, file).replaceAll("\\", "/");

// ---- counting -----------------------------------------------------------------------------------

const CATEGORIES = ["colour", "font-size", "font-weight", "radius", "shadow", "duration", "z-index"] as const;
type Category = (typeof CATEGORIES)[number];
type Counts = Record<Category, number>;

const NAMED_COLOURS =
  "aliceblue antiquewhite aqua aquamarine azure beige bisque black blanchedalmond blue blueviolet brown burlywood cadetblue chartreuse chocolate coral cornflowerblue cornsilk crimson cyan darkblue darkcyan darkgoldenrod darkgray darkgreen darkgrey darkkhaki darkmagenta darkolivegreen darkorange darkorchid darkred darksalmon darkseagreen darkslateblue darkslategray darkslategrey darkturquoise darkviolet deeppink deepskyblue dimgray dimgrey dodgerblue firebrick floralwhite forestgreen fuchsia gainsboro ghostwhite gold goldenrod gray green greenyellow grey honeydew hotpink indianred indigo ivory khaki lavender lavenderblush lawngreen lemonchiffon lightblue lightcoral lightcyan lightgoldenrodyellow lightgray lightgreen lightgrey lightpink lightsalmon lightseagreen lightskyblue lightslategray lightslategrey lightsteelblue lightyellow lime limegreen linen magenta maroon mediumaquamarine mediumblue mediumorchid mediumpurple mediumseagreen mediumslateblue mediumspringgreen mediumturquoise mediumvioletred midnightblue mintcream mistyrose moccasin navajowhite navy oldlace olive olivedrab orange orangered orchid palegoldenrod palegreen paleturquoise palevioletred papayawhip peachpuff peru pink plum powderblue purple rebeccapurple red rosybrown royalblue saddlebrown salmon sandybrown seagreen seashell sienna silver skyblue slateblue slategray slategrey snow springgreen steelblue tan teal thistle tomato turquoise violet wheat white whitesmoke yellow yellowgreen".split(
    " ",
  );
// `transparent`, `currentColor` and `inherit` are not in the list, so they are allowed.
const NAMED_COLOUR = new RegExp(`(?<![-\\w#.$@])(?:${NAMED_COLOURS.join("|")})(?![-\\w(])`, "gi");
const COLOUR_FUNCTION = /\b(?:rgb|rgba|hsl|hsla|hwb|lab|lch|oklab|oklch)\(/gi;
const HEX_COLOUR = /#[0-9a-fA-F]{3,8}\b/g;
const DURATION = /(?<![\w.-])\d*\.?\d+m?s\b/g;
const NOT_A_VALUE = new Set(["none", "inherit", "initial", "unset", "revert", "0"]);

/** Every `property: value` in a stylesheet, comments and string contents removed. */
function declarations(css: string): [string, string][] {
  const text = css.replace(/\/\*[\s\S]*?\*\//g, "").replace(/"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*'/g, '""');
  return [...text.matchAll(/([-\w]+)\s*:\s*([^;{}]+?)\s*(?:;|(?=\}))/g)].map((match) => [match[1].toLowerCase(), match[2].trim()]);
}

/**
 * What a stylesheet still spells out instead of reading from a token:
 * colour: hex, rgb(), hsl() and named colours;
 * font-size: a value that is not var(--fs-*), inherit, or an em or % length;
 * font-weight: a value that is not var(--fw-*) or inherit (numbers and normal or bold);
 * radius and shadow: a border-radius or box-shadow with no var(--…) in it (none, inherit and 0 are free);
 * duration: a literal time in transition or animation; z-index: a value that is not var(--z-*) or auto.
 */
function count(css: string): Counts {
  const counts = Object.fromEntries(CATEGORIES.map((category) => [category, 0])) as Counts;
  for (const [property, value] of declarations(css)) {
    if (property !== "content") counts.colour += (value.match(NAMED_COLOUR) ?? []).length;
    counts.colour += (value.match(HEX_COLOUR) ?? []).length + (value.match(COLOUR_FUNCTION) ?? []).length;
    if (property === "font-size" && !/^var\(--fs-[\w-]+\)$/.test(value) && value !== "inherit" && !/^[\d.]+(em|%)$/.test(value)) counts["font-size"]++;
    if (property === "font-weight" && !/^var\(--fw-[\w-]+\)$/.test(value) && value !== "inherit") counts["font-weight"]++;
    if (/^border(-[a-z]+)*-radius$/.test(property) && !value.includes("var(--") && !NOT_A_VALUE.has(value)) counts.radius++;
    if (property === "box-shadow" && !value.includes("var(--") && !NOT_A_VALUE.has(value)) counts.shadow++;
    if (/^(transition|animation)(-duration|-delay)?$/.test(property)) counts.duration += (value.match(DURATION) ?? []).length;
    if (property === "z-index" && !/^var\(--z-[\w-]+\)$/.test(value) && value !== "auto" && value !== "inherit") counts["z-index"]++;
  }
  return counts;
}

/** The allowance in a file's first-line comment, `/* style-budget: colour=3 font-size=12 … *\/`; a category it does not name is zero. */
function budget(css: string): Counts {
  const budgets = Object.fromEntries(CATEGORIES.map((category) => [category, 0])) as Counts;
  const line = /^\/\* style-budget:([^*]*)\*\//.exec(css)?.[1] ?? "";
  for (const match of line.matchAll(/([\w-]+)=(\d+)/g)) {
    assert.ok((CATEGORIES as readonly string[]).includes(match[1]), `unknown style-budget category ${match[1]}`);
    budgets[match[1] as Category] = Number(match[2]);
  }
  return budgets;
}

test("the counter reads colours, sizes, weights, radii, shadows, durations and z-indexes", () => {
  const css = `/* #fff red */
    .a { color: #fff; background: rgb(0 0 0 / 0.5); border: 1px solid white; content: "red"; }
    .b { color: transparent; background: currentColor; border-color: inherit; fill: var(--accent); }
    .c { font-size: 0.8rem; font-weight: 600; border-radius: 6px; box-shadow: 0 1px 2px rgb(0 0 0 / 0.3); }
    .d { font-size: var(--fs-md); font-weight: var(--fw-medium); border-radius: var(--radius); box-shadow: var(--elev-1); }
    .e { font-size: 1.2em; font-size: 90%; font-size: inherit; border-radius: 0; box-shadow: none; }
    .f { transition: opacity 120ms ease, color var(--fast) var(--ease); animation: spin 0.9s linear infinite; z-index: 5; }
    .g { transition: opacity var(--dur-fast); z-index: var(--z-overlay); }
    @media (hover: none) { .h { z-index: auto; } }`;
  assert.deepEqual(count(css), { colour: 4, "font-size": 1, "font-weight": 1, radius: 1, shadow: 1, duration: 2, "z-index": 1 });
});

test("a budget comment names categories, and a missing one is zero", () => {
  assert.deepEqual(budget("/* style-budget: colour=3 z-index=2 */\n.a {}"), { colour: 3, "font-size": 0, "font-weight": 0, radius: 0, shadow: 0, duration: 0, "z-index": 2 });
  assert.deepEqual(budget(".a {}"), { colour: 0, "font-size": 0, "font-weight": 0, radius: 0, shadow: 0, duration: 0, "z-index": 0 });
});

test("every stylesheet except tokens.css holds exactly the literals its style-budget comment allows", () => {
  const sheets = files(srcDir, ".css").filter((file) => rel(file) !== "styles/tokens.css");
  assert.ok(sheets.length > 1, "no stylesheets found");
  const problems: string[] = [];
  for (const file of sheets) {
    const css = readFileSync(file, "utf8");
    const allowed = budget(css), actual = count(css);
    for (const category of CATEGORIES) {
      if (actual[category] > allowed[category]) problems.push(`${rel(file)}: ${category} is ${actual[category]}, over its budget of ${allowed[category]}`);
      if (actual[category] < allowed[category]) problems.push(`${rel(file)}: ${category} is ${actual[category]}, under its budget of ${allowed[category]}: lower the budget comment`);
    }
  }
  assert.deepEqual(problems, []);
});

// ---- custom properties that are read must be defined ---------------------------------------------

/** Set from TypeScript, not CSS: the style attribute of a row (`--branch-depth`, `--swatch`) and the root element (`--font-prose`). */
const SET_FROM_TSX = ["--branch-depth", "--swatch", "--font-prose"];
/** Read but never defined, with a fallback that is not a token: it resolves to transparent today. The stage that defines it removes the entry. */
const KNOWN_UNDEFINED: string[] = [];

function reads(css: string): { name: string; fallback: string | undefined }[] {
  const text = css.replace(/\/\*[\s\S]*?\*\//g, "");
  return [...text.matchAll(/var\(\s*(--[\w-]+)\s*(?:,((?:[^()]|\([^()]*\))*))?\)/g)].map((match) => ({ name: match[1], fallback: match[2]?.trim() }));
}

test("every var(--name) read in CSS is defined, set from TSX, or falls back to a token", () => {
  const sheets = files(srcDir, ".css").map((file) => [file, readFileSync(file, "utf8")] as const);
  const defined = new Set(sheets.flatMap(([, css]) => declarations(css).map(([property]) => property).filter((property) => property.startsWith("--"))));
  const sources = [...files(srcDir, ".ts"), ...files(srcDir, ".tsx")].map((file) => readFileSync(file, "utf8")).join("\n");
  for (const name of SET_FROM_TSX) assert.ok(sources.includes(name), `${name} is no longer set from TypeScript: drop it from SET_FROM_TSX`);
  const known = new Set<string>();
  const problems: string[] = [];
  for (const [file, css] of sheets) {
    for (const { name, fallback } of reads(css)) {
      if (defined.has(name) || SET_FROM_TSX.includes(name)) continue;
      const token = /^var\(\s*(--[\w-]+)\s*\)$/.exec(fallback ?? "")?.[1];
      if (token && defined.has(token)) continue;
      if (KNOWN_UNDEFINED.includes(name)) { known.add(name); continue; }
      problems.push(`${rel(file)}: var(${name}) is read but never defined`);
    }
  }
  for (const name of KNOWN_UNDEFINED) assert.ok(known.has(name), `${name} is defined or no longer read: drop it from KNOWN_UNDEFINED`);
  assert.deepEqual(problems, []);
});

// ---- no new native <select> ----------------------------------------------------------------------

test("a component file has as many native <select> elements as its `// style-budget: native-select=N` comment allows", () => {
  const problems: string[] = [];
  for (const file of files(srcDir, ".tsx")) {
    const source = readFileSync(file, "utf8");
    const selects = (source.match(/<select\b/g) ?? []).length;
    const allowed = Number(/^\/\/ style-budget: native-select=(\d+)$/m.exec(source)?.[1] ?? 0);
    if (selects !== allowed) problems.push(`${rel(file)}: ${selects} native <select>, allowed ${allowed}`);
  }
  assert.deepEqual(problems, []);
});
