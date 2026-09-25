// Helpers shaped like the tauri-specta runtime, shared by every pending-contract module.
import { invoke as TAURI_INVOKE } from "@tauri-apps/api/core";
import * as TAURI_API_EVENT from "@tauri-apps/api/event";

export type Result<T, E> = { status: "ok"; data: T } | { status: "error"; error: E };

/** A command returning `Result<T, E>` in Rust. */
export async function call<T, E>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<Result<T, E>> {
  try {
    return { status: "ok", data: await TAURI_INVOKE<T>(cmd, args) };
  } catch (e) {
    if (e instanceof Error) throw e;
    return { status: "error", error: e as E };
  }
}

/** A command returning a plain value in Rust. */
export async function get<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return await TAURI_INVOKE<T>(cmd, args);
}

export type EventApi<T> = {
  listen: (cb: TAURI_API_EVENT.EventCallback<T>) => ReturnType<typeof TAURI_API_EVENT.listen<T>>;
  once: (cb: TAURI_API_EVENT.EventCallback<T>) => ReturnType<typeof TAURI_API_EVENT.once<T>>;
  emit: (payload: T) => ReturnType<typeof TAURI_API_EVENT.emit>;
};

/** Same helper shape as tauri-specta's `__makeEvents__`. */
export function makeEvents<T extends Record<string, unknown>>(mappings: Record<keyof T, string>) {
  const out = {} as { [K in keyof T]: EventApi<T[K]> };
  for (const key of Object.keys(mappings) as (keyof T)[]) {
    const name = mappings[key];
    out[key] = {
      listen: (cb) => TAURI_API_EVENT.listen(name, cb),
      once: (cb) => TAURI_API_EVENT.once(name, cb),
      emit: (payload) => TAURI_API_EVENT.emit(name, payload),
    };
  }
  return out;
}
