import { createHash, createHmac } from "node:crypto";
import { readdirSync, readFileSync } from "node:fs";
import { expect, test } from "vitest";
import vector from "../../../protocol/fixtures/handshake/known-answer.json" with { type: "json" };
import challenge from "../../../protocol/fixtures/valid/auth-challenge.json" with { type: "json" };
import auth from "../../../protocol/fixtures/valid/client-auth.json" with { type: "json" };
import hello from "../../../protocol/fixtures/valid/client-hello.json" with { type: "json" };
import serverJson from "../../../protocol/schema/server-frame.schema.json" with { type: "json" };
import { parseClientFrame, parseServerFrame } from "./index.ts";

const root = new URL("../../../protocol/fixtures/", import.meta.url);

test.each([
  ["valid", "frame"],
  ["invalid", "invalid"],
  ["unknown", "unknown_type"],
  ["mismatch", "protocol_mismatch"],
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

test("handshake proofs match the independent known-answer vector", () => {
  const key = createHash("sha256").update(vector.token).digest();
  const nc = Buffer.from(vector.nc, "base64url");
  const ns = Buffer.from(vector.ns, "base64url");
  expect(nc.length).toBe(32);
  expect(ns.length).toBe(32);
  const proof = (role: "server" | "client") =>
    createHmac("sha256", key)
      .update(`chassis/v1/${role}\0`)
      .update(nc)
      .update(ns)
      .digest("base64url");

  expect(proof("server")).toBe(vector.proof_s);
  expect(proof("client")).toBe(vector.proof_c);
  expect(vector.proof_s).not.toBe(vector.proof_c);
  expect(hello.nonce).toBe(vector.nc);
  expect(challenge.nonce).toBe(vector.ns);
  expect(challenge.proof).toBe(vector.proof_s);
  expect(auth.proof).toBe(vector.proof_c);
});
