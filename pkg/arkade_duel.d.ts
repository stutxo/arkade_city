/* tslint:disable */
/* eslint-disable */

export class App {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    address(): string;
    /**
     * Recovery export: the raw key hex. Anyone holding it can spend.
     */
    exportKey(): string;
    /**
     * Load or generate the browser key and fetch server parameters.
     */
    static init(server_url?: string | null): Promise<App>;
    /**
     * "mainnet" or "signet" — known right after init, before any snapshot.
     */
    network(): string;
    /**
     * The single serialized entry point.
     *
     * * `command`: "", "host", "join" (`arg` = host address), or "reset"
     * * `mask`: current WASD key bitmask (see `keyMask`)
     * * `fires`: number of fire presses since the previous step
     *
     * Returns the JSON snapshot for rendering.
     */
    step(command: string, arg: string, mask: number, fires: number): Promise<any>;
}

export function keyMask(w: boolean, a: boolean, s: boolean, d: boolean): number;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_app_free: (a: number, b: number) => void;
    readonly app_address: (a: number, b: number) => void;
    readonly app_exportKey: (a: number, b: number) => void;
    readonly app_init: (a: number, b: number) => number;
    readonly app_network: (a: number, b: number) => void;
    readonly app_step: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => number;
    readonly keyMask: (a: number, b: number, c: number, d: number) => number;
    readonly rustsecp256k1_v0_12_context_create: (a: number) => number;
    readonly rustsecp256k1_v0_12_context_destroy: (a: number) => void;
    readonly rustsecp256k1_v0_12_default_error_callback_fn: (a: number, b: number) => void;
    readonly rustsecp256k1_v0_12_default_illegal_callback_fn: (a: number, b: number) => void;
    readonly rustsecp256k1_v0_10_0_context_create: (a: number) => number;
    readonly rustsecp256k1_v0_10_0_context_destroy: (a: number) => void;
    readonly rustsecp256k1_v0_10_0_default_error_callback_fn: (a: number, b: number) => void;
    readonly rustsecp256k1_v0_10_0_default_illegal_callback_fn: (a: number, b: number) => void;
    readonly __wasm_bindgen_func_elem_3427: (a: number, b: number, c: number, d: number) => void;
    readonly __wasm_bindgen_func_elem_3429: (a: number, b: number, c: number, d: number) => void;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number) => void;
    readonly __wbindgen_export4: (a: number, b: number, c: number) => void;
    readonly __wbindgen_export5: (a: number, b: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
