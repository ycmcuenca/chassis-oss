import { readFileSync } from "node:fs";
import { parseClientFrame, parseServerFrame } from "../src/index.ts";

function frameType(text: string): string {
  try {
    const value: unknown = JSON.parse(text);
    if (typeof value === "object" && value !== null && "type" in value) {
      if (typeof value.type === "string") return value.type;
    }
  } catch {
    // The protocol parser reports syntax errors.
  }
  return "<no type>";
}

function main(paths: string[]): number {
  const [file] = paths;
  if (paths.length !== 1 || file === undefined) {
    console.error("usage: validate FILE");
    return 2;
  }
  let text: string;
  try {
    text = readFileSync(file, "utf8");
  } catch (error) {
    console.error(`ts: could not read ${file}: ${error}`);
    return 2;
  }

  const type = frameType(text);
  const result = type.startsWith("client.") ? parseClientFrame(text) : parseServerFrame(text);
  switch (result.kind) {
    case "frame":
      console.log(`ts: accepted (${type})`);
      break;
    case "unknown_type":
      console.log(`ts: unknown (${result.type})`);
      break;
    case "protocol_mismatch":
      console.log(`ts: protocol mismatch (${type})`);
      break;
    case "invalid":
      console.log(`ts: rejected (${type})`);
      for (const { path, message } of result.issues) {
        console.log(`  ${path.join(".") || "<root>"}: ${message}`);
      }
      break;
  }
  return 0;
}

process.exitCode = main(process.argv.slice(2));
