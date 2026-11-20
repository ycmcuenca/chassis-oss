import { expect, expectTypeOf, test } from "vitest";
import sample from "../../../protocol/fixtures/valid/host-sample-full.json" with { type: "json" };
import {
  type ClientFrame,
  encodeClientFrame,
  parseClientFrame,
  parseServerFrame,
} from "./index.ts";

test("parses known frames with extra fields and narrows by type", () => {
  const result = parseServerFrame(
    JSON.stringify({ v: 1, ts_ms: 123, seq: 1, type: "agent.remove", id: "one", future: true }),
  );
  expect(result.kind).toBe("frame");
  if (result.kind === "frame" && result.frame.type === "agent.remove") {
    expectTypeOf(result.frame.id).toEqualTypeOf<string>();
    expect(result.frame.id).toBe("one");
  }
});

test("skips an unknown type only after checking the envelope", () => {
  expect(parseServerFrame('{"v":1,"ts_ms":123,"seq":1,"type":"future"}')).toEqual({
    kind: "unknown_type",
    type: "future",
  });
  expect(parseServerFrame('{"v":1,"ts_ms":123,"type":"future"}')).toMatchObject({
    kind: "invalid",
    issues: [{ path: ["seq"] }],
  });
});

test("reports malformed JSON and useful paths for invalid known frames", () => {
  expect(parseServerFrame("{")).toMatchObject({ kind: "invalid", issues: [{ path: [] }] });
  const result = parseServerFrame(
    JSON.stringify({ ...sample, cpu: { ...sample.cpu, usage_pct: "50" } }),
  );
  expect(result).toMatchObject({ kind: "invalid", issues: [{ path: ["cpu", "usage_pct"] }] });
});

test("round-trips client frames and rejects invalid input when encoding", () => {
  const frame: ClientFrame = {
    v: 1,
    ts_ms: 123,
    type: "client.hello",
    client: { name: "test", version: "1" },
    protocol_major: 1,
    token: "test-token",
  };
  expectTypeOf<ClientFrame["type"]>().toEqualTypeOf<"client.hello">();
  expect(parseClientFrame(encodeClientFrame(frame))).toEqual({ kind: "frame", frame });
  expect(() => encodeClientFrame({ ...frame, protocol_major: -1 })).toThrow();
});
