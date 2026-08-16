/* tslint:disable */
/* eslint-disable */

export class App {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    address(): string;
    exportKey(): string;
    exportPending(): string;
    /**
     * Recovery data includes the contract parameters needed to rediscover
     * this tab's VTXOs after an operator signer or exit-delay rotation.
     */
    exportRecovery(): string;
    gameAddress(): string;
    /**
     * Connect to one Arkade server and restore the supplied wallet key. The
     * browser owns persistence so a failed network request cannot replace it.
     */
    static init(server: string, secret_key?: string | null, pending_journal?: string | null): Promise<App>;
    /**
     * Return local wallet/game state immediately, without waiting on the
     * indexer. The browser uses this to render the restored wallet while the
     * first network synchronization runs.
     */
    snapshot(): any;
    /**
     * Synchronize state and execute at most one requested wallet/game action.
     */
    step(dirs: Uint8Array, enter_game: boolean, sweep_address?: string | null): Promise<any>;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_app_free: (a: number, b: number) => void;
    readonly app_address: (a: number, b: number) => void;
    readonly app_exportKey: (a: number, b: number) => void;
    readonly app_exportPending: (a: number, b: number) => void;
    readonly app_exportRecovery: (a: number, b: number) => void;
    readonly app_gameAddress: (a: number, b: number) => void;
    readonly app_init: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly app_snapshot: (a: number, b: number) => void;
    readonly app_step: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly rustsecp256k1_v0_12_context_create: (a: number) => number;
    readonly rustsecp256k1_v0_12_context_destroy: (a: number) => void;
    readonly rustsecp256k1_v0_12_default_error_callback_fn: (a: number, b: number) => void;
    readonly rustsecp256k1_v0_12_default_illegal_callback_fn: (a: number, b: number) => void;
    readonly rustsecp256k1_v0_10_0_context_create: (a: number) => number;
    readonly rustsecp256k1_v0_10_0_context_destroy: (a: number) => void;
    readonly rustsecp256k1_v0_10_0_default_error_callback_fn: (a: number, b: number) => void;
    readonly rustsecp256k1_v0_10_0_default_illegal_callback_fn: (a: number, b: number) => void;
    readonly __wasm_bindgen_func_elem_4682: (a: number, b: number, c: number, d: number) => void;
    readonly __wasm_bindgen_func_elem_4684: (a: number, b: number, c: number, d: number) => void;
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
