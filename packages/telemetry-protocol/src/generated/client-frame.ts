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
} & (
  | {
      client: ClientIdentity;
      /**
       * A fresh 32-byte nonce, encoded as unpadded base64url.
       */
      nonce: string;
      /**
       * Protocol major version number spoken by the client.
       */
      protocol_major: number;
      type: "client.hello";
    }
  | {
      proof: string;
      type: "client.auth";
    }
);

export interface ClientIdentity {
  name: string;
  version: string;
}
