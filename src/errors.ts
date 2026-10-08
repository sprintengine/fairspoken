// Tauri commands reject with plain strings; JS failures throw Errors. Either
// way the UI wants one line of text.
export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
