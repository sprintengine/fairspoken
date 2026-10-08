// Look up an element the page markup must provide; a missing one is a build
// error in the HTML, so fail loudly at startup rather than later.
export function required<T extends HTMLElement = HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`Missing #${id}`);
  return node as T;
}
