// In-process stand-ins for the upload workers (jsdom has no Worker), with the mock libraries.
import { handleKeyRequest, type KeyState } from "../upload/keyCore";
import { mockKeyLib, mockPackLib } from "../upload/mockPack";
import { handlePackRequest, packState } from "../upload/packCore";
import type { Workers } from "../upload/rpc";

export function inProcessWorkers(mockPackSize?: number): Workers {
  return {
    pack: () => {
      const state = packState(mockPackLib(mockPackSize));
      return {
        call: (req) => handlePackRequest(state, req),
        terminate: () => {},
      };
    },
    key: () => {
      const state: KeyState = { lib: mockKeyLib, key: null };
      let terminated = false;
      return {
        call: async (req) =>
          terminated
            ? { kind: "error", code: "internal", message: "worker terminated" }
            : handleKeyRequest(state, req),
        terminate: () => {
          terminated = true;
          state.key?.free();
          state.key = null;
        },
      };
    },
  };
}
