import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { checkBoundary } from "./check-boundary.mjs";

function fixture(t, name, body) {
  const root = mkdtempSync(join(tmpdir(), "chassis-boundary-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  writeFileSync(join(root, name), body);
  return root;
}

test("allows a dependency inside the workspace", (t) => {
  const root = fixture(t, "Cargo.toml", '[dependencies]\nfoo = { path = "crates/foo" }\n');
  assert.deepEqual(checkBoundary(root), []);
});

test("rejects a Cargo path outside the workspace", (t) => {
  const root = fixture(t, "Cargo.toml", '[dependencies]\nfoo = { path = "../outside" }\n');
  assert.deepEqual(checkBoundary(root), [
    `boundary: Cargo.toml: ../outside -> ${join(root, "../outside")}`,
  ]);
});

test("rejects a pnpm link outside the workspace", (t) => {
  const root = fixture(t, "package.json", '{"dependencies":{"x":"link:../../elsewhere"}}');
  assert.deepEqual(checkBoundary(root), [
    `boundary: package.json: x -> ${join(root, "../../elsewhere")}`,
  ]);
});
