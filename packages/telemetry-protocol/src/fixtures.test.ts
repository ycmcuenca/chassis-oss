import { readdirSync, readFileSync } from "node:fs";
import { expect, test } from "vitest";
import serverJson from "../../../protocol/schema/server-frame.schema.json" with { type: "json" };
import { parseClientFrame, parseServerFrame } from "./index.ts";

const root = new URL("../../../protocol/fixtures/", import.meta.url);

test.each([
  ["valid", "frame"],
  ["invalid", "invalid"],
  ["unknown", "unknown_type"],
] as const)("shared %s fixtures", (dir, expected) => {
  const directory = new URL(`${dir}/`, root);
  const files = readdirSync(directory)
    .filter((name) => name.endsWith(".json"))
    .sort();
  expect(files.length, `${dir}/: no JSON fixtures`).toBeGreaterThan(0);

  const covered = new Set<string>();
  for (const file of files) {
    const text = readFileSync(new URL(file, directory), "utf8");
    const isClient = file.startsWith("client-");
    const result = isClient ? parseClientFrame(text) : parseServerFrame(text);
    expect(result.kind, `${dir}/${file}`).toBe(expected);
    if (!isClient && result.kind === "frame") covered.add(result.frame.type);
  }

  if (dir === "valid") {
    const serverTypes = serverJson.oneOf.map((branch) => branch.properties.type.const);
    expect(covered).toEqual(new Set(serverTypes));
  }
});
