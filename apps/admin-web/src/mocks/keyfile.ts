// A stand-in publisher key file for mock mode and tests. The real `vgames.key/1` format is only
// readable by pack-wasm; the mock signer (upload/mockPack.ts) understands this JSON instead.
import { MOCK_KEY_ID } from "./db";

export const MOCK_PASSPHRASE = "correct horse battery staple";

export interface MockKeyFile {
  format: "vgames.key/mock";
  kind: "publisher" | "root";
  key_id: string;
  label: string;
  /** Not a secret: the mock only compares it. */
  passphrase: string;
}

export function mockKeyFile(patch: Partial<MockKeyFile> = {}): string {
  const file: MockKeyFile = {
    format: "vgames.key/mock",
    kind: "publisher",
    key_id: MOCK_KEY_ID,
    label: "Adrian's laptop",
    passphrase: MOCK_PASSPHRASE,
    ...patch,
  };
  return JSON.stringify(file);
}
