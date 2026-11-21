import { z } from "zod";
import clientJson from "../../../protocol/schema/client-frame.schema.json" with { type: "json" };
import serverJson from "../../../protocol/schema/server-frame.schema.json" with { type: "json" };
import type { ClientFrame } from "./generated/client-frame.ts";
import type { ServerFrame } from "./generated/server-frame.ts";

export type * from "./generated/client-frame.ts";
export type * from "./generated/server-frame.ts";

// JSON imports widen schema keywords such as "object" to string, so type the
// imported JSON once here instead of asserting at every use.
type JsonSchema = Parameters<typeof z.fromJSONSchema>[0];
type ObjectSchema = {
  type: "object";
  properties: Record<string, unknown>;
  required: string[];
  $defs: Record<string, unknown>;
};
type VariantSchema = ObjectSchema & { properties: { type: { const: string } } };
type ServerSchemaJson = ObjectSchema & { oneOf: VariantSchema[] };

const server = serverJson as unknown as ServerSchemaJson;
const client = clientJson as unknown as ObjectSchema;

export const serverFrameSchema = z.fromJSONSchema(server as JsonSchema) as z.ZodType<ServerFrame>;
export const clientFrameSchema = z.fromJSONSchema(client as JsonSchema) as z.ZodType<ClientFrame>;

// The union reports a root-level error for invalid bodies. Validate against only
// the selected branch on failure so callers get the path to the offending field.
const serverVariants = new Map(
  server.oneOf.map((branch) => [
    branch.properties.type.const,
    z.fromJSONSchema({
      ...branch,
      properties: { ...server.properties, ...branch.properties },
      required: [...server.required, ...branch.required],
      $defs: server.$defs,
    } as JsonSchema),
  ]),
);

export type Issue = { path: (string | number)[]; message: string };
type Invalid = { kind: "invalid"; issues: Issue[] };
type Parsed<T> = { kind: "frame"; frame: T };
type ProtocolMismatch = { kind: "protocol_mismatch" };
export type ParseResult =
  | Parsed<ServerFrame>
  | { kind: "unknown_type"; type: string }
  | ProtocolMismatch
  | Invalid;

export const PROTOCOL_MAJOR = 1;

function checkVersion(value: unknown): Invalid | ProtocolMismatch | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return { kind: "invalid", issues: [{ path: [], message: "frame must be an object" }] };
  }
  const envelope = value as Record<string, unknown>;
  for (const field of ["v", "protocol_major"]) {
    if (field === "protocol_major" && !(field in envelope)) continue;
    const version = envelope[field];
    if (typeof version !== "number" || !Number.isInteger(version)) {
      return {
        kind: "invalid",
        issues: [{ path: [field], message: `${field} must be an integer` }],
      };
    }
    if (version !== PROTOCOL_MAJOR) return { kind: "protocol_mismatch" };
  }
}

function invalid(issues: readonly z.ZodIssue[]): Invalid {
  return {
    kind: "invalid",
    issues: issues.map(({ path, message }) => ({
      path: path.map((key) => (typeof key === "symbol" ? String(key) : key)),
      message,
    })),
  };
}

function parseJson(text: string): { kind: "json"; value: unknown } | Invalid {
  try {
    return { kind: "json", value: JSON.parse(text) };
  } catch (error) {
    return {
      kind: "invalid",
      issues: [{ path: [], message: error instanceof Error ? error.message : "Invalid JSON" }],
    };
  }
}

export function parseServerFrame(text: string): ParseResult {
  const json = parseJson(text);
  if (json.kind === "invalid") return json;
  const versionError = checkVersion(json.value);
  if (versionError) return versionError;

  const envelope = json.value as Record<string, unknown>;
  for (const [field, min, max] of [
    ["ts_ms", 0, Number.MAX_SAFE_INTEGER],
    ["seq", 1, Number.MAX_SAFE_INTEGER],
  ] as const) {
    const value = envelope[field];
    if (typeof value !== "number" || !Number.isInteger(value) || value < min || value > max) {
      return {
        kind: "invalid",
        issues: [{ path: [field], message: `${field} must be an integer in ${min}..=${max}` }],
      };
    }
  }

  const type = envelope.type;
  if (typeof type !== "string") {
    return { kind: "invalid", issues: [{ path: ["type"], message: "type must be a string" }] };
  }
  const variant = serverVariants.get(type);
  if (!variant) return { kind: "unknown_type", type };

  const result = serverFrameSchema.safeParse(json.value);
  if (result.success) return { kind: "frame", frame: result.data };
  const detail = variant.safeParse(json.value);
  return invalid(detail.success ? result.error.issues : detail.error.issues);
}

export function parseClientFrame(text: string): Parsed<ClientFrame> | Invalid | ProtocolMismatch {
  const json = parseJson(text);
  if (json.kind === "invalid") return json;
  const versionError = checkVersion(json.value);
  if (versionError) return versionError;
  const result = clientFrameSchema.safeParse(json.value);
  return result.success ? { kind: "frame", frame: result.data } : invalid(result.error.issues);
}

export function encodeClientFrame(frame: ClientFrame): string {
  return JSON.stringify(clientFrameSchema.parse(frame));
}
