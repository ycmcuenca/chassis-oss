import { mkdir, writeFile } from "node:fs/promises";
import { compile } from "json-schema-to-typescript";
import clientJson from "../../../protocol/schema/client-frame.schema.json" with { type: "json" };
import serverJson from "../../../protocol/schema/server-frame.schema.json" with { type: "json" };

const output = new URL("../src/generated/", import.meta.url);
await mkdir(output, { recursive: true });
for (const [name, schema] of [
  ["server-frame", serverJson],
  ["client-frame", clientJson],
] as const) {
  // JSON imports widen schema keywords such as "object" to string.
  const types = await compile(schema as unknown as Parameters<typeof compile>[0], schema.title, {
    additionalProperties: false,
    bannerComment: "// Generated from JSON Schema. Run pnpm run generate to regenerate.",
  });
  await writeFile(new URL(`${name}.ts`, output), types);
}
