// Generated from JSON Schema. Run pnpm run generate to regenerate.

export type ClientFrame = {
  /**
   * Sender wall-clock time in Unix epoch milliseconds.
   */
  ts_ms: number;
  /**
   * Protocol major version number.
   */
  v: number;
} & {
  client: ClientIdentity;
  /**
   * Protocol major version number spoken by the client.
   */
  protocol_major: number;
  token: string;
  type: "client.hello";
};

export interface ClientIdentity {
  name: string;
  version: string;
}
